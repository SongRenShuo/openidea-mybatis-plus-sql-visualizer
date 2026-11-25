//! SQL log panel (T2-1).
//!
//! Data is pushed by the capture lane and by commands: [`set_log_text`] and
//! [`append_text`] fill a module-level buffer, and the panel parses it lazily on
//! the next render. Parsing stays out of the view loop when nothing changed, and
//! the renderer only serialises the rows of the requested [`Viewport`] page.
//!
//! The store lives in a `OnceLock<Mutex<_>>` because `Main` belongs to the
//! controller (wasm is single-threaded; the mutex only serialises test threads).

use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex, MutexGuard, OnceLock};

use openidea_plugin::host;
use openidea_plugin::views::{Body, Button, View, ViewEvent, Viewport, Views};
use openidea_plugin::{Error, Invocation};

use crate::core::log::{
    DuplicateGroup, EntryClass, LogAnalysis, LogEntry, NPlusOneGroup, ParseOptions, parse,
};
use crate::i18n::t;
use crate::settings;

/// The id declared in `[[contributes.views]]`.
pub const VIEW_ID: &str = "sqlv-logs";

/// Default refresh interval for pushed rows.
pub const DEFAULT_REFRESH_MS: u64 = 250;
/// Rows serialised when the host asks for no particular page.
const DEFAULT_PAGE_ROWS: u32 = 200;
const SQL_DISPLAY_CHARS: usize = 180;

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

/// Panel category of a row: the display axis the user filters by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RowCategory {
    Normal,
    Duplicate,
    NPlusOne,
    Slow,
    Error,
}

impl RowCategory {
    pub const ALL: [RowCategory; 5] = [
        RowCategory::Normal,
        RowCategory::Duplicate,
        RowCategory::NPlusOne,
        RowCategory::Slow,
        RowCategory::Error,
    ];

    /// Stable token used in filters and CSS classes (`n_plus_one`).
    pub fn token(self) -> &'static str {
        match self {
            RowCategory::Normal => "normal",
            RowCategory::Duplicate => "duplicate",
            RowCategory::NPlusOne => "n_plus_one",
            RowCategory::Slow => "slow",
            RowCategory::Error => "error",
        }
    }

    /// Localised label shown in the table.
    pub fn label(self) -> String {
        t(&format!("view.logs.category.{}", self.token()))
    }

    /// Parses a filter token; unknown tokens mean "all categories".
    pub fn parse(token: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|category| category.token() == token)
    }
}

/// One rendered row: either a single entry or a folded duplicate group.
#[derive(Debug, Clone, PartialEq)]
pub struct PanelRow {
    pub line_no: usize,
    pub timestamp_ms: Option<i64>,
    pub duration_ms: Option<f64>,
    /// Restored SQL; empty for error rows.
    pub sql: String,
    pub params: Vec<String>,
    pub category: RowCategory,
    /// Size of the duplicate (or N+1) group this row stands for.
    pub count: usize,
    pub group_key: Option<String>,
    pub occurrences: Vec<usize>,
    /// Original text for rows without SQL (errors, partial entries).
    pub raw_line: Option<String>,
    pub n_plus_one: bool,
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

    pub fn params_text(&self) -> String {
        self.params.join(", ")
    }

    /// SQL (or the raw line when no SQL was recovered), truncated for display.
    pub fn display_sql(&self) -> String {
        let text = if self.sql.is_empty() {
            self.raw_line.as_deref().unwrap_or_default()
        } else {
            self.sql.as_str()
        };
        truncate_chars(text, SQL_DISPLAY_CHARS)
    }

    fn searchable(&self) -> String {
        let mut text = self.sql.clone();
        if let Some(raw) = &self.raw_line {
            text.push('\n');
            text.push_str(raw);
        }
        text.push('\n');
        text.push_str(&self.params.join(", "));
        text
    }
}

/// Category/keyword filter applied before pagination.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    pub category: Option<RowCategory>,
    pub keyword: String,
}

impl Filter {
    fn matches(&self, row: &PanelRow) -> bool {
        if let Some(category) = self.category {
            if row.category != category {
                return false;
            }
        }
        let keyword = self.keyword.trim().to_lowercase();
        keyword.is_empty() || row.searchable().to_lowercase().contains(&keyword)
    }
}

/// Rows of one page plus the total row count after filtering.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Page {
    pub first: u32,
    pub total: u32,
    pub rows: Vec<PanelRow>,
}

// ---------------------------------------------------------------------------
// Row assembly
// ---------------------------------------------------------------------------

/// Collapses duplicate groups (unless expanded), tags N+1 rows and classifies
/// each row. Rows keep `line_no` order.
pub fn build_rows(
    analysis: &LogAnalysis,
    dedupe: bool,
    expanded: &BTreeSet<String>,
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
        let duplicate = duplicate_by_line.get(&entry.line_no).copied();
        let n_plus_one = n_plus_one_by_line.get(&entry.line_no).copied();
        let is_expanded = duplicate
            .map(|group| expanded.contains(&group.key))
            .unwrap_or(false);

        if dedupe && duplicate.is_some() && !is_expanded {
            let group = duplicate.expect("checked above");
            if !folded_groups.insert(group.key.clone()) {
                continue;
            }
            let count = group.count.max(n_plus_one.map(|n1| n1.count).unwrap_or(1));
            rows.push(row_of(
                entry,
                categorize(entry, n_plus_one, slow_threshold_ms),
                count,
                Some(group.key.clone()),
                group.line_nos.clone(),
                n_plus_one.is_some(),
            ));
            continue;
        }

        let occurrences = match (duplicate, is_expanded) {
            (Some(group), true) => group.line_nos.clone(),
            _ => vec![entry.line_no],
        };
        let count = duplicate
            .map(|group| group.count)
            .or_else(|| n_plus_one.map(|group| group.count))
            .unwrap_or(1);
        rows.push(row_of(
            entry,
            categorize(entry, n_plus_one, slow_threshold_ms),
            count,
            duplicate.map(|group| group.key.clone()),
            occurrences,
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

fn row_of(
    entry: &LogEntry,
    category: RowCategory,
    count: usize,
    group_key: Option<String>,
    occurrences: Vec<usize>,
    n_plus_one: bool,
) -> PanelRow {
    PanelRow {
        line_no: entry.line_no,
        timestamp_ms: entry.timestamp_ms,
        duration_ms: entry.duration_ms,
        sql: entry.sql.clone(),
        params: entry.params.clone(),
        category,
        count,
        group_key,
        occurrences,
        raw_line: entry.raw_line.clone(),
        n_plus_one,
    }
}

/// Applies the filter and returns only the requested page's rows.
pub fn paginate(rows: &[PanelRow], filter: &Filter, viewport: Option<Viewport>) -> Page {
    let filtered: Vec<PanelRow> = rows
        .iter()
        .filter(|row| filter.matches(row))
        .cloned()
        .collect();
    let total = filtered.len() as u32;
    let range = Viewport::range(viewport, total, DEFAULT_PAGE_ROWS);
    Page {
        first: range.start,
        total,
        rows: filtered[range.start as usize..range.end as usize].to_vec(),
    }
}

// ---------------------------------------------------------------------------
// Refresh throttle
// ---------------------------------------------------------------------------

/// Decides when a burst of pushed rows becomes one `refresh_view` call.
///
/// `request` grants the first refresh immediately, then at most one per
/// interval. While paused it records a pending refresh and grants nothing;
/// [`Throttle::resume`] clears the timestamp so the next request goes out at
/// once (the panel also turns auto-scroll back on then).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Throttle {
    interval_ms: u64,
    last_ms: Option<u64>,
    paused: bool,
    pending: bool,
}

impl Throttle {
    pub fn new(interval_ms: u64) -> Self {
        Self {
            interval_ms,
            last_ms: None,
            paused: false,
            pending: false,
        }
    }

    pub fn interval_ms(&self) -> u64 {
        self.interval_ms
    }

    pub fn set_interval_ms(&mut self, interval_ms: u64) {
        self.interval_ms = interval_ms;
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn has_pending(&self) -> bool {
        self.pending
    }

    pub fn pause(&mut self) {
        self.paused = true;
    }

    pub fn resume(&mut self) {
        self.paused = false;
        self.last_ms = None;
    }

    /// True when the caller should refresh now.
    pub fn request(&mut self, now_ms: u64) -> bool {
        if self.paused {
            self.pending = true;
            return false;
        }
        match self.last_ms {
            Some(last) if now_ms.saturating_sub(last) < self.interval_ms => {
                self.pending = true;
                false
            }
            _ => {
                self.last_ms = Some(now_ms);
                self.pending = false;
                true
            }
        }
    }

    /// True when a suppressed refresh has become due.
    pub fn tick(&mut self, now_ms: u64) -> bool {
        if self.paused || !self.pending {
            return false;
        }
        self.request(now_ms)
    }
}

// ---------------------------------------------------------------------------
// Store and push API
// ---------------------------------------------------------------------------

struct Store {
    text: String,
    analysis: LogAnalysis,
    dirty: bool,
    filter: Filter,
    expanded: BTreeSet<String>,
    throttle: Throttle,
    auto_scroll: bool,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            text: String::new(),
            analysis: empty_analysis(),
            dirty: false,
            filter: Filter::default(),
            expanded: BTreeSet::new(),
            throttle: Throttle::new(DEFAULT_REFRESH_MS),
            auto_scroll: true,
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
    state.expanded.clear();
}

/// Whether refresh is paused (pushed rows keep accumulating meanwhile).
pub fn is_paused() -> bool {
    store().throttle.is_paused()
}

/// Sets the paused flag; resuming also re-enables auto-scroll.
pub fn set_paused(paused: bool) {
    let mut state = store();
    state.auto_scroll = !paused;
    if paused {
        state.throttle.pause();
    } else {
        state.throttle.resume();
    }
}

/// Toggles pause and returns the new paused state.
pub fn toggle_pause() -> bool {
    let paused = !store().throttle.is_paused();
    set_paused(paused);
    paused
}

/// Replaces the category/keyword filter.
pub fn set_filter(category: Option<RowCategory>, keyword: &str) {
    let mut state = store();
    state.filter = Filter {
        category,
        keyword: keyword.to_string(),
    };
}

/// Expands or folds one duplicate group.
pub fn set_expanded(group_key: &str, expanded: bool) {
    let mut state = store();
    if expanded {
        state.expanded.insert(group_key.to_string());
    } else {
        state.expanded.remove(group_key);
    }
}

/// Whether a duplicate group is currently expanded.
pub fn is_expanded(group_key: &str) -> bool {
    store().expanded.contains(group_key)
}

/// The parsed analysis of the buffered text (parses on demand).
pub fn snapshot() -> LogAnalysis {
    let mut state = store();
    ensure_parsed(&mut state);
    state.analysis.clone()
}

/// True when the caller should call `refresh_view(VIEW_ID)` at `now_ms`.
/// The first burst goes out immediately; later ones are throttled.
pub fn take_refresh(now_ms: u64) -> bool {
    store().throttle.request(now_ms)
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
    let interval =
        settings::get_i64("log.refreshIntervalMs", DEFAULT_REFRESH_MS as i64).max(0) as u64;
    state.throttle.set_interval_ms(interval);
}

fn parse_options() -> ParseOptions {
    let mut options = ParseOptions::default();
    options.slow_threshold_ms = settings::get_i64("log.slowThresholdMs", 1000).max(0) as f64;
    options.n_plus_one_threshold = settings::get_i64("log.nPlusOneThreshold", 5).max(2) as usize;
    options.ignore_params_in_dedupe = settings::get_bool("log.ignoreParamsInDedupe", false);
    let window = settings::get_i64("log.dedupeWindowMs", 0);
    options.dedupe_window_ms = (window > 0).then_some(window);
    options.max_sql_len = settings::get_i64("log.maxSqlLength", 20_000).max(256) as usize;
    options.timestamp_offset_minutes = settings::get_i64("log.timestampOffsetMinutes", 0) as i32;
    options
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Builds the panel view for `viewport` (public for commands and tests).
pub fn render_panel(viewport: Option<Viewport>) -> View {
    let mut state = store();
    ensure_parsed(&mut state);

    let dedupe = settings::get_bool("log.dedupe", true);
    let slow_threshold = settings::get_i64("log.slowThresholdMs", 1000).max(0) as f64;
    let rows = build_rows(&state.analysis, dedupe, &state.expanded, slow_threshold);
    let page = paginate(&rows, &state.filter, viewport);

    let html = build_html(
        &page,
        &state.filter,
        &state.expanded,
        &state.throttle,
        state.auto_scroll,
    );
    let paused = state.throttle.is_paused();
    let pause_label = if paused {
        t("view.logs.button.resume")
    } else {
        t("view.logs.button.pause")
    };

    let mut view = View::new(Body::Html(html));
    view.toolbar
        .push(Button::new("toggle-pause", &pause_label).icon(if paused { "run" } else { "stop" }));
    view.toolbar
        .push(Button::new("clear", &t("view.logs.button.clear")).icon("remove"));
    view.toolbar
        .push(Button::new("copy-all", &t("view.logs.button.copy")));
    let shown_end = page.first + page.rows.len() as u32;
    let first_shown = if page.rows.is_empty() {
        0
    } else {
        page.first + 1
    };
    let mut status = format!("{first_shown}-{shown_end} / {}", page.total);
    if paused {
        status.push_str(" · ");
        status.push_str(&t("view.logs.paused"));
    }
    view.status = Some(status);
    view
}

fn build_html(
    page: &Page,
    filter: &Filter,
    expanded: &BTreeSet<String>,
    throttle: &Throttle,
    auto_scroll: bool,
) -> String {
    let mut html = String::with_capacity(4096);
    html.push_str(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><style>\
body{font:var(--openidea-font-size,13px)/1.4 var(--openidea-font-ui,sans-serif);\
color:var(--openidea-fg);margin:0;padding:6px}\
.bar{display:flex;gap:6px;align-items:center;margin-bottom:6px}\
select,input{background:var(--openidea-bg-input);color:var(--openidea-fg);\
border:1px solid var(--openidea-border);border-radius:3px;padding:2px 4px}\
table{width:100%;border-collapse:collapse}\
th{text-align:left;font-weight:600;border-bottom:1px solid var(--openidea-border);\
padding:3px 6px;position:sticky;top:0;background:var(--openidea-bg)}\
td{padding:3px 6px;border-bottom:1px solid var(--openidea-border);vertical-align:top}\
td.sql{font-family:var(--openidea-font-mono,monospace);white-space:pre-wrap}\
td.t,td.ms{white-space:nowrap;font-family:var(--openidea-font-mono,monospace)}\
tr.row-error td{background:color-mix(in srgb,var(--openidea-danger) 14%,transparent)}\
tr.row-error td.cat{color:var(--openidea-danger);font-weight:600}\
tr.row-slow td.cat{color:var(--openidea-warning)}\
tr.row-n_plus_one td.cat{color:var(--openidea-accent);font-weight:600}\
.badge{display:inline-block;margin-right:6px;padding:0 4px;border-radius:3px;\
background:var(--openidea-bg-selected);color:var(--openidea-fg-strong);font-size:11px}\
button.expand{background:none;border:1px solid var(--openidea-border);border-radius:3px;\
color:var(--openidea-link);cursor:pointer;font-size:11px;margin-right:6px}\
.empty{padding:12px;color:var(--openidea-fg-muted)}</style></head><body ",
    );
    html.push_str(&format!("data-autoscroll=\"{auto_scroll}\">"));

    html.push_str("<div class=\"bar\"><select id=\"log-category\">");
    let all_selected = filter.category.is_none();
    html.push_str(&format!(
        "<option value=\"all\"{}>{}</option>",
        if all_selected { " selected" } else { "" },
        escape_html(&t("view.logs.filter.all"))
    ));
    for category in RowCategory::ALL {
        let selected = filter.category == Some(category);
        html.push_str(&format!(
            "<option value=\"{}\"{}>{}</option>",
            category.token(),
            if selected { " selected" } else { "" },
            escape_html(&category.label())
        ));
    }
    html.push_str("</select>");
    html.push_str(&format!(
        "<input id=\"log-keyword\" value=\"{}\" placeholder=\"{}\">",
        escape_attr(&filter.keyword),
        escape_attr(&t("view.logs.filter.keyword"))
    ));
    if throttle.is_paused() {
        html.push_str(&format!(
            "<span class=\"badge\">{}</span>",
            escape_html(&t("view.logs.paused"))
        ));
    }
    html.push_str("</div>");

    html.push_str("<table><thead><tr>");
    for key in [
        "view.logs.column.time",
        "view.logs.column.duration",
        "view.logs.column.sql",
        "view.logs.column.params",
        "view.logs.column.category",
    ] {
        html.push_str(&format!("<th>{}</th>", escape_html(&t(key))));
    }
    html.push_str("</tr></thead><tbody>");

    if page.rows.is_empty() {
        html.push_str(&format!(
            "<tr><td colspan=\"5\"><div class=\"empty\">{}</div></td></tr>",
            escape_html(&t("view.logs.empty"))
        ));
    }
    for row in &page.rows {
        html.push_str(&row_html(row, expanded));
    }
    html.push_str("</tbody></table>");

    html.push_str(
        "<script>(function(){\
function post(message){if(window.openidea&&window.openidea.postMessage){\
window.openidea.postMessage(JSON.stringify(message));}}\
var cat=document.getElementById('log-category');\
var kw=document.getElementById('log-keyword');\
function apply(){post({op:'set-filter',category:cat.value,keyword:kw.value});}\
if(cat){cat.addEventListener('change',apply);}\
if(kw){kw.addEventListener('change',apply);}\
var buttons=document.querySelectorAll('button.expand');\
for(var i=0;i<buttons.length;i++){(function(b){\
b.addEventListener('click',function(){post({op:'toggle-group',key:b.getAttribute('data-key')});});\
})(buttons[i]);}\
if(document.body.getAttribute('data-autoscroll')==='true'){\
var rows=document.querySelectorAll('tbody tr');\
if(rows.length){rows[rows.length-1].scrollIntoView({block:'end'});}\
}}})();</script></body></html>",
    );
    html
}

fn row_html(row: &PanelRow, expanded: &BTreeSet<String>) -> String {
    let is_expanded = row
        .group_key
        .as_ref()
        .map(|key| expanded.contains(key))
        .unwrap_or(false);
    let mut html = format!(
        "<tr class=\"row row-{}\" data-line=\"{}\">",
        row.category.token(),
        row.line_no
    );
    html.push_str(&format!(
        "<td class=\"t\">{}</td>",
        escape_html(&row.time_text())
    ));
    html.push_str(&format!(
        "<td class=\"ms\">{}</td>",
        escape_html(&row.duration_text())
    ));

    html.push_str("<td class=\"sql\">");
    if row.n_plus_one && row.count > 1 {
        html.push_str(&format!(
            "<span class=\"badge\">{} ×{}</span>",
            escape_html(&t("view.logs.badge.n_plus_one")),
            row.count
        ));
    } else if row.group_key.is_some() && row.count > 1 {
        html.push_str(&format!(
            "<span class=\"badge\">{} ×{}</span>",
            escape_html(&t("view.logs.category.duplicate")),
            row.count
        ));
    }
    if let (Some(key), true) = (&row.group_key, row.occurrences.len() > 1) {
        let label = if is_expanded {
            t("view.logs.collapse")
        } else {
            format!("{} ×{}", t("view.logs.expand"), row.count)
        };
        html.push_str(&format!(
            "<button class=\"expand\" data-key=\"{}\" data-expanded=\"{}\">{}</button>",
            escape_attr(key),
            is_expanded,
            escape_html(&label)
        ));
    }
    html.push_str(&escape_html(&row.display_sql()));
    html.push_str("</td>");

    html.push_str(&format!(
        "<td class=\"params\">{}</td>",
        escape_html(&row.params_text())
    ));
    html.push_str(&format!(
        "<td class=\"cat\">{}</td></tr>",
        escape_html(&row.category.label())
    ));
    html
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// Handles toolbar buttons and messages posted by the panel's page.
pub fn handle_panel_event(event: ViewEvent) -> Result<(), Error> {
    match event {
        ViewEvent::Button(id) if id == "toggle-pause" => {
            toggle_pause();
            host::refresh_view(VIEW_ID);
        }
        ViewEvent::Button(id) if id == "clear" => {
            clear();
            host::refresh_view(VIEW_ID);
        }
        ViewEvent::Button(id) if id == "copy-all" => {
            let text = all_sql();
            host::copy_to_clipboard(&text)?;
        }
        ViewEvent::Message(text) => handle_message(&text)?,
        _ => {}
    }
    Ok(())
}

fn handle_message(text: &str) -> Result<(), Error> {
    let message: serde_json::Value =
        serde_json::from_str(text).map_err(|error| Error::Invalid(error.to_string()))?;
    match message
        .get("op")
        .and_then(|value| value.as_str())
        .unwrap_or("")
    {
        "toggle-group" => {
            if let Some(key) = message.get("key").and_then(|value| value.as_str()) {
                let expanded = !is_expanded(key);
                set_expanded(key, expanded);
                host::refresh_view(VIEW_ID);
            }
        }
        "set-filter" => {
            let category = message
                .get("category")
                .and_then(|value| value.as_str())
                .and_then(RowCategory::parse);
            let keyword = message
                .get("keyword")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            set_filter(category, keyword);
            host::refresh_view(VIEW_ID);
        }
        "toggle-pause" => {
            toggle_pause();
            host::refresh_view(VIEW_ID);
        }
        "copy-line" => {
            if let Some(line) = message.get("line").and_then(|value| value.as_u64()) {
                let sql = sql_of_line(line as usize).unwrap_or_default();
                host::copy_to_clipboard(&sql)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn all_sql() -> String {
    let analysis = snapshot();
    analysis
        .entries
        .iter()
        .filter(|entry| entry.has_sql())
        .map(|entry| entry.sql.as_str())
        .collect::<Vec<_>>()
        .join(";\n")
}

fn sql_of_line(line_no: usize) -> Option<String> {
    snapshot()
        .entries
        .into_iter()
        .find(|entry| entry.line_no == line_no)
        .map(|entry| entry.sql)
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

/// `HH:MM:SS.mmm` UTC for an epoch timestamp. Logs rarely carry a zone; the
/// panel shows the raw wall clock of the row rather than guessing one.
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
        format!("{:.0}", ms)
    } else {
        format!("{:.1}", ms)
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

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn escape_attr(text: &str) -> String {
    escape_html(text)
}

// ---------------------------------------------------------------------------
// View trait
// ---------------------------------------------------------------------------

impl Views for crate::Main {
    fn render(
        &mut self,
        view: &str,
        viewport: Option<Viewport>,
        _ctx: &Invocation,
    ) -> Result<View, Error> {
        match view {
            VIEW_ID => Ok(render_panel(viewport)),
            other => Err(Error::NotFound(format!("view {other}"))),
        }
    }

    fn handle(&mut self, view: &str, event: ViewEvent, _ctx: &Invocation) -> Result<(), Error> {
        match view {
            VIEW_ID => handle_panel_event(event),
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

    /// The store is shared by the whole process; tests run in parallel threads.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn test_host() -> (MockHost, MutexGuard<'static, ()>) {
        let guard = TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let host = MockHost::install();
        clear();
        set_filter(None, "");
        set_paused(false);
        (host, guard)
    }

    fn html_of(view: &View) -> String {
        match &view.body {
            Body::Html(html) => html.clone(),
            other => panic!("expected an html body, got {other:?}"),
        }
    }

    fn big_log(count: usize) -> String {
        let mut text = String::new();
        for index in 0..count {
            text.push_str(&format!(
                "==>  Preparing: SELECT f{index:04}(id) FROM t WHERE id = ?\n"
            ));
            text.push_str(&format!("==> Parameters: {index}(Integer)\n"));
        }
        text
    }

    fn row_count(html: impl AsRef<str>) -> usize {
        html.as_ref().matches("<tr class=\"row").count()
    }

    // Throttle -------------------------------------------------------------

    #[test]
    fn throttle_grants_the_first_refresh_then_one_per_interval() {
        let mut throttle = Throttle::new(250);
        assert!(throttle.request(1_000), "first burst goes out immediately");
        assert!(
            !throttle.request(1_100),
            "within the interval it is suppressed"
        );
        assert!(throttle.has_pending());
        assert!(throttle.request(1_250), "at the interval it goes out");
        assert!(!throttle.has_pending());
    }

    #[test]
    fn throttle_pause_defers_and_resume_releases_immediately() {
        let mut throttle = Throttle::new(250);
        throttle.pause();
        assert!(!throttle.request(2_000));
        assert!(throttle.has_pending());
        assert!(!throttle.tick(2_500), "paused tick never refreshes");
        throttle.resume();
        assert!(throttle.tick(2_501), "resume releases the pending refresh");
        assert!(!throttle.tick(2_502), "then the interval applies again");
    }

    #[test]
    fn throttle_tick_waits_out_the_interval() {
        let mut throttle = Throttle::new(250);
        assert!(throttle.request(0));
        assert!(!throttle.request(100));
        assert!(!throttle.tick(200));
        assert!(throttle.tick(300));
    }

    #[test]
    fn throttle_interval_changes_take_effect() {
        let mut throttle = Throttle::new(250);
        assert!(throttle.request(0));
        throttle.set_interval_ms(1_000);
        assert!(!throttle.request(500));
        assert!(throttle.request(1_000));
        assert_eq!(throttle.interval_ms(), 1_000);
    }

    #[test]
    fn pause_state_is_visible_in_the_panel_and_cleared_on_pause_toggle() {
        let (_host, _guard) = test_host();
        set_paused(true);
        assert!(is_paused());
        assert!(!take_refresh(10));
        let view = render_panel(None);
        assert!(html_of(&view).contains("data-autoscroll=\"false\""));
        assert!(
            view.status
                .as_deref()
                .unwrap_or("")
                .contains(&t("view.logs.paused"))
        );

        set_paused(false);
        assert!(!is_paused());
        assert!(take_refresh(20));
        let view = render_panel(None);
        assert!(html_of(&view).contains("data-autoscroll=\"true\""));
    }

    // Paging ---------------------------------------------------------------

    #[test]
    fn viewport_serialises_only_the_requested_page() {
        let (_host, _guard) = test_host();
        set_log_text(&big_log(1_000));
        let view = render_panel(Some(Viewport {
            first_row: 900,
            rows: 50,
        }));
        let html = html_of(&view);
        assert_eq!(row_count(&html), 50, "only the page's rows are serialised");
        assert!(html.contains("f0900("), "the page starts at row 900");
        assert!(!html.contains("f0000("), "earlier rows are not serialised");
        assert!(!html.contains("f0950("), "later rows are not serialised");
        let page = paginate(
            &build_rows(&snapshot(), true, &BTreeSet::new(), 1_000.0),
            &Filter::default(),
            Some(Viewport {
                first_row: 900,
                rows: 50,
            }),
        );
        assert_eq!(page.first, 900);
        assert_eq!(page.total, 1_000);
        assert_eq!(page.rows.len(), 50);
    }

    #[test]
    fn default_page_is_bounded_for_large_logs() {
        let (_host, _guard) = test_host();
        set_log_text(&big_log(1_000));
        let view = render_panel(None);
        assert_eq!(row_count(html_of(&view)), DEFAULT_PAGE_ROWS as usize);
        assert!(view.status.as_deref().unwrap_or("").ends_with("/ 1000"));
    }

    // Dedupe / N+1 / errors ------------------------------------------------

    #[test]
    fn duplicate_statements_fold_into_one_row_with_a_count() {
        let (_host, _guard) = test_host();
        set_log_text(
            "==>  Preparing: SELECT * FROM user WHERE id = ?\n==> Parameters: 1(Long)\n\
             ==>  Preparing: SELECT * FROM user WHERE id = ?\n==> Parameters: 1(Long)\n",
        );
        let view = render_panel(None);
        let html = html_of(&view);
        assert_eq!(row_count(&html), 1, "the duplicate group is folded");
        assert!(html.contains("×2"), "the folded row shows the group size");
        let page = paginate(
            &build_rows(&snapshot(), true, &BTreeSet::new(), 1_000.0),
            &Filter::default(),
            None,
        );
        assert_eq!(page.rows[0].count, 2);
    }

    #[test]
    fn expanding_a_duplicate_group_lists_every_occurrence() {
        let (_host, _guard) = test_host();
        set_log_text(
            "==>  Preparing: SELECT * FROM user WHERE id = ?\n==> Parameters: 1(Long)\n\
             ==>  Preparing: SELECT * FROM user WHERE id = ?\n==> Parameters: 1(Long)\n",
        );
        let key = snapshot().duplicates[0].key.clone();
        set_expanded(&key, true);
        let html = html_of(&render_panel(None));
        assert_eq!(row_count(&html), 2, "both occurrences are listed");
        set_expanded(&key, false);
        assert_eq!(row_count(html_of(&render_panel(None))), 1);
    }

    #[test]
    fn n_plus_one_rows_carry_a_badge() {
        let (_host, _guard) = test_host();
        let mut text = String::new();
        for id in 0..5 {
            text.push_str(&format!(
                "==>  Preparing: SELECT * FROM t WHERE id = ?\n==> Parameters: {id}(Integer)\n"
            ));
        }
        set_log_text(&text);
        let html = html_of(&render_panel(None));
        assert_eq!(row_count(&html), 5, "distinct statements are not folded");
        assert!(html.contains("N+1 ×5"), "each row shows the N+1 group size");
        assert_eq!(
            html.matches("class=\"row row-n_plus_one\"").count(),
            5,
            "rows carry the N+1 category"
        );
    }

    #[test]
    fn repeated_identical_statements_collapse_into_an_n_plus_one_row() {
        let (_host, _guard) = test_host();
        let mut text = String::new();
        for _ in 0..5 {
            text.push_str(
                "==>  Preparing: SELECT * FROM t WHERE id = ?\n==> Parameters: 1(Integer)\n",
            );
        }
        set_log_text(&text);
        let html = html_of(&render_panel(None));
        assert_eq!(row_count(&html), 1);
        assert!(html.contains("N+1 ×5"));
    }

    #[test]
    fn error_rows_are_highlighted_and_keep_their_raw_text() {
        let (_host, _guard) = test_host();
        set_log_text("java.sql.SQLException: Table 'demo.user' doesn't exist");
        let html = html_of(&render_panel(None));
        assert!(
            html.contains("row-error"),
            "error rows get the highlight class"
        );
        assert!(html.contains("SQLException"), "the raw line is shown");
        assert!(html.contains("doesn&#39;t"), "raw text is html-escaped");
    }

    // Filtering ------------------------------------------------------------

    #[test]
    fn filters_narrow_by_category_and_keyword() {
        let (_host, _guard) = test_host();
        set_log_text(
            "==>  Preparing: SELECT * FROM user WHERE name = ?\n==> Parameters: alice(String)\n\
             java.sql.SQLException: Table 'demo.user' doesn't exist",
        );
        set_filter(Some(RowCategory::Error), "");
        let html = html_of(&render_panel(None));
        assert_eq!(row_count(&html), 1);
        assert!(html.contains("SQLException"));

        set_filter(None, "alice");
        let html = html_of(&render_panel(None));
        assert_eq!(row_count(&html), 1);
        assert!(html.contains("SELECT * FROM user"));
    }

    // Utilities ------------------------------------------------------------

    #[test]
    fn epoch_timestamps_render_as_utc() {
        assert_eq!(format_epoch_ms(0), "1970-01-01 00:00:00.000");
        assert_eq!(
            format_epoch_ms(1_705_314_645_123),
            "2024-01-15 10:30:45.123"
        );
    }

    #[test]
    fn paginate_clamps_out_of_range_viewports() {
        let (_host, _guard) = test_host();
        set_log_text(&big_log(10));
        let rows = build_rows(&snapshot(), true, &BTreeSet::new(), 1_000.0);
        let page = paginate(
            &rows,
            &Filter::default(),
            Some(Viewport {
                first_row: 50,
                rows: 10,
            }),
        );
        assert_eq!(page.total, 10);
        assert_eq!(page.first, 10);
        assert!(page.rows.is_empty());
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
