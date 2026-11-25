//! `generate-crud`: standard CRUD statements from a table description.
//!
//! The argument is JSON: `table`, `schema` (optional), `columns`, `pk`
//! (optional list of primary-key names) and `dialect`. A column is either a
//! name or `{ "name", "primary_key", "auto_increment" }`. All generation is
//! [`crate::core::r#gen::crud`]; this module formats its statements.

use std::fmt::Write as _;

use openidea_plugin::{Error, Invocation};
use serde::Deserialize;

use crate::commands::common::{self, CliError, ColumnInput};
use crate::core::r#gen::{CrudKind, CrudStatement, crud};

/// Matches `[[contributes.commands]] id` in `plugin.toml`.
pub const ID: &str = "generate-crud";

/// What the command accepts as `arg`.
pub const USAGE: &str = r#"{"table":"user","columns":["id","name"],"pk":["id"],"dialect":"mysql"}"#;

#[derive(Debug, Clone, Deserialize)]
struct GenerateCrudInput {
    table: String,
    #[serde(default)]
    schema: Option<String>,
    columns: Vec<ColumnInput>,
    #[serde(default)]
    pk: Vec<String>,
    dialect: String,
}

/// Dispatch entry.
pub fn run(_ctx: &Invocation, arg: Option<&str>) -> Result<Option<String>, Error> {
    if arg.map(str::trim).unwrap_or("").is_empty() {
        return common::fail(common::missing_arg(USAGE));
    }
    match generate(arg) {
        Ok(text) => {
            common::emit(&text)?;
            Ok(None)
        }
        Err(err) => common::fail(err),
    }
}

fn generate(arg: Option<&str>) -> Result<String, CliError> {
    let input: GenerateCrudInput = common::json_arg(arg)?;
    let spec = common::build_table_spec(
        &input.table,
        input.schema.as_deref(),
        &input.columns,
        &input.pk,
    );
    let statements = crud(&spec, &input.dialect).map_err(|err| common::gen_error(&err))?;

    let mut text = String::new();
    let _ = writeln!(
        text,
        "-- generate-crud: {} ({})",
        input.table, input.dialect
    );
    for statement in &statements {
        append_statement(&mut text, statement);
    }
    Ok(text)
}

fn append_statement(text: &mut String, statement: &CrudStatement) {
    let _ = writeln!(text, "\n-- {}", kind_label(statement.kind));
    text.push_str(&statement.sql);
    text.push('\n');
    if !statement.params.is_empty() {
        let _ = writeln!(text, "params: {}", statement.params.join(", "));
    }
}

fn kind_label(kind: CrudKind) -> &'static str {
    match kind {
        CrudKind::SelectById => "select_by_id",
        CrudKind::Insert => "insert",
        CrudKind::UpdateById => "update_by_id",
        CrudKind::DeleteById => "delete_by_id",
        CrudKind::BatchInsert => "batch_insert",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    fn text(host: &MockHost) -> String {
        host.output()[common::CHANNEL].clone()
    }

    #[test]
    fn generates_the_five_statements_for_mysql() {
        let host = MockHost::install();
        run(
            &host.invocation(),
            Some(
                r#"{"table":"user","columns":[
                    {"name":"id","primary_key":true,"auto_increment":true},
                    "name","status"
                ],"dialect":"mysql"}"#,
            ),
        )
        .unwrap();
        let text = text(&host);
        assert!(text.contains("-- select_by_id"));
        assert!(text.contains("SELECT `id`, `name`, `status` FROM `user` WHERE `id` = ?"));
        assert!(text.contains("INSERT INTO `user` (`name`, `status`) VALUES (?, ?)"));
        assert!(text.contains("UPDATE `user` SET `name` = ?, `status` = ? WHERE `id` = ?"));
        assert!(text.contains("DELETE FROM `user` WHERE `id` = ?"));
        assert!(text.contains("params: name, status, id"));
    }

    #[test]
    fn pk_names_mark_columns_by_name() {
        let host = MockHost::install();
        run(
            &host.invocation(),
            Some(r#"{"table":"user","columns":["id","name"],"pk":["ID"],"dialect":"postgresql"}"#),
        )
        .unwrap();
        let text = text(&host);
        assert!(text.contains("DELETE FROM \"user\" WHERE \"id\" = ?"));
    }

    #[test]
    fn an_unknown_dialect_is_refused_with_a_key() {
        let host = MockHost::install();
        let err = run(
            &host.invocation(),
            Some(r#"{"table":"user","columns":["id"],"pk":["id"],"dialect":"nope"}"#),
        )
        .unwrap_err();
        assert!(
            matches!(err, Error::Failed(ref text) if text.contains("gen.error.unknown_dialect"))
        );
        assert!(text(&host).contains("gen.error.unknown_dialect"));
        assert!(!text(&host).contains("SELECT"), "no SQL is produced");
    }

    #[test]
    fn a_table_without_a_primary_key_is_refused() {
        let host = MockHost::install();
        let err = run(
            &host.invocation(),
            Some(r#"{"table":"log","columns":["message"],"dialect":"mysql"}"#),
        )
        .unwrap_err();
        assert!(
            matches!(err, Error::Failed(ref text) if text.contains("gen.error.no_primary_key"))
        );
    }

    #[test]
    fn malformed_json_is_an_error_with_a_key() {
        let host = MockHost::install();
        let err = run(&host.invocation(), Some("{not json")).unwrap_err();
        assert!(matches!(err, Error::Failed(ref text) if text.contains(common::KEY_JSON_INVALID)));
        assert!(text(&host).contains(common::KEY_JSON_INVALID));
    }
}
