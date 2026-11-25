//! `check-sql`: classify a SQL statement and list its safety/perf findings.
//!
//! The argument is SQL text, `@<path>`, or a bare project file path (the form
//! the project view context menu passes). Classification and findings come from
//! [`crate::core::perf::check`], which splits multi-statement text and analyses
//! each statement on its own; this module only maps the report to the
//! `QUERY` / `DML` / `DDL` / `UNKNOWN` labels, the write-safety verdicts and the
//! output lines. Single-statement input keeps the historical output verbatim.
//!
//! The dialect comes from the `sql.dialect` setting (shared with the navigation
//! lane). When it is unset, text rules still run and only the quote-mismatch
//! rule is skipped.

use std::fmt::Write as _;

use openidea_plugin::{Error, Invocation};

use crate::commands::common::{self, CliError};
use crate::core::log::EntryClass;
use crate::core::perf::{SqlCheck, check as analyze_text};

/// Matches `[[contributes.commands]] id` in `plugin.toml`.
pub const ID: &str = "check-sql";

/// What the command accepts as `arg`.
pub const USAGE: &str = "<sql text> | <path> | @<path>";

pub const KEY_NO_SQL: &str = "check_sql.no_sql";

/// Dispatch entry.
pub fn run(ctx: &Invocation, arg: Option<&str>) -> Result<Option<String>, Error> {
    if arg.map(str::trim).unwrap_or("").is_empty() {
        return common::fail(common::missing_arg(USAGE));
    }
    match check(ctx, arg) {
        Ok(text) => {
            common::emit(&text)?;
            Ok(None)
        }
        Err(err) => common::fail(err),
    }
}

fn check(ctx: &Invocation, arg: Option<&str>) -> Result<String, CliError> {
    let (sql, source) = common::sql_arg(ctx, arg)?;
    if sql.trim().is_empty() {
        return Err(CliError::new(KEY_NO_SQL, "the SQL text is empty"));
    }
    let dialect = common::configured_dialect();
    let report = analyze_text(&sql, &dialect);
    let class = class_label(report.class);

    let mut text = String::new();
    let _ = writeln!(text, "-- check-sql");
    if let Some(source) = &source {
        let _ = writeln!(text, "source: {source}");
    }
    let _ = writeln!(text, "class: {class}");
    let _ = writeln!(
        text,
        "dialect: {}",
        if dialect.is_empty() {
            "(unset)"
        } else {
            &dialect
        }
    );
    let _ = writeln!(text, "destructive: {}", destructive_label(&report));
    let _ = writeln!(text, "unbounded-write: {}", unbounded_label(&report));
    if report.is_multi_statement() {
        let _ = writeln!(
            text,
            "statements: {}",
            common::class_mix(report.statements.iter().map(|statement| statement.class))
        );
        let _ = writeln!(text, "findings: {}", report.findings.len());
        for summary in &report.findings {
            common::finding_summary_line(&mut text, summary);
        }
    } else {
        // Single statement: keep the one-line-per-hit output unchanged.
        let hits = report
            .statements
            .first()
            .map_or(&[][..], |statement| statement.findings.as_slice());
        let _ = writeln!(text, "findings: {}", hits.len());
        for finding in hits {
            common::finding_line(&mut text, finding);
        }
    }
    Ok(text)
}

/// `destructive:` verdict. Single-statement text keeps the historical wording;
/// multi-statement text says which statement is destructive.
fn destructive_label(report: &SqlCheck) -> String {
    if !report.destructive {
        return "no".to_string();
    }
    if !report.is_multi_statement() {
        return format!("YES ({})", report.keyword);
    }
    match report.destructive_statements.as_slice() {
        [index] => format!(
            "YES ({}, statement #{index})",
            statement_keyword(report, *index)
        ),
        indexes => format!(
            "YES ({} statements: {})",
            indexes.len(),
            indexes
                .iter()
                .map(|index| format!("#{index} {}", statement_keyword(report, *index)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// `unbounded-write:` verdict, with the offending statement index when the
/// input holds more than one statement.
fn unbounded_label(report: &SqlCheck) -> String {
    if !report.unbounded_write {
        return "no".to_string();
    }
    if !report.is_multi_statement() {
        return format!("YES ({} without WHERE)", report.keyword);
    }
    match report.unbounded_statements.as_slice() {
        [index] => format!(
            "YES ({} without WHERE, statement #{index})",
            statement_keyword(report, *index)
        ),
        indexes => format!(
            "YES ({} statements without WHERE: {})",
            indexes.len(),
            indexes
                .iter()
                .map(|index| format!("#{index} {}", statement_keyword(report, *index)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn statement_keyword(report: &SqlCheck, index: usize) -> &str {
    report
        .statements
        .get(index.saturating_sub(1))
        .map_or("", |statement| statement.keyword.as_str())
}

fn class_label(class: EntryClass) -> &'static str {
    match class {
        EntryClass::Select => "QUERY",
        EntryClass::Insert | EntryClass::Update | EntryClass::Delete => "DML",
        EntryClass::Ddl => "DDL",
        EntryClass::Other | EntryClass::Error => "UNKNOWN",
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
    fn update_without_where_is_an_unbounded_write() {
        let host = MockHost::install();
        run(&host.invocation(), Some("UPDATE user SET name = 'x'")).unwrap();
        let text = text(&host);
        assert!(text.contains("class: DML"));
        assert!(text.contains("unbounded-write: YES (UPDATE without WHERE)"));
        assert!(text.contains("- [error] missing_where perf.missing_where"));
        assert!(text.contains("destructive: no"));
    }

    #[test]
    fn delete_with_where_is_bounded() {
        let host = MockHost::install();
        run(&host.invocation(), Some("DELETE FROM user WHERE id = ?")).unwrap();
        let text = text(&host);
        assert!(text.contains("class: DML"));
        assert!(text.contains("unbounded-write: no"));
    }

    #[test]
    fn multi_statement_file_flags_the_delete_without_where() {
        let host = MockHost::install();
        host.add_file(
            "/project/check.sql",
            "DELETE FROM users;\nSELECT id, name FROM users WHERE name LIKE '%abc' ORDER BY created_at DESC;",
        );
        run(&host.invocation(), Some("@/project/check.sql")).unwrap();
        let text = text(&host);
        assert!(text.contains("class: DML"));
        assert!(text.contains("unbounded-write: YES (DELETE without WHERE, statement #1)"));
        assert!(text.contains("destructive: no"));
        assert!(text.contains("statements: 2 (delete 1, select 1)"));
        assert!(text.contains("missing_where perf.missing_where"));
        assert!(text.contains("leading_wildcard_like"));
        assert!(text.contains("statements=#1"));
        assert!(text.contains("statements=#2"));
    }

    #[test]
    fn multi_statement_file_flags_a_later_drop_as_destructive() {
        let host = MockHost::install();
        host.add_file(
            "/project/check.sql",
            "SELECT id FROM users WHERE id = 1;\nDROP TABLE x;",
        );
        run(&host.invocation(), Some("@/project/check.sql")).unwrap();
        let text = text(&host);
        assert!(text.contains("class: QUERY"));
        assert!(text.contains("destructive: YES (DROP, statement #2)"));
        assert!(text.contains("unbounded-write: no"));
        assert!(text.contains("statements: 2 (select 1, ddl 1)"));
    }

    #[test]
    fn drop_is_classified_as_ddl_and_marked_destructive() {
        let host = MockHost::install();
        run(&host.invocation(), Some("DROP TABLE user")).unwrap();
        let text = text(&host);
        assert!(text.contains("class: DDL"));
        assert!(text.contains("destructive: YES (DROP)"));
    }

    #[test]
    fn select_star_is_a_query_with_a_finding() {
        let host = MockHost::install();
        run(&host.invocation(), Some("SELECT * FROM user")).unwrap();
        let text = text(&host);
        assert!(text.contains("class: QUERY"));
        assert!(text.contains("select_star perf.select_star"));
    }

    #[test]
    fn at_path_reads_sql_from_the_project() {
        let host = MockHost::install();
        host.add_file("/project/check.sql", "SELECT id FROM user WHERE id = 1");
        run(&host.invocation(), Some("@/project/check.sql")).unwrap();
        let text = text(&host);
        assert!(text.contains("source: /project/check.sql"));
        assert!(text.contains("class: QUERY"));
    }

    #[test]
    fn a_bare_path_reads_sql_from_the_project() {
        let host = MockHost::install();
        host.add_file("/project/check.sql", "DROP TABLE user");
        run(&host.invocation(), Some("/project/check.sql")).unwrap();
        let text = text(&host);
        assert!(text.contains("source: /project/check.sql"));
        assert!(text.contains("class: DDL"));
        assert!(text.contains("destructive: YES (DROP)"));
    }

    #[test]
    fn empty_sql_is_an_error() {
        let host = MockHost::install();
        host.add_file("/project/empty.sql", "  \n");
        let err = run(&host.invocation(), Some("@/project/empty.sql")).unwrap_err();
        assert!(matches!(err, Error::Failed(ref text) if text.contains(KEY_NO_SQL)));
        assert!(text(&host).contains(KEY_NO_SQL));
    }
}
