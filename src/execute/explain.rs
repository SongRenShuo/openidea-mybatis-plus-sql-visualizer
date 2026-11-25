//! EXPLAIN result → render model + error classification (T5-5).
//!
//! The plugin does **not** execute EXPLAIN itself ("执行在引擎侧",
//! `docs/ARCHITECTURE.md` §1 — `database` cannot run SQL). The engine returns
//! normalized rows/errors; this module turns them into the two views the UI
//! needs (a tree and a flat table) and classifies a failure so the UI can show
//! the right guidance. Pure: no `host::`, no JDBC.
//!
//! ## Error classification
//!
//! Timeout / permission / syntax / connection are the four cases a user can act
//! on; anything else is [`ExplainErrorKind::Other`]. Classification is
//! keyword-based and deterministic; the message text itself is never shown raw
//! (the UI maps [`ExplainError::message_key`] to localized prose).

use serde::{Deserialize, Serialize};

use crate::sidecar::lifecycle::EngineHandle;
use crate::sidecar::proto::{
    ConnSpec, ErrorValue, ExplainExecuteRequest, ExplainExecuteResponse, Request,
};

/// The four act-on-able failure families plus a catch-all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExplainErrorKind {
    Timeout,
    Permission,
    Syntax,
    Connection,
    Other,
}

impl ExplainErrorKind {
    pub fn message_key(self) -> &'static str {
        match self {
            ExplainErrorKind::Timeout => "explain.error.timeout",
            ExplainErrorKind::Permission => "explain.error.permission",
            ExplainErrorKind::Syntax => "explain.error.syntax",
            ExplainErrorKind::Connection => "explain.error.connection",
            ExplainErrorKind::Other => "explain.error.other",
        }
    }
}

/// A classified EXPLAIN failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplainError {
    pub kind: ExplainErrorKind,
    pub message_key: &'static str,
    pub detail: String,
}

impl ExplainError {
    pub fn new(kind: ExplainErrorKind, detail: impl Into<String>) -> Self {
        ExplainError {
            kind,
            message_key: kind.message_key(),
            detail: detail.into(),
        }
    }

    /// A classified error with an explicit stable key (rejections use keys the
    /// five-kind mapping does not cover, e.g. `explain.reject.writeStatement`).
    pub fn with_key(
        kind: ExplainErrorKind,
        message_key: &'static str,
        detail: impl Into<String>,
    ) -> Self {
        ExplainError {
            kind,
            message_key,
            detail: detail.into(),
        }
    }
}

/// The plan's encoding as the engine returned it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExplainFormat {
    Json,
    Xml,
    Text,
}

impl ExplainFormat {
    pub fn from_wire(value: Option<&str>) -> ExplainFormat {
        match value {
            Some(v) if v.eq_ignore_ascii_case("json") => ExplainFormat::Json,
            Some(v) if v.eq_ignore_ascii_case("xml") => ExplainFormat::Xml,
            _ => ExplainFormat::Text,
        }
    }
}

/// One normalized EXPLAIN row.
///
/// `id`/`parent_id` carry the nesting when the engine reports it (Postgres-style
/// plan trees); MySQL-style tabular EXPLAIN leaves `parent_id` `None`.
#[derive(Debug, Clone, PartialEq)]
pub struct ExplainRow {
    pub id: Option<i64>,
    pub parent_id: Option<i64>,
    pub operation: String,
    pub object: Option<String>,
    pub cost: Option<f64>,
    pub rows: Option<u64>,
    pub extra: Option<String>,
}

/// A node in the tree view.
#[derive(Debug, Clone, PartialEq)]
pub struct ExplainNode {
    pub depth: u32,
    pub label: String,
    pub detail: String,
    pub children: Vec<ExplainNode>,
}

/// Both render views over the same rows.
///
/// For a **live** plan ([`run_live`]) the tree/table are empty and the raw
/// engine output is carried verbatim in `plan` with its [`ExplainFormat`]; a
/// caller that wants the normalized views feeds rows through [`build_plan`].
#[derive(Debug, Clone, PartialEq)]
pub struct ExplainPlan {
    pub dialect: String,
    pub tree: Vec<ExplainNode>,
    pub table: Vec<ExplainRow>,
    pub format: ExplainFormat,
    pub plan: String,
}

/// Classify an engine error message into an [`ExplainErrorKind`].
pub fn classify(message: &str) -> ExplainErrorKind {
    let m = message.to_ascii_lowercase();
    let has = |needle: &str| m.contains(needle);
    if has("timeout") || has("timed out") || has("deadline") {
        ExplainErrorKind::Timeout
    } else if has("permission")
        || has("access denied")
        || has("not authorized")
        || has("privilege")
        || has("insufficient")
    {
        ExplainErrorKind::Permission
    } else if has("syntax")
        || has("parse error")
        || has("unexpected token")
        || has("unexpected end")
    {
        ExplainErrorKind::Syntax
    } else if has("connection")
        || has("could not connect")
        || has("refused")
        || has("no route")
        || has("unknown host")
        || has("communications link")
        || has("socket")
    {
        ExplainErrorKind::Connection
    } else {
        ExplainErrorKind::Other
    }
}

/// Classify and wrap an engine error message.
pub fn classify_error(message: &str) -> ExplainError {
    ExplainError::new(classify(message), message)
}

// ---------------------------------------------------------------------------
// Live EXPLAIN (T5-5): ask the engine to connect and run the plan.
// ---------------------------------------------------------------------------

/// Stable message keys the engine can send back; only these are adopted as
/// `&'static str` (anything else falls back to the kind's own key).
const MESSAGE_KEYS: [&str; 8] = [
    "explain.error.timeout",
    "explain.error.permission",
    "explain.error.syntax",
    "explain.error.connection",
    "explain.error.other",
    "explain.reject.writeStatement",
    "explain.reject.multiStatement",
    "explain.reject.dialect",
];

pub const REJECT_WRITE_KEY: &str = "explain.reject.writeStatement";
pub const REJECT_MULTI_KEY: &str = "explain.reject.multiStatement";

/// First keywords a live plan may start with (mirrors the engine's
/// `ExplainGate.ALLOWED_FIRST_KEYWORDS`, which is the single source of truth
/// on the Kotlin side).
const ALLOWED_FIRST_KEYWORDS: [&str; 4] = ["select", "with", "values", "table"];

/// Words that may sit between `EXPLAIN` and the query (PG's option list).
const EXPLAIN_OPTION_WORDS: [&str; 21] = [
    "analyze",
    "analyse",
    "verbose",
    "costs",
    "buffers",
    "format",
    "timing",
    "summary",
    "settings",
    "wal",
    "generic_plan",
    "memory",
    "serialize",
    "text",
    "json",
    "xml",
    "yaml",
    "true",
    "false",
    "on",
    "off",
];

fn static_message_key(key: Option<&str>) -> Option<&'static str> {
    let key = key?;
    MESSAGE_KEYS.iter().copied().find(|known| *known == key)
}

/// Run a live EXPLAIN through the engine sidecar.
///
/// Safety is decided **before** the wire: [`validate_live_sql`] rejects writes,
/// DDL and multi-statement input, so a rejected query never reaches a database
/// (the engine repeats the gate defensively). Transport failures degrade to a
/// [`ExplainErrorKind::Connection`] error, never a panic.
pub fn run_live(
    engine: &EngineHandle,
    conn: &ConnSpec,
    sql: &str,
) -> Result<ExplainPlan, ExplainError> {
    validate_live_sql(sql)?;
    let request: Request = ExplainExecuteRequest {
        sql: sql.to_string(),
        conn: conn.clone(),
    }
    .into();
    let line = serde_json::to_string(&request).map_err(|e| {
        ExplainError::new(
            ExplainErrorKind::Other,
            format!("encode explain.execute request: {e}"),
        )
    })?;
    // `request_line` never returns the reply body in the error; the degrade
    // reason is a stable key, so no credential can leak through this path.
    let reply = engine.request_line(&line).map_err(|reason| {
        ExplainError::new(
            ExplainErrorKind::Connection,
            format!("engine request failed: {}", reason.message_key()),
        )
    })?;
    parse_live_response(&reply)
}

/// Decode an `explain.execute` reply into a plan or a classified error.
///
/// Kept separate from [`run_live`] so the wire contract is unit-testable
/// without a process.
pub fn parse_live_response(reply: &str) -> Result<ExplainPlan, ExplainError> {
    let response: ExplainExecuteResponse = serde_json::from_str(reply.trim()).map_err(|e| {
        ExplainError::new(
            ExplainErrorKind::Other,
            format!("decode explain.execute reply: {e}"),
        )
    })?;
    if response.ok {
        Ok(ExplainPlan {
            dialect: String::new(),
            tree: Vec::new(),
            table: Vec::new(),
            format: ExplainFormat::from_wire(response.format.as_deref()),
            plan: response.plan.unwrap_or_default(),
        })
    } else {
        Err(error_from_response(&response))
    }
}

fn error_from_response(response: &ExplainExecuteResponse) -> ExplainError {
    // `detail` is a one-line message; the structured dispatcher error
    // (`{"ok":false,"kind":"invalid","error":"…"}`) is the only other shape.
    let detail = response
        .detail
        .clone()
        .or_else(|| match &response.error {
            Some(ErrorValue::Message(message)) => Some(message.clone()),
            Some(ErrorValue::Structured(error)) => Some(error.message.clone()),
            None => None,
        })
        .unwrap_or_default();
    let kind = match response.kind.as_deref() {
        Some("timeout") => ExplainErrorKind::Timeout,
        Some("permission") => ExplainErrorKind::Permission,
        Some("syntax") => ExplainErrorKind::Syntax,
        Some("connection") => ExplainErrorKind::Connection,
        Some("other") => ExplainErrorKind::Other,
        // The engine uses `rejected` for the static gate; the actionable
        // family is a syntax/input problem, with a more specific key.
        Some("rejected") => ExplainErrorKind::Syntax,
        _ => classify(&detail),
    };
    match static_message_key(response.message_key.as_deref()) {
        Some(key) => ExplainError::with_key(kind, key, detail),
        None => ExplainError::new(kind, detail),
    }
}

/// The pure half of the live gate: exactly one read-only query statement.
///
/// The clause list mirrors the engine's `ExplainGate` (SELECT / WITH / VALUES /
/// TABLE). An `EXPLAIN` prefix is stripped first (including PG's option list
/// and `ANALYZE`, which would actually execute the query).
pub fn validate_live_sql(sql: &str) -> Result<(), ExplainError> {
    if sql.trim().is_empty() {
        return Err(ExplainError::with_key(
            ExplainErrorKind::Syntax,
            "explain.error.syntax",
            "sql is empty",
        ));
    }
    let statements = split_top_level_statements(sql);
    if statements.len() != 1 {
        return Err(ExplainError::with_key(
            ExplainErrorKind::Syntax,
            REJECT_MULTI_KEY,
            format!(
                "exactly one statement can be explained, got {}",
                statements.len()
            ),
        ));
    }
    let query = strip_explain_prefix(statements[0]);
    match first_keyword(query) {
        Some(keyword) if ALLOWED_FIRST_KEYWORDS.contains(&keyword.as_str()) => Ok(()),
        other => Err(ExplainError::with_key(
            ExplainErrorKind::Syntax,
            REJECT_WRITE_KEY,
            format!(
                "only read-only SELECT/WITH/VALUES/TABLE queries can be explained, first keyword: {}",
                other.as_deref().unwrap_or("<none>")
            ),
        )),
    }
}

/// Split at top-level `;`, skipping string literals and comments.
fn split_top_level_statements(sql: &str) -> Vec<&str> {
    let bytes = sql.as_bytes();
    let mut statements = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            quote @ (b'\'' | b'"' | b'`') => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == quote {
                        if i + 1 < bytes.len() && bytes[i + 1] == quote {
                            i += 2; // doubled quote stays inside the literal
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'[' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b']' {
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1;
                }
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            }
            b';' => {
                let statement = sql[start..i].trim();
                if !statement.is_empty() {
                    statements.push(statement);
                }
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    let tail = sql[start..].trim();
    if !tail.is_empty() {
        statements.push(tail);
    }
    statements
}

fn starts_with_explain_keyword(sql: &str) -> bool {
    let bytes = sql.as_bytes();
    if bytes.len() < 7 || !bytes[..7].eq_ignore_ascii_case(b"EXPLAIN") {
        return false;
    }
    bytes.len() == 7 || !(bytes[7].is_ascii_alphanumeric() || bytes[7] == b'_')
}

/// Strip `EXPLAIN` plus its option list; returns the query text (or `""`).
fn strip_explain_prefix(sql: &str) -> &str {
    if !starts_with_explain_keyword(sql) {
        return sql;
    }
    let bytes = sql.as_bytes();
    let len = bytes.len();
    let mut i = 7;
    while i < len {
        match bytes[i] {
            byte if byte.is_ascii_whitespace() => i += 1,
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                while i < len && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < len && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(len);
            }
            b'(' => i = skip_balanced_parentheses(bytes, i),
            byte if byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$' => {
                let mut j = i;
                while j < len
                    && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' || bytes[j] == b'$')
                {
                    j += 1;
                }
                if !EXPLAIN_OPTION_WORDS.contains(&sql[i..j].to_ascii_lowercase().as_str()) {
                    return &sql[i..];
                }
                i = j;
            }
            _ => return &sql[i..],
        }
    }
    ""
}

fn skip_balanced_parentheses(bytes: &[u8], from: usize) -> usize {
    let mut depth = 0usize;
    let mut i = from;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    bytes.len()
}

/// The first word of a statement (lowercased), skipping comments and leading
/// parentheses — the same lexical boundary the engine's `ExplainGate` uses.
fn first_keyword(sql: &str) -> Option<String> {
    let bytes = sql.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        match bytes[i] {
            byte if byte.is_ascii_whitespace() => i += 1,
            b'(' => i += 1,
            b'#' => {
                while i < len && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                while i < len && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < len && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(len);
            }
            byte if byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$' => {
                let mut j = i;
                while j < len
                    && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' || bytes[j] == b'$')
                {
                    j += 1;
                }
                return Some(sql[i..j].to_ascii_lowercase());
            }
            _ => return None,
        }
    }
    None
}

/// Build both views for a dialect from normalized rows.
pub fn build_plan(dialect: &str, rows: Vec<ExplainRow>) -> ExplainPlan {
    let tree = build_tree(&rows);
    ExplainPlan {
        dialect: dialect.to_string(),
        tree,
        table: rows,
        format: ExplainFormat::Text,
        plan: String::new(),
    }
}

/// Normalize `column → value` rows (e.g. MySQL `EXPLAIN` output) into [`ExplainRow`]s.
pub fn rows_from_columns(rows: &[Vec<(String, String)>]) -> Vec<ExplainRow> {
    rows.iter()
        .map(|cols| {
            let get = |key: &str| {
                cols.iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(key))
                    .map(|(_, v)| v.trim())
                    .filter(|v| !v.is_empty())
            };
            ExplainRow {
                id: get("id").and_then(|v| v.parse().ok()),
                parent_id: get("parent_id").and_then(|v| v.parse().ok()),
                operation: get("select_type")
                    .or_else(|| get("operation"))
                    .or_else(|| get("type"))
                    .or_else(|| get("node type"))
                    .unwrap_or("")
                    .to_string(),
                object: get("table").map(str::to_string),
                cost: get("cost")
                    .or_else(|| get("total_cost"))
                    .and_then(|v| v.parse().ok()),
                rows: get("rows").and_then(|v| v.parse().ok()),
                extra: get("extra").map(str::to_string),
            }
        })
        .collect()
}

fn label(row: &ExplainRow) -> String {
    match &row.object {
        Some(object) if !object.is_empty() => format!("{} on {}", row.operation, object),
        _ => row.operation.clone(),
    }
}

fn detail(row: &ExplainRow) -> String {
    let mut parts = Vec::new();
    if let Some(cost) = row.cost {
        parts.push(format!("cost={cost}"));
    }
    if let Some(rows) = row.rows {
        parts.push(format!("rows={rows}"));
    }
    if let Some(extra) = &row.extra
        && !extra.is_empty()
    {
        parts.push(extra.clone());
    }
    parts.join(", ")
}

fn build_tree(rows: &[ExplainRow]) -> Vec<ExplainNode> {
    if rows.iter().any(|r| r.parent_id.is_some()) {
        let ids: std::collections::BTreeSet<i64> = rows.iter().filter_map(|r| r.id).collect();
        rows.iter()
            .filter(|r| match r.parent_id {
                None => true,
                Some(parent) => !ids.contains(&parent),
            })
            .map(|r| build_node(r, 0, rows))
            .collect()
    } else {
        // Tabular EXPLAIN: rows are siblings; the tree is a flat list.
        rows.iter()
            .map(|r| ExplainNode {
                depth: 0,
                label: label(r),
                detail: detail(r),
                children: Vec::new(),
            })
            .collect()
    }
}

fn build_node(row: &ExplainRow, depth: u32, all: &[ExplainRow]) -> ExplainNode {
    let children = match row.id {
        Some(id) => all
            .iter()
            .filter(|c| c.parent_id == Some(id))
            .map(|c| build_node(c, depth + 1, all))
            .collect(),
        None => Vec::new(),
    };
    ExplainNode {
        depth,
        label: label(row),
        detail: detail(row),
        children,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: Option<i64>, parent: Option<i64>, op: &str, obj: &str) -> ExplainRow {
        ExplainRow {
            id,
            parent_id: parent,
            operation: op.to_string(),
            object: Some(obj.to_string()),
            cost: Some(1.5),
            rows: Some(10),
            extra: None,
        }
    }

    // ---- classification (≥4) ----

    #[test]
    fn classifies_timeout() {
        let e = classify_error("canceling statement due to statement timeout");
        assert_eq!(e.kind, ExplainErrorKind::Timeout);
        assert_eq!(e.message_key, "explain.error.timeout");
    }

    #[test]
    fn classifies_permission() {
        assert_eq!(
            classify("SELECT command denied: permission denied for table t"),
            ExplainErrorKind::Permission
        );
    }

    #[test]
    fn classifies_syntax() {
        assert_eq!(
            classify("You have an error: syntax error near 'FORM'"),
            ExplainErrorKind::Syntax
        );
    }

    #[test]
    fn classifies_connection() {
        assert_eq!(
            classify("Communications link failure: connection refused"),
            ExplainErrorKind::Connection
        );
    }

    #[test]
    fn classifies_other_and_prefers_timeout_over_connection() {
        assert_eq!(classify("something odd happened"), ExplainErrorKind::Other);
        // "connection timeout" is a timeout (the actionable cause), not generic.
        assert_eq!(
            classify("connection timeout exceeded"),
            ExplainErrorKind::Timeout
        );
        assert_eq!(ExplainErrorKind::Other.message_key(), "explain.error.other");
    }

    // ---- tree / table views (≥3) ----

    #[test]
    fn tabular_rows_become_a_flat_tree_and_full_table() {
        let rows = vec![
            row(Some(1), None, "SIMPLE", "sys_user"),
            row(Some(1), None, "SIMPLE", "sys_order"),
        ];
        let plan = build_plan("MySQL", rows);
        assert_eq!(plan.table.len(), 2, "table view keeps every row");
        assert_eq!(plan.tree.len(), 2, "no parent ids → flat siblings");
        assert_eq!(plan.tree[0].label, "SIMPLE on sys_user");
        assert_eq!(plan.tree[0].depth, 0);
        assert_eq!(plan.dialect, "MySQL");
    }

    #[test]
    fn parent_ids_build_a_nested_tree() {
        let rows = vec![
            row(Some(1), None, "Nested Loop", "(root)"),
            row(Some(2), Some(1), "Index Scan", "sys_user"),
            row(Some(3), Some(2), "Bitmap Heap Scan", "sys_order"),
        ];
        let plan = build_plan("PostgreSQL", rows);
        assert_eq!(plan.table.len(), 3);
        assert_eq!(plan.tree.len(), 1, "one root");
        let root = &plan.tree[0];
        assert_eq!(root.depth, 0);
        assert_eq!(root.children.len(), 1);
        assert_eq!(root.children[0].depth, 1);
        assert_eq!(root.children[0].children[0].depth, 2);
        assert_eq!(root.children[0].label, "Index Scan on sys_user");
    }

    #[test]
    fn rows_from_mysql_columns_maps_the_known_keys() {
        let cols = vec![
            vec![
                ("id".to_string(), "1".to_string()),
                ("select_type".to_string(), "SIMPLE".to_string()),
                ("table".to_string(), "sys_user".to_string()),
                ("type".to_string(), "ALL".to_string()),
                ("rows".to_string(), "42".to_string()),
                ("Extra".to_string(), "Using where".to_string()),
            ],
            vec![
                ("id".to_string(), "1".to_string()),
                ("select_type".to_string(), "SIMPLE".to_string()),
                ("table".to_string(), "sys_order".to_string()),
                ("rows".to_string(), "7".to_string()),
            ],
        ];
        let rows = rows_from_columns(&cols);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, Some(1));
        assert_eq!(rows[0].operation, "SIMPLE");
        assert_eq!(rows[0].object.as_deref(), Some("sys_user"));
        assert_eq!(rows[0].rows, Some(42));
        assert_eq!(rows[0].extra.as_deref(), Some("Using where"));
        assert!(rows[1].extra.is_none());
    }

    // ---- live gate: validation (pure, no connection) ----

    #[test]
    fn allows_single_read_only_queries() {
        for sql in [
            "SELECT 1",
            "select * from sys_user where id = 1",
            "WITH x AS (SELECT 1) SELECT * FROM x",
            "VALUES (1), (2)",
            "TABLE sys_user",
            "EXPLAIN SELECT 1",
            "EXPLAIN ANALYZE (FORMAT JSON) SELECT 1",
            "-- get users\nSELECT 1",
            "SELECT ';' AS semicolon",
        ] {
            assert!(validate_live_sql(sql).is_ok(), "should accept: {sql}");
        }
    }

    #[test]
    fn rejects_writes_ddl_and_unknown_leading_words() {
        for sql in [
            "DELETE FROM sys_user",
            "UPDATE sys_user SET name = 'x'",
            "INSERT INTO sys_user VALUES (1)",
            "CREATE TABLE t (a int)",
            "DROP TABLE t",
            "SELECTED_IS_NOT_A_KEYWORD",
            "EXPLAIN DELETE FROM t",
        ] {
            let error = validate_live_sql(sql).expect_err("must reject");
            assert_eq!(error.kind, ExplainErrorKind::Syntax);
            assert_eq!(error.message_key, REJECT_WRITE_KEY);
        }
    }

    #[test]
    fn rejects_multi_statement_input_before_connecting() {
        let error = validate_live_sql("SELECT 1; DELETE FROM t").expect_err("must reject");
        assert_eq!(error.message_key, REJECT_MULTI_KEY);
        assert!(
            validate_live_sql("SELECT 1;").is_ok(),
            "a trailing semicolon is still one statement"
        );
    }

    #[test]
    fn strips_explain_prefix_and_its_options() {
        assert_eq!(strip_explain_prefix("EXPLAIN SELECT 1"), "SELECT 1");
        assert_eq!(
            strip_explain_prefix("EXPLAIN (FORMAT JSON) SELECT 1"),
            "SELECT 1"
        );
        assert_eq!(
            strip_explain_prefix("explain analyze verbose select 1"),
            "select 1"
        );
        assert_eq!(strip_explain_prefix("SELECT 1"), "SELECT 1");
        assert_eq!(strip_explain_prefix("EXPLAIN"), "");
    }

    // ---- live wire: request / response ----

    #[test]
    fn explain_execute_request_serializes_the_dot_kind_and_connection() {
        let request: Request = ExplainExecuteRequest {
            sql: "SELECT 1".into(),
            conn: ConnSpec::new("jdbc:postgresql://db:5432/x", "u", "test-secret")
                .with_timeout_ms(1_500),
        }
        .into();
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains(r#""kind":"explain.execute""#), "{json}");
        assert!(
            json.contains(r#""url":"jdbc:postgresql://db:5432/x""#),
            "{json}"
        );
        assert!(json.contains(r#""timeoutMs":1500"#), "{json}");
        assert!(json.contains(r#""sql":"SELECT 1""#), "{json}");
    }

    #[test]
    fn parses_a_json_plan_from_a_successful_reply() {
        let plan = parse_live_response(r#"{"ok":true,"format":"json","plan":"[{\"Plan\":{}}]"}"#)
            .expect("ok reply");
        assert_eq!(plan.format, ExplainFormat::Json);
        assert!(plan.plan.contains("Plan"));
        assert!(plan.tree.is_empty() && plan.table.is_empty());
    }

    #[test]
    fn parses_an_xml_plan_and_defaults_unknown_formats_to_text() {
        let xml = parse_live_response(r#"{"ok":true,"format":"xml","plan":"<ShowPlanXML/>"}"#)
            .expect("ok reply");
        assert_eq!(xml.format, ExplainFormat::Xml);
        let text = parse_live_response(r#"{"ok":true,"plan":"Seq Scan"}"#).expect("ok reply");
        assert_eq!(text.format, ExplainFormat::Text);
        assert_eq!(text.plan, "Seq Scan");
    }

    #[test]
    fn maps_engine_error_kinds_and_message_keys() {
        let timeout = parse_live_response(
            r#"{"ok":false,"kind":"timeout","messageKey":"explain.error.timeout","detail":"57014: canceling statement"}"#,
        )
        .expect_err("timeout");
        assert_eq!(timeout.kind, ExplainErrorKind::Timeout);
        assert_eq!(timeout.message_key, "explain.error.timeout");
        assert!(timeout.detail.starts_with("57014"));

        let permission = parse_live_response(
            r#"{"ok":false,"kind":"permission","messageKey":"explain.error.permission","detail":"42501"}"#,
        )
        .expect_err("permission");
        assert_eq!(permission.kind, ExplainErrorKind::Permission);

        let rejected = parse_live_response(
            r#"{"ok":false,"kind":"rejected","messageKey":"explain.reject.writeStatement","detail":"only read-only"}"#,
        )
        .expect_err("rejected");
        assert_eq!(rejected.kind, ExplainErrorKind::Syntax);
        assert_eq!(rejected.message_key, REJECT_WRITE_KEY);

        let generic =
            parse_live_response(r#"{"ok":false,"kind":"invalid","error":"missing kind"}"#)
                .expect_err("generic");
        assert_eq!(generic.kind, ExplainErrorKind::Other);
        assert_eq!(generic.message_key, "explain.error.other");
        assert_eq!(generic.detail, "missing kind");

        let bare = parse_live_response(r#"{"ok":false,"error":"Communications link failure"}"#)
            .expect_err("bare");
        assert_eq!(bare.kind, ExplainErrorKind::Connection);
    }

    #[test]
    fn conn_spec_debug_redacts_the_password() {
        let text = format!(
            "{:?}",
            ConnSpec::new("jdbc:sqlserver://db:1433", "sa", "test-secret")
        );
        assert!(text.contains("<redacted>"), "{text}");
        assert!(!text.contains("test-secret"), "{text}");
    }

    // ---- live end-to-end (ignored: needs a database and the pinned jar) ----

    fn env_triplet(prefix: &str) -> Option<ConnSpec> {
        let url = std::env::var(format!("{prefix}_URL")).ok()?;
        let user = std::env::var(format!("{prefix}_USER")).ok()?;
        let password = std::env::var(format!("{prefix}_PASSWORD")).ok()?;
        Some(ConnSpec::new(url, user, password))
    }

    #[test]
    #[ignore = "live database: set MPSV_PG_URL/USER/PASSWORD and provide the pinned engine jar"]
    fn live_explain_postgres_returns_a_json_plan() {
        let Some(conn) = env_triplet("MPSV_PG") else {
            eprintln!("skip: MPSV_PG_URL/USER/PASSWORD not set");
            return;
        };
        let mut engine = match crate::sidecar::start(crate::sidecar::pinned_version()) {
            Ok(engine) => engine,
            Err(reason) => {
                eprintln!("skip: engine unavailable: {}", reason.message_key());
                return;
            }
        };
        let result = run_live(&engine, &conn, "SELECT 1");
        let _ = engine.stop();
        let plan = result.expect("live PostgreSQL EXPLAIN should succeed");
        assert_eq!(plan.format, ExplainFormat::Json);
        assert!(!plan.plan.trim().is_empty());
    }

    #[test]
    #[ignore = "live database: set MPSV_MSSQL_URL/USER/PASSWORD and provide the pinned engine jar"]
    fn live_explain_sqlserver_returns_an_xml_plan() {
        let Some(conn) = env_triplet("MPSV_MSSQL") else {
            eprintln!("skip: MPSV_MSSQL_URL/USER/PASSWORD not set");
            return;
        };
        let mut engine = match crate::sidecar::start(crate::sidecar::pinned_version()) {
            Ok(engine) => engine,
            Err(reason) => {
                eprintln!("skip: engine unavailable: {}", reason.message_key());
                return;
            }
        };
        let result = run_live(&engine, &conn, "SELECT 1");
        let _ = engine.stop();
        let plan = result.expect("live SQL Server EXPLAIN should succeed");
        assert_eq!(plan.format, ExplainFormat::Xml);
        assert!(!plan.plan.trim().is_empty());
    }
}
