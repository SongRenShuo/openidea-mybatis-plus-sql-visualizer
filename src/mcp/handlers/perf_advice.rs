//! `sqlv_perf_advice`: rules, complexity score and index advice.
//!
//! The rules run on the SQL text alone (no statistics, no catalog), so the
//! result is `partial` whenever the statement is recognised; a text that does
//! not look like SQL yields `unresolved` with an empty score breakdown instead
//! of a misleading number.

use serde::Deserialize;
use serde_json::json;

use crate::core::perf::dialect::profile;
use crate::core::perf::{IndexDef, analyze, index_advice, score};
use crate::mcp::contract::{invalid, parse_args, success};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexArg {
    table: String,
    name: String,
    columns: Vec<String>,
    #[serde(default)]
    unique: bool,
}

impl IndexArg {
    fn to_def(&self) -> IndexDef {
        IndexDef {
            table: self.table.clone(),
            name: self.name.clone(),
            columns: self.columns.clone(),
            unique: self.unique,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    sql: String,
    #[serde(default = "default_dialect")]
    dialect: String,
    #[serde(default)]
    indexes: Vec<IndexArg>,
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
    for index in &args.indexes {
        if index.table.trim().is_empty() || index.name.trim().is_empty() {
            return invalid("index table and name must not be empty");
        }
        if index.columns.is_empty() || index.columns.iter().any(|c| c.trim().is_empty()) {
            return invalid("every index needs at least one non-empty column");
        }
    }
    let indexes: Vec<IndexDef> = args.indexes.iter().map(IndexArg::to_def).collect();
    let findings = analyze(&args.sql, &args.dialect);
    let complexity = score(&args.sql);
    let advice = index_advice(&args.sql, &indexes);
    let confidence = if complexity.recognized {
        "partial"
    } else {
        "unresolved"
    };
    success(
        confidence,
        json!({
            "dialect": args.dialect,
            "findings": findings,
            "score": complexity,
            "advice": advice,
        }),
    )
}
