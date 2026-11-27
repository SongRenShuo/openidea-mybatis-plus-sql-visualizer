//! The log list's ops (lane W2-0).
//!
//! Split out of `views::log_panel` with no behaviour change: row actions
//! (jump / delete / block), the toolbar actions (pause, clear, copy, export,
//! settings, help, community, capture), the sort / follow / group ops and the
//! filter-message parser. `mod.rs` keeps the op dispatch and calls one function
//! per arm here.

use std::collections::BTreeSet;

use openidea_plugin::{Error, Invocation, NotificationLevel, host};
use serde_json::Value;

use crate::commands::common;
use crate::core::log::SourceRef;
use crate::i18n::t;
use crate::indexer;
use crate::settings;

use super::rows::{
    PanelRow, SqlType, apply_sort, build_rows_with, dedupe_key_token, display_rows, filter_rows,
    n_plus_one_key_token,
};
use super::state::{
    Filter, Focus, FocusKind, SortMode, TypeFilter, buffer_cap, clear, dedupe_active,
    ensure_parsed, set_filter, slow_threshold, snapshot, store, toggle_pause,
};
use super::suppress;
use super::{VIEW_ID, format_epoch_ms, message_line, push_state};

// ---------------------------------------------------------------------------
// Row actions: jump / focus / delete / block (lane L28)
// ---------------------------------------------------------------------------
/// Opens `file` at the 0-based `line`, falling back to a visible copy when the
/// host refuses (a window-less call, a closed project).
///
/// The fallback is what `docs/LIMITS.md` promises for a host that cannot open
/// files: the position still reaches the user, on the clipboard and in the
/// Output channel.
pub(super) fn reveal(file: &str, line: u32) -> Result<(), Error> {
    match host::open_location(file, line, 0) {
        Ok(()) => Ok(()),
        Err(_) => {
            let position = format!("{file}:{}", line + 1);
            let _ = host::copy_to_clipboard(&position);
            common::emit(&format!(
                "-- {} ({position})\n",
                t("view.logs.jump.unavailable")
            ))?;
            Ok(())
        }
    }
}

/// Jumps to the row's Java method.
fn jump_java(line_no: usize) -> Result<(), Error> {
    let target = {
        let mut state = store();
        ensure_parsed(&mut state);
        let source = state.sources.source(line_no).cloned();
        drop(state);
        source
    };
    let Some(source) = target else {
        return Ok(());
    };
    let location = {
        let java = indexer::java_methods();
        java.lookup_source_unique(&source)
            .map(|location| (location.file.clone(), location.line))
    };
    match location {
        Some((file, line)) => reveal(&file, line),
        None => Ok(()),
    }
}

/// Jumps to the row's mapper XML statement.
fn jump_xml(line_no: usize) -> Result<(), Error> {
    let target = {
        let mut state = store();
        ensure_parsed(&mut state);
        let source = state.sources.source(line_no).cloned();
        drop(state);
        source
    };
    let Some(source) = target else {
        return Ok(());
    };
    let location = {
        let xml = indexer::xml_statements();
        xml.lookup_unique(&source.class_name, &source.method)
            .map(|location| (location.file.clone(), location.line))
    };
    match location {
        Some((file, line)) => reveal(&file, line),
        None => Ok(()),
    }
}

/// Removes one line from the buffer (`删除`). The line number is what the page
/// has; the text itself is left alone, so the numbering of every other row
/// stays valid.
fn remove_line(line_no: usize) {
    store().removed.insert(line_no);
}

/// Blocks the row's `class_name#method` for this session (`屏蔽此方法`).
///
/// Returns the key that was blocked, if the row had a source.
fn block_line(line_no: usize) -> Option<String> {
    let mut state = store();
    ensure_parsed(&mut state);
    let key = state.sources.source(line_no).map(SourceRef::key);
    if let Some(key) = &key {
        state.blocked.insert(key.clone());
    }
    key
}

/// Clears the session's blocked methods.
fn unblock_all() {
    store().blocked.clear();
}

/// Flips the expanded state of the group the row's `kind` badge toggles
/// (lane L26).
///
/// `kind` is `dedupe` (the ⨉N badge) or `group` (the ×N badge). A row that
/// states no such group is a no-op: the page only makes the badge clickable
/// when the key exists.
fn toggle_group(line_no: usize, kind: &str) {
    let mut state = store();
    ensure_parsed(&mut state);
    let rows = display_rows(
        &state.analysis,
        &state.sources,
        &state.removed,
        &state.blocked,
        dedupe_active(&state),
        slow_threshold(),
        &state.expanded,
    );
    let Some(row) = rows.iter().find(|row| row.line_no == line_no) else {
        return;
    };
    let key = match kind {
        "dedupe" => row.dedupe_key.clone(),
        _ => row.n_plus_one_key.clone(),
    };
    if key.is_empty() {
        return;
    }
    let combined = if kind == "dedupe" {
        dedupe_key_token(&key)
    } else {
        n_plus_one_key_token(&key)
    };
    if state.expanded.contains(&combined) {
        state.expanded.remove(&combined);
    } else {
        state.expanded.insert(combined);
    }
}

/// The one-screen shortcut/interaction cheat sheet the help button prints.
fn help_text() -> String {
    let lines = [
        t("view.logs.help.title"),
        t("view.logs.help.search"),
        t("view.logs.help.tab"),
        t("view.logs.help.dblclick"),
        t("view.logs.help.menu"),
        t("view.logs.help.scroll"),
        t("view.logs.help.sort"),
        t("view.logs.help.blocked"),
    ];
    let mut out = String::new();
    for line in lines {
        out.push_str("-- ");
        out.push_str(&line);
        out.push('\n');
    }
    out
}
fn filter_from_message(message: &Value) -> Filter {
    let selected: BTreeSet<SqlType> = message
        .get("types")
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str())
                .filter_map(SqlType::parse)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let types = match message
        .get("types_mode")
        .and_then(|value| value.as_str())
        .unwrap_or("")
    {
        "none" => TypeFilter::None,
        "only" => TypeFilter::Only(selected),
        "all" => TypeFilter::All,
        // No mode: an empty selection means "all" (backward-compatible).
        _ if selected.is_empty() => TypeFilter::All,
        _ => TypeFilter::Only(selected),
    };
    let keyword = message
        .get("keyword")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    // A focus chip rides the same `filter` op as the other filters; `null` or a
    // missing value clears it.
    let focus = message.get("focus").and_then(|value| {
        let kind = FocusKind::parse(value.get("kind")?.as_str()?)?;
        let value = value.get("value")?.as_str()?.trim();
        (!value.is_empty()).then(|| Focus {
            kind,
            value: value.to_string(),
        })
    });
    Filter {
        types,
        slow_only: message
            .get("slow_only")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        keyword,
        match_case: message
            .get("match_case")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        regex: message
            .get("regex")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        dedupe: message
            .get("dedupe")
            .and_then(|value| value.as_bool())
            .unwrap_or(true),
        focus,
    }
}
pub(super) fn all_sql() -> String {
    // Suppressed statements were never meant to be seen again; "Copy all" and
    // the export keep them out, like the rows themselves (lane E).
    let suppression = suppress::matcher();
    snapshot()
        .entries
        .iter()
        .filter(|entry| entry.has_sql() && !suppression.hidden(&entry.sql))
        .map(|entry| entry.sql.as_str())
        .collect::<Vec<_>>()
        .join(";\n")
}

/// SQL of the visible (filtered + capped) rows, joined for the export action.
///
/// The rows leave in the order the list shows them (lane A): the reference's
/// `ExportVisibleAction` exports the visible entries, so the export follows the
/// active sort mode instead of capture order.
pub fn visible_sql() -> String {
    let mut state = store();
    ensure_parsed(&mut state);
    let slow_threshold_ms = slow_threshold();
    let dedupe = settings::get_bool("log.dedupe", true);
    let rows = build_rows_with(
        &state.analysis,
        Some(&state.sources),
        &state.removed,
        &state.blocked,
        dedupe,
        slow_threshold_ms,
    );
    let filter = state.filter.clone();
    let sort = state.sort;
    drop(state);
    // A suppressed row is not part of what the list shows, so it is not part of
    // what "Export visible" exports either (lane E).
    let suppression = suppress::matcher();
    let rows: Vec<PanelRow> = if suppression.is_empty() {
        rows
    } else {
        rows.into_iter()
            .filter(|row| !suppression.hidden(&row.sql))
            .collect()
    };
    let filtered = filter_rows(&rows, &filter, slow_threshold_ms);
    let cap = buffer_cap();
    let start = filtered.len().saturating_sub(cap);
    let mut shown: Vec<&PanelRow> = filtered[start..].iter().collect();
    apply_sort(&mut shown, sort);
    shown
        .iter()
        .filter(|row| !row.sql.is_empty())
        .map(|row| format!("{};", row.sql))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn sql_of_line(line_no: usize) -> Option<String> {
    snapshot()
        .entries
        .into_iter()
        .find(|entry| entry.line_no == line_no)
        .map(|entry| entry.sql)
        .filter(|sql| !sql.is_empty())
}

fn time_of_line(line_no: usize) -> Option<String> {
    snapshot()
        .entries
        .into_iter()
        .find(|entry| entry.line_no == line_no)
        .and_then(|entry| entry.timestamp_ms)
        .map(format_epoch_ms)
}
// ---------------------------------------------------------------------------
// Ops (the dispatch in `mod.rs` calls one of these per arm)
// ---------------------------------------------------------------------------

/// `filter`: replaces the whole filter and pushes the narrowed rows.
pub(super) fn filter(message: &Value) -> Result<(), Error> {
    set_filter(filter_from_message(message));
    let _ = push_state();
    Ok(())
}

/// `toggle-pause`: flips the refresh pause and refreshes the view.
pub(super) fn toggle_pause_op() -> Result<(), Error> {
    toggle_pause();
    let _ = push_state();
    host::refresh_view(VIEW_ID);
    Ok(())
}

/// `clear`: drops the buffered text and the derived groups.
pub(super) fn clear_op() -> Result<(), Error> {
    clear();
    let _ = push_state();
    host::refresh_view(VIEW_ID);
    Ok(())
}

/// `copy-all`: the visible SQL onto the clipboard.
pub(super) fn copy_all() -> Result<(), Error> {
    let sql = visible_sql();
    if !sql.is_empty() {
        host::copy_to_clipboard(&sql)?;
    }
    Ok(())
}

/// `copy-line`: one row's full statement.
pub(super) fn copy_line(message: &Value) -> Result<(), Error> {
    if let Some(line) = message_line(message) {
        let sql = sql_of_line(line).unwrap_or_default();
        if !sql.is_empty() {
            host::copy_to_clipboard(&sql)?;
        }
    }
    Ok(())
}

/// `copy-time`: one row's timestamp.
pub(super) fn copy_time(message: &Value) -> Result<(), Error> {
    if let Some(line) = message_line(message)
        && let Some(time) = time_of_line(line)
    {
        host::copy_to_clipboard(&time)?;
    }
    Ok(())
}

/// `export`: the visible rows to the Output channel.
pub(super) fn export() -> Result<(), Error> {
    common::emit(&visible_sql())?;
    Ok(())
}

/// `open-line`: the row's statement in the preview workbench.
pub(super) fn open_line(message: &Value, ctx: &Invocation) -> Result<(), Error> {
    if let Some(line) = message_line(message) {
        let sql = sql_of_line(line).unwrap_or_default();
        if !sql.is_empty() {
            crate::commands::open_workbench::run(ctx, Some(&sql))?;
        }
    }
    Ok(())
}

/// `jump-java`: the row's Java method.
pub(super) fn jump_java_op(message: &Value) -> Result<(), Error> {
    if let Some(line) = message_line(message) {
        jump_java(line)?;
    }
    Ok(())
}

/// `jump-xml`: the row's mapper XML statement.
pub(super) fn jump_xml_op(message: &Value) -> Result<(), Error> {
    if let Some(line) = message_line(message) {
        jump_xml(line)?;
    }
    Ok(())
}

/// `delete-line`: drops one line from the displayed rows.
pub(super) fn delete_line(message: &Value) -> Result<(), Error> {
    if let Some(line) = message_line(message) {
        remove_line(line);
        let _ = push_state();
    }
    Ok(())
}

/// `block-method`: mutes the row's `class_name#method` for this session.
pub(super) fn block_method(message: &Value) -> Result<(), Error> {
    if let Some(line) = message_line(message)
        && block_line(line).is_some()
    {
        let _ = push_state();
    }
    Ok(())
}

/// `clear-blocked`: restores every muted method.
pub(super) fn clear_blocked() -> Result<(), Error> {
    unblock_all();
    let _ = push_state();
    Ok(())
}

/// `manage-suppress`: opens the suppression-rule manager (lane E).
///
/// A dialog is revealed from a command or a view event — this is one.
pub(super) fn manage_suppress(ctx: &Invocation) -> Result<(), Error> {
    suppress::reveal(ctx)
}

/// `capture-start`: forwards to the capture command, which writes its own
/// Output lines (connected / already connected / the failure reason).
pub(super) fn capture_start(ctx: &Invocation) -> Result<(), Error> {
    let _ = crate::commands::capture_start::run(ctx, None);
    let _ = push_state();
    Ok(())
}

/// `capture-stop`: forwards to the capture command.
pub(super) fn capture_stop(ctx: &Invocation) -> Result<(), Error> {
    let _ = crate::commands::capture_stop::run(ctx, None);
    let _ = push_state();
    Ok(())
}

/// `capture-retry`: the page ticks this every 2 s while a refused capture start
/// waits for the target application. A tick with nothing pending is a no-op,
/// and a tick after `capture-stop` finds no waiting state at all.
pub(super) fn capture_retry(ctx: &Invocation) -> Result<(), Error> {
    if crate::capture::bridge::waiting().is_some() {
        crate::commands::capture_start::retry(ctx)?;
        let _ = push_state();
    }
    Ok(())
}

/// `capture-poll`: drain what the agent has already sent.
///
/// The host emits `websocket-ready` per arriving frame, but that wake only fires
/// on the transition into a non-empty queue; a wake lost while the plugin was
/// idle leaves frames staged with nothing to wake us again. The page ticks this
/// while capture is on, so the queue always drains.
pub(super) fn capture_poll() -> Result<(), Error> {
    match crate::capture::bridge::poll(super::now_ms()) {
        Ok(report) => {
            if report.fetched > 0 {
                let _ = push_state();
            }
        }
        // A tick every 500 ms must not turn "nothing open" into an error.
        Err(crate::capture::bridge::BridgeError::NotConnected) => {}
        Err(error) => {
            host::log(
                openidea_plugin::LogLevel::Warn,
                &format!("capture: {} {}", error.message_key(), error.detail()),
            );
        }
    }
    Ok(())
}

/// `follow`: the page reports a flip when the user scrolls away from the bottom.
pub(super) fn follow(message: &Value) -> Result<(), Error> {
    let follow = message
        .get("follow")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    store().follow = follow;
    let _ = push_state();
    Ok(())
}

/// `scroll-end`: "scroll to bottom" restores the follow.
pub(super) fn scroll_end() -> Result<(), Error> {
    store().follow = true;
    let _ = push_state();
    host::refresh_view(VIEW_ID);
    Ok(())
}

/// `sort`: the page's sort popup posts one of the seven tokens; an unknown or
/// legacy token is ignored rather than guessed.
pub(super) fn sort(message: &Value) -> Result<(), Error> {
    if let Some(mode) = message
        .get("sort")
        .and_then(Value::as_str)
        .and_then(SortMode::parse)
    {
        store().sort = mode;
        let _ = push_state();
    }
    Ok(())
}

/// `toggle-group`: the ⨉N / ×N badges toggle their group.
pub(super) fn toggle_group_op(message: &Value) -> Result<(), Error> {
    if let Some(line) = message_line(message) {
        let kind = message
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("group");
        toggle_group(line, kind);
        let _ = push_state();
    }
    Ok(())
}

/// `settings`: the platform has no API to open the settings page, so the button
/// points the user there through a notification and the Output channel instead
/// of doing nothing.
pub(super) fn settings() -> Result<(), Error> {
    host::notify(
        NotificationLevel::Info,
        &t("view.logs.settings.title"),
        &t("view.logs.settings.guide"),
    );
    common::emit(&format!(
        "-- {}\n{}\n",
        t("view.logs.settings.title"),
        t("view.logs.settings.guide")
    ))
}

/// `help`: a one-screen shortcut/interaction cheat sheet in the Output channel.
pub(super) fn help() -> Result<(), Error> {
    common::emit(&help_text())
}

/// `community`: this build ships no QR asset and the platform has no dialog API
/// for one, so the entry points at the project's community page the way the
/// settings entry points at the settings page, instead of doing nothing.
pub(super) fn community() -> Result<(), Error> {
    host::notify(
        NotificationLevel::Info,
        &t("view.logs.community.title"),
        &t("view.logs.community.guide"),
    );
    common::emit(&format!(
        "-- {}\n{}\n",
        t("view.logs.community.title"),
        t("view.logs.community.guide")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::views::log_panel::buffered_len;
    use crate::views::log_panel::set_log_text;
    use crate::views::log_panel::test_support::{
        MIXED_LOG, SOURCED_LOG, USER_MAPPER_JAVA, USER_MAPPER_XML, first_line_with,
        handle_panel_message, row_of_line, row_tables, state_of, test_host,
    };

    #[test]
    fn filter_message_narrows_the_pushed_rows() {
        let (host, _guard) = test_host();
        set_log_text(MIXED_LOG);
        handle_panel_message(
            &host,
            "{\"op\":\"filter\",\"types\":[\"select\"],\"types_mode\":\"only\",\"keyword\":\"\"}",
        )
        .unwrap();
        assert!(matches!(store().filter.types, TypeFilter::Only(_)));
        let state = state_of(&host);
        assert_eq!(state["filter"]["types_mode"], "only");
        let rows = state["rows"].as_array().unwrap();
        assert!(
            rows.iter().all(|row| row["sql_type"] == "select"),
            "{rows:?}"
        );
    }

    #[test]
    fn toggle_pause_message_flips_state() {
        let (host, _guard) = test_host();
        handle_panel_message(&host, "{\"op\":\"toggle-pause\"}").unwrap();
        assert!(store().paused);
        assert!(host.refreshed_views().contains(&VIEW_ID.to_string()));
        handle_panel_message(&host, "{\"op\":\"toggle-pause\"}").unwrap();
        assert!(!store().paused);
    }

    #[test]
    fn clear_message_empties_the_buffer() {
        let (host, _guard) = test_host();
        set_log_text(MIXED_LOG);
        handle_panel_message(&host, "{\"op\":\"clear\"}").unwrap();
        assert_eq!(buffered_len(), 0);
        let state = state_of(&host);
        assert!(state["rows"].as_array().unwrap().is_empty());
    }

    #[test]
    fn copy_line_message_copies_the_statement() {
        let (host, _guard) = test_host();
        set_log_text(MIXED_LOG);
        let line = snapshot()
            .entries
            .iter()
            .find(|entry| entry.has_sql())
            .unwrap()
            .line_no;
        handle_panel_message(&host, &format!("{{\"op\":\"copy-line\",\"line\":{line}}}")).unwrap();
        assert!(host.clipboard().is_some_and(|text| !text.is_empty()));
    }

    #[test]
    fn copy_all_message_puts_the_visible_sql_on_the_clipboard() {
        let (host, _guard) = test_host();
        set_log_text(MIXED_LOG);
        handle_panel_message(&host, "{\"op\":\"copy-all\"}").unwrap();
        let copied = host.clipboard().expect("the clipboard was written");
        assert!(
            copied.contains("SELECT") || copied.contains("DELETE"),
            "{copied}"
        );
    }

    #[test]
    fn export_message_writes_the_visible_rows_to_output() {
        let (host, _guard) = test_host();
        set_log_text(MIXED_LOG);
        handle_panel_message(&host, "{\"op\":\"export\"}").unwrap();
        let out = &host.output()[common::CHANNEL];
        assert!(out.contains("SELECT") || out.contains("DELETE"), "{out}");
    }

    #[test]
    fn open_line_message_reveals_the_log_panel_on_the_preview_tab() {
        let (host, _guard) = test_host();
        // Lane L48: this is the tab shell's behaviour; the dialog (the default)
        // is covered by `open_line_message_opens_the_preview_dialog` below.
        host.set_setting("preview.presentation", "\"tab\"");
        set_log_text(MIXED_LOG);
        let line = snapshot()
            .entries
            .iter()
            .find(|entry| entry.has_sql())
            .unwrap()
            .line_no;
        handle_panel_message(&host, &format!("{{\"op\":\"open-line\",\"line\":{line}}}")).unwrap();
        assert_eq!(
            host.revealed_views()
                .iter()
                .filter(|view| *view == VIEW_ID)
                .count(),
            1,
            "double-click reveals the log panel, not a separate view"
        );
        let state = state_of(&host);
        assert_eq!(state["selectTab"], "preview");
        assert!(
            state["tabs"]["preview"]["model"]["sql"]
                .as_str()
                .is_some_and(|sql| !sql.is_empty()),
            "the statement reaches the preview tab"
        );
    }

    #[test]
    fn open_line_message_opens_the_preview_dialog() {
        // Lane L48: with the default setting the row opens the modal dialog
        // instead of pulling the tool window forward — the statement is posted
        // to `sqlv-preview`, and a closed dialog asks for it again on open.
        let (host, _guard) = test_host();
        set_log_text(MIXED_LOG);
        let line = snapshot()
            .entries
            .iter()
            .find(|entry| entry.has_sql())
            .unwrap()
            .line_no;
        handle_panel_message(&host, &format!("{{\"op\":\"open-line\",\"line\":{line}}}")).unwrap();
        assert_eq!(
            host.revealed_views(),
            [crate::views::workbench::DIALOG_VIEW_ID.to_string()],
            "the dialog is revealed, not the tool window"
        );
        assert!(
            !host.revealed_views().contains(&VIEW_ID.to_string()),
            "no tool window comes forward over the editor"
        );
        let posted = host
            .view_messages()
            .iter()
            .find(|(view, _)| view == crate::views::workbench::DIALOG_VIEW_ID)
            .map(|(_, text)| serde_json::from_str::<serde_json::Value>(text).unwrap())
            .expect("the dialog receives its payload");
        assert_eq!(posted["op"], "preview-tab");
        assert!(
            posted["tab"]["model"]["sql"]
                .as_str()
                .is_some_and(|sql| !sql.is_empty()),
            "the statement reaches the dialog"
        );
    }

    #[test]
    fn a_focus_chip_filters_by_method_and_by_request() {
        let (host, _guard) = test_host();
        set_log_text(SOURCED_LOG);
        let users = first_line_with(&snapshot().entries, "FROM users");

        // Method focus: only the UserMapper statement survives.
        handle_panel_message(
            &host,
            "{\"op\":\"filter\",\"types\":[],\"types_mode\":\"all\",\
             \"focus\":{\"kind\":\"method\",\"value\":\"c.e.m.mapper.UserMapper#selectById\"}}",
        )
        .unwrap();
        let state = state_of(&host);
        assert_eq!(
            state["filter"]["focus"],
            json!({"kind": "method", "value": "c.e.m.mapper.UserMapper#selectById"})
        );
        let rows = state["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["line"], json!(users));
        assert_eq!(state["counts"]["total"], 1);

        // Request focus: only the row with that correlation id survives.
        handle_panel_message(
            &host,
            "{\"op\":\"filter\",\"types\":[],\"types_mode\":\"all\",\
             \"focus\":{\"kind\":\"request\",\"value\":\"abc123\"}}",
        )
        .unwrap();
        let rows = state_of(&host)["rows"].as_array().cloned().unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["request_id"], "abc123");

        // A null focus clears the chip and brings every row back.
        handle_panel_message(
            &host,
            "{\"op\":\"filter\",\"types\":[],\"types_mode\":\"all\",\"focus\":null}",
        )
        .unwrap();
        let state = state_of(&host);
        assert_eq!(state["filter"]["focus"], Value::Null);
        assert_eq!(state["rows"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn delete_drops_one_row_and_a_new_log_text_forgets_the_deletions() {
        let (host, _guard) = test_host();
        set_log_text(SOURCED_LOG);
        let users = first_line_with(&snapshot().entries, "FROM users");
        handle_panel_message(
            &host,
            &format!("{{\"op\":\"delete-line\",\"line\":{users}}}"),
        )
        .unwrap();
        let state = state_of(&host);
        let rows = state["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_ne!(rows[0]["line"], json!(users));

        // The whole text replaced: line numbers refer to the new text.
        set_log_text(SOURCED_LOG);
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        assert_eq!(state_of(&host)["rows"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn muting_a_method_hides_every_row_of_it_and_the_counter_clears_it() {
        let (host, _guard) = test_host();
        set_log_text(SOURCED_LOG);
        let users = first_line_with(&snapshot().entries, "FROM users");
        handle_panel_message(
            &host,
            &format!("{{\"op\":\"block-method\",\"line\":{users}}}"),
        )
        .unwrap();
        let state = state_of(&host);
        assert_eq!(state["counts"]["blocked"], 1);
        let rows = state["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "the whole method is hidden: {rows:?}");
        assert_eq!(rows[0]["class_simple"], "OrderMapper");

        handle_panel_message(&host, "{\"op\":\"clear-blocked\"}").unwrap();
        let state = state_of(&host);
        assert_eq!(state["counts"]["blocked"], 0);
        assert_eq!(state["rows"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn jump_java_opens_the_unique_method_at_its_line() {
        let (host, _guard) = test_host();
        crate::indexer::index_source(
            "/project/src/main/java/com/example/myapp/mapper/UserMapper.java",
            USER_MAPPER_JAVA,
        );
        set_log_text(SOURCED_LOG);
        let users = first_line_with(&snapshot().entries, "FROM users");
        handle_panel_message(&host, &format!("{{\"op\":\"jump-java\",\"line\":{users}}}")).unwrap();
        assert_eq!(
            host.opened_locations(),
            vec![(
                "/project/src/main/java/com/example/myapp/mapper/UserMapper.java".to_string(),
                3,
                0
            )],
            "0-based line 3 is selectById"
        );
        // The row offers the jump because the index resolves it uniquely.
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        let state = state_of(&host);
        let row = row_of_line(&state, users);
        assert_eq!(row["jump_line"], 4, "1-based for the page");
    }

    #[test]
    fn an_ambiguous_method_is_never_jumped_to() {
        let (host, _guard) = test_host();
        crate::indexer::index_source("/project/a/UserMapper.java", USER_MAPPER_JAVA);
        crate::indexer::index_source("/project/b/UserMapper.java", USER_MAPPER_JAVA);
        set_log_text(SOURCED_LOG);
        let users = first_line_with(&snapshot().entries, "FROM users");
        handle_panel_message(&host, &format!("{{\"op\":\"jump-java\",\"line\":{users}}}")).unwrap();
        assert!(
            host.opened_locations().is_empty(),
            "two candidates: no jump at all"
        );
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        let state = state_of(&host);
        let row = row_of_line(&state, users);
        assert_eq!(row["jump_file"], Value::Null);
        assert_eq!(row["jump_line"], Value::Null);
    }

    #[test]
    fn jump_xml_opens_the_unique_mapper_statement() {
        let (host, _guard) = test_host();
        crate::indexer::index_source(
            "/project/src/main/resources/mapper/UserMapper.xml",
            USER_MAPPER_XML,
        );
        set_log_text(SOURCED_LOG);
        let users = first_line_with(&snapshot().entries, "FROM users");
        handle_panel_message(&host, &format!("{{\"op\":\"jump-xml\",\"line\":{users}}}")).unwrap();
        assert_eq!(
            host.opened_locations(),
            vec![(
                "/project/src/main/resources/mapper/UserMapper.xml".to_string(),
                1,
                0
            )],
            "0-based line 1 is the select tag"
        );
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        let state = state_of(&host);
        let row = row_of_line(&state, users);
        assert_eq!(row["xml_line"], 2, "1-based for the page");
    }

    #[test]
    fn jump_xml_does_nothing_for_a_statement_the_index_does_not_hold() {
        let (host, _guard) = test_host();
        // Only the Java side is indexed: the XML item has nothing to open.
        crate::indexer::index_source(
            "/project/src/main/java/com/example/mapper/UserMapper.java",
            USER_MAPPER_JAVA,
        );
        set_log_text(SOURCED_LOG);
        let users = first_line_with(&snapshot().entries, "FROM users");
        handle_panel_message(&host, &format!("{{\"op\":\"jump-xml\",\"line\":{users}}}")).unwrap();
        assert!(host.opened_locations().is_empty());
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        assert_eq!(
            row_of_line(&state_of(&host), users)["xml_file"],
            Value::Null
        );
    }

    #[test]
    fn capture_start_message_forwards_to_the_capture_command() {
        let (host, _guard) = test_host();
        // Already holding the one test lock via `test_host()`; take only the
        // capture-session reset (a second lock on this thread would deadlock).
        crate::capture::bridge::reset_capture_for_tests();
        host.set_project_root(Some("/project"));
        handle_panel_message(&host, "{\"op\":\"capture-start\"}").unwrap();
        let out = &host.output()[common::CHANNEL];
        // The command owns the Output text; the mock host has no agent, so the
        // ws layer names the failure with its own key.
        assert!(out.contains("-- "), "{out}");
        assert!(
            out.contains("capture.start") || out.contains("capture.ws"),
            "{out}"
        );
        // The op refreshes the state so the status line follows the attempt.
        assert!(host.view_messages().iter().any(|(view, _)| view == VIEW_ID));
    }

    #[test]
    fn capture_retry_ticks_only_while_a_start_is_waiting() {
        // Lane A: the page ticks `capture-retry` every 2 s while a refused start
        // waits for the target application. The host must dial only while the
        // wait exists, and `capture-stop` must end it.
        let (host, _guard) = test_host();
        host.set_project_root(Some("/project"));
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        let pushed = host.view_messages().len();
        handle_panel_message(&host, "{\"op\":\"capture-retry\"}").unwrap();
        assert_eq!(
            host.view_messages().len(),
            pushed,
            "a tick with nothing pending is a no-op"
        );

        handle_panel_message(&host, "{\"op\":\"capture-start\"}").unwrap();
        let state = state_of(&host);
        let waiting = &state["status"]["capture_waiting"];
        assert!(waiting.is_object(), "the refused start waits: {state}");
        assert_eq!(waiting["attempts"], 1, "{waiting}");
        assert!(waiting["port"].as_u64().is_some(), "{waiting}");
        assert!(
            waiting["lastError"]
                .as_str()
                .is_some_and(|text| !text.is_empty()),
            "the refusal reason is kept: {waiting}"
        );

        handle_panel_message(&host, "{\"op\":\"capture-retry\"}").unwrap();
        let state = state_of(&host);
        assert_eq!(
            state["status"]["capture_waiting"]["attempts"], 2,
            "the tick dialled once more"
        );

        handle_panel_message(&host, "{\"op\":\"capture-stop\"}").unwrap();
        let state = state_of(&host);
        assert_eq!(
            state["status"]["capture_waiting"],
            Value::Null,
            "capture-stop ends the wait so the page stops ticking"
        );
    }

    /// The page ticks a drain every 500 ms while capture is on. With nothing
    /// open that tick must be silent: it is a heartbeat, not a user action.
    #[test]
    fn a_drain_tick_with_nothing_open_is_a_silent_no_op() {
        let (host, _guard) = test_host();
        host.set_project_root(Some("/project"));
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        let pushed = host.view_messages().len();
        handle_panel_message(&host, "{\"op\":\"capture-poll\"}").unwrap();
        assert_eq!(host.view_messages().len(), pushed, "nothing to push");
        assert_eq!(host.output().get(common::CHANNEL), None, "no Output noise");
    }

    #[test]
    fn capture_stop_message_forwards_to_the_capture_command() {
        let (host, _guard) = test_host();
        // Already holding the one test lock via `test_host()`; take only the
        // capture-session reset (a second lock on this thread would deadlock).
        crate::capture::bridge::reset_capture_for_tests();
        handle_panel_message(&host, "{\"op\":\"capture-stop\"}").unwrap();
        let out = &host.output()[common::CHANNEL];
        assert!(out.contains("-- capture-stop:"), "{out}");
        assert!(
            out.contains(crate::commands::capture_stop::KEY_STOP_NOT_CONNECTED),
            "{out}"
        );
        assert!(host.view_messages().iter().any(|(view, _)| view == VIEW_ID));
    }

    #[test]
    fn follow_message_mirrors_the_page_flag_in_the_state() {
        let (host, _guard) = test_host();
        handle_panel_message(&host, "{\"op\":\"follow\",\"follow\":false}").unwrap();
        assert_eq!(state_of(&host)["status"]["follow"], false);
        handle_panel_message(&host, "{\"op\":\"follow\",\"follow\":true}").unwrap();
        assert_eq!(state_of(&host)["status"]["follow"], true);
    }

    #[test]
    fn scroll_end_restores_follow_and_refreshes_the_view() {
        let (host, _guard) = test_host();
        store().follow = false;
        handle_panel_message(&host, "{\"op\":\"scroll-end\"}").unwrap();
        assert_eq!(state_of(&host)["status"]["follow"], true);
        assert!(host.refreshed_views().contains(&VIEW_ID.to_string()));
    }

    #[test]
    fn sort_message_orders_by_duration_and_back_by_time() {
        let (host, _guard) = test_host();
        let text = "\
1723456789012|300|statement|SELECT * FROM a
1723456789013|1000|statement|SELECT * FROM b
1723456789014|50|statement|SELECT * FROM c
";
        set_log_text(text);
        // The default is capture order (`TIME_ASC`, the reference's default).
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        assert_eq!(state_of(&host)["sort"], "time_asc");

        handle_panel_message(&host, "{\"op\":\"sort\",\"sort\":\"duration_desc\"}").unwrap();
        let state = state_of(&host);
        assert_eq!(state["sort"], "duration_desc");
        assert_eq!(row_tables(&state), ["b", "a", "c"], "longest first");

        handle_panel_message(&host, "{\"op\":\"sort\",\"sort\":\"duration_asc\"}").unwrap();
        let state = state_of(&host);
        assert_eq!(state["sort"], "duration_asc");
        assert_eq!(row_tables(&state), ["c", "a", "b"], "shortest first");

        handle_panel_message(&host, "{\"op\":\"sort\",\"sort\":\"time_desc\"}").unwrap();
        let state = state_of(&host);
        assert_eq!(state["sort"], "time_desc");
        assert_eq!(row_tables(&state), ["c", "b", "a"], "newest first");

        handle_panel_message(&host, "{\"op\":\"sort\",\"sort\":\"time_asc\"}").unwrap();
        let state = state_of(&host);
        assert_eq!(state["sort"], "time_asc");
        assert_eq!(row_tables(&state), ["a", "b", "c"], "capture order");
    }

    #[test]
    fn legacy_sort_tokens_still_parse() {
        // The old two-way toggle posted `time` / `duration`; `duration` sorted
        // longest first, so it maps onto `duration_desc`.
        let (host, _guard) = test_host();
        set_log_text(
            "1723456789012|300|statement|SELECT * FROM a\n1723456789013|1000|statement|SELECT * FROM b\n",
        );
        handle_panel_message(&host, "{\"op\":\"sort\",\"sort\":\"duration\"}").unwrap();
        let state = state_of(&host);
        assert_eq!(state["sort"], "duration_desc");
        assert_eq!(row_tables(&state), ["b", "a"]);

        handle_panel_message(&host, "{\"op\":\"sort\",\"sort\":\"time\"}").unwrap();
        assert_eq!(state_of(&host)["sort"], "time_asc");
    }

    #[test]
    fn sort_duration_puts_rows_without_a_duration_last() {
        let (host, _guard) = test_host();
        let text = "\
1723456789012|300|statement|SELECT * FROM timed
==>  Preparing: SELECT * FROM untimed WHERE id = ?
==> Parameters: 1(Long)
";
        set_log_text(text);
        for mode in ["duration_desc", "duration_asc"] {
            handle_panel_message(&host, &format!("{{\"op\":\"sort\",\"sort\":\"{mode}\"}}"))
                .unwrap();
            let rows = state_of(&host)["rows"].as_array().cloned().unwrap();
            assert_eq!(rows.len(), 2, "{rows:?}");
            assert!(
                rows[0]["sql"].as_str().unwrap().contains("timed"),
                "{mode} keeps the timed row first: {rows:?}"
            );
            assert_eq!(rows[1]["duration_ms"], Value::Null, "{mode}");
        }
    }

    #[test]
    fn sort_sequence_restores_the_row_number_order() {
        let (host, _guard) = test_host();
        set_log_text(
            "1723456789012|300|statement|SELECT * FROM a\n1723456789013|1000|statement|SELECT * FROM b\n1723456789014|50|statement|SELECT * FROM c\n",
        );
        handle_panel_message(&host, "{\"op\":\"sort\",\"sort\":\"duration_desc\"}").unwrap();
        assert_eq!(row_tables(&state_of(&host)), ["b", "a", "c"]);
        handle_panel_message(&host, "{\"op\":\"sort\",\"sort\":\"sequence\"}").unwrap();
        let state = state_of(&host);
        assert_eq!(state["sort"], "sequence");
        assert_eq!(row_tables(&state), ["a", "b", "c"]);
    }

    #[test]
    fn sort_structure_clusters_rows_of_the_same_shape() {
        // Two calls of the same statement shape (only the literal differs) and
        // one of a different shape: clustering must put the two same-shape rows
        // next to each other, whatever the fingerprint's own order.
        let (host, _guard) = test_host();
        set_log_text(
            "1723456789012|100|statement|SELECT * FROM users WHERE id = 1\n\
             1723456789013|100|statement|SELECT * FROM orders WHERE id = 2\n\
             1723456789014|100|statement|SELECT * FROM users WHERE id = 3\n",
        );
        handle_panel_message(&host, "{\"op\":\"sort\",\"sort\":\"structure\"}").unwrap();
        let state = state_of(&host);
        assert_eq!(state["sort"], "structure");
        let tables = row_tables(&state);
        let users: Vec<usize> = tables
            .iter()
            .enumerate()
            .filter(|(_, table)| table.as_str() == "users")
            .map(|(index, _)| index)
            .collect();
        assert_eq!(users.len(), 2, "{tables:?}");
        assert_eq!(
            users[1] - users[0],
            1,
            "the same shape is adjacent: {tables:?}"
        );
        assert!(
            !tables[users[0]..=users[1]]
                .iter()
                .any(|table| table == "orders")
        );
    }

    #[test]
    fn sort_database_group_keeps_capture_order_without_connection_names() {
        // This build captures no connection name, so every row is in the
        // reference's "unknown database" group, which sinks to the bottom and
        // keeps capture order — the mode is not a no-op error, it just has one
        // group to order.
        let (host, _guard) = test_host();
        set_log_text(
            "1723456789012|300|statement|SELECT * FROM a\n1723456789013|1000|statement|SELECT * FROM b\n",
        );
        handle_panel_message(&host, "{\"op\":\"sort\",\"sort\":\"database\"}").unwrap();
        let state = state_of(&host);
        assert_eq!(state["sort"], "database");
        assert_eq!(row_tables(&state), ["a", "b"]);
    }

    #[test]
    fn settings_message_points_at_the_settings_page() {
        let (host, _guard) = test_host();
        handle_panel_message(&host, "{\"op\":\"settings\"}").unwrap();
        let notes = host.notifications();
        assert!(!notes.is_empty(), "a notification is published");
        assert!(notes[0].body.contains("Settings"), "{:?}", notes[0]);
        let out = &host.output()[common::CHANNEL];
        assert!(out.contains("Plugins"), "{out}");
    }

    #[test]
    fn help_message_prints_the_cheat_sheet() {
        let (host, _guard) = test_host();
        handle_panel_message(&host, "{\"op\":\"help\"}").unwrap();
        let out = &host.output()[common::CHANNEL];
        assert!(out.contains(&t("view.logs.help.title")), "{out}");
        assert!(out.contains("Ctrl+F"), "{out}");
        assert!(out.contains("Ctrl+Tab"), "{out}");
    }

    #[test]
    fn toggle_group_expands_and_collapses_a_dedupe_group() {
        let (host, _guard) = test_host();
        let text = "\
==>  Preparing: SELECT * FROM user WHERE id = ?
==> Parameters: 1(Long)
==>  Preparing: SELECT * FROM user WHERE id = ?
==> Parameters: 1(Long)
";
        set_log_text(text);
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        let state = state_of(&host);
        let rows = state["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "folded while collapsed: {rows:?}");
        assert_eq!(rows[0]["dedupe_count"], 2);
        let key = rows[0]["dedupe_key"].as_str().unwrap().to_string();
        assert!(!key.is_empty());
        let line = rows[0]["line"].as_u64().unwrap();

        handle_panel_message(
            &host,
            &format!("{{\"op\":\"toggle-group\",\"line\":{line},\"kind\":\"dedupe\"}}"),
        )
        .unwrap();
        let state = state_of(&host);
        let rows = state["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "unfolded into its occurrences: {rows:?}");
        assert_eq!(rows[0]["expanded"], true);
        assert_eq!(rows[1]["expanded"], true);
        assert_eq!(rows[0]["dedupe_count"], 2);
        assert_eq!(rows[1]["dedupe_count"], 1);
        assert_eq!(rows[1]["dedupe_key"], json!(key));

        // The same badge collapses it again.
        handle_panel_message(
            &host,
            &format!("{{\"op\":\"toggle-group\",\"line\":{line},\"kind\":\"dedupe\"}}"),
        )
        .unwrap();
        assert_eq!(state_of(&host)["rows"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn toggle_group_expands_an_n_plus_one_group() {
        let (host, _guard) = test_host();
        let mut text = String::new();
        for id in 0..5 {
            text.push_str(&format!(
                "==>  Preparing: SELECT * FROM t WHERE id = ?\n==> Parameters: {id}(Integer)\n"
            ));
        }
        set_log_text(&text);
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        let state = state_of(&host);
        let rows = state["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 5);
        assert!(rows.iter().all(|row| row["group_count"] == 5));
        let line = rows[0]["line"].as_u64().unwrap();

        handle_panel_message(
            &host,
            &format!("{{\"op\":\"toggle-group\",\"line\":{line},\"kind\":\"group\"}}"),
        )
        .unwrap();
        let state = state_of(&host);
        let rows = state["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 5, "the group's occurrences: {rows:?}");
        let heads = rows.iter().filter(|row| row["group_count"] == 5).count();
        assert_eq!(heads, 1, "only the head keeps the ×N badge: {rows:?}");
        assert!(rows.iter().all(|row| row["expanded"] == true));
        assert!(rows[1..].iter().all(|row| row["group_count"] == 1));
    }

    #[test]
    fn clear_blocked_empties_the_key_list_in_the_state() {
        let (host, _guard) = test_host();
        set_log_text(SOURCED_LOG);
        let users = first_line_with(&snapshot().entries, "FROM users");
        handle_panel_message(
            &host,
            &format!("{{\"op\":\"block-method\",\"line\":{users}}}"),
        )
        .unwrap();
        let state = state_of(&host);
        let blocked = state["filter"]["blocked"].as_array().unwrap();
        assert_eq!(blocked.len(), 1, "{state}");
        assert_eq!(blocked[0], "c.e.m.mapper.UserMapper#selectById");
        assert_eq!(state["counts"]["blocked"], 1);

        handle_panel_message(&host, "{\"op\":\"clear-blocked\"}").unwrap();
        let state = state_of(&host);
        assert!(state["filter"]["blocked"].as_array().unwrap().is_empty());
        assert_eq!(state["counts"]["blocked"], 0);
    }
}
