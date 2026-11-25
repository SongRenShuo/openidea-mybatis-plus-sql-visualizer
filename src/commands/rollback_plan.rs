//! `rollback-plan`: the reverse statements that undo an UPDATE/DELETE.
//!
//! The argument is JSON: `sql`, the table description (`table`, `schema`,
//! `columns`, `pk`) and `dialect`; it may also be `@<path>` or a bare project
//! file path holding that JSON. The plan comes from
//! [`crate::core::r#gen::rollback_plan`]; a statement without a primary key, or
//! whose WHERE does not constrain every primary-key column, is **refused**
//! (`-- refused: <message_key>`) and no reverse SQL is printed.

use std::fmt::Write as _;

use openidea_plugin::{Error, Invocation};
use serde::Deserialize;

use crate::commands::common::{self, ColumnInput};
use crate::core::r#gen::{GenError, RollbackKind, RollbackPlan, rollback_plan};

/// Matches `[[contributes.commands]] id` in `plugin.toml`.
pub const ID: &str = "rollback-plan";

/// What the command accepts as `arg`.
pub const USAGE: &str = r#"{"sql":"DELETE FROM user WHERE id = ?","table":"user","columns":["id","name"],"pk":["id"],"dialect":"mysql"}"#;

#[derive(Debug, Clone, Deserialize)]
struct RollbackInput {
    sql: String,
    table: String,
    #[serde(default)]
    schema: Option<String>,
    columns: Vec<ColumnInput>,
    #[serde(default)]
    pk: Vec<String>,
    dialect: String,
}

/// Dispatch entry.
pub fn run(ctx: &Invocation, arg: Option<&str>) -> Result<Option<String>, Error> {
    if arg.map(str::trim).unwrap_or("").is_empty() {
        return common::fail(common::missing_arg(USAGE));
    }
    let input: RollbackInput = match common::json_arg(ctx, arg) {
        Ok(input) => input,
        Err(err) => return common::fail(err),
    };
    let spec = common::build_table_spec(
        &input.table,
        input.schema.as_deref(),
        &input.columns,
        &input.pk,
    );
    match rollback_plan(&input.sql, &spec, &input.dialect) {
        Ok(plan) => {
            common::emit(&format_success(&plan, &input.dialect))?;
        }
        Err(err) if matches!(&err, GenError::UnknownDialect(_) | GenError::EmptySql) => {
            // Bad input, not a refused plan: report it as a command error.
            return common::fail(common::gen_error(&err));
        }
        Err(err) => {
            // A refused plan is a designed outcome, not a command failure: the
            // user gets the reason and no reverse SQL.
            let (key, detail) = common::gen_error_parts(&err);
            let mut text = String::new();
            if detail.is_empty() {
                let _ = writeln!(text, "-- refused: {key} (table: {})", input.table);
            } else {
                let _ = writeln!(text, "-- refused: {key} {detail}");
            }
            common::emit(&text)?;
        }
    }
    Ok(None)
}

fn format_success(plan: &RollbackPlan, dialect: &str) -> String {
    let mut text = String::new();
    let _ = writeln!(
        text,
        "-- rollback-plan: {} `{}` ({dialect})",
        kind_label(plan.kind),
        plan.table
    );
    let _ = writeln!(text, "primary key: {}", plan.primary_key.join(", "));
    let _ = writeln!(text, "forward where: {}", plan.forward_where);
    if let Some(snapshot) = &plan.snapshot_sql {
        let _ = writeln!(text, "snapshot (run first): {snapshot}");
    }
    text.push_str("reverse:\n");
    for statement in &plan.statements {
        let _ = writeln!(text, "  {statement}");
    }
    let _ = writeln!(
        text,
        "before-image columns: {}",
        plan.before_image_columns.join(", ")
    );
    for note in &plan.notes {
        let _ = writeln!(text, "notes: {} {}", note.message_key, note.detail);
    }
    text
}

fn kind_label(kind: RollbackKind) -> &'static str {
    match kind {
        RollbackKind::Update => "update",
        RollbackKind::Delete => "delete",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    const TABLE: &str =
        r#""table":"user","columns":["id","name","status"],"pk":["id"],"dialect":"mysql""#;

    fn text(host: &MockHost) -> String {
        host.output()[common::CHANNEL].clone()
    }

    #[test]
    fn a_delete_plan_restores_every_column() {
        let host = MockHost::install();
        let arg = format!(r#"{{"sql":"DELETE FROM user WHERE id = ?",{TABLE}}}"#);
        run(&host.invocation(), Some(&arg)).unwrap();
        let text = text(&host);
        assert!(text.contains("-- rollback-plan: delete `user` (mysql)"));
        assert!(text.contains("primary key: id"));
        assert!(text.contains(
            "snapshot (run first): SELECT `id`, `name`, `status` FROM `user` WHERE id = ?"
        ));
        assert!(text.contains("INSERT INTO `user` (`id`, `name`, `status`) VALUES (?, ?, ?)"));
        assert!(text.contains("before-image columns: id, name, status"));
        assert!(text.contains("gen.rollback.before_image_required"));
    }

    #[test]
    fn an_update_plan_restores_the_set_columns() {
        let host = MockHost::install();
        let arg = format!(r#"{{"sql":"UPDATE user SET name = ? WHERE id = ?",{TABLE}}}"#);
        run(&host.invocation(), Some(&arg)).unwrap();
        let text = text(&host);
        assert!(text.contains("-- rollback-plan: update `user` (mysql)"));
        assert!(text.contains("UPDATE `user` SET `name` = ? WHERE id = ?"));
        assert!(text.contains("before-image columns: name"));
    }

    #[test]
    fn a_table_without_a_primary_key_is_refused_without_sql() {
        let host = MockHost::install();
        run(
            &host.invocation(),
            Some(
                r#"{"sql":"DELETE FROM log WHERE id = 1","table":"log","columns":["message"],"dialect":"mysql"}"#,
            ),
        )
        .unwrap();
        let text = text(&host);
        assert!(text.contains("-- refused: gen.error.no_primary_key"));
        assert!(!text.contains("INSERT INTO"));
        assert!(!text.contains("UPDATE "));
    }

    #[test]
    fn a_where_that_skips_the_primary_key_is_refused_without_sql() {
        let host = MockHost::install();
        let arg = format!(r#"{{"sql":"DELETE FROM user WHERE status = 'A'",{TABLE}}}"#);
        run(&host.invocation(), Some(&arg)).unwrap();
        let text = text(&host);
        assert!(text.contains("-- refused: gen.rollback.where_without_primary_key"));
        assert!(text.contains("\"missing_primary_key\":[\"id\"]"));
        assert!(!text.contains("INSERT INTO"));
    }

    #[test]
    fn a_missing_where_is_refused() {
        let host = MockHost::install();
        let arg = format!(r#"{{"sql":"DELETE FROM user",{TABLE}}}"#);
        run(&host.invocation(), Some(&arg)).unwrap();
        assert!(text(&host).contains("-- refused: gen.rollback.where_required"));
    }

    #[test]
    fn malformed_json_is_an_error_with_a_key() {
        let host = MockHost::install();
        let err = run(&host.invocation(), Some("[1,2,3]")).unwrap_err();
        assert!(matches!(err, Error::Failed(ref text) if text.contains(common::KEY_JSON_INVALID)));
    }

    #[test]
    fn an_unknown_dialect_is_an_error_not_a_refusal() {
        let host = MockHost::install();
        let arg = r#"{"sql":"DELETE FROM user WHERE id = 1","table":"user","columns":["id"],"pk":["id"],"dialect":"nope"}"#;
        let err = run(&host.invocation(), Some(arg)).unwrap_err();
        assert!(
            matches!(err, Error::Failed(ref text) if text.contains("gen.error.unknown_dialect"))
        );
        let text = text(&host);
        assert!(text.contains("-- error [gen.error.unknown_dialect]"));
        assert!(!text.contains("-- refused:"));
    }
}
