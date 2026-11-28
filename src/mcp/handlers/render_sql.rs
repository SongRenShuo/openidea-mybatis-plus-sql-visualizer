//! `sqlv_render_sql`: render one statement to SQL.
//!
//! Four sources, selected by `kind`:
//!
//! | `kind` | input | renderer |
//! | --- | --- | --- |
//! | `mapper` (default) | `statement_id` + `xml` or `documents` | `core::xml` |
//! | `annotation` | `text` (Java source) + optional `method` | `core::annotation` |
//! | `ibatis` | `statement_id` + `xml` (a `<sqlMap>` document) | `core::ibatis` |
//! | `wrapper` | `text` (a wrapper chain) + `entity` mapping | `analyze::wrapper` |
//!
//! The output is the core [`RenderOutput`] unchanged, whatever the source, so
//! an agent reads the same `sql` / `params` / `confidence` / `notes` /
//! `message_keys` shape everywhere. `unresolved` results are re-checked to carry
//! no SQL.
//!
//! # Why the wrapper kind needs `entity`
//!
//! Rendering a wrapper chain requires knowing which Java field is which column.
//! The preview command reads that from the entity class in the project; an MCP
//! call has no project read here (no handler in this module touches `host::`,
//! which is what keeps every case a unit test), so the caller supplies the
//! mapping. Without it the result is `unresolved` (`wrapper.entity.not_found`),
//! never a guessed table or column.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::analyze::EntityColumns;
use crate::analyze::sources;
use crate::core::dialect::{self, Dialect};
use crate::core::xml::{Confidence, MapperSet, RenderOutput, render_statement};
use crate::mcp::contract::{invalid, parse_args, success, success_with};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    path: String,
    text: String,
}

/// The field→column projection `kind = "wrapper"` renders against.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntityArg {
    /// The entity class name, for the render's diagnostics; not used to resolve
    /// anything.
    #[serde(default)]
    class: Option<String>,
    table: String,
    /// Java field name → column name (`userName` → `user_name`).
    columns: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    /// `mapper` | `annotation` | `ibatis` | `wrapper`; absent means `mapper`.
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    statement_id: Option<String>,
    #[serde(default)]
    xml: Option<String>,
    #[serde(default)]
    documents: Option<Vec<Document>>,
    #[serde(default)]
    params: Option<Value>,
    /// Java source (`annotation`) or the wrapper chain (`wrapper`).
    #[serde(default)]
    text: Option<String>,
    /// The annotated method to render; required when the source has several.
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    entity: Option<EntityArg>,
    /// Dialect for wrapper rendering; `mysql` when absent.
    #[serde(default)]
    dialect: Option<String>,
}

pub fn run(arguments: &str) -> String {
    let args = match parse_args::<Args>(arguments) {
        Ok(args) => args,
        Err(failure) => return failure,
    };
    // Borrowed then cloned: `args` stays whole for the per-kind helpers.
    let params = match args.params.as_ref() {
        None | Some(Value::Null) => json!({}),
        Some(value) => value.clone(),
    };
    let kind = args
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|kind| !kind.is_empty())
        .unwrap_or("mapper")
        .to_ascii_lowercase();
    match kind.as_str() {
        "mapper" => mapper(&args, &params),
        "annotation" => annotation(&args, &params),
        "ibatis" => ibatis(&args, &params),
        "wrapper" => wrapper(&args),
        other => invalid(format!(
            "unknown kind `{other}`; use mapper, annotation, ibatis or wrapper"
        )),
    }
}

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

fn mapper(args: &Args, params: &Value) -> String {
    let Some(statement_id) = args
        .statement_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return invalid("kind=mapper needs statement_id");
    };
    match (&args.xml, &args.documents) {
        (Some(_), Some(_)) => invalid("provide exactly one of xml or documents"),
        (None, None) => invalid("provide one of xml or documents"),
        (Some(xml), None) => {
            let output = render_statement(xml, statement_id, params);
            success(confidence_str(output.confidence), render_value(output))
        }
        (None, Some(documents)) => {
            if documents.iter().any(|doc| doc.path.trim().is_empty()) {
                return invalid("every document needs a non-empty path");
            }
            let docs: Vec<(&str, &str)> = documents
                .iter()
                .map(|doc| (doc.path.as_str(), doc.text.as_str()))
                .collect();
            let (set, issues) = MapperSet::load(&docs);
            let output = set.render(statement_id, params);
            success_with(
                confidence_str(output.confidence),
                render_value(output),
                &[("issues", json!(issues))],
            )
        }
    }
}

fn annotation(args: &Args, params: &Value) -> String {
    let Some(java) = args.text.as_deref() else {
        return invalid("kind=annotation needs text (Java source)");
    };
    let statements = sources::annotation_statements(java);
    match sources::annotation_pick(java, &statements, args.method.as_deref(), None, "<text>") {
        Ok(statement) => {
            let output = sources::render_annotation(statement, params);
            success_with(
                confidence_str(output.confidence),
                render_value(output),
                &[
                    ("method", json!(statement.method_name)),
                    ("line", json!(statement.line)),
                ],
            )
        }
        Err(output) => success(confidence_str(output.confidence), render_value(output)),
    }
}

fn ibatis(args: &Args, params: &Value) -> String {
    let Some(xml) = args.xml.as_deref() else {
        return invalid("kind=ibatis needs xml (one <sqlMap> document)");
    };
    if args.documents.is_some() {
        return invalid("kind=ibatis takes one xml document, not documents");
    }
    let Some(requested) = args
        .statement_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return invalid("kind=ibatis needs statement_id");
    };
    let ids = match sources::ibatis_statement_ids(xml) {
        Ok(ids) => ids,
        Err(output) => return success(confidence_str(output.confidence), render_value(output)),
    };
    let Some(id) = sources::ibatis_resolve_id(&ids, requested) else {
        let output = RenderOutput::unresolved(
            crate::core::ibatis::KEY_STATEMENT_NOT_FOUND,
            format!(
                "no statement `{requested}`; the document has: {}",
                ids.join(", ")
            ),
        );
        return success(confidence_str(output.confidence), render_value(output));
    };
    let output = crate::core::ibatis::render(xml, &id, params);
    success(confidence_str(output.confidence), render_value(output))
}

fn wrapper(args: &Args) -> String {
    let Some(text) = args.text.as_deref() else {
        return invalid("kind=wrapper needs text (the wrapper chain)");
    };
    let Some(entity) = args.entity.as_ref() else {
        let output = sources::entity_unknown(
            "<text>",
            sources::wrapper_entity_class(text).as_deref(),
            "has no field/column mapping in this call; pass `entity`",
        );
        return success(confidence_str(output.confidence), render_value(output));
    };
    if entity.table.trim().is_empty() {
        return invalid("entity.table must not be empty");
    }
    if entity.columns.is_empty() {
        return invalid("entity.columns must map at least one field");
    }
    if entity
        .columns
        .iter()
        .any(|(field, column)| field.trim().is_empty() || column.trim().is_empty())
    {
        return invalid("entity.columns needs non-empty field and column names");
    }
    let dialect_name = args.dialect.as_deref().unwrap_or("mysql");
    let Some(dialect) = resolve_dialect(dialect_name) else {
        return invalid(format!("unsupported dialect: {dialect_name}"));
    };
    let class = entity.class.as_deref().unwrap_or_default();
    let mut columns = EntityColumns::new(class, entity.table.trim());
    for (field, column) in &entity.columns {
        columns = columns.with_column(field, column, "");
    }
    // Every mapping was declared by the caller, so nothing is derived and an
    // exact chain stays exact.
    let output = sources::wrapper_render(text, &columns, false, dialect);
    success(confidence_str(output.confidence), render_value(output))
}

fn resolve_dialect(name: &str) -> Option<&'static Dialect> {
    dialect::by_name(name).or_else(|| {
        dialect::all()
            .iter()
            .find(|known| known.name.eq_ignore_ascii_case(name))
    })
}

fn confidence_str(confidence: Confidence) -> &'static str {
    match confidence {
        Confidence::Exact => "exact",
        Confidence::Partial => "partial",
        Confidence::Unresolved => "unresolved",
    }
}

/// The core already keeps `unresolved` SQL empty; re-check so a future core
/// change cannot leak SQL through the MCP surface.
fn render_value(mut output: RenderOutput) -> Value {
    if output.confidence == Confidence::Unresolved {
        output.sql.clear();
        output.params.clear();
    }
    serde_json::to_value(&output).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(args: Value) -> Value {
        serde_json::from_str(&run(&args.to_string())).expect("valid JSON")
    }

    const JAVA: &str = r#"public interface UserMapper {
    @Select("SELECT id, name FROM users WHERE id = #{id}")
    User findById(@Param("id") Long id);

    @Update("UPDATE users SET name = #{name} WHERE id = #{id}")
    int rename(@Param("name") String name, @Param("id") Long id);
}"#;

    const SQLMAP: &str = r#"<sqlMap namespace="User">
    <select id="selectById">SELECT * FROM users WHERE id = #id#</select>
</sqlMap>"#;

    #[test]
    fn mapper_stays_the_default_and_keeps_the_old_behaviour() {
        let response = call(json!({
            "statement_id": "find",
            "xml": "<mapper namespace=\"a\"><select id=\"find\">SELECT ?</select></mapper>"
        }));
        assert_eq!(response["ok"], true);
        assert_eq!(response["confidence"], "exact");
        assert_eq!(response["data"]["sql"], "SELECT ?");
    }

    #[test]
    fn annotation_kind_renders_the_named_method() {
        let response = call(json!({
            "kind": "annotation",
            "text": JAVA,
            "method": "rename",
            "params": {"name": "Ada", "id": 9}
        }));
        assert_eq!(response["ok"], true, "{response}");
        assert_eq!(response["confidence"], "exact");
        assert_eq!(
            response["data"]["sql"],
            "UPDATE users SET name = ? WHERE id = ?"
        );
        assert_eq!(response["data"]["params"], json!(["Ada", 9]));
        assert_eq!(response["method"], "rename");
    }

    #[test]
    fn annotation_kind_without_a_method_is_unresolved_when_several_exist() {
        let response = call(json!({ "kind": "annotation", "text": JAVA }));
        assert_eq!(response["ok"], true);
        assert_eq!(response["confidence"], "unresolved");
        assert_eq!(response["data"]["sql"], "");
        assert_eq!(
            response["data"]["message_keys"],
            json!(["annotation.method.required"])
        );
    }

    #[test]
    fn annotation_kind_keeps_a_script_body() {
        let java = r#"interface M {
            @Select("<script>SELECT * FROM users <where><if test='name != null'>name = #{name}</if></where></script>")
            java.util.List<User> search(@Param("name") String name);
        }"#;
        let response = call(json!({
            "kind": "annotation",
            "text": java,
            "params": {"name": "Ada"}
        }));
        assert_eq!(response["confidence"], "exact");
        assert_eq!(
            response["data"]["sql"],
            "SELECT * FROM users  WHERE name = ?"
        );
    }

    #[test]
    fn ibatis_kind_renders_a_sqlmap_statement() {
        let response = call(json!({
            "kind": "ibatis",
            "statement_id": "selectById",
            "xml": SQLMAP,
            "params": {"id": 3}
        }));
        assert_eq!(response["ok"], true, "{response}");
        assert_eq!(response["confidence"], "exact");
        assert_eq!(response["data"]["sql"], "SELECT * FROM users WHERE id = ?");
        assert_eq!(response["data"]["params"], json!([3]));
    }

    #[test]
    fn ibatis_kind_reports_an_unknown_statement_as_unresolved() {
        let response = call(json!({
            "kind": "ibatis",
            "statement_id": "nope",
            "xml": SQLMAP
        }));
        assert_eq!(response["confidence"], "unresolved");
        assert_eq!(response["data"]["sql"], "");
        assert_eq!(
            response["data"]["message_keys"],
            json!(["ibatis.statement.not_found"])
        );
    }

    #[test]
    fn wrapper_kind_renders_with_the_supplied_columns() {
        let response = call(json!({
            "kind": "wrapper",
            "text": "new LambdaQueryWrapper<User>().eq(User::getStatus, \"A\")",
            "entity": {
                "class": "User",
                "table": "sys_user",
                "columns": {"status": "status", "age": "age"}
            }
        }));
        assert_eq!(response["ok"], true, "{response}");
        assert_eq!(response["confidence"], "exact");
        assert_eq!(
            response["data"]["sql"],
            "SELECT * FROM sys_user
WHERE status = 'A'"
        );
    }

    #[test]
    fn wrapper_kind_without_an_entity_is_unresolved_and_names_the_class() {
        let response = call(json!({
            "kind": "wrapper",
            "text": "new LambdaQueryWrapper<User>().eq(User::getStatus, \"A\")"
        }));
        assert_eq!(response["confidence"], "unresolved");
        assert_eq!(response["data"]["sql"], "");
        assert!(
            response["data"]["notes"][0]["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("`User`")),
            "{response}"
        );
    }

    #[test]
    fn wrapper_kind_rejects_an_unknown_dialect_and_an_empty_mapping() {
        let bad_dialect = call(json!({
            "kind": "wrapper",
            "text": "new LambdaQueryWrapper<User>()",
            "entity": {"table": "t", "columns": {"id": "id"}},
            "dialect": "nope"
        }));
        assert_eq!(bad_dialect["ok"], false);
        assert_eq!(bad_dialect["error"]["code"], "invalid_arguments");

        let empty = call(json!({
            "kind": "wrapper",
            "text": "new LambdaQueryWrapper<User>()",
            "entity": {"table": "t", "columns": {}}
        }));
        assert_eq!(empty["ok"], false);
    }

    #[test]
    fn an_unknown_kind_is_an_argument_error() {
        let response = call(json!({ "kind": "joins", "text": "x" }));
        assert_eq!(response["ok"], false);
        assert!(
            response["error"]["detail"]["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("unknown kind")),
            "{response}"
        );
    }

    #[test]
    fn each_kind_requires_its_own_inputs() {
        for args in [
            json!({ "kind": "mapper" }),
            json!({ "kind": "annotation" }),
            json!({ "kind": "ibatis", "xml": SQLMAP }),
            json!({ "kind": "ibatis", "statement_id": "s" }),
            json!({ "kind": "wrapper" }),
        ] {
            let response = call(args.clone());
            assert_eq!(response["ok"], false, "{args} -> {response}");
            assert_eq!(response["error"]["code"], "invalid_arguments", "{args}");
        }
    }
}
