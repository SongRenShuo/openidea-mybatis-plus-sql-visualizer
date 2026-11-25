//! Parity tests for T1-11.
//!
//! Expected values are derived from the reference Kotlin suite
//! `src/test/kotlin/cn/song/mybatisplus/visualizer/log/` (MyBatisLogParserTest,
//! HibernateLogParserTest, JdbcTemplateLogParserTest, ExecutionTimeParserTest,
//! LogDurationParserTest, SqlErrorLineRecognizerTest). The Rust expectations
//! assert the same SQL/params/duration values, not the Kotlin API shape.

use super::*;

fn parse_lines(lines: &[&str]) -> LogAnalysis {
    parse(&lines.join("\n"), &ParseOptions::default())
}

fn parse_with(lines: &[&str], opts: &ParseOptions) -> LogAnalysis {
    parse(&lines.join("\n"), opts)
}

fn entries_of(analysis: &LogAnalysis, family: Family) -> Vec<&LogEntry> {
    analysis
        .entries
        .iter()
        .filter(|entry| entry.family == family)
        .collect()
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
    assert_eq!(analysis.entries[0].affected_rows, Some(3));
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
    let text = (0..6)
        .map(|i| mybatis_pair(i))
        .collect::<Vec<_>>()
        .join("\n");
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
    let text = (0..3)
        .map(|i| mybatis_pair(i))
        .collect::<Vec<_>>()
        .join("\n");
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
