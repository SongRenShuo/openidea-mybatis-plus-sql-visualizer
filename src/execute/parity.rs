//! L8 parity fixtures (T5-7): five categories, ≥10 fixed input/output cases each.
//!
//! Why this lives in the library and not under `tests/`: the package's
//! `[lib] crate-type` is `["cdylib"]` only, so an integration test cannot link
//! the crate (`E0433: unlinked crate`), and L8 may not change `Cargo.toml` (only
//! the controller edits it, via registration). These fixtures therefore run as
//! ordinary `cargo test` unit tests. The crate-independent `#[ignore]` jar
//! end-to-end test does live at `tests/l8_parity.rs` (it needs only `std` +
//! `serde_json`).
//!
//! Categories:
//!   1. wrapper renders (exact)
//!   2. wrapper confidence / degradation (partial · unresolved)
//!   3. EXPLAIN tree/table views
//!   4. EXPLAIN error classification
//!   5. schema cache invalidation / offline / isolation

use crate::analyze::wrapper::{Confidence, EntityColumns, render};
use crate::core::dialect::{self, Dialect};
use crate::execute::explain::{
    ExplainErrorKind, ExplainRow, build_plan, classify, rows_from_columns,
};
use crate::execute::schema::{SchemaCache, SchemaMiss, TableSchema};

fn cols() -> EntityColumns {
    EntityColumns::new("User", "sys_user")
        .with_column("id", "id", "bigint")
        .with_column("name", "name", "varchar")
        .with_column("status", "status", "varchar")
        .with_column("age", "age", "int")
        .with_column("createdAt", "created_at", "timestamp")
}

fn dialect_named(name: &str) -> &'static Dialect {
    dialect::by_name(name).unwrap_or_else(|| panic!("dialect {name}"))
}

// ---------------------------------------------------------------------------
// Category 1: wrapper renders (exact) — 12 cases
// ---------------------------------------------------------------------------

#[test]
fn category1_wrapper_exact_renders() {
    let cases: &[(&str, &str)] = &[
        (
            r#"new LambdaQueryWrapper<User>().eq(User::getStatus, "A")"#,
            "SELECT * FROM sys_user WHERE status = 'A'",
        ),
        (
            "new LambdaQueryWrapper<User>().ne(User::getAge, 18)",
            "SELECT * FROM sys_user WHERE age <> 18",
        ),
        (
            "new LambdaQueryWrapper<User>().ge(User::getAge, 18)",
            "SELECT * FROM sys_user WHERE age >= 18",
        ),
        (
            "new LambdaQueryWrapper<User>().lt(User::getAge, 18)",
            "SELECT * FROM sys_user WHERE age < 18",
        ),
        (
            r#"new LambdaQueryWrapper<User>().like(User::getName, "ab")"#,
            "SELECT * FROM sys_user WHERE name LIKE '%ab%'",
        ),
        (
            r#"new LambdaQueryWrapper<User>().in(User::getStatus, "A", "B")"#,
            "SELECT * FROM sys_user WHERE status IN ('A', 'B')",
        ),
        (
            "new LambdaQueryWrapper<User>().isNull(User::getName)",
            "SELECT * FROM sys_user WHERE name IS NULL",
        ),
        (
            "new LambdaQueryWrapper<User>().between(User::getAge, 18, 30)",
            "SELECT * FROM sys_user WHERE age BETWEEN 18 AND 30",
        ),
        (
            "new LambdaQueryWrapper<User>().orderByDesc(User::getCreatedAt)",
            "SELECT * FROM sys_user ORDER BY created_at DESC",
        ),
        (
            "new LambdaQueryWrapper<User>().limit(10)",
            "SELECT * FROM sys_user LIMIT 10",
        ),
        (
            r#"new LambdaQueryWrapper<User>().eq(User::getStatus, "A").or().eq(User::getStatus, "B")"#,
            "SELECT * FROM sys_user WHERE status = 'A' OR status = 'B'",
        ),
        (
            "new LambdaQueryWrapper<User>().and(w -> w.eq(User::getAge, 18).or().eq(User::getAge, 30))",
            "SELECT * FROM sys_user WHERE (age = 18 OR age = 30)",
        ),
    ];
    assert!(cases.len() >= 10);
    for (src, expected) in cases {
        let out = render(src, &cols(), dialect_named("MySQL"));
        assert_eq!(out.confidence, Confidence::Exact, "src={src}\n{out:?}");
        assert_eq!(out.sql.as_deref(), Some(*expected), "src={src}");
    }
}

// ---------------------------------------------------------------------------
// Category 2: wrapper confidence / degradation — 12 cases + table-unknown
// ---------------------------------------------------------------------------

#[test]
fn category2_wrapper_degradation() {
    let mysql = dialect_named("MySQL");

    // (source, expected confidence, expected sql, expected message_key)
    let cases: &[(&str, Confidence, Option<&str>, &str)] = &[
        (
            r#"new LambdaUpdateWrapper<User>().eq(User::getStatus, "A")"#,
            Confidence::Partial,
            Some("WHERE status = 'A'"),
            "wrapper.update_fragment_only",
        ),
        (
            r#"new LambdaQueryWrapper<User>().eq(User::getStatus, statusVar)"#,
            Confidence::Partial,
            Some("SELECT * FROM sys_user WHERE status = ?"),
            "wrapper.param_value",
        ),
        (
            r#"new LambdaQueryWrapper<User>().eq(User::getNope, "A")"#,
            Confidence::Unresolved,
            None,
            "wrapper.unknown_field",
        ),
        (
            r#"new LambdaQueryWrapper<User>().apply("x")"#,
            Confidence::Partial,
            Some("SELECT * FROM sys_user"),
            "wrapper.unsupported_method",
        ),
        (
            r#"new LambdaQueryWrapper<User>().eq(Other::getStatus, "A")"#,
            Confidence::Partial,
            Some("SELECT * FROM sys_user WHERE status = 'A'"),
            "wrapper.receiver_mismatch",
        ),
        (
            "int x = 1;",
            Confidence::Unresolved,
            None,
            "wrapper.not_a_wrapper",
        ),
        (
            r#"new LambdaQueryWrapper<User>().in(User::getStatus, Arrays.asList("A", "B"))"#,
            Confidence::Partial,
            Some("SELECT * FROM sys_user WHERE status IN (?)"),
            "wrapper.param_value",
        ),
        (
            r#"new LambdaQueryWrapper<User>().like(User::getName, nameVar)"#,
            Confidence::Partial,
            Some("SELECT * FROM sys_user WHERE name LIKE ?"),
            "wrapper.param_value",
        ),
        (
            "new LambdaQueryWrapper<User>().orderByAsc(User::getNope)",
            Confidence::Unresolved,
            None,
            "wrapper.unknown_field",
        ),
        (
            "new LambdaQueryWrapper<User>().between(User::getAge, 18)",
            Confidence::Partial,
            Some("SELECT * FROM sys_user WHERE age BETWEEN 18 AND ?"),
            "wrapper.between_arity",
        ),
        (
            "new LambdaQueryWrapper<User>().in(User::getStatus)",
            Confidence::Partial,
            Some("SELECT * FROM sys_user WHERE status IN (NULL)"),
            "wrapper.empty_in",
        ),
        (
            r#"new LambdaQueryWrapper<User>().eq(false, User::getStatus, "A")"#,
            Confidence::Exact,
            Some("SELECT * FROM sys_user"),
            "",
        ),
    ];

    assert!(cases.len() >= 10);
    for (src, confidence, sql, key) in cases {
        let out = render(src, &cols(), mysql);
        assert_eq!(out.confidence, *confidence, "src={src}\n{out:?}");
        assert_eq!(out.sql.as_deref(), *sql, "src={src}\n{out:?}");
        if !key.is_empty() {
            assert!(
                out.message_keys.contains(&key.to_string()),
                "src={src} missing key {key}; got {:?}",
                out.message_keys
            );
        }
    }

    // A missing table is its own unresolved reason.
    let no_table = EntityColumns::new("User", "");
    let out = render(
        r#"new LambdaQueryWrapper<User>().eq(User::getStatus, "A")"#,
        &no_table,
        mysql,
    );
    assert_eq!(out.confidence, Confidence::Unresolved);
    assert!(
        out.message_keys
            .contains(&"wrapper.table_unknown".to_string())
    );
}

// ---------------------------------------------------------------------------
// Category 3: EXPLAIN tree/table views — 10 fixtures
// ---------------------------------------------------------------------------

fn erow(id: Option<i64>, parent: Option<i64>, op: &str, obj: &str) -> ExplainRow {
    ExplainRow {
        id,
        parent_id: parent,
        operation: op.to_string(),
        object: Some(obj.to_string()),
        cost: Some(2.0),
        rows: Some(5),
        extra: None,
    }
}

#[test]
fn category3_explain_views() {
    let fixtures: Vec<(Vec<ExplainRow>, usize, usize)> = vec![
        (vec![erow(Some(1), None, "SIMPLE", "t1")], 1, 1),
        (
            vec![
                erow(Some(1), None, "SIMPLE", "t1"),
                erow(Some(1), None, "SIMPLE", "t2"),
            ],
            2,
            2,
        ),
        (
            vec![
                erow(Some(1), None, "Nested Loop", "(root)"),
                erow(Some(2), Some(1), "Seq Scan", "t1"),
            ],
            1,
            2,
        ),
        (
            vec![
                erow(Some(1), None, "Nested Loop", "(root)"),
                erow(Some(2), Some(1), "Hash Join", "(join)"),
                erow(Some(3), Some(2), "Index Scan", "t1"),
            ],
            1,
            3,
        ),
        (vec![erow(None, None, "ALL", "t1")], 1, 1),
        (
            vec![erow(None, None, "ALL", "t1"), erow(None, None, "ALL", "t2")],
            2,
            2,
        ),
        (
            vec![
                erow(Some(1), None, "Gather", "(gather)"),
                erow(Some(2), Some(1), "Seq Scan", "a"),
                erow(Some(3), Some(1), "Seq Scan", "b"),
            ],
            1,
            3,
        ),
        (
            vec![
                erow(Some(1), None, "Root", "r"),
                erow(Some(2), Some(1), "Child", "c1"),
                erow(Some(3), Some(2), "Grandchild", "g1"),
                erow(Some(4), Some(2), "Grandchild", "g2"),
            ],
            1,
            4,
        ),
        (vec![erow(Some(10), None, "SIMPLE", "x")], 1, 1),
        (
            vec![
                erow(Some(1), None, "SIMPLE", "x"),
                erow(Some(2), Some(99), "SIMPLE", "orphan"),
            ],
            2,
            2,
        ),
    ];

    assert!(fixtures.len() >= 10);
    for (rows, roots, table_len) in fixtures {
        let plan = build_plan("MySQL", rows);
        assert_eq!(plan.table.len(), table_len, "table view keeps every row");
        assert_eq!(plan.tree.len(), roots, "root count");
        assert_eq!(plan.dialect, "MySQL");
    }
}

// ---------------------------------------------------------------------------
// Category 4: EXPLAIN error classification — 10 messages
// ---------------------------------------------------------------------------

#[test]
fn category4_explain_error_classification() {
    let cases: &[(&str, ExplainErrorKind)] = &[
        (
            "canceling statement due to statement timeout",
            ExplainErrorKind::Timeout,
        ),
        (
            "deadline exceeded while planning",
            ExplainErrorKind::Timeout,
        ),
        (
            "permission denied for table t",
            ExplainErrorKind::Permission,
        ),
        (
            "Access denied for user 'root'",
            ExplainErrorKind::Permission,
        ),
        ("syntax error near 'FORM'", ExplainErrorKind::Syntax),
        ("unexpected token: FROM", ExplainErrorKind::Syntax),
        ("connection refused", ExplainErrorKind::Connection),
        ("Unknown host db.local", ExplainErrorKind::Connection),
        ("a totally unknown failure", ExplainErrorKind::Other),
        (
            "connection timeout exceeded",
            ExplainErrorKind::Timeout, // precedence
        ),
    ];
    assert!(cases.len() >= 10);
    for (message, kind) in cases {
        assert_eq!(classify(message), *kind, "message={message}");
    }
}

// ---------------------------------------------------------------------------
// Category 5: schema cache — 10 scenarios + a column mapping
// ---------------------------------------------------------------------------

fn table(name: &str) -> TableSchema {
    TableSchema::new(name)
        .with_column("id", "bigint", false)
        .with_column("name", "varchar", true)
}

#[test]
fn category5_schema_cache() {
    // 1 alter
    let mut c = SchemaCache::new();
    c.put("MySQL", table("sys_user"));
    assert_eq!(
        c.invalidate_on_ddl("ALTER TABLE sys_user ADD k INT"),
        vec!["sys_user".to_string()]
    );
    assert!(c.lookup("MySQL", "sys_user").is_err());

    // 2 drop
    let mut c = SchemaCache::new();
    c.put("MySQL", table("sys_user"));
    assert_eq!(
        c.invalidate_on_ddl("DROP TABLE IF EXISTS `sys_user`"),
        vec!["sys_user".to_string()]
    );

    // 3 create
    let mut c = SchemaCache::new();
    c.put("MySQL", table("sys_user"));
    assert_eq!(
        c.invalidate_on_ddl("CREATE TABLE sys_user (id bigint)"),
        vec!["sys_user".to_string()]
    );

    // 4 truncate
    let mut c = SchemaCache::new();
    c.put("MySQL", table("sys_user"));
    assert_eq!(
        c.invalidate_on_ddl("TRUNCATE TABLE sys_user"),
        vec!["sys_user".to_string()]
    );

    // 5 rename (both sides)
    let mut c = SchemaCache::new();
    c.put("MySQL", table("a"));
    c.put("MySQL", table("b"));
    assert_eq!(
        c.invalidate_on_ddl("RENAME TABLE a TO b"),
        vec!["a".to_string(), "b".to_string()]
    );

    // 6 create index
    let mut c = SchemaCache::new();
    c.put("MySQL", table("sys_user"));
    assert_eq!(
        c.invalidate_on_ddl("CREATE INDEX i ON sys_user (name)"),
        vec!["sys_user".to_string()]
    );

    // 7 drop index
    let mut c = SchemaCache::new();
    c.put("MySQL", table("sys_user"));
    assert_eq!(
        c.invalidate_on_ddl("DROP INDEX i ON sys_user"),
        vec!["sys_user".to_string()]
    );

    // 8 non-DDL is a no-op
    let mut c = SchemaCache::new();
    c.put("MySQL", table("sys_user"));
    assert!(c.invalidate_on_ddl("SELECT * FROM sys_user").is_empty());
    assert!(c.lookup("MySQL", "sys_user").is_ok());

    // 9 offline != absent
    let mut c = SchemaCache::new();
    c.put("MySQL", table("sys_user"));
    c.mark_offline("connection refused");
    assert_eq!(
        c.lookup("MySQL", "sys_user"),
        Err(SchemaMiss::Offline("connection refused".to_string()))
    );

    // 10 dialect isolation
    let mut c = SchemaCache::new();
    c.put("MySQL", table("t"));
    assert!(c.lookup("MySQL", "t").is_ok());
    assert_eq!(c.lookup("PostgreSQL", "t"), Err(SchemaMiss::Absent));

    // column mapping is fixed input/output too
    let cols = vec![vec![
        ("id".to_string(), "1".to_string()),
        ("select_type".to_string(), "SIMPLE".to_string()),
        ("table".to_string(), "t".to_string()),
    ]];
    let rows = rows_from_columns(&cols);
    assert_eq!(rows[0].id, Some(1));
    assert_eq!(rows[0].object.as_deref(), Some("t"));
}
