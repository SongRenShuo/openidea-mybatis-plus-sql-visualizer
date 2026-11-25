//! Acceptance tests for the MCP surface (T6-1~T6-3).
//!
//! All tests call [`call_tool`] directly; no `host::` is involved, so the cases
//! run as ordinary unit tests. Assertions are on the real response JSON.

use serde_json::{Value, json};

use super::{TOOLS, call_tool};

const PLUGIN_ID: &str = "songrenshuo_mybatis_plus_sql_visualizer";

const SCHEMAS: &[&str] = &[
    include_str!("schemas/sqlv_render_sql.schema.json"),
    include_str!("schemas/sqlv_render_logs.schema.json"),
    include_str!("schemas/sqlv_check_sql.schema.json"),
    include_str!("schemas/sqlv_perf_advice.schema.json"),
    include_str!("schemas/sqlv_crud_sql.schema.json"),
    include_str!("schemas/sqlv_rollback_plan.schema.json"),
    include_str!("schemas/sqlv_reverse_sql.schema.json"),
    include_str!("schemas/sqlv_mapper_check.schema.json"),
    include_str!("schemas/sqlv_history_search.schema.json"),
    include_str!("schemas/sqlv_schema_context.schema.json"),
    include_str!("schemas/sqlv_explain.schema.json"),
];

fn call(tool: &str, args: Value) -> Value {
    let text = call_tool(tool, &args.to_string()).expect("call_tool never returns Err");
    serde_json::from_str(&text).expect("every response is JSON")
}

fn expected_schema_path(tool: &str) -> String {
    format!("src/mcp/schemas/{tool}.schema.json")
}

const MAPPER: &str = r#"<mapper namespace="com.example.UserMapper">
    <select id="find" resultType="User">SELECT id FROM users WHERE id = #{id}</select>
</mapper>"#;

fn minimal_args(tool: &str) -> Value {
    match tool {
        "sqlv_render_sql" => json!({ "statement_id": "find", "xml": MAPPER }),
        "sqlv_render_logs" => json!({
            "text": "==>  Preparing: SELECT id FROM user WHERE id = ?\n==> Parameters: 1(Long)"
        }),
        "sqlv_check_sql" => json!({ "sql": "SELECT 1" }),
        "sqlv_perf_advice" => json!({ "sql": "SELECT id FROM user WHERE id = 1" }),
        "sqlv_crud_sql" => json!({
            "table": {
                "name": "user",
                "columns": [{ "name": "id", "primary_key": true }, { "name": "name" }]
            }
        }),
        "sqlv_rollback_plan" => json!({
            "sql": "DELETE FROM user WHERE id = ?",
            "table": { "name": "user", "columns": [{ "name": "id", "primary_key": true }] }
        }),
        "sqlv_reverse_sql" => {
            json!({ "sql": "SELECT id FROM user WHERE id = 1", "entity": "User" })
        }
        "sqlv_mapper_check" => json!({
            "documents": [{ "path": "UserMapper.xml", "text": MAPPER }]
        }),
        "sqlv_history_search" => json!({ "text": "SELECT 1\nSELECT 2", "query": "SELECT" }),
        "sqlv_schema_context" => json!({
            "schema": { "tables": [{ "name": "user", "columns": [{ "name": "id", "type": "bigint" }] }] }
        }),
        "sqlv_explain" => json!({ "result": [{ "id": 1, "table": "user" }] }),
        other => panic!("no minimal arguments for {other}"),
    }
}

// ---------------------------------------------------------------------------
// T6-1: schemas
// ---------------------------------------------------------------------------

#[test]
fn schemas_are_wellformed_self_contained_and_fit_the_name_budget() {
    assert_eq!(TOOLS.len(), 11);
    assert_eq!(SCHEMAS.len(), 11);
    for (spec, text) in TOOLS.iter().zip(SCHEMAS) {
        assert_eq!(spec.schema_path, expected_schema_path(spec.name));
        assert!(text.len() <= 256 << 10, "{} exceeds 256 KiB", spec.name);
        assert!(
            !text.contains("http://") && !text.contains("https://"),
            "{} must not reference the network",
            spec.name
        );

        let schema: Value = serde_json::from_str(text).expect("schema is valid JSON");
        assert_eq!(schema["type"], "object", "{}", spec.name);
        assert_eq!(schema["additionalProperties"], false, "{}", spec.name);
        assert!(
            schema["description"]
                .as_str()
                .is_some_and(|d| !d.is_empty()),
            "{} needs a description",
            spec.name
        );
        let properties = schema["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{} has an object properties map", spec.name));
        assert!(!properties.is_empty(), "{}", spec.name);
        for (property, definition) in properties {
            assert!(
                definition["description"]
                    .as_str()
                    .is_some_and(|d| !d.is_empty()),
                "{}.{property} needs a description",
                spec.name
            );
            assert!(
                definition.get("type").is_some(),
                "{}.{property} needs a type",
                spec.name
            );
        }
    }
}

#[test]
fn tool_names_match_the_local_name_rules() {
    let mut seen = std::collections::HashSet::new();
    for spec in TOOLS {
        assert!(seen.insert(spec.name), "duplicate tool name {}", spec.name);
        assert!(spec.name.starts_with("sqlv_"), "{} prefix", spec.name);
        assert!((1..=40).contains(&spec.name.len()), "{} length", spec.name);
        assert!(
            spec.name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
            "{} characters",
            spec.name
        );
        // Registered name = normalized plugin id + "__" + local name.
        let registered = format!("{PLUGIN_ID}__{}", spec.name);
        assert!(registered.len() <= 64, "{registered} exceeds 64 bytes");
        assert!(
            spec.name.len() <= 64 - PLUGIN_ID.len() - 2,
            "{} leaves room for the registered name",
            spec.name
        );
        assert!((1..=4096).contains(&spec.description.len()));
        assert!(spec.read_only, "{} must stay read-only", spec.name);
        assert!(!spec.destructive, "{} must not be destructive", spec.name);
        assert_eq!(spec.schema_path, expected_schema_path(spec.name));
    }
}

// ---------------------------------------------------------------------------
// T6-2: dispatch
// ---------------------------------------------------------------------------

#[test]
fn dispatch_routes_every_declared_tool() {
    for spec in TOOLS {
        let response = call(spec.name, minimal_args(spec.name));
        assert_eq!(response["ok"], true, "{} failed: {response}", spec.name);
        assert!(
            response["confidence"].is_string(),
            "{} must carry confidence",
            spec.name
        );
        assert!(response["data"].is_object() || response["data"].is_array());
    }
}

#[test]
fn unknown_tool_is_a_structured_error() {
    let response = call("sqlv_nope", json!({}));
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], "unknown_tool");
    assert_eq!(response["error"]["message_key"], "mcp.error.unknown_tool");
    assert_eq!(response["confidence"], "unresolved");
    assert_eq!(
        response["error"]["detail"]["available"]
            .as_array()
            .map(Vec::len),
        Some(11)
    );
}

// ---------------------------------------------------------------------------
// Argument validation (missing / wrong type / oversized / unknown field)
// ---------------------------------------------------------------------------

#[test]
fn invalid_arguments_are_readable_errors() {
    let cases: &[(&str, Value)] = &[
        ("sqlv_render_sql", json!({ "xml": MAPPER })),
        (
            "sqlv_render_sql",
            json!({ "statement_id": "s", "xml": MAPPER, "documents": [] }),
        ),
        ("sqlv_render_sql", json!({ "statement_id": "s" })),
        (
            "sqlv_render_sql",
            json!({ "statement_id": 1, "xml": MAPPER }),
        ),
        (
            "sqlv_render_logs",
            json!({ "text": "x", "options": "nope" }),
        ),
        (
            "sqlv_render_logs",
            json!({ "text": "x", "options": { "n_plus_one_threshold": 1 } }),
        ),
        ("sqlv_check_sql", json!({ "sql": "   " })),
        (
            "sqlv_check_sql",
            json!({ "sql": "SELECT 1", "dialect": "no-such-dialect" }),
        ),
        (
            "sqlv_history_search",
            json!({ "text": "a", "query": "a", "limit": 0 }),
        ),
        (
            "sqlv_history_search",
            json!({ "text": "a", "query": "a", "limit": 500 }),
        ),
        (
            "sqlv_crud_sql",
            json!({ "table": { "name": "t", "columns": "nope" } }),
        ),
        (
            "sqlv_crud_sql",
            json!({ "table": { "name": "t", "columns": [{ "name": "id" }], "extra": 1 } }),
        ),
        (
            "sqlv_perf_advice",
            json!({ "sql": "SELECT 1", "indexes": [{ "table": "t" }] }),
        ),
        ("sqlv_mapper_check", json!({ "documents": [] })),
        ("sqlv_schema_context", json!({ "schema": { "tables": [] } })),
        (
            "sqlv_rollback_plan",
            json!({ "sql": "DELETE FROM t", "table": { "name": "t" } }),
        ),
    ];
    for (tool, args) in cases {
        let response = call(tool, args.clone());
        assert_eq!(response["ok"], false, "{tool} {args}: {response}");
        assert_eq!(response["error"]["code"], "invalid_arguments", "{tool}");
        assert_eq!(response["confidence"], "unresolved");
        assert!(
            response["error"]["detail"]["reason"]
                .as_str()
                .is_some_and(|reason| !reason.is_empty()),
            "{tool} needs a readable reason: {response}"
        );
    }
}

#[test]
fn oversized_and_non_object_arguments_are_rejected_without_panicking() {
    let oversized = "x".repeat((1 << 20) + 1);
    let response: Value =
        serde_json::from_str(&call_tool("sqlv_check_sql", &oversized).unwrap()).unwrap();
    assert_eq!(response["error"]["code"], "invalid_arguments");
    assert!(
        response["error"]["detail"]["reason"]
            .as_str()
            .unwrap()
            .contains("exceed")
    );

    let response: Value =
        serde_json::from_str(&call_tool("sqlv_check_sql", "[]").unwrap()).unwrap();
    assert_eq!(response["error"]["code"], "invalid_arguments");
    assert_eq!(
        response["error"]["detail"]["reason"],
        "arguments must be a JSON object, got array"
    );
}

// ---------------------------------------------------------------------------
// T6-3: confidence passthrough (unresolved never carries SQL)
// ---------------------------------------------------------------------------

#[test]
fn render_sql_passes_exact_partial_and_unresolved_through() {
    let exact = call(
        "sqlv_render_sql",
        json!({ "statement_id": "find", "xml": MAPPER, "params": { "id": 7 } }),
    );
    assert_eq!(exact["confidence"], "exact");
    assert_eq!(exact["data"]["confidence"], "exact");
    assert_eq!(exact["data"]["sql"], "SELECT id FROM users WHERE id = ?");
    assert_eq!(exact["data"]["params"], json!([7]));

    let missing_include = r#"<mapper namespace="n"><select id="s">SELECT <include refid="nope"/> FROM t</select></mapper>"#;
    let partial = call(
        "sqlv_render_sql",
        json!({ "statement_id": "s", "xml": missing_include }),
    );
    assert_eq!(partial["confidence"], "partial");
    assert_eq!(partial["data"]["confidence"], "partial");
    assert!(
        partial["data"]["message_keys"]
            .as_array()
            .unwrap()
            .iter()
            .any(|key| key == "xml.include.not_found")
    );

    let unresolved = call(
        "sqlv_render_sql",
        json!({ "statement_id": "missing", "xml": MAPPER }),
    );
    assert_eq!(unresolved["confidence"], "unresolved");
    assert_eq!(unresolved["data"]["confidence"], "unresolved");
    assert_eq!(unresolved["data"]["sql"], "");
    assert_eq!(unresolved["data"]["params"], json!([]));
    assert_eq!(
        unresolved["data"]["message_keys"],
        json!(["xml.statement.not_found"])
    );
}

#[test]
fn multi_document_render_reports_load_issues() {
    let response = call(
        "sqlv_render_sql",
        json!({
            "statement_id": "a.s",
            "documents": [
                { "path": "a.xml", "text": "<mapper namespace=\"a\"><select id=\"s\">SELECT 1</select></mapper>" },
                { "path": "broken.xml", "text": "<mapper>" }
            ]
        }),
    );
    assert_eq!(response["ok"], true);
    let issues = response["issues"].as_array().unwrap();
    assert!(
        issues
            .iter()
            .any(|issue| issue["message_key"] == "xml.file.unparseable"),
        "{response}"
    );
}

#[test]
fn reverse_sql_passes_unresolved_through_without_code() {
    let response = call(
        "sqlv_reverse_sql",
        json!({ "sql": "DELETE FROM user WHERE id = 1", "entity": "User" }),
    );
    assert_eq!(response["ok"], true);
    assert_eq!(response["confidence"], "unresolved");
    assert_eq!(response["data"]["code"], Value::Null);
    assert_eq!(
        response["data"]["unresolved"][0]["message_key"],
        "gen.java.not_select"
    );

    let no_entity = call(
        "sqlv_reverse_sql",
        json!({ "sql": "SELECT id FROM user WHERE id = 1", "entity": "" }),
    );
    assert_eq!(no_entity["confidence"], "unresolved");
    assert_eq!(no_entity["data"]["code"], Value::Null);
    assert_eq!(
        no_entity["data"]["unresolved"][0]["message_key"],
        "gen.java.entity_required"
    );
}

#[test]
fn rollback_without_primary_key_is_unresolved_and_has_no_sql() {
    let text = call_tool(
        "sqlv_rollback_plan",
        &json!({
            "sql": "DELETE FROM user WHERE name = ?",
            "table": { "name": "user", "columns": [{ "name": "name" }] }
        })
        .to_string(),
    )
    .unwrap();
    let response: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(response["ok"], false);
    assert_eq!(response["confidence"], "unresolved");
    assert_eq!(response["error"]["code"], "generation_failed");
    assert_eq!(response["error"]["message_key"], "gen.error.no_primary_key");
    assert!(
        !text.contains("INSERT INTO"),
        "no reverse SQL on refusal: {text}"
    );
    assert!(
        !text.contains("UPDATE "),
        "no reverse SQL on refusal: {text}"
    );

    let planned = call(
        "sqlv_rollback_plan",
        json!({
            "sql": "DELETE FROM user WHERE id = ?",
            "table": { "name": "user", "columns": [{ "name": "id", "primary_key": true }, { "name": "name" }] }
        }),
    );
    assert_eq!(planned["ok"], true);
    assert_eq!(planned["confidence"], "partial");
    assert!(
        planned["data"]["statements"][0]
            .as_str()
            .unwrap()
            .contains("INSERT INTO")
    );
    assert_eq!(
        planned["data"]["notes"][0]["message_key"],
        "gen.rollback.before_image_required"
    );
}

#[test]
fn explain_without_a_result_is_unresolved_and_never_fabricates_one() {
    let response = call("sqlv_explain", json!({}));
    assert_eq!(response["ok"], true);
    assert_eq!(response["confidence"], "unresolved");
    assert_eq!(response["data"]["available"], false);
    assert_eq!(response["data"]["text"], "");
    assert_eq!(
        response["data"]["message_keys"],
        json!(["engine.explain.unavailable"])
    );
}

#[test]
fn render_logs_passes_entry_confidence_through() {
    let response = call(
        "sqlv_render_logs",
        json!({ "text": "java.sql.SQLException: Table 'demo.user' doesn't exist" }),
    );
    assert_eq!(response["ok"], true);
    assert_eq!(response["confidence"], "unresolved");
    assert_eq!(response["data"]["entries"][0]["confidence"], "unresolved");
    assert_eq!(response["data"]["entries"][0]["sql"], "");
    assert_eq!(response["data"]["errors"][0]["sql"], "");
}

// ---------------------------------------------------------------------------
// Behaviour: classification, logs, crud, schema, history, mapper checks
// ---------------------------------------------------------------------------

#[test]
fn check_sql_classifies_and_flags_unbounded_writes_and_destructive_ddl() {
    let update = call(
        "sqlv_check_sql",
        json!({ "sql": "UPDATE user SET name = 'x'" }),
    );
    assert_eq!(update["confidence"], "partial");
    assert_eq!(update["data"]["class"], "DML");
    assert_eq!(update["data"]["unbounded_write"], true);
    assert_eq!(update["data"]["destructive"], false);
    assert!(
        update["data"]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check["message_key"] == "check.unbounded_write")
    );

    let guarded = call(
        "sqlv_check_sql",
        json!({ "sql": "DELETE FROM user WHERE id = 1" }),
    );
    assert_eq!(guarded["data"]["class"], "DML");
    assert_eq!(guarded["data"]["unbounded_write"], false);

    let drop = call("sqlv_check_sql", json!({ "sql": "DROP TABLE user" }));
    assert_eq!(drop["data"]["class"], "DDL");
    assert_eq!(drop["data"]["destructive"], true);
    assert!(
        drop["data"]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check["message_key"] == "check.destructive_ddl")
    );

    let select = call("sqlv_check_sql", json!({ "sql": "SELECT id FROM user" }));
    assert_eq!(select["data"]["class"], "QUERY");
    assert_eq!(select["data"]["unbounded_write"], false);
}

#[test]
fn check_sql_checks_every_statement_of_a_multi_statement_text() {
    let response = call(
        "sqlv_check_sql",
        json!({
            "sql": "DELETE FROM users;\nSELECT id, name FROM users WHERE name LIKE '%abc' ORDER BY created_at DESC;"
        }),
    );
    assert_eq!(response["ok"], true);
    assert_eq!(response["confidence"], "partial");
    let data = &response["data"];
    assert_eq!(data["class"], "DML");
    assert_eq!(data["keyword"], "DELETE");
    assert_eq!(data["unbounded_write"], true);
    assert_eq!(data["destructive"], false);
    assert_eq!(data["evidence"]["unbounded_write"], json!([1]));
    assert_eq!(data["evidence"]["destructive"], json!([]));
    let statements = data["statements"].as_array().expect("statements");
    assert_eq!(statements.len(), 2);
    assert_eq!(statements[0]["keyword"], "DELETE");
    assert_eq!(statements[0]["unbounded_write"], true);
    assert_eq!(statements[1]["class"], "QUERY");
    assert_eq!(statements[1]["destructive"], false);

    let rules: Vec<&str> = data["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .map(|finding| finding["rule_id"].as_str().expect("rule id"))
        .collect();
    assert!(rules.contains(&"missing_where"));
    assert!(rules.contains(&"leading_wildcard_like"));
    let mut unique = rules.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(rules.len(), unique.len(), "findings must not repeat a rule");
    assert!(
        data["checks"]
            .as_array()
            .expect("checks")
            .iter()
            .any(|check| check["message_key"] == "check.unbounded_write"
                && check["detail"]["index"] == 1)
    );
}

#[test]
fn check_sql_single_statement_payload_is_unchanged() {
    let response = call(
        "sqlv_check_sql",
        json!({ "sql": "UPDATE user SET name = 'x'" }),
    );
    let data = &response["data"];
    assert_eq!(response["confidence"], "partial");
    assert_eq!(data["class"], "DML");
    assert_eq!(data["keyword"], "UPDATE");
    assert_eq!(data["unbounded_write"], true);
    assert_eq!(data["destructive"], false);
    assert_eq!(
        data["checks"][0]["detail"],
        json!({ "statement": "UPDATE" })
    );
    assert_eq!(data["findings"][0]["rule_id"], "missing_where");
    assert_eq!(data["statements"].as_array().map(Vec::len), Some(1));
    assert_eq!(data["evidence"]["unbounded_write"], json!([1]));
}

#[test]
fn perf_advice_scores_each_statement_and_merges_findings() {
    let response = call(
        "sqlv_perf_advice",
        json!({
            "sql": "DELETE FROM users;\nSELECT id FROM users WHERE name LIKE '%abc'"
        }),
    );
    assert_eq!(response["ok"], true);
    assert_eq!(response["confidence"], "partial");
    let data = &response["data"];
    assert_eq!(data["score"]["score"], 69);
    assert_eq!(data["score"]["level"], "medium");
    let statements = data["statements"].as_array().expect("statements");
    assert_eq!(statements.len(), 2);
    assert_eq!(statements[0]["score"], 53);
    assert_eq!(statements[0]["class"], "DML");
    // `tableCount` is summed per statement (3 + 3), never mixed.
    let table = data["score"]["breakdown"]
        .as_array()
        .expect("breakdown")
        .iter()
        .find(|part| part["factor"] == "tableCount")
        .expect("tableCount");
    assert_eq!(table["points"], 6);

    let rules: Vec<&str> = data["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .map(|finding| finding["rule_id"].as_str().expect("rule id"))
        .collect();
    assert!(rules.contains(&"missing_where"));
    assert!(rules.contains(&"leading_wildcard_like"));
    assert_eq!(rules.len(), 2, "one entry per rule");
}

#[test]
fn perf_advice_single_statement_payload_is_unchanged() {
    let response = call(
        "sqlv_perf_advice",
        json!({ "sql": "UPDATE user SET name = 'x'" }),
    );
    let data = &response["data"];
    assert_eq!(data["score"]["score"], 50);
    assert_eq!(data["score"]["level"], "medium");
    assert_eq!(data["findings"][0]["rule_id"], "missing_where");
    assert!(
        data.get("statements").is_none(),
        "single-statement payload keeps its historical fields"
    );
}

#[test]
fn render_logs_recovers_parameters() {
    let response = call(
        "sqlv_render_logs",
        json!({
            "text": "==>  Preparing: SELECT id, name FROM user WHERE id = ? AND status = ?\n==> Parameters: 1(Long), active(String)"
        }),
    );
    assert_eq!(response["confidence"], "exact");
    assert_eq!(
        response["data"]["entries"][0]["sql"],
        "SELECT id, name FROM user WHERE id = 1 AND status = 'active'"
    );
    assert_eq!(response["data"]["entries"][0]["family"], "mybatis");
}

#[test]
fn crud_generates_the_five_statements() {
    let response = call(
        "sqlv_crud_sql",
        json!({
            "table": {
                "name": "user",
                "schema": "app",
                "columns": [
                    { "name": "id", "primary_key": true, "auto_increment": true },
                    { "name": "name" }
                ]
            },
            "dialect": "mysql"
        }),
    );
    assert_eq!(response["confidence"], "exact");
    let statements = response["data"].as_array().unwrap();
    assert_eq!(statements.len(), 5);
    assert_eq!(statements[0]["kind"], "select_by_id");
    assert!(
        statements[0]["sql"]
            .as_str()
            .unwrap()
            .contains("FROM `app`.`user`")
    );
}

#[test]
fn schema_context_returns_compact_text() {
    let response = call(
        "sqlv_schema_context",
        json!({
            "schema": { "tables": [{
                "name": "user",
                "schema": "app",
                "columns": [
                    { "name": "id", "type": "bigint", "nullable": false, "primary_key": true, "auto_increment": true },
                    { "name": "name", "type": "varchar(64)", "nullable": true, "comment": "display" }
                ],
                "indexes": [{ "name": "idx_name", "columns": ["name"] }]
            }] }
        }),
    );
    assert_eq!(response["confidence"], "exact");
    assert_eq!(response["data"]["tables"], 1);
    assert_eq!(response["data"]["columns"], 2);
    assert_eq!(response["data"]["indexes"], 1);
    let text = response["data"]["text"].as_str().unwrap();
    assert!(text.contains("TABLE app.user (2 columns)"), "{text}");
    assert!(text.contains("id bigint NOT NULL PK AUTO"), "{text}");
    assert!(text.contains("INDEX idx_name (name)"), "{text}");
}

#[test]
fn history_search_enforces_the_limit() {
    let response = call(
        "sqlv_history_search",
        json!({
            "text": "SELECT 1\nSELECT 2\nSELECT 3\nSELECT 4\nSELECT 5",
            "query": "select",
            "limit": 2
        }),
    );
    assert_eq!(response["confidence"], "exact");
    assert_eq!(response["data"]["total_matches"], 5);
    assert_eq!(response["data"]["returned"], 2);
    assert_eq!(response["data"]["truncated"], true);
    let matches = response["data"]["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0]["line_no"], 1);
    assert_eq!(matches[1]["text"], "SELECT 2");
}

#[test]
fn mapper_check_reports_dangling_include_duplicate_id_and_missing_result_map() {
    let dangling = call(
        "sqlv_mapper_check",
        json!({
            "documents": [{
                "path": "UserMapper.xml",
                "text": "<mapper namespace=\"a\"><select id=\"s\">SELECT <include refid=\"nope\"/> FROM t</select></mapper>"
            }]
        }),
    );
    assert_eq!(dangling["confidence"], "partial");
    let issues = dangling["data"]["issues"].as_array().unwrap();
    assert!(
        issues
            .iter()
            .any(|issue| issue["message_key"] == "xml.include.not_found"
                && issue["detail"]["refid"] == "nope"),
        "{dangling}"
    );

    let duplicate = call(
        "sqlv_mapper_check",
        json!({
            "documents": [
                { "path": "a.xml", "text": "<mapper namespace=\"a\"><select id=\"s\">SELECT 1</select></mapper>" },
                { "path": "a2.xml", "text": "<mapper namespace=\"a\"><select id=\"s\">SELECT 2</select></mapper>" }
            ]
        }),
    );
    assert_eq!(duplicate["confidence"], "partial");
    assert!(
        duplicate["data"]["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue["message_key"] == "xml.statement.duplicate"),
        "{duplicate}"
    );

    let missing_map = call(
        "sqlv_mapper_check",
        json!({
            "documents": [{
                "path": "UserMapper.xml",
                "text": "<mapper namespace=\"a\"><select id=\"s\" resultMap=\"userMap\">SELECT 1</select></mapper>"
            }]
        }),
    );
    assert_eq!(missing_map["confidence"], "partial");
    assert!(
        missing_map["data"]["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(
                |issue| issue["message_key"] == "mapper.result_map.not_found"
                    && issue["detail"]["resultMap"] == "userMap"
            ),
        "{missing_map}"
    );
}

#[test]
fn explain_formats_a_supplied_result() {
    let table = call(
        "sqlv_explain",
        json!({ "result": [{ "id": 1, "table": "user", "type": "ALL" }] }),
    );
    assert_eq!(table["confidence"], "exact");
    let text = table["data"]["text"].as_str().unwrap();
    assert!(text.contains("id | table | type"), "{text}");
    assert!(text.contains("1 | user | ALL"), "{text}");

    let tree = call(
        "sqlv_explain",
        json!({
            "format": "tree",
            "result": {
                "Plan": {
                    "Node Type": "Nested Loop",
                    "Plans": [
                        { "Node Type": "Seq Scan", "Relation Name": "user" }
                    ]
                }
            }
        }),
    );
    assert_eq!(tree["confidence"], "exact");
    let text = tree["data"]["text"].as_str().unwrap();
    assert!(text.contains("Nested Loop"), "{text}");
    assert!(text.contains("  Seq Scan on user"), "{text}");
}
