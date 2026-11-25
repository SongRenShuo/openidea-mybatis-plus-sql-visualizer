//! `sqlv_check_sql`: lightweight statement classification and write-safety
//! checks.
//!
//! This is a text-level classifier built in this lane, not an executed plan:
//! it knows the first keyword and the presence of a top-level `WHERE`, nothing
//! about the catalog. It therefore always reports `partial` — never `exact`.
//! The checks are:
//! - `unbounded_write`: UPDATE/DELETE without a top-level WHERE;
//! - `destructive_ddl`: DROP or TRUNCATE (the statement destroys data/objects).
//!
//! Performance findings come from `core::perf::analyze`, so rule ids and
//! message keys match the rest of the plugin.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::core::log::{EntryClass, classify_sql};
use crate::core::perf::analyze;
use crate::core::perf::dialect::profile;
use crate::core::perf::sql::{Analysis, StmtKind};
use crate::mcp::contract::{invalid, parse_args, success};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    sql: String,
    #[serde(default = "default_dialect")]
    dialect: String,
}

fn default_dialect() -> String {
    "mysql".to_string()
}

pub fn run(arguments: &str) -> String {
    let args = match parse_args::<Args>(arguments) {
        Ok(args) => args,
        Err(failure) => return failure,
    };
    if args.sql.trim().is_empty() {
        return invalid("sql must not be empty");
    }
    if profile(&args.dialect).is_none() {
        return invalid(format!("unsupported dialect: {}", args.dialect));
    }

    let analysis = Analysis::parse(&args.sql);
    let class = class_name(classify_sql(&args.sql));
    let keyword = analysis.word_at(0).unwrap_or("").to_string();
    let statement = match analysis.stmt {
        StmtKind::Update => "UPDATE",
        StmtKind::Delete => "DELETE",
        _ => keyword.as_str(),
    };
    let unbounded_write = matches!(analysis.stmt, StmtKind::Update | StmtKind::Delete)
        && analysis.find_top("WHERE").is_none();
    let destructive = matches!(keyword.as_str(), "DROP" | "TRUNCATE");

    let mut checks: Vec<Value> = Vec::new();
    if unbounded_write {
        checks.push(json!({
            "rule_id": "unbounded_write",
            "message_key": "check.unbounded_write",
            "severity": "error",
            "detail": { "statement": statement },
        }));
    }
    if destructive {
        checks.push(json!({
            "rule_id": "destructive_ddl",
            "message_key": "check.destructive_ddl",
            "severity": "error",
            "detail": { "keyword": keyword },
        }));
    }

    let findings = analyze(&args.sql, &args.dialect);
    success(
        "partial",
        json!({
            "class": class,
            "keyword": keyword,
            "dialect": args.dialect,
            "unbounded_write": unbounded_write,
            "destructive": destructive,
            "checks": checks,
            "findings": findings,
        }),
    )
}

fn class_name(class: EntryClass) -> &'static str {
    match class {
        EntryClass::Select => "QUERY",
        EntryClass::Insert | EntryClass::Update | EntryClass::Delete => "DML",
        EntryClass::Ddl => "DDL",
        EntryClass::Other | EntryClass::Error => "UNKNOWN",
    }
}
