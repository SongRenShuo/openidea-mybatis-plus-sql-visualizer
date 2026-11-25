//! Parity tests for T1-11.
//!
//! Expected values are derived from the reference Kotlin suite
//! `src/test/kotlin/cn/song/mybatisplus/visualizer/log/` (MyBatisLogParserTest,
//! HibernateLogParserTest, JdbcTemplateLogParserTest, ExecutionTimeParserTest,
//! LogDurationParserTest, SqlErrorLineRecognizerTest). The Rust expectations
//! assert the same SQL/params/duration values, not the Kotlin API shape.

use std::time::{Duration, Instant};

use super::*;

fn parse_lines(lines: &[&str]) -> LogAnalysis {
    parse(&lines.join("\n"), &ParseOptions::default())
}

fn parse_with(lines: &[&str], opts: &ParseOptions) -> LogAnalysis {
    parse(&lines.join("\n"), opts)
}

// ---------------------------------------------------------------------------
// MyBatis (reference MyBatisLogParserTest, 10 cases)
// ---------------------------------------------------------------------------

#[test]
fn mybatis_mp_prefix_pairs_preparing_and_parameters() {
    let analysis = parse_lines(&[
        "==>  Preparing: SELECT id, name FROM user WHERE id = ? AND status = ?",
        "==> Parameters: 1(Long), active(String)",
    ]);
    assert_eq!(analysis.entries.len(), 1);
    let entry = &analysis.entries[0];
    assert_eq!(entry.family, Family::MyBatis);
    assert_eq!(entry.line_no, 1);
    assert_eq!(
        entry.sql,
        "SELECT id, name FROM user WHERE id = 1 AND status = 'active'"
    );
    assert_eq!(entry.params, vec!["1", "'active'"]);
    assert_eq!(entry.confidence, Confidence::Exact);
}

#[test]
fn mybatis_native_format_without_arrows() {
    let analysis = parse_lines(&[
        "Preparing: DELETE FROM t WHERE id = ?",
        "Parameters: 7(Integer)",
    ]);
    assert_eq!(analysis.entries.len(), 1);
    assert_eq!(analysis.entries[0].sql, "DELETE FROM t WHERE id = 7");
}

#[test]
fn mybatis_keyword_after_log_framework_prefix_and_timestamp() {
    let analysis = parse_lines(&[
        "2024-01-15 10:30:45.123  INFO 1234 --- [main] c.s.m.Log : ==>  Preparing: SELECT * FROM user WHERE id = ?",
        "2024-01-15 10:30:45.124  INFO 1234 --- [main] c.s.m.Log : ==> Parameters: 42(Long)",
    ]);
    assert_eq!(analysis.entries.len(), 1);
    assert_eq!(analysis.entries[0].sql, "SELECT * FROM user WHERE id = 42");
    assert_eq!(analysis.entries[0].timestamp_ms, Some(1_705_314_645_123));
}

#[test]
fn mybatis_multiple_statements_are_separate() {
    let analysis = parse_lines(&[
        "==>  Preparing: SELECT * FROM a WHERE x = ?",
        "==> Parameters: 1(Integer)",
        "==>  Preparing: SELECT * FROM b WHERE y = ?",
        "==> Parameters: 2(Integer)",
    ]);
    assert_eq!(analysis.entries.len(), 2);
    assert_eq!(analysis.entries[0].sql, "SELECT * FROM a WHERE x = 1");
    assert_eq!(analysis.entries[1].sql, "SELECT * FROM b WHERE y = 2");
    assert_eq!(analysis.entries[1].line_no, 3);
}

#[test]
fn mybatis_parameterless_statement_records_total_rows() {
    let analysis = parse_lines(&[
        "==>  Preparing: SELECT COUNT(*) FROM user",
        "==> Total: 3",
        "<==    Columns: COUNT(*)",
    ]);
    assert_eq!(analysis.entries.len(), 1);
    assert_eq!(analysis.entries[0].sql, "SELECT COUNT(*) FROM user");
    assert_eq!(analysis.entries[0].result_rows, Some(3));
    assert_eq!(analysis.entries[0].affected_rows, None);
    assert!(analysis.entries[0].params.is_empty());
}

#[test]
fn mybatis_null_boolean_and_timestamp_parameters() {
    let analysis = parse_lines(&[
        "==>  Preparing: SELECT * FROM t WHERE a = ? AND b = ? AND c = ? AND d = ?",
        "==> Parameters: 1(Long), null, true(Boolean), 2024-01-15 10:30:45.123(Timestamp)",
    ]);
    assert_eq!(analysis.entries.len(), 1);
    assert_eq!(
        analysis.entries[0].sql,
        "SELECT * FROM t WHERE a = 1 AND b = NULL AND c = true AND d = TIMESTAMP '2024-01-15 10:30:45.123'"
    );
}

#[test]
fn mybatis_values_with_commas_and_parentheses() {
    let analysis = parse_lines(&[
        "==>  Preparing: SELECT * FROM t WHERE name IN (?, ?)",
        "==> Parameters: a, b(String), c(x)(String)",
    ]);
    assert_eq!(analysis.entries.len(), 1);
    assert_eq!(
        analysis.entries[0].sql,
        "SELECT * FROM t WHERE name IN ('a, b', 'c(x)')"
    );
}

#[test]
fn mybatis_multiline_sql_is_joined() {
    let analysis = parse_lines(&[
        "2024-01-15 10:30:45.123 DEBUG c.s.M : ==>  Preparing: SELECT id",
        "  , name FROM user",
        " WHERE id = ?",
        "2024-01-15 10:30:45.124 DEBUG c.s.M : ==> Parameters: 7(Long)",
    ]);
    assert_eq!(analysis.entries.len(), 1);
    assert_eq!(
        analysis.entries[0].sql,
        "SELECT id\n  , name FROM user\n WHERE id = 7"
    );
    assert_eq!(analysis.entries[0].line_no, 1);
    assert_eq!(analysis.entries[0].timestamp_ms, Some(1_705_314_645_123));
}

#[test]
fn mybatis_missing_parameters_marks_partial_and_keeps_raw_line() {
    let analysis = parse_lines(&[
        "==>  Preparing: SELECT * FROM t WHERE id = ?",
        "==> Total: 1",
    ]);
    assert_eq!(analysis.entries.len(), 1);
    let entry = &analysis.entries[0];
    assert_eq!(entry.sql, "SELECT * FROM t WHERE id = ?");
    assert_eq!(entry.confidence, Confidence::Partial);
    assert!(
        entry
            .message_keys
            .iter()
            .any(|key| key == message_keys::ENTRY_PARAMS_MISSING)
    );
    assert!(entry.raw_line.is_some());
}

#[test]
fn mybatis_statement_executing_form_has_no_parameters() {
    let analysis =
        parse_lines(&["==>  Executing: INSERT INTO audit_log (id, msg) VALUES (1, 'x')"]);
    assert_eq!(analysis.entries.len(), 1);
    assert_eq!(
        analysis.entries[0].sql,
        "INSERT INTO audit_log (id, msg) VALUES (1, 'x')"
    );
    assert_eq!(analysis.entries[0].confidence, Confidence::Exact);
}

/// `fixture-java/mybatis-capture.log` verbatim: six statements, each a
/// `Preparing:` / `Parameters:` / close-row triple on its own console thread.
/// The close rows are the only place the end timestamp appears, so the
/// duration is the gap between the two log lines.
const MYBATIS_CAPTURE: &str = r#"2026-11-25 21:00:01.001 DEBUG 4242 --- [nio-8080-exec-1] c.e.m.mapper.UserMapper.selectById : ==>  Preparing: SELECT id, name, email FROM users WHERE id = ?
2026-11-25 21:00:01.002 DEBUG 4242 --- [nio-8080-exec-1] c.e.m.mapper.UserMapper.selectById : ==> Parameters: 42(Long)
2026-11-25 21:00:01.006 DEBUG 4242 --- [nio-8080-exec-1] c.e.m.mapper.UserMapper.selectById : <==      Total: 1
2026-11-25 21:00:02.101 DEBUG 4242 --- [nio-8080-exec-2] c.e.m.mapper.OrderMapper.selectByUser : ==>  Preparing: SELECT id, user_id, amount FROM orders WHERE user_id = ? ORDER BY id DESC
2026-11-25 21:00:02.102 DEBUG 4242 --- [nio-8080-exec-2] c.e.m.mapper.OrderMapper.selectByUser : ==> Parameters: 42(Long)
2026-11-25 21:00:02.115 DEBUG 4242 --- [nio-8080-exec-2] c.e.m.mapper.OrderMapper.selectByUser : <==      Total: 3
2026-11-25 21:00:03.001 DEBUG 4242 --- [nio-8080-exec-3] c.e.m.mapper.UserMapper.selectById : ==>  Preparing: SELECT id, name, email FROM users WHERE id = ?
2026-11-25 21:00:03.002 DEBUG 4242 --- [nio-8080-exec-3] c.e.m.mapper.UserMapper.selectById : ==> Parameters: 43(Long)
2026-11-25 21:00:03.004 DEBUG 4242 --- [nio-8080-exec-3] c.e.m.mapper.UserMapper.selectById : <==      Total: 1
2026-11-25 21:00:03.501 DEBUG 4242 --- [nio-8080-exec-4] c.e.m.mapper.UserMapper.selectById : ==>  Preparing: SELECT id, name, email FROM users WHERE id = ?
2026-11-25 21:00:03.502 DEBUG 4242 --- [nio-8080-exec-4] c.e.m.mapper.UserMapper.selectById : ==> Parameters: 44(Long)
2026-11-25 21:00:03.505 DEBUG 4242 --- [nio-8080-exec-4] c.e.m.mapper.UserMapper.selectById : <==      Total: 1
2026-11-25 21:00:04.001 DEBUG 4242 --- [nio-8080-exec-5] c.e.m.mapper.UserMapper.selectById : ==>  Preparing: SELECT id, name, email FROM users WHERE id = ?
2026-11-25 21:00:04.002 DEBUG 4242 --- [nio-8080-exec-5] c.e.m.mapper.UserMapper.selectById : ==> Parameters: 45(Long)
2026-11-25 21:00:04.006 DEBUG 4242 --- [nio-8080-exec-5] c.e.m.mapper.UserMapper.selectById : <==      Total: 1
2026-11-25 21:00:06.700 DEBUG 4242 --- [nio-8080-exec-6] c.e.m.mapper.OrderMapper.updateStatus : ==>  Preparing: UPDATE orders SET status = ? WHERE user_id = ? AND status = ?
2026-11-25 21:00:06.701 DEBUG 4242 --- [nio-8080-exec-6] c.e.m.mapper.OrderMapper.updateStatus : ==> Parameters: SHIPPED(String), 42(Long), PAID(String)
2026-11-25 21:00:06.980 DEBUG 4242 --- [nio-8080-exec-6] c.e.m.mapper.OrderMapper.updateStatus : <==    Updates: 3
"#;

#[test]
fn mybatis_capture_fixture_derives_duration_from_close_rows() {
    let analysis = parse(MYBATIS_CAPTURE, &ParseOptions::default());
    assert_eq!(analysis.entries.len(), 6);
    assert_eq!(
        analysis
            .entries
            .iter()
            .map(|entry| entry.duration_ms)
            .collect::<Vec<_>>(),
        vec![
            Some(5.0),
            Some(14.0),
            Some(3.0),
            Some(4.0),
            Some(5.0),
            Some(280.0)
        ]
    );
    // `Total:` is the result row count, `Updates:` the changed row count.
    assert_eq!(
        analysis
            .entries
            .iter()
            .map(|entry| entry.result_rows)
            .collect::<Vec<_>>(),
        vec![Some(1), Some(3), Some(1), Some(1), Some(1), None]
    );
    assert_eq!(
        analysis
            .entries
            .iter()
            .map(|entry| entry.affected_rows)
            .collect::<Vec<_>>(),
        vec![None, None, None, None, None, Some(3)]
    );
    let last = &analysis.entries[5];
    assert_eq!(last.class, EntryClass::Update);
    assert_eq!(
        last.sql,
        "UPDATE orders SET status = 'SHIPPED' WHERE user_id = 42 AND status = 'PAID'"
    );
}

#[test]
fn mybatis_capture_fixture_feeds_slow_query_detection() {
    let opts = ParseOptions {
        slow_threshold_ms: 200.0,
        ..ParseOptions::default()
    };
    let analysis = parse(MYBATIS_CAPTURE, &opts);
    assert_eq!(analysis.slow.len(), 1);
    assert_eq!(analysis.slow[0].line_no, 16);
    assert_eq!(analysis.slow[0].duration_ms, Some(280.0));
}

#[test]
fn close_row_is_matched_by_thread_not_by_emission_order() {
    let analysis = parse_lines(&[
        "2026-11-25 21:00:01.000 DEBUG 1 --- [nio-1] a : ==>  Preparing: SELECT 1 FROM a WHERE id = ?",
        "2026-11-25 21:00:01.001 DEBUG 1 --- [nio-1] a : ==> Parameters: 1(Long)",
        "2026-11-25 21:00:02.000 DEBUG 1 --- [nio-2] b : ==>  Preparing: SELECT 2 FROM b WHERE id = ?",
        "2026-11-25 21:00:02.010 DEBUG 1 --- [nio-2] b : ==> Parameters: 2(Long)",
        // Thread 1's close row arrives after thread 2's statement was emitted.
        "2026-11-25 21:00:01.010 DEBUG 1 --- [nio-1] a : <==      Total: 1",
        "2026-11-25 21:00:02.050 DEBUG 1 --- [nio-2] b : <==      Total: 1",
    ]);
    assert_eq!(analysis.entries.len(), 2);
    assert_eq!(analysis.entries[0].duration_ms, Some(10.0));
    assert_eq!(analysis.entries[1].duration_ms, Some(50.0));
    assert_eq!(analysis.entries[0].result_rows, Some(1));
    assert_eq!(analysis.entries[1].result_rows, Some(1));
}

#[test]
fn close_row_duration_spans_a_minute_boundary() {
    let analysis = parse_lines(&[
        "2026-11-25 21:00:59.998 DEBUG 1 --- [nio-1] a : ==>  Preparing: SELECT 1 FROM a WHERE id = ?",
        "2026-11-25 21:00:59.999 DEBUG 1 --- [nio-1] a : ==> Parameters: 1(Long)",
        "2026-11-25 21:01:00.004 DEBUG 1 --- [nio-1] a : <==      Total: 1",
    ]);
    assert_eq!(analysis.entries.len(), 1);
    assert_eq!(analysis.entries[0].duration_ms, Some(6.0));
    assert_eq!(analysis.entries[0].result_rows, Some(1));
}

#[test]
fn statement_without_a_close_row_keeps_duration_unset() {
    let analysis = parse_lines(&[
        "2026-11-25 21:00:01.001 DEBUG 1 --- [nio-1] a : ==>  Preparing: SELECT 1 FROM a WHERE id = ?",
        "2026-11-25 21:00:01.002 DEBUG 1 --- [nio-1] a : ==> Parameters: 1(Long)",
    ]);
    assert_eq!(analysis.entries.len(), 1);
    assert_eq!(analysis.entries[0].duration_ms, None);
    assert_eq!(analysis.entries[0].result_rows, None);
    assert_eq!(analysis.entries[0].affected_rows, None);
}

// ---------------------------------------------------------------------------
// p6spy (reference LogDurationParserTest / ExecutionTimeParserTest / real
// scenario fixture, 6 cases)
// ---------------------------------------------------------------------------

#[test]
fn p6spy_pipe_row_parses_sql_duration_and_epoch() {
    let analysis = parse_lines(&["1723456789012|123|statement|select * from user where id = 1"]);
    assert_eq!(analysis.entries.len(), 1);
    let entry = &analysis.entries[0];
    assert_eq!(entry.family, Family::P6Spy);
    assert_eq!(entry.sql, "select * from user where id = 1");
    assert_eq!(entry.duration_ms, Some(123.0));
    assert_eq!(entry.timestamp_ms, Some(1_723_456_789_012));
    assert_eq!(entry.class, EntryClass::Select);
}

#[test]
fn p6spy_effective_sql_field_is_used_as_restored_form() {
    let analysis = parse_lines(&[
        "1723456789012|5|statement|select * from t where id = ?|select * from t where id = 7|",
    ]);
    let entry = &analysis.entries[0];
    assert_eq!(entry.raw_sql, "select * from t where id = ?");
    assert_eq!(entry.sql, "select * from t where id = 7");
    assert_eq!(entry.confidence, Confidence::Exact);
}

#[test]
fn p6spy_log_prefixed_exec_row() {
    let analysis = parse_lines(&[
        "2026-08-31 17:31:00.123  INFO 1234 --- [pool-2-thread-1] p6spy : #1725000000000 | elapsed 5 ms | exec | INSERT INTO user_order (user_id, amount) VALUES (?, ?)",
    ]);
    let entry = &analysis.entries[0];
    assert_eq!(entry.family, Family::P6Spy);
    assert_eq!(entry.class, EntryClass::Insert);
    assert_eq!(entry.duration_ms, Some(5.0));
    assert_eq!(entry.confidence, Confidence::Partial);
}

#[test]
fn p6spy_result_row_does_not_create_an_entry_but_contributes_duration() {
    let analysis = parse_lines(&[
        "1723456789012|123|statement|select 1|select 1|",
        "1723456789013|7|result|",
    ]);
    assert_eq!(analysis.entries.len(), 1);
    assert_eq!(analysis.entries[0].duration_ms, Some(123.0));
}

#[test]
fn p6spy_epoch_without_log_prefix() {
    let analysis = parse_lines(&["1695000000123|0|statement|select 1 from dual"]);
    let entry = &analysis.entries[0];
    assert_eq!(entry.timestamp_ms, Some(1_695_000_000_123));
    assert_eq!(entry.duration_ms, Some(0.0));
}

#[test]
fn p6spy_batch_category_is_sql() {
    let analysis = parse_lines(&["1712345678901|12|batch|update t set a = ?|update t set a = 1|"]);
    assert_eq!(analysis.entries.len(), 1);
    assert_eq!(analysis.entries[0].class, EntryClass::Update);
    assert_eq!(analysis.entries[0].sql, "update t set a = 1");
}

// ---------------------------------------------------------------------------
// Hibernate (reference HibernateLogParserTest, 8 cases)
// ---------------------------------------------------------------------------

#[test]
fn hibernate_sql_with_two_binding_parameters() {
    let analysis = parse_lines(&[
        "Hibernate: select user0_.id, user0_.name from users user0_ where user0_.id=? and user0_.name=?",
        "binding parameter [1] as [BIGINT] - [42]",
        "binding parameter [2] as [VARCHAR] - [John Smith]",
    ]);
    assert_eq!(analysis.entries.len(), 1);
    assert_eq!(
        analysis.entries[0].sql,
        "select user0_.id, user0_.name from users user0_ where user0_.id=42 and user0_.name='John Smith'"
    );
    assert_eq!(analysis.entries[0].family, Family::Hibernate);
}

#[test]
fn hibernate_varchar_parameter_escapes_quotes() {
    let analysis = parse_lines(&[
        "Hibernate: select * from users where name=?",
        "binding parameter [1] as [VARCHAR] - [O'Brien]",
    ]);
    assert_eq!(
        analysis.entries[0].sql,
        "select * from users where name='O''Brien'"
    );
}

#[test]
fn hibernate_timestamp_parameter_uses_ansi_literal() {
    let analysis = parse_lines(&[
        "Hibernate: select * from users where created_at > ?",
        "binding parameter [1] as [TIMESTAMP] - [2025-01-01 12:00:00.0]",
    ]);
    assert_eq!(
        analysis.entries[0].sql,
        "select * from users where created_at > TIMESTAMP '2025-01-01 12:00:00.0'"
    );
}

#[test]
fn hibernate_null_and_empty_string_parameters() {
    let analysis = parse_lines(&[
        "Hibernate: select * from users where email = ? and nickname = ?",
        "binding parameter [1] as [VARCHAR] - [null]",
        "binding parameter [2] as [VARCHAR] - []",
    ]);
    assert_eq!(analysis.entries[0].params, vec!["NULL", "''"]);
}

#[test]
fn hibernate_comment_prefix_is_stripped() {
    let analysis = parse_lines(&[
        "/* load com.example.User */ select user0_.id from users user0_ where user0_.id=?",
        "binding parameter [1] as [BIGINT] - [1]",
    ]);
    assert_eq!(analysis.entries.len(), 1);
    assert_eq!(
        analysis.entries[0].sql,
        "select user0_.id from users user0_ where user0_.id=1"
    );
}

#[test]
fn hibernate_bare_sql_row_is_recognized() {
    let analysis = parse_lines(&["select 1 from dual"]);
    assert_eq!(analysis.entries.len(), 1);
    assert_eq!(analysis.entries[0].family, Family::Hibernate);
}

#[test]
fn hibernate_multiline_sql_block() {
    let analysis = parse_lines(&[
        "Hibernate: select id",
        "from users",
        "where id = ?",
        "binding parameter [1] as [BIGINT] - [7]",
    ]);
    assert_eq!(analysis.entries.len(), 1);
    assert_eq!(analysis.entries[0].sql, "select id from users where id = 7");
    assert_eq!(analysis.entries[0].line_no, 1);
}

#[test]
fn hibernate_orphan_binding_row_is_noise() {
    let analysis = parse_lines(&["binding parameter [1] as [BIGINT] - [1]"]);
    assert!(analysis.entries.is_empty());
    assert!(analysis.unparsed_lines.is_empty());
}

// ---------------------------------------------------------------------------
// JdbcTemplate (reference JdbcTemplateLogParserTest, 8 cases)
// ---------------------------------------------------------------------------

#[test]
fn jdbc_prepared_statement_parameters_sorted_by_index() {
    let analysis = parse_lines(&[
        "2026-08-24 15:46:42.444 DEBUG o.s.j.c.JdbcTemplate - Executing prepared SQL statement [SELECT \"owner\" FROM t WHERE deleted = ? AND id = ?]",
        "2026-08-24 15:46:42.445 TRACE o.s.j.c.StatementCreatorUtils - Setting SQL statement parameter value: column index 2, parameter value [abc], value class [java.lang.String], SQL type unknown",
        "2026-08-24 15:46:42.446 TRACE o.s.j.c.StatementCreatorUtils - Setting SQL statement parameter value: column index 1, parameter value [0], value class [java.lang.Integer], SQL type unknown",
    ]);
    assert_eq!(analysis.entries.len(), 1);
    assert_eq!(analysis.entries[0].params, vec!["0", "'abc'"]);
    assert_eq!(
        analysis.entries[0].sql,
        "SELECT \"owner\" FROM t WHERE deleted = 0 AND id = 'abc'"
    );
}

#[test]
fn jdbc_executing_sql_update_row() {
    let analysis = parse_lines(&[
        "Executing SQL update [UPDATE users SET name = ? WHERE id = ?]",
        "Setting SQL statement parameter value: column index 1, parameter value [Alice], value class [java.lang.String], SQL type unknown",
        "Setting SQL statement parameter value: column index 2, parameter value [42], value class [java.lang.Long], SQL type unknown",
    ]);
    assert_eq!(
        analysis.entries[0].sql,
        "UPDATE users SET name = 'Alice' WHERE id = 42"
    );
    assert_eq!(analysis.entries[0].class, EntryClass::Update);
}

#[test]
fn jdbc_stored_procedure_call() {
    let analysis = parse_lines(&[
        "Calling stored procedure [{call sp_get_user(?)}]",
        "Setting SQL statement parameter value: column index 1, parameter value [100], value class [java.lang.Integer], SQL type unknown",
    ]);
    assert_eq!(analysis.entries[0].sql, "{call sp_get_user(100)}");
    assert_eq!(analysis.entries[0].class, EntryClass::Other);
}

#[test]
fn jdbc_named_parameter_row() {
    let analysis =
        parse_lines(&["sql = SELECT * FROM t WHERE a = :a and b = :b , parameters = {a=1, b=x}"]);
    assert_eq!(
        analysis.entries[0].sql,
        "SELECT * FROM t WHERE a = 1 and b = 'x'"
    );
    assert_eq!(analysis.entries[0].params, vec!["1", "'x'"]);
}

#[test]
fn jdbc_multiline_statement_keeps_identifier_brackets() {
    let analysis = parse_lines(&[
        "Executing prepared SQL statement [SELECT a[1] FROM t WHERE x = ?",
        " and y = ?]",
        "Setting SQL statement parameter value: column index 1, parameter value [1], value class [java.lang.Integer], SQL type unknown",
        "Setting SQL statement parameter value: column index 2, parameter value [2], value class [java.lang.Integer], SQL type unknown",
    ]);
    assert_eq!(analysis.entries.len(), 1);
    assert_eq!(
        analysis.entries[0].sql,
        "SELECT a[1] FROM t WHERE x = 1\n and y = 2"
    );
}

#[test]
fn jdbc_null_parameter() {
    let analysis = parse_lines(&[
        "Executing prepared SQL statement [SELECT * FROM users WHERE deleted = ?]",
        "Setting SQL statement parameter value: column index 1, parameter value [null], value class [null], SQL type unknown",
    ]);
    assert_eq!(
        analysis.entries[0].sql,
        "SELECT * FROM users WHERE deleted = NULL"
    );
}

#[test]
fn jdbc_bigdecimal_parameter_is_bare() {
    let analysis = parse_lines(&[
        "Executing prepared SQL statement [SELECT * FROM orders WHERE total > ?]",
        "Setting SQL statement parameter value: column index 1, parameter value [99.99], value class [java.math.BigDecimal], SQL type unknown",
    ]);
    assert_eq!(
        analysis.entries[0].sql,
        "SELECT * FROM orders WHERE total > 99.99"
    );
}

#[test]
fn jdbc_header_row_without_sql_text_is_ignored() {
    let analysis = parse_lines(&["Executing prepared SQL query"]);
    assert!(analysis.entries.is_empty());
}

// ---------------------------------------------------------------------------
// Dedupe (3 cases)
// ---------------------------------------------------------------------------

fn mybatis_pair(id: u64) -> String {
    format!("==>  Preparing: SELECT id FROM user WHERE id = ?\n==> Parameters: {id}(Long)")
}

#[test]
fn dedupe_identical_sql_and_params_are_grouped() {
    let text = [mybatis_pair(7), mybatis_pair(7), mybatis_pair(7)].join("\n");
    let analysis = parse(&text, &ParseOptions::default());
    assert_eq!(analysis.entries.len(), 3);
    assert_eq!(analysis.duplicates.len(), 1);
    let group = &analysis.duplicates[0];
    assert_eq!(group.count, 3);
    assert_eq!(group.line_nos, vec![1, 3, 5]);
    assert_eq!(group.first_line_no, 1);
    assert_eq!(group.last_line_no, 5);
    assert_eq!(group.params, vec!["7"]);
}

#[test]
fn dedupe_different_params_are_distinct_by_default() {
    let text = [mybatis_pair(1), mybatis_pair(2)].join("\n");
    let analysis = parse(&text, &ParseOptions::default());
    assert!(analysis.duplicates.is_empty());
}

#[test]
fn dedupe_ignore_params_groups_templates() {
    let text = [mybatis_pair(1), mybatis_pair(2), mybatis_pair(3)].join("\n");
    let opts = ParseOptions {
        ignore_params_in_dedupe: true,
        ..ParseOptions::default()
    };
    let analysis = parse(&text, &opts);
    assert_eq!(analysis.duplicates.len(), 1);
    assert_eq!(analysis.duplicates[0].count, 3);
    assert!(analysis.duplicates[0].key.contains("id = ?"));
}

#[test]
fn dedupe_window_splits_groups() {
    let text = [
        "2024-01-15 10:00:00.000 INFO x - ==>  Preparing: SELECT 1",
        "2024-01-15 10:00:05.000 INFO x - ==>  Preparing: SELECT 1",
    ]
    .join("\n");
    let opts = ParseOptions {
        dedupe_window_ms: Some(1000),
        ..ParseOptions::default()
    };
    let analysis = parse(&text, &opts);
    assert!(analysis.duplicates.is_empty());
}

// ---------------------------------------------------------------------------
// N+1 (2 cases)
// ---------------------------------------------------------------------------

#[test]
fn n_plus_one_groups_same_template() {
    let text = (0..6).map(mybatis_pair).collect::<Vec<_>>().join("\n");
    let opts = ParseOptions {
        n_plus_one_threshold: 5,
        ..ParseOptions::default()
    };
    let analysis = parse(&text, &opts);
    assert_eq!(analysis.n_plus_one.len(), 1);
    let group = &analysis.n_plus_one[0];
    assert_eq!(group.count, 6);
    assert!(group.template.ends_with("WHERE id = ?"));
    assert_eq!(group.line_nos.len(), 6);
}

#[test]
fn n_plus_one_below_threshold_is_empty() {
    let text = (0..3).map(mybatis_pair).collect::<Vec<_>>().join("\n");
    let opts = ParseOptions {
        n_plus_one_threshold: 5,
        ..ParseOptions::default()
    };
    let analysis = parse(&text, &opts);
    assert!(analysis.n_plus_one.is_empty());
}

// ---------------------------------------------------------------------------
// Slow queries (2 cases)
// ---------------------------------------------------------------------------

#[test]
fn slow_query_collects_entries_over_threshold() {
    let analysis = parse_lines(&[
        "==>  Preparing: SELECT * FROM big_table WHERE id = ?",
        "==> Parameters: 1(Long)",
        "TimeCount: 1500ms",
    ]);
    assert_eq!(analysis.entries.len(), 1);
    assert_eq!(analysis.entries[0].duration_ms, Some(1500.0));
    assert_eq!(analysis.slow.len(), 1);
    assert_eq!(analysis.slow[0].line_no, 1);
}

#[test]
fn fast_query_is_not_slow() {
    let analysis = parse_lines(&["1723456789012|5|statement|select 1|select 1|"]);
    assert_eq!(analysis.entries[0].duration_ms, Some(5.0));
    assert!(analysis.slow.is_empty());
}

// ---------------------------------------------------------------------------
// Errors (2 cases)
// ---------------------------------------------------------------------------

#[test]
fn sql_exception_row_becomes_unresolved_error_entry() {
    let analysis = parse_lines(&[
        "java.sql.SQLException: Table 'demo.user' doesn't exist",
        "\tat com.example.Dao.find(Dao.java:10)",
        "\tat java.base/java.lang.Thread.run(Thread.java:833)",
    ]);
    assert_eq!(analysis.entries.len(), 1);
    let entry = &analysis.entries[0];
    assert_eq!(entry.class, EntryClass::Error);
    assert_eq!(entry.confidence, Confidence::Unresolved);
    assert_eq!(entry.sql, "");
    assert_eq!(entry.family, Family::Unknown);
    assert_eq!(analysis.errors.len(), 1);
    assert!(analysis.unparsed_lines.is_empty());
}

#[test]
fn mybatis_error_block_does_not_become_sql() {
    let analysis = parse_lines(&[
        "### Error querying database.  Cause: java.sql.SQLSyntaxErrorException: bad SQL",
        "### The error may exist in com/example/UserMapper.xml",
        "### SQL: select * from missing_table where id = ?",
        "### Cause: java.sql.SQLSyntaxErrorException: bad SQL",
    ]);
    assert_eq!(analysis.errors.len(), 4);
    assert!(analysis.entries.iter().all(|entry| entry.sql.is_empty()));
    assert!(analysis.unparsed_lines.is_empty());
}

// ---------------------------------------------------------------------------
// Unparsed lines (2 cases)
// ---------------------------------------------------------------------------

#[test]
fn orphan_parameters_row_is_reported_as_unparsed() {
    let analysis = parse_lines(&["==> Parameters: 1(Long)"]);
    assert!(analysis.entries.is_empty());
    assert_eq!(analysis.unparsed_lines.len(), 1);
    assert_eq!(
        analysis.unparsed_lines[0].message_key,
        message_keys::UNPARSED_ORPHAN_PARAMETERS
    );
    assert_eq!(analysis.unparsed_lines[0].line_no, 1);
}

#[test]
fn sql_looking_row_without_a_family_is_reported() {
    let opts = ParseOptions {
        recognize_bare_sql: false,
        ..ParseOptions::default()
    };
    let analysis = parse_with(&["select * from t where id = 1"], &opts);
    assert!(analysis.entries.is_empty());
    assert_eq!(analysis.unparsed_lines.len(), 1);
    assert_eq!(
        analysis.unparsed_lines[0].message_key,
        message_keys::UNPARSED_UNRECOGNIZED_SQL
    );
}

// ---------------------------------------------------------------------------
// Cross-family / robustness (4 cases)
// ---------------------------------------------------------------------------

#[test]
fn mixed_family_console_keeps_families_apart() {
    let analysis = parse_lines(&[
        "==>  Preparing: SELECT 1",
        "Hibernate: select 2",
        "Executing prepared SQL statement [SELECT 3]",
        "1723456789012|1|statement|SELECT 4|SELECT 4|",
    ]);
    assert_eq!(analysis.entries.len(), 4);
    let families: Vec<Family> = analysis.entries.iter().map(|e| e.family).collect();
    assert_eq!(
        families,
        vec![
            Family::MyBatis,
            Family::Hibernate,
            Family::JdbcTemplate,
            Family::P6Spy
        ]
    );
}

#[test]
fn stack_trace_after_sql_is_not_sql() {
    let analysis = parse_lines(&[
        "==>  Preparing: SELECT * FROM t WHERE id = ?",
        "\tat com.example.Mapper.find(Mapper.java:1)",
        "==> Parameters: 1(Long)",
    ]);
    assert_eq!(analysis.entries.len(), 1);
    assert_eq!(analysis.entries[0].sql, "SELECT * FROM t WHERE id = 1");
}

#[test]
fn message_keys_are_dotted_lower_case() {
    let analysis = parse_lines(&["==> Parameters: 1(Long)", "select * from t where id = 1"]);
    let mut keys: Vec<String> = analysis
        .entries
        .iter()
        .flat_map(|entry| entry.message_keys.iter().cloned())
        .chain(
            analysis
                .unparsed_lines
                .iter()
                .map(|line| line.message_key.clone()),
        )
        .collect();
    if analysis
        .entries
        .iter()
        .any(|e| e.confidence == Confidence::Partial)
    {
        keys.push(message_keys::ENTRY_PARAMS_MISSING.to_string());
    }
    assert!(!keys.is_empty());
    for key in keys {
        assert!(
            key.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_'),
            "unexpected key shape: {key}"
        );
    }
}

#[test]
fn serde_round_trip_and_message_key_defaults() {
    let analysis = parse_lines(&[
        "==>  Preparing: SELECT * FROM t WHERE id = ?",
        "==> Parameters: 1(Long)",
        "java.sql.SQLException: boom",
    ]);
    let json = serde_json::to_string(&analysis).expect("serialize");
    let back: LogAnalysis = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, analysis);
    let opts: ParseOptions = serde_json::from_str("{}").expect("defaults");
    assert_eq!(opts, ParseOptions::default());
    let entry_json = serde_json::to_string(&analysis.entries[0]).expect("entry");
    assert!(entry_json.contains("\"family\":\"mybatis\""));
    assert!(entry_json.contains("\"class\":\"select\""));
    assert!(entry_json.contains("\"confidence\":\"exact\""));
}

// ---------------------------------------------------------------------------
// Performance and boundaries (node 3)
// ---------------------------------------------------------------------------

#[test]
fn ten_thousand_lines_parse_quickly() {
    let mut text = String::with_capacity(600_000);
    for i in 0..5000u64 {
        text.push_str("==>  Preparing: SELECT id, name FROM user WHERE id = ?\n");
        text.push_str(&format!("==> Parameters: {i}(Long)\n"));
    }
    let started = Instant::now();
    let analysis = parse(&text, &ParseOptions::default());
    let elapsed = started.elapsed();
    assert_eq!(analysis.entries.len(), 5000);
    assert!(
        elapsed < Duration::from_secs(10),
        "10k lines took {elapsed:?}"
    );
    // One template repeated 5000 times (default threshold 5).
    assert_eq!(analysis.n_plus_one.len(), 1);
    assert_eq!(analysis.n_plus_one[0].count, 5000);
}

#[test]
fn crlf_line_endings_are_normalised_by_the_splitter() {
    let text = "==>  Preparing: SELECT * FROM t WHERE id = ?\r\n==> Parameters: 9(Long)\r\n";
    let analysis = parse(text, &ParseOptions::default());
    assert_eq!(analysis.entries.len(), 1);
    assert_eq!(analysis.entries[0].sql, "SELECT * FROM t WHERE id = 9");
}

#[test]
fn unicode_literals_and_comment_question_marks() {
    let analysis = parse_lines(&[
        "==>  Preparing: SELECT * FROM t WHERE name = ? -- 名称 ?",
        "==> Parameters: 张三(String)",
    ]);
    assert_eq!(analysis.entries.len(), 1);
    assert_eq!(
        analysis.entries[0].sql,
        "SELECT * FROM t WHERE name = '张三' -- 名称 ?"
    );
    assert_eq!(analysis.entries[0].confidence, Confidence::Exact);
}

#[test]
fn max_sql_len_truncation_is_partial_and_preserves_the_line() {
    let opts = ParseOptions {
        max_sql_len: 20,
        ..ParseOptions::default()
    };
    let analysis = parse_with(
        &[
            "==>  Preparing: SELECT * FROM a_very_long_table WHERE id = ?",
            "==> Parameters: 1(Long)",
        ],
        &opts,
    );
    assert_eq!(analysis.entries.len(), 1);
    let entry = &analysis.entries[0];
    assert_eq!(entry.raw_sql, "SELECT * FROM a_very");
    assert_eq!(entry.confidence, Confidence::Partial);
    assert!(
        entry
            .message_keys
            .iter()
            .any(|key| key == message_keys::ENTRY_SQL_TRUNCATED)
    );
    assert!(entry.raw_line.is_some());
}

#[test]
fn unfinished_jdbc_bracket_is_kept_as_partial() {
    let analysis = parse_lines(&["Executing prepared SQL statement [SELECT * FROM t WHERE id = ?"]);
    assert_eq!(analysis.entries.len(), 1);
    let entry = &analysis.entries[0];
    assert_eq!(entry.sql, "SELECT * FROM t WHERE id = ?");
    assert_eq!(entry.confidence, Confidence::Partial);
    assert!(
        entry
            .message_keys
            .iter()
            .any(|key| key == message_keys::ENTRY_MULTILINE_UNCLOSED)
    );
    assert!(entry.raw_line.is_some());
}

// ---------------------------------------------------------------------------
// Log source: logger-derived class/method, thread, correlation id
// ---------------------------------------------------------------------------

fn parse_sourced(lines: &[&str]) -> ParsedLog {
    parse_with_sources(&lines.join("\n"), &ParseOptions::default())
}

#[test]
fn mybatis_logger_name_yields_class_and_method_per_entry() {
    let parsed = parse_sourced(&[
        "2026-11-25 21:00:01.001 DEBUG 4242 --- [nio-8080-exec-1] c.e.m.mapper.UserMapper.selectById : ==>  Preparing: SELECT * FROM user WHERE id = ?",
        "2026-11-25 21:00:01.002 DEBUG 4242 --- [nio-8080-exec-1] c.e.m.mapper.UserMapper.selectById : ==> Parameters: 7(Long)",
        "2026-11-25 21:00:01.003 DEBUG 4242 --- [nio-8080-exec-1] c.e.m.mapper.UserMapper.selectById : <==      Total: 1",
        "2026-11-25 21:00:02.000 DEBUG 4242 --- [nio-8080-exec-2] c.e.m.order.OrderMapper.listAll : ==>  Preparing: SELECT * FROM orders",
        "2026-11-25 21:00:02.001 DEBUG 4242 --- [nio-8080-exec-2] c.e.m.order.OrderMapper.listAll : <==      Total: 3",
    ]);
    assert_eq!(parsed.analysis.entries.len(), 2);

    let first = parsed
        .sources
        .get(1)
        .expect("source of the first statement");
    let source = first.source.as_ref().expect("logger source");
    assert_eq!(source.class_name, "c.e.m.mapper.UserMapper");
    assert_eq!(source.class_simple, "UserMapper");
    assert_eq!(source.method, "selectById");
    assert_eq!(source.key(), "c.e.m.mapper.UserMapper#selectById");
    assert_eq!(first.thread.as_deref(), Some("nio-8080-exec-1"));
    assert!(first.request_id.is_none());

    let second = parsed
        .sources
        .get(4)
        .expect("source of the second statement");
    let source = second.source.as_ref().expect("logger source");
    assert_eq!(source.class_simple, "OrderMapper");
    assert_eq!(source.method, "listAll");
    assert_eq!(second.thread.as_deref(), Some("nio-8080-exec-2"));
    assert_eq!(parsed.sources.len(), 2, "只存有来源的行");
}

#[test]
fn correlation_ids_are_read_in_every_documented_shape() {
    let shapes = [
        (
            "2026-11-25 21:00:01.001 INFO 4242 --- [http-nio-8080-exec-3] [traceId=4bf92f3577b34da6] c.e.m.UserMapper.selectById : ==>  Preparing: SELECT 1",
            "4bf92f3577b34da6",
        ),
        (
            "2026-11-25 21:00:01.001 INFO 4242 --- [http-nio-8080-exec-3] [requestId: r-42] c.e.m.UserMapper.selectById : ==>  Preparing: SELECT 1",
            "r-42",
        ),
        (
            "2026-11-25 21:00:01.001 INFO 4242 --- [http-nio-8080-exec-3] [X-Request-Id='abc-9'] c.e.m.UserMapper.selectById : ==>  Preparing: SELECT 1",
            "abc-9",
        ),
        (
            "2026-11-25 21:00:01.001 INFO 4242 --- [http-nio-8080-exec-3] [traceid=a.b.c] c.e.m.UserMapper.selectById : ==>  Preparing: SELECT 1",
            "a.b.c",
        ),
    ];
    for (line, expected) in shapes {
        let parsed = parse_sourced(&[line]);
        let line_source = parsed.sources.get(1).expect("provenance of the row");
        assert_eq!(line_source.request_id.as_deref(), Some(expected), "{line}");
        assert!(
            line_source.source.is_some(),
            "logger source survives the MDC bracket: {line}"
        );
        assert_eq!(
            line_source.thread.as_deref(),
            Some("http-nio-8080-exec-3"),
            "{line}"
        );
    }
}

#[test]
fn a_sql_predicate_is_not_a_correlation_id() {
    let parsed = parse_sourced(&[
        "2026-11-25 21:00:01.001 DEBUG 4242 --- [main] c.e.UserMapper.selectById : ==>  Preparing: SELECT * FROM t WHERE request_id = 'abc' AND traceId=?",
        "2026-11-25 21:00:01.002 DEBUG 4242 --- [main] c.e.UserMapper.selectById : ==> Parameters: 1(Long), 2(Long)",
    ]);
    let line_source = parsed.sources.get(1).expect("provenance of the row");
    assert!(
        line_source.request_id.is_none(),
        "SQL 谓词里的 request_id/traceId 不是请求来源"
    );
    assert_eq!(line_source.source.as_ref().unwrap().method, "selectById");
}

#[test]
fn non_mybatis_families_state_no_statement_source() {
    let parsed = parse_sourced(&[
        "Hibernate: select id from users where id=?",
        "2026-11-25 21:00:01.002 DEBUG 4242 --- [main] org.springframework.jdbc.core.JdbcTemplate : Executing prepared SQL statement [select id from users]",
        "2026-11-25 21:00:01.003  INFO 4242 --- [pool-2-thread-1] p6spy : #1725000000000 | elapsed 5 ms | exec | INSERT INTO user_order (user_id) VALUES (?)",
    ]);
    assert_eq!(parsed.analysis.entries.len(), 3);
    for entry in &parsed.analysis.entries {
        assert!(
            parsed.sources.source(entry.line_no).is_none(),
            "logger 名不是 statement id，取不到来源：line {}",
            entry.line_no
        );
    }
    assert!(
        parsed.sources.get(1).is_none(),
        "无前缀的 Hibernate 行连线程都没有"
    );
    assert_eq!(
        parsed.sources.get(2).unwrap().thread.as_deref(),
        Some("main"),
        "线程仍可读"
    );
    assert_eq!(
        parsed.sources.get(3).unwrap().thread.as_deref(),
        Some("pool-2-thread-1")
    );
}

#[test]
fn a_row_without_provenance_is_absent_rather_than_guessed() {
    let parsed = parse_sourced(&["==>  Preparing: SELECT 1"]);
    assert_eq!(parsed.analysis.entries.len(), 1);
    assert!(parsed.sources.is_empty());
    assert!(parsed.sources.get(1).is_none());
}

#[test]
fn thread_falls_back_to_the_keyword_bounded_bracket() {
    let parsed = parse_sourced(&[
        "2026-11-25 21:00:01.001 INFO 4242 [exec-2] c.e.UserMapper.selectById : ==>  Preparing: SELECT 1",
    ]);
    assert_eq!(
        parsed.sources.get(1).unwrap().thread.as_deref(),
        Some("exec-2")
    );
}

#[test]
fn by_line_keeps_rows_that_are_not_analysis_entries() {
    let text = "\
2026-11-25 21:00:01.001 DEBUG 4242 --- [main] c.e.m.UserMapper.selectById : ==>  Preparing: SELECT 1
2026-11-25 21:00:01.002 DEBUG 4242 --- [main] c.e.m.UserMapper.selectById : <==      Total: 1";
    let single = parse_with_sources(text, &ParseOptions::default()).sources;
    let pairs: Vec<(usize, LineSource)> = text
        .lines()
        .enumerate()
        .map(|(index, line)| (index + 1, source_of_line(line)))
        .collect();
    let mapped = by_line(pairs);
    assert_eq!(single.len(), 1, "只跟分析条目 join");
    assert_eq!(mapped.len(), 2, "按行建索引保留 close 行");
    assert_eq!(single.get(1), mapped.get(1));
    assert_eq!(single.get(2), None);
    assert!(mapped.get(2).is_some());
}
