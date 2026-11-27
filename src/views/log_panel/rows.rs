//! Row assembly, classification and filtering (lane W2-0).
//!
//! Split out of `views::log_panel` with no behaviour change: the panel row
//! model, `build_rows` / `build_rows_with`, the duplicate and N+1 grouping, the
//! sort orders, and the pure filter matcher.

use std::collections::{BTreeSet, HashMap};

use crate::core::log::{
    DuplicateGroup, EntryClass, LineSource, LogAnalysis, LogEntry, LogSources, NPlusOneGroup,
    SourceRef,
};
use crate::indexer;

use super::state::{Filter, SortMode};
use super::suppress;
use super::{SQL_DISPLAY_CHARS, format_epoch_ms, format_ms, truncate_chars};

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
/// The row's sort timestamp. The panel has one time per row (the parsed log
/// timestamp, which is also what the row header prints); a row that stated none
/// sorts as `0`, exactly like the reference's `displayTimeOf` on a missing value.
fn sort_time(row: &PanelRow) -> i64 {
    row.timestamp_ms.unwrap_or(0)
}

/// The FNV-1a 64-bit hex fingerprint of a literal-parameterised statement.
///
/// This mirrors the reference's `structureFingerprint` (`LogSqlModels.kt`):
/// whitespace is collapsed, a trailing `;` is dropped, every string literal and
/// every bare number is folded to `?`, and the result is hashed with FNV-1a
/// 64-bit. The implementation lives in [`suppress::structure_fingerprint`], one
/// copy shared by the structure sort (lane A) and the suppression rules
/// (lane E), so the two can never disagree on what "the same shape" means.
///
/// The port hashes the parameterised statement's UTF-8 bytes where the
/// reference hashes its UTF-16 code units, so the two produce the same
/// fingerprint for ASCII SQL (every realistic statement) and only diverge on
/// non-ASCII identifiers. Nothing compares across the two plugins: the value is
/// only ever compared with another value from this function, so equal
/// statement shapes always land on the same fingerprint.
fn structure_fingerprint(sql: &str) -> String {
    suppress::structure_fingerprint(sql)
}

/// Orders `rows` in place for `mode`, the reference's `applySort` (`SqlLogPanel.kt`).
///
/// Rust's `sort_by` is stable, so equal rows keep the order they arrived in —
/// the same guarantee the reference gets from `sortedBy` / `sortedWith`.
pub(super) fn apply_sort(rows: &mut Vec<&PanelRow>, mode: SortMode) {
    match mode {
        SortMode::TimeAsc => rows.sort_by_key(|row| sort_time(row)),
        SortMode::TimeDesc => rows.sort_by_key(|row| std::cmp::Reverse(sort_time(row))),
        // Duration: structured duration only (the panel has no text fallback on
        // this side); a missing duration is `MAX` ascending and `MIN`
        // descending, so it sinks in both directions like the reference.
        SortMode::DurationAsc => {
            rows.sort_by(|a, b| duration_asc_key(a).total_cmp(&duration_asc_key(b)))
        }
        SortMode::DurationDesc => {
            rows.sort_by(|a, b| duration_desc_key(b).total_cmp(&duration_desc_key(a)))
        }
        // Row number: the row's own log line, which is what the panel numbers
        // rows by. A row without one (0) sinks to the bottom, as the reference
        // does with its `sequenceNumber`.
        SortMode::Sequence => rows.sort_by_key(|row| {
            if row.line_no > 0 {
                row.line_no
            } else {
                usize::MAX
            }
        }),
        // Structure clusters: same fingerprint adjacent, capture order inside a
        // group (the reference's `compareBy({structureKey}, {displayTimeOf})`).
        SortMode::Structure => {
            let mut keyed: Vec<(String, i64, &PanelRow)> = rows
                .iter()
                .map(|row| (structure_fingerprint(&row.sql), sort_time(row), *row))
                .collect();
            keyed.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
            rows.clear();
            rows.extend(keyed.into_iter().map(|(_, _, row)| row));
        }
        // Group by target database. The reference keys on `databaseName ?:
        // DATABASE_UNKNOWN_SORT_KEY`, so rows that state no database sink to the
        // bottom and keep capture order. This build captures no connection name
        // (see LIMITS: the row badge states the engine only), so every row is in
        // that unknown group and the mode resolves to capture order — the
        // reference behaves the same way for a log without connection names.
        SortMode::Database => rows.sort_by_key(|row| sort_time(row)),
    }
}

fn duration_asc_key(row: &PanelRow) -> f64 {
    row.duration_ms.unwrap_or(f64::MAX)
}

fn duration_desc_key(row: &PanelRow) -> f64 {
    row.duration_ms.unwrap_or(f64::MIN)
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
pub(super) fn display_rows(
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
        if d_new && let Some(group) = dedupe_by_key.get(&row.dedupe_key) {
            lines.extend(group.line_nos.iter().copied());
        }
        if n_new && let Some(group) = n_plus_one_by_key.get(&row.n_plus_one_key) {
            for line in &group.line_nos {
                if !lines.contains(line) {
                    lines.push(*line);
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
pub(super) fn dedupe_key_token(key: &str) -> String {
    format!("d:{key}")
}

/// The expanded-set token for an N+1 template key.
pub(super) fn n_plus_one_key_token(key: &str) -> String {
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

/// Assembles one list row: the log entry plus the derived columns the list
/// needs to render and filter it. Every argument is a distinct derived value,
/// so bundling them into a struct would only move the same list elsewhere.
#[allow(clippy::too_many_arguments)]
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
/// Where "Jump to Java" goes: the one Java method the row's logger names.
///
/// `lookup_source_unique` answers `None` for an overload set or two classes of
/// the same name; the row then offers no jump instead of picking one.
pub(super) fn java_target(
    row: &PanelRow,
    java: &indexer::JavaMethodIndex,
) -> Option<(String, u32)> {
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
pub(super) fn xml_target(
    row: &PanelRow,
    xml: &indexer::XmlStatementIndex,
) -> Option<(String, u32)> {
    let source = row.source_ref()?;
    xml.lookup_unique(&source.class_name, &source.method)
        .map(|location| (location.file.clone(), location.line))
}

#[cfg(test)]
mod tests {
    use super::super::state::{Focus, FocusKind, TypeFilter};
    use super::*;
    use crate::views::log_panel::test_support::{MIXED_LOG, analysis_of, rows_of, test_host};
    use std::collections::BTreeSet;

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
        let mut filter = Filter {
            keyword: "name".to_string(),
            ..Filter::default()
        };
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
        let mut filter = Filter {
            regex: true,
            keyword: "ord.rs".to_string(),
            ..Filter::default()
        };
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

    #[test]
    fn focus_by_template_keeps_only_that_structure() {
        // Lane L32's "按模板聚焦": the chip filters on the same structure key
        // the `×N` badge counts, so rows of other shapes drop out.
        let mut text = String::new();
        for id in 0..5 {
            text.push_str(&format!(
                "==>  Preparing: SELECT * FROM t WHERE id = ?\n==> Parameters: {id}(Integer)\n"
            ));
        }
        text.push_str(
            "==>  Preparing: SELECT * FROM other WHERE id = ?\n==> Parameters: 9(Integer)\n",
        );
        let rows = build_rows(&analysis_of(&text), false, 1000.0);
        let key = rows[0].n_plus_one_key.clone();
        assert!(!key.is_empty(), "the group row carries its template key");
        let filter = Filter {
            focus: Some(Focus {
                kind: FocusKind::Template,
                value: key.clone(),
            }),
            ..Filter::default()
        };
        let kept = filter_rows(&rows, &filter, 1000.0);
        assert_eq!(kept.len(), 5, "{:?}", kept.len());
        assert!(kept.iter().all(|row| row.n_plus_one_key == key));
        // The chip's kind survives the round trip the page uses.
        assert_eq!(FocusKind::Template.token(), "template");
        assert_eq!(FocusKind::parse("template"), Some(FocusKind::Template));
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
    fn structure_fingerprint_folds_literals_but_not_identifiers() {
        // Same shape with different literals -> same fingerprint.
        assert_eq!(
            super::structure_fingerprint("SELECT * FROM t WHERE id = 1 AND name = 'a'"),
            super::structure_fingerprint("SELECT * FROM t WHERE id = 42 AND name = 'b'")
        );
        // Whitespace and a trailing terminator do not change the shape.
        assert_eq!(
            super::structure_fingerprint("SELECT  *  FROM t WHERE id = 1;"),
            super::structure_fingerprint("SELECT * FROM t WHERE id = 7")
        );
        // Different table / column / IN arity -> different fingerprints.
        assert_ne!(
            super::structure_fingerprint("SELECT * FROM a WHERE id = 1"),
            super::structure_fingerprint("SELECT * FROM b WHERE id = 1")
        );
        assert_ne!(
            super::structure_fingerprint("SELECT * FROM t WHERE id IN (1, 2)"),
            super::structure_fingerprint("SELECT * FROM t WHERE id IN (1, 2, 3)")
        );
        // A digit inside an identifier stays: `t2024` is not a literal.
        assert_ne!(
            super::structure_fingerprint("SELECT * FROM t2024 WHERE id = 1"),
            super::structure_fingerprint("SELECT * FROM t2025 WHERE id = 1")
        );
    }
}
