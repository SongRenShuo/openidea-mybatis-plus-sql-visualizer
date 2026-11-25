//! `perf-advice`: performance findings and the complexity score for a SQL text.
//!
//! The argument is SQL text, a project file path (as handed over by the project
//! view context menu) or `@<path>`. Findings and the score come from
//! [`crate::core::perf::advise`], which analyses every `;`-separated statement
//! on its own; this module only formats. A single statement prints exactly the
//! historical lines; several statements add a `statements:` summary and merge
//! the findings by `rule_id`.

use std::fmt::Write as _;

use openidea_plugin::{Error, Invocation};

use crate::commands::common::{self, CliError};
use crate::core::perf::{ComplexityLevel, advise as build_advice};

/// Matches `[[contributes.commands]] id` in `plugin.toml`.
pub const ID: &str = "perf-advice";

/// What the command accepts as `arg`.
pub const USAGE: &str = "<sql text> | <path> | @<path>";

/// Dispatch entry.
pub fn run(ctx: &Invocation, arg: Option<&str>) -> Result<Option<String>, Error> {
    if arg.map(str::trim).unwrap_or("").is_empty() {
        return common::fail(common::missing_arg(USAGE));
    }
    match advise(ctx, arg) {
        Ok(text) => {
            common::emit(&text)?;
            Ok(None)
        }
        Err(err) => common::fail(err),
    }
}

fn advise(ctx: &Invocation, arg: Option<&str>) -> Result<String, CliError> {
    let (sql, source) = common::sql_arg(ctx, arg)?;
    if sql.trim().is_empty() {
        return Err(CliError::new(
            common::KEY_EMPTY_ARG,
            "the SQL text is empty",
        ));
    }
    let dialect = common::configured_dialect();
    let report = build_advice(&sql, &dialect);

    let mut text = String::new();
    let _ = writeln!(text, "-- perf-advice");
    if let Some(source) = &source {
        let _ = writeln!(text, "source: {source}");
    }
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
        "score: {} ({})",
        report.complexity.score,
        level_label(report.complexity.level)
    );
    let _ = writeln!(text, "recognized: {}", report.complexity.recognized);
    if report.is_multi_statement() {
        let _ = writeln!(
            text,
            "statements: {}",
            common::class_mix(report.statements.iter().map(|statement| statement.class))
        );
    }
    if !report.complexity.breakdown.is_empty() {
        let parts = report
            .complexity
            .breakdown
            .iter()
            .map(|part| format!("{}={}", part.factor, part.points))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(text, "breakdown: {parts}");
    }
    if report.is_multi_statement() {
        let _ = writeln!(text, "findings: {}", report.findings.len());
        for summary in &report.findings {
            common::finding_summary_line(&mut text, summary);
        }
    } else {
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

fn level_label(level: ComplexityLevel) -> &'static str {
    match level {
        ComplexityLevel::Low => "low",
        ComplexityLevel::Medium => "medium",
        ComplexityLevel::High => "high",
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
    fn lists_findings_with_rule_id_key_and_severity() {
        let host = MockHost::install();
        run(
            &host.invocation(),
            Some("SELECT id FROM user WHERE name LIKE '%abc'"),
        )
        .unwrap();
        let text = text(&host);
        assert!(text.contains("- [warn] leading_wildcard_like perf.leading_wildcard"));
        assert!(text.contains("findings: 1"));
    }

    #[test]
    fn unbounded_update_scores_medium() {
        let host = MockHost::install();
        run(&host.invocation(), Some("UPDATE user SET name = 'x'")).unwrap();
        let text = text(&host);
        assert!(text.contains("score: 50 (medium)"));
        assert!(text.contains("missing_where perf.missing_where"));
        assert!(text.contains("missingWhere=50"));
    }

    #[test]
    fn plain_text_is_not_recognised_and_scores_zero() {
        let host = MockHost::install();
        run(&host.invocation(), Some("hello world")).unwrap();
        let text = text(&host);
        assert!(text.contains("score: 0 (low)"));
        assert!(text.contains("recognized: false"));
        assert!(text.contains("findings: 0"));
    }

    #[test]
    fn a_missing_argument_is_an_error() {
        let host = MockHost::install();
        let err = run(&host.invocation(), None).unwrap_err();
        assert!(matches!(err, Error::Failed(ref text) if text.contains(common::KEY_MISSING_ARG)));
        assert!(text(&host).contains(common::KEY_MISSING_ARG));
    }

    #[test]
    fn a_bare_path_reads_sql_from_the_project() {
        let host = MockHost::install();
        host.add_file("/project/perf.sql", "SELECT id FROM user ORDER BY RAND()");
        run(&host.invocation(), Some("/project/perf.sql")).unwrap();
        let text = text(&host);
        assert!(text.contains("source: /project/perf.sql"));
        assert!(text.contains("order_by_rand perf.order_by_rand"));
    }

    #[test]
    fn sql_text_with_spaces_is_analysed_as_text() {
        let host = MockHost::install();
        // No such file: even a path-shaped tail must not break literal SQL.
        run(&host.invocation(), Some("UPDATE user SET name = 'x'")).unwrap();
        assert!(text(&host).contains("missing_where perf.missing_where"));
    }

    #[test]
    fn single_statement_output_keeps_its_historical_lines() {
        let host = MockHost::install();
        run(&host.invocation(), Some("UPDATE user SET name = 'x'")).unwrap();
        let text = text(&host);
        assert!(text.contains("score: 50 (medium)"));
        assert!(text.contains("recognized: true"));
        assert!(text.contains("breakdown: missingWhere=50"));
        assert!(text.contains("findings: 1"));
        assert!(text.contains("- [error] missing_where perf.missing_where"));
        assert!(!text.contains("statements:"));
    }

    #[test]
    fn multi_statement_file_sums_the_score_and_merges_findings() {
        let host = MockHost::install();
        host.add_file(
            "/project/perf.sql",
            "DELETE FROM users;\nSELECT id FROM user WHERE name LIKE '%abc'",
        );
        run(&host.invocation(), Some("/project/perf.sql")).unwrap();
        let text = text(&host);
        assert!(text.contains("statements: 2 (delete 1, select 1)"));
        // 53 (DELETE: 3 tables + 50 missing WHERE) + 16 (SELECT: 3 tables +
        // 8 leading wildcard + 5 no limit) — summed, not one mixed analysis.
        assert!(text.contains("score: 69 (medium)"));
        assert!(text.contains("tableCount=6"));
        assert!(text.contains("missingWhere=50"));
        assert!(text.contains("missing_where perf.missing_where"));
        assert!(text.contains("statements=#1"));
        assert!(text.contains("leading_wildcard_like"));
        assert!(text.contains("statements=#2"));
        assert!(text.contains("findings: 2"));
    }

    #[test]
    fn the_same_rule_in_several_statements_is_printed_once() {
        let host = MockHost::install();
        host.add_file("/project/perf.sql", "DELETE FROM a;\nDELETE FROM b;");
        run(&host.invocation(), Some("/project/perf.sql")).unwrap();
        let text = text(&host);
        assert_eq!(text.matches("missing_where perf.missing_where").count(), 1);
        assert!(text.contains("count=2 statements=#1,#2"));
    }
}
