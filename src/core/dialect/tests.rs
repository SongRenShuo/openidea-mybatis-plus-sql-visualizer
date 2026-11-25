//! Per-dialect capability assertions (T1-6 acceptance).
//!
//! Parity source: `mybatis-plus-sql-visualizer` Kotlin
//! `src/test/kotlin/.../sql/DialectTest.kt` (pagination / quoting / upsert /
//! folding / registry). Rewritten in Rust; behaviour is asserted against the
//! same literal strings the Kotlin tests pin.

use super::*;

fn d(name: &str) -> &'static Dialect {
    by_name(name).unwrap_or_else(|| panic!("dialect {name} missing"))
}

#[test]
fn registry_has_all_fifteen_in_order() {
    let names: Vec<&str> = all().iter().map(|x| x.name).collect();
    assert_eq!(
        names,
        vec![
            "MySQL",
            "PostgreSQL",
            "Oracle",
            "SQL Server",
            "H2",
            "SQLite",
            "MariaDB",
            "DM（达梦）",
            "KingBase（人大金仓）",
            "OceanBase",
            "openGauss",
            "GaussDB",
            "HighGo",
            "Vastbase",
            "GoldenDB",
        ]
    );
    assert_eq!(all().len(), 15);
}

#[test]
fn by_name_hits_and_misses() {
    assert_eq!(d("MySQL").name, "MySQL");
    assert_eq!(d("DM（达梦）").name, "DM（达梦）");
    assert_eq!(d("KingBase（人大金仓）").name, "KingBase（人大金仓）");
    assert!(by_name("NoSuchDialect").is_none());
    assert!(by_name("").is_none());
}

#[test]
fn pagination_every_dialect() {
    // MySQL family: LIMIT [offset,] count
    for name in ["MySQL", "MariaDB", "DM（达梦）", "OceanBase", "GoldenDB"] {
        let d = d(name);
        assert_eq!(d.render_limit(10, None), "LIMIT 10", "{name}");
        assert_eq!(d.render_limit(10, Some(5)), "LIMIT 5, 10", "{name}");
    }
    // PG family: LIMIT count OFFSET offset
    for name in [
        "PostgreSQL",
        "H2",
        "SQLite",
        "KingBase（人大金仓）",
        "openGauss",
        "GaussDB",
        "HighGo",
        "Vastbase",
    ] {
        let d = d(name);
        assert_eq!(d.render_limit(10, None), "LIMIT 10", "{name}");
        assert_eq!(d.render_limit(10, Some(5)), "LIMIT 10 OFFSET 5", "{name}");
    }
    // Oracle: FETCH FIRST / OFFSET ... FETCH NEXT
    let oracle = d("Oracle");
    assert_eq!(oracle.render_limit(10, None), "FETCH FIRST 10 ROWS ONLY");
    assert_eq!(
        oracle.render_limit(10, Some(5)),
        "OFFSET 5 ROWS FETCH NEXT 10 ROWS ONLY"
    );
    // SQL Server: offset defaults to 0
    let mssql = d("SQL Server");
    assert_eq!(
        mssql.render_limit(10, None),
        "OFFSET 0 ROWS FETCH NEXT 10 ROWS ONLY"
    );
    assert_eq!(
        mssql.render_limit(10, Some(5)),
        "OFFSET 5 ROWS FETCH NEXT 10 ROWS ONLY"
    );
}

#[test]
fn identifier_quotes_per_dialect() {
    let cases = [
        ("MySQL", "`id`"),
        ("MariaDB", "`id`"),
        ("OceanBase", "`id`"),
        ("GoldenDB", "`id`"),
        ("PostgreSQL", "\"id\""),
        ("Oracle", "\"id\""),
        ("DM（达梦）", "\"id\""),
        ("KingBase（人大金仓）", "\"id\""),
        ("openGauss", "\"id\""),
        ("GaussDB", "\"id\""),
        ("HighGo", "\"id\""),
        ("Vastbase", "\"id\""),
        ("SQL Server", "[id]"),
        ("H2", "\"id\""),
        ("SQLite", "\"id\""),
    ];
    for (name, expected) in cases {
        assert_eq!(d(name).quote_identifier("id"), expected, "{name}");
    }
}

#[test]
fn acceptable_identifier_quote_sets() {
    assert_eq!(
        d("MySQL").capabilities.identifier_quotes.to_vec(),
        vec!['`']
    );
    assert_eq!(
        d("PostgreSQL").capabilities.identifier_quotes.to_vec(),
        vec!['"']
    );
    // SQL Server accepts brackets and double quotes (QUOTED_IDENTIFIER default ON).
    assert_eq!(
        d("SQL Server").capabilities.identifier_quotes.to_vec(),
        vec!['[', '"']
    );
    // SQLite / H2 accept all three.
    assert_eq!(
        d("SQLite").capabilities.identifier_quotes.to_vec(),
        vec!['"', '`', '[']
    );
    assert_eq!(
        d("H2").capabilities.identifier_quotes.to_vec(),
        vec!['"', '`', '[']
    );
}

#[test]
fn reserved_word_quoting_is_per_dialect() {
    // `table` reserved in MySQL → backtick; `name` not reserved → bare.
    assert_eq!(d("MySQL").quote_if_keyword("table"), "`table`");
    assert_eq!(d("MySQL").quote_if_keyword("name"), "name");
    // `number` is an Oracle reserved word (quoted) but legal bare in MySQL.
    assert_eq!(d("Oracle").quote_if_keyword("number"), "\"number\"");
    assert_eq!(d("MySQL").quote_if_keyword("number"), "number");
    // `table` reserved in PG family → double quote.
    assert_eq!(d("PostgreSQL").quote_if_keyword("table"), "\"table\"");
    // `value` reserved in H2 but not MySQL.
    assert_eq!(d("H2").quote_if_keyword("value"), "\"value\"");
    assert_eq!(d("MySQL").quote_if_keyword("value"), "value");
    // `rownum` is openGauss/KingBase reserved (not plain PG).
    assert_eq!(d("openGauss").quote_if_keyword("rownum"), "\"rownum\"");
    assert_eq!(d("PostgreSQL").quote_if_keyword("rownum"), "rownum");
    // `storagedb` is a GoldenDB-specific reserved word.
    assert_eq!(d("GoldenDB").quote_if_keyword("storagedb"), "`storagedb`");
}

#[test]
fn identifier_folding_families() {
    for name in ["Oracle", "H2"] {
        assert_eq!(
            d(name).capabilities.identifier_folding,
            IdentifierFolding::Upper,
            "{name}"
        );
    }
    for name in [
        "PostgreSQL",
        "KingBase（人大金仓）",
        "openGauss",
        "GaussDB",
        "HighGo",
        "Vastbase",
    ] {
        assert_eq!(
            d(name).capabilities.identifier_folding,
            IdentifierFolding::Lower,
            "{name}"
        );
    }
    for name in [
        "MySQL",
        "MariaDB",
        "SQLite",
        "SQL Server",
        "DM（达梦）",
        "OceanBase",
        "GoldenDB",
    ] {
        assert_eq!(
            d(name).capabilities.identifier_folding,
            IdentifierFolding::None,
            "{name}"
        );
    }
}

#[test]
fn catalog_quoting_by_folding_family() {
    use IdentifierCaseSource::{Catalog, Entity};
    // UPPER (Oracle): lowercase catalog name quoted, all-uppercase bare.
    assert_eq!(
        d("Oracle").quote_if_needed("issue_new", Catalog),
        "\"issue_new\""
    );
    assert_eq!(
        d("Oracle").quote_if_needed("ISSUE_NEW", Catalog),
        "ISSUE_NEW"
    );
    assert_eq!(d("Oracle").quote_if_needed("NUMBER", Catalog), "\"NUMBER\"");
    // LOWER (PG): uppercase catalog name quoted, all-lowercase bare.
    assert_eq!(
        d("PostgreSQL").quote_if_needed("Issue_New", Catalog),
        "\"Issue_New\""
    );
    assert_eq!(
        d("PostgreSQL").quote_if_needed("issue_new", Catalog),
        "issue_new"
    );
    // NONE (MySQL): catalog quoting is just keyword quoting.
    assert_eq!(
        d("MySQL").quote_if_needed("Issue_New", Catalog),
        "Issue_New"
    );
    assert_eq!(d("MySQL").quote_if_needed("table", Catalog), "`table`");
    // Entity source never applies case-based quoting.
    assert_eq!(
        d("Oracle").quote_if_needed("Issue_New", Entity),
        "Issue_New"
    );
    assert_eq!(
        d("PostgreSQL").quote_if_needed("Issue_New", Entity),
        "Issue_New"
    );
}

#[test]
fn upsert_suffix_shapes() {
    // MySQL family: ON DUPLICATE KEY UPDATE ... VALUES(...)
    assert_eq!(
        d("MySQL").render_upsert("id", &["table", "name"]).unwrap(),
        "ON DUPLICATE KEY UPDATE `table` = VALUES(`table`), name = VALUES(name)"
    );
    assert_eq!(
        d("MariaDB").render_upsert("id", &["table"]).unwrap(),
        "ON DUPLICATE KEY UPDATE `table` = VALUES(`table`)"
    );
    assert_eq!(
        d("GoldenDB").render_upsert("id", &["name"]).unwrap(),
        "ON DUPLICATE KEY UPDATE name = VALUES(name)"
    );
    // PG family: ON CONFLICT (k) DO UPDATE SET ... EXCLUDED...
    assert_eq!(
        d("PostgreSQL").render_upsert("table", &["id"]).unwrap(),
        "ON CONFLICT (\"table\") DO UPDATE SET id = EXCLUDED.id"
    );
    assert_eq!(
        d("SQLite").render_upsert("id", &["name"]).unwrap(),
        "ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name"
    );
    assert_eq!(
        d("KingBase（人大金仓）")
            .render_upsert("id", &["name"])
            .unwrap(),
        "ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name"
    );
    // openGauss/GaussDB use MySQL-style upsert even though quoting is double-quote.
    assert_eq!(
        d("openGauss").render_upsert("id", &["name"]).unwrap(),
        "ON DUPLICATE KEY UPDATE name = VALUES(name)"
    );
    // Oracle / SQL Server / DM: no suffix-form upsert.
    assert!(d("Oracle").render_upsert("id", &["name"]).is_none());
    assert!(d("SQL Server").render_upsert("id", &["name"]).is_none());
    assert!(d("DM（达梦）").render_upsert("id", &["name"]).is_none());
}

#[test]
fn h2_whole_statement_upsert() {
    let h2 = d("H2");
    assert!(h2.render_upsert("id", &["name"]).is_none());
    assert_eq!(
        h2.render_upsert_statement("t", &["id", "name"], "id")
            .unwrap(),
        "MERGE INTO t (id, name) KEY (id) VALUES (?, ?)"
    );
    // UPPER folding: catalog name with lowercase gets quoted.
    assert_eq!(
        h2.render_upsert_statement_with(
            "Issue_New",
            &["Id", "Name"],
            "Id",
            IdentifierCaseSource::Catalog
        )
        .unwrap(),
        "MERGE INTO \"Issue_New\" (\"Id\", \"Name\") KEY (\"Id\") VALUES (?, ?)"
    );
    // Only H2 has the whole-statement form.
    assert!(
        d("MySQL")
            .render_upsert_statement("t", &["id"], "id")
            .is_none()
    );
}

#[test]
fn identity_wrap_shapes() {
    // SQL Server and DM wrap with SET IDENTITY_INSERT ... ON/OFF.
    for name in ["SQL Server", "DM（达梦）"] {
        let wrapped = d(name)
            .render_identity_wrapper("t_user", "INSERT INTO t_user (id) VALUES (1)")
            .unwrap();
        assert_eq!(
            wrapped,
            "SET IDENTITY_INSERT t_user ON;\nINSERT INTO t_user (id) VALUES (1)\nSET IDENTITY_INSERT t_user OFF;",
            "{name}"
        );
    }
    // Everyone else: no wrapper.
    for name in ["MySQL", "PostgreSQL", "Oracle", "H2", "SQLite"] {
        assert!(
            d(name)
                .render_identity_wrapper("t", "INSERT INTO t VALUES (1)")
                .is_none(),
            "{name}"
        );
    }
}

#[test]
fn delete_limit_capability() {
    for name in [
        "MySQL",
        "MariaDB",
        "DM（达梦）",
        "OceanBase",
        "GoldenDB",
        "Vastbase",
    ] {
        assert!(d(name).capabilities.supports_delete_limit, "{name}");
    }
    for name in [
        "PostgreSQL",
        "Oracle",
        "SQL Server",
        "H2",
        "SQLite",
        "openGauss",
        "GaussDB",
        "HighGo",
    ] {
        assert!(!d(name).capabilities.supports_delete_limit, "{name}");
    }
}

#[test]
fn boolean_and_ansi_date_capabilities() {
    // Oracle / DM have no boolean literal.
    assert!(!d("Oracle").capabilities.supports_boolean_literal);
    assert!(!d("DM（达梦）").capabilities.supports_boolean_literal);
    assert!(d("MySQL").capabilities.supports_boolean_literal);
    // SQL Server / SQLite do not parse ANSI typed date literals.
    assert!(!d("SQL Server").capabilities.supports_ansi_date_literal);
    assert!(!d("SQLite").capabilities.supports_ansi_date_literal);
    assert!(d("Oracle").capabilities.supports_ansi_date_literal);
    assert!(d("MySQL").capabilities.supports_ansi_date_literal);
}

#[test]
fn reserved_word_tables_are_nonempty_and_lowercase() {
    for dialect in all() {
        assert!(!dialect.reserved_words().is_empty(), "{}", dialect.name);
        for w in dialect.reserved_words() {
            assert_eq!(
                *w,
                w.to_lowercase(),
                "{} has non-lowercase word {w}",
                dialect.name
            );
        }
        // sanity: the canonical `select` keyword is reserved everywhere.
        assert!(dialect.is_reserved("SELECT"), "{}", dialect.name);
    }
}
