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

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex, MutexGuard, OnceLock};

use openidea_plugin::host;
use openidea_plugin::views::{Body, Button, View, ViewEvent, Views};
use openidea_plugin::{Error, Invocation, NotificationLevel};
use serde_json::{Value, json};

use crate::commands::common;
use crate::core::log::{
    DuplicateGroup, EntryClass, LineSource, LogAnalysis, LogEntry, LogSources, NPlusOneGroup,
    ParseOptions, SourceRef, parse_with_sources, restore_rendered,
};
use crate::i18n::t;
use crate::indexer;
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
    /// The dedupe group's key when the row is the folded representative
    /// (lane L26: the ⨉N badge toggles this group); empty otherwise.
    pub dedupe_key: String,
    /// The N+1 group's template key when the row belongs to one (lane L26:
    /// the ×N badge toggles this group); empty otherwise.
    pub n_plus_one_key: String,
    /// `<== Total:` affected-row count, when the family prints one.
    pub affected_rows: Option<u64>,
    /// Where the row came from: the logger-derived class/method plus the
    /// console thread and correlation id, when the row stated them (lane L28).
    pub source: LineSource,
    /// Original text for error rows (kept when no SQL was recovered).
    pub raw_line: Option<String>,
}

impl PanelRow {
    /// `UserMapper` — the Java file's own name, empty when the row stated none.
    pub fn class_simple(&self) -> &str {
        self.source
            .source
            .as_ref()
            .map(|source| source.class_simple.as_str())
            .unwrap_or("")
    }

    /// `selectById`, empty when the row stated no statement id.
    pub fn method(&self) -> &str {
        self.source
            .source
            .as_ref()
            .map(|source| source.method.as_str())
            .unwrap_or("")
    }

    /// `c.e.m.mapper.UserMapper` — the logger name as written (Spring may have
    /// shortened it), empty when the row stated none.
    pub fn class_name(&self) -> &str {
        self.source
            .source
            .as_ref()
            .map(|source| source.class_name.as_str())
            .unwrap_or("")
    }

    /// `class_name#method`, the value a method focus chip carries.
    pub fn focus_key(&self) -> Option<String> {
        self.source.source.as_ref().map(SourceRef::key)
    }

    /// The logger-derived source, if the row has one.
    pub fn source_ref(&self) -> Option<&SourceRef> {
        self.source.source.as_ref()
    }

    pub fn thread(&self) -> &str {
        self.source.thread.as_deref().unwrap_or("")
    }

    pub fn request_id(&self) -> &str {
        self.source.request_id.as_deref().unwrap_or("")
    }

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

/// What a focus chip filters by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusKind {
    /// The logger-derived `class_name#method` of the statement.
    Method,
    /// The row's correlation id (`traceId=…`).
    Request,
}

impl FocusKind {
    pub fn token(self) -> &'static str {
        match self {
            FocusKind::Method => "method",
            FocusKind::Request => "request",
        }
    }

    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "method" => Some(FocusKind::Method),
            "request" => Some(FocusKind::Request),
            _ => None,
        }
    }
}

/// One active focus chip. A chip is a filter, not a group: the list stays flat
/// and time-ordered, rows that do not match are simply not shown.
///
/// At most one chip is active (the reference panel's per-dimension tabs are a
/// later refinement); clicking the same value again clears it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Focus {
    pub kind: FocusKind,
    pub value: String,
}

impl Focus {
    /// Does `row` satisfy this focus?
    pub fn matches(&self, row: &PanelRow) -> bool {
        match self.kind {
            FocusKind::Method => row.focus_key().is_some_and(|key| key == self.value),
            FocusKind::Request => !self.value.is_empty() && row.request_id() == self.value,
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
    /// The active focus chip, if any.
    pub focus: Option<Focus>,
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
            focus: None,
        }
    }
}

/// The display order of the pushed rows (lane L26, the "sort by duration"
/// toolbar button). `Time` keeps capture order; `Duration` orders by the
/// statement's duration, longest first, rows without a duration last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMode {
    Time,
    Duration,
}

impl SortMode {
    pub fn token(self) -> &'static str {
        match self {
            SortMode::Time => "time",
            SortMode::Duration => "duration",
        }
    }

    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "duration" => Some(SortMode::Duration),
            "time" => Some(SortMode::Time),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Row assembly
// ---------------------------------------------------------------------------

/// Collapses duplicate groups (when `dedupe`), tags N+1 rows and classifies
/// each row. Rows keep `line_no` order.
pub fn build_rows(analysis: &LogAnalysis, dedupe: bool, slow_threshold_ms: f64) -> Vec<PanelRow> {
    build_rows_with(
        analysis,
        None,
        &BTreeSet::new(),
        &BTreeSet::new(),
        dedupe,
        slow_threshold_ms,
    )
}

/// [`build_rows`] with the panel's row-level state: the parsed provenance, the
/// lines the user deleted and the methods the user blocked.
///
/// A removed line and a blocked method drop the entry before anything else, so
/// neither reaches the dedupe fold, the filters or the push.
pub fn build_rows_with(
    analysis: &LogAnalysis,
    sources: Option<&LogSources>,
    removed: &BTreeSet<usize>,
    blocked: &BTreeSet<String>,
    dedupe: bool,
    slow_threshold_ms: f64,
) -> Vec<PanelRow> {
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
        if removed.contains(&entry.line_no) {
            continue;
        }
        let source = sources
            .and_then(|sources| sources.get(entry.line_no))
            .cloned()
            .unwrap_or_default();
        if source
            .source
            .as_ref()
            .is_some_and(|source| blocked.contains(&source.key()))
        {
            continue;
        }
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
                source,
                group.key.clone(),
                n_plus_one
                    .map(|group| group.template.clone())
                    .unwrap_or_default(),
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
            source,
            String::new(),
            n_plus_one
                .map(|group| group.template.clone())
                .unwrap_or_default(),
        ));
    }
    rows
}

/// The rows the panel shows: the folded rows, with the groups the user
/// expanded (lane L26) unfolded into their occurrences.
///
/// A group is keyed `d:<dedupe key>` (the ⨉N badge) or `n:<n+1 template>`
/// (the ×N badge). When a key is expanded, the group's occurrences replace
/// the folded representative: the first occurrence keeps the group's counts
/// (its badges stay, now collapsing), the rest are members carrying the same
/// keys so the page renders them indented. When both of a row's groups are
/// expanded, the occurrences are emitted once, as the union of both groups.
fn display_rows(
    analysis: &LogAnalysis,
    sources: &LogSources,
    removed: &BTreeSet<usize>,
    blocked: &BTreeSet<String>,
    dedupe: bool,
    slow_threshold_ms: f64,
    expanded: &BTreeSet<String>,
) -> Vec<PanelRow> {
    let rows = build_rows_with(
        analysis,
        Some(sources),
        removed,
        blocked,
        dedupe,
        slow_threshold_ms,
    );
    if expanded.is_empty() {
        return rows;
    }
    let entries_by_line: HashMap<usize, &LogEntry> = analysis
        .entries
        .iter()
        .map(|entry| (entry.line_no, entry))
        .collect();
    let dedupe_by_key: HashMap<String, &DuplicateGroup> = analysis
        .duplicates
        .iter()
        .map(|group| (group.key.clone(), group))
        .collect();
    let n_plus_one_by_key: HashMap<String, &NPlusOneGroup> = analysis
        .n_plus_one
        .iter()
        .map(|group| (group.template.clone(), group))
        .collect();

    let mut out = Vec::new();
    let mut emitted: BTreeSet<String> = BTreeSet::new();
    for row in rows {
        let d_key = dedupe_key_token(&row.dedupe_key);
        let n_key = n_plus_one_key_token(&row.n_plus_one_key);
        let d_expand = !d_key.is_empty() && expanded.contains(&d_key);
        let n_expand = !n_key.is_empty() && expanded.contains(&n_key);
        if !d_expand && !n_expand {
            out.push(row);
            continue;
        }
        // The first row of an expanded group emits its occurrences; the other
        // rows of the same group (possible when dedupe is off) are replaced by
        // that emission rather than repeated.
        let d_new = d_expand && emitted.insert(d_key.clone());
        let n_new = n_expand && emitted.insert(n_key.clone());
        if !d_new && !n_new {
            continue;
        }
        let mut lines: Vec<usize> = Vec::new();
        if d_new {
            if let Some(group) = dedupe_by_key.get(&row.dedupe_key) {
                lines.extend(group.line_nos.iter().copied());
            }
        }
        if n_new {
            if let Some(group) = n_plus_one_by_key.get(&row.n_plus_one_key) {
                for line in &group.line_nos {
                    if !lines.contains(line) {
                        lines.push(*line);
                    }
                }
            }
        }
        let n1_group = n_plus_one_by_key.get(&row.n_plus_one_key).copied();
        for (index, line) in lines.iter().enumerate() {
            let Some(entry) = entries_by_line.get(line).copied() else {
                continue;
            };
            let source = sources.get(*line).cloned().unwrap_or_default();
            let member = row_of(
                entry,
                categorize(entry, n1_group, slow_threshold_ms),
                sql_type_of(entry),
                if index == 0 { row.dedupe_count } else { 1 },
                if index == 0 { row.group_count } else { 1 },
                n1_group.is_some(),
                source,
                row.dedupe_key.clone(),
                row.n_plus_one_key.clone(),
            );
            out.push(member);
        }
    }
    out
}

/// The expanded-set token for a dedupe group key.
fn dedupe_key_token(key: &str) -> String {
    format!("d:{key}")
}

/// The expanded-set token for an N+1 template key.
fn n_plus_one_key_token(key: &str) -> String {
    format!("n:{key}")
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
    source: LineSource,
    dedupe_key: String,
    n_plus_one_key: String,
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
        dedupe_key,
        n_plus_one_key,
        affected_rows: entry.affected_rows,
        source,
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
    // A focus chip is a filter: rows without that dimension are hidden, never
    // grouped or reordered.
    if filter
        .focus
        .as_ref()
        .is_some_and(|focus| !focus.matches(row))
    {
        return false;
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

/// [`filter_rows`] without the clone, for the push path: a 5k-row buffer is
/// filtered on every push, and cloning every row there is the push's largest
/// avoidable cost.
pub fn filter_row_refs<'a>(
    rows: &'a [PanelRow],
    filter: &Filter,
    slow_threshold_ms: f64,
) -> Vec<&'a PanelRow> {
    rows.iter()
        .filter(|row| row_matches(row, filter, slow_threshold_ms))
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
    /// Per-entry provenance, joined by `line_no` (lane L25's pass).
    sources: LogSources,
    /// Lines the user removed from the buffer (`删除`).
    removed: BTreeSet<usize>,
    /// `class_name#method` keys the user blocked for this session (`屏蔽此方法`).
    blocked: BTreeSet<String>,
    /// The groups the user expanded (`×N` / `⨉N` badges, lane L26), keyed
    /// `d:<dedupe key>` / `n:<n+1 template>`.
    expanded: BTreeSet<String>,
    dirty: bool,
    filter: Filter,
    /// Once the page sends a filter, its `dedupe` choice wins over the setting.
    filter_touched: bool,
    paused: bool,
    /// Whether the list auto-scrolls to the newest row (lane L26). The page
    /// reports a flip when the user scrolls away from the bottom; the
    /// "scroll to bottom" button and scrolling back restore it.
    follow: bool,
    /// The display order (`time` / `duration`), lane L26.
    sort: SortMode,
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
            sources: LogSources::default(),
            removed: BTreeSet::new(),
            blocked: BTreeSet::new(),
            expanded: BTreeSet::new(),
            dirty: false,
            filter: Filter::default(),
            filter_touched: false,
            paused: false,
            follow: true,
            sort: SortMode::Time,
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
///
/// Line numbers refer to the new text, so the lines the user deleted from the
/// old one no longer mean anything and are dropped.
pub fn set_log_text(text: &str) {
    let mut state = store();
    state.text.clear();
    state.text.push_str(text);
    state.removed.clear();
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
    state.sources = LogSources::default();
    state.removed.clear();
    state.blocked.clear();
    state.filter.focus = None;
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
    // One pass for the analysis plus one for the provenance; rows that stated
    // nothing are not stored (lane L25).
    let parsed = parse_with_sources(&state.text, &options);
    state.analysis = parsed.analysis;
    state.sources = parsed.sources;
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

/// The dedupe choice the display uses: the page's filter wins once it sent
/// one, the setting decides before that.
fn dedupe_active(state: &Store) -> bool {
    if state.filter_touched {
        state.filter.dedupe
    } else {
        settings::get_bool("log.dedupe", true)
    }
}

// ---------------------------------------------------------------------------
// State push
// ---------------------------------------------------------------------------

/// Builds the full push state (see the lane contract in `docs/registration/L17.md`).
fn build_state() -> Value {
    let mut state = store();
    ensure_parsed(&mut state);

    let slow_threshold_ms = slow_threshold();
    let dedupe = dedupe_active(&state);
    let rows = display_rows(
        &state.analysis,
        &state.sources,
        &state.removed,
        &state.blocked,
        dedupe,
        slow_threshold_ms,
        &state.expanded,
    );

    let filter = state.filter.clone();
    let mut filtered = filter_row_refs(&rows, &filter, slow_threshold_ms);
    // Lane L26: `duration` orders longest first (rows without a duration last,
    // stable so capture order survives among equals). The buffer cap then
    // keeps the first `cap` rows of the sorted order.
    if state.sort == SortMode::Duration {
        filtered.sort_by(|a, b| match (a.duration_ms, b.duration_ms) {
            (Some(x), Some(y)) => y.partial_cmp(&x).unwrap_or(Ordering::Equal),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        });
    }
    let total = filtered.len();
    let slow = filtered
        .iter()
        .filter(|row| row.duration_ms.is_some_and(|ms| ms >= slow_threshold_ms))
        .count();
    let n_plus_one = filtered.iter().filter(|row| row.n_plus_one).count();
    let blocked = state.blocked.len();

    let cap = buffer_cap();
    let start = total.saturating_sub(cap);
    let shown = &filtered[start..];
    let truncated = total > shown.len();

    // The two location indexes are queried once per push, on the rows actually
    // sent, and only through memory: each lookup is a hash lookup over an index
    // the host filled in its own background batches. No file is read here.
    let java_targets: Vec<Option<(String, u32)>> = {
        let java = indexer::java_methods();
        shown.iter().map(|row| java_target(row, &java)).collect()
    };
    let xml_targets: Vec<Option<(String, u32)>> = {
        let xml = indexer::xml_statements();
        shown.iter().map(|row| xml_target(row, &xml)).collect()
    };

    let rows_json: Vec<Value> = shown
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let jump = java_targets.get(index).and_then(Option::as_ref);
            let xml = xml_targets.get(index).and_then(Option::as_ref);
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
                // Provenance (lane L28). `class_simple` + `method` are what the
                // row header shows; `focus_key` is what a method chip filters by.
                "class_name": row.class_name(),
                "class_simple": row.class_simple(),
                "method": row.method(),
                "focus_key": row.focus_key(),
                "thread": row.thread(),
                "request_id": row.request_id(),
                // Only a unique resolution is offered; a multi-candidate row
                // states nothing rather than picking one (1-based lines).
                "jump_file": jump.map(|(file, _)| file.clone()),
                "jump_line": jump.map(|(_, line)| line + 1),
                "xml_file": xml.map(|(file, _)| file.clone()),
                "xml_line": xml.map(|(_, line)| line + 1),
                "sql": row.display_sql(),
                "error_text": row.error_text(),
                "category": row.category.token(),
                "sql_type": row.sql_type.token(),
                // Group expansion (lane L26): the keys the ⨉N / ×N badges
                // toggle, and whether this row belongs to an expanded group.
                "dedupe_key": row.dedupe_key,
                "n_plus_one_key": row.n_plus_one_key,
                "expanded": state.expanded.contains(&dedupe_key_token(&row.dedupe_key))
                    || state.expanded.contains(&n_plus_one_key_token(&row.n_plus_one_key)),
            })
        })
        .collect();

    let console_lines = state.text.lines().count() as u64;
    let paused = state.paused;
    let follow = state.follow;
    let sort = state.sort;
    let blocked_keys: Vec<String> = state.blocked.iter().cloned().collect();
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
            // The active focus chip; the page only draws it (the filtering is
            // this module's).
            "focus": match &filter.focus {
                Some(focus) => json!({"kind": focus.kind.token(), "value": focus.value}),
                None => Value::Null,
            },
            // The blocked keys themselves (lane L26: the eye-off popup lists
            // them); the count stays in `counts.blocked`.
            "blocked": blocked_keys,
        },
        "counts": {
            "total": total,
            "shown": shown.len(),
            "slow": slow,
            "n_plus_one": n_plus_one,
            "truncated": truncated,
            "cap": cap,
            "blocked": blocked,
        },
        "status": {
            "console_lines": console_lines,
            "agent_connected": stats.connected,
            "agent_events": stats.events,
            "paused": paused,
            "follow": follow,
        },
        "sort": sort.token(),
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
// Row actions: jump / focus / delete / block (lane L28)
// ---------------------------------------------------------------------------

/// Where "Jump to Java" goes: the one Java method the row's logger names.
///
/// `lookup_source_unique` answers `None` for an overload set or two classes of
/// the same name; the row then offers no jump instead of picking one.
fn java_target(row: &PanelRow, java: &indexer::JavaMethodIndex) -> Option<(String, u32)> {
    let source = row.source_ref()?;
    java.lookup_source_unique(source)
        .map(|location| (location.file.clone(), location.line))
}

/// Where "Jump to Mapper XML" goes: the one mapper statement with this id.
///
/// `resolve_statement` on the Java index maps the same id to the *interface*
/// method, which is the "Jump to Java" target; the XML element needs its own
/// index ([`indexer::xml_statements`]). A statement declared in two namespaces
/// that both match is ambiguous and yields no jump.
fn xml_target(row: &PanelRow, xml: &indexer::XmlStatementIndex) -> Option<(String, u32)> {
    let source = row.source_ref()?;
    xml.lookup_unique(&source.class_name, &source.method)
        .map(|location| (location.file.clone(), location.line))
}

/// Opens `file` at the 0-based `line`, falling back to a visible copy when the
/// host refuses (a window-less call, a closed project).
///
/// The fallback is what `docs/LIMITS.md` promises for a host that cannot open
/// files: the position still reaches the user, on the clipboard and in the
/// Output channel.
fn reveal(file: &str, line: u32) -> Result<(), Error> {
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

/// The label map the page binds (see [`labels_base`] and [`labels_actions`]).
///
/// The two halves are separate `json!` objects because one object literal of
/// this size exceeds the macro's recursion limit; merging keeps the page's
/// single `labels` object.
fn labels_json() -> Value {
    let mut base = labels_base();
    if let (Some(target), Value::Object(extra)) = (base.as_object_mut(), labels_actions()) {
        target.extend(extra);
    }
    base
}

fn labels_base() -> Value {
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
        // Row actions (lane L28).
        "menuJumpJava": t("view.logs.menu.jump_java"),
        "menuJumpXml": t("view.logs.menu.jump_xml"),
        "menuFocusMethod": t("view.logs.menu.focus_method"),
        "menuFocusRequest": t("view.logs.menu.focus_request"),
        "menuDelete": t("view.logs.menu.delete"),
        "menuBlockMethod": t("view.logs.menu.block_method"),
        "focusMethodPrefix": t("view.logs.focus.method_prefix"),
        "focusRequestPrefix": t("view.logs.focus.request_prefix"),
        "focusClear": t("view.logs.focus.clear"),
        "blockedPrefix": t("view.logs.blocked.prefix"),
        "blockedClear": t("view.logs.blocked.clear"),
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

/// Toolbar actions and interactions (lane L26).
fn labels_actions() -> Value {
    json!({
        "captureStart": t("view.logs.button.capture_start"),
        "captureStop": t("view.logs.button.capture_stop"),
        "scrollEnd": t("view.logs.button.scroll_end"),
        "sortDuration": t("view.logs.button.sort_duration"),
        "sortTimeTitle": t("view.logs.sort.time_title"),
        "sortDurationTitle": t("view.logs.sort.duration_title"),
        "blocked": t("view.logs.button.blocked"),
        "blockedTitle": t("view.logs.blocked.title"),
        "blockedEmpty": t("view.logs.blocked.empty"),
        "settings": t("view.logs.button.settings"),
        "help": t("view.logs.button.help"),
        "followOn": t("view.logs.status.follow_on"),
        "followOff": t("view.logs.status.follow_off"),
        "groupExpand": t("view.logs.group.expand"),
        "groupCollapse": t("view.logs.group.collapse"),
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
        // Row actions (lane L28): jump to the method / to the mapper XML, drop
        // the line, block the method. Each is a no-op when the row does not
        // state the dimension — the page disables those items, the host never
        // guesses a target.
        "jump-java" => {
            if let Some(line) = message_line(&message) {
                jump_java(line)?;
            }
        }
        "jump-xml" => {
            if let Some(line) = message_line(&message) {
                jump_xml(line)?;
            }
        }
        "delete-line" => {
            if let Some(line) = message_line(&message) {
                remove_line(line);
                let _ = push_state();
            }
        }
        "block-method" => {
            if let Some(line) = message_line(&message)
                && block_line(line).is_some()
            {
                let _ = push_state();
            }
        }
        "clear-blocked" => {
            unblock_all();
            let _ = push_state();
        }
        // Capture controls (lane L26): the toolbar buttons forward to the
        // existing commands, which write their own Output lines (connected /
        // already connected / the failure reason); the panel only refreshes the
        // state so the status line follows the connection.
        "capture-start" => {
            let _ = crate::commands::capture_start::run(ctx, None);
            let _ = push_state();
        }
        "capture-stop" => {
            let _ = crate::commands::capture_stop::run(ctx, None);
            let _ = push_state();
        }
        // Follow (lane L26): the page reports a flip when the user scrolls
        // away from the bottom; "scroll to bottom" restores it.
        "follow" => {
            let follow = message
                .get("follow")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            store().follow = follow;
            let _ = push_state();
        }
        "scroll-end" => {
            store().follow = true;
            let _ = push_state();
            host::refresh_view(VIEW_ID);
        }
        // Display order (lane L26): the sort-by-duration button toggles.
        "sort" => {
            if let Some(mode) = message
                .get("sort")
                .and_then(Value::as_str)
                .and_then(SortMode::parse)
            {
                store().sort = mode;
                let _ = push_state();
            }
        }
        // Group expansion (lane L26): the ⨉N / ×N badges toggle their group.
        "toggle-group" => {
            if let Some(line) = message_line(&message) {
                let kind = message
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("group");
                toggle_group(line, kind);
                let _ = push_state();
            }
        }
        // The platform has no API to open the settings page, so the button
        // points the user there through a notification and the Output channel
        // instead of doing nothing.
        "settings" => {
            host::notify(
                NotificationLevel::Info,
                &t("view.logs.settings.title"),
                &t("view.logs.settings.guide"),
            );
            common::emit(&format!(
                "-- {}\n{}\n",
                t("view.logs.settings.title"),
                t("view.logs.settings.guide")
            ))?;
        }
        // Help: a one-screen shortcut/interaction cheat sheet in the Output
        // channel.
        "help" => {
            common::emit(&help_text())?;
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
    drop(state);
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
        // The location indexes are process-wide too (lane L28).
        crate::indexer::clear_indexes();
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
        // The panel parses with the provenance pass (lane L28); the helpers keep
        // the same pass so a fixture's sources are present too.
        parse_with_sources(text, &ParseOptions::default()).analysis
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
            // Lane L26 toolbar actions.
            ("captureBtn", "'capture-start'"),
            ("scrollEndBtn", "'scroll-end'"),
            ("sortBtn", "'sort'"),
            ("blockedBtn", "'clear-blocked'"),
            ("settingsBtn", "'settings'"),
            ("helpBtn", "'help'"),
        ] {
            assert!(page.contains(&format!("id=\"{id}\"")), "missing {id}");
            assert!(page.contains(op), "missing op {op}");
            assert!(
                page.contains(&format!("id=\"{id}\" class=\"iconbtn\"")),
                "{id} is an icon button"
            );
        }
        // The action row draws pause + resume (one is hidden by the state),
        // clear, copy, capture run + stop, scroll-to-bottom, sort, blocked,
        // settings and help: eleven SVG bodies, no text buttons left.
        let bar_start = page.find("class=\"actionbar\"").expect("action row");
        let bar_end = page.find("class=\"filterbar\"").expect("filter row");
        let actionbar = &page[bar_start..bar_end];
        assert_eq!(actionbar.matches("<svg").count(), 11, "{actionbar}");
        assert!(
            !actionbar.contains("class=\"btn\""),
            "the action row has no text buttons"
        );
        for id in [
            "pauseIcon",
            "resumeIcon",
            "captureRunIcon",
            "captureStopIcon",
        ] {
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
        for key in [
            "pause",
            "resume",
            "clear",
            "copyAll",
            "captureStart",
            "captureStop",
            "scrollEnd",
            "sortDuration",
            "blocked",
            "settings",
            "help",
            "followOn",
            "followOff",
            "groupExpand",
            "groupCollapse",
        ] {
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
            "align-items: center; justify-content: flex-start;",
            "gap: 8px; font-size: 11px;",
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
    fn the_row_metadata_is_a_left_aligned_flow() {
        // The reference row header is one left-to-right flow:
        // #seq time duration [db] (rows) ⨉N ×N N+1 [file:line] [Class#method].
        // The duration is *not* a right-aligned corner badge; the only
        // right-aligned element of the panel is the slow capsule.
        let (_host, _guard) = test_host();
        let page = include_str!("../../web/log-panel.html");
        let meta_start = page.find("    .meta {").expect("the meta rule");
        let meta_end = page.find(".meta .seq").expect("the next meta rule");
        let meta = &page[meta_start..meta_end];
        assert!(meta.contains("justify-content: flex-start;"), "{meta}");
        assert!(!meta.contains("space-between"), "{meta}");
        assert!(!meta.contains("margin-left: auto"), "{meta}");
        assert!(
            !page.contains(".meta .dur { margin-left"),
            "the duration stays in the flow"
        );
        assert!(
            page.contains("#slowBadge { margin-left: auto; }"),
            "the slow capsule is the one right-aligned element"
        );

        let build_start = page
            .find("function buildRow(row, slowThreshold, isMember)")
            .unwrap();
        let build_end = page.find("function formatMs(ms)").unwrap();
        let build = &page[build_start..build_end];
        let seq = build.find("makePart('seq'").expect("seq");
        let time = build.find("makePart('part time'").expect("time");
        let dur = build.find("dur.className = 'dur'").expect("duration");
        let badge = build.find("makePart('badge dedupe'").expect("a badge");
        let source = build.find("makePart('part src'").expect("source");
        let method = build
            .find("makePart('part method focusable'")
            .expect("method");
        assert!(seq < time, "seq before time");
        assert!(time < dur, "duration right after the time");
        assert!(dur < badge, "badges after the duration");
        assert!(badge < source, "the source comes after the badges");
        assert!(source < method, "[file:line] before [Class#method]");
    }

    #[test]
    fn the_row_header_shows_the_class_and_method_and_filters_on_click() {
        let (_host, _guard) = test_host();
        let page = include_str!("../../web/log-panel.html");
        // The chip container sits in the filter row, before the slow capsule.
        assert!(
            page.contains("id=\"focusChips\" class=\"chips\""),
            "the chip container is in the filter row"
        );
        // The row header draws [Class#method] from class_simple + method, and a
        // row that states none draws nothing.
        assert!(page.contains("row.class_simple && row.method"));
        assert!(page.contains("'[' + row.class_simple + '#' + row.method + ']'"));
        assert!(page.contains("setFocus('method', row.focus_key)"));
        assert!(page.contains("setFocus('request', row.request_id)"));
        // A chip carries an × and rides the same `filter` op.
        assert!(page.contains("close.className = 'fclose'"));
        assert!(page.contains("close.textContent = '×'"));
        assert!(page.contains("function clearFocus()"));
        assert!(
            page.contains("focus: ui.focus"),
            "the chip rides the filter op"
        );

        let labels = labels_json();
        for key in [
            "menuJumpJava",
            "menuJumpXml",
            "menuFocusMethod",
            "menuFocusRequest",
            "menuDelete",
            "menuBlockMethod",
            "focusMethodPrefix",
            "focusRequestPrefix",
            "focusClear",
            "blockedPrefix",
            "blockedClear",
        ] {
            assert!(labels.get(key).is_some(), "missing label {key}");
        }
    }

    #[test]
    fn the_context_menu_offers_the_row_actions_and_disables_what_the_row_lacks() {
        let (_host, _guard) = test_host();
        let page = include_str!("../../web/log-panel.html");
        for act in [
            "jump-java",
            "jump-xml",
            "focus-method",
            "focus-request",
            "delete-line",
            "block-method",
            "copy-sql",
            "copy-time",
            "export",
        ] {
            assert!(
                page.contains(&format!("data-act=\"{act}\"")),
                "missing menu item {act}"
            );
        }
        // An item is disabled exactly when the row does not state the dimension.
        assert!(page.contains("enableItem('jump-java', !!(row && row.jump_file))"));
        assert!(page.contains("enableItem('jump-xml', !!(row && row.xml_file))"));
        assert!(page.contains("enableItem('focus-method', !!(row && row.focus_key))"));
        assert!(page.contains("enableItem('focus-request', !!(row && row.request_id))"));
        assert!(page.contains("enableItem('block-method', !!(row && row.focus_key))"));
        assert!(page.contains(".ctxmenu button[disabled]"));
        // The host ops the items post.
        for op in ["jump-java", "jump-xml", "delete-line", "block-method"] {
            assert!(page.contains(&format!("op: '{op}'")), "missing op {op}");
        }
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
            "pvParamsList",
            "pvRebindBtn",
            "pvImportBtn",
            "pvRollback",
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
        // Lane L27: the parameter column is editable, rebinds through the host
        // and imports a comma/newline separated list from the clipboard; when
        // the clipboard API is absent the import button is greyed with the
        // reason, and the rollback entry explains why it is disabled.
        assert!(page.contains("pv-param-input"), "editable parameter inputs");
        assert!(page.contains("op: 'preview-params'"), "the rebind op");
        assert!(
            page.contains("navigator.clipboard.readText()"),
            "the clipboard read"
        );
        assert!(page.contains("importBtn.disabled"), "the degrade branch");
        assert!(
            page.contains("split(/[\\r\\n,]+/)"),
            "comma/newline parsing"
        );
        assert!(page.contains("op: 'preview-rollback'"), "the rollback op");
        assert!(page.contains("labels.rollbackNote"), "the rollback note");
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

    // Provenance, focus, delete, mute, jump (lane L28) ---------------------

    /// Two MyBatis statements under a logback prefix, the second one with a
    /// correlation id: the shape Spring Boot prints and the only place a
    /// class/method can be read from.
    const SOURCED_LOG: &str = "\
2026-11-25 21:00:01.001 DEBUG 4242 --- [nio-8080-exec-1] c.e.m.mapper.UserMapper.selectById : ==>  Preparing: SELECT id FROM users WHERE id = ?
2026-11-25 21:00:01.002 DEBUG 4242 --- [nio-8080-exec-1] c.e.m.mapper.UserMapper.selectById : ==> Parameters: 1(Long)
2026-11-25 21:00:02.001 DEBUG 4242 --- [nio-8080-exec-2] traceId=abc123 c.e.m.mapper.OrderMapper.findByUser : ==>  Preparing: SELECT id FROM orders WHERE uid = ?
2026-11-25 21:00:02.002 DEBUG 4242 --- [nio-8080-exec-2] traceId=abc123 c.e.m.mapper.OrderMapper.findByUser : ==> Parameters: 1(Long)
";

    const USER_MAPPER_JAVA: &str = "\
package com.example.myapp.mapper;

public interface UserMapper {
    int selectById(Long id);
}
";

    const USER_MAPPER_XML: &str = r#"<mapper namespace="com.example.myapp.mapper.UserMapper">
  <select id="selectById">select 1</select>
</mapper>"#;

    fn row_of_line<'a>(state: &'a Value, line: usize) -> &'a Value {
        state["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .find(|row| row["line"] == line)
            .expect("the row of that line")
    }

    fn first_line_with(entries: &[LogEntry], needle: &str) -> usize {
        entries
            .iter()
            .find(|entry| entry.sql.contains(needle))
            .expect("a statement")
            .line_no
    }

    #[test]
    fn the_state_rows_carry_the_logger_source_thread_and_request() {
        let (host, _guard) = test_host();
        set_log_text(SOURCED_LOG);
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        let state = state_of(&host);

        let line = first_line_with(&snapshot().entries, "FROM users");
        let row = row_of_line(&state, line);
        assert_eq!(row["class_simple"], "UserMapper");
        assert_eq!(row["method"], "selectById");
        assert_eq!(row["class_name"], "c.e.m.mapper.UserMapper");
        assert_eq!(row["focus_key"], "c.e.m.mapper.UserMapper#selectById");
        assert_eq!(row["thread"], "nio-8080-exec-1");
        assert_eq!(row["request_id"], "", "this row states no id");

        let line = first_line_with(&snapshot().entries, "FROM orders");
        let row = row_of_line(&state, line);
        assert_eq!(row["class_simple"], "OrderMapper");
        assert_eq!(row["request_id"], "abc123");
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
    fn a_focus_chip_hides_rows_that_state_no_source() {
        let (_host, _guard) = test_host();
        let rows = build_rows_with(
            &analysis_of("Preparing: SELECT 1 FROM t\nParameters: 1(Long)\n"),
            None,
            &BTreeSet::new(),
            &BTreeSet::new(),
            true,
            1000.0,
        );
        let filter = Filter {
            focus: Some(Focus {
                kind: FocusKind::Method,
                value: "c.e.m.mapper.UserMapper#selectById".to_string(),
            }),
            ..Filter::default()
        };
        assert!(
            filter_rows(&rows, &filter, 1000.0).is_empty(),
            "a row without a method never matches a method chip"
        );
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
    fn a_five_thousand_row_push_stays_within_the_budget() {
        let (host, _guard) = test_host();
        host.set_setting("logs.pageSize", "5000");
        let mut text = String::new();
        for index in 0..5000 {
            text.push_str(&format!(
                "2026-11-25 21:00:01.001 DEBUG 4242 --- [nio-8080-exec-1] \
                 c.e.m.mapper.UserMapper.selectById : ==>  Preparing: SELECT c{index} FROM t WHERE id = ?\n"
            ));
            text.push_str(&format!(
                "2026-11-25 21:00:01.001 DEBUG 4242 --- [nio-8080-exec-1] \
                 c.e.m.mapper.UserMapper.selectById : ==> Parameters: {index}(Integer)\n"
            ));
        }
        set_log_text(&text);
        // Warm the parse once (a real session parses on the first push), then
        // measure a full push of 5k rows: parse + rows + focus + index queries
        // + JSON. The index is empty here, which is the common case; a filled
        // index only replaces empty hash lookups with small ones.
        let _ = push_state();
        let started = std::time::Instant::now();
        let state = build_state();
        let elapsed = started.elapsed();
        println!("5k-row push: {elapsed:?}");
        assert_eq!(state["rows"].as_array().unwrap().len(), 5000);
        // Measured ~180 ms for 5000 shown rows in a debug build (the shipped
        // wasm is optimised); the bound keeps a 2x margin so a loaded CI machine
        // does not turn a timing check into a flake. The default cap is 50 rows,
        // which makes a real push two orders of magnitude cheaper.
        assert!(
            elapsed < std::time::Duration::from_millis(400),
            "5k-row push took {elapsed:?}"
        );
    }

    // Toolbar actions and interactions (lane L26) -------------------------

    #[test]
    fn capture_start_message_forwards_to_the_capture_command() {
        let (host, _guard) = test_host();
        let _bridge = crate::capture::bridge::test_guard();
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
    fn capture_stop_message_forwards_to_the_capture_command() {
        let (host, _guard) = test_host();
        let _bridge = crate::capture::bridge::test_guard();
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
        handle_panel_message(&host, "{\"op\":\"sort\",\"sort\":\"duration\"}").unwrap();
        let state = state_of(&host);
        assert_eq!(state["sort"], "duration");
        let order: Vec<String> = state["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["sql"].as_str().unwrap().to_string())
            .collect();
        assert!(order[0].contains("FROM b"), "{order:?}");
        assert!(order[1].contains("FROM a"), "{order:?}");
        assert!(order[2].contains("FROM c"), "{order:?}");

        handle_panel_message(&host, "{\"op\":\"sort\",\"sort\":\"time\"}").unwrap();
        let state = state_of(&host);
        assert_eq!(state["sort"], "time");
        let order: Vec<String> = state["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["sql"].as_str().unwrap().to_string())
            .collect();
        assert!(order[0].contains("FROM a"), "{order:?}");
        assert!(order[2].contains("FROM c"), "{order:?}");
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
        handle_panel_message(&host, "{\"op\":\"sort\",\"sort\":\"duration\"}").unwrap();
        let rows = state_of(&host)["rows"].as_array().cloned().unwrap();
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(rows[0]["sql"].as_str().unwrap().contains("timed"));
        assert_eq!(rows[1]["duration_ms"], Value::Null);
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

    #[test]
    fn the_page_draws_the_l26_actions_and_the_interaction_state() {
        let (_host, _guard) = test_host();
        let page = include_str!("../../web/log-panel.html");
        // The two pause concepts stay separate: refresh pause (existing
        // `paused`) and follow pause (`atBottom` / `follow`).
        assert!(page.contains("scrollTop + list.clientHeight >= list.scrollHeight - 4"));
        assert!(page.contains("var follow = true;"));
        assert!(page.contains("post({ op: 'follow', follow: false })"));
        assert!(page.contains("post({ op: 'scroll-end' })"));
        // The sort button toggles time <-> duration and shows the current mode.
        assert!(page.contains("sortMode = sortMode === 'duration' ? 'time' : 'duration';"));
        assert!(page.contains("post({ op: 'sort', sort: sortMode })"));
        assert!(page.contains("L.sortDurationTitle"));
        // The blocked popup lists the keys and clears them.
        assert!(page.contains("id=\"blockedMenu\""));
        assert!(page.contains("id=\"blockedList\""));
        assert!(page.contains("id=\"blockedClear\""));
        assert!(page.contains("post({ op: 'clear-blocked' })"));
        assert!(page.contains("renderBlocked(state)"));
        // The group badges are clickable and post toggle-group with the kind.
        assert!(page.contains("badgeButton(dedupeText, 'dedupe', dedupeTitle"));
        assert!(page.contains("badgeButton(groupText, 'group', groupTitle"));
        assert!(page.contains("kind: 'dedupe'"));
        assert!(page.contains("kind: 'group'"));
        assert!(page.contains("content.classList.add('member')"));
        // The capture button is one icon button whose glyph follows the state.
        assert!(page.contains("post({ op: connected ? 'capture-stop' : 'capture-start' })"));
        assert!(page.contains("$('captureRunIcon').hidden = connected"));
        // Settings and help have real feedback paths, not dead buttons.
        assert!(page.contains("post({ op: 'settings' })"));
        assert!(page.contains("post({ op: 'help' })"));
        // The status line names the follow state.
        assert!(page.contains("right = L.followOff || ''"));
        // The scroll-to-bottom tooltip names the follow state too.
        assert!(page.contains("$('scrollEndBtn').title = follow ? (L.followOn"));
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
