//! CRUD SQL generation from a table description.
//!
//! Identifiers are always quoted using the dialect's canonical style, so the
//! output is independent of the reserved-word tables owned by the dialect
//! lanes. Auto-increment columns are omitted from INSERT and UPDATE SET lists
//! (MyBatis-Plus `IdType.AUTO` semantics), while they stay in SELECT lists and
//! in the primary-key predicate.

use serde::{Deserialize, Serialize};

use super::{GenError, TableSpec, prepare};

/// The five generated statement kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrudKind {
    SelectById,
    Insert,
    UpdateById,
    DeleteById,
    BatchInsert,
}

/// One generated statement plus the parameter sources, in placeholder order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrudStatement {
    pub kind: CrudKind,
    pub sql: String,
    /// Column names backing each `?`, in order.
    pub params: Vec<String>,
}

/// Generate the standard CRUD set. The batch example uses two rows; use
/// [`crud_batch`] for a different row count.
pub fn crud(spec: &TableSpec, dialect: &str) -> Result<Vec<CrudStatement>, GenError> {
    crud_batch(spec, dialect, 2)
}

/// Generate the standard CRUD set with `batch_rows` placeholder rows in the
/// batch insert.
pub fn crud_batch(
    spec: &TableSpec,
    dialect: &str,
    batch_rows: usize,
) -> Result<Vec<CrudStatement>, GenError> {
    let profile = prepare(spec, dialect)?;
    let pks = spec.primary_keys();
    if pks.is_empty() {
        return Err(GenError::NoPrimaryKey);
    }
    if batch_rows == 0 {
        return Err(GenError::unresolved(
            "gen.crud.batch_rows_zero",
            serde_json::json!({ "table": spec.name }),
        ));
    }

    let table = profile.quote_table(spec.schema.as_deref(), &spec.name);
    let select_columns = spec
        .columns
        .iter()
        .map(|c| profile.quote(&c.name))
        .collect::<Vec<_>>()
        .join(", ");
    let where_pk = pks
        .iter()
        .map(|c| format!("{} = ?", profile.quote(&c.name)))
        .collect::<Vec<_>>()
        .join(" AND ");
    let pk_params = pks.iter().map(|c| c.name.clone()).collect::<Vec<_>>();

    let select = CrudStatement {
        kind: CrudKind::SelectById,
        sql: format!("SELECT {select_columns} FROM {table} WHERE {where_pk}"),
        params: pk_params.clone(),
    };

    let insert_columns = spec.writable_columns();
    if insert_columns.is_empty() {
        return Err(GenError::NoColumns);
    }
    let insert_names = insert_columns
        .iter()
        .map(|c| profile.quote(&c.name))
        .collect::<Vec<_>>()
        .join(", ");
    let insert_placeholders = vec!["?"; insert_columns.len()].join(", ");
    let insert = CrudStatement {
        kind: CrudKind::Insert,
        sql: format!("INSERT INTO {table} ({insert_names}) VALUES ({insert_placeholders})"),
        params: insert_columns.iter().map(|c| c.name.clone()).collect(),
    };

    let set_columns = spec
        .columns
        .iter()
        .filter(|c| !c.primary_key && !c.auto_increment)
        .collect::<Vec<_>>();
    if set_columns.is_empty() {
        return Err(GenError::NoColumns);
    }
    let set_sql = set_columns
        .iter()
        .map(|c| format!("{} = ?", profile.quote(&c.name)))
        .collect::<Vec<_>>()
        .join(", ");
    let mut update_params: Vec<String> = set_columns.iter().map(|c| c.name.clone()).collect();
    update_params.extend(pk_params.iter().cloned());
    let update = CrudStatement {
        kind: CrudKind::UpdateById,
        sql: format!("UPDATE {table} SET {set_sql} WHERE {where_pk}"),
        params: update_params,
    };

    let delete = CrudStatement {
        kind: CrudKind::DeleteById,
        sql: format!("DELETE FROM {table} WHERE {where_pk}"),
        params: pk_params,
    };

    let row = format!("({})", vec!["?"; insert_columns.len()].join(", "));
    let batch_values = vec![row; batch_rows].join(", ");
    let batch_params = (0..batch_rows)
        .flat_map(|_| insert_columns.iter().map(|c| c.name.clone()))
        .collect();
    let batch_insert = CrudStatement {
        kind: CrudKind::BatchInsert,
        sql: format!("INSERT INTO {table} ({insert_names}) VALUES {batch_values}"),
        params: batch_params,
    };

    Ok(vec![select, insert, update, delete, batch_insert])
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

    fn stmt(out: &[CrudStatement], kind: CrudKind) -> &CrudStatement {
        out.iter().find(|s| s.kind == kind).expect("statement")
    }

    #[test]
    fn mysql_quotes_with_backticks_and_skips_auto_column_on_insert() {
        let out = crud(&user_table(), "mysql").unwrap();
        let select = stmt(&out, CrudKind::SelectById);
        assert_eq!(
            select.sql,
            "SELECT `id`, `name`, `status` FROM `user` WHERE `id` = ?"
        );
        let insert = stmt(&out, CrudKind::Insert);
        assert_eq!(
            insert.sql,
            "INSERT INTO `user` (`name`, `status`) VALUES (?, ?)"
        );
        assert_eq!(insert.params, vec!["name", "status"]);
    }

    #[test]
    fn postgresql_quotes_with_double_quotes() {
        let out = crud(&user_table(), "postgresql").unwrap();
        assert_eq!(
            stmt(&out, CrudKind::DeleteById).sql,
            "DELETE FROM \"user\" WHERE \"id\" = ?"
        );
    }

    #[test]
    fn sqlserver_quotes_with_brackets() {
        let out = crud(&user_table(), "sqlserver").unwrap();
        assert_eq!(
            stmt(&out, CrudKind::SelectById).sql,
            "SELECT [id], [name], [status] FROM [user] WHERE [id] = ?"
        );
    }

    #[test]
    fn oracle_quotes_with_double_quotes() {
        let out = crud(&user_table(), "oracle").unwrap();
        assert!(stmt(&out, CrudKind::Insert).sql.contains("INTO \"user\""));
    }

    #[test]
    fn update_sets_non_key_columns_and_filters_by_key() {
        let out = crud(&user_table(), "mysql").unwrap();
        let update = stmt(&out, CrudKind::UpdateById);
        assert_eq!(
            update.sql,
            "UPDATE `user` SET `name` = ?, `status` = ? WHERE `id` = ?"
        );
        assert_eq!(update.params, vec!["name", "status", "id"]);
    }

    #[test]
    fn composite_primary_key_uses_and_predicate() {
        let table = TableSpec::new(
            "order_item",
            vec![
                ColumnSpec::new("order_id").primary_key(),
                ColumnSpec::new("line_no").primary_key(),
                ColumnSpec::new("qty"),
            ],
        );
        let out = crud(&table, "mysql").unwrap();
        assert_eq!(
            stmt(&out, CrudKind::SelectById).sql,
            "SELECT `order_id`, `line_no`, `qty` FROM `order_item` \
             WHERE `order_id` = ? AND `line_no` = ?"
        );
        assert_eq!(
            stmt(&out, CrudKind::DeleteById).params,
            vec!["order_id", "line_no"]
        );
    }

    #[test]
    fn batch_insert_repeats_placeholders_for_each_row() {
        let out = crud_batch(&user_table(), "mysql", 3).unwrap();
        let batch = stmt(&out, CrudKind::BatchInsert);
        assert_eq!(
            batch.sql,
            "INSERT INTO `user` (`name`, `status`) VALUES (?, ?), (?, ?), (?, ?)"
        );
        assert_eq!(batch.params.len(), 6);
        assert_eq!(
            batch.params,
            vec!["name", "status", "name", "status", "name", "status"]
        );
    }

    #[test]
    fn one_row_batch_matches_plain_insert_shape() {
        let out = crud_batch(&user_table(), "mysql", 1).unwrap();
        let batch = stmt(&out, CrudKind::BatchInsert);
        assert_eq!(
            batch.sql,
            "INSERT INTO `user` (`name`, `status`) VALUES (?, ?)"
        );
    }

    #[test]
    fn schema_is_qualified() {
        let table = user_table().with_schema("app");
        let out = crud(&table, "postgresql").unwrap();
        assert!(
            stmt(&out, CrudKind::Insert)
                .sql
                .contains("\"app\".\"user\"")
        );
    }

    #[test]
    fn no_primary_key_is_rejected() {
        let table = TableSpec::new("log", vec![ColumnSpec::new("message")]);
        assert_eq!(crud(&table, "mysql"), Err(GenError::NoPrimaryKey));
    }

    #[test]
    fn empty_columns_are_rejected() {
        let table = TableSpec::new("empty", vec![]);
        assert_eq!(crud(&table, "mysql"), Err(GenError::NoColumns));
    }

    #[test]
    fn unknown_dialect_is_rejected() {
        let err = crud(&user_table(), "no-such-db").unwrap_err();
        assert_eq!(err, GenError::UnknownDialect("no-such-db".to_string()));
    }

    #[test]
    fn zero_batch_rows_is_rejected() {
        let err = crud_batch(&user_table(), "mysql", 0).unwrap_err();
        assert_eq!(err.message_key(), Some("gen.crud.batch_rows_zero"));
    }

    #[test]
    fn table_with_only_auto_key_cannot_insert() {
        let table = TableSpec::new(
            "seq",
            vec![ColumnSpec::new("id").primary_key().auto_increment()],
        );
        assert_eq!(crud(&table, "mysql"), Err(GenError::NoColumns));
    }

    #[test]
    fn auto_increment_column_is_not_updated() {
        let table = TableSpec::new(
            "seq",
            vec![
                ColumnSpec::new("id").primary_key(),
                ColumnSpec::new("seq_no").auto_increment(),
                ColumnSpec::new("name"),
            ],
        );
        let out = crud(&table, "mysql").unwrap();
        let update = stmt(&out, CrudKind::UpdateById);
        assert!(update.sql.contains("SET `name` = ?"));
        assert!(!update.sql.contains("`seq_no` = ?"));
    }

    #[test]
    fn statements_are_serde_roundtrippable() {
        let out = crud(&user_table(), "mysql").unwrap();
        let json = serde_json::to_string(&out).unwrap();
        let back: Vec<CrudStatement> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, out);
    }
}
