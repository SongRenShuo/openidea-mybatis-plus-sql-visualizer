//! Panel state: the filter/sort model and the process-wide store (lane W2-0).
//!
//! Split out of `views::log_panel` with no behaviour change. Everything here is
//! what the panel's rows, tabs and ops share: the filter/sort model, the
//! per-tab payloads the store carries, and the lazily parsed buffer. Items the
//! sibling modules need are `pub(super)`; the panel's own API is re-exported
//! from `mod.rs`, so `log_panel::Filter` and friends resolve exactly as before.

use std::collections::BTreeSet;
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::core::log::{LogAnalysis, LogSources, ParseOptions, parse_with_sources};
use crate::settings;

use super::history::HistoryTab;
use super::rows::{PanelRow, SqlType};
use super::suppress;
use super::tabs::{LogToSqlTab, SendToTab, SqlToJavaTab};

/// `logs.pageSize` default and clamp when used as the panel buffer cap.
const DEFAULT_BUFFER_CAP: i64 = 50;
const MAX_BUFFER_CAP: i64 = 100_000;

/// `logs.displayMode` (lane L34): how the list shows a statement.
/// `keyword_wrap` is the default and the reference's three-line cell.
pub(super) const DISPLAY_MODE_SETTING: &str = "logs.displayMode";
/// `log.showDuration` (lane L34): the row header's duration segment.
pub(super) const SHOW_DURATION_SETTING: &str = "log.showDuration";
/// `logs.crud.style` (lane L2): how the CRUD type is marked in a list row —
/// a 3px left bar (default), a whole-row tint, or nothing.
pub(super) const CRUD_STYLE_SETTING: &str = "logs.crud.style";

/// Normalises `logs.crud.style` to its three tokens; an unknown stored value
/// falls back to `bar` instead of leaving the list unmarked.
///
/// The key is declared by the settings lane (`logs.crud.*`); until it lands the
/// key is absent and the default applies, so this lane never waits on it.
pub fn crud_style(value: &str) -> &'static str {
    match value {
        "background" => "background",
        "off" => "off",
        _ => "bar",
    }
}

/// Normalises `logs.displayMode` to its three tokens; an unknown stored value
/// falls back to `keyword_wrap` instead of guessing a fold.
///
/// The tokens are the reference's `SqlListDisplayMode` values: `single` (one
/// summary line, 160 characters at most), `keyword_wrap` (one major clause per
/// line, three lines at most) and `formatted` (the dialect formatter's full
/// output, no cap). `multi` is the token this schema shipped before the lane
/// renamed it; it maps onto `formatted` because its old meaning was "no fold,
/// the whole statement" — never onto a different fold.
///
/// The display text itself is prepared in `rows::list_display_sql`, so the page
/// renders exactly what the host resolved and never re-reads the setting.
pub fn display_mode(value: &str) -> &'static str {
    match value {
        "single" => "single",
        "formatted" | "multi" => "formatted",
        _ => "keyword_wrap",
    }
}
/// The "SQL 类型" selection: the button shows 全部 / 无 / N 项, so "all six" and
/// "none" must stay distinguishable (an empty `Only` set is never produced by
/// the page; empty defaults to `All`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum TypeFilter {
    #[default]
    All,
    None,
    Only(BTreeSet<SqlType>),
}

impl TypeFilter {
    pub(super) fn matches(&self, kind: SqlType) -> bool {
        match self {
            TypeFilter::All => true,
            TypeFilter::None => false,
            TypeFilter::Only(set) => set.contains(&kind),
        }
    }

    /// Selected tokens, for the push state (`Only` only).
    pub(super) fn selected_tokens(&self) -> Vec<&'static str> {
        match self {
            TypeFilter::Only(set) => set.iter().map(|kind| kind.token()).collect(),
            _ => Vec::new(),
        }
    }

    pub(super) fn mode(&self) -> &'static str {
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
    /// The statement's structure key (lane L32, "按模板聚焦"): every row that
    /// shares the same shape, literals aside — the same key the `×N` badge
    /// counts, so the menu entry and the badge describe one concept.
    Template,
}

impl FocusKind {
    pub fn token(self) -> &'static str {
        match self {
            FocusKind::Method => "method",
            FocusKind::Request => "request",
            FocusKind::Template => "template",
        }
    }

    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "method" => Some(FocusKind::Method),
            "request" => Some(FocusKind::Request),
            "template" => Some(FocusKind::Template),
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
            FocusKind::Template => !self.value.is_empty() && row.n_plus_one_key == self.value,
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
/// The display order of the pushed rows (lane A), mirroring the reference's
/// `cn.song.mybatisplus.visualizer.log.SortMode`.
///
/// Seven modes in the reference's `sortOptions()` order — the order its
/// sort-down menu lists them. `TimeAsc` is the default: capture order, which is
/// the order the statements appear in the log. Rows without a duration sink to
/// the bottom in **both** duration directions, and the two grouping modes keep
/// capture order inside a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMode {
    TimeAsc,
    TimeDesc,
    DurationAsc,
    DurationDesc,
    Sequence,
    Structure,
    Database,
}

impl SortMode {
    /// Every mode, in the reference's menu order (`sortOptions()`).
    pub const ALL: [SortMode; 7] = [
        SortMode::TimeAsc,
        SortMode::TimeDesc,
        SortMode::DurationAsc,
        SortMode::DurationDesc,
        SortMode::Sequence,
        SortMode::Structure,
        SortMode::Database,
    ];

    pub fn token(self) -> &'static str {
        match self {
            SortMode::TimeAsc => "time_asc",
            SortMode::TimeDesc => "time_desc",
            SortMode::DurationAsc => "duration_asc",
            SortMode::DurationDesc => "duration_desc",
            SortMode::Sequence => "sequence",
            SortMode::Structure => "structure",
            SortMode::Database => "database",
        }
    }

    /// Parses a token from the page.
    ///
    /// `time` / `duration` are the pre-lane tokens: the old button was a
    /// two-way toggle whose duration direction sorted longest first, so they
    /// map onto the ascending-time / descending-duration modes.
    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "time_asc" | "time" => Some(SortMode::TimeAsc),
            "time_desc" => Some(SortMode::TimeDesc),
            "duration_asc" => Some(SortMode::DurationAsc),
            "duration_desc" | "duration" => Some(SortMode::DurationDesc),
            "sequence" => Some(SortMode::Sequence),
            "structure" => Some(SortMode::Structure),
            "database" => Some(SortMode::Database),
            _ => None,
        }
    }
}
// ---------------------------------------------------------------------------
// Store and push API
// ---------------------------------------------------------------------------

pub(super) struct Store {
    pub(super) text: String,
    pub(super) analysis: LogAnalysis,
    /// Per-entry provenance, joined by `line_no` (lane L25's pass).
    pub(super) sources: LogSources,
    /// Lines the user removed from the buffer (`删除`).
    pub(super) removed: BTreeSet<usize>,
    /// `class_name#method` keys the user blocked for this session (`屏蔽此方法`).
    pub(super) blocked: BTreeSet<String>,
    /// The groups the user expanded (`×N` / `⨉N` badges, lane L26), keyed
    /// `d:<dedupe key>` / `n:<n+1 template>`.
    pub(super) expanded: BTreeSet<String>,
    pub(super) dirty: bool,
    pub(super) filter: Filter,
    /// Once the page sends a filter, its `dedupe` choice wins over the setting.
    pub(super) filter_touched: bool,
    pub(super) paused: bool,
    /// Whether the list auto-scrolls to the newest row (lane L26). The page
    /// reports a flip when the user scrolls away from the bottom; the
    /// "scroll to bottom" button and scrolling back restore it.
    pub(super) follow: bool,
    /// The display order (lane A: seven modes, default `time_asc`).
    pub(super) sort: SortMode,
    /// The "Log → SQL" tab (lane L33): its own input pane and the last
    /// conversion, carried in `tabs.logToSql`. Independent of the log list's
    /// selection — the tab is a standalone paste-and-convert panel.
    pub(super) log_to_sql: LogToSqlTab,
    /// Last "SQL → Java" conversion, carried in `tabs.sqlToJava`.
    pub(super) sql_to_java: SqlToJavaTab,
    /// Last history search over the buffered log text, carried in
    /// `tabs.history`.
    pub(super) history: HistoryTab,
    /// A "发送到 SQL→Java" / "发送到 日志→SQL" hand-off (lane L32), carried in
    /// `sendToTab` until the page reports the tab it now shows.
    pub(super) send_to_tab: Option<SendToTab>,
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
            sort: SortMode::TimeAsc,
            log_to_sql: LogToSqlTab::default(),
            sql_to_java: SqlToJavaTab::default(),
            history: HistoryTab::default(),
            send_to_tab: None,
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

pub(super) fn store() -> MutexGuard<'static, Store> {
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
    // A replaced buffer restarts its line numbers: the suppression cursor that
    // belongs to the old text means nothing here (lane E).
    suppress::reset_cursor();
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
    // The two input tabs are standalone panes (lane L33): their text and
    // results do not come from the buffer, so clearing the log leaves them
    // alone. The history tab searched the buffer, so it goes.
    state.history = HistoryTab::default();
    drop(state);
    // The buffer is empty, so no row is left for a suppression rule to count
    // (lane E); the rules themselves are not this panel's to clear.
    suppress::reset_cursor();
}

/// Puts the process-wide panel store back to a fresh baseline. Tests in several
/// modules share it (they take `views::test_state_lock`), so every one of them
/// must start from the same state — otherwise a `rows.len()` assertion depends
/// on which test happened to hold the lock last.
#[cfg(test)]
pub(crate) fn reset_for_tests() {
    clear();
    // The two input tabs survive `clear()` by design, so a test resets them
    // explicitly rather than relying on the previous test having done so.
    let mut state = store();
    state.log_to_sql = LogToSqlTab::default();
    state.sql_to_java = SqlToJavaTab::default();
    // The display order is process-wide too (lane A): a test that leaves
    // `structure` or `database` behind would make the next test's "the default
    // is time_asc" assertion depend on which test ran before it.
    state.sort = SortMode::TimeAsc;
    drop(state);
    // Suppression rules and their line cursor are process-wide as well (lane E).
    suppress::reset_for_tests();
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

pub(super) fn ensure_parsed(state: &mut Store) {
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
    // Lane E: count the rows that arrived while a suppression rule was active.
    // The cursor makes this idempotent — a push that changes nothing counts
    // nothing, and rows that predate a rule stay uncounted.
    suppress::note_new_entries(&state.analysis.entries);
}

pub(super) fn parse_options() -> ParseOptions {
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

pub(super) fn slow_threshold() -> f64 {
    settings::get_i64("log.slowThresholdMs", 1000).max(0) as f64
}

pub(super) fn buffer_cap() -> usize {
    settings::get_i64("logs.pageSize", DEFAULT_BUFFER_CAP).clamp(1, MAX_BUFFER_CAP) as usize
}

/// The dedupe choice the display uses: the page's filter wins once it sent
/// one, the setting decides before that.
pub(super) fn dedupe_active(state: &Store) -> bool {
    if state.filter_touched {
        state.filter.dedupe
    } else {
        settings::get_bool("log.dedupe", true)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::log_panel::test_support::test_host;

    #[test]
    fn display_mode_normalises_the_setting() {
        // A stored value outside the enum falls back to the default instead of
        // guessing a fold; the three tokens are what the list renderer branches
        // on. `multi` is the pre-lane token for "no fold" and stays an alias of
        // `formatted`, never of a different shape.
        assert_eq!(display_mode("single"), "single");
        assert_eq!(display_mode("keyword_wrap"), "keyword_wrap");
        assert_eq!(display_mode("formatted"), "formatted");
        assert_eq!(display_mode("multi"), "formatted");
        assert_eq!(display_mode("weird"), "keyword_wrap");
    }

    #[test]
    fn crud_style_normalises_the_setting() {
        // The settings lane owns the key; this lane only consumes it and never
        // turns a typo into "no marking at all".
        assert_eq!(crud_style("bar"), "bar");
        assert_eq!(crud_style("background"), "background");
        assert_eq!(crud_style("off"), "off");
        assert_eq!(crud_style("weird"), "bar");
        assert_eq!(crud_style(""), "bar");
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
