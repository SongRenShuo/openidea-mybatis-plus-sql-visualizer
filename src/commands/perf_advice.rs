//! `perf-advice`: performance findings and the complexity score for a SQL text.
//!
//! The argument is SQL text, a project file path (as handed over by the project
//! view context menu) or `@<path>`. Findings come from
//! [`crate::core::perf::analyze`] and are printed with their stable `rule_id`,
//! `message_key` and severity; the score is [`crate::core::perf::score`], which
//! depends on the SQL text only.

use std::fmt::Write as _;

use openidea_plugin::{Error, Invocation};

use crate::commands::common::{self, CliError};
use crate::core::perf::{ComplexityLevel, Severity, analyze, score};

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
    let findings = analyze(&sql, &dialect);
    let complexity = score(&sql);

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
        complexity.score,
        level_label(complexity.level)
    );
    let _ = writeln!(text, "recognized: {}", complexity.recognized);
    if !complexity.breakdown.is_empty() {
        let parts = complexity
            .breakdown
            .iter()
            .map(|part| format!("{}={}", part.factor, part.points))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(text, "breakdown: {parts}");
    }
    let _ = writeln!(text, "findings: {}", findings.len());
    for finding in &findings {
        let _ = writeln!(
            text,
            "- [{}] {} {} {}",
            severity_label(finding.severity),
            finding.rule_id,
            finding.message_key,
            finding.detail
        );
    }
    Ok(text)
}

fn severity_label(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warn => "warn",
        Severity::Info => "info",
    }
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
}
