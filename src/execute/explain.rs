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
#[derive(Debug, Clone, PartialEq)]
pub struct ExplainPlan {
    pub dialect: String,
    pub tree: Vec<ExplainNode>,
    pub table: Vec<ExplainRow>,
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

/// Build both views for a dialect from normalized rows.
pub fn build_plan(dialect: &str, rows: Vec<ExplainRow>) -> ExplainPlan {
    let tree = build_tree(&rows);
    ExplainPlan {
        dialect: dialect.to_string(),
        tree,
        table: rows,
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
}
