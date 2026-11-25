//! `sqlv_crud_sql`: generate the standard CRUD statements for a table.
//!
//! Generation is deterministic over the supplied table description, so the
//! result is `exact`; structural problems (no primary key, no columns, unknown
//! dialect) come back as the shared failure envelope with the core message key.

use serde::Deserialize;
use serde_json::json;

use crate::core::r#gen::crud;
use crate::mcp::contract::{TableArg, gen_failure, invalid, parse_args, success, validate_table};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
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
    if let Err(failure) = validate_table(&args.table) {
        return failure;
    }
    if args.dialect.trim().is_empty() {
        return invalid("dialect must not be empty");
    }
    match crud(&args.table.to_spec(), &args.dialect) {
        Ok(statements) => success("exact", json!(statements)),
        Err(error) => gen_failure(&error),
    }
}
