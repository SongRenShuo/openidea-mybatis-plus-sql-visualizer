//! `sqlv_rollback_plan`: reverse plan for an UPDATE or DELETE.
//!
//! A plan without a primary key cannot locate the affected rows, so the core
//! refuses it; this handler returns that refusal as the shared failure envelope
//! (`confidence: unresolved`) and never emits SQL. A successful plan is
//! `partial` by nature: the reverse statements need before-image values that
//! only exist at execution time.

use serde::Deserialize;
use serde_json::json;

use crate::core::r#gen::{Confidence as GenConfidence, rollback_plan};
use crate::mcp::contract::{TableArg, gen_failure, invalid, parse_args, success, validate_table};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    sql: String,
    table: TableArg,
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
    if let Err(failure) = validate_table(&args.table) {
        return failure;
    }
    match rollback_plan(&args.sql, &args.table.to_spec(), &args.dialect) {
        Ok(plan) => {
            let confidence = match plan.confidence {
                GenConfidence::Exact => "exact",
                GenConfidence::Partial => "partial",
                GenConfidence::Unresolved => "unresolved",
            };
            success(confidence, json!(plan))
        }
        Err(error) => gen_failure(&error),
    }
}
