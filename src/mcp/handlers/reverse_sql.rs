//! `sqlv_reverse_sql`: SQL SELECT → MyBatis-Plus `LambdaQueryWrapper`.
//!
//! The core refuses unsupported shapes instead of guessing; that `unresolved`
//! result (with its message keys and `code: null`) is passed through as-is and
//! is not an error envelope: the tool did its job and reported a limitation.

use serde::Deserialize;
use serde_json::json;

use crate::core::r#gen::{Confidence as GenConfidence, to_mybatis_plus_java};
use crate::mcp::contract::{gen_failure, invalid, parse_args, success};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    sql: String,
    entity: String,
}

pub fn run(arguments: &str) -> String {
    let args = match parse_args::<Args>(arguments) {
        Ok(args) => args,
        Err(failure) => return failure,
    };
    if args.sql.trim().is_empty() {
        return invalid("sql must not be empty");
    }
    match to_mybatis_plus_java(&args.sql, &args.entity) {
        Ok(code) => {
            let confidence = match code.confidence {
                GenConfidence::Exact => "exact",
                GenConfidence::Partial => "partial",
                GenConfidence::Unresolved => "unresolved",
            };
            success(confidence, json!(code))
        }
        Err(error) => gen_failure(&error),
    }
}
