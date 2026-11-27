//! SQL history: snapshots in plugin storage and the two-level tab (lane W2-0).
//!
//! Split out of `views::log_panel` with no behaviour change: the snapshot
//! save/prune/list/open path (the reference's auto-saver), the entries built
//! from one snapshot, and the `history-*` ops.

use std::hash::{Hash, Hasher};
use std::sync::{Mutex, MutexGuard, OnceLock};

use openidea_plugin::{Error, Invocation, host};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::core::log::{LogAnalysis, LogSources, SourceRef, parse_with_sources};
use crate::indexer;

use super::logs::reveal;
use super::state::{ensure_parsed, parse_options, store};
use super::{
    SQL_DISPLAY_CHARS, civil_from_days, format_epoch_ms, message_line, now_ms, push_state,
    truncate_chars,
};

// SQL history (parity with the reference's HistorySqlLogPanel): snapshots are
// persisted in the plugin storage, one key per file, and the tab lists them as
// files first, then the statements of the file. Storage is per plugin, not per
// project, so every key is prefixed with a hash of the project root — the
// reference keeps one history directory per project.
const HISTORY_BASE: &str = "history";
/// Keep at most this many history files (the oldest are dropped).
const HISTORY_MAX_FILES: usize = 10;
/// A snapshot larger than this is not stored; the panel keeps working.
const HISTORY_MAX_BYTES: usize = 512 * 1024;
/// Statements handed to the entries level (the rest are counted, not listed).
const HISTORY_MAX_ENTRIES: usize = 500;

/// The project root the history keys belong to (set from the invocation before
/// any history operation).
static HISTORY_SCOPE: OnceLock<Mutex<String>> = OnceLock::new();

fn history_scope() -> MutexGuard<'static, String> {
    HISTORY_SCOPE
        .get_or_init(|| Mutex::new(String::new()))
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

fn history_scope_key(ctx: &Invocation) -> String {
    let root = ctx
        .project()
        .and_then(|project| project.root().ok())
        .unwrap_or_default();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

pub(super) fn set_history_scope(ctx: &Invocation) {
    let key = history_scope_key(ctx);
    *history_scope() = key;
}

fn history_prefix() -> String {
    format!("{HISTORY_BASE}.{}.file.", *history_scope())
}

fn history_seq_key() -> String {
    format!("{HISTORY_BASE}.{}.seq", *history_scope())
}

fn history_hash_key() -> String {
    format!("{HISTORY_BASE}.{}.last_hash", *history_scope())
}
/// One statement inside an opened history file.
#[derive(Debug, Clone, PartialEq)]
struct HistoryEntry {
    /// 1-based line in the snapshot text (used by the page's ops).
    line: usize,
    time: String,
    duration_ms: Option<f64>,
    /// `class#method` when the logger stated it.
    source: Option<SourceRef>,
    /// Resolved `File.java:line` when the Java index knows the source.
    location: String,
    /// Full SQL for copy / preview (the row shows [`Self::summary`]).
    sql: String,
    /// Truncated SQL for the list.
    summary: String,
    has_sql: bool,
}

/// One row of the first level.
#[derive(Debug, Clone, PartialEq)]
struct HistoryFile {
    id: String,
    name: String,
    bytes: usize,
    count: usize,
    saved: String,
}

/// The two levels the reference panel uses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum HistoryLevel {
    #[default]
    Files,
    Entries,
}

/// State of the "SQL history" tab: a file list, then the statements of the
/// opened file (the reference's `HistorySqlLogPanel`).
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct HistoryTab {
    level: HistoryLevel,
    keyword: String,
    files: Vec<HistoryFile>,
    /// Id of the opened snapshot (`None` at the files level).
    current: Option<String>,
    current_name: String,
    entries: Vec<HistoryEntry>,
    /// Statements before the keyword filter (the file's own count).
    total: usize,
    truncated: bool,
    /// Quick preview under the list (the reference's docked preview toggle).
    preview: bool,
    preview_line: Option<usize>,
    /// Whether a keyword has been applied at this level.
    ran: bool,
}

/// The JSON stored per history file. The text is the raw log, so opening it
/// reuses the same parser the live panel uses.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct HistorySnapshot {
    name: String,
    /// UTC stamp `YYYYMMDD-HHMMSS`, used as the file name and the saved label.
    saved: String,
    bytes: usize,
    count: usize,
    text: String,
}
/// `tabs.history`: the last search over the buffered log text.
pub(super) fn history_state(tab: &HistoryTab) -> Value {
    let files: Vec<Value> = tab
        .files
        .iter()
        .map(|file| {
            json!({
                "id": file.id,
                "name": file.name,
                "bytes": file.bytes,
                "count": file.count,
                "saved": file.saved,
            })
        })
        .collect();
    let entries: Vec<Value> = tab
        .entries
        .iter()
        .map(|item| {
            json!({
                "line": item.line,
                "time": item.time,
                "duration_ms": item.duration_ms,
                "source": item.location,
                "method": item
                    .source
                    .as_ref()
                    .map(|source| source.key())
                    .unwrap_or_default(),
                "sql": item.summary,
                "has_sql": item.has_sql,
            })
        })
        .collect();
    json!({
        "level": match tab.level {
            HistoryLevel::Files => "files",
            HistoryLevel::Entries => "entries",
        },
        "ran": tab.ran,
        "keyword": tab.keyword,
        "files": files,
        "current": tab.current,
        "currentName": tab.current_name,
        "entries": entries,
        "total": tab.total,
        "truncated": tab.truncated,
        "preview": tab.preview,
        "previewLine": tab.preview_line,
    })
}
/// Saves the current buffer as a history file (the reference's auto-saver).
///
/// The snapshot is stored in plugin storage under this project's key prefix,
/// deduped by a content hash, and pruned to [`HISTORY_MAX_FILES`]. Called after
/// `capture-open-log` and whenever the history tab is listed, so a session is
/// not lost when the panel closes.
pub fn history_capture(ctx: &Invocation, name: &str) {
    set_history_scope(ctx);
    let _ = save_history(Some(name));
}

fn save_history(source_name: Option<&str>) -> Option<String> {
    let (text, count) = {
        let mut state = store();
        ensure_parsed(&mut state);
        (
            state.text.clone(),
            state
                .analysis
                .entries
                .iter()
                .filter(|entry| entry.has_sql())
                .count(),
        )
    };
    if text.trim().is_empty() {
        return None;
    }
    let hash = text_hash(&text).to_string();
    if host::storage::get(&history_hash_key())
        .ok()
        .flatten()
        .as_deref()
        == Some(hash.as_str())
    {
        return None;
    }
    let seq = host::storage::get(&history_seq_key())
        .ok()
        .flatten()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
        + 1;
    let stamp = format_stamp(now_ms() as i64);
    let name = match source_name {
        Some(name) if !name.trim().is_empty() => format!("{}.log", name.trim()),
        _ => format!("sql-log-{stamp}.log"),
    };
    let snapshot = HistorySnapshot {
        name,
        saved: stamp,
        bytes: text.len(),
        count,
        text,
    };
    let encoded = serde_json::to_string(&snapshot).ok()?;
    if encoded.len() > HISTORY_MAX_BYTES {
        return None;
    }
    let id = format!("{seq:08}");
    host::storage::set(&format!("{}{id}", history_prefix()), &encoded).ok()?;
    host::storage::set(&history_seq_key(), &seq.to_string()).ok();
    host::storage::set(&history_hash_key(), &hash).ok();
    prune_history();
    Some(id)
}

fn prune_history() {
    let prefix = history_prefix();
    let mut keys = host::storage::keys(&prefix).unwrap_or_default();
    keys.sort();
    while keys.len() > HISTORY_MAX_FILES {
        let oldest = keys.remove(0);
        host::storage::delete(&oldest).ok();
    }
}

fn text_hash(text: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// UTC `YYYYMMDD-HHMMSS` from epoch milliseconds (the save time labels the
/// history file, like the reference's auto-saver).
fn format_stamp(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

fn saved_label(stamp: &str) -> String {
    if stamp.len() == 15 {
        format!(
            "{}-{}-{} {}:{}:{}",
            &stamp[0..4],
            &stamp[4..6],
            &stamp[6..8],
            &stamp[9..11],
            &stamp[11..13],
            &stamp[13..15]
        )
    } else {
        String::new()
    }
}

fn list_history_files() -> Vec<HistoryFile> {
    let prefix = history_prefix();
    let mut files: Vec<HistoryFile> = host::storage::keys(&prefix)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|key| {
            let id = key.strip_prefix(&prefix)?.to_string();
            let encoded = host::storage::get(&key).ok().flatten()?;
            let snapshot: HistorySnapshot = serde_json::from_str(&encoded).ok()?;
            Some(HistoryFile {
                id,
                name: snapshot.name,
                bytes: snapshot.bytes,
                count: snapshot.count,
                saved: saved_label(&snapshot.saved),
            })
        })
        .collect();
    files.sort_by(|a, b| b.id.cmp(&a.id));
    files
}

fn open_history_file(id: &str, keyword: &str) -> bool {
    let key = format!("{}{id}", history_prefix());
    let Some(encoded) = host::storage::get(&key).ok().flatten() else {
        return false;
    };
    let Ok(snapshot) = serde_json::from_str::<HistorySnapshot>(&encoded) else {
        return false;
    };
    let parsed = parse_with_sources(&snapshot.text, &parse_options());
    let (entries, total, truncated) =
        build_history_entries(&parsed.analysis, &parsed.sources, keyword);
    let files = list_history_files();
    let mut state = store();
    state.history = HistoryTab {
        level: HistoryLevel::Entries,
        keyword: keyword.to_string(),
        files,
        current: Some(id.to_string()),
        current_name: snapshot.name,
        entries,
        total,
        truncated,
        preview: state.history.preview,
        preview_line: None,
        ran: !keyword.trim().is_empty(),
    };
    true
}

fn build_history_entries(
    analysis: &LogAnalysis,
    sources: &LogSources,
    keyword: &str,
) -> (Vec<HistoryEntry>, usize, bool) {
    let keyword = keyword.trim().to_lowercase();
    let mut out = Vec::new();
    let mut total = 0usize;
    for entry in &analysis.entries {
        if !entry.has_sql() {
            continue;
        }
        let source = sources.source(entry.line_no).cloned();
        let location = history_location(source.as_ref());
        if !keyword.is_empty() {
            let haystack = format!(
                "{}\n{}\n{}",
                entry.sql.to_lowercase(),
                location.to_lowercase(),
                source
                    .as_ref()
                    .map(|value| value.key().to_lowercase())
                    .unwrap_or_default()
            );
            if !haystack.contains(&keyword) {
                continue;
            }
        }
        total += 1;
        if out.len() >= HISTORY_MAX_ENTRIES {
            continue;
        }
        out.push(HistoryEntry {
            line: entry.line_no,
            time: entry
                .timestamp_ms
                .map(format_epoch_ms)
                .unwrap_or_else(|| "-".to_string()),
            duration_ms: entry.duration_ms,
            source,
            location,
            sql: entry.sql.clone(),
            summary: truncate_chars(&entry.sql, SQL_DISPLAY_CHARS),
            has_sql: entry.has_sql(),
        });
    }
    (out, total, total > HISTORY_MAX_ENTRIES)
}

fn history_location(source: Option<&SourceRef>) -> String {
    let Some(source) = source else {
        return String::new();
    };
    let java = indexer::java_methods();
    match java.lookup_source_unique(source) {
        Some(location) => {
            let name = location
                .file
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&location.file);
            format!("{name}:{}", location.line + 1)
        }
        None => source.key(),
    }
}

/// Rebuilds the entries level's filter from the opened snapshot.
fn filter_history(keyword: &str) {
    let current = store().history.current.clone();
    let Some(id) = current else {
        return;
    };
    open_history_file(&id, keyword);
}

/// `history` op: filter files (level 1) or statements (level 2).
fn run_history(keyword: &str) {
    let keyword = keyword.trim().to_string();
    {
        let mut state = store();
        state.history.keyword = keyword.clone();
        state.history.ran = !keyword.is_empty();
        state.history.preview_line = None;
    }
    let level = store().history.level;
    match level {
        HistoryLevel::Files => {
            let all = list_history_files();
            let folded = keyword.to_lowercase();
            let files: Vec<HistoryFile> = if folded.is_empty() {
                all
            } else {
                all.into_iter()
                    .filter(|file| file.name.to_lowercase().contains(&folded))
                    .collect()
            };
            let total = files.len();
            let mut state = store();
            state.history.files = files;
            state.history.total = total;
        }
        HistoryLevel::Entries => filter_history(&keyword),
    }
}

fn history_entry(line: usize) -> Option<HistoryEntry> {
    store()
        .history
        .entries
        .iter()
        .find(|entry| entry.line == line)
        .cloned()
}

fn reveal_java_source(source: &SourceRef) -> Result<(), Error> {
    let location = indexer::java_methods()
        .lookup_source_unique(source)
        .map(|location| (location.file.clone(), location.line));
    match location {
        Some((file, line)) => reveal(&file, line),
        None => Ok(()),
    }
}

fn reveal_xml_source(source: &SourceRef) -> Result<(), Error> {
    let location = indexer::xml_statements()
        .lookup_unique(&source.class_name, &source.method)
        .map(|location| (location.file.clone(), location.line));
    match location {
        Some((file, line)) => reveal(&file, line),
        None => Ok(()),
    }
}
// ---------------------------------------------------------------------------
// Ops (the dispatch in `mod.rs` calls one of these per arm)
// ---------------------------------------------------------------------------

/// `history-list`: the file list. Listing saves the current buffer first (the
/// reference's auto-saver).
pub(super) fn list() -> Result<(), Error> {
    let _ = save_history(None);
    let files = list_history_files();
    let total = files.len();
    let keyword = store().history.keyword.clone();
    {
        let mut state = store();
        state.history = HistoryTab {
            level: HistoryLevel::Files,
            files,
            total,
            keyword: keyword.clone(),
            ..HistoryTab::default()
        };
    }
    if !keyword.trim().is_empty() {
        run_history(&keyword);
    }
    let _ = push_state();
    Ok(())
}

/// `history-open-file`: opens one snapshot at the entries level.
pub(super) fn open_file(message: &Value) -> Result<(), Error> {
    let id = message.get("id").and_then(Value::as_str).unwrap_or("");
    if open_history_file(id, "") {
        let _ = push_state();
    }
    Ok(())
}

/// `history-back`: returns to the files level.
pub(super) fn back() -> Result<(), Error> {
    let mut state = store();
    state.history.level = HistoryLevel::Files;
    state.history.current = None;
    state.history.current_name.clear();
    state.history.entries.clear();
    state.history.keyword.clear();
    state.history.ran = false;
    state.history.total = state.history.files.len();
    state.history.preview_line = None;
    drop(state);
    let _ = push_state();
    Ok(())
}

/// `history-refresh`: re-reads the current level.
pub(super) fn refresh() -> Result<(), Error> {
    if store().history.level == HistoryLevel::Files {
        let _ = save_history(None);
        let files = list_history_files();
        let total = files.len();
        let mut state = store();
        state.history.files = files;
        state.history.total = total;
    } else {
        let keyword = store().history.keyword.clone();
        filter_history(&keyword);
    }
    let _ = push_state();
    Ok(())
}

/// `history`: filters the files (level 1) or the statements (level 2).
pub(super) fn run(message: &Value) -> Result<(), Error> {
    let keyword = message.get("keyword").and_then(Value::as_str).unwrap_or("");
    run_history(keyword);
    let _ = push_state();
    Ok(())
}

/// `history-open`: an entry opens the preview workbench (the reference's
/// double-click).
pub(super) fn open(message: &Value, ctx: &Invocation) -> Result<(), Error> {
    if let Some(line) = message_line(message)
        && let Some(entry) = history_entry(line)
        && !entry.sql.trim().is_empty()
    {
        crate::commands::open_workbench::run(ctx, Some(&entry.sql))?;
    }
    Ok(())
}

/// `history-copy`: copies one entry's full statement.
pub(super) fn copy(message: &Value) -> Result<(), Error> {
    if let Some(line) = message_line(message)
        && let Some(entry) = history_entry(line)
    {
        host::copy_to_clipboard(&entry.sql)?;
    }
    Ok(())
}

/// `history-jump-java`: opens the entry's Java method.
pub(super) fn jump_java(message: &Value) -> Result<(), Error> {
    if let Some(line) = message_line(message)
        && let Some(source) = history_entry(line).and_then(|entry| entry.source)
    {
        reveal_java_source(&source)?;
    }
    Ok(())
}

/// `history-jump-xml`: opens the entry's mapper XML statement.
pub(super) fn jump_xml(message: &Value) -> Result<(), Error> {
    if let Some(line) = message_line(message)
        && let Some(source) = history_entry(line).and_then(|entry| entry.source)
    {
        reveal_xml_source(&source)?;
    }
    Ok(())
}

/// `history-preview`: shows one entry in the quick preview.
pub(super) fn preview(message: &Value) -> Result<(), Error> {
    if let Some(line) = message_line(message) {
        let mut state = store();
        state.history.preview = true;
        state.history.preview_line = Some(line);
    }
    let _ = push_state();
    Ok(())
}

/// `history-preview-toggle`: opens or closes the quick preview.
pub(super) fn preview_toggle(message: &Value) -> Result<(), Error> {
    let on = message.get("on").and_then(Value::as_bool).unwrap_or(false);
    let mut state = store();
    state.history.preview = on;
    if !on {
        state.history.preview_line = None;
    }
    drop(state);
    let _ = push_state();
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::log_panel::set_log_text;
    use crate::views::log_panel::test_support::{
        MIXED_LOG, handle_panel_message, state_of, test_host,
    };

    #[test]
    fn history_lists_saved_files_then_statements_and_filters_both_levels() {
        let (host, _guard) = test_host();
        set_log_text(MIXED_LOG);

        // Listing saves the buffer and shows the file level.
        handle_panel_message(&host, "{\"op\":\"history-list\"}").unwrap();
        let tab = state_of(&host)["tabs"]["history"].clone();
        assert_eq!(tab["level"], "files", "{tab}");
        let files = tab["files"].as_array().unwrap();
        assert_eq!(files.len(), 1, "{tab}");
        assert_eq!(files[0]["count"], json!(3), "three parsed statements");
        assert!(
            files[0]["name"]
                .as_str()
                .is_some_and(|name| name.ends_with(".log"))
        );
        let id = files[0]["id"].as_str().unwrap().to_string();

        // Opening the file switches to the entries level.
        handle_panel_message(
            &host,
            &format!("{{\"op\":\"history-open-file\",\"id\":\"{id}\"}}"),
        )
        .unwrap();
        let tab = state_of(&host)["tabs"]["history"].clone();
        assert_eq!(tab["level"], "entries", "{tab}");
        assert_eq!(tab["current"], json!(id));
        assert!(!tab["entries"].as_array().unwrap().is_empty());

        // Keyword search filters statements, not files.
        handle_panel_message(&host, "{\"op\":\"history\",\"keyword\":\"users\"}").unwrap();
        let tab = state_of(&host)["tabs"]["history"].clone();
        assert_eq!(tab["ran"], true);
        assert_eq!(tab["total"], 2, "{tab}");
        let entries = tab["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|item| item["has_sql"] == true));
        assert!(
            entries[0]["sql"]
                .as_str()
                .is_some_and(|sql| sql.contains("SELECT id FROM users")),
            "{entries:?}"
        );

        // No match is a result, not an error.
        handle_panel_message(&host, "{\"op\":\"history\",\"keyword\":\"no-such-line\"}").unwrap();
        let tab = state_of(&host)["tabs"]["history"].clone();
        assert_eq!(tab["total"], 0);
        assert!(tab["entries"].as_array().unwrap().is_empty());

        // Back returns to the files level.
        handle_panel_message(&host, "{\"op\":\"history-back\"}").unwrap();
        let tab = state_of(&host)["tabs"]["history"].clone();
        assert_eq!(tab["level"], "files");
        assert_eq!(tab["current"], Value::Null);
    }

    #[test]
    fn history_saves_each_buffer_once_and_filters_file_names() {
        let (host, _guard) = test_host();
        set_log_text(MIXED_LOG);
        handle_panel_message(&host, "{\"op\":\"history-list\"}").unwrap();
        assert_eq!(
            state_of(&host)["tabs"]["history"]["files"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        // The same buffer is not saved twice.
        handle_panel_message(&host, "{\"op\":\"history-list\"}").unwrap();
        assert_eq!(
            state_of(&host)["tabs"]["history"]["files"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        // A different buffer becomes a second file.
        set_log_text("Preparing: SELECT 1 FROM dual\nParameters: \n");
        handle_panel_message(&host, "{\"op\":\"history-list\"}").unwrap();
        let tab = state_of(&host)["tabs"]["history"].clone();
        assert_eq!(tab["files"].as_array().unwrap().len(), 2, "{tab}");

        // At the files level the keyword filters by file name.
        handle_panel_message(&host, "{\"op\":\"history\",\"keyword\":\"no-such-file\"}").unwrap();
        let tab = state_of(&host)["tabs"]["history"].clone();
        assert_eq!(tab["total"], 0, "{tab}");
        assert!(tab["files"].as_array().unwrap().is_empty());
    }

    #[test]
    fn history_actions_open_preview_copy_and_toggle_quick_preview() {
        let (host, _guard) = test_host();
        // Lane L48: the history double-click opens whatever shell the setting
        // names; this test follows the tab shell's `selectTab` hand-off. The
        // dialog default is covered in `views::workbench` and
        // `open_line_message_opens_the_preview_dialog`.
        host.set_setting("preview.presentation", "\"tab\"");
        set_log_text(MIXED_LOG);
        handle_panel_message(&host, "{\"op\":\"history-list\"}").unwrap();
        let id = state_of(&host)["tabs"]["history"]["files"][0]["id"]
            .as_str()
            .unwrap()
            .to_string();
        handle_panel_message(
            &host,
            &format!("{{\"op\":\"history-open-file\",\"id\":\"{id}\"}}"),
        )
        .unwrap();
        let entries = state_of(&host)["tabs"]["history"]["entries"]
            .as_array()
            .unwrap()
            .clone();
        let line = entries[0]["line"].as_u64().unwrap();

        // The reference's double-click opens the SQL preview workbench.
        handle_panel_message(
            &host,
            &format!("{{\"op\":\"history-open\",\"line\":{line}}}"),
        )
        .unwrap();
        assert_eq!(state_of(&host)["selectTab"], "preview");

        // Copy hands the full statement to the clipboard.
        handle_panel_message(
            &host,
            &format!("{{\"op\":\"history-copy\",\"line\":{line}}}"),
        )
        .unwrap();
        assert!(
            host.clipboard().is_some_and(|text| !text.trim().is_empty()),
            "{:?}",
            host.clipboard()
        );

        // Quick preview keeps the selected line until toggled off.
        handle_panel_message(
            &host,
            &format!("{{\"op\":\"history-preview\",\"line\":{line}}}"),
        )
        .unwrap();
        let tab = state_of(&host)["tabs"]["history"].clone();
        assert_eq!(tab["preview"], true);
        assert_eq!(tab["previewLine"], json!(line));
        handle_panel_message(&host, "{\"op\":\"history-preview-toggle\",\"on\":false}").unwrap();
        let tab = state_of(&host)["tabs"]["history"].clone();
        assert_eq!(tab["preview"], false);
        assert_eq!(tab["previewLine"], Value::Null);
    }
}
