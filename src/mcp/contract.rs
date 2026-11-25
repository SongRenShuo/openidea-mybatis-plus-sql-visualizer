//! Shared contract for the MCP tools (T6-1~T6-3).
//!
//! One place for the tool declarations, the success/error envelopes and the
//! argument validation every handler goes through. Handlers themselves are
//! plain functions over `&str` → `String`; nothing here touches `host::`, so
//! the whole surface is unit-testable without an IDE.
//!
//! Envelopes:
//! - success: `{"ok":true,"confidence":"exact|partial|unresolved","data":…}`;
//! - failure: `{"ok":false,"error":{"code","message_key","detail"},
//!   "confidence":"unresolved"}`.
//!
//! `partial` is never reported as `exact`, and any `unresolved` result carries
//! no SQL (each handler also enforces that where the core already guarantees
//! it).

use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::core::r#gen::{ColumnSpec, GenError, TableSpec};

/// Upper bound for the raw argument text, mirroring the host limit.
pub(crate) const MAX_ARGUMENT_BYTES: usize = 1 << 20;

pub(crate) const CODE_INVALID_ARGUMENTS: &str = "invalid_arguments";
pub(crate) const CODE_UNKNOWN_TOOL: &str = "unknown_tool";
pub(crate) const CODE_GENERATION_FAILED: &str = "generation_failed";

pub(crate) const KEY_INVALID_ARGUMENTS: &str = "mcp.error.invalid_arguments";
pub(crate) const KEY_UNKNOWN_TOOL: &str = "mcp.error.unknown_tool";

/// One declared tool. `schema_path` is package-relative and is what
/// `plugin.toml` lists as `inputSchema`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub schema_path: &'static str,
    pub read_only: bool,
    pub destructive: bool,
}

/// Success envelope.
pub(crate) fn success(confidence: &str, data: Value) -> String {
    success_with(confidence, data, &[])
}

/// Success envelope with extra top-level fields (load issues, message keys).
pub(crate) fn success_with(confidence: &str, data: Value, extra: &[(&str, Value)]) -> String {
    let mut envelope = Map::new();
    envelope.insert("ok".into(), Value::Bool(true));
    envelope.insert("confidence".into(), Value::String(confidence.to_owned()));
    envelope.insert("data".into(), data);
    for (key, value) in extra {
        envelope.insert((*key).to_owned(), value.clone());
    }
    Value::Object(envelope).to_string()
}

/// Failure envelope (the shared error contract).
pub(crate) fn failure(code: &str, message_key: &str, detail: Value) -> String {
    json!({
        "ok": false,
        "error": {
            "code": code,
            "message_key": message_key,
            "detail": detail,
        },
        "confidence": "unresolved",
    })
    .to_string()
}

/// Argument error with a human-readable reason.
pub(crate) fn invalid(reason: impl Into<String>) -> String {
    failure(
        CODE_INVALID_ARGUMENTS,
        KEY_INVALID_ARGUMENTS,
        json!({ "reason": reason.into() }),
    )
}

/// Parses and validates the arguments object for one tool.
///
/// The host validates against `inputSchema` first; this second layer keeps the
/// same guarantees for direct calls and gives an error agents can act on.
/// Missing required fields, wrong types, unknown fields and oversized input all
/// become `Ok(Err(failure_json))` — never a panic.
pub(crate) fn parse_args<T: DeserializeOwned>(arguments: &str) -> Result<T, String> {
    if arguments.len() > MAX_ARGUMENT_BYTES {
        return Err(invalid(format!(
            "arguments exceed {MAX_ARGUMENT_BYTES} bytes (got {})",
            arguments.len()
        )));
    }
    let value: Value = serde_json::from_str(arguments)
        .map_err(|e| invalid(format!("arguments are not valid JSON: {e}")))?;
    if !value.is_object() {
        return Err(invalid(format!(
            "arguments must be a JSON object, got {}",
            json_type(&value)
        )));
    }
    serde_json::from_value(value)
        .map_err(|e| invalid(format!("arguments do not match the tool schema: {e}")))
}

/// Maps a generator failure onto the shared error contract.
pub(crate) fn gen_failure(err: &GenError) -> String {
    match err {
        GenError::NoPrimaryKey => failure(
            CODE_GENERATION_FAILED,
            "gen.error.no_primary_key",
            json!({}),
        ),
        GenError::NoColumns => failure(CODE_GENERATION_FAILED, "gen.error.no_columns", json!({})),
        GenError::EmptySql => failure(CODE_GENERATION_FAILED, "gen.error.empty_sql", json!({})),
        GenError::UnknownDialect(name) => failure(
            CODE_GENERATION_FAILED,
            "gen.error.unknown_dialect",
            json!({ "dialect": name }),
        ),
        GenError::Unresolved {
            message_key,
            detail,
        } => failure(CODE_GENERATION_FAILED, message_key, detail.clone()),
    }
}

/// JSON type name for diagnostics (stable, lowercase).
pub(crate) fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Table description as accepted by `sqlv_crud_sql` / `sqlv_rollback_plan`.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TableArg {
    pub name: String,
    #[serde(default)]
    pub schema: Option<String>,
    pub columns: Vec<ColumnArg>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ColumnArg {
    pub name: String,
    #[serde(default)]
    pub primary_key: bool,
    #[serde(default)]
    pub auto_increment: bool,
}

impl TableArg {
    pub(crate) fn to_spec(&self) -> TableSpec {
        let mut spec = TableSpec::new(
            self.name.clone(),
            self.columns
                .iter()
                .map(|column| ColumnSpec {
                    name: column.name.clone(),
                    primary_key: column.primary_key,
                    auto_increment: column.auto_increment,
                })
                .collect(),
        );
        if let Some(schema) = &self.schema {
            spec = spec.with_schema(schema.clone());
        }
        spec
    }
}

/// Structural validation the core would otherwise fail on with a less specific
/// message. Returns a ready-made failure envelope.
pub(crate) fn validate_table(table: &TableArg) -> Result<(), String> {
    if table.name.trim().is_empty() {
        return Err(invalid("table.name must not be empty"));
    }
    if table.columns.is_empty() {
        return Err(invalid("table.columns must not be empty"));
    }
    if table.columns.iter().any(|c| c.name.trim().is_empty()) {
        return Err(invalid("every table column needs a non-empty name"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Probe {
        value: String,
    }

    #[test]
    fn parse_args_reports_missing_required_field() {
        let err = parse_args::<Probe>("{}").unwrap_err();
        let value: Value = serde_json::from_str(&err).unwrap();
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["code"], CODE_INVALID_ARGUMENTS);
        assert!(
            value["error"]["detail"]["reason"]
                .as_str()
                .unwrap()
                .contains("value")
        );
    }

    #[test]
    fn parse_args_rejects_non_object_and_unknown_fields() {
        assert!(parse_args::<Probe>("[]").is_err());
        assert!(parse_args::<Probe>(r#"{"value":"x","extra":1}"#).is_err());
        let parsed = parse_args::<Probe>(r#"{"value":"x"}"#).unwrap();
        assert_eq!(parsed.value, "x");
    }

    #[test]
    fn parse_args_rejects_oversized_input() {
        let big = format!(r#"{{"value":"{}"}}"#, "x".repeat(MAX_ARGUMENT_BYTES));
        let err = parse_args::<Probe>(&big).unwrap_err();
        assert!(err.contains("exceed"));
    }

    #[test]
    fn table_arg_maps_to_core_spec() {
        let table = TableArg {
            name: "user".into(),
            schema: Some("app".into()),
            columns: vec![ColumnArg {
                name: "id".into(),
                primary_key: true,
                auto_increment: true,
            }],
        };
        let spec = table.to_spec();
        assert_eq!(spec.name, "user");
        assert_eq!(spec.schema.as_deref(), Some("app"));
        assert!(spec.columns[0].primary_key);
        assert!(spec.columns[0].auto_increment);
    }
}
