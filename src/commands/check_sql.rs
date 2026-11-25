//! `check-sql`: classify a SQL statement and list its safety/perf findings.
//!
//! The argument is SQL text, or `@<path>` to read a file inside the project.
//! Classification comes from [`crate::core::log::classify_sql`] (the only
//! public statement classifier) and the findings from
//! [`crate::core::perf::analyze`]; this module maps them to `QUERY` / `DML` /
//! `DDL` / `UNKNOWN` and flags unbounded writes and destructive statements.
//!
//! The dialect comes from the `sql.dialect` setting (shared with the navigation
//! lane). When it is unset, text rules still run and only the quote-mismatch
//! rule is skipped.

use std::fmt::Write as _;

use openidea_plugin::{Error, Invocation};

use crate::commands::common::{self, CliError};
use crate::core::log::{EntryClass, classify_sql};
use crate::core::perf::{Finding, Severity, analyze};

/// Matches `[[contributes.commands]] id` in `plugin.toml`.
pub const ID: &str = "check-sql";

/// What the command accepts as `arg`.
pub const USAGE: &str = "<sql text> | @<path>";

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
    let entry_class = classify_sql(&sql);
    let class = class_label(entry_class);
    let finding_list = analyze(&sql, &dialect);
    let keyword = first_keyword(&sql);
    let destructive = matches!(keyword.as_str(), "DROP" | "TRUNCATE");
    let unbounded = matches!(
        entry_class,
        EntryClass::Insert | EntryClass::Update | EntryClass::Delete
    ) && finding_list.iter().any(|f| f.rule_id == "missing_where");

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
    let _ = writeln!(
        text,
        "destructive: {}",
        if destructive {
            format!("YES ({keyword})")
        } else {
            "no".to_string()
        }
    );
    let _ = writeln!(
        text,
        "unbounded-write: {}",
        if unbounded {
            format!("YES ({keyword} without WHERE)")
        } else {
            "no".to_string()
        }
    );
    let _ = writeln!(text, "findings: {}", finding_list.len());
    for finding in &finding_list {
        append_finding(&mut text, finding);
    }
    Ok(text)
}

fn append_finding(text: &mut String, finding: &Finding) {
    let _ = writeln!(
        text,
        "- [{}] {} {} {}",
        severity_label(finding.severity),
        finding.rule_id,
        finding.message_key,
        finding.detail
    );
}

fn severity_label(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warn => "warn",
        Severity::Info => "info",
    }
}

fn class_label(class: EntryClass) -> &'static str {
    match class {
        EntryClass::Select => "QUERY",
        EntryClass::Insert | EntryClass::Update | EntryClass::Delete => "DML",
        EntryClass::Ddl => "DDL",
        EntryClass::Other | EntryClass::Error => "UNKNOWN",
    }
}

/// Leading keyword of a statement, skipping whitespace and leading comments.
///
/// This is presentation only: the statement class still comes from
/// `core::log::classify_sql` and `core::perf::analyze`.
fn first_keyword(sql: &str) -> String {
    let mut rest = sql.trim_start();
    loop {
        if let Some(after) = rest.strip_prefix("--") {
            rest = after
                .split_once('\n')
                .map_or("", |(_, tail)| tail)
                .trim_start();
        } else if let Some(after) = rest.strip_prefix("/*") {
            rest = after
                .split_once("*/")
                .map_or("", |(_, tail)| tail)
                .trim_start();
        } else {
            break;
        }
    }
    rest.chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_uppercase()
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
    fn empty_sql_is_an_error() {
        let host = MockHost::install();
        host.add_file("/project/empty.sql", "  \n");
        let err = run(&host.invocation(), Some("@/project/empty.sql")).unwrap_err();
        assert!(matches!(err, Error::Failed(ref text) if text.contains(KEY_NO_SQL)));
        assert!(text(&host).contains(KEY_NO_SQL));
    }
}
