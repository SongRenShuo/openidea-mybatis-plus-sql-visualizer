//! The performance rule set.
//!
//! Rules never return prose: each finding carries a stable `message_key` and a
//! structured `detail` payload. The 21 canonical rules mirror the IntelliJ
//! edition (`PerfRulesBatch1..4`) one-for-one so a future parity harness can
//! diff them; five extension rules cover anti-patterns required by the lane
//! card that the IntelliJ set does not contain. `CANONICAL_RULE_IDS` and
//! `EXTENSION_RULE_IDS` are public so callers can filter for parity.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::dialect;
use super::features::{Features, has_row_limit};
use super::sql::{Analysis, StmtKind, TokenKind};

/// The 21 rule ids mirrored from the IntelliJ edition.
pub const CANONICAL_RULE_IDS: [&str; 21] = [
    "missing_where",
    "leading_wildcard_like",
    "select_star",
    "order_by_without_limit",
    "order_by_rand",
    "union_instead_of_union_all",
    "join_without_on",
    "subquery_depth",
    "in_clause_overflow",
    "function_on_column",
    "or_condition_count",
    "distinct_with_join",
    "group_by_without_aggregate",
    "no_limit_large_result",
    "correlated_subquery",
    "implicit_type_conversion",
    "between_on_dates",
    "nested_subquery_in_select",
    "offset_pagination",
    "multiple_group_by_orders",
    "dialect_quote_mismatch",
];

/// Additional coverage required by the lane card, beyond the IntelliJ set.
pub const EXTENSION_RULE_IDS: [&str; 5] = [
    "not_in_predicate",
    "null_comparison",
    "having_without_group",
    "aggregate_without_group",
    "temporary_table",
];

/// Severity levels, serialized lower-case (`error` / `warn` / `info`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warn,
    Info,
}

/// One performance finding. `detail` is structured JSON, never a sentence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub rule_id: String,
    pub message_key: String,
    pub severity: Severity,
    pub range: Option<(u32, u32)>,
    pub detail: Value,
}

/// Run every rule against `sql` for `dialect`.
///
/// `dialect` is a plain name (see `super::dialect` for accepted aliases). An
/// unknown dialect disables only the quote-mismatch rule; all text rules still
/// run.
pub(crate) fn evaluate(sa: &Analysis, dialect: &str) -> Vec<Finding> {
    let f = Features::extract(sa);
    let profile = dialect::profile(dialect);
    let mut out = Vec::new();

    macro_rules! push {
        ($opt:expr) => {
            if let Some(finding) = $opt {
                out.push(finding);
            }
        };
    }

    push!(missing_where(sa, &f));
    push!(leading_wildcard(sa, &f));
    push!(select_star(sa, &f));
    push!(order_by_without_limit(sa, &f));
    push!(order_by_rand(sa, &f));
    push!(union_without_all(sa, &f));
    push!(join_without_on(&f));
    push!(subquery_depth(&f));
    push!(in_clause_overflow(&f));
    push!(function_on_column(sa, &f));
    push!(or_condition_count(sa, &f));
    push!(distinct_with_join(&f));
    push!(group_by_without_aggregate(&f));
    push!(no_limit_large_result(&f));
    push!(correlated_subquery(sa));
    push!(implicit_type_conversion(&f));
    push!(between_on_dates(&f));
    push!(nested_subquery_in_select(&f));
    push!(offset_pagination(&f));
    push!(multiple_group_by_orders(&f));
    push!(dialect_quote_mismatch(sa, profile));

    // Extension rules.
    push!(not_in_predicate(&f));
    push!(null_comparison(&f));
    push!(having_without_group(&f));
    push!(aggregate_without_group(&f));
    push!(temporary_table(&f));

    out
}

fn finding(
    rule_id: &str,
    message_key: &str,
    severity: Severity,
    range: (u32, u32),
    detail: Value,
) -> Finding {
    Finding {
        rule_id: rule_id.to_string(),
        message_key: message_key.to_string(),
        severity,
        range: Some(range),
        detail,
    }
}

fn tok_range(sa: &Analysis, i: usize) -> (u32, u32) {
    sa.tokens
        .get(i)
        .map(|t| (t.start as u32, t.end as u32))
        .unwrap_or_else(|| sa.stmt_range())
}

fn missing_where(sa: &Analysis, f: &Features) -> Option<Finding> {
    if !f.missing_where {
        return None;
    }
    let statement = match sa.stmt {
        StmtKind::Update => "UPDATE",
        StmtKind::Delete => "DELETE",
        _ => return None,
    };
    Some(finding(
        "missing_where",
        "perf.missing_where",
        Severity::Error,
        sa.stmt_range(),
        json!({ "statement": statement }),
    ))
}

fn leading_wildcard(sa: &Analysis, f: &Features) -> Option<Finding> {
    if !f.leading_wildcard {
        return None;
    }
    let pos = (0..sa.len()).find(|&i| {
        sa.is_word_at(i, "LIKE")
            && sa
                .tokens
                .get(i + 1)
                .and_then(|t| t.string_value().or(t.value.as_deref()))
                .is_some_and(|v| v.starts_with('%'))
    })?;
    Some(finding(
        "leading_wildcard_like",
        "perf.leading_wildcard",
        Severity::Warn,
        tok_range(sa, pos),
        json!({ "pattern_position": "leading", "wildcard": "%" }),
    ))
}

fn select_star(sa: &Analysis, f: &Features) -> Option<Finding> {
    if !f.select_star {
        return None;
    }
    let sel = sa.main_select?;
    Some(finding(
        "select_star",
        if f.select_star_with_join {
            "perf.select_star_with_join"
        } else {
            "perf.select_star"
        },
        if f.select_star_with_join {
            Severity::Warn
        } else {
            Severity::Info
        },
        tok_range(sa, sel),
        json!({ "has_join": f.select_star_with_join }),
    ))
}

fn order_by_without_limit(sa: &Analysis, f: &Features) -> Option<Finding> {
    if sa.stmt != StmtKind::Select || f.order_by_count == 0 || has_row_limit(sa) {
        return None;
    }
    let pos = sa.find_top_seq(&["ORDER", "BY"])?;
    Some(finding(
        "order_by_without_limit",
        "perf.order_by_without_limit",
        Severity::Info,
        tok_range(sa, pos),
        json!({ "order_by_items": f.order_by_count }),
    ))
}

fn order_by_rand(sa: &Analysis, f: &Features) -> Option<Finding> {
    if !f.order_by_rand {
        return None;
    }
    let order = sa.find_top_seq(&["ORDER", "BY"])?;
    let end = sa.clause_end(order + 2);
    let (name, pos) = (order + 2..end).find_map(|i| {
        sa.word_at(i)
            .filter(|w| matches!(*w, "RAND" | "RANDOM" | "NEWID"))
            .map(|w| (w.to_string(), i))
    })?;
    Some(finding(
        "order_by_rand",
        "perf.order_by_rand",
        Severity::Warn,
        tok_range(sa, pos),
        json!({ "function": name }),
    ))
}

fn union_without_all(sa: &Analysis, f: &Features) -> Option<Finding> {
    if f.union_count == 0 {
        return None;
    }
    let pos = sa.find_top("UNION")?;
    Some(finding(
        "union_instead_of_union_all",
        "perf.union_instead_of_union_all",
        Severity::Info,
        tok_range(sa, pos),
        json!({ "count": f.union_count, "suggestion": "UNION ALL" }),
    ))
}

fn join_without_on(f: &Features) -> Option<Finding> {
    if !f.join_without_on {
        return None;
    }
    Some(Finding {
        rule_id: "join_without_on".to_string(),
        message_key: "perf.join_without_on".to_string(),
        severity: Severity::Error,
        range: None,
        detail: json!({ "kind": "cartesian_product_risk" }),
    })
}

fn subquery_depth(f: &Features) -> Option<Finding> {
    if f.subquery_depth <= 2 {
        return None;
    }
    Some(Finding {
        rule_id: "subquery_depth".to_string(),
        message_key: "perf.subquery_depth".to_string(),
        severity: Severity::Warn,
        range: None,
        detail: json!({ "depth": f.subquery_depth, "threshold": 2 }),
    })
}

fn in_clause_overflow(f: &Features) -> Option<Finding> {
    if f.big_in_lists == 0 {
        return None;
    }
    Some(Finding {
        rule_id: "in_clause_overflow".to_string(),
        message_key: "perf.in_clause_overflow".to_string(),
        severity: Severity::Warn,
        range: None,
        detail: json!({ "lists": f.big_in_lists, "threshold": 100 }),
    })
}

fn function_on_column(sa: &Analysis, f: &Features) -> Option<Finding> {
    if !f.function_on_column {
        return None;
    }
    let w = sa.find_top("WHERE")?;
    let end = sa.clause_end(w + 1);
    let mut names = Vec::new();
    let mut first = None;
    let mut i = w + 1;
    while i < end {
        let Some(word) = sa.word_at(i) else {
            i += 1;
            continue;
        };
        if super::sql::is_keyword(word) || !sa.tokens.get(i + 1).is_some_and(|t| t.is_punct('(')) {
            i += 1;
            continue;
        }
        if let Some(col) = sa.read_column_ref(i + 2)
            && !sa.tokens.get(i + 2).is_some_and(|t| t.is_punct('('))
        {
            names.push(format!("{}({})", word, col.column));
            first.get_or_insert(i);
        }
        i += 1;
    }
    let pos = first?;
    Some(finding(
        "function_on_column",
        "perf.function_on_column",
        Severity::Warn,
        tok_range(sa, pos),
        json!({ "functions": names }),
    ))
}

fn or_condition_count(sa: &Analysis, f: &Features) -> Option<Finding> {
    if f.or_count <= 3 {
        return None;
    }
    let w = sa.find_top("WHERE")?;
    Some(finding(
        "or_condition_count",
        "perf.or_condition_count",
        Severity::Info,
        tok_range(sa, w),
        json!({ "count": f.or_count, "threshold": 3 }),
    ))
}

fn distinct_with_join(f: &Features) -> Option<Finding> {
    if !f.distinct || f.join_count == 0 {
        return None;
    }
    Some(Finding {
        rule_id: "distinct_with_join".to_string(),
        message_key: "perf.distinct_with_join".to_string(),
        severity: Severity::Info,
        range: None,
        detail: json!({ "join_count": f.join_count }),
    })
}

fn group_by_without_aggregate(f: &Features) -> Option<Finding> {
    if !f.group_by || f.select_has_aggregate {
        return None;
    }
    Some(Finding {
        rule_id: "group_by_without_aggregate".to_string(),
        message_key: "perf.group_by_without_aggregate".to_string(),
        severity: Severity::Info,
        range: None,
        detail: json!({ "group_by_items": f.group_by_count }),
    })
}

fn no_limit_large_result(f: &Features) -> Option<Finding> {
    if f.stmt != Some(StmtKind::Select) || f.join_count < 2 || !f.no_limit || f.has_where {
        return None;
    }
    Some(Finding {
        rule_id: "no_limit_large_result".to_string(),
        message_key: "perf.no_limit_large_result".to_string(),
        severity: Severity::Warn,
        range: None,
        detail: json!({ "join_count": f.join_count, "has_where": false }),
    })
}

/// A subquery that references a table alias declared only by the outer query.
fn correlated_subquery(sa: &Analysis) -> Option<Finding> {
    let outer = sa.table_refs();
    let mut outer_aliases: Vec<String> = Vec::new();
    for tr in &outer {
        if let Some(a) = &tr.alias {
            outer_aliases.push(a.clone());
        }
        if let Some(last) = tr.name.rsplit('.').next() {
            outer_aliases.push(last.to_string());
        }
    }
    if outer_aliases.is_empty() {
        return None;
    }

    for open in 0..sa.len() {
        if !sa.tokens[open].is_punct('(')
            || !sa.tokens.get(open + 1).is_some_and(|t| t.is_word("SELECT"))
        {
            continue;
        }
        let Some(close) = matching_paren(sa, open) else {
            continue;
        };
        let inner_sql = &sa.sql[sa.tokens[open].start..sa.tokens[close].end];
        let inner = Analysis::parse(inner_sql);
        let mut inner_aliases: Vec<String> = Vec::new();
        for tr in inner.table_refs() {
            if let Some(a) = &tr.alias {
                inner_aliases.push(a.clone());
            }
            if let Some(last) = tr.name.rsplit('.').next() {
                inner_aliases.push(last.to_string());
            }
        }
        for i in 0..inner.len() {
            let Some(q) = inner.tokens[i].ident_name() else {
                continue;
            };
            let qualified = inner.tokens.get(i + 1).is_some_and(|t| t.is_punct('.'));
            if !qualified {
                continue;
            }
            let refers_outer = outer_aliases.iter().any(|a| a.eq_ignore_ascii_case(q));
            let locally_bound = inner_aliases.iter().any(|a| a.eq_ignore_ascii_case(q));
            if refers_outer && !locally_bound {
                return Some(finding(
                    "correlated_subquery",
                    "perf.correlated_subquery",
                    Severity::Warn,
                    tok_range(sa, open),
                    json!({ "outer_alias": q }),
                ));
            }
        }
    }
    None
}

fn matching_paren(sa: &Analysis, open: usize) -> Option<usize> {
    let mut level = 0i32;
    for i in open..sa.len() {
        if sa.tokens[i].is_punct('(') {
            level += 1;
        } else if sa.tokens[i].is_punct(')') {
            level -= 1;
            if level == 0 {
                return Some(i);
            }
        }
    }
    None
}

fn implicit_type_conversion(f: &Features) -> Option<Finding> {
    if f.implicit_type_columns.is_empty() {
        return None;
    }
    Some(Finding {
        rule_id: "implicit_type_conversion".to_string(),
        message_key: "perf.implicit_type_conversion".to_string(),
        severity: Severity::Warn,
        range: None,
        detail: json!({
            "columns": f.implicit_type_columns,
            "heuristic": "column_name",
            "note_key": "perf.implicit_type_conversion.heuristic",
        }),
    })
}

fn between_on_dates(f: &Features) -> Option<Finding> {
    if f.between_count == 0 {
        return None;
    }
    Some(Finding {
        rule_id: "between_on_dates".to_string(),
        message_key: "perf.between_on_dates".to_string(),
        severity: Severity::Info,
        range: None,
        detail: json!({ "count": f.between_count }),
    })
}

fn nested_subquery_in_select(f: &Features) -> Option<Finding> {
    if f.nested_subquery_in_select == 0 {
        return None;
    }
    Some(Finding {
        rule_id: "nested_subquery_in_select".to_string(),
        message_key: "perf.nested_subquery_in_select".to_string(),
        severity: Severity::Info,
        range: None,
        detail: json!({ "count": f.nested_subquery_in_select }),
    })
}

fn offset_pagination(f: &Features) -> Option<Finding> {
    let offset = f.offset?;
    if offset <= 1000 {
        return None;
    }
    Some(Finding {
        rule_id: "offset_pagination".to_string(),
        message_key: "perf.offset_pagination".to_string(),
        severity: Severity::Info,
        range: None,
        detail: json!({ "offset": offset, "threshold": 1000 }),
    })
}

fn multiple_group_by_orders(f: &Features) -> Option<Finding> {
    if f.group_by_count < 3 || f.order_by_count < 2 {
        return None;
    }
    Some(Finding {
        rule_id: "multiple_group_by_orders".to_string(),
        message_key: "perf.multiple_group_by_orders".to_string(),
        severity: Severity::Info,
        range: None,
        detail: json!({
            "group_by_items": f.group_by_count,
            "order_by_items": f.order_by_count,
        }),
    })
}

/// Double-quoted text can be a string literal (MySQL without ANSI_QUOTES) or a
/// list value. Without an AST the rule stays conservative and skips positions
/// that are syntactically values, so `LIKE "%x"` is not reported as an
/// identifier quote problem.
fn is_literal_position(sa: &Analysis, i: usize) -> bool {
    let Some(prev) = i.checked_sub(1).and_then(|j| sa.tokens.get(j)) else {
        return false;
    };
    matches!(prev.kind, TokenKind::Op)
        || (prev.kind == TokenKind::Word
            && matches!(prev.upper.as_str(), "LIKE" | "IN" | "VALUES" | "VALUE"))
        || prev.is_punct('(')
        || prev.is_punct(',')
}

fn dialect_quote_mismatch(
    sa: &Analysis,
    profile: Option<dialect::DialectProfile>,
) -> Option<Finding> {
    let profile = profile?;
    let mut offending: Vec<(char, String, usize)> = Vec::new();
    for i in 0..sa.len() {
        if let TokenKind::QuotedIdent(q) = sa.tokens[i].kind
            && !profile.accepts(q)
            && !is_literal_position(sa, i)
        {
            let name = sa.tokens[i]
                .value
                .clone()
                .unwrap_or_else(|| sa.tokens[i].text.clone());
            if !offending
                .iter()
                .any(|(c, n, _)| *c == q && n.eq_ignore_ascii_case(&name))
            {
                offending.push((q, name, i));
            }
        }
    }
    if offending.is_empty() {
        return None;
    }
    // Ambiguous only for MySQL-family double quotes; everything else is a
    // guaranteed syntax error in that dialect.
    let ambiguous = offending
        .iter()
        .find(|(q, _, _)| *q == '"' && profile.mysql_family);
    let accepted: String = {
        let mut chars = profile.accepted_quotes.to_vec();
        chars.sort_unstable();
        chars
            .iter()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join("/")
    };
    match ambiguous {
        Some((q, _, pos)) => Some(finding(
            "dialect_quote_mismatch",
            "perf.dialect_quote_mismatch_ansi_quotes",
            Severity::Warn,
            tok_range(sa, *pos),
            json!({
                "quote": q.to_string(),
                "dialect": profile.name,
                "accepted": accepted,
                "ambiguous": true,
                "identifiers": offending.iter().map(|(_, n, _)| n).collect::<Vec<_>>(),
            }),
        )),
        None => {
            let (q, _, pos) = offending[0];
            Some(finding(
                "dialect_quote_mismatch",
                "perf.dialect_quote_mismatch",
                Severity::Error,
                tok_range(sa, pos),
                json!({
                    "quote": q.to_string(),
                    "dialect": profile.name,
                    "accepted": accepted,
                    "ambiguous": false,
                    "identifiers": offending.iter().map(|(_, n, _)| n).collect::<Vec<_>>(),
                }),
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Extension rules
// ---------------------------------------------------------------------------

fn not_in_predicate(f: &Features) -> Option<Finding> {
    if !f.not_in {
        return None;
    }
    Some(Finding {
        rule_id: "not_in_predicate".to_string(),
        message_key: "perf.not_in".to_string(),
        severity: Severity::Warn,
        range: None,
        detail: json!({ "kind": "not_in", "null_semantics": true }),
    })
}

fn null_comparison(f: &Features) -> Option<Finding> {
    if !f.null_comparison {
        return None;
    }
    Some(Finding {
        rule_id: "null_comparison".to_string(),
        message_key: "perf.null_comparison".to_string(),
        severity: Severity::Warn,
        range: None,
        detail: json!({ "suggest": "IS NULL / IS NOT NULL" }),
    })
}

fn having_without_group(f: &Features) -> Option<Finding> {
    if !f.having_without_group {
        return None;
    }
    Some(Finding {
        rule_id: "having_without_group".to_string(),
        message_key: "perf.having_without_group".to_string(),
        severity: Severity::Warn,
        range: None,
        detail: json!({ "has_having": true, "has_group_by": false }),
    })
}

fn aggregate_without_group(f: &Features) -> Option<Finding> {
    if !f.aggregate_without_group {
        return None;
    }
    Some(Finding {
        rule_id: "aggregate_without_group".to_string(),
        message_key: "perf.aggregate_without_group".to_string(),
        severity: Severity::Info,
        range: None,
        detail: json!({ "has_aggregate": true, "has_group_by": false }),
    })
}

fn temporary_table(f: &Features) -> Option<Finding> {
    if !f.temporary_table {
        return None;
    }
    Some(Finding {
        rule_id: "temporary_table".to_string(),
        message_key: "perf.temporary_table".to_string(),
        severity: Severity::Info,
        range: None,
        detail: json!({ "kind": "temporary_table" }),
    })
}
