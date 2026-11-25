//! SQL log panel (T2-1, lane L17).
//!
//! Data is pushed by the capture lane and by commands: [`set_log_text`] and
//! [`append_text`] fill a module-level buffer, and the panel parses it lazily.
//! The panel UI is the packaged page `web/log-panel.html`
//! (`web = "web/log-panel.html"` in `plugin.toml`, applied by the controller):
//! the page owns the DOM and posts user actions (`ready`, `filter`, `copy-line`,
//! `open-line`, `toggle-pause`, `clear`, `export`), and the plugin answers with
//! the full state through [`host::post_view_message`]. No HTML is built in Rust
//! any more, so the page always renders text with `textContent` (no injection).
//!
//! The store lives in a `OnceLock<Mutex<_>>` because `Main` belongs to the
//! controller (wasm is single-threaded; the mutex only serialises test threads).

use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex, MutexGuard, OnceLock};

use openidea_plugin::host;
use openidea_plugin::views::{Body, Button, View, ViewEvent, Views};
use openidea_plugin::{Error, Invocation};
use serde_json::{Value, json};

use crate::commands::common;
use crate::core::log::{
    DuplicateGroup, EntryClass, LogAnalysis, LogEntry, NPlusOneGroup, ParseOptions, parse,
    restore_rendered,
};
use crate::i18n::t;
use crate::settings;

/// The id declared in `[[contributes.views]]`.
pub const VIEW_ID: &str = "sqlv-logs";

/// Default refresh interval for pushed rows (also the parse throttle window).
pub const DEFAULT_REFRESH_MS: u64 = 250;
/// SQL displayed before truncation (the full statement is kept in the store and
/// copied/exported in full).
const SQL_DISPLAY_CHARS: usize = 180;
/// `logs.pageSize` default and clamp when used as the panel buffer cap.
const DEFAULT_BUFFER_CAP: i64 = 50;
const MAX_BUFFER_CAP: i64 = 100_000;

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

/// Panel category of a row: the display axis used for the text/left-bar colours
/// (normal / duplicate / N+1 / slow / error).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RowCategory {
    Normal,
    Duplicate,
    NPlusOne,
    Slow,
    Error,
}

impl RowCategory {
    /// Stable token used in the push state (`n_plus_one`).
    pub fn token(self) -> &'static str {
        match self {
            RowCategory::Normal => "normal",
            RowCategory::Duplicate => "duplicate",
            RowCategory::NPlusOne => "n_plus_one",
            RowCategory::Slow => "slow",
            RowCategory::Error => "error",
        }
    }
}

/// SQL statement kind, the axis of the "SQL 类型" multi-select filter and the
/// left colour bar. DDL is split into CREATE and DROP so the two filters the
/// reference UI offers stay distinct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SqlType {
    Select,
    Insert,
    Update,
    Delete,
    Create,
    Drop,
    Other,
    Error,
}

impl SqlType {
    /// The six kinds the filter offers, in display order.
    pub const FILTERABLE: [SqlType; 6] = [
        SqlType::Select,
        SqlType::Insert,
        SqlType::Update,
        SqlType::Delete,
        SqlType::Create,
        SqlType::Drop,
    ];

    pub fn token(self) -> &'static str {
        match self {
            SqlType::Select => "select",
            SqlType::Insert => "insert",
            SqlType::Update => "update",
            SqlType::Delete => "delete",
            SqlType::Create => "create",
            SqlType::Drop => "drop",
            SqlType::Other => "other",
            SqlType::Error => "error",
        }
    }

    /// Parses a filter token; unknown tokens are ignored (never "matches all").
    pub fn parse(token: &str) -> Option<Self> {
        [
            SqlType::Select,
            SqlType::Insert,
            SqlType::Update,
            SqlType::Delete,
            SqlType::Create,
            SqlType::Drop,
            SqlType::Other,
            SqlType::Error,
        ]
        .into_iter()
        .find(|kind| kind.token() == token)
    }
}

/// One rendered row: a single entry, or the representative of a folded
/// duplicate group.
#[derive(Debug, Clone, PartialEq)]
pub struct PanelRow {
    pub line_no: usize,
    pub timestamp_ms: Option<i64>,
    pub duration_ms: Option<f64>,
    /// Restored SQL; empty for error rows.
    pub sql: String,
    pub params: Vec<String>,
    pub category: RowCategory,
    pub sql_type: SqlType,
    /// Size of the folded duplicate group (1 when this row is one occurrence).
    pub dedupe_count: usize,
    /// Size of the N+1 group (1 when the row is not part of one).
    pub group_count: usize,
    pub n_plus_one: bool,
    /// `<== Total:` affected-row count, when the family prints one.
    pub affected_rows: Option<u64>,
    /// Source file/method: not carried by the log parser yet (empty → omitted).
    pub source: String,
    pub method: String,
    /// Original text for error rows (kept when no SQL was recovered).
    pub raw_line: Option<String>,
}

impl PanelRow {
    pub fn time_text(&self) -> String {
        self.timestamp_ms
            .map(format_epoch_ms)
            .unwrap_or_else(|| "-".to_string())
    }

    pub fn duration_text(&self) -> String {
        match self.duration_ms {
            Some(ms) => format_ms(ms),
            None => "-".to_string(),
        }
    }

    /// SQL truncated for the list; error rows have no SQL here (see
    /// [`PanelRow::error_text`]).
    pub fn display_sql(&self) -> String {
        truncate_chars(&self.sql, SQL_DISPLAY_CHARS)
    }

    /// The original line shown under an error row (empty for non-error rows).
    pub fn error_text(&self) -> String {
        if self.category == RowCategory::Error {
            self.raw_line.clone().unwrap_or_default()
        } else {
            String::new()
        }
    }

    /// Full text searched by the keyword/regex filter.
    fn searchable(&self) -> String {
        let mut text = self.sql.clone();
        if let Some(raw) = &self.raw_line {
            text.push('\n');
            text.push_str(raw);
        }
        if !self.params.is_empty() {
            text.push('\n');
            text.push_str(&self.params.join(", "));
        }
        text
    }
}

/// The "SQL 类型" selection: the button shows 全部 / 无 / N 项, so "all six" and
/// "none" must stay distinguishable (an empty `Only` set is never produced by
/// the page; empty defaults to `All`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeFilter {
    All,
    None,
    Only(BTreeSet<SqlType>),
}

impl Default for TypeFilter {
    fn default() -> Self {
        TypeFilter::All
    }
}

impl TypeFilter {
    fn matches(&self, kind: SqlType) -> bool {
        match self {
            TypeFilter::All => true,
            TypeFilter::None => false,
            TypeFilter::Only(set) => set.contains(&kind),
        }
    }

    /// Selected tokens, for the push state (`Only` only).
    fn selected_tokens(&self) -> Vec<&'static str> {
        match self {
            TypeFilter::Only(set) => set.iter().map(|kind| kind.token()).collect(),
            _ => Vec::new(),
        }
    }

    fn mode(&self) -> &'static str {
        match self {
            TypeFilter::All => "all",
            TypeFilter::None => "none",
            TypeFilter::Only(_) => "only",
        }
    }
}

/// The category/keyword/type filter applied before the buffer cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    /// Selected statement kinds.
    pub types: TypeFilter,
    pub slow_only: bool,
    pub keyword: String,
    pub match_case: bool,
    pub regex: bool,
    /// Fold duplicate statements into one row.
    pub dedupe: bool,
}

impl Default for Filter {
    fn default() -> Self {
        Self {
            types: TypeFilter::All,
            slow_only: false,
            keyword: String::new(),
            match_case: false,
            regex: false,
            dedupe: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Row assembly
// ---------------------------------------------------------------------------

/// Collapses duplicate groups (when `dedupe`), tags N+1 rows and classifies
/// each row. Rows keep `line_no` order.
pub fn build_rows(analysis: &LogAnalysis, dedupe: bool, slow_threshold_ms: f64) -> Vec<PanelRow> {
    let mut duplicate_by_line: HashMap<usize, &DuplicateGroup> = HashMap::new();
    for group in &analysis.duplicates {
        for &line in &group.line_nos {
            duplicate_by_line.insert(line, group);
        }
    }
    let mut n_plus_one_by_line: HashMap<usize, &NPlusOneGroup> = HashMap::new();
    for group in &analysis.n_plus_one {
        for &line in &group.line_nos {
            n_plus_one_by_line.insert(line, group);
        }
    }

    let mut rows = Vec::new();
    let mut folded_groups: BTreeSet<String> = BTreeSet::new();
    for entry in &analysis.entries {
        let duplicate = duplicate_by_line.get(&entry.line_no).copied();
        let n_plus_one = n_plus_one_by_line.get(&entry.line_no).copied();
        let category = categorize(entry, n_plus_one, slow_threshold_ms);
        let sql_type = sql_type_of(entry);

        if dedupe && let Some(group) = duplicate {
            if !folded_groups.insert(group.key.clone()) {
                continue;
            }
            rows.push(row_of(
                entry,
                category,
                sql_type,
                group.count.max(1),
                n_plus_one.map(|group| group.count).unwrap_or(1),
                n_plus_one.is_some(),
            ));
            continue;
        }

        rows.push(row_of(
            entry,
            category,
            sql_type,
            1,
            n_plus_one.map(|group| group.count).unwrap_or(1),
            n_plus_one.is_some(),
        ));
    }
    rows
}

fn categorize(
    entry: &LogEntry,
    n_plus_one: Option<&NPlusOneGroup>,
    slow_threshold_ms: f64,
) -> RowCategory {
    if entry.class == EntryClass::Error {
        RowCategory::Error
    } else if n_plus_one.is_some() {
        RowCategory::NPlusOne
    } else if entry.duration_ms.is_some_and(|ms| ms >= slow_threshold_ms) {
        RowCategory::Slow
    } else {
        RowCategory::Normal
    }
}

/// Statement kind for the filter/colour bar. `EntryClass` groups all DDL into
/// one variant, so the first keyword decides between CREATE and DROP.
fn sql_type_of(entry: &LogEntry) -> SqlType {
    if entry.class == EntryClass::Error {
        return SqlType::Error;
    }
    match first_keyword(&entry.sql).as_str() {
        "SELECT" => SqlType::Select,
        "INSERT" | "REPLACE" => SqlType::Insert,
        "UPDATE" => SqlType::Update,
        "DELETE" => SqlType::Delete,
        "CREATE" => SqlType::Create,
        "DROP" | "TRUNCATE" => SqlType::Drop,
        _ => match entry.class {
            EntryClass::Select => SqlType::Select,
            EntryClass::Insert => SqlType::Insert,
            EntryClass::Update => SqlType::Update,
            EntryClass::Delete => SqlType::Delete,
            EntryClass::Ddl => SqlType::Create,
            _ => SqlType::Other,
        },
    }
}

fn first_keyword(sql: &str) -> String {
    sql.trim_start()
        .chars()
        .take_while(|ch| ch.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_uppercase()
}

fn row_of(
    entry: &LogEntry,
    category: RowCategory,
    sql_type: SqlType,
    dedupe_count: usize,
    group_count: usize,
    n_plus_one: bool,
) -> PanelRow {
    PanelRow {
        line_no: entry.line_no,
        timestamp_ms: entry.timestamp_ms,
        duration_ms: entry.duration_ms,
        sql: entry.sql.clone(),
        params: entry.params.clone(),
        category,
        sql_type,
        dedupe_count,
        group_count,
        n_plus_one,
        affected_rows: entry.affected_rows,
        source: String::new(),
        method: String::new(),
        raw_line: entry.raw_line.clone(),
    }
}

// ---------------------------------------------------------------------------
// Filtering (pure)
// ---------------------------------------------------------------------------

/// A compiled keyword matcher. `Invalid` is what an unparsable regular
/// expression becomes: it matches nothing instead of panicking.
#[derive(Debug)]
enum Matcher {
    Empty,
    Text {
        needle: String,
        case_sensitive: bool,
    },
    Regex(regex::Regex),
    Invalid,
}

impl Matcher {
    fn new(keyword: &str, case_sensitive: bool, regex: bool) -> Self {
        let needle = keyword.trim();
        if needle.is_empty() {
            return Self::Empty;
        }
        if regex {
            match regex::RegexBuilder::new(needle)
                .case_insensitive(!case_sensitive)
                .build()
            {
                Ok(compiled) => Self::Regex(compiled),
                Err(_) => Self::Invalid,
            }
        } else {
            Self::Text {
                needle: needle.to_string(),
                case_sensitive,
            }
        }
    }

    fn is_match(&self, haystack: &str) -> bool {
        match self {
            Self::Empty => true,
            Self::Invalid => false,
            Self::Regex(compiled) => compiled.is_match(haystack),
            Self::Text {
                needle,
                case_sensitive,
            } => {
                if *case_sensitive {
                    haystack.contains(needle)
                } else {
                    haystack.to_lowercase().contains(&needle.to_lowercase())
                }
            }
        }
    }
}

/// True when `row` passes `filter` at `slow_threshold_ms`.
pub fn row_matches(row: &PanelRow, filter: &Filter, slow_threshold_ms: f64) -> bool {
    if !filter.types.matches(row.sql_type) {
        return false;
    }
    if filter.slow_only {
        let slow = row.duration_ms.is_some_and(|ms| ms >= slow_threshold_ms);
        if !slow {
            return false;
        }
    }
    let matcher = Matcher::new(&filter.keyword, filter.match_case, filter.regex);
    matcher.is_match(&row.searchable())
}

/// Applies `filter` and returns the surviving rows (order preserved).
pub fn filter_rows(rows: &[PanelRow], filter: &Filter, slow_threshold_ms: f64) -> Vec<PanelRow> {
    rows.iter()
        .filter(|row| row_matches(row, filter, slow_threshold_ms))
        .cloned()
        .collect()
}

/// Whether the keyword is a regular expression that fails to compile (so the
/// page can flag it). Only meaningful when `filter.regex` is set.
pub fn regex_is_invalid(filter: &Filter) -> bool {
    filter.regex
        && !filter.keyword.trim().is_empty()
        && regex::RegexBuilder::new(filter.keyword.trim())
            .case_insensitive(!filter.match_case)
            .build()
            .is_err()
}

// ---------------------------------------------------------------------------
// Store and push API
// ---------------------------------------------------------------------------

struct Store {
    text: String,
    analysis: LogAnalysis,
    dirty: bool,
    filter: Filter,
    /// Once the page sends a filter, its `dedupe` choice wins over the setting.
    filter_touched: bool,
    paused: bool,
    /// Line the "Log → SQL" tab restores (the page's selection; `None` means
    /// "last statement"), kept so a later state push does not drop the tab.
    log_to_sql_line: Option<usize>,
    /// Last "SQL → Java" conversion, carried in `tabs.sqlToJava`.
    sql_to_java: SqlToJavaTab,
    /// Last history search over the buffered log text, carried in
    /// `tabs.history`.
    history: HistoryTab,
}

/// Result of one "SQL → Java" conversion (the tab keeps it across pushes).
#[derive(Debug, Clone, Default, PartialEq)]
struct SqlToJavaTab {
    /// Generated code; `None` for `unresolved` and before the first run.
    code: Option<String>,
    /// `exact` / `partial` / `unresolved`; empty before the first run.
    confidence: String,
    /// Why no code was produced (message keys, one per line).
    reason: String,
    /// Java identifiers the generated chain expects, in order.
    params: Vec<String>,
    /// False until the page asks for a conversion.
    ran: bool,
}

/// One line the history search matched, enriched from the parsed entries.
#[derive(Debug, Clone, PartialEq)]
struct HistoryMatch {
    /// 1-based line number in the buffered log text.
    line: usize,
    time: String,
    /// Restored SQL of that line when it is a parsed statement (else empty).
    sql: String,
    /// The matched log line, verbatim.
    text: String,
    has_sql: bool,
}

/// Result of one history search.
#[derive(Debug, Clone, Default, PartialEq)]
struct HistoryTab {
    keyword: String,
    matches: Vec<HistoryMatch>,
    total: usize,
    truncated: bool,
    ran: bool,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            text: String::new(),
            analysis: empty_analysis(),
            dirty: false,
            filter: Filter::default(),
            filter_touched: false,
            paused: false,
            log_to_sql_line: None,
            sql_to_java: SqlToJavaTab::default(),
            history: HistoryTab::default(),
        }
    }
}

fn empty_analysis() -> LogAnalysis {
    LogAnalysis {
        entries: Vec::new(),
        duplicates: Vec::new(),
        n_plus_one: Vec::new(),
        slow: Vec::new(),
        errors: Vec::new(),
        unparsed_lines: Vec::new(),
    }
}

static STORE: OnceLock<Mutex<Store>> = OnceLock::new();

fn store() -> MutexGuard<'static, Store> {
    let mutex = STORE.get_or_init(|| Mutex::new(Store::default()));
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Replaces the whole log text (capture lane / commands).
pub fn set_log_text(text: &str) {
    let mut state = store();
    state.text.clear();
    state.text.push_str(text);
    state.dirty = true;
}

/// Appends a chunk of log text (capture lane).
pub fn append_text(text: &str) {
    if text.is_empty() {
        return;
    }
    let mut state = store();
    if !state.text.is_empty() && !state.text.ends_with('\n') {
        state.text.push('\n');
    }
    state.text.push_str(text);
    state.dirty = true;
}

/// Drops the buffered text and the derived groups (panel Clear button). The
/// history tab searched that buffer, so its result goes too; the "SQL → Java"
/// result does not depend on the log and survives.
pub fn clear() {
    let mut state = store();
    state.text.clear();
    state.analysis = empty_analysis();
    state.dirty = false;
    state.log_to_sql_line = None;
    state.history = HistoryTab::default();
}

/// Whether live refresh is paused (rows keep accumulating meanwhile).
pub fn is_paused() -> bool {
    store().paused
}

/// Sets the paused flag.
pub fn set_paused(paused: bool) {
    store().paused = paused;
}

/// Toggles pause and returns the new paused state.
pub fn toggle_pause() -> bool {
    let paused = !store().paused;
    set_paused(paused);
    paused
}

/// Replaces the whole filter (from a `filter` message).
pub fn set_filter(filter: Filter) {
    let mut state = store();
    state.filter = filter;
    state.filter_touched = true;
}

/// The parsed analysis of the buffered text (parses on demand).
pub fn snapshot() -> LogAnalysis {
    let mut state = store();
    ensure_parsed(&mut state);
    state.analysis.clone()
}

/// Number of buffered characters (for status lines and diagnostics).
pub fn buffered_len() -> usize {
    store().text.len()
}

fn ensure_parsed(state: &mut Store) {
    if !state.dirty {
        return;
    }
    let options = parse_options();
    state.analysis = parse(&state.text, &options);
    state.dirty = false;
}

fn parse_options() -> ParseOptions {
    let window = settings::get_i64("log.dedupeWindowMs", 0);
    ParseOptions {
        slow_threshold_ms: settings::get_i64("log.slowThresholdMs", 1000).max(0) as f64,
        n_plus_one_threshold: settings::get_i64("log.nPlusOneThreshold", 5).max(2) as usize,
        ignore_params_in_dedupe: settings::get_bool("log.ignoreParamsInDedupe", false),
        dedupe_window_ms: (window > 0).then_some(window),
        max_sql_len: settings::get_i64("log.maxSqlLength", 20_000).max(256) as usize,
        timestamp_offset_minutes: settings::get_i64("log.timestampOffsetMinutes", 0) as i32,
        ..ParseOptions::default()
    }
}

fn slow_threshold() -> f64 {
    settings::get_i64("log.slowThresholdMs", 1000).max(0) as f64
}

fn buffer_cap() -> usize {
    settings::get_i64("logs.pageSize", DEFAULT_BUFFER_CAP).clamp(1, MAX_BUFFER_CAP) as usize
}

// ---------------------------------------------------------------------------
// State push
// ---------------------------------------------------------------------------

/// Builds the full push state (see the lane contract in `docs/registration/L17.md`).
fn build_state() -> Value {
    let mut state = store();
    ensure_parsed(&mut state);

    let slow_threshold_ms = slow_threshold();
    let dedupe = if state.filter_touched {
        state.filter.dedupe
    } else {
        settings::get_bool("log.dedupe", true)
    };
    let rows = build_rows(&state.analysis, dedupe, slow_threshold_ms);

    let filter = state.filter.clone();
    let filtered = filter_rows(&rows, &filter, slow_threshold_ms);
    let total = filtered.len();
    let slow = filtered
        .iter()
        .filter(|row| row.duration_ms.is_some_and(|ms| ms >= slow_threshold_ms))
        .count();
    let n_plus_one = filtered.iter().filter(|row| row.n_plus_one).count();

    let cap = buffer_cap();
    let start = total.saturating_sub(cap);
    let shown = &filtered[start..];
    let truncated = total > shown.len();

    let rows_json: Vec<Value> = shown
        .iter()
        .enumerate()
        .map(|(index, row)| {
            json!({
                "line": row.line_no,
                "seq": start + index + 1,
                "time": row.time_text(),
                "duration_ms": row.duration_ms,
                "db": "",
                "rows_affected": row.affected_rows,
                "dedupe_count": row.dedupe_count,
                "group_count": row.group_count,
                "n_plus_one": row.n_plus_one,
                "source": row.source,
                "method": row.method,
                "sql": row.display_sql(),
                "error_text": row.error_text(),
                "category": row.category.token(),
                "sql_type": row.sql_type.token(),
            })
        })
        .collect();

    let console_lines = state.text.lines().count() as u64;
    let paused = state.paused;
    let log_to_sql = log_to_sql_payload(&state.analysis, state.log_to_sql_line);
    let sql_to_java = sql_to_java_state(&state.sql_to_java);
    let history = history_state(&state.history);
    drop(state);

    // The fifth tab (lane L23) is the preview workbench: the model and the
    // ops live in `views::workbench`, the state, the page and the push channel
    // are this panel's. `selectTab` is `null` unless something asked for it
    // (`open-workbench`, a double-click on a row) and stays set until the page
    // reports the tab it shows.
    let preview = crate::views::workbench::preview_tab();
    let select_tab = crate::views::workbench::select_tab();

    let stats = crate::capture::bridge::Stats::current();

    json!({
        "op": "state",
        "rows": rows_json,
        "selectTab": select_tab,
        "tabs": {
            "logToSql": log_to_sql,
            "sqlToJava": sql_to_java,
            "history": history,
            "preview": preview,
        },
        "filter": {
            "types": filter.types.selected_tokens(),
            "types_mode": filter.types.mode(),
            "slow_only": filter.slow_only,
            "keyword": filter.keyword,
            "match_case": filter.match_case,
            "regex": filter.regex,
            "dedupe": dedupe,
            "regex_error": regex_is_invalid(&filter),
        },
        "counts": {
            "total": total,
            "shown": shown.len(),
            "slow": slow,
            "n_plus_one": n_plus_one,
            "truncated": truncated,
            "cap": cap,
        },
        "status": {
            "console_lines": console_lines,
            "agent_connected": stats.connected,
            "agent_events": stats.events,
            "paused": paused,
        },
        "slow_threshold_ms": slow_threshold_ms,
        "labels": labels_json(),
    })
}

/// Posts the full state to the packaged page.
pub fn push_state() -> Result<(), Error> {
    let state = build_state();
    host::post_view_message(VIEW_ID, &state.to_string())
}

// ---------------------------------------------------------------------------
// Tab payloads (lane L22, docs/registration/L22.md)
// ---------------------------------------------------------------------------

/// Matches one history search returns (the handler's own default).
const HISTORY_LIMIT: usize = 20;

/// `tabs.logToSql`: the selected statement (or the last one when nothing is
/// selected) in `?` form, its rendered parameters and the executable result.
fn log_to_sql_payload(analysis: &LogAnalysis, line: Option<usize>) -> Value {
    let entry = line
        .and_then(|line| analysis.entries.iter().find(|entry| entry.line_no == line))
        .filter(|entry| entry.has_sql())
        .or_else(|| analysis.entries.iter().rev().find(|entry| entry.has_sql()));
    let Some(entry) = entry else {
        return json!({
            "empty": true,
            "line": null,
            "rawSql": "",
            "params": [],
            "restored": "",
            "confidence": "",
        });
    };
    json!({
        "empty": false,
        "line": entry.line_no,
        "rawSql": entry.raw_sql,
        "params": entry.params,
        "restored": restore_rendered(&entry.raw_sql, &entry.params),
        "confidence": entry.confidence,
    })
}

/// `tabs.sqlToJava`: the last conversion (absent until the page asks).
fn sql_to_java_state(tab: &SqlToJavaTab) -> Value {
    json!({
        "ran": tab.ran,
        "code": tab.code,
        "confidence": tab.confidence,
        "reason": tab.reason,
        "params": tab.params,
    })
}

/// `tabs.history`: the last search over the buffered log text.
fn history_state(tab: &HistoryTab) -> Value {
    let matches: Vec<Value> = tab
        .matches
        .iter()
        .map(|item| {
            json!({
                "line": item.line,
                "time": item.time,
                "sql": item.sql,
                "text": item.text,
                "has_sql": item.has_sql,
            })
        })
        .collect();
    json!({
        "ran": tab.ran,
        "keyword": tab.keyword,
        "total": tab.total,
        "truncated": tab.truncated,
        "matches": matches,
    })
}

/// Runs the `sqlv_reverse_sql` handler for the page and keeps the result.
fn run_sql_to_java(sql: &str, entity: &str) {
    let sql = sql.trim();
    if sql.is_empty() {
        store().sql_to_java = SqlToJavaTab::default();
        return;
    }
    let arguments = json!({ "sql": sql, "entity": entity }).to_string();
    let raw = crate::mcp::handlers::reverse_sql::run(&arguments);
    store().sql_to_java = sql_to_java_tab(&raw);
}

/// The tab result of one handler envelope. `unresolved` keeps no code and
/// reports why; the confidence is passed through, never upgraded.
fn sql_to_java_tab(raw: &str) -> SqlToJavaTab {
    let value: Value = serde_json::from_str(raw).unwrap_or(Value::Null);
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        return SqlToJavaTab {
            ran: true,
            code: None,
            confidence: "unresolved".to_string(),
            reason: value
                .pointer("/error/message_key")
                .and_then(Value::as_str)
                .unwrap_or("mcp.error.unknown_tool")
                .to_string(),
            params: Vec::new(),
        };
    }
    let data = &value["data"];
    let confidence = value
        .get("confidence")
        .and_then(Value::as_str)
        .unwrap_or("unresolved")
        .to_string();
    let code = data.get("code").and_then(Value::as_str).map(str::to_string);
    let reason = if code.is_some() {
        String::new()
    } else {
        reason_lines(data)
    };
    let params = data
        .get("params")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    SqlToJavaTab {
        code,
        confidence,
        reason,
        params,
        ran: true,
    }
}

/// `unresolved[]` of a generator result, one line per entry. A key without a
/// translation stays visible as the key, the same contract the workbench uses.
fn reason_lines(data: &Value) -> String {
    data.get("unresolved")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    let key = item
                        .get("message_key")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let detail = item.get("detail").cloned().unwrap_or(Value::Null);
                    let has_detail = detail.as_object().is_some_and(|map| !map.is_empty());
                    if has_detail {
                        format!("{} {}", t(key), detail)
                    } else {
                        t(key)
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Runs the `sqlv_history_search` handler over the buffered log text. This
/// build keeps no persisted history, so the search covers exactly what the
/// panel has captured (stated in `docs/LIMITS.md`).
fn run_history(keyword: &str) {
    let keyword = keyword.trim();
    let text = store().text.clone();
    if keyword.is_empty() {
        store().history = HistoryTab::default();
        return;
    }
    let mut tab = HistoryTab {
        keyword: keyword.to_string(),
        ran: true,
        ..HistoryTab::default()
    };
    if text.trim().is_empty() {
        store().history = tab;
        return;
    }
    let arguments = json!({ "text": text, "query": keyword, "limit": HISTORY_LIMIT }).to_string();
    let raw = crate::mcp::handlers::history_search::run(&arguments);
    let value: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    if value.get("ok").and_then(Value::as_bool) == Some(true) {
        let data = &value["data"];
        tab.total = data
            .get("total_matches")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        tab.truncated = data
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let analysis = snapshot();
        tab.matches = data
            .get("matches")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|item| history_match(item, &analysis))
                    .collect()
            })
            .unwrap_or_default();
    }
    store().history = tab;
}

/// One handler match enriched with the parsed entry that owns the line.
fn history_match(item: &Value, analysis: &LogAnalysis) -> HistoryMatch {
    let line = item.get("line_no").and_then(Value::as_u64).unwrap_or(0) as usize;
    let entry = analysis.entries.iter().find(|entry| entry.line_no == line);
    HistoryMatch {
        line,
        time: entry
            .and_then(|entry| entry.timestamp_ms)
            .map(format_epoch_ms)
            .unwrap_or_else(|| "-".to_string()),
        sql: entry
            .map(|entry| truncate_chars(&entry.sql, SQL_DISPLAY_CHARS))
            .unwrap_or_default(),
        text: item
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        has_sql: entry.is_some_and(|entry| entry.has_sql()),
    }
}

fn labels_json() -> Value {
    json!({
        "typesTitle": t("view.logs.filter.types_title"),
        "typesAll": t("view.logs.filter.all"),
        "typesNone": t("view.logs.filter.none"),
        "typesCountSuffix": t("view.logs.filter.count_suffix"),
        "selectAll": t("view.logs.filter.select_all"),
        "selectNone": t("view.logs.filter.select_none"),
        "slowOnly": t("view.logs.filter.slow_only"),
        "search": t("view.logs.filter.search"),
        // The page draws its own action row: a packaged view gets no host toolbar.
        "pause": t("view.logs.button.pause"),
        "resume": t("view.logs.button.resume"),
        "clear": t("view.logs.button.clear"),
        "copyAll": t("view.logs.button.copy"),
        "dedupe": t("view.logs.filter.dedupe"),
        "keywordPlaceholder": t("view.logs.filter.keyword"),
        "matchCase": t("view.logs.filter.match_case"),
        "matchCaseTitle": t("view.logs.filter.match_case_title"),
        "regex": t("view.logs.filter.regex"),
        "regexTitle": t("view.logs.filter.regex_title"),
        "hitsSuffix": t("view.logs.filter.hits_suffix"),
        "regexError": t("view.logs.filter.regex_error"),
        "slowPrefix": t("view.logs.slow_prefix"),
        "empty": t("view.logs.empty"),
        "paused": t("view.logs.paused"),
        "statusConsolePrefix": t("view.logs.status.console_prefix"),
        "statusConsoleSuffix": t("view.logs.status.console_suffix"),
        "statusAgentOff": t("view.logs.status.agent_off"),
        "statusAgentEventsPrefix": t("view.logs.status.agent_events_prefix"),
        "totalPrefix": t("view.logs.status.total_prefix"),
        "totalSuffix": t("view.logs.status.total_suffix"),
        "truncatedPrefix": t("view.logs.status.truncated_prefix"),
        "nPlusOne": t("view.logs.badge.n_plus_one"),
        "dedupeBadgePrefix": t("view.logs.badge.dedupe_prefix"),
        "groupBadgePrefix": t("view.logs.badge.group_prefix"),
        "menuCopySql": t("view.logs.menu.copy_sql"),
        "menuCopyTime": t("view.logs.menu.copy_time"),
        "menuExport": t("view.logs.menu.export"),
        "typeSelect": t("view.logs.type.select"),
        "typeInsert": t("view.logs.type.insert"),
        "typeUpdate": t("view.logs.type.update"),
        "typeDelete": t("view.logs.type.delete"),
        "typeCreate": t("view.logs.type.create"),
        "typeDrop": t("view.logs.type.drop"),
        // Tabs (lane L22).
        "tabLogs": t("view.logs.tab.logs"),
        "tabLogToSql": t("view.logs.tab.log_to_sql"),
        "tabSqlToJava": t("view.logs.tab.sql_to_java"),
        "tabHistory": t("view.logs.tab.history"),
        // Fifth tab, the preview workbench (lane L23).
        "tabPreview": t("view.logs.tab.preview"),
        "tabLogToSqlRaw": t("view.logs.tab.log_to_sql.raw"),
        "tabLogToSqlParams": t("view.logs.tab.log_to_sql.params"),
        "tabLogToSqlRestored": t("view.logs.tab.log_to_sql.restored"),
        "tabLogToSqlEmpty": t("view.logs.tab.log_to_sql.empty"),
        "tabLogToSqlCopy": t("view.logs.tab.log_to_sql.copy"),
        "tabLogToSqlSend": t("view.logs.tab.log_to_sql.send"),
        "tabSqlToJavaInput": t("view.logs.tab.sql_to_java.input"),
        "tabSqlToJavaEntity": t("view.logs.tab.sql_to_java.entity"),
        "tabSqlToJavaRun": t("view.logs.tab.sql_to_java.run"),
        "tabSqlToJavaCopy": t("view.logs.tab.sql_to_java.copy"),
        "tabSqlToJavaEmpty": t("view.logs.tab.sql_to_java.empty"),
        "tabSqlToJavaParams": t("view.logs.tab.sql_to_java.params"),
        "tabSqlToJavaReason": t("view.logs.tab.sql_to_java.reason"),
        "tabSqlToJavaOutput": t("view.logs.tab.sql_to_java.output"),
        "tabHistoryKeyword": t("view.logs.tab.history.keyword"),
        "tabHistorySearch": t("view.logs.tab.history.search"),
        "tabHistoryEmpty": t("view.logs.tab.history.empty"),
        "tabHistoryNoMatch": t("view.logs.tab.history.no_match"),
        "tabHistoryCountSuffix": t("view.logs.tab.history.count_suffix"),
        "tabHistorySource": t("view.logs.tab.history.source"),
        "tabHistoryTruncated": t("view.logs.tab.history.truncated"),
        "tabConfidencePrefix": t("view.logs.tab.confidence_prefix"),
        "tabConfidenceExact": t("view.logs.tab.confidence.exact"),
        "tabConfidencePartial": t("view.logs.tab.confidence.partial"),
        "tabConfidenceUnresolved": t("view.logs.tab.confidence.unresolved"),
    })
}

// ---------------------------------------------------------------------------
// Rendering (toolbar + status chrome; the body is the packaged page)
// ---------------------------------------------------------------------------

/// Builds the panel view chrome for a `web =` view.
///
/// The page is the body; this returns the toolbar buttons and status that the
/// host draws around it. It also re-pushes the state on every render so the
/// live capture path (`refresh_view`) reaches the page.
pub fn render_panel() -> View {
    let paused = store().paused;
    if !paused {
        let _ = push_state();
    }

    let pause_label = if paused {
        t("view.logs.button.resume")
    } else {
        t("view.logs.button.pause")
    };
    let mut view = View::new(Body::Html(String::new()));
    view.toolbar
        .push(Button::new("toggle-pause", &pause_label).icon(if paused { "run" } else { "stop" }));
    view.toolbar
        .push(Button::new("clear", &t("view.logs.button.clear")).icon("remove"));
    view.toolbar
        .push(Button::new("copy-all", &t("view.logs.button.copy")));
    if paused {
        view.status = Some(t("view.logs.paused"));
    }
    view
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// Handles toolbar buttons and messages posted by the packaged page.
pub fn handle_panel_event(event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
    match event {
        ViewEvent::Button(id) if id == "toggle-pause" => {
            toggle_pause();
            let _ = push_state();
            host::refresh_view(VIEW_ID);
        }
        ViewEvent::Button(id) if id == "clear" => {
            clear();
            let _ = push_state();
            host::refresh_view(VIEW_ID);
        }
        ViewEvent::Button(id) if id == "copy-all" => {
            host::copy_to_clipboard(&all_sql())?;
        }
        ViewEvent::Message(text) => handle_message(&text, ctx)?,
        _ => {}
    }
    Ok(())
}

fn handle_message(text: &str, ctx: &Invocation) -> Result<(), Error> {
    let message: Value =
        serde_json::from_str(text).map_err(|error| Error::Invalid(error.to_string()))?;
    let op = message
        .get("op")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    match op {
        // The packaged page asks for the state once its script is ready.
        "ready" | "page" => {
            let _ = push_state();
        }
        "filter" => {
            set_filter(filter_from_message(&message));
            let _ = push_state();
        }
        "toggle-pause" => {
            toggle_pause();
            let _ = push_state();
            host::refresh_view(VIEW_ID);
        }
        "clear" => {
            clear();
            let _ = push_state();
            host::refresh_view(VIEW_ID);
        }
        "copy-all" => {
            let sql = visible_sql();
            if !sql.is_empty() {
                host::copy_to_clipboard(&sql)?;
            }
        }
        "copy-line" => {
            if let Some(line) = message_line(&message) {
                let sql = sql_of_line(line).unwrap_or_default();
                if !sql.is_empty() {
                    host::copy_to_clipboard(&sql)?;
                }
            }
        }
        "copy-time" => {
            if let Some(line) = message_line(&message)
                && let Some(time) = time_of_line(line)
            {
                host::copy_to_clipboard(&time)?;
            }
        }
        "export" => {
            common::emit(&visible_sql())?;
        }
        "open-line" => {
            if let Some(line) = message_line(&message) {
                let sql = sql_of_line(line).unwrap_or_default();
                if !sql.is_empty() {
                    crate::commands::open_workbench::run(ctx, Some(&sql))?;
                }
            }
        }
        // Tab strip: the page reports the tab it now shows, which closes a
        // pending "switch to the preview tab" request.
        "tab" => crate::views::workbench::note_page_tab(),
        // "Log → SQL" tab: remember which line the page selected and re-push
        // the tab payload (the page keeps its list, so nothing is lost).
        "log-to-sql" => {
            store().log_to_sql_line = message_line(&message);
            let _ = push_state();
        }
        // "SQL → Java" tab: the input comes from the page, the conversion is
        // the `sqlv_reverse_sql` handler.
        "sql-to-java" => {
            // `text` is the page's field; `sql` stays accepted for direct calls.
            let sql = message
                .get("text")
                .or_else(|| message.get("sql"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let entity = message.get("entity").and_then(Value::as_str).unwrap_or("");
            run_sql_to_java(sql, entity);
            let _ = push_state();
        }
        // "SQL history" tab: the handler searches the buffered log text.
        "history" => {
            let keyword = message.get("keyword").and_then(Value::as_str).unwrap_or("");
            run_history(keyword);
            let _ = push_state();
        }
        // A history row: parsed statements open the workbench; any other
        // matched line is copied verbatim (no silently dead click).
        "history-open" => {
            if let Some(line) = message_line(&message) {
                match sql_of_line(line) {
                    Some(sql) => {
                        crate::commands::open_workbench::run(ctx, Some(&sql))?;
                    }
                    None => {
                        // Not a parsed statement: hand the line over verbatim,
                        // preferring the text the search matched.
                        let mut text = store()
                            .history
                            .matches
                            .iter()
                            .find(|item| item.line == line)
                            .map(|item| item.text.clone());
                        if text.is_none() {
                            text = store()
                                .text
                                .lines()
                                .nth(line.saturating_sub(1))
                                .map(str::to_string);
                        }
                        if let Some(text) = text {
                            host::copy_to_clipboard(&text)?;
                        }
                    }
                }
            }
        }
        // "SQL → Java" copy: the page cannot reach the clipboard itself.
        "copy-code" => {
            let code = store().sql_to_java.code.clone();
            if let Some(code) = code {
                host::copy_to_clipboard(&code)?;
            }
        }
        // The preview tab (lane L23): the page prefixes its ops so they cannot
        // collide with this panel's own `clear` / `export` / `copy`; the model
        // and the op handling stay in `views::workbench`, the push is this
        // panel's state.
        _ if op.starts_with("preview") => {
            crate::views::workbench::handle_preview_message(&message)?;
            let _ = push_state();
        }
        _ => {}
    }
    Ok(())
}

fn message_line(message: &Value) -> Option<usize> {
    message
        .get("line")
        .and_then(|value| value.as_u64())
        .map(|line| line as usize)
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
    }
}

fn all_sql() -> String {
    snapshot()
        .entries
        .iter()
        .filter(|entry| entry.has_sql())
        .map(|entry| entry.sql.as_str())
        .collect::<Vec<_>>()
        .join(";\n")
}

/// SQL of the visible (filtered + capped) rows, joined for the export action.
pub fn visible_sql() -> String {
    let analysis = snapshot();
    let slow_threshold_ms = slow_threshold();
    let dedupe = settings::get_bool("log.dedupe", true);
    let rows = build_rows(&analysis, dedupe, slow_threshold_ms);
    let filter = store().filter.clone();
    let filtered = filter_rows(&rows, &filter, slow_threshold_ms);
    let cap = buffer_cap();
    let start = filtered.len().saturating_sub(cap);
    filtered[start..]
        .iter()
        .filter(|row| !row.sql.is_empty())
        .map(|row| format!("{};", row.sql))
        .collect::<Vec<_>>()
        .join("\n")
}

fn sql_of_line(line_no: usize) -> Option<String> {
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
// Small utilities
// ---------------------------------------------------------------------------

/// `now` in milliseconds since the Unix epoch (WASI clocks on wasm).
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// `YYYY-MM-DD HH:MM:SS.mmm` UTC for an epoch timestamp. Logs rarely carry a
/// zone; the panel shows the raw wall clock rather than guessing one.
fn format_epoch_ms(ms: i64) -> String {
    let seconds = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let seconds_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3600;
    let minute = (seconds_of_day % 3600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}.{millis:03}")
}

/// Howard Hinnant's civil-from-days (UTC).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = (z - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn format_ms(ms: f64) -> String {
    if (ms - ms.round()).abs() < 0.001 {
        format!("{ms:.0}")
    } else {
        format!("{ms:.1}")
    }
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut truncated: String = text.chars().take(max.saturating_sub(1)).collect();
    truncated.push('…');
    truncated
}

// ---------------------------------------------------------------------------
// View trait
// ---------------------------------------------------------------------------

impl Views for crate::Main {
    fn render(
        &mut self,
        view: &str,
        _viewport: Option<openidea_plugin::views::Viewport>,
        _ctx: &Invocation,
    ) -> Result<View, Error> {
        match view {
            VIEW_ID => Ok(render_panel()),
            other => Err(Error::NotFound(format!("view {other}"))),
        }
    }

    fn handle(&mut self, view: &str, event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
        match view {
            VIEW_ID => handle_panel_event(event, ctx),
            other => Err(Error::NotFound(format!("view {other}"))),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    /// The row store and the workbench model are shared by the whole process;
    /// the lock is the one both panels' tests and the `commands` tests take.
    fn test_host() -> (MockHost, MutexGuard<'static, ()>) {
        let guard = super::super::test_state_lock();
        let host = MockHost::install();
        clear();
        set_filter(Filter::default());
        set_paused(false);
        // The preview tab's model is process-wide too (lane L23).
        crate::views::workbench::clear();
        crate::views::workbench::note_page_tab();
        (host, guard)
    }

    /// Two statements with different kinds, one slow, one error.
    const MIXED_LOG: &str = "\
Preparing: SELECT id FROM users WHERE id = ?
Parameters: 1(Long)
Preparing: DELETE FROM users WHERE id = ?
Parameters: 2(Long)
Preparing: UPDATE account SET balance = ?
Parameters: 3(Long)
java.sql.SQLException: Table 'demo.user' doesn't exist
";

    fn analysis_of(text: &str) -> LogAnalysis {
        parse(text, &ParseOptions::default())
    }

    fn rows_of(text: &str) -> Vec<PanelRow> {
        build_rows(&analysis_of(text), true, 1000.0)
    }

    // build_rows ----------------------------------------------------------

    #[test]
    fn build_rows_classifies_statement_kinds() {
        let rows = rows_of(MIXED_LOG);
        let kinds: Vec<&str> = rows.iter().map(|row| row.sql_type.token()).collect();
        assert!(kinds.contains(&"select"), "{kinds:?}");
        assert!(kinds.contains(&"delete"), "{kinds:?}");
        assert!(kinds.contains(&"update"), "{kinds:?}");
        assert!(kinds.contains(&"error"), "{kinds:?}");
    }

    #[test]
    fn build_rows_splits_ddl_into_create_and_drop() {
        assert_eq!(
            sql_type_of_entry("CREATE TABLE t (id INT)"),
            SqlType::Create
        );
        assert_eq!(sql_type_of_entry("DROP TABLE t"), SqlType::Drop);
        assert_eq!(sql_type_of_entry("TRUNCATE TABLE t"), SqlType::Drop);
        assert_eq!(
            sql_type_of_entry("ALTER TABLE t ADD c INT"),
            SqlType::Create
        );
    }

    fn sql_type_of_entry(sql: &str) -> SqlType {
        let entry = LogEntry {
            family: crate::core::log::Family::MyBatis,
            line_no: 1,
            timestamp_ms: None,
            duration_ms: None,
            sql: sql.to_string(),
            params: Vec::new(),
            class: if sql.to_ascii_uppercase().starts_with("CREATE")
                || sql.to_ascii_uppercase().starts_with("DROP")
                || sql.to_ascii_uppercase().starts_with("TRUNCATE")
                || sql.to_ascii_uppercase().starts_with("ALTER")
            {
                EntryClass::Ddl
            } else {
                EntryClass::Other
            },
            confidence: crate::core::log::Confidence::Exact,
            raw_sql: sql.to_string(),
            affected_rows: None,
            result_rows: None,
            raw_line: None,
            message_keys: Vec::new(),
        };
        sql_type_of(&entry)
    }

    #[test]
    fn duplicate_statements_fold_into_one_row_with_a_count() {
        let text = "\
==>  Preparing: SELECT * FROM user WHERE id = ?
==> Parameters: 1(Long)
==>  Preparing: SELECT * FROM user WHERE id = ?
==> Parameters: 1(Long)
";
        let rows = build_rows(&analysis_of(text), true, 1000.0);
        assert_eq!(rows.len(), 1, "the duplicate group is folded");
        assert_eq!(rows[0].dedupe_count, 2);
    }

    #[test]
    fn dedupe_off_keeps_every_occurrence() {
        let text = "\
==>  Preparing: SELECT * FROM user WHERE id = ?
==> Parameters: 1(Long)
==>  Preparing: SELECT * FROM user WHERE id = ?
==> Parameters: 1(Long)
";
        let rows = build_rows(&analysis_of(text), false, 1000.0);
        assert_eq!(rows.len(), 2, "without dedupe both occurrences stay");
        assert_eq!(rows[0].dedupe_count, 1);
    }

    #[test]
    fn n_plus_one_rows_carry_their_group_count() {
        let mut text = String::new();
        for id in 0..5 {
            text.push_str(&format!(
                "==>  Preparing: SELECT * FROM t WHERE id = ?\n==> Parameters: {id}(Integer)\n"
            ));
        }
        let rows = build_rows(&analysis_of(&text), false, 1000.0);
        assert_eq!(rows.len(), 5);
        assert!(rows.iter().all(|row| row.n_plus_one));
        assert_eq!(rows[0].group_count, 5);
    }

    // Filtering -----------------------------------------------------------

    #[test]
    fn empty_type_set_means_all_kinds() {
        let rows = rows_of(MIXED_LOG);
        let filter = Filter::default();
        assert_eq!(filter.types, TypeFilter::All, "default is 全部");
        let kept = filter_rows(&rows, &filter, 1000.0);
        assert_eq!(kept.len(), rows.len());
    }

    #[test]
    fn type_set_narrows_to_selected_kinds() {
        let rows = rows_of(MIXED_LOG);
        let filter = Filter {
            types: TypeFilter::Only([SqlType::Select].into_iter().collect()),
            ..Filter::default()
        };
        let kept = filter_rows(&rows, &filter, 1000.0);
        assert!(!kept.is_empty());
        assert!(kept.iter().all(|row| row.sql_type == SqlType::Select));

        let none = Filter {
            types: TypeFilter::None,
            ..Filter::default()
        };
        assert!(
            filter_rows(&rows, &none, 1000.0).is_empty(),
            "无 shows no rows"
        );
    }

    #[test]
    fn slow_only_keeps_rows_at_or_above_the_threshold() {
        let text = "\
1723456789012|999|statement|SELECT * FROM a
1723456789013|1000|statement|SELECT * FROM b
1723456789014|1200|statement|SELECT * FROM c
";
        let rows = build_rows(&analysis_of(text), true, 1000.0);
        assert_eq!(rows.len(), 3, "the p6spy rows parse: {rows:?}");
        let filter = Filter {
            slow_only: true,
            ..Filter::default()
        };
        let kept = filter_rows(&rows, &filter, 1000.0);
        // Exactly the 1000 ms and 1200 ms rows survive; 999 ms is out.
        assert_eq!(kept.len(), 2, "{kept:?}");
        assert!(
            kept.iter()
                .all(|row| row.duration_ms.is_some_and(|ms| ms >= 1000.0))
        );
    }

    #[test]
    fn keyword_case_insensitive_by_default_and_sensitive_when_asked() {
        let text = "\
==>  Preparing: SELECT Name FROM user
==> Parameters: 1(Long)
";
        let rows = rows_of(text);
        let mut filter = Filter::default();
        filter.keyword = "name".to_string();
        assert_eq!(filter_rows(&rows, &filter, 1000.0).len(), 1);
        filter.keyword = "NAME".to_string();
        assert_eq!(filter_rows(&rows, &filter, 1000.0).len(), 1);

        filter.match_case = true;
        filter.keyword = "NAME".to_string();
        assert_eq!(
            filter_rows(&rows, &filter, 1000.0).len(),
            0,
            "case-sensitive NAME does not match Name"
        );
        filter.keyword = "Name".to_string();
        assert_eq!(filter_rows(&rows, &filter, 1000.0).len(), 1);
    }

    #[test]
    fn regex_filter_matches_and_invalid_regex_is_safe() {
        let text = "\
==>  Preparing: SELECT id FROM orders WHERE id = ?
==> Parameters: 42(Long)
";
        let rows = rows_of(text);
        let mut filter = Filter::default();
        filter.regex = true;
        filter.keyword = "ord.rs".to_string();
        assert_eq!(filter_rows(&rows, &filter, 1000.0).len(), 1);

        filter.keyword = "(".to_string();
        // Must not panic on an invalid pattern.
        assert_eq!(filter_rows(&rows, &filter, 1000.0).len(), 0);
        assert!(regex_is_invalid(&filter));
    }

    #[test]
    fn row_matches_combines_type_slow_and_keyword() {
        let rows = rows_of(MIXED_LOG);
        let select = rows
            .iter()
            .find(|row| row.sql_type == SqlType::Select)
            .unwrap();
        let filter = Filter {
            types: TypeFilter::Only([SqlType::Select].into_iter().collect()),
            keyword: "users".to_string(),
            ..Filter::default()
        };
        assert!(row_matches(select, &filter, 1000.0));
        let delete_only = Filter {
            types: TypeFilter::Only([SqlType::Delete].into_iter().collect()),
            keyword: "users".to_string(),
            ..Filter::default()
        };
        assert!(!row_matches(select, &delete_only, 1000.0));
    }

    // State push ----------------------------------------------------------

    fn state_of(host: &MockHost) -> Value {
        let messages = host.view_messages();
        let last = messages.last().expect("a state message").clone();
        serde_json::from_str(&last.1).expect("state is JSON")
    }

    #[test]
    fn ready_message_pushes_rows_and_labels_without_html() {
        let (host, _guard) = test_host();
        set_log_text(MIXED_LOG);
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        let state = state_of(&host);
        assert_eq!(state["op"], "state");
        let rows = state["rows"].as_array().expect("rows array");
        assert!(!rows.is_empty());
        assert!(state["labels"]["empty"].as_str().is_some());
        // No pre-rendered HTML leaks into the state.
        let raw = state.to_string();
        assert!(!raw.contains("<div"), "no html markup in state");
        assert!(!raw.contains("<tr"), "no html markup in state");
    }

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
    fn the_packaged_page_draws_its_own_action_row() {
        // A `web =` view gets no host toolbar, so the page must offer pause,
        // clear and copy itself — as IDE-style icon buttons (L24): a 24×24
        // hit area, an inline SVG from icons/toolbar/, and the Chinese label
        // in `title` + `aria-label` instead of visible text.
        let (_host, _guard) = test_host();
        let page = include_str!("../../web/log-panel.html");
        for (id, op) in [
            ("pauseBtn", "'toggle-pause'"),
            ("clearBtn", "'clear'"),
            ("copyAllBtn", "'copy-all'"),
        ] {
            assert!(page.contains(&format!("id=\"{id}\"")), "missing {id}");
            assert!(page.contains(op), "missing op {op}");
            assert!(
                page.contains(&format!("id=\"{id}\" class=\"iconbtn\"")),
                "{id} is an icon button"
            );
        }
        // The action row draws pause + resume (one is hidden by the state),
        // clear and copy: four SVG bodies, no text buttons left.
        let bar_start = page.find("class=\"actionbar\"").expect("action row");
        let bar_end = page.find("class=\"filterbar\"").expect("filter row");
        let actionbar = &page[bar_start..bar_end];
        assert_eq!(actionbar.matches("<svg").count(), 4, "{actionbar}");
        assert!(
            !actionbar.contains("class=\"btn\""),
            "the action row has no text buttons"
        );
        for id in ["pauseIcon", "resumeIcon"] {
            assert!(page.contains(&format!("id=\"{id}\"")), "missing {id}");
        }
        // The label binds through `title` and `aria-label`, both fed by the
        // state's `labels`.
        assert!(
            page.contains("node.setAttribute('aria-label', label || '')"),
            "aria-label binding"
        );
        assert!(page.contains("node.title = label || ''"), "title binding");
        let labels = labels_json();
        for key in ["pause", "resume", "clear", "copyAll"] {
            assert!(labels.get(key).is_some(), "missing label {key}");
        }
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
    fn buffer_cap_truncates_rows_but_keeps_the_total() {
        let (host, _guard) = test_host();
        host.set_setting("logs.pageSize", "2");
        let mut text = String::new();
        for index in 0..10 {
            text.push_str(&format!(
                "==>  Preparing: SELECT c{index} FROM t\n==> Parameters: {index}(Integer)\n"
            ));
        }
        set_log_text(&text);
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        let state = state_of(&host);
        assert_eq!(state["rows"].as_array().unwrap().len(), 2, "capped to 2");
        assert_eq!(state["counts"]["total"], 10);
        assert_eq!(state["counts"]["truncated"], true);
    }

    #[test]
    fn render_pushes_state_unless_paused() {
        let (host, _guard) = test_host();
        set_log_text(MIXED_LOG);
        let _ = render_panel();
        assert_eq!(host.view_messages().len(), 1, "render pushes the state");
        set_paused(true);
        let view = render_panel();
        assert_eq!(
            host.view_messages().len(),
            1,
            "render does not push while paused"
        );
        assert!(
            view.status.is_some(),
            "the paused state shows in the status"
        );
    }

    // Tabs (lane L22) -----------------------------------------------------

    fn line_of(kind: EntryClass) -> usize {
        snapshot()
            .entries
            .iter()
            .find(|entry| entry.class == kind)
            .unwrap()
            .line_no
    }

    #[test]
    fn log_to_sql_message_restores_the_selected_line() {
        let (host, _guard) = test_host();
        set_log_text(MIXED_LOG);
        let line = line_of(EntryClass::Select);
        handle_panel_message(&host, &format!("{{\"op\":\"log-to-sql\",\"line\":{line}}}")).unwrap();
        let tab = &state_of(&host)["tabs"]["logToSql"];
        assert_eq!(tab["line"], json!(line));
        assert_eq!(tab["empty"], false);
        assert_eq!(tab["rawSql"], "SELECT id FROM users WHERE id = ?");
        assert_eq!(tab["params"], json!(["1"]));
        assert_eq!(tab["restored"], "SELECT id FROM users WHERE id = 1");
        assert_eq!(tab["confidence"], "exact");
    }

    #[test]
    fn log_to_sql_falls_back_to_the_last_statement() {
        let (host, _guard) = test_host();
        set_log_text(MIXED_LOG);
        let last = line_of(EntryClass::Update);
        handle_panel_message(&host, "{\"op\":\"log-to-sql\"}").unwrap();
        let tab = &state_of(&host)["tabs"]["logToSql"];
        assert_eq!(
            tab["line"],
            json!(last),
            "no selection shows the last statement"
        );
        assert!(
            tab["restored"]
                .as_str()
                .is_some_and(|sql| sql.contains("= 3")),
            "{tab}"
        );
    }

    #[test]
    fn log_to_sql_is_empty_when_nothing_was_captured() {
        let (host, _guard) = test_host();
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        let tab = &state_of(&host)["tabs"]["logToSql"];
        assert_eq!(tab["empty"], true);
        assert_eq!(tab["line"], Value::Null);
        assert_eq!(tab["restored"], "");
    }

    #[test]
    fn sql_to_java_message_reports_exact_and_unresolved_code() {
        let (host, _guard) = test_host();
        // `text` is the field the page sends.
        let exact = r#"{"op":"sql-to-java","text":"SELECT id, name FROM user WHERE id = ?","entity":"User"}"#;
        handle_panel_message(&host, exact).unwrap();
        let tab = state_of(&host)["tabs"]["sqlToJava"].clone();
        assert_eq!(tab["ran"], true);
        assert_eq!(tab["confidence"], "exact");
        assert!(
            tab["code"]
                .as_str()
                .is_some_and(|code| code.contains("LambdaQueryWrapper<User>")),
            "{tab}"
        );
        assert_eq!(tab["params"], json!(["param1"]));
        assert_eq!(tab["reason"], "");

        // An empty entity name cannot produce code: unresolved, no code, a reason.
        // (`sql` stays accepted as an alias of `text`.)
        let unresolved =
            r#"{"op":"sql-to-java","sql":"SELECT id, name FROM user WHERE id = ?","entity":""}"#;
        handle_panel_message(&host, unresolved).unwrap();
        let tab = state_of(&host)["tabs"]["sqlToJava"].clone();
        assert_eq!(tab["confidence"], "unresolved");
        assert_eq!(tab["code"], Value::Null, "unresolved ships no code");
        assert!(
            tab["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("entity_required")),
            "{tab}"
        );
    }

    #[test]
    fn history_message_lists_matches_and_reports_empty_results() {
        let (host, _guard) = test_host();
        set_log_text(MIXED_LOG);

        handle_panel_message(&host, "{\"op\":\"history\",\"keyword\":\"users\"}").unwrap();
        let tab = state_of(&host)["tabs"]["history"].clone();
        assert_eq!(tab["ran"], true);
        assert_eq!(tab["keyword"], "users");
        assert_eq!(tab["total"], 2, "{tab}");
        let matches = tab["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 2);
        assert!(matches.iter().all(|item| item["has_sql"] == true));
        assert!(
            matches[0]["sql"]
                .as_str()
                .is_some_and(|sql| sql.contains("SELECT id FROM users")),
            "{matches:?}"
        );
        assert!(matches[0]["time"].as_str().is_some());

        // No match is a result, not an error.
        handle_panel_message(&host, "{\"op\":\"history\",\"keyword\":\"no-such-line\"}").unwrap();
        let tab = state_of(&host)["tabs"]["history"].clone();
        assert_eq!(tab["ran"], true);
        assert_eq!(tab["total"], 0);
        assert!(tab["matches"].as_array().unwrap().is_empty());

        // An empty keyword clears the result instead of calling the handler.
        handle_panel_message(&host, "{\"op\":\"history\",\"keyword\":\"\"}").unwrap();
        let tab = state_of(&host)["tabs"]["history"].clone();
        assert_eq!(tab["ran"], false);
    }

    #[test]
    fn history_open_reveals_the_log_panel_or_copies_a_non_statement_line() {
        let (host, _guard) = test_host();
        set_log_text(MIXED_LOG);
        handle_panel_message(&host, "{\"op\":\"history-open\",\"line\":1}").unwrap();
        assert_eq!(
            host.revealed_views()
                .iter()
                .filter(|view| *view == VIEW_ID)
                .count(),
            1,
            "a parsed statement opens the preview tab of the log panel"
        );
        assert_eq!(state_of(&host)["selectTab"], "preview");

        // Line 2 is the Parameters row: not a statement, copied verbatim.
        handle_panel_message(&host, "{\"op\":\"history-open\",\"line\":2}").unwrap();
        assert!(
            host.clipboard()
                .is_some_and(|text| text.contains("Parameters")),
            "{:?}",
            host.clipboard()
        );
    }

    #[test]
    fn tab_message_is_accepted_without_pushing_state() {
        // Switching tabs is DOM-only: the host never answers `tab`, so the
        // page keeps the log list (and its scroll position) untouched.
        let (host, _guard) = test_host();
        set_log_text(MIXED_LOG);
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        let before = host.view_messages().len();
        handle_panel_message(&host, "{\"op\":\"tab\",\"tab\":\"history\"}").unwrap();
        assert_eq!(host.view_messages().len(), before, "tab pushes nothing");
        handle_panel_message(&host, "{\"op\":\"tab\"}").unwrap();
        assert_eq!(host.view_messages().len(), before);
    }

    #[test]
    fn the_page_has_five_tabs_and_keeps_the_log_list() {
        let (_host, _guard) = test_host();
        let page = include_str!("../../web/log-panel.html");
        for id in [
            "tab-logs",
            "tab-logToSql",
            "tab-sqlToJava",
            "tab-history",
            "tab-preview",
            "panelLogs",
            "panelLogToSql",
            "panelSqlToJava",
            "panelHistory",
            "panelPreview",
        ] {
            assert!(page.contains(&format!("id=\"{id}\"")), "missing {id}");
        }
        // The log tab's DOM (list + filters + status) must stay in place.
        for id in ["list", "kw", "typeBtn", "statusLeft", "pauseBtn"] {
            assert!(page.contains(&format!("id=\"{id}\"")), "missing {id}");
        }
        // Switching is `hidden` toggling only, and a state push with unchanged
        // rows must not rebuild the list.
        assert!(
            page.contains("panel.hidden = !on"),
            "tab switch hides panels"
        );
        assert!(page.contains("rowsSig"), "rows are signature guarded");
        assert!(page.contains("event.key === 'Tab'"), "Ctrl+Tab cycling");
        // Tab strip (L24): JBTabs-style cards instead of the underline. The
        // selected tab is a raised page with a 4px top radius, and a narrow
        // strip scrolls it into view (the host can switch to the fifth tab).
        assert!(
            page.contains(".tab.active"),
            "the selected tab has its own state class"
        );
        assert!(
            page.contains("border-radius: 4px 4px 0 0"),
            "the selected tab is a card"
        );
        assert!(!page.contains(".tab.sel"), "the underline tab is gone");
        assert!(
            page.contains("scrollIntoView({ inline: 'nearest', block: 'nearest' })"),
            "the active tab is scrolled into view"
        );
        assert!(!page.contains("border-bottom-color: var(--openidea-accent)"));
        assert!(page.contains("op: 'log-to-sql'"), "log-to-sql op");
        assert!(page.contains("op: 'sql-to-java'"), "sql-to-java op");
        assert!(page.contains("op: 'history'"), "history op");
        // The preview tab's ops carry the `preview-` prefix (lane L23), so the
        // log panel can route them without colliding with its own names.
        for op in [
            "preview-render",
            "preview-format",
            "preview-load-file",
            "preview-clear",
            "preview-copy",
            "preview-export",
        ] {
            assert!(page.contains(op), "missing op {op}");
        }

        let labels = labels_json();
        for key in [
            "tabLogs",
            "tabLogToSql",
            "tabSqlToJava",
            "tabHistory",
            "tabPreview",
            "tabLogToSqlEmpty",
            "tabSqlToJavaEmpty",
            "tabHistoryEmpty",
            "tabConfidenceUnresolved",
        ] {
            assert!(labels.get(key).is_some(), "missing label {key}");
        }
    }

    #[test]
    fn the_log_list_follows_the_jblist_row_style() {
        // L24: the rows read like a JBList cell — a 3px type bar, a muted
        // metadata line, the statement, and a 1px rule between rows.
        let (_host, _guard) = test_host();
        let page = include_str!("../../web/log-panel.html");
        for needle in [
            "min-height: 40px",
            ".row:hover { background: var(--openidea-bg-hover); }",
            ".row.sel { background: var(--openidea-bg-selected); }",
            ".row .bar { width: 3px; flex: 0 0 3px; }",
        ] {
            assert!(page.contains(needle), "missing row style {needle}");
        }
        // Metadata is 11px muted with monospace sequence/time/duration; the
        // statement is 12px monospace and wraps.
        for needle in [
            "align-items: center; gap: 8px; font-size: 11px;",
            ".meta .time, .meta .dur { font-family: var(--openidea-font-mono, monospace); }",
            "font-size: 12px; line-height: 1.4;",
            "white-space: pre-wrap; word-break: break-word;",
        ] {
            assert!(page.contains(needle), "missing text style {needle}");
        }
        // Badges are 8px-radius 10px capsules tinted 16% with the semantic
        // host variable; the slow duration and the error line take the
        // warning/danger ink.
        assert!(
            page.contains("border-radius: 8px; font-size: 10px;"),
            "capsule badges"
        );
        for tint in ["fg-muted", "warning", "danger"] {
            let rule = format!("color-mix(in srgb, var(--openidea-{tint}) 16%, transparent)");
            assert!(page.contains(&rule), "missing {tint} tint");
        }
        assert!(
            page.contains(".meta .dur.hot { color: var(--openidea-warning);"),
            "slow duration in warning"
        );
        assert!(
            page.contains("id=\"slowBadge\" class=\"pill danger\""),
            "the slow count is a right-side capsule"
        );
    }

    #[test]
    fn the_log_page_uses_no_literal_colours() {
        // Everything visual rides on the host's `--openidea-*` variables, so a
        // theme switch recolours the panel without touching the page.
        let (_host, _guard) = test_host();
        let page = include_str!("../../web/log-panel.html");
        for needle in ["#6C707E", "#DB3B4B", "#208A3C", "#F2FCF3", "rgb(", "hsl("] {
            assert!(!page.contains(needle), "literal colour {needle}");
        }
    }

    #[test]
    fn the_preview_tab_carries_the_workbench_ui() {
        let (_host, _guard) = test_host();
        let page = include_str!("../../web/log-panel.html");
        // The input pane: statement box plus the four actions the IntelliJ
        // panel's top half has.
        for id in [
            "pvInput",
            "pvResolve",
            "pvFormat",
            "pvLoadFile",
            "pvClear",
            "pvOpenDialog",
        ] {
            assert!(page.contains(&format!("id=\"{id}\"")), "missing {id}");
        }
        // The preview pane: read-only dialect label, copy / format / mode /
        // export, parameters beside the SQL tab, and the disabled Execute.
        for id in [
            "pvDialectChip",
            "pvCopy",
            "pvFormatPreview",
            "pvMode",
            "pvExport",
            "pvParams",
            "pvTabBody",
            "pvExecute",
            "pvCopySql",
            "pvStatus",
        ] {
            assert!(page.contains(&format!("id=\"{id}\"")), "missing {id}");
        }
        for tab in ["sql", "raw", "columns"] {
            assert!(
                page.contains(&format!("data-pvtab=\"{tab}\"")),
                "missing inner tab {tab}"
            );
        }
        // Execute stays disabled with the reason in its tooltip; the page has
        // no external link, no storage and builds no markup from data.
        assert!(page.contains("pvSig"), "the tab body is signature guarded");
        assert!(page.contains("id=\"pvExecute\" class=\"btn\" type=\"button\" disabled"));
        assert!(
            page.contains("openidea.postMessage"),
            "the page talks to the plugin only"
        );
        for needle in [
            "http://",
            "https://",
            "localStorage",
            "fetch(",
            "XMLHttpRequest",
            "innerHTML",
            "outerHTML",
            "insertAdjacentHTML",
            "eval(",
        ] {
            assert!(!page.contains(needle), "web page must not contain {needle}");
        }
    }

    // -- preview tab routing (lane L23) ------------------------------------

    #[test]
    fn a_preview_render_message_reaches_the_workbench_model_and_the_state() {
        let (host, _guard) = test_host();
        handle_panel_message(
            &host,
            "{\"op\":\"preview-render\",\"text\":\"select 7\",\"kind\":\"sql\"}",
        )
        .unwrap();
        let preview = &state_of(&host)["tabs"]["preview"];
        assert_eq!(preview["model"]["sql"], "select 7");
        assert_eq!(preview["model"]["confidence"], "exact");
        assert_eq!(
            preview["replaceInput"], true,
            "the input box follows the render"
        );
        assert_eq!(
            crate::views::workbench::model().sql,
            "select 7",
            "the host-side model is the one the ops use"
        );
    }

    #[test]
    fn a_preview_copy_message_reaches_the_clipboard_and_the_state() {
        let (host, _guard) = test_host();
        handle_panel_message(
            &host,
            "{\"op\":\"preview-render\",\"text\":\"select 8\",\"kind\":\"sql\"}",
        )
        .unwrap();
        handle_panel_message(&host, "{\"op\":\"preview-copy\"}").unwrap();
        assert_eq!(host.clipboard().as_deref(), Some("select 8"));
        assert_eq!(
            state_of(&host)["tabs"]["preview"]["status"]["text"],
            t("view.workbench.copied")
        );
        assert_eq!(
            state_of(&host)["tabs"]["preview"]["replaceInput"],
            false,
            "a copy leaves the input box alone"
        );
    }

    #[test]
    fn a_preview_export_message_writes_the_output_channel() {
        let (host, _guard) = test_host();
        handle_panel_message(
            &host,
            "{\"op\":\"preview-render\",\"text\":\"select 9\",\"kind\":\"sql\"}",
        )
        .unwrap();
        handle_panel_message(&host, "{\"op\":\"preview-export\"}").unwrap();
        let out = &host.output()[common::CHANNEL];
        assert!(out.contains("mybatis-sql workbench export"), "{out}");
        // The body is the statement formatted for the dialect, so the
        // whitespace differs from what was typed.
        let compact = out.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(compact.to_lowercase().contains("select 9"), "{out}");
    }

    #[test]
    fn the_select_tab_request_survives_until_the_page_reports_its_tab() {
        let (host, _guard) = test_host();
        crate::views::workbench::request_tab();
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        assert_eq!(state_of(&host)["selectTab"], "preview");
        // Every push keeps the request until the page confirms.
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        assert_eq!(state_of(&host)["selectTab"], "preview");
        handle_panel_message(&host, "{\"op\":\"tab\",\"tab\":\"preview\"}").unwrap();
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        assert!(
            state_of(&host)["selectTab"].is_null(),
            "the page confirmed its tab, so the request is done"
        );
    }

    #[test]
    fn the_bare_ops_of_the_log_panel_do_not_touch_the_preview() {
        let (host, _guard) = test_host();
        set_log_text(MIXED_LOG);
        crate::views::workbench::set_model(crate::views::workbench::WorkbenchModel::new(
            "select keep",
            Vec::new(),
            "MySQL",
        ));
        handle_panel_message(
            &host,
            "{\"op\":\"preview-render\",\"text\":\"select keep\"}",
        )
        .unwrap();
        // `clear` empties the log buffer; the preview model stays.
        handle_panel_message(&host, "{\"op\":\"clear\"}").unwrap();
        assert_eq!(crate::views::workbench::model().sql, "select keep");
        assert_eq!(
            state_of(&host)["tabs"]["preview"]["model"]["sql"],
            "select keep"
        );
    }

    // Helpers -------------------------------------------------------------

    fn handle_panel_message(host: &MockHost, message: &str) -> Result<(), Error> {
        handle_panel_event(ViewEvent::Message(message.to_string()), &host.invocation())
    }

    // Utilities -----------------------------------------------------------

    #[test]
    fn epoch_timestamps_render_as_utc() {
        assert_eq!(format_epoch_ms(0), "1970-01-01 00:00:00.000");
        assert_eq!(
            format_epoch_ms(1_705_314_645_123),
            "2024-01-15 10:30:45.123"
        );
    }

    #[test]
    fn push_api_replaces_and_appends_without_losing_lines() {
        let (_host, _guard) = test_host();
        set_log_text("==>  Preparing: SELECT 1");
        append_text("==>  Preparing: SELECT 2");
        assert_eq!(
            buffered_len(),
            "==>  Preparing: SELECT 1\n==>  Preparing: SELECT 2".len()
        );
        assert_eq!(snapshot().entries.len(), 2);
    }
}
