//! Structural facts extracted once and shared by the rules and the complexity
//! score.
//!
//! Everything here is derived from [`Analysis`]; no rule-specific message text
//! lives in this module.

use super::sql::{Analysis, PredKind, StmtKind, TokenKind, predicates};

/// Facts about one SQL statement.
#[derive(Debug, Clone, Default)]
pub(crate) struct Features {
    pub recognized: bool,
    pub stmt: Option<StmtKind>,
    pub has_where: bool,
    pub table_count: usize,
    pub join_count: usize,
    pub subquery_depth: u32,
    pub union_count: usize,
    pub cte_count: usize,
    pub or_count: usize,
    pub big_in_lists: usize,
    pub select_star: bool,
    pub select_star_with_join: bool,
    pub leading_wildcard: bool,
    pub missing_where: bool,
    pub no_limit: bool,
    pub function_on_column: bool,
    pub order_by_rand: bool,
    pub distinct: bool,
    pub group_by: bool,
    pub having: bool,
    pub select_has_aggregate: bool,
    pub group_by_count: usize,
    pub order_by_count: usize,
    pub between_count: usize,
    pub offset: Option<u64>,
    pub nested_subquery_in_select: usize,
    pub join_without_on: bool,
    pub not_in: bool,
    pub null_comparison: bool,
    pub having_without_group: bool,
    pub aggregate_without_group: bool,
    pub temporary_table: bool,
    pub implicit_type_columns: Vec<String>,
    pub predicate_columns: Vec<(Option<String>, String)>,
}

impl Features {
    // The struct is filled incrementally because several facts depend on
    // earlier ones; a single literal would be unreadable.
    #[allow(clippy::field_reassign_with_default)]
    pub fn extract(sa: &Analysis) -> Self {
        let mut f = Features::default();
        f.recognized = sa.stmt != StmtKind::Other || sa.main_select.is_some();
        f.stmt = Some(sa.stmt);
        f.table_count = sa.table_refs().len();
        f.has_where = sa.find_top("WHERE").is_some();
        f.join_count = (0..sa.len()).filter(|&i| sa.is_top_word(i, "JOIN")).count();
        f.subquery_depth = sa.subquery_depth();
        f.cte_count = sa.cte_count();
        f.group_by = sa.find_top_seq(&["GROUP", "BY"]).is_some();
        f.having = sa.find_top("HAVING").is_some();
        f.distinct = sa
            .main_select
            .and_then(|i| sa.tokens.get(i + 1))
            .is_some_and(|t| t.is_word("DISTINCT"));

        // UNION count (only `UNION` without `ALL`).
        for i in 0..sa.len() {
            if sa.is_top_word(i, "UNION") && !sa.tokens.get(i + 1).is_some_and(|t| t.is_word("ALL"))
            {
                f.union_count += 1;
            }
        }

        // OR count in the top-level WHERE.
        if let Some(w) = sa.find_top("WHERE") {
            let end = sa.clause_end(w + 1);
            f.or_count = (w + 1..end).filter(|&i| sa.is_word_at(i, "OR")).count();
        }

        f.big_in_lists = count_big_in_lists(sa);
        (f.select_star, f.select_star_with_join) = select_star(sa);
        f.leading_wildcard = leading_wildcard(sa);
        f.missing_where = matches!(sa.stmt, StmtKind::Update | StmtKind::Delete)
            && sa.find_top("WHERE").is_none();
        f.no_limit = sa.stmt == StmtKind::Select && !has_row_limit(sa);
        f.function_on_column = function_on_column(sa);
        f.order_by_rand = order_by_rand(sa);
        f.select_has_aggregate = select_has_aggregate(sa) || f.having;
        (f.group_by_count, f.order_by_count) = grouping_counts(sa);
        f.between_count = between_count(sa);
        f.offset = offset_value(sa);
        f.nested_subquery_in_select = nested_subquery_in_select(sa);
        f.join_without_on = join_without_on(sa);
        (f.not_in, f.null_comparison, f.implicit_type_columns) = predicate_flags(sa);
        f.having_without_group = f.having && !f.group_by;
        f.aggregate_without_group = aggregate_without_group(sa);
        f.temporary_table = temporary_table(sa);
        f.predicate_columns = predicates(sa)
            .into_iter()
            .filter(|p| p.kind != PredKind::OrderBy)
            .map(|p| (p.table, p.column))
            .collect();
        f
    }
}

/// Does the statement limit rows (`LIMIT`, `TOP n` or `FETCH ... ROWS`)?
pub(crate) fn has_row_limit(sa: &Analysis) -> bool {
    if sa.find_top("LIMIT").is_some() {
        return true;
    }
    if sa.find_top("FETCH").is_some() {
        return true;
    }
    // `SELECT TOP 10 ...` only counts when TOP follows SELECT.
    if let Some(sel) = sa.main_select
        && sa.tokens.get(sel + 1).is_some_and(|t| t.is_word("TOP"))
    {
        return true;
    }
    false
}

fn select_star(sa: &Analysis) -> (bool, bool) {
    let mut star = false;
    let mut star_with_join = false;
    for sel in 0..sa.len() {
        if sa.word_at(sel) != Some("SELECT") {
            continue;
        }
        let depth = sa.depth[sel];
        let Some((start, end)) = select_projection_at_depth(sa, sel, depth) else {
            continue;
        };
        let mut i = start;
        if i < end
            && sa
                .word_at(i)
                .is_some_and(|w| matches!(w, "DISTINCT" | "ALL"))
        {
            i += 1;
        }
        while i < end {
            if sa.depth[i] == depth && sa.tokens[i].is_punct('*') {
                star = true;
                if (0..sa.len()).any(|j| sa.is_top_word(j, "JOIN")) {
                    star_with_join = true;
                }
            }
            // `alias.*`
            if sa.depth[i] == depth
                && sa.tokens[i].ident_name().is_some()
                && sa.tokens.get(i + 1).is_some_and(|t| t.is_punct('.'))
                && sa.tokens.get(i + 2).is_some_and(|t| t.is_punct('*'))
            {
                star = true;
                if sa.find_top("JOIN").is_some() {
                    star_with_join = true;
                }
            }
            i += 1;
        }
    }
    (star, star_with_join)
}

/// Projection span for a `SELECT` at an arbitrary parenthesis depth.
fn select_projection_at_depth(sa: &Analysis, sel: usize, depth: u32) -> Option<(usize, usize)> {
    let start = sel + 1;
    for i in start..sa.len() {
        if sa.depth[i] == depth && sa.is_word_at(i, "FROM") {
            return Some((start, i));
        }
        // A closing parenthesis at lower depth ends this select.
        if sa.depth[i] < depth {
            break;
        }
    }
    None
}

fn leading_wildcard(sa: &Analysis) -> bool {
    for i in 0..sa.len() {
        if !sa.is_word_at(i, "LIKE") {
            continue;
        }
        let pattern = sa
            .tokens
            .get(i + 1)
            .and_then(|t| t.string_value().or(t.value.as_deref()));
        if pattern.is_some_and(|v| v.starts_with('%')) {
            return true;
        }
    }
    false
}

fn function_on_column(sa: &Analysis) -> bool {
    let Some(w) = sa.find_top("WHERE") else {
        return false;
    };
    let end = sa.clause_end(w + 1);
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
        // First argument must be a column reference (not a literal, not another
        // function call, not an empty list).
        if let Some(col) = sa.read_column_ref(i + 2)
            && !sa.tokens.get(i + 2).is_some_and(|t| t.is_punct('('))
            && col.alias.as_deref() != Some("*")
        {
            return true;
        }
        i += 1;
    }
    false
}

fn order_by_rand(sa: &Analysis) -> bool {
    let Some(order) = sa.find_top_seq(&["ORDER", "BY"]) else {
        return false;
    };
    let end = sa.clause_end(order + 2);
    (order + 2..end).any(|i| {
        sa.word_at(i)
            .is_some_and(|w| matches!(w, "RAND" | "RANDOM" | "NEWID"))
            && sa.tokens.get(i + 1).is_some_and(|t| t.is_punct('('))
    })
}

fn grouping_counts(sa: &Analysis) -> (usize, usize) {
    let group = sa
        .find_top_seq(&["GROUP", "BY"])
        .map(|i| count_list_items(sa, i + 2, sa.clause_end(i + 2)))
        .unwrap_or(0);
    let order = sa
        .find_top_seq(&["ORDER", "BY"])
        .map(|i| count_list_items(sa, i + 2, sa.clause_end(i + 2)))
        .unwrap_or(0);
    (group, order)
}

/// Count comma-separated items between token indices, at the depth of `start`.
fn count_list_items(sa: &Analysis, start: usize, end: usize) -> usize {
    if start >= end {
        return 0;
    }
    let depth = sa.depth[start];
    let mut items = 1;
    for i in start..end {
        if sa.depth[i] == depth && sa.tokens[i].is_punct(',') {
            items += 1;
        }
    }
    items
}

fn between_count(sa: &Analysis) -> usize {
    let Some(w) = sa.find_top("WHERE") else {
        return 0;
    };
    let end = sa.clause_end(w + 1);
    (w + 1..end)
        .filter(|&i| sa.is_top(i) && sa.is_word_at(i, "BETWEEN"))
        .count()
}

fn offset_value(sa: &Analysis) -> Option<u64> {
    // `OFFSET n`
    if let Some(off) = sa.find_top("OFFSET")
        && let Some(n) = sa.tokens.get(off + 1).and_then(|t| t.number_value())
    {
        return n.parse().ok();
    }
    // MySQL `LIMIT offset, count`
    if let Some(lim) = sa.find_top("LIMIT") {
        if let (Some(a), Some(b)) = (sa.tokens.get(lim + 1), sa.tokens.get(lim + 2))
            && a.number_value().is_some()
            && b.is_punct(',')
        {
            return a.number_value().and_then(|n| n.parse::<u64>().ok());
        }
        // `LIMIT count OFFSET offset`
        if let (Some(a), Some(o), Some(b)) = (
            sa.tokens.get(lim + 1),
            sa.tokens.get(lim + 2),
            sa.tokens.get(lim + 3),
        ) && a.number_value().is_some()
            && o.is_word("OFFSET")
        {
            return b.number_value().and_then(|n| n.parse::<u64>().ok());
        }
    }
    None
}

fn nested_subquery_in_select(sa: &Analysis) -> usize {
    let mut count = 0;
    for sel in 0..sa.len() {
        if sa.word_at(sel) != Some("SELECT") {
            continue;
        }
        let depth = sa.depth[sel];
        let Some((start, end)) = select_projection_at_depth(sa, sel, depth) else {
            continue;
        };
        for i in start..end {
            if sa.tokens[i].is_punct('(')
                && sa.depth[i] == depth
                && sa.tokens.get(i + 1).is_some_and(|t| t.is_word("SELECT"))
            {
                count += 1;
            }
        }
    }
    count
}

fn join_without_on(sa: &Analysis) -> bool {
    for i in 0..sa.len() {
        if !sa.is_top_word(i, "JOIN") {
            continue;
        }
        // `CROSS JOIN` / `NATURAL JOIN` are legal without ON.
        let prev1 = i.checked_sub(1).and_then(|j| sa.word_at(j));
        let prev2 = i.checked_sub(2).and_then(|j| sa.word_at(j));
        if matches!(prev1, Some("CROSS" | "NATURAL")) || matches!(prev2, Some("CROSS" | "NATURAL"))
        {
            continue;
        }
        let end = sa.clause_end(i + 1);
        let has_on = (i + 1..end)
            .any(|j| sa.is_top(j) && (sa.is_word_at(j, "ON") || sa.is_word_at(j, "USING")));
        if !has_on {
            return true;
        }
    }
    false
}

fn predicate_flags(sa: &Analysis) -> (bool, bool, Vec<String>) {
    let Some(w) = sa.find_top("WHERE") else {
        return (false, false, Vec::new());
    };
    let end = sa.clause_end(w + 1);
    let mut not_in = false;
    let mut null_comparison = false;
    let mut implicit = Vec::new();

    let mut i = w + 1;
    while i < end {
        let tok = &sa.tokens[i];
        if tok.is_word("NOT") && sa.tokens.get(i + 1).is_some_and(|t| t.is_word("IN")) {
            not_in = true;
        }
        if tok.is_word("NOT") && sa.tokens.get(i + 1).is_some_and(|t| t.is_word("LIKE")) {
            // Still a LIKE, not IN.
        }
        // `col = NULL` / `col <> NULL` / `col != NULL`
        if let Some(op) = sa.tokens.get(i + 1) {
            let is_cmp = matches!(op.text.as_str(), "=" | "==" | "<>" | "!=");
            if is_cmp
                && sa.tokens.get(i + 2).is_some_and(|t| t.is_word("NULL"))
                && tok.ident_name().is_some()
            {
                null_comparison = true;
            }
        }
        // `numeric_like_column = 'literal'`
        if let Some(col) = sa.read_column_ref(i) {
            let after = ref_end_at(sa, i);
            if sa.tokens.get(after).is_some_and(|t| t.text == "=")
                && let Some(v) = sa.tokens.get(after + 1).and_then(|t| t.string_value())
                && is_numeric_like(&col.column)
                && !v.is_empty()
                && !implicit.iter().any(|c| c == &col.column)
            {
                implicit.push(col.column);
            }
        }
        i += 1;
    }
    (not_in, null_comparison, implicit)
}

fn ref_end_at(sa: &Analysis, i: usize) -> usize {
    if i + 2 < sa.len() && sa.tokens[i + 1].is_punct('.') && sa.tokens[i + 2].ident_name().is_some()
    {
        i + 3
    } else {
        i + 1
    }
}

/// Reference-compatible heuristic: column names that look numeric are flagged
/// when compared to a quoted literal. No type metadata exists, so this stays a
/// heuristic and is reported as such in `detail.heuristic`.
pub(crate) fn is_numeric_like(column: &str) -> bool {
    let c = column.to_ascii_lowercase();
    [
        "id", "num", "count", "age", "qty", "price", "amount", "code", "status",
    ]
    .iter()
    .any(|k| c.contains(k))
}

fn select_has_aggregate(sa: &Analysis) -> bool {
    for sel in 0..sa.len() {
        if sa.word_at(sel) != Some("SELECT") {
            continue;
        }
        let depth = sa.depth[sel];
        let Some((start, end)) = select_projection_at_depth(sa, sel, depth) else {
            continue;
        };
        for i in start..end {
            if let Some(word) = sa.word_at(i)
                && is_aggregate(word)
                && sa.tokens.get(i + 1).is_some_and(|t| t.is_punct('('))
            {
                return true;
            }
        }
    }
    false
}

fn aggregate_without_group(sa: &Analysis) -> bool {
    if sa.stmt != StmtKind::Select || sa.find_top_seq(&["GROUP", "BY"]).is_some() {
        return false;
    }
    let Some(sel) = sa.main_select else {
        return false;
    };
    let Some((start, end)) = select_projection_at_depth(sa, sel, 0) else {
        return false;
    };
    let mut has_aggregate = false;
    let mut has_plain_column = false;
    let depth = sa.depth[sel];
    for i in start..end {
        if let Some(word) = sa.word_at(i)
            && is_aggregate(word)
            && sa.tokens.get(i + 1).is_some_and(|t| t.is_punct('('))
        {
            has_aggregate = true;
        }
        if sa.depth[i] == depth && sa.read_column_ref(i).is_some() {
            has_plain_column = true;
        }
    }
    has_aggregate && has_plain_column
}

fn is_aggregate(word: &str) -> bool {
    matches!(
        word,
        "COUNT"
            | "SUM"
            | "AVG"
            | "MAX"
            | "MIN"
            | "GROUP_CONCAT"
            | "STRING_AGG"
            | "ARRAY_AGG"
            | "BIT_AND"
            | "BIT_OR"
    )
}

fn temporary_table(sa: &Analysis) -> bool {
    if sa.stmt == StmtKind::Create {
        let mut i = 0;
        while i < sa.len() {
            if sa.is_word_at(i, "TEMPORARY") || sa.is_word_at(i, "TEMP") {
                return true;
            }
            if sa.is_word_at(i, "TABLE") {
                break;
            }
            i += 1;
        }
    }
    // `SELECT ... INTO TEMP|TEMPORARY t` (PostgreSQL/Oracle style).
    if sa.stmt == StmtKind::Select
        && let Some(into) = sa.find_top("INTO")
        && sa
            .tokens
            .get(into + 1)
            .is_some_and(|t| t.is_word("TEMP") || t.is_word("TEMPORARY"))
    {
        return true;
    }
    // SQL Server `#temp` table names.
    sa.tokens
        .iter()
        .any(|t| t.kind == TokenKind::Word && t.text.starts_with('#') && t.text.len() > 1)
}

/// Count `IN (...)` lists with more than 100 literal items.
fn count_big_in_lists(sa: &Analysis) -> usize {
    let mut count = 0;
    let mut i = 0;
    while i < sa.len() {
        if !sa.is_word_at(i, "IN") {
            i += 1;
            continue;
        }
        let Some(open) = (i + 1..sa.len()).find(|&j| sa.tokens[j].is_punct('(')) else {
            break;
        };
        if open != i + 1 {
            i += 1;
            continue;
        }
        let mut level = 1i32;
        let mut j = open + 1;
        let mut items = 1usize;
        let mut has_select = false;
        while j < sa.len() && level > 0 {
            if sa.tokens[j].is_punct('(') {
                level += 1;
            } else if sa.tokens[j].is_punct(')') {
                level -= 1;
                if level == 0 {
                    break;
                }
            } else if sa.depth[j] == sa.depth[open] + 1 {
                if sa.tokens[j].is_punct(',') {
                    items += 1;
                }
                if sa.tokens[j].is_word("SELECT") {
                    has_select = true;
                }
            }
            j += 1;
        }
        if !has_select && items > 100 {
            count += 1;
        }
        i = open + 1;
    }
    count
}
