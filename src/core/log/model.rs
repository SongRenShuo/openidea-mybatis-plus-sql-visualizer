//! Serializable model for SQL log analysis (T1-11).
//!
//! Everything here is plain data: the panel and the MCP tools consume the same
//! `serde` shapes. No platform types, no `host::` calls.

use serde::{Deserialize, Serialize};

/// Log family a statement was recovered from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Family {
    /// MyBatis `Preparing:` / `Parameters:` / `Total:` triples.
    #[serde(rename = "mybatis")]
    MyBatis,
    /// p6spy `#epoch | elapsed | statement | sql` rows.
    #[serde(rename = "p6spy")]
    P6Spy,
    /// Hibernate `Hibernate:` rows and `binding parameter [n] as [TYPE] - [value]`.
    #[serde(rename = "hibernate")]
    Hibernate,
    /// Spring JdbcTemplate `Executing prepared SQL statement [...]` rows.
    #[serde(rename = "jdbc_template")]
    JdbcTemplate,
    /// Error rows and anything that could not be attributed to a parser.
    #[serde(rename = "unknown")]
    Unknown,
}

impl Family {
    /// Stable lower-case name used by diagnostics (not localised).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MyBatis => "mybatis",
            Self::P6Spy => "p6spy",
            Self::Hibernate => "hibernate",
            Self::JdbcTemplate => "jdbc_template",
            Self::Unknown => "unknown",
        }
    }
}

/// Statement class. SQL is classified by its first token, mirroring the
/// reference `LogStatementClassifier` (DDL is kept separate from INSERT/DELETE
/// so filters do not mix them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EntryClass {
    #[serde(rename = "select")]
    Select,
    #[serde(rename = "insert")]
    Insert,
    #[serde(rename = "update")]
    Update,
    #[serde(rename = "delete")]
    Delete,
    /// `CREATE` / `ALTER` / `RENAME` / `DROP` / `TRUNCATE`.
    #[serde(rename = "ddl")]
    Ddl,
    /// `MERGE`, `REPLACE`, stored-procedure calls, anything else.
    #[serde(rename = "other")]
    Other,
    /// A console error row (`SQLException`, MyBatis `### Error`, ...).
    #[serde(rename = "error")]
    Error,
}

impl EntryClass {
    /// Stable lower-case name used by the command-line summaries
    /// (`statements: 2 (delete 1, select 1)`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Select => "select",
            Self::Insert => "insert",
            Self::Update => "update",
            Self::Delete => "delete",
            Self::Ddl => "ddl",
            Self::Other => "other",
            Self::Error => "error",
        }
    }
}

/// Confidence in the produced SQL, per `docs/ARCHITECTURE.md` §4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Confidence {
    /// Every placeholder got its parameter; the SQL is executable as-is.
    #[serde(rename = "exact")]
    Exact,
    /// SQL was recovered but something is missing (parameters, truncation,
    /// unclosed multi-line block); the raw line is preserved in `raw_line`.
    #[serde(rename = "partial")]
    Partial,
    /// No SQL at all (error rows); `sql` stays empty on purpose.
    #[serde(rename = "unresolved")]
    Unresolved,
}

/// One parsed log statement.
///
/// `sql` is the executable form (parameters substituted where possible; an
/// unrecoverable `?` is kept). `raw_sql` keeps the placeholder form so callers
/// can re-render or correlate. `params` holds the rendered SQL literals in
/// placeholder order, which makes duplicate groups self-describing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogEntry {
    pub family: Family,
    /// 1-based line number in the analysed text.
    pub line_no: usize,
    pub timestamp_ms: Option<i64>,
    pub duration_ms: Option<f64>,
    pub sql: String,
    pub params: Vec<String>,
    pub class: EntryClass,
    pub confidence: Confidence,
    /// Placeholder form (`?` intact), truncated to `ParseOptions::max_sql_len`.
    pub raw_sql: String,
    /// `<==    Updates:` count: rows changed by a DML statement.
    pub affected_rows: Option<u64>,
    /// `<==      Total:` count: result rows returned to the caller. Kept apart
    /// from [`Self::affected_rows`] because the two mybatis close rows report
    /// different things.
    pub result_rows: Option<u64>,
    /// Original line text, filled only when `confidence != Exact` so the panel
    /// can show what could not be parsed without keeping every log line.
    pub raw_line: Option<String>,
    /// Dotted lower-case message keys explaining `Partial` / diagnostics.
    pub message_keys: Vec<String>,
}

impl LogEntry {
    /// True when the entry carries executable SQL (error rows do not).
    pub fn has_sql(&self) -> bool {
        !self.sql.is_empty() && self.class != EntryClass::Error
    }
}

/// A group of repeated statements (deduplication).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DuplicateGroup {
    /// Normalised SQL, or the parameterised template when
    /// `ParseOptions::ignore_params_in_dedupe` is set.
    pub key: String,
    /// SQL of the first occurrence (`sql`, i.e. already restored).
    pub sql: String,
    /// Rendered parameters of the first occurrence.
    pub params: Vec<String>,
    pub count: usize,
    pub first_line_no: usize,
    pub last_line_no: usize,
    pub line_nos: Vec<usize>,
}

/// A statement template executed at least `n_plus_one_threshold` times.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NPlusOneGroup {
    /// Literal-parameterised SQL (strings and numbers folded to `?`).
    pub template: String,
    /// First occurrence, restored, for display.
    pub sample_sql: String,
    pub count: usize,
    pub first_line_no: usize,
    pub last_line_no: usize,
    pub line_nos: Vec<usize>,
}

/// A line that looked like SQL but no family could parse it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnparsedLine {
    pub line_no: usize,
    pub text: String,
    /// Dotted lower-case key, e.g. `log.unparsed.orphan_parameters`.
    pub message_key: String,
}

/// Result of one pass over the log text.
///
/// `slow` and `errors` hold copies of the matching entries so a consumer does
/// not have to re-filter; their `line_no` identifies the entry in `entries`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogAnalysis {
    pub entries: Vec<LogEntry>,
    pub duplicates: Vec<DuplicateGroup>,
    pub n_plus_one: Vec<NPlusOneGroup>,
    pub slow: Vec<LogEntry>,
    pub errors: Vec<LogEntry>,
    pub unparsed_lines: Vec<UnparsedLine>,
}

/// Parser knobs. Every field has a `Default`, and the struct is
/// `#[serde(default)]` so the panel can send partial JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ParseOptions {
    /// Statements with `duration_ms >= slow_threshold_ms` land in `slow`.
    pub slow_threshold_ms: f64,
    /// Minimum repeat count for an N+1 group (clamped to at least 2).
    pub n_plus_one_threshold: usize,
    /// When true, duplicates are grouped by literal-parameterised template
    /// instead of the exact restored SQL.
    pub ignore_params_in_dedupe: bool,
    /// Duplicate window in milliseconds. `None` = the whole text; `Some(w)`
    /// starts a new group once the gap between occurrences reaches `w`.
    pub dedupe_window_ms: Option<i64>,
    /// Cap for `raw_sql` / `sql` length, matching the reference 20 000.
    pub max_sql_len: usize,
    /// Offset applied to timestamps that carry no explicit zone (minutes east
    /// of UTC). Default 0 = UTC; the panel can pass the machine offset.
    pub timestamp_offset_minutes: i32,
    /// Recognise Hibernate bare SQL rows (no `Hibernate:` prefix).
    pub recognize_bare_sql: bool,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            slow_threshold_ms: 1000.0,
            n_plus_one_threshold: 5,
            ignore_params_in_dedupe: false,
            dedupe_window_ms: None,
            max_sql_len: 20_000,
            timestamp_offset_minutes: 0,
            recognize_bare_sql: true,
        }
    }
}

/// Dotted lower-case message keys. The i18n lane maps these to text; the core
/// never returns display strings.
pub mod message_keys {
    pub const ENTRY_PARAMS_MISSING: &str = "log.entry.params_missing";
    pub const ENTRY_PARAMS_UNUSED: &str = "log.entry.params_unused";
    pub const ENTRY_SQL_TRUNCATED: &str = "log.entry.sql_truncated";
    pub const ENTRY_MULTILINE_UNCLOSED: &str = "log.entry.multiline_unclosed";
    pub const ENTRY_RAW_PRESERVED: &str = "log.entry.raw_preserved";
    pub const ENTRY_SQL_MISSING: &str = "log.entry.sql_missing";

    pub const UNPARSED_ORPHAN_PARAMETERS: &str = "log.unparsed.orphan_parameters";
    pub const UNPARSED_ORPHAN_PREPARING: &str = "log.unparsed.orphan_preparing";
    pub const UNPARSED_UNRECOGNIZED_SQL: &str = "log.unparsed.unrecognized_sql";
    pub const UNPARSED_SQL_TOO_LONG: &str = "log.unparsed.sql_too_long";
}
