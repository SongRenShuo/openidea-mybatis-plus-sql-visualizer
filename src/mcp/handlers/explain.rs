//! `sqlv_explain`: format a caller-supplied EXPLAIN result.
//!
//! The platform does not execute SQL from plugins, so this tool never runs an
//! EXPLAIN. Two cases:
//! - the caller passes the result it already obtained: it is formatted as a
//!   table (MySQL row shape) or a tree (nested plan shape) and returned as
//!   `exact` text;
//! - no result: `unresolved` with `engine.explain.unavailable`, never a fake
//!   plan.

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::mcp::contract::{invalid, parse_args, success};

const KEY_EXPLAIN_UNAVAILABLE: &str = "engine.explain.unavailable";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Layout {
    Table,
    Tree,
}

impl Layout {
    fn as_str(self) -> &'static str {
        match self {
            Layout::Table => "table",
            Layout::Tree => "tree",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    format: Option<Layout>,
}

pub fn run(arguments: &str) -> String {
    let args = match parse_args::<Args>(arguments) {
        Ok(args) => args,
        Err(failure) => return failure,
    };
    let layout = args.format.unwrap_or(Layout::Table);
    match args.result {
        None | Some(Value::Null) => success(
            "unresolved",
            json!({
                "available": false,
                "format": layout.as_str(),
                "text": "",
                "message_keys": [KEY_EXPLAIN_UNAVAILABLE],
            }),
        ),
        Some(value @ (Value::Array(_) | Value::Object(_))) => {
            let text = match (&value, layout) {
                (Value::Array(rows), Layout::Table) => table_text(rows),
                (Value::Array(rows), Layout::Tree) => tree_rows(rows),
                (Value::Object(map), Layout::Tree) => tree_object(map),
                (Value::Object(map), Layout::Table) => table_object(map),
                _ => unreachable!("the match arm only admits arrays and objects"),
            };
            success(
                "exact",
                json!({
                    "available": true,
                    "format": layout.as_str(),
                    "text": text,
                }),
            )
        }
        Some(other) => invalid(format!(
            "result must be an object, an array or null; got {}",
            crate::mcp::contract::json_type(&other)
        )),
    }
}

fn table_text(rows: &[Value]) -> String {
    let mut columns: Vec<String> = Vec::new();
    for row in rows {
        if let Some(map) = row.as_object() {
            for key in map.keys() {
                if !columns.iter().any(|column| column == key) {
                    columns.push(key.clone());
                }
            }
        }
    }
    if columns.is_empty() {
        return rows.iter().map(cell_text).collect::<Vec<_>>().join("\n");
    }
    let mut lines = Vec::with_capacity(rows.len() + 2);
    lines.push(columns.join(" | "));
    lines.push(
        columns
            .iter()
            .map(|_| "---")
            .collect::<Vec<_>>()
            .join(" | "),
    );
    for row in rows {
        let cells = columns
            .iter()
            .map(|column| row.get(column).map(cell_text).unwrap_or_default())
            .collect::<Vec<_>>();
        lines.push(cells.join(" | "));
    }
    lines.join("\n")
}

fn table_object(map: &Map<String, Value>) -> String {
    map.iter()
        .map(|(key, value)| format!("{key}: {}", cell_text(value)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn tree_rows(rows: &[Value]) -> String {
    let mut lines = Vec::new();
    for row in rows {
        render_node(row, 0, &mut lines);
    }
    lines.join("\n")
}

fn tree_object(map: &Map<String, Value>) -> String {
    let mut lines = Vec::new();
    render_node(&Value::Object(map.clone()), 0, &mut lines);
    lines.join("\n")
}

fn render_node(value: &Value, depth: usize, lines: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if let Some(plan) = map.get("Plan").or_else(|| map.get("plan")) {
                node_line(plan, depth, lines);
                for (key, item) in map {
                    if key == "Plan" || key == "plan" {
                        continue;
                    }
                    render_detail(key, item, depth + 1, lines);
                }
                return;
            }
            for (key, item) in map {
                render_detail(key, item, depth, lines);
            }
        }
        Value::Array(items) => {
            for item in items {
                render_node(item, depth, lines);
            }
        }
        other => lines.push(format!("{}{}", indent(depth), cell_text(other))),
    }
}

fn node_line(value: &Value, depth: usize, lines: &mut Vec<String>) {
    let Some(map) = value.as_object() else {
        lines.push(format!("{}{}", indent(depth), cell_text(value)));
        return;
    };
    let mut label = map
        .get("Node Type")
        .or_else(|| map.get("node_type"))
        .or_else(|| map.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("Plan")
        .to_string();
    if let Some(relation) = map
        .get("Relation Name")
        .or_else(|| map.get("relation_name"))
        .and_then(Value::as_str)
    {
        label.push_str(" on ");
        label.push_str(relation);
    }
    if let Some(cost) = map.get("Total Cost").or_else(|| map.get("total_cost")) {
        label.push_str(&format!(" (cost={})", cell_text(cost)));
    }
    lines.push(format!("{}{}", indent(depth), label));
    for child_key in ["Plans", "plans", "children"] {
        if let Some(Value::Array(children)) = map.get(child_key) {
            for child in children {
                node_line(child, depth + 1, lines);
            }
            break;
        }
    }
    for (key, item) in map {
        if matches!(
            key.as_str(),
            "Node Type"
                | "node_type"
                | "type"
                | "Relation Name"
                | "relation_name"
                | "Total Cost"
                | "total_cost"
                | "Plans"
                | "plans"
                | "children"
        ) {
            continue;
        }
        render_detail(key, item, depth + 1, lines);
    }
}

fn render_detail(key: &str, value: &Value, depth: usize, lines: &mut Vec<String>) {
    match value {
        Value::Object(_) | Value::Array(_) => {
            lines.push(format!("{}{key}:", indent(depth)));
            render_node(value, depth + 1, lines);
        }
        other => lines.push(format!("{}{key}: {}", indent(depth), cell_text(other))),
    }
}

fn cell_text(value: &Value) -> String {
    match value {
        Value::Null => "NULL".to_string(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn indent(depth: usize) -> String {
    "  ".repeat(depth)
}
