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

/// Drops the buffered text and the derived groups (panel Clear button).
pub fn clear() {
    let mut state = store();
    state.text.clear();
    state.analysis = empty_analysis();
    state.dirty = false;
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
    drop(state);

    let stats = crate::capture::bridge::Stats::current();

    json!({
        "op": "state",
        "rows": rows_json,
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
            crate::views::workbench::VIEW_ID => Ok(crate::views::workbench::render_workbench()),
            other => Err(Error::NotFound(format!("view {other}"))),
        }
    }

    fn handle(&mut self, view: &str, event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
        match view {
            VIEW_ID => handle_panel_event(event, ctx),
            crate::views::workbench::VIEW_ID => {
                crate::views::workbench::handle_workbench_event(event)
            }
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
        // clear and copy itself, with the labels the state carries.
        let (_host, _guard) = test_host();
        let page = include_str!("../../web/log-panel.html");
        for (id, op) in [
            ("pauseBtn", "'toggle-pause'"),
            ("clearBtn", "'clear'"),
            ("copyAllBtn", "'copy-all'"),
        ] {
            assert!(page.contains(&format!("id=\"{id}\"")), "missing {id}");
            assert!(page.contains(op), "missing op {op}");
        }
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
    fn open_line_message_reveals_the_workbench() {
        let (host, _guard) = test_host();
        set_log_text(MIXED_LOG);
        let line = snapshot()
            .entries
            .iter()
            .find(|entry| entry.has_sql())
            .unwrap()
            .line_no;
        handle_panel_message(&host, &format!("{{\"op\":\"open-line\",\"line\":{line}}}")).unwrap();
        assert!(
            host.revealed_views()
                .iter()
                .any(|view| view == crate::views::workbench::VIEW_ID),
            "double-click opens the workbench"
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
