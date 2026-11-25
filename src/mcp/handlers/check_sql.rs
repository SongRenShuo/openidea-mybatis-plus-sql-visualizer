//! `sqlv_check_sql`: lightweight statement classification and write-safety
//! checks.
//!
//! This is a text-level classifier built in this lane, not an executed plan:
//! it knows the leading keyword and the presence of a top-level `WHERE`,
//! nothing about the catalog. It therefore always reports `partial` — never
//! `exact`.
//!
//! `;`-separated statements are checked one by one (`core::perf::check`), so a
//! later statement's `WHERE` can no longer hide an earlier unbounded write.
//! The checks are:
//! - `unbounded_write`: any UPDATE/DELETE without a top-level WHERE;
//! - `destructive_ddl`: any DROP or TRUNCATE (the statement destroys data or
//!   objects).
//!
//! The historical payload fields keep their meaning; a multi-statement input
//! adds `statements` (per-statement summary) and `evidence` (which statements
//! triggered each verdict). Performance findings come from the same core
//! module, so rule ids and message keys match the rest of the plugin.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::core::log::EntryClass;
use crate::core::perf::dialect::profile;
use crate::core::perf::{Finding, SqlCheck, check};
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

    let report = check(&args.sql, &args.dialect);
    let checks = check_entries(&report);
    let findings = findings_payload(&report);
    success(
        "partial",
        json!({
            "class": class_name(report.class),
            "keyword": report.keyword,
            "dialect": args.dialect,
            "unbounded_write": report.unbounded_write,
            "destructive": report.destructive,
            "checks": checks,
            "findings": findings,
            "statements": report
                .statements
                .iter()
                .map(|statement| json!({
                    "index": statement.index,
                    "class": class_name(statement.class),
                    "keyword": statement.keyword,
                    "findings": statement.findings.len(),
                    "unbounded_write": report.unbounded_statements.contains(&statement.index),
                    "destructive": report.destructive_statements.contains(&statement.index),
                }))
                .collect::<Vec<_>>(),
            "evidence": {
                "unbounded_write": report.unbounded_statements,
                "destructive": report.destructive_statements,
            },
        }),
    )
}

/// `checks` entries, one per offending statement. The historical `detail`
/// shape (`{"statement": …}` / `{"keyword": …}`) is kept for a single
/// statement; multi-statement input adds the `index`.
fn check_entries(report: &SqlCheck) -> Vec<Value> {
    let multi = report.is_multi_statement();
    let mut checks: Vec<Value> = Vec::new();
    for index in &report.unbounded_statements {
        let statement = statement_keyword(report, *index);
        checks.push(json!({
            "rule_id": "unbounded_write",
            "message_key": "check.unbounded_write",
            "severity": "error",
            "detail": if multi {
                json!({ "statement": statement, "index": index })
            } else {
                json!({ "statement": statement })
            },
        }));
    }
    for index in &report.destructive_statements {
        let keyword = statement_keyword(report, *index);
        checks.push(json!({
            "rule_id": "destructive_ddl",
            "message_key": "check.destructive_ddl",
            "severity": "error",
            "detail": if multi {
                json!({ "keyword": keyword, "index": index })
            } else {
                json!({ "keyword": keyword })
            },
        }));
    }
    checks
}

/// Findings for the payload: a single statement keeps every hit in rule order
/// (historical payload); several statements collapse to one entry per
/// `rule_id` so a rule that fires in each statement is not repeated.
fn findings_payload(report: &SqlCheck) -> Vec<Finding> {
    if !report.is_multi_statement() {
        return report
            .statements
            .first()
            .map_or_else(Vec::new, |statement| statement.findings.clone());
    }
    let mut seen: Vec<&str> = Vec::new();
    let mut out: Vec<Finding> = Vec::new();
    for finding in report
        .statements
        .iter()
        .flat_map(|statement| statement.findings.iter())
    {
        if seen.contains(&finding.rule_id.as_str()) {
            continue;
        }
        seen.push(&finding.rule_id);
        out.push(finding.clone());
    }
    out
}

fn statement_keyword(report: &SqlCheck, index: usize) -> &str {
    report
        .statements
        .get(index.saturating_sub(1))
        .map_or("", |statement| statement.keyword.as_str())
}

fn class_name(class: EntryClass) -> &'static str {
    match class {
        EntryClass::Select => "QUERY",
        EntryClass::Insert | EntryClass::Update | EntryClass::Delete => "DML",
        EntryClass::Ddl => "DDL",
        EntryClass::Other | EntryClass::Error => "UNKNOWN",
    }
}
