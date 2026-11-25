//! SQL `SELECT` → MyBatis-Plus `LambdaQueryWrapper` Java chain.
//!
//! The supported surface is deliberately narrow and exact: a single-table
//! `SELECT` with a plain column projection, a boolean `WHERE` built from
//! comparison / `LIKE` / `IN` / `IS NULL` / `BETWEEN` predicates, `ORDER BY`
//! and `LIMIT`/`OFFSET`. Anything else (JOIN, subquery, function on a column,
//! aggregate, `DISTINCT`, `GROUP BY`, a non-literal pattern, …) is returned as
//! `unresolved` with a `message_key` instead of silently generating wrong code.
//!
//! `?` placeholders become identifiers `param1`, `param2`, … listed in
//! `JavaCode::params`; the caller supplies them as method arguments.

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{Confidence, GenError, Unresolved};
use crate::core::perf::sql::{Analysis, StmtKind, TokenKind};

/// Generated Java plus the information needed to use it honestly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JavaCode {
    pub entity: String,
    /// `None` when `confidence == unresolved`.
    pub code: Option<String>,
    pub confidence: Confidence,
    pub unresolved: Vec<Unresolved>,
    /// Java identifiers for `?` placeholders, in order.
    pub params: Vec<String>,
}

impl JavaCode {
    fn unresolved(entity: &str, message_key: &str, detail: serde_json::Value) -> Self {
        Self {
            entity: entity.to_string(),
            code: None,
            confidence: Confidence::Unresolved,
            unresolved: vec![Unresolved::new(message_key, detail)],
            params: Vec::new(),
        }
    }
}

/// Generate a `LambdaQueryWrapper` chain for `sql`.
///
/// `entity` is the Java class name used in `Entity::getField` references. An
/// empty entity name yields an `unresolved` result rather than invented code.
pub fn to_mybatis_plus_java(sql: &str, entity: &str) -> Result<JavaCode, GenError> {
    if sql.trim().is_empty() {
        return Err(GenError::EmptySql);
    }
    let entity = entity.trim();
    if entity.is_empty() {
        return Ok(JavaCode::unresolved(
            entity,
            "gen.java.entity_required",
            json!({}),
        ));
    }
    let sa = Analysis::parse(sql);
    if sa.stmt != StmtKind::Select {
        return Ok(JavaCode::unresolved(
            entity,
            "gen.java.not_select",
            json!({ "statement": format!("{:?}", sa.stmt) }),
        ));
    }
    let refs = sa.table_refs();
    if sa.find_top("UNION").is_some() {
        return Ok(JavaCode::unresolved(
            entity,
            "gen.java.union_unsupported",
            json!({}),
        ));
    }
    if refs.is_empty() {
        return Ok(JavaCode::unresolved(
            entity,
            "gen.java.table_required",
            json!({}),
        ));
    }
    if refs.len() > 1 || sa.find_top("JOIN").is_some() {
        return Ok(JavaCode::unresolved(
            entity,
            "gen.java.join_unsupported",
            json!({ "tables": refs.iter().map(|t| t.name.clone()).collect::<Vec<_>>() }),
        ));
    }
    if sa.find_top_seq(&["GROUP", "BY"]).is_some() {
        return Ok(JavaCode::unresolved(
            entity,
            "gen.java.group_unsupported",
            json!({}),
        ));
    }
    if sa.find_top("HAVING").is_some() {
        return Ok(JavaCode::unresolved(
            entity,
            "gen.java.having_unsupported",
            json!({}),
        ));
    }
    if sa.subquery_depth() > 0 {
        return Ok(JavaCode::unresolved(
            entity,
            "gen.java.subquery_unsupported",
            json!({ "depth": sa.subquery_depth() }),
        ));
    }

    let sel = sa.main_select.expect("select statement has a SELECT");
    if sa
        .tokens
        .get(sel + 1)
        .is_some_and(|t| t.is_word("DISTINCT"))
    {
        return Ok(JavaCode::unresolved(
            entity,
            "gen.java.distinct_unsupported",
            json!({}),
        ));
    }

    let table = &refs[0];
    let table_alias = table.alias.as_deref();
    let table_name = table.name.rsplit('.').next().unwrap_or(&table.name);

    // Projection.
    let from = sa
        .find_top_after("FROM", sel + 1)
        .expect("a plain SELECT has a FROM");
    let mut fields: Vec<String> = Vec::new();
    let mut star = false;
    let mut i = sel + 1;
    if sa.tokens.get(i).is_some_and(|t| t.is_word("ALL")) {
        i += 1;
    }
    while i < from {
        if sa.tokens[i].is_punct(',') {
            i += 1;
            continue;
        }
        if sa.tokens[i].is_punct('*') {
            star = true;
            i += 1;
            continue;
        }
        let Some(col) = sa.read_column_ref(i) else {
            return Ok(JavaCode::unresolved(
                entity,
                "gen.java.unsupported_projection",
                json!({ "token": sa.tokens[i].text }),
            ));
        };
        if !alias_matches(col.alias.as_deref(), table_alias, table_name) {
            return Ok(JavaCode::unresolved(
                entity,
                "gen.java.unknown_table_alias",
                json!({ "alias": col.alias }),
            ));
        }
        let end = ref_end(&sa, i);
        if end < from && !sa.tokens[end].is_punct(',') {
            return Ok(JavaCode::unresolved(
                entity,
                "gen.java.unsupported_projection",
                json!({ "token": sa.tokens[i].text }),
            ));
        }
        fields.push(field_name(&col.column));
        i = end;
    }
    if star && !fields.is_empty() {
        return Ok(JavaCode::unresolved(
            entity,
            "gen.java.unsupported_projection",
            json!({ "reason": "star_with_columns" }),
        ));
    }

    let mut ctx = Ctx {
        sa: &sa,
        entity,
        table_alias,
        table_name,
        next_param: 0,
        params: Vec::new(),
    };

    // WHERE.
    let mut conditions = String::new();
    if let Some(where_idx) = sa.find_top("WHERE") {
        let end = sa.clause_end(where_idx + 1);
        let mut cursor = where_idx + 1;
        if cursor < end {
            let expr = match ctx.parse_or(&mut cursor, end) {
                Ok(e) => e,
                Err(u) => return Ok(ctx.unresolved(u)),
            };
            if cursor < end {
                return Ok(JavaCode::unresolved(
                    entity,
                    "gen.java.unsupported_condition",
                    json!({ "token": sa.tokens[cursor].text }),
                ));
            }
            conditions = ctx.render_root(&expr);
        }
    }

    // ORDER BY.
    let mut order = String::new();
    if let Some(order_idx) = sa.find_top_seq(&["ORDER", "BY"]) {
        let end = sa.clause_end(order_idx + 2);
        let items = cursor_split(&sa, order_idx + 2, end);
        if items.is_empty() {
            return Ok(JavaCode::unresolved(
                entity,
                "gen.java.unsupported_order_by",
                json!({}),
            ));
        }
        for (start, seg_end) in items {
            let Some(col) = sa.read_column_ref(start) else {
                return Ok(JavaCode::unresolved(
                    entity,
                    "gen.java.unsupported_order_by",
                    json!({ "token": sa.tokens.get(start).map(|t| t.text.clone()) }),
                ));
            };
            if !alias_matches(col.alias.as_deref(), table_alias, table_name) {
                return Ok(JavaCode::unresolved(
                    entity,
                    "gen.java.unknown_table_alias",
                    json!({ "alias": col.alias }),
                ));
            }
            let mut k = ref_end(&sa, start);
            let mut desc = false;
            if k < seg_end {
                if sa.is_word_at(k, "DESC") {
                    desc = true;
                    k += 1;
                } else if sa.is_word_at(k, "ASC") {
                    k += 1;
                }
            }
            if k != seg_end {
                return Ok(JavaCode::unresolved(
                    entity,
                    "gen.java.unsupported_order_by",
                    json!({ "token": sa.tokens[k].text }),
                ));
            }
            let field = field_name(&col.column);
            if desc {
                order.push_str(&format!("\n    .orderByDesc({entity}::get{})", cap(&field)));
            } else {
                order.push_str(&format!("\n    .orderByAsc({entity}::get{})", cap(&field)));
            }
        }
    }

    // LIMIT / OFFSET.
    let mut tail = String::new();
    if let Some(lim) = sa.find_top("LIMIT") {
        let end = sa.len();
        let text = limit_text(&sa, lim, end);
        let Some(text) = text else {
            return Ok(JavaCode::unresolved(
                entity,
                "gen.java.limit_unsupported",
                json!({}),
            ));
        };
        tail.push_str(&format!("\n    .last(\"{}\")", text));
    } else if let Some(off) = sa.find_top("OFFSET") {
        match sa.tokens.get(off + 1).and_then(|t| t.number_value()) {
            Some(n) => tail.push_str(&format!("\n    .last(\"OFFSET {n}\")")),
            None => {
                return Ok(JavaCode::unresolved(
                    entity,
                    "gen.java.offset_param_unsupported",
                    json!({}),
                ));
            }
        }
    }

    let mut chain = String::new();
    if !fields.is_empty() {
        let refs = fields
            .iter()
            .map(|f| format!("{entity}::get{}", cap(f)))
            .collect::<Vec<_>>()
            .join(", ");
        chain.push_str(&format!("\n    .select({refs})"));
    }
    chain.push_str(&conditions);
    chain.push_str(&order);
    chain.push_str(&tail);

    let code = format!(
        "LambdaQueryWrapper<{entity}> wrapper = new LambdaQueryWrapper<>();\nwrapper{chain};"
    );

    Ok(JavaCode {
        entity: entity.to_string(),
        code: Some(code),
        confidence: Confidence::Exact,
        unresolved: Vec::new(),
        params: ctx.params,
    })
}

struct Ctx<'a> {
    sa: &'a Analysis,
    entity: &'a str,
    table_alias: Option<&'a str>,
    table_name: &'a str,
    next_param: usize,
    params: Vec<String>,
}

impl<'a> Ctx<'a> {
    fn unresolved(&self, u: Unresolved) -> JavaCode {
        JavaCode {
            entity: self.entity.to_string(),
            code: None,
            confidence: Confidence::Unresolved,
            unresolved: vec![u],
            params: Vec::new(),
        }
    }

    fn param(&mut self) -> String {
        self.next_param += 1;
        let name = format!("param{}", self.next_param);
        self.params.push(name.clone());
        name
    }

    fn parse_or(&mut self, i: &mut usize, end: usize) -> Result<Expr, Unresolved> {
        let mut items = vec![self.parse_and(i, end)?];
        while *i < end && self.sa.is_word_at(*i, "OR") {
            *i += 1;
            items.push(self.parse_and(i, end)?);
        }
        if items.len() == 1 {
            Ok(items.pop().expect("one item"))
        } else {
            Ok(Expr::Or(items))
        }
    }

    fn parse_and(&mut self, i: &mut usize, end: usize) -> Result<Expr, Unresolved> {
        let mut items = vec![self.parse_primary(i, end)?];
        while *i < end && self.sa.is_word_at(*i, "AND") {
            *i += 1;
            items.push(self.parse_primary(i, end)?);
        }
        if items.len() == 1 {
            Ok(items.pop().expect("one item"))
        } else {
            Ok(Expr::And(items))
        }
    }

    fn parse_primary(&mut self, i: &mut usize, end: usize) -> Result<Expr, Unresolved> {
        if *i < end && self.sa.tokens[*i].is_punct('(') {
            *i += 1;
            let inner = self.parse_or(i, end)?;
            if *i >= end || !self.sa.tokens[*i].is_punct(')') {
                return Err(Unresolved::new(
                    "gen.java.unbalanced_parentheses",
                    json!({ "offset": *i }),
                ));
            }
            *i += 1;
            return Ok(Expr::Group(Box::new(inner)));
        }
        self.parse_predicate(i, end)
    }

    fn parse_predicate(&mut self, i: &mut usize, end: usize) -> Result<Expr, Unresolved> {
        let start = *i;
        let Some(col) = self.sa.read_column_ref(start) else {
            return Err(Unresolved::new(
                "gen.java.unsupported_condition",
                json!({ "token": self.sa.tokens.get(start).map(|t| t.text.clone()) }),
            ));
        };
        if !alias_matches(col.alias.as_deref(), self.table_alias, self.table_name) {
            return Err(Unresolved::new(
                "gen.java.unknown_table_alias",
                json!({ "alias": col.alias }),
            ));
        }
        let field = field_name(&col.column);
        let mut j = ref_end(self.sa, start);
        if j >= end {
            return Err(Unresolved::new(
                "gen.java.unsupported_condition",
                json!({ "column": col.column }),
            ));
        }

        // `IS [NOT] NULL`
        if self.sa.is_word_at(j, "IS") {
            let mut k = j + 1;
            let negated = self.sa.is_word_at(k, "NOT");
            if negated {
                k += 1;
            }
            if !self.sa.tokens.get(k).is_some_and(|t| t.is_word("NULL")) {
                return Err(Unresolved::new(
                    "gen.java.unsupported_condition",
                    json!({ "operator": "IS" }),
                ));
            }
            *i = k + 1;
            return Ok(Expr::Pred(Pred {
                method: if negated { "isNotNull" } else { "isNull" },
                field,
                args: Vec::new(),
            }));
        }

        // `NOT LIKE` / `NOT IN` / `NOT BETWEEN`
        let mut negated = false;
        if self.sa.is_word_at(j, "NOT") {
            negated = true;
            j += 1;
            if j >= end {
                return Err(Unresolved::new(
                    "gen.java.unsupported_condition",
                    json!({ "operator": "NOT" }),
                ));
            }
        }

        if self.sa.is_word_at(j, "LIKE") {
            return self.parse_like(i, j, field, negated, end);
        }
        if self.sa.is_word_at(j, "IN") {
            return self.parse_in(i, j, field, negated, end);
        }
        if self.sa.is_word_at(j, "BETWEEN") {
            let (low, mut k) = self.value(j + 1, end)?;
            if !self.sa.is_word_at(k, "AND") {
                return Err(Unresolved::new(
                    "gen.java.unsupported_between",
                    json!({ "offset": k }),
                ));
            }
            k += 1;
            let (high, k) = self.value(k, end)?;
            *i = k;
            return Ok(Expr::Pred(Pred {
                method: if negated { "notBetween" } else { "between" },
                field,
                args: vec![low, high],
            }));
        }

        // Comparison operators.
        let Some(op) = self.sa.tokens.get(j) else {
            return Err(Unresolved::new(
                "gen.java.unsupported_condition",
                json!({ "column": col.column }),
            ));
        };
        if op.kind != TokenKind::Op {
            return Err(Unresolved::new(
                "gen.java.unsupported_condition",
                json!({ "operator": op.text }),
            ));
        }
        let method = match op.text.as_str() {
            "=" | "==" => "eq",
            "<>" | "!=" => "ne",
            ">" => "gt",
            ">=" => "ge",
            "<" => "lt",
            "<=" => "le",
            _ => {
                return Err(Unresolved::new(
                    "gen.java.unsupported_condition",
                    json!({ "operator": op.text }),
                ));
            }
        };
        if self.sa.tokens.get(j + 1).is_some_and(|t| t.is_word("NULL")) {
            return Err(Unresolved::new(
                "gen.java.null_comparison",
                json!({ "column": col.column, "operator": op.text }),
            ));
        }
        let (value, k) = self.value(j + 1, end)?;
        *i = k;
        Ok(Expr::Pred(Pred {
            method,
            field,
            args: vec![value],
        }))
    }

    fn parse_like(
        &mut self,
        i: &mut usize,
        like_idx: usize,
        field: String,
        negated: bool,
        end: usize,
    ) -> Result<Expr, Unresolved> {
        let idx = like_idx + 1;
        if idx >= end {
            return Err(Unresolved::new(
                "gen.java.unsupported_like",
                json!({ "offset": idx }),
            ));
        }
        let tok = &self.sa.tokens[idx];
        let pattern = match tok.kind {
            TokenKind::Str => tok.string_value().unwrap_or_default().to_string(),
            TokenKind::QuotedIdent('"') => tok.value.clone().unwrap_or_default(),
            TokenKind::Param => {
                return Err(Unresolved::new(
                    "gen.java.like_param_unsupported",
                    json!({ "column": field }),
                ));
            }
            _ => {
                return Err(Unresolved::new(
                    "gen.java.unsupported_like",
                    json!({ "token": tok.text }),
                ));
            }
        };
        let leading = pattern.starts_with('%');
        let trailing = pattern.len() > 1 && pattern.ends_with('%');
        if pattern.contains('_') || pattern.contains('\\') {
            return Err(Unresolved::new(
                "gen.java.like_wildcard_unsupported",
                json!({ "pattern": pattern }),
            ));
        }
        if !pattern.contains('%') {
            return Err(Unresolved::new(
                "gen.java.like_without_wildcard",
                json!({ "pattern": pattern }),
            ));
        }
        if !leading && !trailing {
            return Err(Unresolved::new(
                "gen.java.like_middle_wildcard",
                json!({ "pattern": pattern }),
            ));
        }
        let core = pattern.trim_matches('%').to_string();
        let base = match (leading, trailing) {
            (true, true) => "like",
            (true, false) => "likeLeft",
            _ => "likeRight",
        };
        let method = if negated {
            match base {
                "likeLeft" => "notLikeLeft",
                "likeRight" => "notLikeRight",
                _ => "notLike",
            }
        } else {
            base
        };
        *i = idx + 1;
        Ok(Expr::Pred(Pred {
            method,
            field,
            args: vec![Val::Lit(java_string(&core))],
        }))
    }

    fn parse_in(
        &mut self,
        i: &mut usize,
        in_idx: usize,
        field: String,
        negated: bool,
        end: usize,
    ) -> Result<Expr, Unresolved> {
        let open = in_idx + 1;
        if open >= end || !self.sa.tokens[open].is_punct('(') {
            return Err(Unresolved::new(
                "gen.java.unsupported_in",
                json!({ "offset": open }),
            ));
        }
        let mut k = open + 1;
        if k < end && self.sa.is_word_at(k, "SELECT") {
            return Err(Unresolved::new(
                "gen.java.in_subquery_unsupported",
                json!({ "column": field }),
            ));
        }
        let mut values = Vec::new();
        loop {
            if k >= end {
                return Err(Unresolved::new(
                    "gen.java.unbalanced_parentheses",
                    json!({ "offset": k }),
                ));
            }
            if self.sa.tokens[k].is_punct(')') {
                if values.is_empty() {
                    return Err(Unresolved::new(
                        "gen.java.empty_in_list",
                        json!({ "column": field }),
                    ));
                }
                k += 1;
                break;
            }
            let (value, next) = self.value(k, end)?;
            values.push(value);
            k = next;
            if k < end && self.sa.tokens[k].is_punct(',') {
                k += 1;
                continue;
            }
            if k < end && self.sa.tokens[k].is_punct(')') {
                k += 1;
                break;
            }
            return Err(Unresolved::new(
                "gen.java.unsupported_in",
                json!({ "token": self.sa.tokens.get(k).map(|t| t.text.clone()) }),
            ));
        }
        *i = k;
        Ok(Expr::Pred(Pred {
            method: if negated { "notIn" } else { "in" },
            field,
            args: values,
        }))
    }

    fn value(&mut self, idx: usize, end: usize) -> Result<(Val, usize), Unresolved> {
        if idx >= end {
            return Err(Unresolved::new(
                "gen.java.missing_value",
                json!({ "offset": idx }),
            ));
        }
        let tok = &self.sa.tokens[idx];
        let value = match tok.kind {
            TokenKind::Str => Val::Lit(java_string(tok.string_value().unwrap_or_default())),
            TokenKind::Number => Val::Lit(tok.text.clone()),
            TokenKind::Param => Val::Param(self.param()),
            TokenKind::Word => {
                let upper = tok.upper.as_str();
                match upper {
                    "TRUE" => Val::Lit("true".to_string()),
                    "FALSE" => Val::Lit("false".to_string()),
                    "NULL" => {
                        return Err(Unresolved::new(
                            "gen.java.null_comparison",
                            json!({ "offset": idx }),
                        ));
                    }
                    _ => {
                        return Err(Unresolved::new(
                            "gen.java.column_to_column",
                            json!({ "token": tok.text }),
                        ));
                    }
                }
            }
            TokenKind::QuotedIdent('"') => {
                return Err(Unresolved::new(
                    "gen.java.ambiguous_quoted_value",
                    json!({ "token": tok.text }),
                ));
            }
            TokenKind::QuotedIdent(_) => {
                return Err(Unresolved::new(
                    "gen.java.column_to_column",
                    json!({ "token": tok.text }),
                ));
            }
            _ => {
                return Err(Unresolved::new(
                    "gen.java.unsupported_value",
                    json!({ "token": tok.text }),
                ));
            }
        };
        Ok((value, idx + 1))
    }

    // -----------------------------------------------------------------
    // Rendering
    // -----------------------------------------------------------------

    fn render_root(&self, expr: &Expr) -> String {
        match expr {
            Expr::Pred(p) => format!("\n    .{}", p.call(self.entity)),
            Expr::Group(inner) => format!("\n    .and(w -> {})", self.render_lambda(inner, 1)),
            Expr::And(items) => {
                let mut out = String::new();
                for item in items {
                    out.push_str(&self.render_root_and_item(item));
                }
                out
            }
            Expr::Or(items) => {
                let mut out = String::new();
                for (k, item) in items.iter().enumerate() {
                    if k > 0 {
                        out.push_str("\n    .or()");
                    }
                    out.push_str(&self.render_root_or_item(item));
                }
                out
            }
        }
    }

    fn render_root_and_item(&self, expr: &Expr) -> String {
        match expr {
            Expr::Pred(p) => format!("\n    .{}", p.call(self.entity)),
            Expr::Group(inner) => format!("\n    .and(w -> {})", self.render_lambda(inner, 1)),
            other => format!("\n    .and(w -> {})", self.render_lambda(other, 1)),
        }
    }

    fn render_root_or_item(&self, expr: &Expr) -> String {
        match expr {
            Expr::Pred(p) => format!("\n    .{}", p.call(self.entity)),
            Expr::And(items) => {
                let mut out = String::new();
                for item in items {
                    out.push_str(&self.render_root_and_item(item));
                }
                out
            }
            Expr::Group(inner) => format!("\n    .and(w -> {})", self.render_lambda(inner, 1)),
            other => format!("\n    .and(w -> {})", self.render_lambda(other, 1)),
        }
    }

    fn render_lambda(&self, expr: &Expr, depth: usize) -> String {
        let var = if depth == 1 {
            "w".to_string()
        } else {
            format!("w{depth}")
        };
        match expr {
            Expr::Pred(p) => format!("{var}.{}", p.call(self.entity)),
            Expr::Group(inner) => format!(
                "{var}.and(w{} -> {})",
                depth + 1,
                self.render_lambda(inner, depth + 1)
            ),
            Expr::And(items) => {
                let mut out = String::new();
                for (k, item) in items.iter().enumerate() {
                    if k == 0 {
                        out.push_str(&var);
                    }
                    out.push_str(&self.render_lambda_item(item, depth));
                }
                out
            }
            Expr::Or(items) => {
                let mut out = String::new();
                for (k, item) in items.iter().enumerate() {
                    if k == 0 {
                        out.push_str(&var);
                    } else {
                        out.push_str(".or()");
                    }
                    out.push_str(&self.render_lambda_item(item, depth));
                }
                out
            }
        }
    }

    fn render_lambda_item(&self, expr: &Expr, depth: usize) -> String {
        match expr {
            Expr::Pred(p) => format!(".{}", p.call(self.entity)),
            other => format!(
                ".and(w{} -> {})",
                depth + 1,
                self.render_lambda(other, depth + 1)
            ),
        }
    }
}

#[derive(Debug, Clone)]
enum Expr {
    Pred(Pred),
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Group(Box<Expr>),
}

#[derive(Debug, Clone)]
struct Pred {
    method: &'static str,
    field: String,
    args: Vec<Val>,
}

impl Pred {
    fn call(&self, entity: &str) -> String {
        if self.args.is_empty() {
            return format!("{}({entity}::get{})", self.method, cap(&self.field));
        }
        let args = self
            .args
            .iter()
            .map(Val::render)
            .collect::<Vec<_>>()
            .join(", ");
        format!("{}({entity}::get{}, {args})", self.method, cap(&self.field))
    }
}

#[derive(Debug, Clone)]
enum Val {
    Lit(String),
    Param(String),
}

impl Val {
    fn render(&self) -> String {
        match self {
            Val::Lit(s) => s.clone(),
            Val::Param(p) => p.clone(),
        }
    }
}

fn alias_matches(alias: Option<&str>, table_alias: Option<&str>, table_name: &str) -> bool {
    let Some(alias) = alias else {
        return true;
    };
    table_alias.is_some_and(|a| a.eq_ignore_ascii_case(alias))
        || table_name.eq_ignore_ascii_case(alias)
}

fn ref_end(sa: &Analysis, i: usize) -> usize {
    if i + 2 < sa.len() && sa.tokens[i + 1].is_punct('.') && sa.tokens[i + 2].ident_name().is_some()
    {
        i + 3
    } else {
        i + 1
    }
}

/// Split `[start, end)` by top-level commas, returning each segment's bounds.
fn cursor_split(sa: &Analysis, start: usize, end: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut seg = start;
    let depth = sa.depth.get(start).copied().unwrap_or(0);
    for i in start..end {
        if sa.depth[i] == depth && sa.tokens[i].is_punct(',') {
            if seg < i {
                out.push((seg, i));
            }
            seg = i + 1;
        }
    }
    if seg < end {
        out.push((seg, end));
    }
    out
}

fn limit_text(sa: &Analysis, lim: usize, end: usize) -> Option<String> {
    let mut k = lim + 1;
    let first = sa.tokens.get(k)?.number_value()?.to_string();
    k += 1;
    if k < end && sa.tokens[k].is_word("OFFSET") {
        let offset = sa.tokens.get(k + 1)?.number_value()?.to_string();
        k += 2;
        while k < end && sa.tokens[k].is_punct(';') {
            k += 1;
        }
        if k != end {
            return None;
        }
        return Some(format!("LIMIT {first} OFFSET {offset}"));
    }
    if k < end && sa.tokens[k].is_punct(',') {
        let second = sa.tokens.get(k + 1)?.number_value()?.to_string();
        k += 2;
        while k < end && sa.tokens[k].is_punct(';') {
            k += 1;
        }
        if k != end {
            return None;
        }
        return Some(format!("LIMIT {first}, {second}"));
    }
    while k < end && sa.tokens[k].is_punct(';') {
        k += 1;
    }
    if k != end {
        return None;
    }
    Some(format!("LIMIT {first}"))
}

/// `order_code` → `orderCode`; identifiers are lower-cased before camel
/// casing, so the mapping is deterministic.
pub(crate) fn field_name(column: &str) -> String {
    let lower = column.to_ascii_lowercase();
    let mut out = String::new();
    let mut upper = false;
    for ch in lower.chars() {
        if ch == '_' || !ch.is_ascii_alphanumeric() {
            upper = true;
            continue;
        }
        if out.is_empty() {
            out.push(ch);
        } else if upper {
            out.push(ch.to_ascii_uppercase());
            upper = false;
        } else {
            out.push(ch);
        }
    }
    if out.is_empty() {
        "value".to_string()
    } else {
        out
    }
}

fn cap(field: &str) -> String {
    let mut chars = field.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

fn java_string(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generate(sql: &str) -> JavaCode {
        to_mybatis_plus_java(sql, "User").expect("no structural error")
    }

    fn code(sql: &str) -> String {
        generate(sql).code.expect("generated code")
    }

    fn unresolved_key(sql: &str) -> String {
        let out = generate(sql);
        assert_eq!(out.confidence, Confidence::Unresolved);
        assert!(out.code.is_none());
        out.unresolved[0].message_key.clone()
    }

    #[test]
    fn select_by_id_becomes_lambda_eq() {
        let out = generate("SELECT id, name FROM user WHERE id = ?");
        assert_eq!(out.confidence, Confidence::Exact);
        assert_eq!(out.params, vec!["param1"]);
        assert_eq!(
            out.code.unwrap(),
            "LambdaQueryWrapper<User> wrapper = new LambdaQueryWrapper<>();\n\
             wrapper\n    .select(User::getId, User::getName)\n    .eq(User::getId, param1);"
        );
    }

    #[test]
    fn and_chain_is_implicit() {
        let sql = code("SELECT * FROM user WHERE status = 1 AND age > 18");
        assert!(sql.contains(".eq(User::getStatus, 1)"));
        assert!(sql.contains(".gt(User::getAge, 18)"));
    }

    #[test]
    fn or_chain_uses_or_method() {
        let sql = code("SELECT id FROM user WHERE a = 1 OR b = 2");
        assert!(sql.contains(".eq(User::getA, 1)"));
        assert!(sql.contains(".or()"));
        assert!(sql.contains(".eq(User::getB, 2)"));
    }

    #[test]
    fn parenthesized_or_group_uses_and_consumer() {
        let sql = code("SELECT id FROM user WHERE a = 1 AND (b = 2 OR c = 3)");
        assert!(
            sql.contains(".and(w -> w.eq(User::getB, 2).or().eq(User::getC, 3))"),
            "sql={sql}"
        );
    }

    #[test]
    fn like_positions_map_to_like_left_right_and_strip_wildcards() {
        assert!(
            code("SELECT id FROM user WHERE name LIKE 'abc%'")
                .contains(".likeRight(User::getName, \"abc\")")
        );
        assert!(
            code("SELECT id FROM user WHERE name LIKE '%abc'")
                .contains(".likeLeft(User::getName, \"abc\")")
        );
        assert!(
            code("SELECT id FROM user WHERE name LIKE '%abc%'")
                .contains(".like(User::getName, \"abc\")")
        );
        assert!(
            code("SELECT id FROM user WHERE name NOT LIKE 'abc%'")
                .contains(".notLikeRight(User::getName, \"abc\")")
        );
    }

    #[test]
    fn in_and_not_in_lists() {
        assert!(
            code("SELECT id FROM user WHERE status IN (1, 2, 3)")
                .contains(".in(User::getStatus, 1, 2, 3)")
        );
        assert!(
            code("SELECT id FROM user WHERE status NOT IN (1, 2)")
                .contains(".notIn(User::getStatus, 1, 2)")
        );
        let out = generate("SELECT id FROM user WHERE status IN (?, ?)");
        assert_eq!(out.params, vec!["param1", "param2"]);
        assert!(
            out.code
                .unwrap()
                .contains(".in(User::getStatus, param1, param2)")
        );
    }

    #[test]
    fn comparison_operators_cover_gt_ge_lt_le_ne() {
        let sql =
            code("SELECT id FROM user WHERE a > 1 AND b >= 2 AND c < 3 AND d <= 4 AND e <> 5");
        for expected in [
            ".gt(User::getA, 1)",
            ".ge(User::getB, 2)",
            ".lt(User::getC, 3)",
            ".le(User::getD, 4)",
            ".ne(User::getE, 5)",
        ] {
            assert!(sql.contains(expected), "missing {expected} in {sql}");
        }
    }

    #[test]
    fn is_null_and_is_not_null() {
        assert!(code("SELECT id FROM user WHERE name IS NULL").contains(".isNull(User::getName)"));
        assert!(
            code("SELECT id FROM user WHERE name IS NOT NULL")
                .contains(".isNotNull(User::getName)")
        );
    }

    #[test]
    fn between_maps_to_between() {
        assert!(
            code("SELECT id FROM user WHERE age BETWEEN 18 AND 30")
                .contains(".between(User::getAge, 18, 30)")
        );
        assert!(
            code("SELECT id FROM user WHERE age NOT BETWEEN 18 AND 30")
                .contains(".notBetween(User::getAge, 18, 30)")
        );
    }

    #[test]
    fn order_by_maps_to_asc_and_desc() {
        let sql = code("SELECT id FROM user ORDER BY created_at DESC, id ASC");
        assert!(sql.contains(".orderByDesc(User::getCreatedAt)"));
        assert!(sql.contains(".orderByAsc(User::getId)"));
    }

    #[test]
    fn limit_and_offset_become_last_clauses() {
        assert!(code("SELECT id FROM user LIMIT 10").contains(".last(\"LIMIT 10\")"));
        assert!(
            code("SELECT id FROM user LIMIT 10 OFFSET 20")
                .contains(".last(\"LIMIT 10 OFFSET 20\")")
        );
        assert!(code("SELECT id FROM user LIMIT 20, 10").contains(".last(\"LIMIT 20, 10\")"));
    }

    #[test]
    fn qualified_columns_resolve_against_the_table_alias() {
        let sql = code("SELECT u.id, u.name FROM user u WHERE u.status = 1");
        assert!(sql.contains(".select(User::getId, User::getName)"));
        assert!(sql.contains(".eq(User::getStatus, 1)"));
    }

    #[test]
    fn snake_case_columns_become_camel_case_fields() {
        let sql = code("SELECT user_id FROM user WHERE order_code = 'A'");
        assert!(sql.contains(".select(User::getUserId)"));
        assert!(sql.contains(".eq(User::getOrderCode, \"A\")"));
    }

    #[test]
    fn parameters_are_numbered_in_place() {
        let out =
            generate("SELECT id FROM user WHERE a = ? AND b > ? AND c IN (?, ?) AND d LIKE '%x%'");
        assert_eq!(out.params, vec!["param1", "param2", "param3", "param4"]);
    }

    #[test]
    fn join_is_unresolved() {
        assert_eq!(
            unresolved_key("SELECT u.id FROM user u JOIN address a ON a.user_id = u.id"),
            "gen.java.join_unsupported"
        );
    }

    #[test]
    fn function_projection_is_unresolved() {
        assert_eq!(
            unresolved_key("SELECT count(*) FROM user"),
            "gen.java.unsupported_projection"
        );
        assert_eq!(
            unresolved_key("SELECT id, date(created_at) FROM user"),
            "gen.java.unsupported_projection"
        );
    }

    #[test]
    fn function_on_column_in_where_is_unresolved() {
        assert_eq!(
            unresolved_key("SELECT id FROM user WHERE date(created_at) = '2024-01-01'"),
            "gen.java.unsupported_condition"
        );
    }

    #[test]
    fn column_to_column_comparison_is_unresolved() {
        assert_eq!(
            unresolved_key("SELECT id FROM user WHERE name = other_name"),
            "gen.java.column_to_column"
        );
    }

    #[test]
    fn subquery_is_unresolved() {
        assert_eq!(
            unresolved_key("SELECT id FROM user WHERE id IN (SELECT id FROM banned)"),
            "gen.java.subquery_unsupported"
        );
        assert_eq!(
            unresolved_key("SELECT id, (SELECT max(x) FROM t) FROM user"),
            "gen.java.subquery_unsupported"
        );
    }

    #[test]
    fn distinct_group_and_union_are_unresolved() {
        assert_eq!(
            unresolved_key("SELECT DISTINCT id FROM user"),
            "gen.java.distinct_unsupported"
        );
        assert_eq!(
            unresolved_key("SELECT id, count(*) FROM user GROUP BY id"),
            "gen.java.group_unsupported"
        );
        assert_eq!(
            unresolved_key("SELECT id FROM a UNION SELECT id FROM b"),
            "gen.java.union_unsupported"
        );
    }

    #[test]
    fn parameterized_like_and_pattern_without_wildcard_are_unresolved() {
        assert_eq!(
            unresolved_key("SELECT id FROM user WHERE name LIKE ?"),
            "gen.java.like_param_unsupported"
        );
        assert_eq!(
            unresolved_key("SELECT id FROM user WHERE name LIKE 'abc'"),
            "gen.java.like_without_wildcard"
        );
        assert_eq!(
            unresolved_key("SELECT id FROM user WHERE name LIKE 'a%b'"),
            "gen.java.like_middle_wildcard"
        );
    }

    #[test]
    fn unknown_alias_is_unresolved() {
        assert_eq!(
            unresolved_key("SELECT x.id FROM user u"),
            "gen.java.unknown_table_alias"
        );
    }

    #[test]
    fn null_comparison_is_unresolved_not_silently_rewritten() {
        assert_eq!(
            unresolved_key("SELECT id FROM user WHERE name = NULL"),
            "gen.java.null_comparison"
        );
    }

    #[test]
    fn empty_entity_and_non_select_are_unresolved() {
        let out = to_mybatis_plus_java("SELECT id FROM user", "  ").unwrap();
        assert_eq!(out.unresolved[0].message_key, "gen.java.entity_required");
        assert_eq!(
            unresolved_key("UPDATE user SET name = 'x' WHERE id = 1"),
            "gen.java.not_select"
        );
    }

    #[test]
    fn empty_sql_is_a_structural_error() {
        assert_eq!(
            to_mybatis_plus_java("   ", "User").unwrap_err(),
            GenError::EmptySql
        );
    }

    #[test]
    fn result_is_serde_roundtrippable() {
        let out = generate("SELECT id FROM user WHERE id = ?");
        let json = serde_json::to_string(&out).unwrap();
        let back: JavaCode = serde_json::from_str(&json).unwrap();
        assert_eq!(back, out);
    }
}
