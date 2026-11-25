//! Parameter-binding parity tests (T1-7 acceptance, ≥25 cases).
//!
//! Parity source: `mybatis-plus-sql-visualizer` Kotlin
//! `ParameterReplacerTest.kt` / `SqlDateLiteralTest.kt` (read-only behaviour
//! baseline; rewritten in Rust and expressed with JSON-typed params).

use super::*;
use crate::core::dialect::{Dialect, by_name};
use serde_json::json;

fn d(name: &str) -> &'static Dialect {
    by_name(name).unwrap_or_else(|| panic!("dialect {name} missing"))
}

fn bind_with(sql: &str, params: &serde_json::Value, dialect: &str) -> BoundSql {
    bind(
        sql,
        params.as_array().cloned().unwrap_or_default().as_slice(),
        d(dialect),
    )
}

#[test]
fn numeric_params_render_without_quotes() {
    let r = bind(
        "SELECT id FROM tbl WHERE id = ? AND age > ?",
        &[json!(1001), json!(18)],
        d("MySQL"),
    );
    assert_eq!(r.sql, "SELECT id FROM tbl WHERE id = 1001 AND age > 18");
    assert_eq!(r.confidence, Confidence::Exact);
    assert_eq!(
        r.params,
        vec![
            SqlLiteral::Number("1001".into()),
            SqlLiteral::Number("18".into())
        ]
    );
}

#[test]
fn float_param_keeps_decimal() {
    let r = bind("SELECT ? AS x", &[json!(3.5)], d("MySQL"));
    assert_eq!(r.sql, "SELECT 3.5 AS x");
}

#[test]
fn string_params_are_quoted() {
    let r = bind(
        "SELECT id FROM tbl WHERE name = ? AND city = ?",
        &[json!("John"), json!("Beijing")],
        d("PostgreSQL"),
    );
    assert_eq!(
        r.sql,
        "SELECT id FROM tbl WHERE name = 'John' AND city = 'Beijing'"
    );
}

#[test]
fn single_quote_is_escaped() {
    let r = bind(
        "SELECT id FROM tbl WHERE name = ?",
        &[json!("O'Brien")],
        d("MySQL"),
    );
    assert_eq!(r.sql, "SELECT id FROM tbl WHERE name = 'O''Brien'");
}

#[test]
fn pre_quoted_literal_is_not_double_escaped() {
    let r = bind(
        "SELECT id FROM tbl WHERE name = ?",
        &[json!("'abc'")],
        d("MySQL"),
    );
    assert_eq!(r.sql, "SELECT id FROM tbl WHERE name = 'abc'");
}

#[test]
fn pre_quoted_with_inner_escape_is_verbatim() {
    let r = bind(
        "SELECT id FROM tbl WHERE name = ?",
        &[json!("'a''b'")],
        d("MySQL"),
    );
    assert_eq!(r.sql, "SELECT id FROM tbl WHERE name = 'a''b'");
}

#[test]
fn like_pattern_is_quoted_but_not_escaped() {
    let r = bind(
        "SELECT * FROM t WHERE name LIKE ? AND code LIKE ?",
        &[json!("abc%"), json!("%a_b%")],
        d("MySQL"),
    );
    assert_eq!(
        r.sql,
        "SELECT * FROM t WHERE name LIKE 'abc%' AND code LIKE '%a_b%'"
    );
}

#[test]
fn date_ansi_literal() {
    let r = bind(
        "SELECT * FROM t WHERE d = ?",
        &[json!({"$date": "2026-11-07"})],
        d("MySQL"),
    );
    assert_eq!(r.sql, "SELECT * FROM t WHERE d = DATE '2026-11-07'");
    assert_eq!(r.params, vec![SqlLiteral::Date("2026-11-07".into())]);
}

#[test]
fn timestamp_ansi_literal_normalises_t_separator() {
    let r = bind(
        "UPDATE t SET update_time = ? WHERE id = ?",
        &[json!({"$timestamp": "2026-11-07T13:20:14.417"}), json!(1)],
        d("Oracle"),
    );
    assert_eq!(
        r.sql,
        "UPDATE t SET update_time = TIMESTAMP '2026-11-07 13:20:14.417' WHERE id = 1"
    );
}

#[test]
fn timestamp_missing_seconds_gets_00() {
    let r = bind(
        "SELECT ? AS ts",
        &[json!({"$timestamp": "2026-11-07T13:20"})],
        d("PostgreSQL"),
    );
    assert_eq!(r.sql, "SELECT TIMESTAMP '2026-11-07 13:20:00' AS ts");
}

#[test]
fn temporal_type_date_on_timestamp_family_yields_date() {
    // Timestamp-typed wrapper but pure date shape → DATE literal (reference D4).
    let r = bind(
        "SELECT * FROM t WHERE d = ?",
        &[json!({"type": "timestamp", "value": "2026-11-07"})],
        d("MySQL"),
    );
    assert_eq!(r.sql, "SELECT * FROM t WHERE d = DATE '2026-11-07'");
}

#[test]
fn sqlserver_date_falls_back_to_quoted() {
    let r = bind(
        "UPDATE t SET update_time = ?",
        &[json!({"$timestamp": "2026-11-07T13:20:14.417"})],
        d("SQL Server"),
    );
    assert_eq!(
        r.sql,
        "UPDATE t SET update_time = '2026-11-07T13:20:14.417'"
    );
    assert!(
        r.message_keys
            .iter()
            .any(|k| k == "params.date_quoted_fallback"),
        "keys: {:?}",
        r.message_keys
    );
}

#[test]
fn sqlite_date_falls_back_to_quoted() {
    let r = bind(
        "SELECT * FROM t WHERE d = ?",
        &[json!({"$date": "2026-11-07"})],
        d("SQLite"),
    );
    assert_eq!(r.sql, "SELECT * FROM t WHERE d = '2026-11-07'");
}

#[test]
fn bool_true_false_on_boolean_capable_dialect() {
    let r = bind(
        "SELECT * FROM t WHERE a = ? AND b = ?",
        &[json!(true), json!(false)],
        d("MySQL"),
    );
    assert_eq!(r.sql, "SELECT * FROM t WHERE a = TRUE AND b = FALSE");
}

#[test]
fn bool_on_oracle_degrades_to_one_zero() {
    let r = bind("SELECT * FROM t WHERE a = ?", &[json!(true)], d("Oracle"));
    assert_eq!(r.sql, "SELECT * FROM t WHERE a = 1");
    assert_eq!(r.confidence, Confidence::Partial);
    assert!(
        r.message_keys
            .iter()
            .any(|k| k == "params.boolean_literal_unsupported"),
        "keys: {:?}",
        r.message_keys
    );
}

#[test]
fn null_literal() {
    let r = bind("SELECT * FROM t WHERE a = ?", &[json!(null)], d("MySQL"));
    assert_eq!(r.sql, "SELECT * FROM t WHERE a = NULL");
    assert_eq!(r.params, vec![SqlLiteral::Null]);
}

#[test]
fn bytes_from_array() {
    let r = bind(
        "SELECT ? AS blob",
        &[json!({"$bytes": [1, 2, 255]})],
        d("MySQL"),
    );
    assert_eq!(r.sql, "SELECT X'0102FF' AS blob");
    assert_eq!(r.params, vec![SqlLiteral::Bytes(vec![1, 2, 255])]);
}

#[test]
fn bytes_from_hex_string() {
    let r = bind(
        "SELECT ? AS blob",
        &[json!({"$bytes": "0A1Bb2"})],
        d("PostgreSQL"),
    );
    assert_eq!(r.sql, "SELECT X'0A1BB2' AS blob");
}

#[test]
fn json_array_renders_as_list() {
    let r = bind(
        "SELECT * FROM t WHERE id IN (?)",
        &[json!([1, 2, 3])],
        d("MySQL"),
    );
    assert_eq!(r.sql, "SELECT * FROM t WHERE id IN (1, 2, 3)");
}

#[test]
fn named_params_by_name() {
    let r = bind(
        "SELECT * FROM t WHERE a = :id AND b = :nm",
        &[
            json!({"name": "id", "value": 7}),
            json!({"name": "nm", "value": "x"}),
        ],
        d("MySQL"),
    );
    assert_eq!(r.sql, "SELECT * FROM t WHERE a = 7 AND b = 'x'");
}

#[test]
fn named_param_same_name_twice_replaced_both() {
    let r = bind(
        "SELECT * FROM t WHERE a = :v OR b = :v",
        &[json!({"name": "v", "value": 5})],
        d("PostgreSQL"),
    );
    assert_eq!(r.sql, "SELECT * FROM t WHERE a = 5 OR b = 5");
}

#[test]
fn named_param_missing_is_kept_and_partial() {
    let r = bind(
        "SELECT * FROM t WHERE a = :id AND b = :other",
        &[json!({"name": "id", "value": 1})],
        d("MySQL"),
    );
    assert_eq!(r.sql, "SELECT * FROM t WHERE a = 1 AND b = :other");
    assert_eq!(r.confidence, Confidence::Partial);
    assert!(
        r.message_keys.iter().any(|k| k == "params.missing_named"),
        "keys: {:?}",
        r.message_keys
    );
}

#[test]
fn double_colon_cast_is_not_a_named_param() {
    let r = bind(
        "SELECT a::int FROM t WHERE b = :b",
        &[json!({"name": "b", "value": 9})],
        d("PostgreSQL"),
    );
    assert_eq!(r.sql, "SELECT a::int FROM t WHERE b = 9");
}

#[test]
fn question_mark_in_string_literal_not_replaced() {
    let r = bind(
        "SELECT * FROM t WHERE note = 'a?b' AND id = ?",
        &[json!(7)],
        d("MySQL"),
    );
    assert_eq!(r.sql, "SELECT * FROM t WHERE note = 'a?b' AND id = 7");
}

#[test]
fn question_mark_in_line_comment_not_replaced() {
    let r = bind(
        "SELECT id FROM t -- 备注?\nWHERE id = ?",
        &[json!(7)],
        d("MySQL"),
    );
    assert_eq!(r.sql, "SELECT id FROM t -- 备注?\nWHERE id = 7");
}

#[test]
fn question_mark_in_block_comment_not_replaced() {
    let r = bind(
        "SELECT id FROM t /* 占?位 */ WHERE id = ?",
        &[json!(7)],
        d("MySQL"),
    );
    assert_eq!(r.sql, "SELECT id FROM t /* 占?位 */ WHERE id = 7");
}

#[test]
fn missing_positional_kept_and_partial() {
    let r = bind(
        "SELECT id FROM t WHERE a = ? AND b = ?",
        &[json!(1)],
        d("MySQL"),
    );
    assert_eq!(r.sql, "SELECT id FROM t WHERE a = 1 AND b = ?");
    assert_eq!(r.confidence, Confidence::Partial);
    assert!(
        r.message_keys
            .iter()
            .any(|k| k == "params.missing_positional"),
        "keys: {:?}",
        r.message_keys
    );
}

#[test]
fn extra_params_are_ignored() {
    let r = bind(
        "SELECT id FROM t WHERE id = ?",
        &[json!(1), json!("extra")],
        d("MySQL"),
    );
    assert_eq!(r.sql, "SELECT id FROM t WHERE id = 1");
    // Only one literal bound (the consumed one).
    assert_eq!(r.params.len(), 1);
}

#[test]
fn unknown_type_object_degrades() {
    let r = bind(
        "SELECT * FROM t WHERE x = ?",
        &[json!({"type": "weird", "value": "x"})],
        d("MySQL"),
    );
    assert_eq!(r.sql, "SELECT * FROM t WHERE x = 'x'");
    assert_eq!(r.confidence, Confidence::Partial);
    assert!(
        r.message_keys.iter().any(|k| k == "params.unknown_type"),
        "keys: {:?}",
        r.message_keys
    );
}

#[test]
fn no_params_returns_sql_unchanged() {
    let r = bind("SELECT id FROM t", &[], d("MySQL"));
    assert_eq!(r.sql, "SELECT id FROM t");
    assert_eq!(r.confidence, Confidence::Exact);
    assert!(r.params.is_empty());
}

#[test]
fn bind_with_helper_smoke() {
    // Exercises the array-slice helper path used above.
    let r = bind_with("SELECT * FROM t WHERE id = ?", &json!([42]), "PostgreSQL");
    assert_eq!(r.sql, "SELECT * FROM t WHERE id = 42");
}
