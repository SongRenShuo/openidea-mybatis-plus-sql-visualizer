//! T1-12 parity harness: `cases/cases.jsonl` → `core::*` → assertions.
//!
//! One JSON object per line, one line per case. The runner:
//!
//! 1. parses every line and dispatches on `kind` (`xml`, `annotation`,
//!    `ibatis`, `params`, `dialect`, `perf`, `gen`, `log`);
//! 2. asserts only the fields the case declares under `expect`, so a case can
//!    pin SQL alone or SQL plus confidence plus parameter values;
//! 3. prints `cases: N, passed: N, whitelisted: M, failed: F`;
//! 4. reads `cases/whitelist.md` and excuses *only* the fields listed there —
//!    and fails when a whitelisted case starts passing, so a stale excuse
//!    cannot linger.
//!
//! SQL comparison is whitespace-insensitive (`split_whitespace` + join), the
//! same normalisation the IntelliJ reference tests call `compact()`. Every
//! other field is compared exactly.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use openidea_plugin_mybatis_plus_sql_visualizer::core::{
    annotation, dialect, r#gen, ibatis, log, params, perf, xml,
};

const CASES: &str = include_str!("../cases/cases.jsonl");
const WHITELIST: &str = include_str!("../cases/whitelist.md");

// ---------------------------------------------------------------------------
// Case model
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Case {
    id: String,
    kind: String,
    input: Value,
    expect: Value,
    line: usize,
}

fn load_cases() -> Vec<Case> {
    let mut out = Vec::new();
    for (index, raw) in CASES.lines().enumerate() {
        let line = index + 1;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with('#') {
            continue;
        }
        let value: Value = serde_json::from_str(trimmed)
            .unwrap_or_else(|e| panic!("cases.jsonl line {line}: invalid JSON: {e}\n{trimmed}"));
        let object = value
            .as_object()
            .unwrap_or_else(|| panic!("cases.jsonl line {line}: expected a JSON object"));
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("cases.jsonl line {line}: missing string `id`"))
            .to_string();
        let kind = object
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {id}: missing string `kind`"))
            .to_string();
        out.push(Case {
            id: id.clone(),
            kind,
            input: object.get("input").cloned().unwrap_or(Value::Null),
            expect: object
                .get("expect")
                .cloned()
                .unwrap_or_else(|| panic!("case {id}: missing `expect`")),
            line,
        });
    }
    out
}

/// Whitelist entry: the fields a documented difference covers, plus the human
/// reason (kept for the report, not asserted).
#[derive(Debug, Clone)]
struct Exemption {
    fields: Vec<String>,
    reason: String,
}

fn load_whitelist() -> BTreeMap<String, Exemption> {
    let mut out = BTreeMap::new();
    for raw in WHITELIST.lines() {
        let line = raw.trim();
        if !line.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        if cells.len() < 3 {
            continue;
        }
        let id = cells[0];
        if !id.contains('-') || id == "id" || id.chars().all(|c| c == '-' || c == ':') {
            continue;
        }
        let fields = cells[1]
            .split(',')
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .map(str::to_string)
            .collect();
        out.insert(
            id.to_string(),
            Exemption {
                fields,
                reason: cells[2].to_string(),
            },
        );
    }
    out
}

// ---------------------------------------------------------------------------
// Comparison helpers
// ---------------------------------------------------------------------------

/// Whitespace-insensitive form used for SQL text (reference `compact`).
fn compact(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn diff_sql(diffs: &mut Vec<String>, field: &str, expected: &str, actual: &str) {
    let want = compact(expected);
    let got = compact(actual);
    if want != got {
        diffs.push(format!(
            "{field}: expected {want:?}\n           actual   {got:?}"
        ));
    }
}

fn diff_exact<T: PartialEq + std::fmt::Debug + ?Sized>(
    diffs: &mut Vec<String>,
    field: &str,
    expected: &T,
    actual: &T,
) {
    if expected != actual {
        diffs.push(format!("{field}: expected {expected:?}, actual {actual:?}"));
    }
}

fn diff_confidence(diffs: &mut Vec<String>, expected: &Value, actual: &str) {
    if let Some(want) = expected.as_str()
        && want != actual
    {
        diffs.push(format!("confidence: expected {want:?}, actual {actual:?}"));
    }
}

fn missing(field: &str, case: &str) -> String {
    format!("case {case}: input.{field} is missing or has the wrong type")
}

fn check_json(diffs: &mut Vec<String>, field: &str, expected: &Value, actual: &Value) {
    if expected != actual {
        diffs.push(format!(
            "{field}: expected {}\n           actual   {}",
            pretty(expected),
            pretty(actual)
        ));
    }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<unprintable>".to_string())
}

fn contains_keys(diffs: &mut Vec<String>, expected: &Value, actual: &[String]) {
    if let Some(arr) = expected.as_array() {
        for key in arr {
            if let Some(key) = key.as_str()
                && !actual.iter().any(|k| k == key)
            {
                diffs.push(format!(
                    "message_keys: expected to contain {key:?}, actual {actual:?}"
                ));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

fn evaluate(case: &Case) -> Result<Vec<String>, String> {
    match case.kind.as_str() {
        "xml" => xml_case(case),
        "annotation" => annotation_case(case),
        "ibatis" => ibatis_case(case),
        "params" => params_case(case),
        "dialect" => dialect_case(case),
        "perf" => perf_case(case),
        "gen" => gen_case(case),
        "log" => log_case(case),
        other => Err(format!("case {}: unknown kind {other:?}", case.id)),
    }
}

fn xml_confidence(value: xml::Confidence) -> &'static str {
    match value {
        xml::Confidence::Exact => "exact",
        xml::Confidence::Partial => "partial",
        xml::Confidence::Unresolved => "unresolved",
    }
}

fn xml_case(case: &Case) -> Result<Vec<String>, String> {
    let statement = case
        .input
        .get("statement")
        .and_then(Value::as_str)
        .ok_or_else(|| missing("statement", &case.id))?;
    let parameters = case.input.get("params").cloned().unwrap_or(Value::Null);
    let output = if let Some(docs) = case.input.get("docs").and_then(Value::as_array) {
        let mut owned: Vec<(String, String)> = Vec::new();
        for entry in docs {
            let pair = entry
                .as_array()
                .filter(|p| p.len() == 2)
                .ok_or_else(|| format!("case {}: docs entries must be [path, xml]", case.id))?;
            let path = pair[0].as_str().unwrap_or_default().to_string();
            let text = pair[1].as_str().unwrap_or_default().to_string();
            owned.push((path, text));
        }
        let refs: Vec<(&str, &str)> = owned
            .iter()
            .map(|(p, x)| (p.as_str(), x.as_str()))
            .collect();
        let (set, _issues) = xml::MapperSet::load(&refs);
        set.render(statement, &parameters)
    } else {
        let text = case
            .input
            .get("xml")
            .and_then(Value::as_str)
            .ok_or_else(|| missing("xml", &case.id))?;
        xml::render_statement(text, statement, &parameters)
    };

    let mut diffs = Vec::new();
    if let Some(expected) = case.expect.get("sql") {
        diff_sql(
            &mut diffs,
            "sql",
            expected.as_str().unwrap_or_default(),
            &output.sql,
        );
    }
    if let Some(expected) = case.expect.get("confidence") {
        diff_confidence(&mut diffs, expected, xml_confidence(output.confidence));
    }
    if let Some(expected) = case.expect.get("params") {
        check_json(&mut diffs, "params", expected, &json!(output.params));
    }
    if let Some(expected) = case.expect.get("keys_contains") {
        contains_keys(&mut diffs, expected, &output.message_keys);
    }
    if case.expect.get("keys_empty") == Some(&Value::Bool(true)) && !output.message_keys.is_empty()
    {
        diffs.push(format!(
            "message_keys: expected empty, actual {:?}",
            output.message_keys
        ));
    }
    Ok(diffs)
}

fn annotation_case(case: &Case) -> Result<Vec<String>, String> {
    let java = case
        .input
        .get("java")
        .and_then(Value::as_str)
        .ok_or_else(|| missing("java", &case.id))?;
    let statements = annotation::parse_annotations(java);
    if let Some(expected) = case.expect.get("count") {
        let mut diffs = Vec::new();
        check_json(&mut diffs, "count", expected, &json!(statements.len()));
        return Ok(diffs);
    }
    let statement = if let Some(method) = case.input.get("method").and_then(Value::as_str) {
        statements
            .iter()
            .find(|s| s.method_name.as_deref() == Some(method))
            .ok_or_else(|| format!("case {}: no annotation for method {method:?}", case.id))?
    } else {
        let index = case.input.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        statements
            .get(index)
            .ok_or_else(|| format!("case {}: no annotation at index {index}", case.id))?
    };

    let mut diffs = Vec::new();
    if let Some(expected) = case.expect.get("kind") {
        diff_exact(
            &mut diffs,
            "kind",
            expected.as_str().unwrap_or_default(),
            statement.kind.as_str(),
        );
    }
    if let Some(expected) = case.expect.get("is_script") {
        diff_exact(
            &mut diffs,
            "is_script",
            &expected.as_bool().unwrap_or(false),
            &statement.is_script,
        );
    }
    if let Some(expected) = case.expect.get("method_line") {
        check_json(
            &mut diffs,
            "method_line",
            expected,
            &json!(statement.method_line),
        );
    }
    let parameters = case.input.get("params").cloned().unwrap_or(Value::Null);
    let output = annotation::render_annotated(statement, &parameters);
    if let Some(expected) = case.expect.get("sql") {
        diff_sql(
            &mut diffs,
            "sql",
            expected.as_str().unwrap_or_default(),
            &output.sql,
        );
    }
    if let Some(expected) = case.expect.get("confidence") {
        diff_confidence(&mut diffs, expected, xml_confidence(output.confidence));
    }
    if let Some(expected) = case.expect.get("keys_contains") {
        contains_keys(&mut diffs, expected, &output.message_keys);
    }
    Ok(diffs)
}

fn ibatis_case(case: &Case) -> Result<Vec<String>, String> {
    let text = case
        .input
        .get("xml")
        .and_then(Value::as_str)
        .ok_or_else(|| missing("xml", &case.id))?;
    let statement = case
        .input
        .get("statement")
        .and_then(Value::as_str)
        .ok_or_else(|| missing("statement", &case.id))?;
    let parameters = case.input.get("params").cloned().unwrap_or(Value::Null);
    let output = ibatis::render(text, statement, &parameters);

    let mut diffs = Vec::new();
    if let Some(expected) = case.expect.get("sql") {
        diff_sql(
            &mut diffs,
            "sql",
            expected.as_str().unwrap_or_default(),
            &output.sql,
        );
    }
    if let Some(expected) = case.expect.get("confidence") {
        diff_confidence(&mut diffs, expected, xml_confidence(output.confidence));
    }
    if let Some(expected) = case.expect.get("params") {
        check_json(&mut diffs, "params", expected, &json!(output.params));
    }
    if let Some(expected) = case.expect.get("keys_contains") {
        contains_keys(&mut diffs, expected, &output.message_keys);
    }
    Ok(diffs)
}

fn params_case(case: &Case) -> Result<Vec<String>, String> {
    let sql = case
        .input
        .get("sql")
        .and_then(Value::as_str)
        .ok_or_else(|| missing("sql", &case.id))?;
    let dialect_name = case
        .input
        .get("dialect")
        .and_then(Value::as_str)
        .ok_or_else(|| missing("dialect", &case.id))?;
    let profile = dialect::by_name(dialect_name)
        .ok_or_else(|| format!("case {}: unknown dialect {dialect_name:?}", case.id))?;
    let values: Vec<Value> = case
        .input
        .get("params")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let bound = params::bind(sql, &values, profile);

    let mut diffs = Vec::new();
    if let Some(expected) = case.expect.get("sql") {
        // `bind` keeps the caller's text; an exact comparison is meaningful.
        diff_exact(
            &mut diffs,
            "sql",
            expected.as_str().unwrap_or_default(),
            &bound.sql,
        );
    }
    if let Some(expected) = case.expect.get("confidence") {
        let actual = match bound.confidence {
            params::Confidence::Exact => "exact",
            params::Confidence::Partial => "partial",
            params::Confidence::Unresolved => "unresolved",
        };
        diff_confidence(&mut diffs, expected, actual);
    }
    if let Some(expected) = case.expect.get("params") {
        check_json(&mut diffs, "params", expected, &json!(bound.params));
    }
    if let Some(expected) = case.expect.get("keys_contains") {
        contains_keys(&mut diffs, expected, &bound.message_keys);
    }
    Ok(diffs)
}

fn string_array(input: &Value) -> Option<Vec<String>> {
    input.as_array().map(|items| {
        items
            .iter()
            .map(|v| v.as_str().unwrap_or_default().to_string())
            .collect()
    })
}

fn dialect_case(case: &Case) -> Result<Vec<String>, String> {
    let dialect_name = case
        .input
        .get("dialect")
        .and_then(Value::as_str)
        .ok_or_else(|| missing("dialect", &case.id))?;
    let expected = case
        .expect
        .get("result")
        .cloned()
        .ok_or_else(|| format!("case {}: expect.result is required", case.id))?;
    let operation = case
        .input
        .get("op")
        .and_then(Value::as_str)
        .ok_or_else(|| missing("op", &case.id))?;

    let actual = match operation {
        "exists" => json!(dialect::by_name(dialect_name).is_some()),
        _ => {
            let profile = dialect::by_name(dialect_name)
                .ok_or_else(|| format!("case {}: unknown dialect {dialect_name:?}", case.id))?;
            match operation {
                "limit" => {
                    let limit = case
                        .input
                        .get("limit")
                        .and_then(Value::as_u64)
                        .ok_or_else(|| missing("limit", &case.id))?;
                    let offset = case.input.get("offset").and_then(Value::as_u64);
                    json!(profile.render_limit(limit, offset))
                }
                "quote_identifier" => {
                    let ident = case
                        .input
                        .get("ident")
                        .and_then(Value::as_str)
                        .ok_or_else(|| missing("ident", &case.id))?;
                    json!(profile.quote_identifier(ident))
                }
                "quote_if_keyword" => {
                    let ident = case
                        .input
                        .get("ident")
                        .and_then(Value::as_str)
                        .ok_or_else(|| missing("ident", &case.id))?;
                    json!(profile.quote_if_keyword(ident))
                }
                "quote_if_needed" => {
                    let ident = case
                        .input
                        .get("ident")
                        .and_then(Value::as_str)
                        .ok_or_else(|| missing("ident", &case.id))?;
                    let source = match case
                        .input
                        .get("source")
                        .and_then(Value::as_str)
                        .unwrap_or("entity")
                    {
                        "catalog" => dialect::IdentifierCaseSource::Catalog,
                        "entity" => dialect::IdentifierCaseSource::Entity,
                        other => {
                            return Err(format!("case {}: unknown source {other:?}", case.id));
                        }
                    };
                    json!(profile.quote_if_needed(ident, source))
                }
                "upsert" => {
                    let key = case
                        .input
                        .get("conflict_key")
                        .and_then(Value::as_str)
                        .ok_or_else(|| missing("conflict_key", &case.id))?;
                    let columns =
                        string_array(case.input.get("update_columns").unwrap_or(&Value::Null))
                            .unwrap_or_default();
                    let refs: Vec<&str> = columns.iter().map(String::as_str).collect();
                    json!(profile.render_upsert(key, &refs))
                }
                "upsert_statement" => {
                    let table = case
                        .input
                        .get("table")
                        .and_then(Value::as_str)
                        .ok_or_else(|| missing("table", &case.id))?;
                    let key = case
                        .input
                        .get("conflict_key")
                        .and_then(Value::as_str)
                        .ok_or_else(|| missing("conflict_key", &case.id))?;
                    let columns = string_array(case.input.get("columns").unwrap_or(&Value::Null))
                        .unwrap_or_default();
                    let refs: Vec<&str> = columns.iter().map(String::as_str).collect();
                    json!(profile.render_upsert_statement(table, &refs, key))
                }
                "identity_wrap" => {
                    let table = case
                        .input
                        .get("table")
                        .and_then(Value::as_str)
                        .ok_or_else(|| missing("table", &case.id))?;
                    let sql = case
                        .input
                        .get("sql")
                        .and_then(Value::as_str)
                        .ok_or_else(|| missing("sql", &case.id))?;
                    json!(profile.render_identity_wrapper(table, sql))
                }
                other => return Err(format!("case {}: unknown op {other:?}", case.id)),
            }
        }
    };

    let mut diffs = Vec::new();
    check_json(&mut diffs, "result", &expected, &actual);
    Ok(diffs)
}

fn severity_str(value: perf::Severity) -> &'static str {
    match value {
        perf::Severity::Error => "error",
        perf::Severity::Warn => "warn",
        perf::Severity::Info => "info",
    }
}

fn perf_case(case: &Case) -> Result<Vec<String>, String> {
    let sql = case
        .input
        .get("sql")
        .and_then(Value::as_str)
        .ok_or_else(|| missing("sql", &case.id))?;
    let dialect_name = case
        .input
        .get("dialect")
        .and_then(Value::as_str)
        .ok_or_else(|| missing("dialect", &case.id))?;
    let findings = perf::analyze(sql, dialect_name);

    let mut diffs = Vec::new();
    let rule_ids: Vec<String> = findings.iter().map(|f| f.rule_id.clone()).collect();
    if let Some(expected) = case.expect.get("rule_ids") {
        check_json(&mut diffs, "rule_ids", expected, &json!(rule_ids));
    }
    if let Some(expected) = case.expect.get("rule_ids_contains")
        && let Some(arr) = expected.as_array()
    {
        for rule in arr {
            let rule = rule.as_str().unwrap_or_default();
            if !rule_ids.iter().any(|r| r == rule) {
                diffs.push(format!(
                    "rule_ids: expected to contain {rule:?}, actual {rule_ids:?}"
                ));
            }
        }
    }
    if let Some(expected) = case.expect.get("rule_ids_absent")
        && let Some(arr) = expected.as_array()
    {
        for rule in arr {
            let rule = rule.as_str().unwrap_or_default();
            if rule_ids.iter().any(|r| r == rule) {
                diffs.push(format!("rule_ids: expected NOT to contain {rule:?}"));
            }
        }
    }
    if let Some(expected) = case.expect.get("severities")
        && let Some(map) = expected.as_object()
    {
        for (rule, want) in map {
            match findings.iter().find(|f| &f.rule_id == rule) {
                Some(f) => {
                    let actual = severity_str(f.severity);
                    if Some(actual) != want.as_str() {
                        diffs.push(format!(
                            "severities[{rule}]: expected {want}, actual {actual:?}"
                        ));
                    }
                }
                None => diffs.push(format!("severities[{rule}]: rule did not fire")),
            }
        }
    }
    Ok(diffs)
}

// ---------------------------------------------------------------------------
// Generators
// ---------------------------------------------------------------------------

fn table_spec(value: &Value) -> Result<r#gen::TableSpec, String> {
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "table.name is missing".to_string())?;
    let mut spec = r#gen::TableSpec::new(name, Vec::new());
    if let Some(schema) = value.get("schema").and_then(Value::as_str) {
        spec = spec.with_schema(schema);
    }
    let columns = value
        .get("columns")
        .and_then(Value::as_array)
        .ok_or_else(|| "table.columns is missing".to_string())?;
    for column in columns {
        let column_name = column
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| "column.name is missing".to_string())?;
        let mut entry = r#gen::ColumnSpec::new(column_name);
        if column.get("primary_key").and_then(Value::as_bool) == Some(true) {
            entry = entry.primary_key();
        }
        if column.get("auto_increment").and_then(Value::as_bool) == Some(true) {
            entry = entry.auto_increment();
        }
        spec.columns.push(entry);
    }
    Ok(spec)
}

fn gen_failure(error: &r#gen::GenError) -> Value {
    match error {
        r#gen::GenError::NoPrimaryKey => json!("gen.error.no_primary_key"),
        r#gen::GenError::NoColumns => json!("gen.error.no_columns"),
        r#gen::GenError::EmptySql => json!("gen.error.empty_sql"),
        r#gen::GenError::UnknownDialect(_) => json!("gen.error.unknown_dialect"),
        r#gen::GenError::Unresolved { message_key, .. } => json!(message_key),
    }
}

fn gen_confidence(value: r#gen::Confidence) -> &'static str {
    match value {
        r#gen::Confidence::Exact => "exact",
        r#gen::Confidence::Partial => "partial",
        r#gen::Confidence::Unresolved => "unresolved",
    }
}

/// CRUD statements may be pinned either as a whole array (exact) or as a list
/// of `{"kind": …, "sql": …, "params": …}` entries matched by kind.
fn check_statements(diffs: &mut Vec<String>, expected: &Value, actual: &[r#gen::CrudStatement]) {
    let Some(items) = expected.as_array() else {
        return;
    };
    let by_kind = items.iter().all(|item| item.get("kind").is_some());
    if !by_kind {
        check_json(diffs, "statements", expected, &json!(actual));
        return;
    }
    for want in items {
        let kind = want.get("kind").and_then(Value::as_str).unwrap_or_default();
        let Some(found) = actual.iter().find(|s| {
            serde_json::to_value(s.kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .as_deref()
                == Some(kind)
        }) else {
            diffs.push(format!("statements[{kind}]: statement not generated"));
            continue;
        };
        if let Some(sql) = want.get("sql") {
            diff_sql(
                diffs,
                &format!("statements[{kind}].sql"),
                sql.as_str().unwrap_or_default(),
                &found.sql,
            );
        }
        if let Some(params) = want.get("params") {
            check_json(
                diffs,
                &format!("statements[{kind}].params"),
                params,
                &json!(found.params),
            );
        }
    }
}

fn gen_case(case: &Case) -> Result<Vec<String>, String> {
    let operation = case
        .input
        .get("op")
        .and_then(Value::as_str)
        .ok_or_else(|| missing("op", &case.id))?;
    let mut diffs = Vec::new();

    match operation {
        "crud" | "crud_batch" => {
            let table = case
                .input
                .get("table")
                .ok_or_else(|| format!("case {}: input.table is required", case.id))?;
            let spec = table_spec(table)?;
            let dialect_name = case
                .input
                .get("dialect")
                .and_then(Value::as_str)
                .ok_or_else(|| missing("dialect", &case.id))?;
            let result = if operation == "crud_batch" {
                let rows = case
                    .input
                    .get("batch_rows")
                    .and_then(Value::as_u64)
                    .unwrap_or(2) as usize;
                r#gen::crud_batch(&spec, dialect_name, rows)
            } else {
                r#gen::crud(&spec, dialect_name)
            };
            match result {
                Ok(statements) => {
                    if let Some(expected) = case.expect.get("statements") {
                        check_statements(&mut diffs, expected, &statements);
                    } else if let Some(expected) = case.expect.get("statement_count") {
                        check_json(
                            &mut diffs,
                            "statement_count",
                            expected,
                            &json!(statements.len()),
                        );
                    }
                }
                Err(error) => {
                    if let Some(expected) = case.expect.get("error") {
                        check_json(&mut diffs, "error", expected, &gen_failure(&error));
                    } else {
                        diffs.push(format!("unexpected generator error: {error}"));
                    }
                }
            }
        }
        "java" => {
            let sql = case
                .input
                .get("sql")
                .and_then(Value::as_str)
                .ok_or_else(|| missing("sql", &case.id))?;
            let entity = case
                .input
                .get("entity")
                .and_then(Value::as_str)
                .unwrap_or_default();
            match r#gen::to_mybatis_plus_java(sql, entity) {
                Ok(code) => {
                    if let Some(expected) = case.expect.get("confidence") {
                        diff_confidence(&mut diffs, expected, gen_confidence(code.confidence));
                    }
                    if let Some(expected) = case.expect.get("code") {
                        check_json(&mut diffs, "code", expected, &json!(code.code));
                    }
                    if let Some(expected) = case.expect.get("params") {
                        check_json(&mut diffs, "params", expected, &json!(code.params));
                    }
                    if let Some(expected) = case.expect.get("unresolved_key") {
                        let keys: Vec<String> = code
                            .unresolved
                            .iter()
                            .map(|u| u.message_key.clone())
                            .collect();
                        contains_keys(&mut diffs, expected, &keys);
                    }
                }
                Err(error) => {
                    check_json(
                        &mut diffs,
                        "error",
                        &case.expect["error"],
                        &gen_failure(&error),
                    );
                }
            }
        }
        "rollback" => {
            let sql = case
                .input
                .get("sql")
                .and_then(Value::as_str)
                .ok_or_else(|| missing("sql", &case.id))?;
            let table = case
                .input
                .get("table")
                .ok_or_else(|| format!("case {}: input.table is required", case.id))?;
            let spec = table_spec(table)?;
            let dialect_name = case
                .input
                .get("dialect")
                .and_then(Value::as_str)
                .ok_or_else(|| missing("dialect", &case.id))?;
            match r#gen::rollback_plan(sql, &spec, dialect_name) {
                Ok(plan) => {
                    for field in ["statements", "before_image_columns", "primary_key"] {
                        if let Some(expected) = case.expect.get(field) {
                            let actual = match field {
                                "statements" => json!(plan.statements),
                                "before_image_columns" => json!(plan.before_image_columns),
                                _ => json!(plan.primary_key),
                            };
                            check_json(&mut diffs, field, expected, &actual);
                        }
                    }
                    if let Some(expected) = case.expect.get("forward_where") {
                        check_json(
                            &mut diffs,
                            "forward_where",
                            expected,
                            &json!(plan.forward_where),
                        );
                    }
                    if let Some(expected) = case.expect.get("confidence") {
                        diff_confidence(&mut diffs, expected, gen_confidence(plan.confidence));
                    }
                }
                Err(error) => {
                    check_json(
                        &mut diffs,
                        "error",
                        &case.expect["error"],
                        &gen_failure(&error),
                    );
                }
            }
        }
        other => return Err(format!("case {}: unknown gen op {other:?}", case.id)),
    }

    Ok(diffs)
}

// ---------------------------------------------------------------------------
// Log analysis
// ---------------------------------------------------------------------------

fn log_family(value: log::Family) -> &'static str {
    value.as_str()
}

fn log_class(value: log::EntryClass) -> &'static str {
    match value {
        log::EntryClass::Select => "select",
        log::EntryClass::Insert => "insert",
        log::EntryClass::Update => "update",
        log::EntryClass::Delete => "delete",
        log::EntryClass::Ddl => "ddl",
        log::EntryClass::Other => "other",
        log::EntryClass::Error => "error",
    }
}

fn log_confidence(value: log::Confidence) -> &'static str {
    match value {
        log::Confidence::Exact => "exact",
        log::Confidence::Partial => "partial",
        log::Confidence::Unresolved => "unresolved",
    }
}

fn log_case(case: &Case) -> Result<Vec<String>, String> {
    let text = case
        .input
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| missing("text", &case.id))?;
    let options: log::ParseOptions = match case.input.get("options") {
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|e| format!("case {}: invalid options: {e}", case.id))?,
        None => log::ParseOptions::default(),
    };
    let analysis = log::parse(text, &options);

    let mut diffs = Vec::new();
    for (field, actual) in [
        ("entry_count", analysis.entries.len()),
        ("duplicate_count", analysis.duplicates.len()),
        ("n_plus_one_count", analysis.n_plus_one.len()),
        ("slow_count", analysis.slow.len()),
        ("errors_count", analysis.errors.len()),
        ("unparsed_count", analysis.unparsed_lines.len()),
    ] {
        if let Some(expected) = case.expect.get(field) {
            check_json(&mut diffs, field, expected, &json!(actual));
        }
    }

    if let Some(entries) = case.expect.get("entries").and_then(Value::as_array) {
        for want in entries {
            let index = want.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            let Some(entry) = analysis.entries.get(index) else {
                diffs.push(format!(
                    "entries[{index}]: missing (only {} entries)",
                    analysis.entries.len()
                ));
                continue;
            };
            if let Some(v) = want.get("family") {
                diff_exact(
                    &mut diffs,
                    "family",
                    v.as_str().unwrap_or_default(),
                    log_family(entry.family),
                );
            }
            if let Some(v) = want.get("class") {
                diff_exact(
                    &mut diffs,
                    "class",
                    v.as_str().unwrap_or_default(),
                    log_class(entry.class),
                );
            }
            if let Some(v) = want.get("confidence") {
                diff_confidence(&mut diffs, v, log_confidence(entry.confidence));
            }
            if let Some(v) = want.get("sql") {
                diff_sql(
                    &mut diffs,
                    "sql",
                    v.as_str().unwrap_or_default(),
                    &entry.sql,
                );
            }
            if let Some(v) = want.get("raw_sql") {
                diff_sql(
                    &mut diffs,
                    "raw_sql",
                    v.as_str().unwrap_or_default(),
                    &entry.raw_sql,
                );
            }
            if let Some(v) = want.get("params_count") {
                check_json(&mut diffs, "params_count", v, &json!(entry.params.len()));
            }
            if let Some(v) = want.get("params") {
                check_json(&mut diffs, "params", v, &json!(entry.params));
            }
            if let Some(v) = want.get("affected_rows") {
                check_json(&mut diffs, "affected_rows", v, &json!(entry.affected_rows));
            }
            if let Some(v) = want.get("result_rows") {
                check_json(&mut diffs, "result_rows", v, &json!(entry.result_rows));
            }
            if let Some(v) = want.get("duration_ms") {
                check_json(&mut diffs, "duration_ms", v, &json!(entry.duration_ms));
            }
            if let Some(v) = want.get("keys_contains") {
                contains_keys(&mut diffs, v, &entry.message_keys);
            }
        }
    }

    if let Some(want) = case.expect.get("duplicates") {
        let actual = json!(analysis.duplicates);
        if let Some(expected_count) = want.get("count") {
            if let Some(first) = analysis.duplicates.first() {
                check_json(
                    &mut diffs,
                    "duplicates[0].count",
                    expected_count,
                    &json!(first.count),
                );
            } else {
                diffs.push("duplicates: expected a group, found none".to_string());
            }
        }
        if let Some(expected) = want.get("first") {
            let first = analysis.duplicates.first().map(|g| {
                json!({
                    "sql": g.sql,
                    "params": g.params,
                    "count": g.count,
                })
            });
            check_json(
                &mut diffs,
                "duplicates[0]",
                expected,
                &first.unwrap_or(Value::Null),
            );
        }
        if want.get("none") == Some(&Value::Bool(true)) && !analysis.duplicates.is_empty() {
            diffs.push(format!("duplicates: expected none, actual {actual}"));
        }
    }

    if let Some(want) = case.expect.get("n_plus_one") {
        if let Some(first) = analysis.n_plus_one.first() {
            if let Some(expected) = want.get("count") {
                check_json(
                    &mut diffs,
                    "n_plus_one[0].count",
                    expected,
                    &json!(first.count),
                );
            }
            if let Some(expected) = want.get("template") {
                check_json(
                    &mut diffs,
                    "n_plus_one[0].template",
                    expected,
                    &json!(first.template),
                );
            }
        } else if want.get("count").is_some() {
            diffs.push("n_plus_one: expected a group, found none".to_string());
        }
    }

    if let Some(want) = case.expect.get("unparsed")
        && let Some(expected) = want.get("keys")
    {
        let keys: Vec<String> = analysis
            .unparsed_lines
            .iter()
            .map(|u| u.message_key.clone())
            .collect();
        check_json(&mut diffs, "unparsed.keys", expected, &json!(keys));
    }

    Ok(diffs)
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

#[test]
fn cases_parity() {
    let cases = load_cases();
    assert!(
        cases.len() >= 100,
        "cases.jsonl must hold at least 100 cases, found {}",
        cases.len()
    );

    let exempt = load_whitelist();
    let known: std::collections::BTreeSet<String> = cases.iter().map(|c| c.id.clone()).collect();
    for id in exempt.keys() {
        assert!(
            known.contains(id),
            "whitelist.md mentions unknown case {id:?}"
        );
    }

    let mut passed = 0usize;
    let mut whitelisted = 0usize;
    let mut failures: Vec<String> = Vec::new();
    let mut kind_counts: BTreeMap<String, usize> = BTreeMap::new();

    for case in &cases {
        *kind_counts.entry(case.kind.clone()).or_insert(0) += 1;
        let diffs = match evaluate(case) {
            Ok(diffs) => diffs,
            Err(message) => {
                failures.push(format!("{} (line {}): {message}", case.id, case.line));
                continue;
            }
        };

        match exempt.get(&case.id) {
            Some(entry) => {
                if diffs.is_empty() {
                    failures.push(format!(
                        "{}: whitelist entry is stale — the case now passes; remove it from cases/whitelist.md",
                        case.id
                    ));
                    continue;
                }
                let mut unexpected: Vec<String> = Vec::new();
                for diff in &diffs {
                    let field = diff.split(':').next().unwrap_or("");
                    if entry.fields.iter().any(|f| f == field || f == "all") {
                        continue;
                    }
                    unexpected.push(diff.clone());
                }
                if unexpected.is_empty() {
                    whitelisted += 1;
                    eprintln!(
                        "whitelisted {} ({}): {}",
                        case.id,
                        entry.fields.join(","),
                        entry.reason
                    );
                } else {
                    failures.push(format!(
                        "{} (line {}): un-whitelisted differences:\n    {}",
                        case.id,
                        case.line,
                        unexpected.join("\n    ")
                    ));
                }
            }
            None => {
                if diffs.is_empty() {
                    passed += 1;
                } else {
                    failures.push(format!(
                        "{} (line {}):\n    {}",
                        case.id,
                        case.line,
                        diffs.join("\n    ")
                    ));
                }
            }
        }
    }

    let mut kinds = String::new();
    for (kind, count) in &kind_counts {
        kinds.push_str(&format!(" {kind}={count}"));
    }
    println!(
        "cases: {}, passed: {}, whitelisted: {}, failed: {}{}",
        cases.len(),
        passed,
        whitelisted,
        failures.len(),
        format_args!(" | kinds:{kinds}")
    );

    assert!(
        failures.is_empty(),
        "{} parity case(s) failed:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// Guard: a whitelist must not become a dumping ground. The task requires the
/// mechanism to be exercised, not merely present.
#[test]
fn whitelist_is_exercised_and_documented() {
    let exempt = load_whitelist();
    assert!(
        exempt.len() >= 5,
        "expected at least 5 documented differences to prove the whitelist works, found {}",
        exempt.len()
    );
    for (id, entry) in &exempt {
        assert!(
            !entry.reason.trim().is_empty(),
            "whitelist entry {id} has no reason"
        );
    }
}

/// Guard: case ids are unique and each kind has coverage.
#[test]
fn case_inventory_is_sane() {
    let cases = load_cases();
    let mut seen = std::collections::BTreeSet::new();
    for case in &cases {
        assert!(
            seen.insert(case.id.clone()),
            "duplicate case id {:?}",
            case.id
        );
    }
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for case in &cases {
        *counts.entry(case.kind.as_str()).or_insert(0) += 1;
    }
    for kind in [
        "xml",
        "annotation",
        "ibatis",
        "params",
        "dialect",
        "perf",
        "gen",
        "log",
    ] {
        assert!(
            counts.get(kind).copied().unwrap_or(0) > 0,
            "no cases for kind {kind:?}"
        );
    }
}
