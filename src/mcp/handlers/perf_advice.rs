//! `sqlv_perf_advice`: rules, complexity score and index advice.
//!
//! The rules run on the SQL text alone (no statistics, no catalog), so the
//! result is `partial` whenever the statement is recognised; a text that does
//! not look like SQL yields `unresolved` with an empty score breakdown instead
//! of a misleading number.
//!
//! `;`-separated statements are analysed one by one: findings and index advice
//! are merged (no rule repeated), and the score is the sum of the statements'
//! own scores so no breakdown factor counts a token stream that mixes
//! statements. A single statement returns exactly the historical payload.

use serde::Deserialize;
use serde_json::json;

use crate::core::log::EntryClass;
use crate::core::perf::dialect::profile;
use crate::core::perf::{
    Finding, IndexAdvice, IndexDef, PerfAdvice, StatementAdvice, advise, index_advice,
};
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
    let report = advise(&args.sql, &args.dialect);
    let findings = findings_payload(&report);
    let advice = statement_advice(&report.statements, &indexes);
    let confidence = if report.complexity.recognized {
        "partial"
    } else {
        "unresolved"
    };
    let mut data = json!({
        "dialect": args.dialect,
        "findings": findings,
        "score": report.complexity,
        "advice": advice,
    });
    if report.is_multi_statement() {
        data["statements"] = json!(
            report
                .statements
                .iter()
                .map(|statement| json!({
                    "index": statement.index,
                    "class": class_bucket(statement.class),
                    "keyword": statement.keyword,
                    "score": statement.complexity.score,
                    "findings": statement.findings.len(),
                }))
                .collect::<Vec<_>>()
        );
    }
    success(confidence, data)
}

/// Findings for the payload: a single statement keeps every hit in rule order
/// (historical payload); several statements collapse to one entry per
/// `rule_id` so a rule that fires in each statement is not repeated.
fn findings_payload(report: &PerfAdvice) -> Vec<Finding> {
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

/// Index advice per statement, deduplicated across statements.
fn statement_advice(statements: &[StatementAdvice], indexes: &[IndexDef]) -> Vec<IndexAdvice> {
    let mut out: Vec<IndexAdvice> = Vec::new();
    for advice in statements
        .iter()
        .flat_map(|statement| index_advice(&statement.sql, indexes))
    {
        let duplicate = out.iter().any(|held| {
            held.table.eq_ignore_ascii_case(&advice.table)
                && held.columns == advice.columns
                && held.reason_key == advice.reason_key
        });
        if !duplicate {
            out.push(advice);
        }
    }
    out
}

fn class_bucket(class: EntryClass) -> &'static str {
    match class {
        EntryClass::Select => "QUERY",
        EntryClass::Insert | EntryClass::Update | EntryClass::Delete => "DML",
        EntryClass::Ddl => "DDL",
        EntryClass::Other | EntryClass::Error => "UNKNOWN",
    }
}
