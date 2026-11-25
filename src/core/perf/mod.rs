//! Performance rules (21 canonical + 5 coverage extensions), complexity score
//! and index advice (T1-9).
//!
//! Rules return `messageKey`s, never prose: localization belongs to the plugin.
//! Everything here is pure and dependency-free apart from `serde`.

use serde_json::Value;

use crate::core::log::{EntryClass, classify_sql};

mod advice;
pub(crate) mod dialect;
mod features;
mod rules;
mod scoring;
pub(crate) mod sql;

pub use advice::{IndexAdvice, IndexDef, index_advice};
pub use rules::{CANONICAL_RULE_IDS, EXTENSION_RULE_IDS, Finding, Severity};
pub use scoring::{Complexity, ComplexityLevel, ScorePart, score};

/// Analyze `sql` against `dialect`, returning findings in rule order.
///
/// The dialect is a plain name (`mysql`, `postgresql`, `sqlserver`, …). An
/// unknown dialect only disables the quote-mismatch rule; an empty statement
/// yields no findings.
///
/// A **single** statement is expected. For a text holding several `;`-separated
/// statements use [`check`], which analyses each one separately (this function
/// tokenizes the whole text as one statement, so a `WHERE` from a later
/// statement would hide an earlier unbounded `DELETE`).
pub fn analyze(sql: &str, dialect: &str) -> Vec<Finding> {
    if sql.trim().is_empty() {
        return Vec::new();
    }
    let sa = sql::Analysis::parse(sql);
    if sa.is_empty() {
        return Vec::new();
    }
    rules::evaluate(&sa, dialect)
}

/// One statement inside a (possibly multi-statement) text.
#[derive(Debug, Clone, PartialEq)]
pub struct StatementCheck {
    /// 1-based position in the text.
    pub index: usize,
    /// Statement class, from the same classifier the log family uses.
    pub class: EntryClass,
    /// Leading keyword, upper-case, leading comments skipped.
    pub keyword: String,
    /// The statement text exactly as analysed.
    pub sql: String,
    /// Raw rule hits of this statement, in rule order.
    pub findings: Vec<Finding>,
}

/// Rule hits of a whole text merged by `rule_id`.
///
/// Evidence is kept so a multi-statement text reports "this rule fired twice,
/// in statements #1 and #3" instead of repeating the same line.
#[derive(Debug, Clone, PartialEq)]
pub struct FindingSummary {
    pub rule_id: String,
    pub message_key: String,
    pub severity: Severity,
    /// Number of raw hits across every statement.
    pub count: usize,
    /// 1-based statement indexes with a hit, ascending and distinct.
    pub statements: Vec<usize>,
    /// Structured detail of the first hit.
    pub detail: Value,
    /// Byte range of the first hit.
    pub range: Option<(u32, u32)>,
}

/// Text-level `check-sql` report.
///
/// A single statement produces exactly what [`analyze`] produces (same class,
/// keyword and findings); several statements are analysed one by one and their
/// findings merged, so an unbounded write can no longer hide behind a later
/// statement's `WHERE`.
#[derive(Debug, Clone, PartialEq)]
pub struct SqlCheck {
    /// Class of the first statement (the surface label is the caller's job).
    pub class: EntryClass,
    /// Leading keyword of the first statement.
    pub keyword: String,
    pub statements: Vec<StatementCheck>,
    /// Merged findings (one entry per `rule_id`).
    pub findings: Vec<FindingSummary>,
    /// True when any INSERT/UPDATE/DELETE has no top-level `WHERE`.
    pub unbounded_write: bool,
    /// 1-based indexes of those statements.
    pub unbounded_statements: Vec<usize>,
    /// True when any statement starts with `DROP` or `TRUNCATE`.
    pub destructive: bool,
    /// 1-based indexes of those statements.
    pub destructive_statements: Vec<usize>,
}

impl SqlCheck {
    /// True when the input held more than one statement.
    pub fn is_multi_statement(&self) -> bool {
        self.statements.len() > 1
    }
}

/// Classify and check every statement of `sql` against `dialect`.
pub fn check(sql: &str, dialect: &str) -> SqlCheck {
    let parts = sql::split_statements(sql);
    // A single statement is analysed from the original text so findings,
    // ranges and classification stay byte-identical to [`analyze`].
    let texts: Vec<&str> = if parts.len() <= 1 { vec![sql] } else { parts };

    let mut statements = Vec::with_capacity(texts.len());
    let mut findings: Vec<FindingSummary> = Vec::new();
    let mut unbounded_statements = Vec::new();
    let mut destructive_statements = Vec::new();

    for (position, text) in texts.iter().enumerate() {
        let index = position + 1;
        let class = classify_sql(text);
        let keyword = leading_keyword(text);
        let hits = analyze(text, dialect);
        if matches!(class, EntryClass::Update | EntryClass::Delete)
            && hits.iter().any(|hit| hit.rule_id == "missing_where")
        {
            unbounded_statements.push(index);
        }
        if matches!(keyword.as_str(), "DROP" | "TRUNCATE") {
            destructive_statements.push(index);
        }
        merge_findings(&mut findings, &hits, index);
        statements.push(StatementCheck {
            index,
            class,
            keyword,
            sql: text.to_string(),
            findings: hits,
        });
    }

    let first = statements.first();
    SqlCheck {
        class: first.map_or(EntryClass::Other, |statement| statement.class),
        keyword: first.map_or_else(String::new, |statement| statement.keyword.clone()),
        unbounded_write: !unbounded_statements.is_empty(),
        unbounded_statements,
        destructive: !destructive_statements.is_empty(),
        destructive_statements,
        statements,
        findings,
    }
}

fn merge_findings(merged: &mut Vec<FindingSummary>, hits: &[Finding], index: usize) {
    for hit in hits {
        if let Some(summary) = merged
            .iter_mut()
            .find(|summary| summary.rule_id == hit.rule_id)
        {
            summary.count += 1;
            if summary.statements.last() != Some(&index) {
                summary.statements.push(index);
            }
            continue;
        }
        merged.push(FindingSummary {
            rule_id: hit.rule_id.clone(),
            message_key: hit.message_key.clone(),
            severity: hit.severity,
            count: 1,
            statements: vec![index],
            detail: hit.detail.clone(),
            range: hit.range,
        });
    }
}

/// Leading keyword of a statement, skipping whitespace and leading comments.
fn leading_keyword(sql: &str) -> String {
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

    fn has<'a>(findings: &'a [Finding], rule: &str) -> Option<&'a Finding> {
        findings.iter().find(|f| f.rule_id == rule)
    }

    fn assert_rule(sql: &str, rule: &str, key: &str, severity: Severity) {
        let findings = analyze(sql, "mysql");
        let f = has(&findings, rule)
            .unwrap_or_else(|| panic!("rule {rule} missing for: {sql}\nfindings={findings:#?}"));
        assert_eq!(f.message_key, key, "rule {rule} key");
        assert_eq!(f.severity, severity, "rule {rule} severity");
        assert!(!f.message_key.contains(' '), "messageKey must be a key");
    }

    fn assert_no_rule(sql: &str, rule: &str) {
        let findings = analyze(sql, "mysql");
        assert!(
            has(&findings, rule).is_none(),
            "rule {rule} unexpectedly fired for: {sql}"
        );
    }

    // ---------------------------------------------------------------------
    // Canonical 21 — at least two positives and one negative each.
    // ---------------------------------------------------------------------

    #[test]
    fn missing_where_two_positives_one_negative() {
        assert_rule(
            "UPDATE user SET name = 'x'",
            "missing_where",
            "perf.missing_where",
            Severity::Error,
        );
        assert_rule(
            "DELETE FROM user",
            "missing_where",
            "perf.missing_where",
            Severity::Error,
        );
        assert_no_rule("DELETE FROM user WHERE id = ?", "missing_where");
        assert_no_rule("SELECT * FROM user", "missing_where");
    }

    #[test]
    fn leading_wildcard_two_positives_one_negative() {
        assert_rule(
            "SELECT id FROM user WHERE name LIKE '%abc'",
            "leading_wildcard_like",
            "perf.leading_wildcard",
            Severity::Warn,
        );
        assert_rule(
            "SELECT id FROM user WHERE name NOT LIKE \"%abc\"",
            "leading_wildcard_like",
            "perf.leading_wildcard",
            Severity::Warn,
        );
        assert_no_rule(
            "SELECT id FROM user WHERE name LIKE 'abc%'",
            "leading_wildcard_like",
        );
    }

    #[test]
    fn select_star_two_positives_one_negative() {
        assert_rule(
            "SELECT * FROM user",
            "select_star",
            "perf.select_star",
            Severity::Info,
        );
        assert_rule(
            "SELECT u.* FROM user u JOIN address a ON a.user_id = u.id",
            "select_star",
            "perf.select_star_with_join",
            Severity::Warn,
        );
        assert_no_rule("SELECT id, name FROM user", "select_star");
        assert_no_rule("SELECT count(*) FROM user", "select_star");
    }

    #[test]
    fn order_by_without_limit_two_positives_one_negative() {
        assert_rule(
            "SELECT id FROM user ORDER BY created_at",
            "order_by_without_limit",
            "perf.order_by_without_limit",
            Severity::Info,
        );
        assert_rule(
            "SELECT id FROM user ORDER BY created_at DESC",
            "order_by_without_limit",
            "perf.order_by_without_limit",
            Severity::Info,
        );
        assert_no_rule(
            "SELECT id FROM user ORDER BY created_at LIMIT 10",
            "order_by_without_limit",
        );
        assert_no_rule("SELECT id FROM user WHERE id > 3", "order_by_without_limit");
    }

    #[test]
    fn order_by_rand_two_positives_one_negative() {
        assert_rule(
            "SELECT id FROM user ORDER BY RAND()",
            "order_by_rand",
            "perf.order_by_rand",
            Severity::Warn,
        );
        assert_rule(
            "SELECT id FROM user ORDER BY created_at, RANDOM()",
            "order_by_rand",
            "perf.order_by_rand",
            Severity::Warn,
        );
        assert_no_rule("SELECT id FROM user ORDER BY created_at", "order_by_rand");
    }

    #[test]
    fn union_without_all_two_positives_one_negative() {
        assert_rule(
            "SELECT id FROM a UNION SELECT id FROM b",
            "union_instead_of_union_all",
            "perf.union_instead_of_union_all",
            Severity::Info,
        );
        assert_rule(
            "SELECT id FROM a UNION SELECT id FROM b UNION SELECT id FROM c",
            "union_instead_of_union_all",
            "perf.union_instead_of_union_all",
            Severity::Info,
        );
        assert_no_rule(
            "SELECT id FROM a UNION ALL SELECT id FROM b",
            "union_instead_of_union_all",
        );
    }

    #[test]
    fn join_without_on_two_positives_one_negative() {
        assert_rule(
            "SELECT * FROM a JOIN b",
            "join_without_on",
            "perf.join_without_on",
            Severity::Error,
        );
        assert_rule(
            "SELECT * FROM a LEFT JOIN b WHERE a.id = 1",
            "join_without_on",
            "perf.join_without_on",
            Severity::Error,
        );
        assert_no_rule("SELECT * FROM a JOIN b ON b.a_id = a.id", "join_without_on");
        assert_no_rule("SELECT * FROM a CROSS JOIN b", "join_without_on");
    }

    #[test]
    fn subquery_depth_two_positives_one_negative() {
        assert_rule(
            "SELECT id FROM (SELECT id FROM (SELECT id FROM (SELECT id FROM t) x) y) z",
            "subquery_depth",
            "perf.subquery_depth",
            Severity::Warn,
        );
        assert_rule(
            "SELECT id FROM t WHERE id IN (SELECT id FROM u WHERE id IN (SELECT id FROM (SELECT id FROM v) a))",
            "subquery_depth",
            "perf.subquery_depth",
            Severity::Warn,
        );
        assert_no_rule(
            "SELECT id FROM t WHERE id IN (SELECT id FROM u)",
            "subquery_depth",
        );
    }

    #[test]
    fn in_clause_overflow_two_positives_one_negative() {
        let list = (0..150)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        assert_rule(
            &format!("SELECT id FROM t WHERE id IN ({list})"),
            "in_clause_overflow",
            "perf.in_clause_overflow",
            Severity::Warn,
        );
        assert_rule(
            &format!("SELECT id FROM t WHERE a = 1 AND b IN ({list})"),
            "in_clause_overflow",
            "perf.in_clause_overflow",
            Severity::Warn,
        );
        assert_no_rule(
            "SELECT id FROM t WHERE id IN (1, 2, 3)",
            "in_clause_overflow",
        );
    }

    #[test]
    fn function_on_column_two_positives_one_negative() {
        assert_rule(
            "SELECT id FROM user WHERE date(created_at) = '2024-01-01'",
            "function_on_column",
            "perf.function_on_column",
            Severity::Warn,
        );
        assert_rule(
            "SELECT id FROM user WHERE UPPER(name) = 'A'",
            "function_on_column",
            "perf.function_on_column",
            Severity::Warn,
        );
        assert_no_rule("SELECT id FROM user WHERE name = 'A'", "function_on_column");
        assert_no_rule(
            "SELECT id FROM user WHERE id IN (1, 2)",
            "function_on_column",
        );
    }

    #[test]
    fn or_condition_count_two_positives_one_negative() {
        assert_rule(
            "SELECT id FROM t WHERE a = 1 OR b = 2 OR c = 3 OR d = 4 OR e = 5",
            "or_condition_count",
            "perf.or_condition_count",
            Severity::Info,
        );
        assert_rule(
            "SELECT id FROM t WHERE (a = 1 OR b = 2) AND (c = 3 OR d = 4 OR e = 5 OR f = 6)",
            "or_condition_count",
            "perf.or_condition_count",
            Severity::Info,
        );
        assert_no_rule(
            "SELECT id FROM t WHERE a = 1 OR b = 2",
            "or_condition_count",
        );
    }

    #[test]
    fn distinct_with_join_two_positives_one_negative() {
        assert_rule(
            "SELECT DISTINCT u.id FROM user u JOIN address a ON a.user_id = u.id",
            "distinct_with_join",
            "perf.distinct_with_join",
            Severity::Info,
        );
        assert_rule(
            "SELECT DISTINCT a.id FROM a LEFT JOIN b ON b.a_id = a.id",
            "distinct_with_join",
            "perf.distinct_with_join",
            Severity::Info,
        );
        assert_no_rule("SELECT DISTINCT id FROM user", "distinct_with_join");
    }

    #[test]
    fn group_by_without_aggregate_two_positives_one_negative() {
        assert_rule(
            "SELECT status FROM user GROUP BY status",
            "group_by_without_aggregate",
            "perf.group_by_without_aggregate",
            Severity::Info,
        );
        assert_rule(
            "SELECT status, name FROM user GROUP BY status, name",
            "group_by_without_aggregate",
            "perf.group_by_without_aggregate",
            Severity::Info,
        );
        assert_no_rule(
            "SELECT status, count(*) FROM user GROUP BY status",
            "group_by_without_aggregate",
        );
        assert_no_rule("SELECT status FROM user", "group_by_without_aggregate");
    }

    #[test]
    fn no_limit_large_result_two_positives_one_negative() {
        assert_rule(
            "SELECT * FROM a JOIN b ON b.a_id = a.id JOIN c ON c.b_id = b.id",
            "no_limit_large_result",
            "perf.no_limit_large_result",
            Severity::Warn,
        );
        assert_rule(
            "SELECT a.id FROM a JOIN b ON b.a_id = a.id JOIN c ON c.b_id = b.id",
            "no_limit_large_result",
            "perf.no_limit_large_result",
            Severity::Warn,
        );
        assert_no_rule(
            "SELECT * FROM a JOIN b ON b.a_id = a.id JOIN c ON c.b_id = b.id LIMIT 100",
            "no_limit_large_result",
        );
        assert_no_rule(
            "SELECT * FROM a JOIN b ON b.a_id = a.id WHERE a.id = 1",
            "no_limit_large_result",
        );
    }

    #[test]
    fn correlated_subquery_two_positives_one_negative() {
        assert_rule(
            "SELECT id FROM user u WHERE EXISTS (SELECT 1 FROM address a WHERE a.user_id = u.id)",
            "correlated_subquery",
            "perf.correlated_subquery",
            Severity::Warn,
        );
        assert_rule(
            "SELECT id FROM user u WHERE id IN (SELECT user_id FROM address a WHERE a.city = u.city)",
            "correlated_subquery",
            "perf.correlated_subquery",
            Severity::Warn,
        );
        assert_no_rule(
            "SELECT id FROM user u WHERE id IN (SELECT user_id FROM address)",
            "correlated_subquery",
        );
    }

    #[test]
    fn implicit_type_conversion_two_positives_one_negative() {
        assert_rule(
            "SELECT id FROM user WHERE user_id = '123'",
            "implicit_type_conversion",
            "perf.implicit_type_conversion",
            Severity::Warn,
        );
        assert_rule(
            "SELECT id FROM orders WHERE order_code = 'A001'",
            "implicit_type_conversion",
            "perf.implicit_type_conversion",
            Severity::Warn,
        );
        assert_no_rule(
            "SELECT id FROM user WHERE name = '123'",
            "implicit_type_conversion",
        );
        assert_no_rule(
            "SELECT id FROM user WHERE user_id = 123",
            "implicit_type_conversion",
        );
    }

    #[test]
    fn between_on_dates_two_positives_one_negative() {
        assert_rule(
            "SELECT id FROM user WHERE created_at BETWEEN '2024-01-01' AND '2024-12-31'",
            "between_on_dates",
            "perf.between_on_dates",
            Severity::Info,
        );
        assert_rule(
            "SELECT id FROM user WHERE age BETWEEN 18 AND 30",
            "between_on_dates",
            "perf.between_on_dates",
            Severity::Info,
        );
        assert_no_rule("SELECT id FROM user WHERE age > 18", "between_on_dates");
    }

    #[test]
    fn nested_subquery_in_select_two_positives_one_negative() {
        assert_rule(
            "SELECT id, (SELECT count(*) FROM address a WHERE a.user_id = u.id) AS cnt FROM user u",
            "nested_subquery_in_select",
            "perf.nested_subquery_in_select",
            Severity::Info,
        );
        assert_rule(
            "SELECT id, (SELECT max(x) FROM t2), (SELECT min(y) FROM t3) FROM t1",
            "nested_subquery_in_select",
            "perf.nested_subquery_in_select",
            Severity::Info,
        );
        assert_no_rule("SELECT id, name FROM user", "nested_subquery_in_select");
    }

    #[test]
    fn offset_pagination_two_positives_one_negative() {
        assert_rule(
            "SELECT id FROM user LIMIT 10 OFFSET 5000",
            "offset_pagination",
            "perf.offset_pagination",
            Severity::Info,
        );
        assert_rule(
            "SELECT id FROM user LIMIT 5000, 10",
            "offset_pagination",
            "perf.offset_pagination",
            Severity::Info,
        );
        assert_no_rule(
            "SELECT id FROM user LIMIT 10 OFFSET 20",
            "offset_pagination",
        );
    }

    #[test]
    fn multiple_group_by_orders_two_positives_one_negative() {
        assert_rule(
            "SELECT a, b, c, count(*) FROM t GROUP BY a, b, c ORDER BY a, b",
            "multiple_group_by_orders",
            "perf.multiple_group_by_orders",
            Severity::Info,
        );
        assert_rule(
            "SELECT a, b, c, sum(x) FROM t GROUP BY a, b, c ORDER BY a DESC, b ASC",
            "multiple_group_by_orders",
            "perf.multiple_group_by_orders",
            Severity::Info,
        );
        assert_no_rule(
            "SELECT a, b, count(*) FROM t GROUP BY a, b ORDER BY a",
            "multiple_group_by_orders",
        );
    }

    #[test]
    fn dialect_quote_mismatch_two_positives_one_negative() {
        // MySQL only accepts backticks; double quotes are at most a warning.
        assert_rule(
            "SELECT id FROM \"user\"",
            "dialect_quote_mismatch",
            "perf.dialect_quote_mismatch_ansi_quotes",
            Severity::Warn,
        );
        // PostgreSQL rejects backticks outright.
        let findings = analyze("SELECT `id` FROM user", "postgresql");
        let f = has(&findings, "dialect_quote_mismatch").expect("quote mismatch");
        assert_eq!(f.message_key, "perf.dialect_quote_mismatch");
        assert_eq!(f.severity, Severity::Error);
        assert_eq!(f.detail["quote"], "`");
        // Accepted quote is silent.
        let findings = analyze("SELECT `id` FROM `user`", "mysql");
        assert!(has(&findings, "dialect_quote_mismatch").is_none());
    }

    // ---------------------------------------------------------------------
    // Extension rules
    // ---------------------------------------------------------------------

    #[test]
    fn not_in_predicate_two_positives_one_negative() {
        assert_rule(
            "SELECT id FROM user WHERE status NOT IN (1, 2)",
            "not_in_predicate",
            "perf.not_in",
            Severity::Warn,
        );
        assert_rule(
            "SELECT id FROM user WHERE id NOT IN (SELECT id FROM banned)",
            "not_in_predicate",
            "perf.not_in",
            Severity::Warn,
        );
        assert_no_rule(
            "SELECT id FROM user WHERE status IN (1, 2)",
            "not_in_predicate",
        );
    }

    #[test]
    fn null_comparison_two_positives_one_negative() {
        assert_rule(
            "SELECT id FROM user WHERE name = NULL",
            "null_comparison",
            "perf.null_comparison",
            Severity::Warn,
        );
        assert_rule(
            "SELECT id FROM user WHERE name <> NULL",
            "null_comparison",
            "perf.null_comparison",
            Severity::Warn,
        );
        assert_no_rule("SELECT id FROM user WHERE name IS NULL", "null_comparison");
    }

    #[test]
    fn having_without_group_two_positives_one_negative() {
        assert_rule(
            "SELECT count(*) FROM user HAVING count(*) > 10",
            "having_without_group",
            "perf.having_without_group",
            Severity::Warn,
        );
        assert_rule(
            "SELECT sum(amount) FROM orders HAVING sum(amount) > 100",
            "having_without_group",
            "perf.having_without_group",
            Severity::Warn,
        );
        assert_no_rule(
            "SELECT status, count(*) FROM user GROUP BY status HAVING count(*) > 1",
            "having_without_group",
        );
    }

    #[test]
    fn aggregate_without_group_two_positives_one_negative() {
        assert_rule(
            "SELECT id, count(*) FROM user",
            "aggregate_without_group",
            "perf.aggregate_without_group",
            Severity::Info,
        );
        assert_rule(
            "SELECT name, sum(amount) FROM orders",
            "aggregate_without_group",
            "perf.aggregate_without_group",
            Severity::Info,
        );
        assert_no_rule(
            "SELECT id, count(*) FROM user GROUP BY id",
            "aggregate_without_group",
        );
        assert_no_rule("SELECT count(*) FROM user", "aggregate_without_group");
    }

    #[test]
    fn temporary_table_two_positives_one_negative() {
        assert_rule(
            "CREATE TEMPORARY TABLE tmp_user AS SELECT id FROM user",
            "temporary_table",
            "perf.temporary_table",
            Severity::Info,
        );
        assert_rule(
            "SELECT id INTO TEMP tmp_user FROM user",
            "temporary_table",
            "perf.temporary_table",
            Severity::Info,
        );
        assert_no_rule(
            "CREATE TABLE user_copy AS SELECT id FROM user",
            "temporary_table",
        );
    }

    // ---------------------------------------------------------------------
    // Multi-statement check (D4)
    // ---------------------------------------------------------------------

    #[test]
    fn multi_statement_check_analyses_every_statement() {
        let report = check(
            "DELETE FROM users;\nSELECT id, name FROM users WHERE name LIKE '%abc' ORDER BY created_at DESC;",
            "mysql",
        );
        assert_eq!(report.statements.len(), 2);
        assert_eq!(report.class, EntryClass::Delete);
        assert_eq!(report.statements[0].keyword, "DELETE");
        assert_eq!(report.statements[1].class, EntryClass::Select);
        // The later statement's WHERE must not make the DELETE look bounded.
        assert!(report.unbounded_write);
        assert_eq!(report.unbounded_statements, vec![1]);
        assert!(!report.destructive);
        let missing = report
            .findings
            .iter()
            .find(|summary| summary.rule_id == "missing_where")
            .expect("missing_where must fire for the DELETE");
        assert_eq!(missing.count, 1);
        assert_eq!(missing.statements, vec![1]);
        let like = report
            .findings
            .iter()
            .find(|summary| summary.rule_id == "leading_wildcard_like")
            .expect("the wildcard SELECT must still be reported");
        assert_eq!(like.statements, vec![2]);
    }

    #[test]
    fn multi_statement_check_flags_a_later_drop() {
        let report = check("SELECT id FROM users WHERE id = 1;\nDROP TABLE x;", "mysql");
        assert_eq!(report.statements.len(), 2);
        assert!(report.destructive);
        assert_eq!(report.destructive_statements, vec![2]);
        assert_eq!(report.statements[1].class, EntryClass::Ddl);
        assert!(!report.unbounded_write);
    }

    #[test]
    fn merged_findings_keep_one_entry_per_rule_with_statement_evidence() {
        let report = check("DELETE FROM a;\nDELETE FROM b;", "mysql");
        assert_eq!(report.findings.len(), 1);
        let summary = &report.findings[0];
        assert_eq!(summary.rule_id, "missing_where");
        assert_eq!(summary.count, 2);
        assert_eq!(summary.statements, vec![1, 2]);
        assert_eq!(report.unbounded_statements, vec![1, 2]);
    }

    #[test]
    fn single_statement_check_matches_analyze_exactly() {
        let sql = "UPDATE user SET name = 'x'";
        let report = check(sql, "mysql");
        assert_eq!(report.statements.len(), 1);
        assert!(!report.is_multi_statement());
        assert_eq!(report.class, EntryClass::Update);
        assert_eq!(report.statements[0].findings, analyze(sql, "mysql"));
        assert!(report.unbounded_write);
        assert_eq!(report.unbounded_statements, vec![1]);
    }

    #[test]
    fn split_statements_ignores_semicolons_in_literals_and_comments() {
        let parts = sql::split_statements(
            "SELECT ';' AS s FROM t WHERE id = 1; -- tail ;\nSELECT \"a;b\" FROM t",
        );
        assert_eq!(
            parts,
            vec![
                "SELECT ';' AS s FROM t WHERE id = 1;",
                "SELECT \"a;b\" FROM t"
            ]
        );
    }

    // ---------------------------------------------------------------------
    // Serialization and API surface
    // ---------------------------------------------------------------------

    #[test]
    fn findings_are_serde_roundtrippable() {
        let findings = analyze("SELECT * FROM user", "mysql");
        let json = serde_json::to_string(&findings).expect("serialize");
        let back: Vec<Finding> = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, findings);
    }

    #[test]
    fn unknown_dialect_still_runs_text_rules() {
        let findings = analyze("SELECT * FROM user", "no-such-dialect");
        assert!(has(&findings, "select_star").is_some());
    }

    #[test]
    fn empty_sql_yields_no_findings() {
        assert!(analyze("   \n\t", "mysql").is_empty());
    }

    #[test]
    fn rule_id_sets_are_mutually_exclusive_and_complete() {
        assert_eq!(CANONICAL_RULE_IDS.len(), 21);
        assert_eq!(EXTENSION_RULE_IDS.len(), 5);
        for id in CANONICAL_RULE_IDS {
            assert!(!EXTENSION_RULE_IDS.contains(&id));
        }
    }
}
