//! Rollback plan generation for `UPDATE` / `DELETE`.
//!
//! The plan re-creates or restores rows located by the statement's primary key
//! predicate. Old column values do not exist in SQL text, so the reverse
//! statements use `?` placeholders and the plan lists, in order, which columns
//! those placeholders come from (`before_image_columns`). A snapshot query that
//! must run *before* the forward statement accompanies the plan.
//!
//! Hard rule: without a primary key there is no way to locate the affected
//! rows, so generation is refused with [`GenError::NoPrimaryKey`].

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{Confidence, GenError, TableSpec, Unresolved, prepare};
use crate::core::perf::sql::{Analysis, StmtKind};

/// Which statement the plan reverses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RollbackKind {
    Update,
    Delete,
}

/// A reverse plan plus the metadata needed to fill its placeholders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RollbackPlan {
    pub kind: RollbackKind,
    pub table: String,
    pub primary_key: Vec<String>,
    /// Reverse statement(s), using `?` for values not present in the SQL text.
    pub statements: Vec<String>,
    /// Columns whose old values fill the `?` placeholders, in order.
    pub before_image_columns: Vec<String>,
    /// Run this before the forward statement to capture the before-image.
    pub snapshot_sql: Option<String>,
    /// WHERE clause of the forward statement, reused verbatim.
    pub forward_where: String,
    pub confidence: Confidence,
    pub notes: Vec<Unresolved>,
}

/// Build a rollback plan for `sql` against `table`.
pub fn rollback_plan(
    sql: &str,
    table: &TableSpec,
    dialect: &str,
) -> Result<RollbackPlan, GenError> {
    if sql.trim().is_empty() {
        return Err(GenError::EmptySql);
    }
    let profile = prepare(table, dialect)?;
    let pks = table.primary_keys();
    if pks.is_empty() {
        return Err(GenError::NoPrimaryKey);
    }
    let sa = Analysis::parse(sql);
    if has_extra_statement(&sa) {
        return Err(GenError::unresolved(
            "gen.rollback.multi_statement",
            json!({}),
        ));
    }
    let kind = match sa.stmt {
        StmtKind::Update => RollbackKind::Update,
        StmtKind::Delete => RollbackKind::Delete,
        other => {
            return Err(GenError::unresolved(
                "gen.rollback.not_dml",
                json!({ "statement": format!("{other:?}") }),
            ));
        }
    };

    let (target_name, target_alias) = target_of(&sa, kind)
        .ok_or_else(|| GenError::unresolved("gen.rollback.table_required", json!({})))?;
    if !last_segment(&target_name).eq_ignore_ascii_case(last_segment(&table.name)) {
        return Err(GenError::unresolved(
            "gen.rollback.table_mismatch",
            json!({ "sql_table": target_name, "spec_table": table.name }),
        ));
    }

    let where_idx = sa.find_top("WHERE").ok_or_else(|| {
        GenError::unresolved(
            "gen.rollback.where_required",
            json!({ "table": table.name }),
        )
    })?;
    let where_end = sa.clause_end(where_idx + 1);
    let forward_where = where_text(&sa, where_idx);
    if forward_where.is_empty() {
        return Err(GenError::unresolved(
            "gen.rollback.where_required",
            json!({ "table": table.name }),
        ));
    }

    let pk_names: Vec<String> = pks.iter().map(|c| c.name.clone()).collect();
    let missing: Vec<String> = pk_names
        .iter()
        .filter(|pk| {
            !has_equality_on(
                &sa,
                where_idx + 1,
                where_end,
                pk,
                target_alias.as_deref(),
                &target_name,
            )
        })
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(GenError::unresolved(
            "gen.rollback.where_without_primary_key",
            json!({ "table": table.name, "missing_primary_key": missing }),
        ));
    }

    let table_sql = profile.quote_table(table.schema.as_deref(), &table.name);
    let all_columns = table
        .columns
        .iter()
        .map(|c| profile.quote(&c.name))
        .collect::<Vec<_>>();
    let all_column_list = all_columns.join(", ");
    let snapshot_sql = Some(format!(
        "SELECT {all_column_list} FROM {table_sql} WHERE {forward_where}"
    ));

    let (statements, before_image_columns) = match kind {
        RollbackKind::Delete => {
            let placeholders = vec!["?"; table.columns.len()].join(", ");
            (
                vec![format!(
                    "INSERT INTO {table_sql} ({all_column_list}) VALUES ({placeholders})"
                )],
                table.columns.iter().map(|c| c.name.clone()).collect(),
            )
        }
        RollbackKind::Update => {
            let set_idx = sa
                .find_top("SET")
                .ok_or_else(|| GenError::unresolved("gen.rollback.no_set_columns", json!({})))?;
            let set_end = sa.find_top_after("WHERE", set_idx + 1).unwrap_or(where_idx);
            let set_columns = set_columns(&sa, set_idx + 1, set_end);
            if set_columns.is_empty() {
                return Err(GenError::unresolved(
                    "gen.rollback.no_set_columns",
                    json!({ "table": table.name }),
                ));
            }
            if let Some(updated_pk) = set_columns
                .iter()
                .find(|c| pk_names.iter().any(|p| p.eq_ignore_ascii_case(c)))
            {
                return Err(GenError::unresolved(
                    "gen.rollback.primary_key_update",
                    json!({ "column": updated_pk }),
                ));
            }
            let set_sql = set_columns
                .iter()
                .map(|c| format!("{} = ?", profile.quote(c)))
                .collect::<Vec<_>>()
                .join(", ");
            (
                vec![format!(
                    "UPDATE {table_sql} SET {set_sql} WHERE {forward_where}"
                )],
                set_columns,
            )
        }
    };

    let notes = vec![Unresolved::new(
        "gen.rollback.before_image_required",
        json!({
            "columns": before_image_columns.clone(),
            "placeholder_order": "before_image_columns, then the forward WHERE placeholders",
            "kind": match kind { RollbackKind::Update => "update", RollbackKind::Delete => "delete" },
        }),
    )];

    Ok(RollbackPlan {
        kind,
        table: table.name.clone(),
        primary_key: pk_names,
        statements,
        before_image_columns,
        snapshot_sql,
        forward_where,
        confidence: Confidence::Partial,
        notes,
    })
}

/// Reject multi-statement input: a plan must map one forward statement to one
/// reverse intent. Trailing `;` is allowed.
fn has_extra_statement(sa: &Analysis) -> bool {
    let last_code = sa
        .tokens
        .iter()
        .rposition(|t| !t.is_punct(';'))
        .map(|i| i + 1)
        .unwrap_or(0);
    (0..last_code).any(|i| sa.is_top(i) && sa.tokens[i].is_punct(';'))
}

fn last_segment(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// Extract the DML target table and optional alias.
fn target_of(sa: &Analysis, kind: RollbackKind) -> Option<(String, Option<String>)> {
    let kw = match kind {
        RollbackKind::Delete => sa.find_top("FROM")?,
        RollbackKind::Update => sa.find_top("UPDATE")?,
    };
    let mut i = kw + 1;
    let name = sa.tokens.get(i)?.ident_name()?.to_string();
    let mut qualified = name;
    i += 1;
    while i + 1 < sa.len() && sa.tokens[i].is_punct('.') && sa.tokens[i + 1].ident_name().is_some()
    {
        qualified.push('.');
        qualified.push_str(sa.tokens[i + 1].ident_name().unwrap_or_default());
        i += 2;
    }
    let mut alias = None;
    if i < sa.len() {
        if sa.is_word_at(i, "AS") {
            alias = sa
                .tokens
                .get(i + 1)
                .and_then(|t| t.ident_name())
                .map(str::to_string);
        } else if let Some(a) = sa.tokens.get(i).and_then(|t| t.ident_name()) {
            if !crate::core::perf::sql::is_keyword(a) {
                alias = Some(a.to_string());
            }
        }
    }
    Some((qualified, alias))
}

/// Raw text of the forward `WHERE` clause (keyword dropped, trailing `;`
/// dropped, leading/trailing whitespace trimmed).
fn where_text(sa: &Analysis, where_idx: usize) -> String {
    let start = sa.tokens[where_idx].end;
    let end = sa
        .tokens
        .iter()
        .rev()
        .find(|t| !t.is_punct(';'))
        .map(|t| t.end)
        .unwrap_or(start);
    if end <= start {
        return String::new();
    }
    sa.sql[start..end].trim().to_string()
}

/// Does `[start, end)` contain `pk = <value>` (optionally qualified)?
fn has_equality_on(
    sa: &Analysis,
    start: usize,
    end: usize,
    pk: &str,
    alias: Option<&str>,
    table: &str,
) -> bool {
    let mut i = start;
    while i < end {
        let Some(col) = sa.read_column_ref(i) else {
            i += 1;
            continue;
        };
        if !col.column.eq_ignore_ascii_case(pk) {
            i += 1;
            continue;
        }
        if let Some(q) = col.alias.as_deref() {
            let matches = alias.is_some_and(|a| a.eq_ignore_ascii_case(q))
                || last_segment(table).eq_ignore_ascii_case(q)
                || table.eq_ignore_ascii_case(q);
            if !matches {
                i += 1;
                continue;
            }
        }
        let after = ref_end(sa, i);
        if sa
            .tokens
            .get(after)
            .is_some_and(|t| t.text == "=" || t.text == "==")
        {
            return true;
        }
        i = after;
    }
    false
}

fn set_columns(sa: &Analysis, start: usize, end: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut i = start;
    while i < end {
        let Some(col) = sa.read_column_ref(i) else {
            i += 1;
            continue;
        };
        let after = ref_end(sa, i);
        if sa
            .tokens
            .get(after)
            .is_some_and(|t| t.text == "=" || t.text == "==")
            && !out.iter().any(|c| c.eq_ignore_ascii_case(&col.column))
        {
            out.push(col.column.clone());
        }
        i = after;
    }
    out
}

fn ref_end(sa: &Analysis, i: usize) -> usize {
    if i + 2 < sa.len() && sa.tokens[i + 1].is_punct('.') && sa.tokens[i + 2].ident_name().is_some()
    {
        i + 3
    } else {
        i + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::r#gen::ColumnSpec;

    fn user_table() -> TableSpec {
        TableSpec::new(
            "user",
            vec![
                ColumnSpec::new("id").primary_key().auto_increment(),
                ColumnSpec::new("name"),
                ColumnSpec::new("status"),
            ],
        )
    }

    #[test]
    fn delete_becomes_insert_of_all_columns() {
        let plan = rollback_plan("DELETE FROM user WHERE id = ?", &user_table(), "mysql").unwrap();
        assert_eq!(plan.kind, RollbackKind::Delete);
        assert_eq!(
            plan.statements,
            vec!["INSERT INTO `user` (`id`, `name`, `status`) VALUES (?, ?, ?)"]
        );
        assert_eq!(plan.before_image_columns, vec!["id", "name", "status"]);
        assert_eq!(plan.confidence, Confidence::Partial);
        assert!(
            plan.notes
                .iter()
                .any(|n| n.message_key == "gen.rollback.before_image_required")
        );
        assert_eq!(
            plan.snapshot_sql.as_deref(),
            Some("SELECT `id`, `name`, `status` FROM `user` WHERE id = ?")
        );
    }

    #[test]
    fn update_restores_set_columns_and_reuses_the_primary_key_where() {
        let plan = rollback_plan(
            "UPDATE user SET name = ?, status = 'A' WHERE id = ?",
            &user_table(),
            "mysql",
        )
        .unwrap();
        assert_eq!(plan.kind, RollbackKind::Update);
        assert_eq!(
            plan.statements,
            vec!["UPDATE `user` SET `name` = ?, `status` = ? WHERE id = ?"]
        );
        assert_eq!(plan.before_image_columns, vec!["name", "status"]);
        assert_eq!(plan.primary_key, vec!["id"]);
        assert_eq!(plan.forward_where, "id = ?");
    }

    #[test]
    fn composite_primary_key_must_all_be_constrained() {
        let table = TableSpec::new(
            "order_item",
            vec![
                ColumnSpec::new("order_id").primary_key(),
                ColumnSpec::new("line_no").primary_key(),
                ColumnSpec::new("qty"),
            ],
        );
        assert!(
            rollback_plan(
                "DELETE FROM order_item WHERE order_id = 1 AND line_no = 2",
                &table,
                "mysql"
            )
            .is_ok()
        );
        let err = rollback_plan("DELETE FROM order_item WHERE order_id = 1", &table, "mysql")
            .unwrap_err();
        assert_eq!(
            err.message_key(),
            Some("gen.rollback.where_without_primary_key")
        );
    }

    #[test]
    fn no_primary_key_is_rejected() {
        let table = TableSpec::new(
            "log",
            vec![ColumnSpec::new("message"), ColumnSpec::new("level")],
        );
        assert_eq!(
            rollback_plan("DELETE FROM log WHERE message = 'x'", &table, "mysql"),
            Err(GenError::NoPrimaryKey)
        );
    }

    #[test]
    fn where_without_primary_key_is_rejected_even_if_a_key_exists() {
        let err = rollback_plan(
            "DELETE FROM user WHERE status = 'A'",
            &user_table(),
            "mysql",
        )
        .unwrap_err();
        assert_eq!(
            err.message_key(),
            Some("gen.rollback.where_without_primary_key")
        );
    }

    #[test]
    fn missing_where_is_rejected() {
        let err = rollback_plan("DELETE FROM user", &user_table(), "mysql").unwrap_err();
        assert_eq!(err.message_key(), Some("gen.rollback.where_required"));
    }

    #[test]
    fn non_dml_is_rejected() {
        let err =
            rollback_plan("SELECT * FROM user WHERE id = 1", &user_table(), "mysql").unwrap_err();
        assert_eq!(err.message_key(), Some("gen.rollback.not_dml"));
    }

    #[test]
    fn multi_statement_is_rejected() {
        let err = rollback_plan(
            "DELETE FROM user WHERE id = 1; DELETE FROM user WHERE id = 2",
            &user_table(),
            "mysql",
        )
        .unwrap_err();
        assert_eq!(err.message_key(), Some("gen.rollback.multi_statement"));
    }

    #[test]
    fn trailing_semicolon_is_allowed() {
        assert!(rollback_plan("DELETE FROM user WHERE id = 1;", &user_table(), "mysql").is_ok());
    }

    #[test]
    fn postgresql_quotes_with_double_quotes() {
        let plan =
            rollback_plan("DELETE FROM user WHERE id = ?", &user_table(), "postgresql").unwrap();
        assert!(plan.statements[0].starts_with("INSERT INTO \"user\" ("));
    }

    #[test]
    fn schema_is_qualified() {
        let plan = rollback_plan(
            "DELETE FROM user WHERE id = ?",
            &user_table().with_schema("app"),
            "mysql",
        )
        .unwrap();
        assert!(plan.statements[0].contains("INSERT INTO `app`.`user`"));
    }

    #[test]
    fn table_mismatch_is_rejected() {
        let err =
            rollback_plan("DELETE FROM account WHERE id = 1", &user_table(), "mysql").unwrap_err();
        assert_eq!(err.message_key(), Some("gen.rollback.table_mismatch"));
    }

    #[test]
    fn table_alias_is_accepted_in_the_where_clause() {
        let plan = rollback_plan(
            "UPDATE user u SET name = ? WHERE u.id = ?",
            &user_table(),
            "mysql",
        )
        .unwrap();
        assert_eq!(plan.forward_where, "u.id = ?");
    }

    #[test]
    fn updating_the_primary_key_is_rejected() {
        let err = rollback_plan(
            "UPDATE user SET id = 2 WHERE id = 1",
            &user_table(),
            "mysql",
        )
        .unwrap_err();
        assert_eq!(err.message_key(), Some("gen.rollback.primary_key_update"));
    }

    #[test]
    fn unknown_dialect_is_rejected() {
        assert_eq!(
            rollback_plan("DELETE FROM user WHERE id = 1", &user_table(), "nope"),
            Err(GenError::UnknownDialect("nope".to_string()))
        );
    }

    #[test]
    fn plan_is_serde_roundtrippable() {
        let plan = rollback_plan("DELETE FROM user WHERE id = 1", &user_table(), "mysql").unwrap();
        let json = serde_json::to_string(&plan).unwrap();
        let back: RollbackPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(back, plan);
    }
}
