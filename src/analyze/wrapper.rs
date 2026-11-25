//! Text → `WrapperChain`: MyBatis-Plus wrapper source to a structured chain and
//! rendered SQL.
//!
//! First version covers `LambdaQueryWrapper`, `LambdaUpdateWrapper` and
//! `QueryWrapper` chains with the methods listed in `docs/TASKS.md` T5-4:
//! `and`/`or`/`eq`/`ne`/`gt`/`ge`/`lt`/`le`/`like`/`likeLeft`/`likeRight`/
//! `in`/`notIn`/`isNull`/`isNotNull`/`between`/`orderByAsc`/`orderByDesc`/
//! `limit`/`last`.
//!
//! ## What this module is (and is not) allowed to do
//!
//! - It never calls `host::` — the field→column table is supplied by the caller
//!   (`Project::symbols` + entity annotations), so this stays pure and testable
//!   (`docs/ARCHITECTURE.md` §6, `src/core/` rules).
//! - It never guesses. A field that is not in [`EntityColumns`] makes the result
//!   [`Confidence::Unresolved`] (no SQL). A runtime variable used as a value
//!   renders as a `?` placeholder and downgrades to [`Confidence::Partial`]; an
//!   unsupported method is recorded and downgrades to `Partial`.
//!
//! ## Update wrappers
//!
//! A `LambdaUpdateWrapper`/`UpdateWrapper` renders only its condition fragment
//! (`WHERE …`), marked `Partial`: the `UPDATE <table> SET …` verb and the `SET`
//! list come from the mapper XML, not the wrapper. Emitting a full `UPDATE`
//! from the wrapper alone would be a guess.

use crate::core::dialect::{Dialect, IdentifierCaseSource};

/// Result confidence, matching the product model (`docs/ARCHITECTURE.md` §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    /// Every condition resolved from literals; SQL is runnable.
    Exact,
    /// SQL emitted but with at least one placeholder or an unsupported piece.
    Partial,
    /// A key input is missing; no SQL is emitted.
    Unresolved,
}

/// The entity → table/column projection the caller supplies.
///
/// `columns` is `(field_name, column_name, sql_type)` — the exact shape from
/// `docs/TASKS.md` T5-4. The field name is the Java field (`active`), not the
/// getter (`isActive`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EntityColumns {
    pub class_name: String,
    pub table: String,
    pub columns: Vec<(String, String, String)>,
}

impl EntityColumns {
    pub fn new(class_name: &str, table: &str) -> Self {
        EntityColumns {
            class_name: class_name.to_string(),
            table: table.to_string(),
            columns: Vec::new(),
        }
    }

    /// Adds a column mapping (builder style).
    pub fn with_column(mut self, field: &str, column: &str, sql_type: &str) -> Self {
        self.columns
            .push((field.to_string(), column.to_string(), sql_type.to_string()));
        self
    }

    /// The column for a Java field, if mapped.
    pub fn column_for(&self, field: &str) -> Option<&str> {
        self.columns
            .iter()
            .find(|(f, _, _)| f == field)
            .map(|(_, c, _)| c.as_str())
    }
}

/// Which statement family the wrapper belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WrapperKind {
    #[default]
    Query,
    Update,
}

/// How a condition joins to the previous one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Connector {
    #[default]
    And,
    Or,
}

/// Comparison operator of a leaf condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    Like,
    LikeLeft,
    LikeRight,
    In,
    NotIn,
    IsNull,
    IsNotNull,
    Between,
}

/// A rendered value argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Str(String),
    /// Kept as the literal text (already a numeric literal).
    Num(String),
    Bool(bool),
    Null,
    /// A runtime expression we cannot evaluate — renders as `?`.
    Param,
}

impl Value {
    fn is_param(&self) -> bool {
        matches!(self, Value::Param)
    }
}

/// One leaf condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Condition {
    pub connector: Connector,
    pub op: Op,
    pub column: String,
    pub values: Vec<Value>,
}

/// A condition tree node: a leaf or a parenthesized group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CondNode {
    Leaf(Condition),
    Group {
        connector: Connector,
        inner: Vec<CondNode>,
    },
}

/// One `ORDER BY` item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderBy {
    pub column: String,
    pub desc: bool,
}

/// The structured wrapper chain (before rendering).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WrapperChain {
    pub kind: WrapperKind,
    pub table: Option<String>,
    pub conditions: Vec<CondNode>,
    pub order_by: Vec<OrderBy>,
    pub limit: Option<u64>,
    pub last: Option<String>,
    /// Methods seen but not rendered (each also raises a `message_key`).
    pub unsupported: Vec<String>,
}

/// The render result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrapperOutput {
    pub confidence: Confidence,
    pub sql: Option<String>,
    pub chain: WrapperChain,
    /// Localization keys for the UI; empty when `Exact`.
    pub message_keys: Vec<String>,
    pub notes: Vec<String>,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Render a wrapper source snippet.
///
/// `columns` supplies the field → column projection; `dialect` controls pagination
/// shape and identifier quoting.
pub fn render(text: &str, columns: &EntityColumns, dialect: &Dialect) -> WrapperOutput {
    let mut ctx = Ctx {
        columns,
        notes: Vec::new(),
        message_keys: Vec::new(),
        partial: false,
        unresolved: false,
    };

    let calls = extract_calls(text);
    let detected = detect_kind(text);
    // A bare `new LambdaQueryWrapper<User>()` has no chain calls but is still a
    // valid (unconditional) wrapper: it renders a plain SELECT. Only text with
    // neither a wrapper type nor any call is "not a wrapper".
    if calls.is_empty() && detected.is_none() {
        return WrapperOutput {
            confidence: Confidence::Unresolved,
            sql: None,
            chain: WrapperChain {
                table: non_empty(&columns.table),
                ..Default::default()
            },
            message_keys: vec!["wrapper.not_a_wrapper".to_string()],
            notes: vec!["no wrapper class or method call found in the source".to_string()],
        };
    }

    let kind = detected.unwrap_or(WrapperKind::Query);
    let mut chain = WrapperChain {
        kind,
        table: non_empty(&columns.table),
        ..Default::default()
    };

    let mut pending = Connector::And;
    let nodes = parse_calls(&calls, &mut pending, &mut chain, &mut ctx);

    chain.conditions = nodes;

    // Missing table is a hard stop: the statement can't be built.
    if chain.table.is_none() {
        ctx.unresolved = true;
        push_key(&mut ctx, "wrapper.table_unknown");
        ctx.notes
            .push("the entity table is unknown; pass EntityColumns.table".to_string());
    }
    if chain.kind == WrapperKind::Update {
        ctx.partial = true;
        push_key(&mut ctx, "wrapper.update_fragment_only");
        ctx.notes.push(
            "update wrappers render only the condition fragment; UPDATE/SET come from the mapper"
                .to_string(),
        );
    }

    let confidence = if ctx.unresolved {
        Confidence::Unresolved
    } else if ctx.partial {
        Confidence::Partial
    } else {
        Confidence::Exact
    };

    let sql = match (confidence, chain.kind) {
        (Confidence::Unresolved, _) => None,
        (_, WrapperKind::Query) => Some(render_select(&chain, dialect)),
        (_, WrapperKind::Update) => render_fragment(&chain, dialect),
    };

    WrapperOutput {
        confidence,
        sql,
        chain,
        message_keys: ctx.message_keys,
        notes: ctx.notes,
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

struct Ctx<'a> {
    columns: &'a EntityColumns,
    notes: Vec<String>,
    message_keys: Vec<String>,
    partial: bool,
    unresolved: bool,
}

fn push_key(ctx: &mut Ctx, key: &str) {
    if !ctx.message_keys.iter().any(|k| k == key) {
        ctx.message_keys.push(key.to_string());
    }
}

struct Call {
    name: String,
    args: Vec<String>,
}

/// Extract `name(args)` calls at punctuation depth 0, skipping strings/comments.
fn extract_calls(text: &str) -> Vec<Call> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i] as char;
        match c {
            '"' | '\'' => i = skip_string(bytes, i),
            '/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            '/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            }
            '(' => {
                depth += 1;
                i += 1;
            }
            ')' => {
                depth = (depth - 1).max(0);
                i += 1;
            }
            '.' if depth == 0 => {
                if let Some((call, next)) = try_call(text, bytes, i) {
                    out.push(call);
                    i = next;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    out
}

fn skip_string(bytes: &[u8], start: usize) -> usize {
    let quote = bytes[start];
    let mut i = start + 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
        } else if bytes[i] == quote {
            return i + 1;
        } else {
            i += 1;
        }
    }
    i
}

/// Try to read a `.name(args)` call whose dot is at `dot`.
fn try_call(text: &str, bytes: &[u8], dot: usize) -> Option<(Call, usize)> {
    let mut j = dot + 1;
    while j < bytes.len() && bytes[j] == b' ' {
        j += 1;
    }
    let name_start = j;
    while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
        j += 1;
    }
    if j == name_start {
        return None;
    }
    let name = text[name_start..j].to_string();
    while j < bytes.len() && bytes[j] == b' ' {
        j += 1;
    }
    if bytes.get(j) != Some(&b'(') {
        return None;
    }
    // Find the matching close paren.
    let mut depth = 0i32;
    let mut k = j;
    while k < bytes.len() {
        match bytes[k] {
            b'"' | b'\'' => {
                k = skip_string(bytes, k);
                continue;
            }
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    let args = split_args(&text[j + 1..k]);
                    return Some((Call { name, args }, k + 1));
                }
            }
            _ => {}
        }
        k += 1;
    }
    None
}

/// Split a call's argument text on top-level commas (respecting nested (), [], {}, strings).
fn split_args(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' => {
                i = skip_string(bytes, i);
                continue;
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = (depth - 1).max(0),
            b',' if depth == 0 => {
                let arg = text[start..i].trim();
                if !arg.is_empty() {
                    out.push(arg.to_string());
                }
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    let tail = text[start..].trim();
    if !tail.is_empty() {
        out.push(tail.to_string());
    }
    out
}

fn detect_kind(text: &str) -> Option<WrapperKind> {
    // Check the longer/more specific names first: "LambdaQueryWrapper" contains
    // the substring "QueryWrapper".
    for (name, kind) in [
        ("LambdaUpdateWrapper", WrapperKind::Update),
        ("UpdateWrapper", WrapperKind::Update),
        ("LambdaQueryWrapper", WrapperKind::Query),
        ("QueryWrapper", WrapperKind::Query),
    ] {
        if text.contains(name) {
            return Some(kind);
        }
    }
    if text.contains("lambdaUpdate") || text.contains("lambdaQuery") {
        return Some(if text.contains("lambdaUpdate") {
            WrapperKind::Update
        } else {
            WrapperKind::Query
        });
    }
    None
}

fn parse_calls(
    calls: &[Call],
    pending: &mut Connector,
    chain: &mut WrapperChain,
    ctx: &mut Ctx,
) -> Vec<CondNode> {
    let mut nodes = Vec::new();
    for call in calls {
        match call.name.as_str() {
            // Factory / meta calls that build the wrapper, not conditions.
            "lambdaQuery" | "query" | "lambdaUpdate" | "update" => {}
            "or" if call.args.is_empty() => *pending = Connector::Or,
            "and" if call.args.is_empty() => *pending = Connector::And,
            "and" | "or" => {
                // Group form: `.and(w -> w.eq(..).or().eq(..))`.
                let connector = if call.name == "or" {
                    Connector::Or
                } else {
                    Connector::And
                };
                let inner_text = call.args.join(",");
                let body = strip_lambda_prefix(&inner_text);
                let inner_calls = extract_calls(&body);
                if inner_calls.is_empty() {
                    ctx.partial = true;
                    push_key(ctx, "wrapper.unsupported_method");
                    ctx.notes
                        .push(format!("could not parse the `.{}` group", call.name));
                    chain.unsupported.push(format!("{}(group)", call.name));
                } else {
                    let mut inner_pending = Connector::And;
                    let inner = parse_calls(&inner_calls, &mut inner_pending, chain, ctx);
                    nodes.push(CondNode::Group { connector, inner });
                }
            }
            "orderByAsc" | "orderByDesc" => {
                let desc = call.name == "orderByDesc";
                for arg in &call.args {
                    match resolve_column(arg, ctx) {
                        Some(column) => chain.order_by.push(OrderBy { column, desc }),
                        None => {
                            ctx.unresolved = true;
                            push_key(ctx, "wrapper.unknown_field");
                            ctx.notes
                                .push(format!("cannot map ORDER BY argument `{arg}` to a column"));
                        }
                    }
                }
            }
            "limit" => match call.args.first().and_then(|a| parse_u64(a)) {
                Some(n) => chain.limit = Some(n),
                None => {
                    ctx.partial = true;
                    push_key(ctx, "wrapper.limit_not_literal");
                    chain.unsupported.push("limit".to_string());
                }
            },
            "last" => match call.args.first().and_then(|a| string_literal(a)) {
                Some(s) => chain.last = Some(s),
                None => {
                    ctx.partial = true;
                    push_key(ctx, "wrapper.last_not_literal");
                    chain.unsupported.push("last".to_string());
                }
            },
            name => {
                if let Some(op) = op_of(name) {
                    match parse_leaf(op, &call.args, *pending, ctx) {
                        LeafOutcome::Node(node) => nodes.push(CondNode::Leaf(node)),
                        LeafOutcome::Skip => {}
                        LeafOutcome::Failed => {}
                    }
                } else {
                    ctx.partial = true;
                    push_key(ctx, "wrapper.unsupported_method");
                    ctx.notes
                        .push(format!("`.{name}` is not rendered by this version"));
                    chain.unsupported.push(name.to_string());
                }
            }
        }
    }
    nodes
}

fn strip_lambda_prefix(text: &str) -> String {
    // `w -> w.eq(...)` → `w.eq(...)`
    if let Some(idx) = text.find("->") {
        let (lhs, rhs) = text.split_at(idx);
        if lhs
            .trim()
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c.is_whitespace())
        {
            return rhs[2..].to_string();
        }
    }
    text.to_string()
}

fn op_of(name: &str) -> Option<Op> {
    Some(match name {
        "eq" => Op::Eq,
        "ne" => Op::Ne,
        "gt" => Op::Gt,
        "ge" => Op::Ge,
        "lt" => Op::Lt,
        "le" => Op::Le,
        "like" => Op::Like,
        "likeLeft" => Op::LikeLeft,
        "likeRight" => Op::LikeRight,
        "in" => Op::In,
        "notIn" => Op::NotIn,
        "isNull" => Op::IsNull,
        "isNotNull" => Op::IsNotNull,
        "between" => Op::Between,
        _ => return None,
    })
}

enum LeafOutcome {
    Node(Condition),
    Skip,
    Failed,
}

fn parse_leaf(op: Op, raw_args: &[String], connector: Connector, ctx: &mut Ctx) -> LeafOutcome {
    let base = match op {
        Op::IsNull | Op::IsNotNull => 1,
        Op::Between => 2,
        Op::In | Op::NotIn => 1, // column + ≥1 values
        _ => 2,
    };

    // Conditional form `eq(boolean condition, column, value)`: skip when false.
    let mut args: &[String] = raw_args;
    if args.len() > base {
        match args.first().and_then(|a| parse_bool(a)) {
            Some(false) => return LeafOutcome::Skip,
            Some(true) => args = &args[1..],
            None => {}
        }
    }

    let column = match args.first() {
        Some(raw) => match resolve_column(raw, ctx) {
            Some(c) => c,
            None => {
                ctx.unresolved = true;
                push_key(ctx, "wrapper.unknown_field");
                ctx.notes.push(format!(
                    "cannot map `{raw}` to a column (field not in EntityColumns)"
                ));
                return LeafOutcome::Failed;
            }
        },
        None => return LeafOutcome::Failed,
    };

    let values: Vec<Value> = args.iter().skip(1).map(|a| parse_value(a)).collect();
    for v in &values {
        if v.is_param() {
            ctx.partial = true;
            push_key(ctx, "wrapper.param_value");
        }
    }

    match op {
        Op::IsNull | Op::IsNotNull => {}
        Op::Between => {
            if values.len() != 2 {
                ctx.partial = true;
                push_key(ctx, "wrapper.between_arity");
                ctx.notes
                    .push("between needs exactly two values".to_string());
            }
        }
        Op::In | Op::NotIn => {
            if values.is_empty() {
                ctx.partial = true;
                push_key(ctx, "wrapper.empty_in");
                ctx.notes
                    .push("in/notIn with no values renders `IN (NULL)`".to_string());
            }
        }
        _ => {}
    }

    LeafOutcome::Node(Condition {
        connector,
        op,
        column,
        values,
    })
}

// ---------------------------------------------------------------------------
// Argument decoding
// ---------------------------------------------------------------------------

fn non_empty(s: &str) -> Option<String> {
    if s.trim().is_empty() {
        None
    } else {
        Some(s.trim().to_string())
    }
}

fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn parse_u64(raw: &str) -> Option<u64> {
    raw.trim().trim_end_matches(['L', 'l']).parse().ok()
}

fn is_number(raw: &str) -> bool {
    let t = raw.trim();
    if t.is_empty() {
        return false;
    }
    let t = t.trim_end_matches(['L', 'l', 'f', 'F', 'd', 'D']);
    t.parse::<f64>().is_ok()
}

/// The content of a `"..."` or `'...'` literal, or `None`.
fn string_literal(raw: &str) -> Option<String> {
    let t = raw.trim();
    let bytes = t.as_bytes();
    if bytes.len() >= 2 {
        let q = bytes[0];
        if (q == b'"' || q == b'\'') && bytes[bytes.len() - 1] == q {
            return Some(unescape(&t[1..t.len() - 1]));
        }
    }
    None
}

fn unescape(inner: &str) -> String {
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn parse_value(raw: &str) -> Value {
    let t = raw.trim();
    if t == "null" || t == "NULL" {
        return Value::Null;
    }
    if let Some(b) = parse_bool(t) {
        return Value::Bool(b);
    }
    if let Some(s) = string_literal(t) {
        return Value::Str(s);
    }
    if is_number(t) {
        return Value::Num(t.trim_end_matches(['L', 'l']).to_string());
    }
    Value::Param
}

/// A lambda field reference `User::getStatus` → `(User, getStatus)`.
fn parse_field_ref(raw: &str) -> Option<(String, String)> {
    let t = raw.trim();
    let (recv, getter) = t.split_once("::")?;
    let recv = recv.trim();
    let getter = getter.trim();
    if is_ident(recv) && is_ident(getter) {
        Some((recv.to_string(), getter.to_string()))
    } else {
        None
    }
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    }
}

/// Map a wrapper condition argument (a lambda field ref or a `QueryWrapper`
/// column string) to the real column.
fn resolve_column(raw: &str, ctx: &mut Ctx) -> Option<String> {
    if let Some((recv, getter)) = parse_field_ref(raw) {
        if !ctx.columns.class_name.is_empty() && recv != ctx.columns.class_name {
            ctx.partial = true;
            push_key(ctx, "wrapper.receiver_mismatch");
            ctx.notes.push(format!(
                "receiver `{recv}` differs from entity `{}`",
                ctx.columns.class_name
            ));
        }
        let field = field_from_getter(&getter);
        ctx.columns.column_for(&field).map(str::to_string)
    } else if let Some(s) = string_literal(raw) {
        // QueryWrapper passes the column name directly.
        Some(s)
    } else {
        None
    }
}

/// `getStatus` → `status`, `isActive` → `active`, `getURL` → `URL`.
fn field_from_getter(getter: &str) -> String {
    if let Some(rest) = getter.strip_prefix("get") {
        decap(rest)
    } else if let Some(rest) = getter.strip_prefix("is") {
        decap(rest)
    } else {
        decap(getter)
    }
}

fn decap(s: &str) -> String {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let rest: String = chars.collect();
    let second_upper = rest.chars().next().is_some_and(|c| c.is_uppercase());
    if first.is_uppercase() && second_upper {
        format!("{first}{rest}") // JavaBeans: "URL" stays "URL"
    } else {
        let mut out = String::new();
        out.extend(first.to_lowercase());
        out.push_str(&rest);
        out
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn quote(ident: &str, dialect: &Dialect) -> String {
    dialect.quote_if_needed(ident, IdentifierCaseSource::Entity)
}

fn render_select(chain: &WrapperChain, dialect: &Dialect) -> String {
    let mut sql = match chain.table.as_deref() {
        // The SELECT list is `*`: the wrapper does not carry a column list, and
        // the mapper's `<select>` supplies it (`SELECT *` is the neutral case).
        Some(t) => format!("SELECT * FROM {}", quote(t, dialect)),
        None => "SELECT *".to_string(),
    };
    if let Some(where_sql) = render_where(chain, dialect) {
        sql.push(' ');
        sql.push_str(&where_sql);
    }
    sql.push_str(&render_clauses(chain, dialect));
    sql
}

fn render_fragment(chain: &WrapperChain, dialect: &Dialect) -> Option<String> {
    let where_part = render_where(chain, dialect)?;
    let mut sql = where_part;
    sql.push_str(&render_clauses(chain, dialect));
    Some(sql)
}

/// ORDER BY / LIMIT / LAST only (the WHERE clause belongs to the caller).
fn render_clauses(chain: &WrapperChain, dialect: &Dialect) -> String {
    let mut out = String::new();
    if !chain.order_by.is_empty() {
        let items: Vec<String> = chain
            .order_by
            .iter()
            .map(|o| {
                format!(
                    "{} {}",
                    quote(&o.column, dialect),
                    if o.desc { "DESC" } else { "ASC" }
                )
            })
            .collect();
        out.push_str(" ORDER BY ");
        out.push_str(&items.join(", "));
    }
    if let Some(limit) = chain.limit {
        out.push(' ');
        out.push_str(&dialect.render_limit(limit, None));
    }
    if let Some(last) = &chain.last {
        out.push(' ');
        out.push_str(last);
    }
    out
}

fn render_where(chain: &WrapperChain, dialect: &Dialect) -> Option<String> {
    if chain.conditions.is_empty() {
        return None;
    }
    let body = render_nodes(&chain.conditions, dialect);
    if body.is_empty() {
        None
    } else {
        Some(format!("WHERE {body}"))
    }
}

fn render_nodes(nodes: &[CondNode], dialect: &Dialect) -> String {
    let mut parts = Vec::new();
    let mut first = true;
    for node in nodes {
        let (connector, body) = match node {
            CondNode::Leaf(c) => (c.connector, render_leaf(c, dialect)),
            CondNode::Group { connector, inner } => {
                (*connector, format!("({})", render_nodes(inner, dialect)))
            }
        };
        if body.is_empty() {
            continue;
        }
        if first {
            parts.push(body);
            first = false;
        } else {
            parts.push(format!("{} {}", connector_sql(connector), body));
        }
    }
    parts.join(" ")
}

fn connector_sql(c: Connector) -> &'static str {
    match c {
        Connector::And => "AND",
        Connector::Or => "OR",
    }
}

fn render_leaf(c: &Condition, dialect: &Dialect) -> String {
    let col = quote(&c.column, dialect);
    match c.op {
        Op::IsNull => format!("{col} IS NULL"),
        Op::IsNotNull => format!("{col} IS NOT NULL"),
        Op::Between => {
            let a = c.values.first().map(|v| render_value(v, dialect));
            let b = c.values.get(1).map(|v| render_value(v, dialect));
            match (a, b) {
                (Some(a), Some(b)) => format!("{col} BETWEEN {a} AND {b}"),
                (Some(a), None) => format!("{col} BETWEEN {a} AND ?"),
                _ => format!("{col} BETWEEN ? AND ?"),
            }
        }
        Op::In | Op::NotIn => {
            let kw = if c.op == Op::In { "IN" } else { "NOT IN" };
            let list = if c.values.is_empty() {
                "NULL".to_string() // an empty IN matches nothing
            } else {
                c.values
                    .iter()
                    .map(|v| render_value(v, dialect))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            format!("{col} {kw} ({list})")
        }
        Op::Like | Op::LikeLeft | Op::LikeRight => {
            let raw = c
                .values
                .first()
                .map(value_pattern_text)
                .unwrap_or_else(|| "?".to_string());
            let pattern = match c.op {
                Op::LikeLeft => format!("%{raw}"),
                Op::LikeRight => format!("{raw}%"),
                _ => format!("%{raw}%"),
            };
            let quoted = if raw == "?" {
                "?".to_string()
            } else {
                format!("'{}'", escape_sql(&pattern))
            };
            format!("{col} LIKE {quoted}")
        }
        _ => {
            let op = match c.op {
                Op::Eq => "=",
                Op::Ne => "<>",
                Op::Gt => ">",
                Op::Ge => ">=",
                Op::Lt => "<",
                Op::Le => "<=",
                _ => "=",
            };
            let value = c
                .values
                .first()
                .map(|v| render_value(v, dialect))
                .unwrap_or_else(|| "?".to_string());
            format!("{col} {op} {value}")
        }
    }
}

/// The textual core of a LIKE value (no wildcards yet).
fn value_pattern_text(v: &Value) -> String {
    match v {
        Value::Str(s) => s.clone(),
        Value::Num(n) => n.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "NULL".to_string(),
        Value::Param => "?".to_string(),
    }
}

fn render_value(v: &Value, dialect: &Dialect) -> String {
    match v {
        Value::Str(s) => format!("'{}'", escape_sql(s)),
        Value::Num(n) => n.clone(),
        Value::Bool(b) => {
            if dialect.capabilities.supports_boolean_literal {
                if *b {
                    "TRUE".to_string()
                } else {
                    "FALSE".to_string()
                }
            } else if *b {
                "1".to_string()
            } else {
                "0".to_string()
            }
        }
        Value::Null => "NULL".to_string(),
        Value::Param => "?".to_string(),
    }
}

fn escape_sql(s: &str) -> String {
    s.replace('\'', "''")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::dialect;

    fn cols() -> EntityColumns {
        EntityColumns::new("User", "sys_user")
            .with_column("id", "id", "bigint")
            .with_column("name", "name", "varchar")
            .with_column("status", "status", "varchar")
            .with_column("age", "age", "int")
            .with_column("createdAt", "created_at", "timestamp")
    }

    fn mysql() -> &'static Dialect {
        dialect::by_name("MySQL").expect("MySQL dialect")
    }
    fn pg() -> &'static Dialect {
        dialect::by_name("PostgreSQL").expect("PostgreSQL dialect")
    }
    fn oracle() -> &'static Dialect {
        dialect::by_name("Oracle").expect("Oracle dialect")
    }

    fn exact_sql(src: &str, dialect: &Dialect) -> String {
        let out = render(src, &cols(), dialect);
        assert_eq!(
            out.confidence,
            Confidence::Exact,
            "src={src}\nkeys={:?}\nnotes={:?}",
            out.message_keys,
            out.notes
        );
        out.sql.expect("an exact result carries SQL")
    }

    /// The 30-case corpus: every entry must be `exact`, ≥90% is the T5-4 bar
    /// (this corpus is 100% exact; the partial/unresolved cases live below).
    #[test]
    fn corpus_of_thirty_exact_wrapper_renders() {
        let cases: &[(&str, &str)] = &[
            (
                r#"new LambdaQueryWrapper<User>().eq(User::getStatus, "A")"#,
                "SELECT * FROM sys_user WHERE status = 'A'",
            ),
            (
                "new LambdaQueryWrapper<User>().eq(User::getAge, 18)",
                "SELECT * FROM sys_user WHERE age = 18",
            ),
            (
                r#"new LambdaQueryWrapper<User>().ne(User::getStatus, "A")"#,
                "SELECT * FROM sys_user WHERE status <> 'A'",
            ),
            (
                "new LambdaQueryWrapper<User>().gt(User::getAge, 18)",
                "SELECT * FROM sys_user WHERE age > 18",
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
                "new LambdaQueryWrapper<User>().le(User::getAge, 18)",
                "SELECT * FROM sys_user WHERE age <= 18",
            ),
            (
                r#"new LambdaQueryWrapper<User>().like(User::getName, "ab")"#,
                "SELECT * FROM sys_user WHERE name LIKE '%ab%'",
            ),
            (
                r#"new LambdaQueryWrapper<User>().likeLeft(User::getName, "ab")"#,
                "SELECT * FROM sys_user WHERE name LIKE '%ab'",
            ),
            (
                r#"new LambdaQueryWrapper<User>().likeRight(User::getName, "ab")"#,
                "SELECT * FROM sys_user WHERE name LIKE 'ab%'",
            ),
            (
                r#"new LambdaQueryWrapper<User>().in(User::getStatus, "A", "B")"#,
                "SELECT * FROM sys_user WHERE status IN ('A', 'B')",
            ),
            (
                r#"new LambdaQueryWrapper<User>().notIn(User::getStatus, "A")"#,
                "SELECT * FROM sys_user WHERE status NOT IN ('A')",
            ),
            (
                "new LambdaQueryWrapper<User>().isNull(User::getName)",
                "SELECT * FROM sys_user WHERE name IS NULL",
            ),
            (
                "new LambdaQueryWrapper<User>().isNotNull(User::getName)",
                "SELECT * FROM sys_user WHERE name IS NOT NULL",
            ),
            (
                "new LambdaQueryWrapper<User>().between(User::getAge, 18, 30)",
                "SELECT * FROM sys_user WHERE age BETWEEN 18 AND 30",
            ),
            (
                "new LambdaQueryWrapper<User>().orderByAsc(User::getAge)",
                "SELECT * FROM sys_user ORDER BY age ASC",
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
                r#"new LambdaQueryWrapper<User>().last("FOR UPDATE")"#,
                "SELECT * FROM sys_user FOR UPDATE",
            ),
            (
                r#"new LambdaQueryWrapper<User>().eq(User::getStatus, "A").gt(User::getAge, 18)"#,
                "SELECT * FROM sys_user WHERE status = 'A' AND age > 18",
            ),
            (
                r#"new LambdaQueryWrapper<User>().eq(User::getStatus, "A").or().eq(User::getStatus, "B")"#,
                "SELECT * FROM sys_user WHERE status = 'A' OR status = 'B'",
            ),
            (
                "new LambdaQueryWrapper<User>().and(w -> w.eq(User::getAge, 18).or().eq(User::getAge, 30))",
                "SELECT * FROM sys_user WHERE (age = 18 OR age = 30)",
            ),
            (
                r#"new QueryWrapper<User>().eq("status", "A")"#,
                "SELECT * FROM sys_user WHERE status = 'A'",
            ),
            (
                r#"new LambdaQueryWrapper<User>().eq(true, User::getStatus, "A")"#,
                "SELECT * FROM sys_user WHERE status = 'A'",
            ),
            (
                r#"new LambdaQueryWrapper<User>().eq(false, User::getStatus, "A")"#,
                "SELECT * FROM sys_user",
            ),
            (
                "new LambdaQueryWrapper<User>().eq(User::getStatus, true)",
                "SELECT * FROM sys_user WHERE status = TRUE",
            ),
            (
                r#"new LambdaQueryWrapper<User>().like(User::getName, "a'b")"#,
                "SELECT * FROM sys_user WHERE name LIKE '%a''b%'",
            ),
            (
                "new LambdaQueryWrapper<User>().orderByAsc(User::getAge).orderByDesc(User::getCreatedAt)",
                "SELECT * FROM sys_user ORDER BY age ASC, created_at DESC",
            ),
            (
                r#"new LambdaQueryWrapper<User>().limit(3).last("FOR UPDATE")"#,
                "SELECT * FROM sys_user LIMIT 3 FOR UPDATE",
            ),
            (
                "new LambdaQueryWrapper<User>().eq(User::getStatus, 1L)",
                "SELECT * FROM sys_user WHERE status = 1",
            ),
            ("new LambdaQueryWrapper<User>()", "SELECT * FROM sys_user"),
        ];

        assert!(cases.len() >= 30, "corpus must have at least 30 entries");
        let mut exact = 0;
        for (src, expected) in cases {
            let out = render(src, &cols(), mysql());
            assert_eq!(out.confidence, Confidence::Exact, "src={src}\n{out:?}");
            assert_eq!(out.sql.as_deref(), Some(*expected), "src={src}\n{out:?}");
            exact += 1;
        }
        assert!(
            exact * 100 / cases.len() >= 90,
            "exact rate {exact}/{} below 90%",
            cases.len()
        );
    }

    #[test]
    fn postgres_and_oracle_pagination_use_the_dialect() {
        assert_eq!(
            exact_sql("new LambdaQueryWrapper<User>().limit(5)", pg()),
            "SELECT * FROM sys_user LIMIT 5"
        );
        assert_eq!(
            exact_sql("new LambdaQueryWrapper<User>().limit(5)", oracle()),
            "SELECT * FROM sys_user FETCH FIRST 5 ROWS ONLY"
        );
    }

    #[test]
    fn oracle_has_no_boolean_literal_so_true_renders_as_one() {
        assert_eq!(
            exact_sql(
                "new LambdaQueryWrapper<User>().eq(User::getStatus, true)",
                oracle()
            ),
            "SELECT * FROM sys_user WHERE status = 1"
        );
    }

    #[test]
    fn update_wrapper_renders_only_a_partial_condition_fragment() {
        let out = render(
            r#"new LambdaUpdateWrapper<User>().eq(User::getStatus, "A")"#,
            &cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Partial);
        assert_eq!(out.sql.as_deref(), Some("WHERE status = 'A'"));
        assert_eq!(out.chain.kind, WrapperKind::Update);
        assert!(
            out.message_keys
                .contains(&"wrapper.update_fragment_only".to_string())
        );
    }

    #[test]
    fn a_receiver_mismatch_degrades_to_partial_but_still_resolves_the_column() {
        let out = render(
            r#"new LambdaQueryWrapper<User>().eq(Other::getStatus, "A")"#,
            &cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Partial);
        assert_eq!(
            out.sql.as_deref(),
            Some("SELECT * FROM sys_user WHERE status = 'A'")
        );
        assert!(
            out.message_keys
                .contains(&"wrapper.receiver_mismatch".to_string())
        );
    }

    #[test]
    fn an_unsupported_method_degrades_to_partial_and_is_recorded() {
        let out = render(
            r#"new LambdaQueryWrapper<User>().apply("x")"#,
            &cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Partial);
        assert_eq!(out.sql.as_deref(), Some("SELECT * FROM sys_user"));
        assert!(
            out.message_keys
                .contains(&"wrapper.unsupported_method".to_string())
        );
        assert!(out.chain.unsupported.contains(&"apply".to_string()));
    }

    #[test]
    fn an_unknown_field_is_unresolved_and_emits_no_sql() {
        let out = render(
            r#"new LambdaQueryWrapper<User>().eq(User::getNope, "A")"#,
            &cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Unresolved);
        assert_eq!(out.sql, None);
        assert!(
            out.message_keys
                .contains(&"wrapper.unknown_field".to_string())
        );
    }

    #[test]
    fn a_runtime_value_renders_as_a_placeholder_and_degrades_to_partial() {
        let out = render(
            r#"new LambdaQueryWrapper<User>().eq(User::getStatus, statusVar)"#,
            &cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Partial);
        assert_eq!(
            out.sql.as_deref(),
            Some("SELECT * FROM sys_user WHERE status = ?")
        );
        assert!(
            out.message_keys
                .contains(&"wrapper.param_value".to_string())
        );
    }

    #[test]
    fn text_without_a_wrapper_is_unresolved() {
        let out = render("int x = 1;", &cols(), mysql());
        assert_eq!(out.confidence, Confidence::Unresolved);
        assert_eq!(out.sql, None);
        assert!(
            out.message_keys
                .contains(&"wrapper.not_a_wrapper".to_string())
        );
    }

    #[test]
    fn field_from_getter_handles_is_get_and_url_casing() {
        assert_eq!(field_from_getter("getStatus"), "status");
        assert_eq!(field_from_getter("isActive"), "active");
        assert_eq!(field_from_getter("getURL"), "URL");
    }

    #[test]
    fn in_with_a_list_literal_is_not_guessed_and_becomes_a_param() {
        // `Arrays.asList(...)` is not a literal we evaluate — it is a `?`.
        let out = render(
            r#"new LambdaQueryWrapper<User>().in(User::getStatus, Arrays.asList("A", "B"))"#,
            &cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Partial);
        assert_eq!(
            out.sql.as_deref(),
            Some("SELECT * FROM sys_user WHERE status IN (?)")
        );
    }
}
