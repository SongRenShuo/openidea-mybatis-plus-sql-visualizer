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
    /// Projections of the entities the chain joins to (`leftJoin(Car.class,
    /// …)`): the renderer resolves a lambda column against the entity its
    /// receiver names, which is also how MPJ picks the ON-side table alias.
    /// The command layer fills this from the project index; empty = single-table.
    pub related: Vec<EntityColumns>,
}

impl EntityColumns {
    pub fn new(class_name: &str, table: &str) -> Self {
        EntityColumns {
            class_name: class_name.to_string(),
            table: table.to_string(),
            columns: Vec::new(),
            related: Vec::new(),
        }
    }

    /// Adds a column mapping (builder style).
    pub fn with_column(mut self, field: &str, column: &str, sql_type: &str) -> Self {
        self.columns
            .push((field.to_string(), column.to_string(), sql_type.to_string()));
        self
    }

    /// Adds a joined entity's projection (builder style).
    pub fn with_related(mut self, entity: EntityColumns) -> Self {
        self.related.push(entity);
        self
    }

    /// The column for a Java field, if mapped.
    pub fn column_for(&self, field: &str) -> Option<&str> {
        self.columns
            .iter()
            .find(|(f, _, _)| f == field)
            .map(|(_, c, _)| c.as_str())
    }

    /// The projection a lambda column's receiver names: this entity, or one of
    /// the joined ones. The receiver may be a simple name (`Car`) or fully
    /// qualified (`com.x.Car`); the tail after the last dot decides.
    pub fn find(&self, class_name: &str) -> Option<&EntityColumns> {
        let tail = class_name.rsplit('.').next().unwrap_or(class_name);
        if self.class_name == tail {
            return Some(self);
        }
        self.related.iter().find(|entity| entity.class_name == tail)
    }

    /// MPJ's default table alias for an entity: the class name with its first
    /// letter lowercased (`AddressDO` → `addressDO`), the rule the reference
    /// documents. The main table keeps the short `t` instead.
    pub fn alias_of(&self, class_name: &str) -> String {
        let tail = class_name.rsplit('.').next().unwrap_or(class_name);
        let mut chars = tail.chars();
        match chars.next() {
            Some(first) => first.to_lowercase().collect::<String>() + chars.as_str(),
            None => tail.to_string(),
        }
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
    Exists,
    NotExists,
}

/// A rendered value argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Str(String),
    /// Kept as the literal text (already a numeric literal).
    Num(String),
    Bool(bool),
    Null,
    /// A runtime expression we cannot evaluate — renders as `?`. The expression
    /// text is kept so the preview can name the parameter row (`#username`)
    /// the way the IntelliJ edition's `ArgResolver` does.
    Param(String),
    /// A column reference used where a value would go — the ON lambda's
    /// `on.eq(AreaDO::getId, AddressDO::getAreaId)` right side. Renders as the
    /// qualified column, never as a parameter.
    Column(String),
    /// A rendered sub-select (`in(User::getId, AddressDO.class, sub -> …)`),
    /// already single-line. The parentheses come from the operator.
    SubQuery(String),
}

impl Value {
    fn is_param(&self) -> bool {
        matches!(self, Value::Param(_))
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
    /// MPJ join clauses, in call order.
    pub joins: Vec<Join>,
    /// Tables `selectAll()` / `selectAll(X.class)` named, in call order; the
    /// empty string is the main table. Expanded against the projections at
    /// render time, the way the reference builds `selectAll` columns.
    pub select_tables: Vec<String>,
    /// Columns `selectIgnore` dropped from the expanded list.
    pub select_ignored: Vec<String>,
    /// The column-name prefix `selectAll(X.class, "p")` puts before every
    /// expanded column (`p` + column), the reference's `parseSelectAll` prefix.
    pub select_prefix: Option<String>,
    /// The explicit SELECT list. Empty means "every column of the queried
    /// table(s)" — expanded at render time, the way the reference does.
    pub select: Vec<SelectColumn>,
    /// `select("…")` fragments, passed through verbatim.
    pub select_raw: Vec<String>,
    /// `GROUP BY` columns / fragments.
    pub group_by: Vec<String>,
    /// `having("…", args)` with `{i}` already replaced by the rendered values.
    pub having: Option<String>,
    /// Parameters the HAVING fragment contributed, in SQL order.
    pub having_params: Vec<Value>,
    /// The main table's alias (`setAlias("x")`); MPJ's default is `t`.
    pub main_alias: Option<String>,
    pub distinct: bool,
}

/// How a join combines the joined table (`LEFT JOIN` / `INNER JOIN` / …).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinKind {
    Left,
    Inner,
    Right,
    Full,
}

impl JoinKind {
    fn sql(self) -> &'static str {
        match self {
            JoinKind::Left => "LEFT JOIN",
            JoinKind::Inner => "INNER JOIN",
            JoinKind::Right => "RIGHT JOIN",
            JoinKind::Full => "FULL JOIN",
        }
    }
}

/// One MPJ join clause, resolved against the joined entity's projection.
///
/// `alias` is MPJ's default (the entity class decapitalised) or the explicit
/// one from the four/five-argument form. `on` is the condition tree the chain
/// built for this join — column-to-column comparisons and any `ext` lambda
/// conditions — with every column already qualified by its own entity's alias
/// (the reference's A47 rule, restated over text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Join {
    pub kind: JoinKind,
    pub table: String,
    pub alias: String,
    pub on: Vec<CondNode>,
    /// A raw join fragment (`leftJoin("user_address addr on t.id = addr.user_id")`)
    /// is passed through verbatim after the join keyword.
    pub raw: Option<String>,
    /// The sub-table consumer form
    /// (`leftJoin(C, t -> t.setAlias("tt").selectAll().ge(…), left, right)`):
    /// the joined relation is `(sub-select)` rather than a table. The reference
    /// renders it as `LEFT JOIN (SELECT … FROM sub tt WHERE …) tt ON …`, with
    /// the sub-alias from the lambda's `setAlias` or the class decapitalised.
    pub sub_select: Option<String>,
}

/// One column of the SELECT list: which table alias it belongs to, its column,
/// and the `AS` name (`selectAs` / the aggregate functions set one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectColumn {
    pub alias: String,
    pub column: String,
    pub as_name: Option<String>,
    /// `COUNT` / `SUM` / `MAX` / `MIN` / `AVG` — wrapped around the column.
    pub function: Option<&'static str>,
}

/// The render result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrapperOutput {
    pub confidence: Confidence,
    pub sql: Option<String>,
    pub chain: WrapperChain,
    /// The runtime expressions that became `?`, in the order the SQL has them.
    /// The preview shows one editable row per entry, named after the expression
    /// (`#username`), the way the IntelliJ edition's parameter column does.
    pub params: Vec<String>,
    /// Localization keys for the UI; empty when `Exact`.
    pub message_keys: Vec<String>,
    pub notes: Vec<String>,
}

/// The `#name` of every `?` in `sql`, in order, taken from the chain.
///
/// A leaf renders its values in order, and every leaf in `conditions` renders
/// (a skipped `eq(false, …)` never reaches the tree), so walking the tree the
/// way `render_nodes` does yields exactly the placeholders' order.
pub fn param_sources(nodes: &[CondNode]) -> Vec<String> {
    let mut out = Vec::new();
    for node in nodes {
        match node {
            CondNode::Leaf(condition) => {
                for value in &condition.values {
                    if let Value::Param(source) = value {
                        out.push(source.clone());
                    }
                }
            }
            CondNode::Group { inner, .. } => out.extend(param_sources(inner)),
        }
    }
    out
}

/// One row per condition: `(id, label)`.
///
/// `id` is the node's position in the tree (`c0`, `c0.1`, …) — stable across a
/// re-render with some branches switched off, which is what lets the page keep
/// the reader's choices under the same paths. A group is one row labelled with
/// its rendered body; its children follow, so a nested `or(…)` can be switched
/// as a whole and per branch, as the reference's Wrapper rows do.
pub fn condition_labels(nodes: &[CondNode], dialect: &Dialect) -> Vec<(String, String)> {
    labels_with(nodes, CONDITION_ID_PREFIX, dialect)
}

fn labels_with(nodes: &[CondNode], prefix: &str, dialect: &Dialect) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        let id = format!("{prefix}{index}");
        let label = match node {
            CondNode::Leaf(condition) => render_leaf(condition, dialect),
            CondNode::Group { inner, .. } => format!("({})", render_nodes(inner, dialect)),
        };
        if label.is_empty() {
            continue;
        }
        out.push((id.clone(), label));
        if let CondNode::Group { inner, .. } = node {
            out.extend(labels_with(inner, &format!("{id}."), dialect));
        }
    }
    out
}

/// The prefix every condition id starts with, so a page can tell a wrapper row
/// from a mapper `<if>` id at a glance.
const CONDITION_ID_PREFIX: &str = "c";

/// The tree with the branches the reader switched off removed. Ids are the
/// positions in the *original* tree, so a switch keeps meaning the same node
/// after its neighbours changed.
fn filter_disabled(
    nodes: &[CondNode],
    prefix: &str,
    disabled: &std::collections::BTreeSet<String>,
) -> Vec<CondNode> {
    let mut out = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        let id = format!("{prefix}{index}");
        if disabled.contains(&id) {
            continue;
        }
        match node {
            CondNode::Leaf(leaf) => out.push(CondNode::Leaf(leaf.clone())),
            CondNode::Group { connector, inner } => out.push(CondNode::Group {
                connector: *connector,
                inner: filter_disabled(inner, &format!("{id}."), disabled),
            }),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Render a wrapper source snippet.
///
/// `columns` supplies the field → column projection; `dialect` controls pagination
/// shape and identifier quoting.
pub fn render(text: &str, columns: &EntityColumns, dialect: &Dialect) -> WrapperOutput {
    render_with(text, columns, dialect, &std::collections::BTreeSet::new())
}

/// [`render`] with the condition branches the preview's reader switched off.
///
/// The returned `chain` keeps the *whole* tree — the page draws one row per
/// node and needs the switched-off ones to stay listed — while `sql` and
/// `params` come from the filtered tree.
pub fn render_with(
    text: &str,
    columns: &EntityColumns,
    dialect: &Dialect,
    disabled: &std::collections::BTreeSet<String>,
) -> WrapperOutput {
    render_with_alias(text, columns, dialect, disabled, None)
}

/// [`render_with`] under an explicit main-table alias: a sub-table consumer
/// renders its joined relation as a sub-select declared under the sub-alias
/// (`(SELECT … FROM sub tt WHERE …) tt`), so every qualified column inside it
/// has to use that name rather than the `t` default.
pub fn render_with_alias(
    text: &str,
    columns: &EntityColumns,
    dialect: &Dialect,
    disabled: &std::collections::BTreeSet<String>,
    alias: Option<&str>,
) -> WrapperOutput {
    // MPJ recognition must come from the whole text, not from the call list: a
    // chain written `JoinWrappers.lambda(User.class)` names its factory in the
    // *call* (`lambda`), while `new MPJLambdaWrapper<User>()` names it in the
    // construction — neither contains the plain `LambdaQueryWrapper` token.
    let mpj = text.contains("MPJLambdaWrapper")
        || text.contains("MPJQueryWrapper")
        || text.contains("JoinWrappers")
        || text.contains("MPJWrappers");
    // A join-update/delete writes several tables at once: the SET list and the
    // multi-table DELETE target are statement structure this renderer does not
    // build, so it refuses rather than emitting a single-table statement.
    if mpj && (text.contains("JoinWrappers.update(") || text.contains("JoinWrappers.delete(")) {
        return structural_output(
            columns,
            vec!["`.updateJoin`/`.deleteJoin` writes several tables at once; the SET list and the delete targets are statement structure this version does not render".to_string()],
            vec!["updateJoin".to_string()],
            &["wrapper.structural_method".to_string()],
        );
    }

    let mut ctx = Ctx {
        columns,
        notes: Vec::new(),
        message_keys: Vec::new(),
        partial: false,
        unresolved: false,
        mpj,
        main_alias: alias.unwrap_or("t").to_string(),
        join_aliases: Vec::new(),
        on_mode: false,
        dialect,
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
            params: Vec::new(),
            message_keys: vec!["wrapper.not_a_wrapper".to_string()],
            notes: vec!["no wrapper class or method call found in the source".to_string()],
        };
    }

    // Pre-scan the explicit join aliases: a condition may name a joined entity
    // before the join call appears in the chain, and the alias has to be known
    // by then. The default (class decapitalised) is computed on demand.
    if mpj {
        for call in &calls {
            if !is_join_method(&call.name) {
                continue;
            }
            if let (Some(class), Some(alias)) = (
                call.args.first().and_then(|a| class_literal(a)),
                call.args.get(1).and_then(|a| string_literal(a)),
            ) {
                ctx.join_aliases.push((class, alias));
            }
        }
    }

    let kind = detected.unwrap_or(WrapperKind::Query);
    let mut chain = WrapperChain {
        kind,
        table: non_empty(&columns.table),
        main_alias: if mpj {
            Some(ctx.main_alias.clone())
        } else {
            None
        },
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

    // The SQL comes from the switched-on branches only; the chain keeps the
    // whole tree so the page can still list what the reader switched off.
    let mut effective = chain.clone();
    effective.conditions = filter_disabled(&chain.conditions, CONDITION_ID_PREFIX, disabled);

    let sql = match (confidence, chain.kind) {
        (Confidence::Unresolved, _) => None,
        (_, WrapperKind::Query) => Some(render_select(&effective, columns, dialect)),
        (_, WrapperKind::Update) => render_fragment(&effective, dialect),
    };

    let mut params = param_sources(&effective.conditions);
    params.extend(chain.having_params.iter().filter_map(|value| match value {
        Value::Param(source) => Some(source.clone()),
        _ => None,
    }));
    WrapperOutput {
        confidence,
        sql,
        chain,
        params,
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
    /// True once the chain is recognised as MPJ (`MPJLambdaWrapper`,
    /// `JoinWrappers`, `MPJWrappers`): every column reference then resolves
    /// against its own entity's alias, and the statement renders with table
    /// aliases instead of `SELECT *`.
    mpj: bool,
    /// The main table's alias (`t`, or `setAlias("x")`).
    main_alias: String,
    /// Explicit join aliases the chain declared, `(class, alias)`, collected by
    /// the pre-scan so a condition before the join call still resolves.
    join_aliases: Vec<(String, String)>,
    /// True while parsing an ON lambda: the second argument of a comparison is
    /// a column reference, not a value.
    on_mode: bool,
    /// The dialect, for rendering `HAVING` fragments and their values at parse
    /// time (the fragment is baked once, in call order).
    dialect: &'a Dialect,
}

/// The methods whose effect cannot be left out of the SQL: dropping them
/// changes which rows and which columns the statement returns.
///
/// `.apply("...")` merely narrows a condition the renderer did not understand —
/// the statement still means "these rows, with one filter missing", which
/// `Partial` says honestly. A dropped `leftJoin` or `selectAll` is different in
/// kind: `selectList(new MPJLambdaWrapper<User>().selectAll().leftJoin(...))`
/// narrows nothing, it *builds the row shape*, and rendering
/// `SELECT * FROM user` for it hands the reader a statement the query does not
/// produce and that may well select from the wrong rows.
///
/// So this set is "loses structure", not "unsupported": such a chain is
/// [`Confidence::Unresolved`] and ships no SQL, exactly like an unmapped field.
const STRUCTURAL_METHODS: [&str; 11] = [
    "selectFunc",
    "selectSub",
    "selectCollection",
    "selectAssociation",
    "selectMax",
    "selectMin",
    "selectSum",
    "selectAvg",
    "from",
    "around",
    "join",
];

/// Whether a call builds a FROM-side join clause.
fn is_join_method(name: &str) -> bool {
    matches!(name, "leftJoin" | "innerJoin" | "rightJoin" | "fullJoin")
}

/// A chain-argument entity class (`Car.class` → `Car`). Bare identifiers that
/// merely look like classes are rejected — the `.class` suffix is what MPJ
/// writes.
fn class_literal(arg: &str) -> Option<String> {
    let text = arg.trim();
    let class = text.strip_suffix(".class")?;
    if class.is_empty()
        || !class
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$' || c == '.')
    {
        return None;
    }
    Some(class.rsplit('.').next().unwrap_or(class).to_string())
}

/// An `unresolved` render that names the structure it could not build. The
/// chain still carries the table, so the reason line can point at the entity.
fn structural_output(
    columns: &EntityColumns,
    notes: Vec<String>,
    unsupported: Vec<String>,
    keys: &[String],
) -> WrapperOutput {
    WrapperOutput {
        confidence: Confidence::Unresolved,
        sql: None,
        chain: WrapperChain {
            table: non_empty(&columns.table),
            unsupported,
            ..Default::default()
        },
        params: Vec::new(),
        message_keys: keys.to_vec(),
        notes,
    }
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
            "lambdaQuery" | "query" | "lambdaUpdate" | "update" | "delete" | "lambda" => {}
            // `setAlias("x")` names the main table; `setTableName` is a runtime
            // table swap the text cannot evaluate (the reference treats it as a
            // no-op too).
            "setAlias" => {
                if ctx.mpj
                    && let Some(alias) = call.args.first().and_then(|a| string_literal(a))
                {
                    chain.main_alias = Some(alias.clone());
                    ctx.main_alias = alias;
                }
            }
            "setTableName" => {}
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
            // Sub-query conditions (`in(col, Class, sub -> …)` / `exists(Class,
            // sub -> …)`): the lambda renders as its own SELECT, wrapped by the
            // operator.
            name @ ("in" | "notIn")
                if call.args.len() == 3
                    && call.args.get(1).and_then(|a| class_literal(a)).is_some() =>
            {
                parse_in_subquery(name, &call.args, pending, chain, ctx, &mut nodes)
            }
            name @ ("exists" | "notExists")
                if call.args.first().and_then(|a| class_literal(a)).is_some() =>
            {
                parse_exists_subquery(name, &call.args, pending, chain, ctx, &mut nodes)
            }
            // `exists("select …")`: the caller wrote the whole sub-select; it
            // renders verbatim inside the operator.
            name @ ("exists" | "notExists")
                if call.args.len() == 1
                    && call.args.first().and_then(|a| string_literal(a)).is_some() =>
            {
                let sql = call
                    .args
                    .first()
                    .and_then(|a| string_literal(a))
                    .expect("checked");
                ctx.partial = true;
                push_key(ctx, "wrapper.raw_fragment");
                nodes.push(CondNode::Leaf(Condition {
                    connector: *pending,
                    op: if name == "exists" {
                        Op::Exists
                    } else {
                        Op::NotExists
                    },
                    column: String::new(),
                    values: vec![Value::SubQuery(sql)],
                }));
                *pending = Connector::And;
            }
            name if is_join_method(name) => parse_join(name, &call.args, chain, ctx),
            name @ ("selectAll" | "select" | "selectAs" | "selectIgnore" | "selectCount"
            | "selectSum" | "selectMax" | "selectMin" | "selectAvg") => {
                parse_select(name, &call.args, ctx, chain)
            }
            "groupBy" => {
                for arg in &call.args {
                    match resolve_column(arg, ctx) {
                        Some(column) => chain.group_by.push(column),
                        None => {
                            ctx.unresolved = true;
                            push_key(ctx, "wrapper.unknown_field");
                            ctx.notes
                                .push(format!("cannot map GROUP BY argument `{arg}` to a column"));
                        }
                    }
                }
            }
            "having" => parse_having(&call.args, ctx, chain),
            "distinct" => chain.distinct = true,
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
                } else if STRUCTURAL_METHODS.contains(&name) {
                    // Dropping this changes which rows or columns come back, so
                    // the chain cannot be rendered as `SELECT * FROM <table>`:
                    // that statement answers a different question. Same rule as
                    // an unmapped field — no SQL, reason instead.
                    ctx.unresolved = true;
                    push_key(ctx, "wrapper.structural_method");
                    ctx.notes.push(format!(
                        "`.{name}` builds the statement's structure (join or select list); it is not rendered by this version, so no SQL is emitted"
                    ));
                    chain.unsupported.push(name.to_string());
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

// ---------------------------------------------------------------------------
// MPJ: joins, select lists, grouping
// ---------------------------------------------------------------------------

/// Records an unrenderable structural method: no SQL comes out of this chain.
fn structural(ctx: &mut Ctx, note: String) {
    ctx.unresolved = true;
    push_key(ctx, "wrapper.structural_method");
    ctx.notes.push(note);
    ctx.partial = false;
}

/// The table alias MPJ gives `class`: the explicit one a join declared, else
/// the class name decapitalised. The main table always uses `main_alias`.
fn alias_for_class(ctx: &Ctx, class: &str) -> String {
    if ctx
        .columns
        .find(class)
        .is_some_and(|found| found.class_name == ctx.columns.class_name)
    {
        return ctx.main_alias.clone();
    }
    ctx.join_aliases
        .iter()
        .find(|(name, _)| name == class)
        .map(|(_, alias)| alias.clone())
        .unwrap_or_else(|| ctx.columns.alias_of(class))
}

/// A column reference resolved to `(alias, column)`: the alias comes from the
/// entity the lambda's receiver names (the reference's A47 rule — the ON side
/// and the condition side both qualify by their own entity), the column from
/// that entity's projection.
fn resolve_qualified(raw: &str, ctx: &mut Ctx) -> Option<(String, String)> {
    let (class, getter) = parse_field_ref(raw)?;
    let entity = ctx.columns.find(&class)?;
    let field = field_from_getter(&getter);
    let column = entity.column_for(&field)?.to_string();
    Some((alias_for_class(ctx, &class), column))
}

fn parse_join(name: &str, args: &[String], chain: &mut WrapperChain, ctx: &mut Ctx) {
    let kind = match name {
        "leftJoin" => JoinKind::Left,
        "innerJoin" => JoinKind::Inner,
        "rightJoin" => JoinKind::Right,
        _ => JoinKind::Full,
    };
    // A raw fragment (`leftJoin("user_address addr on t.id = addr.user_id")`)
    // is rendered verbatim after the join keyword — the caller wrote the whole
    // clause, and nothing here can improve on it.
    if let Some(raw) = args.first().and_then(|a| string_literal(a)) {
        chain.joins.push(Join {
            kind,
            table: String::new(),
            alias: String::new(),
            on: Vec::new(),
            raw: Some(raw),
            sub_select: None,
        });
        return;
    }
    let Some(class) = args.first().and_then(|a| class_literal(a)) else {
        structural(
            ctx,
            format!("`.{name}` names no entity class this renderer can read"),
        );
        chain.unsupported.push(name.to_string());
        return;
    };
    let Some(entity) = ctx.columns.find(&class).cloned() else {
        // The joined table is unknown: rendering `LEFT JOIN ??` would be a
        // guess, and a wrong table name is worse than no SQL.
        ctx.unresolved = true;
        push_key(ctx, "wrapper.join_table_unknown");
        ctx.notes.push(format!(
            "the joined entity `{class}` is not in the projection; its table name is needed for the `{name}` clause"
        ));
        ctx.partial = false;
        chain.unsupported.push(name.to_string());
        return;
    };
    // The alias: MPJ derives it from the entity class; the four/five-argument
    // forms may override it with a string.
    let mut index = 1usize;
    let mut alias = ctx.columns.alias_of(&class);
    if let Some(explicit) = args.get(1).and_then(|a| string_literal(a)) {
        alias = explicit;
        index = 2;
    }
    ctx.join_aliases.push((class.clone(), alias.clone()));

    let rest = &args[index..];
    let mut on: Vec<CondNode> = Vec::new();
    let is_lambda = |arg: &str| arg.contains("->");

    // Sub-table consumer: `leftJoin(C, t -> t.setAlias("tt").selectAll().ge(…),
    // left, right)` renders the joined relation as a sub-select —
    // `LEFT JOIN (SELECT … FROM sub tt WHERE …) tt ON tt.left = main.right`.
    // The reference builds it with a nested `QueryWrapperParser` over the child
    // entity's `TableInfo` and takes the sub-alias from the lambda's `setAlias`
    // (the class decapitalised when it names none).
    if rest.len() == 3 && is_lambda(rest.first().map(String::as_str).unwrap_or("")) {
        let body = strip_lambda_prefix(&rest[0]);
        // The sub-alias: `setAlias("tt")` inside the lambda, else the default.
        let sub_alias = extract_string_call(&body, "setAlias").unwrap_or(alias.clone());
        // The sub-select's own text: the lambda body with `setAlias(…)` removed
        // (the alias moves into the FROM clause the sub-select declares).
        let inner = remove_call(&body, "setAlias");
        // The sub-select is an MPJ statement in its own right, rendered under
        // the sub-alias so its columns and its conditions agree on one name.
        let sub = {
            let joined = JoinWrappers::rehead(&inner, &entity.class_name);
            render_with_alias(
                &joined,
                &entity,
                ctx.dialect,
                &Default::default(),
                Some(&sub_alias),
            )
        };
        if sub.confidence == Confidence::Unresolved {
            ctx.unresolved = true;
            push_key(ctx, "wrapper.structural_method");
            ctx.notes.push(format!(
                "the `.{}` sub-table lambda could not be rendered as a sub-select; no SQL is emitted",
                name
            ));
            ctx.partial = false;
            chain.unsupported.push(name.to_string());
            return;
        }
        let sub_sql = sub.sql.unwrap_or_default();
        // The ON columns: the joined side is the sub-select, so its column takes
        // the SUB alias (`tt`), not the class-decapsulated one — the reference
        // prefixes the left column with `alias`, which for this form is the
        // sub-alias it just declared.
        let left = resolve_qualified(&rest[1], ctx)
            .map(|(_, column)| (sub_alias.clone(), column))
            .or_else(|| string_literal(&rest[1]).map(|column| (sub_alias.clone(), column)));
        let right = resolve_qualified(&rest[2], ctx)
            .or_else(|| string_literal(&rest[2]).map(|column| (ctx.main_alias.clone(), column)));
        let (Some((left_alias, left_col)), Some((right_alias, right_col))) = (left, right) else {
            ctx.unresolved = true;
            push_key(ctx, "wrapper.unknown_field");
            ctx.notes.push(format!(
                "cannot map the `{name}` ON arguments `{}` / `{}` to columns",
                rest[1], rest[2]
            ));
            ctx.partial = false;
            chain.unsupported.push(name.to_string());
            return;
        };
        chain.joins.push(Join {
            kind,
            table: String::new(),
            alias: sub_alias,
            on: vec![CondNode::Leaf(Condition {
                connector: Connector::And,
                op: Op::Eq,
                column: format!("{left_alias}.{left_col}"),
                values: vec![Value::Column(format!("{right_alias}.{right_col}"))],
            })],
            raw: None,
            sub_select: Some(sub_sql),
        });
        return;
    }
    // Lambda ON: `leftJoin(C, on -> on.eq(a, b))` — one lambda builds the whole
    // ON group.
    if rest.len() == 1 && is_lambda(&rest[0]) {
        let body = strip_lambda_prefix(&rest[0]);
        on = parse_on_lambda(&body, ctx, chain);
    } else if rest.len() >= 2 {
        // Column-pair forms. `rest` may still carry a right-side alias string
        // (the five-argument shape `leftJoin(C, "st", left, "t", right)`).
        let mut pieces = rest.to_vec();
        let right_alias_override = if pieces.len() == 3 {
            pieces
                .get(1)
                .and_then(|a| string_literal(a))
                .map(|explicit| {
                    let raw = pieces.remove(1);
                    (raw, explicit)
                })
        } else {
            None
        };
        let left_raw = pieces.first().cloned();
        let right_raw = pieces.get(1).cloned();
        let (Some(left_raw), Some(right_raw)) = (left_raw, right_raw) else {
            structural(
                ctx,
                format!("`.{name}` needs a joined column and a main column"),
            );
            chain.unsupported.push(name.to_string());
            return;
        };
        let left = resolve_qualified(&left_raw, ctx)
            .or_else(|| string_literal(&left_raw).map(|column| (alias.clone(), column)));
        let right = resolve_qualified(&right_raw, ctx)
            .or_else(|| string_literal(&right_raw).map(|column| (ctx.main_alias.clone(), column)));
        let (Some((left_alias, left_col)), Some((right_alias, right_col))) = (left, right) else {
            ctx.unresolved = true;
            push_key(ctx, "wrapper.unknown_field");
            ctx.notes.push(format!(
                "cannot map the `{name}` ON arguments `{left_raw}` / `{right_raw}` to columns"
            ));
            ctx.partial = false;
            chain.unsupported.push(name.to_string());
            return;
        };
        let (right_alias, right_col) = match right_alias_override {
            // The five-argument form names the right column's table alias
            // explicitly (`"t"`); the column itself was already resolved above.
            Some((_, explicit)) => (explicit, right_col),
            None => (right_alias, right_col),
        };
        on.push(CondNode::Leaf(Condition {
            connector: Connector::And,
            op: Op::Eq,
            column: format!("{left_alias}.{left_col}"),
            values: vec![Value::Column(format!("{right_alias}.{right_col}"))],
        }));
        // A trailing `ext -> …` lambda appends its conditions to the ON group.
        if pieces.len() >= 3 && is_lambda(&pieces[2]) {
            let body = strip_lambda_prefix(&pieces[2]);
            on.extend(parse_on_lambda(&body, ctx, chain));
        }
    } else {
        structural(
            ctx,
            format!("`.{name}` argument shape is not one this renderer reads"),
        );
        chain.unsupported.push(name.to_string());
        return;
    }

    chain.joins.push(Join {
        kind,
        table: entity.table.clone(),
        alias,
        on,
        raw: None,
        sub_select: None,
    });
}

/// Parses an ON lambda (`on -> on.eq(a, b).or().eq(c, d)`) into condition
/// nodes. Column-to-column comparisons are the point of an ON clause, so the
/// parser runs with `on_mode` on: the second argument of a comparison resolves
/// as a column instead of a runtime value.
fn parse_on_lambda(body: &str, ctx: &mut Ctx, chain: &mut WrapperChain) -> Vec<CondNode> {
    let calls = extract_calls(body);
    if calls.is_empty() {
        ctx.unresolved = true;
        push_key(ctx, "wrapper.structural_method");
        ctx.notes.push(
            "the ON lambda could not be parsed into conditions; no SQL is emitted".to_string(),
        );
        ctx.partial = false;
        return Vec::new();
    }
    let was = ctx.on_mode;
    ctx.on_mode = true;
    let mut pending = Connector::And;
    let nodes = parse_calls(&calls, &mut pending, chain, ctx);
    ctx.on_mode = was;
    nodes
}

/// `selectAll()` / `selectAll(X.class)` name the tables whose columns the
/// statement returns; `selectIgnore` drops named columns from that expansion.
fn parse_select(name: &str, args: &[String], ctx: &mut Ctx, chain: &mut WrapperChain) {
    match name {
        "selectAll" => {
            // The reference expands the MAIN table's columns for every
            // `selectAll` call — `parseSelectAll` uses `table.allColumnNames`
            // and `mainAlias` regardless of the class argument — and treats the
            // second string argument as a column-name prefix. Same here: the
            // class argument is accepted and ignored, so `selectAll(Main.class)`
            // and `selectAll(Other.class)` both mean "all main columns".
            if let Some(prefix) = args.get(1).and_then(|a| string_literal(a)) {
                chain.select_prefix = Some(prefix);
            }
            chain.select_tables.push(String::new());
        }
        "selectIgnore" => {
            for arg in args {
                if let Some(column) = string_literal(arg) {
                    chain.select_ignored.push(column);
                }
            }
        }
        "select" | "selectAs" => {
            for (index, arg) in args.iter().enumerate() {
                if let Some(literal) = string_literal(arg) {
                    if name == "selectAs" {
                        if let Some(as_name) = next_as(args, index + 1) {
                            chain.select_raw.push(format!("{literal} AS {as_name}"));
                        }
                        break;
                    }
                    chain.select_raw.push(literal);
                    continue;
                }
                let resolved = if ctx.mpj {
                    resolve_qualified(arg, ctx)
                } else {
                    // A single-table chain renders its select columns bare: the
                    // statement has one table, so no alias is needed (and the
                    // reference does not add one for a plain `QueryWrapper`).
                    let (_, getter) = match parse_field_ref(arg) {
                        Some(pair) => pair,
                        None => {
                            ctx.unresolved = true;
                            push_key(ctx, "wrapper.unknown_field");
                            ctx.notes
                                .push(format!("cannot map select argument `{arg}` to a column"));
                            ctx.partial = false;
                            continue;
                        }
                    };
                    ctx.columns
                        .column_for(&field_from_getter(&getter))
                        .map(|column| (String::new(), column.to_string()))
                        .or_else(|| {
                            ctx.unresolved = true;
                            push_key(ctx, "wrapper.unknown_field");
                            ctx.notes
                                .push(format!("cannot map select argument `{arg}` to a column"));
                            ctx.partial = false;
                            None
                        })
                };
                let Some((alias, column)) = resolved else {
                    continue;
                };
                let as_name = if name == "selectAs" {
                    next_as(args, index + 1)
                } else {
                    None
                };
                chain.select.push(SelectColumn {
                    alias,
                    column,
                    as_name,
                    function: None,
                });
            }
        }
        function => {
            // `selectCount(col[, as])` and the Sum/Max/Min/Avg family: the
            // aggregate wraps the qualified column, the `AS` name comes from
            // the second argument when one is given.
            let keyword = match function {
                "selectCount" => "COUNT",
                "selectSum" => "SUM",
                "selectMax" => "MAX",
                "selectMin" => "MIN",
                _ => "AVG",
            };
            let Some(first) = args.first() else {
                structural(ctx, format!("`.{function}` needs a column"));
                chain.unsupported.push(function.to_string());
                return;
            };
            let resolved = if ctx.mpj {
                resolve_qualified(first, ctx)
            } else {
                parse_field_ref(first).and_then(|(_, getter)| {
                    ctx.columns
                        .column_for(&field_from_getter(&getter))
                        .map(|column| (String::new(), column.to_string()))
                })
            };
            let Some((alias, column)) = resolved else {
                ctx.unresolved = true;
                push_key(ctx, "wrapper.unknown_field");
                ctx.notes.push(format!("cannot map `{first}` to a column"));
                ctx.partial = false;
                return;
            };
            chain.select.push(SelectColumn {
                alias,
                column,
                as_name: next_as(args, 1),
                function: Some(keyword),
            });
        }
    }
}

/// The first string argument of a call in `text` (`setAlias("tt")` → `tt`).
fn extract_string_call(text: &str, method: &str) -> Option<String> {
    let at = text.find(&format!("{method}("))?;
    let rest = &text[at + method.len() + 1..];
    let close = rest.find(')')?;
    string_literal(&rest[..close])
}

/// Drops one whole call (`setAlias("tt")`) from a chain body, including its
/// argument parentheses, leaving the surrounding chain intact.
fn remove_call(text: &str, method: &str) -> String {
    let needle = format!("{method}(");
    let Some(at) = text.find(&needle) else {
        return text.to_string();
    };
    let after = &text[at + needle.len()..];
    let Some(close) = after.find(')') else {
        return text.to_string();
    };
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..at]);
    out.push_str(&after[close + 1..]);
    out
}

/// Gives a chain body an MPJ head so it is recognised as a join chain: a
/// sub-table lambda's body (`selectAll().ge(…)`) carries no factory of its own.
struct JoinWrappers;

impl JoinWrappers {
    fn rehead(body: &str, class: &str) -> String {
        format!(
            "JoinWrappers.lambda({class}.class).{}",
            body.trim_start_matches('.')
        )
    }
}

/// The `AS` name for a select argument: a string literal is the name itself, a
/// `VO::getProperty` reference contributes the property (`getAge` → `age`),
/// which is what MPJ aliases to for result mapping.
fn next_as(args: &[String], index: usize) -> Option<String> {
    let arg = args.get(index)?;
    if let Some(literal) = string_literal(arg) {
        return Some(literal);
    }
    let (_, getter) = parse_field_ref(arg)?;
    Some(field_from_getter(&getter))
}

/// `in(User::getId, AddressDO.class, sub -> sub.select(...).eq(...))` →
/// `t.id IN (SELECT user_id FROM user_address WHERE status = 1)`. The lambda is
/// rendered as its own statement against the joined entity's projection — the
/// reference builds the same shape and explicitly does *not* inherit the main
/// table's alias prefix.
///
/// A sub-select that itself degrades (a runtime parameter inside it) refuses
/// the whole render: its `?` would have to splice into the middle of the main
/// statement's parameter order, which the text alone cannot line up.
fn parse_in_subquery(
    name: &str,
    args: &[String],
    pending: &mut Connector,
    chain: &mut WrapperChain,
    ctx: &mut Ctx,
    nodes: &mut Vec<CondNode>,
) {
    let Some(column) = resolve_column(&args[0], ctx) else {
        ctx.unresolved = true;
        push_key(ctx, "wrapper.unknown_field");
        ctx.partial = false;
        return;
    };
    let class = class_literal(&args[1]).expect("dispatch checked");
    let Some(sub) = subquery_sql(&class, &args[2], ctx) else {
        ctx.unresolved = true;
        push_key(ctx, "wrapper.structural_method");
        ctx.notes.push(format!(
            "`.{name}(col, Class, lambda)` could not be rendered as a literal sub-select; no SQL is emitted"
        ));
        ctx.partial = false;
        chain.unsupported.push(name.to_string());
        return;
    };
    let op = if name == "in" { Op::In } else { Op::NotIn };
    nodes.push(CondNode::Leaf(Condition {
        connector: *pending,
        op,
        column,
        values: vec![Value::SubQuery(sub)],
    }));
    *pending = Connector::And;
}

/// `exists(AddressDO.class, sub -> …)` → `EXISTS (SELECT …)`.
fn parse_exists_subquery(
    name: &str,
    args: &[String],
    pending: &mut Connector,
    chain: &mut WrapperChain,
    ctx: &mut Ctx,
    nodes: &mut Vec<CondNode>,
) {
    let class = class_literal(&args[0]).expect("dispatch checked");
    let Some(sub) = subquery_sql(&class, &args[1], ctx) else {
        ctx.unresolved = true;
        push_key(ctx, "wrapper.structural_method");
        ctx.notes.push(format!(
            "`.{name}(Class, lambda)` could not be rendered as a literal sub-select; no SQL is emitted"
        ));
        ctx.partial = false;
        chain.unsupported.push(name.to_string());
        return;
    };
    let op = if name == "exists" {
        Op::Exists
    } else {
        Op::NotExists
    };
    nodes.push(CondNode::Leaf(Condition {
        connector: *pending,
        op,
        column: String::new(),
        values: vec![Value::SubQuery(sub)],
    }));
    *pending = Connector::And;
}

/// The lambda's body rendered as a standalone SELECT, single-line. `None` when
/// the class has no projection or the sub-select is not fully literal.
fn subquery_sql(class: &str, lambda: &str, ctx: &Ctx) -> Option<String> {
    let entity = ctx.columns.find(class)?;
    let body = strip_lambda_prefix(lambda);
    let out = render_with(&body, entity, ctx.dialect, &Default::default());
    if out.confidence != Confidence::Exact {
        return None;
    }
    out.sql
}

/// `having("count(*) > {0}", 0)`: the fragment renders verbatim with `{i}`
/// replaced by the argument values; runtime expressions become `?` and join
/// the parameter order after the conditions.
fn parse_having(args: &[String], ctx: &mut Ctx, chain: &mut WrapperChain) {
    let Some(template) = args.first().and_then(|a| string_literal(a)) else {
        structural(ctx, "`having` needs a string fragment".to_string());
        chain.unsupported.push("having".to_string());
        return;
    };
    ctx.partial = true;
    let mut out = template;
    let mut params: Vec<Value> = Vec::new();
    for (index, arg) in args.iter().enumerate().skip(1) {
        let rendered = match parse_value_in(arg, ctx) {
            Value::Param(source) => {
                params.push(Value::Param(source));
                "?".to_string()
            }
            value => render_value(&value, ctx.dialect),
        };
        out = out.replace(&format!("{{{}}}", index - 1), &rendered);
    }
    chain.having = Some(out);
    chain.having_params = params;
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

    let values: Vec<Value> = args
        .iter()
        .skip(1)
        .map(|a| parse_value_in(a, ctx))
        .collect();
    for v in &values {
        if v.is_param() {
            ctx.partial = true;
            push_key(ctx, "wrapper.param_value");
        }
    }

    match op {
        Op::Between if values.len() != 2 => {
            ctx.partial = true;
            push_key(ctx, "wrapper.between_arity");
            ctx.notes
                .push("between needs exactly two values".to_string());
        }
        Op::In | Op::NotIn if values.is_empty() => {
            ctx.partial = true;
            push_key(ctx, "wrapper.empty_in");
            ctx.notes
                .push("in/notIn with no values renders `IN (NULL)`".to_string());
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

/// `parse_value`, plus the ON-lambda rule: inside an ON condition the second
/// argument of a comparison is itself a column (`on.eq(AreaDO::getId,
/// AddressDO::getAreaId)`), rendered as the qualified column — never as a `?`.
fn parse_value_in(raw: &str, ctx: &mut Ctx) -> Value {
    if ctx.on_mode
        && let Some((alias, column)) = resolve_qualified(raw, ctx)
    {
        return Value::Column(format!("{alias}.{column}"));
    }
    parse_value(raw)
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
    Value::Param(t.to_string())
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
        let field = field_from_getter(&getter);
        if ctx.mpj {
            // MPJ condition columns take the MAIN alias unconditionally — the
            // reference's `QueryWrapperParser` prefixes every condition column
            // with `columnPrefix` (the main alias `t`) whatever entity the
            // receiver names. The column name itself must still resolve in the
            // main entity or one of the joined ones; a receiver that is neither
            // is a real error, not a degradation.
            let Some(entity) = ctx.columns.find(&recv) else {
                ctx.unresolved = true;
                push_key(ctx, "wrapper.join_table_unknown");
                ctx.notes.push(format!(
                    "the column's entity `{recv}` is neither the chain's entity nor a joined table"
                ));
                ctx.partial = false;
                return None;
            };
            let column = entity.column_for(&field)?;
            // Inside an ON lambda the column takes its OWN entity's alias (the
            // reference's A47 rule — `ON t.user_id = t.id` is the very defect
            // A47 fixed); everywhere else the reference prefixes condition
            // columns with the main alias unconditionally.
            let alias = if ctx.on_mode {
                alias_for_class(ctx, &recv)
            } else {
                ctx.main_alias.clone()
            };
            return Some(format!("{alias}.{column}"));
        }
        if !ctx.columns.class_name.is_empty() && recv != ctx.columns.class_name {
            ctx.partial = true;
            push_key(ctx, "wrapper.receiver_mismatch");
            ctx.notes.push(format!(
                "receiver `{recv}` differs from entity `{}`",
                ctx.columns.class_name
            ));
        }
        ctx.columns.column_for(&field).map(str::to_string)
    } else if ctx.mpj {
        // The string forms of `MPJQueryWrapper` / `JoinWrappers.query`: a
        // fragment that already names its table (`"t.id"`) passes through; a
        // bare column (`"status"`) takes the main alias, the same prefix the
        // reference applies to condition columns.
        let literal = string_literal(raw)?;
        if literal.contains('.') {
            Some(literal)
        } else {
            Some(format!("{}.{literal}", ctx.main_alias))
        }
    } else {
        // QueryWrapper passes the column name directly.
        string_literal(raw)
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

/// A possibly table-qualified column (`t.id` / `addressDO.user_id`): the alias
/// renders bare (it is generated here, never a keyword), the column is quoted
/// by the dialect rules — quoting the whole dotted string would produce
/// `` `t.id` ``, which is a different identifier.
fn render_column_name(name: &str, dialect: &Dialect) -> String {
    match name.split_once('.') {
        Some((alias, column)) => format!("{alias}.{}", quote(column, dialect)),
        None => quote(name, dialect),
    }
}

fn render_select(chain: &WrapperChain, columns: &EntityColumns, dialect: &Dialect) -> String {
    let mut sql = match chain.table.as_deref() {
        Some(t) if chain.main_alias.is_some() => format!(
            "SELECT {} FROM {} {}",
            render_select_list(chain, columns, dialect),
            quote(t, dialect),
            chain.main_alias.as_deref().unwrap_or("t"),
        ),
        // The SELECT list is `*`: the wrapper does not carry a column list, and
        // the mapper's `<select>` supplies it (`SELECT *` is the neutral case).
        Some(t) => format!(
            "SELECT {} FROM {}",
            render_single_table_list(chain, dialect),
            quote(t, dialect)
        ),
        None => format!("SELECT {}", render_single_table_list(chain, dialect)),
    };
    for join in &chain.joins {
        sql.push(' ');
        sql.push_str(&render_join(join, dialect));
    }
    if let Some(where_sql) = render_where(chain, dialect) {
        sql.push(' ');
        sql.push_str(&where_sql);
    }
    sql.push_str(&render_clauses(chain, dialect));
    sql
}

/// The single-table SELECT list: the chain's own columns when it named any,
/// `*` otherwise.
fn render_single_table_list(chain: &WrapperChain, dialect: &Dialect) -> String {
    if chain.select.is_empty() && chain.select_raw.is_empty() {
        return "*".to_string();
    }
    let mut items: Vec<String> = chain
        .select
        .iter()
        .map(|column| render_select_column(column, dialect))
        .collect();
    items.extend(chain.select_raw.iter().cloned());
    items.join(", ")
}

/// The MPJ SELECT list: every `selectAll` table expanded to its columns
/// (minus `selectIgnore`), then the explicit columns and fragments. The
/// reference builds the same list from `selectAllCalls` + `base.selectColumns`
/// + `selectAsColumns`; with nothing named at all it expands the main table.
fn render_select_list(chain: &WrapperChain, columns: &EntityColumns, dialect: &Dialect) -> String {
    let mut items: Vec<String> = Vec::new();
    for table in &chain.select_tables {
        let Some((entity, alias)) = expand_select_table(table, chain, columns) else {
            continue;
        };
        let prefix = chain.select_prefix.as_deref().unwrap_or("");
        for (_, column, _) in &entity.columns {
            if chain.select_ignored.iter().any(|ignored| ignored == column) {
                continue;
            }
            items.push(render_column_name(
                &format!("{alias}.{prefix}{column}"),
                dialect,
            ));
        }
    }
    items.extend(
        chain
            .select
            .iter()
            .map(|column| render_select_column(column, dialect)),
    );
    items.extend(chain.select_raw.iter().cloned());
    if items.is_empty() {
        // No select call at all: MPJ still returns the main table's columns,
        // qualified by its alias.
        let alias = chain.main_alias.as_deref().unwrap_or("t");
        for (_, column, _) in &columns.columns {
            if chain.select_ignored.iter().any(|ignored| ignored == column) {
                continue;
            }
            items.push(render_column_name(&format!("{alias}.{column}"), dialect));
        }
    }
    items.join(", ")
}

/// One `selectAll` entry → the entity and the alias its columns render under.
fn expand_select_table<'a>(
    table: &str,
    chain: &WrapperChain,
    columns: &'a EntityColumns,
) -> Option<(&'a EntityColumns, String)> {
    if table.is_empty() {
        return Some((
            columns,
            chain.main_alias.as_deref().unwrap_or("t").to_string(),
        ));
    }
    let entity = columns.find(table)?;
    let alias = if entity.class_name == columns.class_name {
        chain.main_alias.as_deref().unwrap_or("t").to_string()
    } else {
        chain
            .joins
            .iter()
            .find(|join| join.table == entity.table)
            .map(|join| join.alias.clone())
            .unwrap_or_else(|| columns.alias_of(table))
    };
    Some((entity, alias))
}

fn render_select_column(column: &SelectColumn, dialect: &Dialect) -> String {
    let qualified = if column.alias.is_empty() {
        render_column_name(&column.column, dialect)
    } else {
        render_column_name(&format!("{}.{}", column.alias, column.column), dialect)
    };
    let expr = match column.function {
        Some(function) => format!("{function}({qualified})"),
        None => qualified,
    };
    match &column.as_name {
        Some(as_name) => format!("{expr} AS {as_name}"),
        None => expr,
    }
}

/// One join clause: `LEFT JOIN <table> <alias> ON <conditions>`, or the raw
/// fragment form passed through verbatim.
fn render_join(join: &Join, dialect: &Dialect) -> String {
    if let Some(raw) = &join.raw {
        return format!("{} {raw}", join.kind.sql());
    }
    let on = if join.on.is_empty() {
        String::new()
    } else {
        format!(" ON {}", render_nodes(&join.on, dialect))
    };
    let relation = match &join.sub_select {
        Some(sub) => format!("({sub})"),
        None => quote(&join.table, dialect),
    };
    format!("{} {} {}{}", join.kind.sql(), relation, join.alias, on)
}

fn render_fragment(chain: &WrapperChain, dialect: &Dialect) -> Option<String> {
    let where_part = render_where(chain, dialect)?;
    let mut sql = where_part;
    sql.push_str(&render_clauses(chain, dialect));
    Some(sql)
}

/// GROUP BY / HAVING / ORDER BY / LIMIT / LAST (the WHERE clause belongs to
/// the caller); the clause order is the standard SQL one.
fn render_clauses(chain: &WrapperChain, dialect: &Dialect) -> String {
    let mut out = String::new();
    if !chain.group_by.is_empty() {
        let items: Vec<String> = chain
            .group_by
            .iter()
            .map(|column| render_column_name(column, dialect))
            .collect();
        out.push_str(" GROUP BY ");
        out.push_str(&items.join(", "));
    }
    if let Some(having) = &chain.having {
        out.push_str(" HAVING ");
        out.push_str(having);
    }
    if !chain.order_by.is_empty() {
        let items: Vec<String> = chain
            .order_by
            .iter()
            .map(|o| {
                format!(
                    "{} {}",
                    render_column_name(&o.column, dialect),
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
    if matches!(c.op, Op::Exists | Op::NotExists) {
        let keyword = if c.op == Op::Exists {
            "EXISTS"
        } else {
            "NOT EXISTS"
        };
        let Some(Value::SubQuery(sql)) = c.values.first() else {
            return String::new();
        };
        return format!("{keyword} ({sql})");
    }
    let col = render_column_name(&c.column, dialect);
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
        Value::Param(_) => "?".to_string(),
        // A column never appears as a LIKE pattern; keep the match total.
        Value::Column(_) => String::new(),
        // Neither does a sub-select.
        Value::SubQuery(_) => String::new(),
    }
}

fn render_value(v: &Value, dialect: &Dialect) -> String {
    match v {
        Value::Column(name) => render_column_name(name, dialect),
        Value::SubQuery(sql) => sql.clone(),
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
        Value::Param(_) => "?".to_string(),
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

    // MPJ: joins, select lists and sub-queries (demo-repo parity) --------

    /// The demo repo's `mybatis-plus-join-demo` entities, reduced to the fields
    /// the parity chains below reference. Table names are the `@TableName`
    /// values the demo declares (`car_device_info` is the main table of the
    /// chain this work started from).
    fn device_cols() -> EntityColumns {
        let device = EntityColumns::new("DeviceInfoEntity", "car_device_info")
            .with_column("id", "id", "int")
            .with_column("stationId", "station_id", "int")
            .with_column("deviceName", "device_name", "varchar");
        let station = EntityColumns::new("StationInfoEntity", "station_info")
            .with_column("stationId", "station_id", "int")
            .with_column("stationName", "station_name", "varchar")
            .with_column("merchantId", "merchant_id", "int");
        let driver = EntityColumns::new("DriverInfoEntity", "driver_info")
            .with_column("carId", "car_id", "int");
        device.with_related(station).with_related(driver)
    }

    /// The A47 parity fixture from the reference's own converter test:
    /// `UserDO` + `AddressDO` + `AreaDO`, with the ON side of the second join
    /// belonging to the *first joined* table.
    fn user_cols() -> EntityColumns {
        let user = EntityColumns::new("UserDO", "user")
            .with_column("id", "id", "bigint")
            .with_column("name", "name", "varchar")
            .with_column("sex", "sex", "int")
            .with_column("age", "age", "int")
            .with_column("headImg", "head_img", "varchar");
        let address = EntityColumns::new("AddressDO", "user_address")
            .with_column("id", "id", "bigint")
            .with_column("userId", "user_id", "bigint")
            .with_column("areaId", "area_id", "bigint")
            .with_column("tel", "tel", "varchar")
            .with_column("status", "status", "int");
        let area = EntityColumns::new("AreaDO", "area")
            .with_column("id", "id", "bigint")
            .with_column("province", "province", "varchar")
            .with_column("city", "city", "varchar");
        let station = EntityColumns::new("StationInfoEntity", "station_info").with_column(
            "stationName",
            "station_name",
            "varchar",
        );
        let device = EntityColumns::new("DeviceInfoEntity", "car_device_info").with_column(
            "deviceName",
            "device_name",
            "varchar",
        );
        user.with_related(address)
            .with_related(area)
            .with_related(station)
            .with_related(device)
    }

    #[test]
    fn the_reported_mpj_chain_renders_its_joins_and_its_columns() {
        // The chain from the defect report: `selectAll` + two `leftJoin` + `eq`.
        // Each join names its own table, the conditions qualify by the main
        // alias `t`, and the SELECT expands the named entity's columns.
        let out = render(
            r#"JoinWrappers.lambda(DeviceInfoEntity.class)
                .selectAll(DeviceInfoEntity.class)
                .leftJoin(StationInfoEntity.class, StationInfoEntity::getStationId, DeviceInfoEntity::getStationId)
                .leftJoin(DriverInfoEntity.class, DriverInfoEntity::getCarId, DeviceInfoEntity::getId)
                .eq(DeviceInfoEntity::getId, 1)"#,
            &device_cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Exact, "{out:?}");
        let sql = out.sql.as_deref().expect("exact renders SQL");
        assert_eq!(
            sql,
            "SELECT t.id, t.station_id, t.device_name \
             FROM car_device_info t \
             LEFT JOIN station_info stationInfoEntity ON stationInfoEntity.station_id = t.station_id \
             LEFT JOIN driver_info driverInfoEntity ON driverInfoEntity.car_id = t.id \
             WHERE t.id = 1"
        );
    }

    #[test]
    fn on_sides_qualify_by_their_own_entity_a47() {
        // The reference fixed this as A47: the ON side of the second join
        // belongs to the first joined table, not to the main one. Ported from
        // `MpjJoinOnAliasConverterTest`.
        let out = render(
            r#"JoinWrappers.lambda(UserDO.class)
                .selectAll(UserDO.class)
                .leftJoin(AddressDO.class, AddressDO::getUserId, UserDO::getId)
                .rightJoin(AreaDO.class, AreaDO::getId, AddressDO::getAreaId)"#,
            &user_cols(),
            mysql(),
        );
        let sql = out.sql.expect("the chain renders");
        assert!(
            sql.contains("ON areaDO.id = addressDO.area_id"),
            "right side follows its own entity: {sql}"
        );
        assert!(
            sql.contains("ON addressDO.user_id = t.id"),
            "the main side keeps the main alias: {sql}"
        );
        assert!(!sql.contains("t.area_id"), "A47 regression: {sql}");
    }

    #[test]
    fn join_alias_and_right_alias_forms_follow_the_reference() {
        // Four-argument alias (`leftJoin(C, "st", left, right)`) and the
        // five-argument right-alias form from the demo's `joinRightAlias`.
        let out = render(
            r#"JoinWrappers.lambda(DeviceInfoEntity.class)
                .selectAll(DeviceInfoEntity.class)
                .leftJoin(StationInfoEntity.class, "st",
                    StationInfoEntity::getStationId, "t", DeviceInfoEntity::getId)
                .eq(DeviceInfoEntity::getId, 1)"#,
            &device_cols(),
            mysql(),
        );
        let sql = out.sql.expect("the chain renders");
        assert!(
            sql.contains("LEFT JOIN station_info st ON st.station_id = t.id"),
            "{sql}"
        );
    }

    #[test]
    fn lambda_on_and_ext_conditions_join_the_on_group() {
        // Demo `onLambdaApply`: lambda ON plus the three-argument `ext` form.
        let out = render(
            r#"JoinWrappers.lambda(UserDO.class)
                .selectAll(UserDO.class)
                .leftJoin(AddressDO.class, on -> on
                    .eq(AddressDO::getUserId, UserDO::getId)
                    .eq(AddressDO::getStatus, 1))
                .leftJoin(AreaDO.class, AreaDO::getId, AddressDO::getAreaId,
                    ext -> ext.eq(AreaDO::getProvince, "浙江省"))
                .eq(UserDO::getSex, 0)"#,
            &user_cols(),
            mysql(),
        );
        let sql = out.sql.expect("the chain renders");
        assert!(
            sql.contains(
                "LEFT JOIN user_address addressDO ON addressDO.user_id = t.id AND addressDO.status = 1"
            ),
            "{sql}"
        );
        assert!(
            sql.contains("LEFT JOIN area areaDO ON areaDO.id = addressDO.area_id AND areaDO.province = '浙江省'"),
            "{sql}"
        );
    }

    #[test]
    fn select_list_forms_render_the_named_columns() {
        // Demo `joinOneAndCount` / `aggregateExt` / `joinListEntity`.
        let out = render(
            r#"JoinWrappers.lambda(UserDO.class)
                .selectAll(UserDO.class)
                .select(AddressDO::getTel)
                .selectAs(StationInfoEntity::getStationName, DeviceInfoEntity::getDeviceName)
                .selectCount(UserDO::getId, "total")
                .leftJoin(AddressDO.class, AddressDO::getUserId, UserDO::getId)
                .eq(UserDO::getId, 1)"#,
            &user_cols(),
            mysql(),
        );
        let sql = out.sql.expect("the chain renders");
        // selectAll expansion first, then the explicit columns in call order.
        assert!(
            sql.starts_with("SELECT t.id, t.name, t.sex, t.age, t.head_img, "),
            "{sql}"
        );
        assert!(sql.contains("addressDO.tel, "), "{sql}");
        assert!(sql.contains(" AS deviceName"), "{sql}");
        assert!(sql.contains("COUNT(t.id) AS total"), "{sql}");
    }

    #[test]
    fn select_ignore_drops_named_columns_from_the_expansion() {
        // Demo `queryStringExt`'s `selectIgnore` (entity form here).
        let out = render(
            r#"JoinWrappers.lambda(UserDO.class)
                .selectAll(UserDO.class)
                .selectIgnore("head_img")
                .leftJoin(AddressDO.class, AddressDO::getUserId, UserDO::getId)
                .eq(UserDO::getId, 1)"#,
            &user_cols(),
            mysql(),
        );
        let sql = out.sql.expect("the chain renders");
        assert!(
            sql.contains("SELECT t.id, t.name, t.sex, t.age FROM"),
            "{sql}"
        );
        assert!(!sql.contains("head_img"), "{sql}");
    }

    #[test]
    fn group_by_and_having_render_between_where_and_order() {
        let out = render(
            r#"JoinWrappers.lambda(UserDO.class)
                .select(UserDO::getSex)
                .selectCount(UserDO::getId, "total")
                .leftJoin(AddressDO.class, AddressDO::getUserId, UserDO::getId)
                .groupBy(UserDO::getSex)
                .having("count(*) > {0}", 1)
                .eq(UserDO::getId, 1)
                .orderByDesc(UserDO::getSex)"#,
            &user_cols(),
            mysql(),
        );
        let sql = out.sql.expect("the chain renders");
        let group = sql.find("GROUP BY t.sex").expect("group by");
        let having = sql.find("HAVING count(*) > 1").expect("having");
        let order = sql.find("ORDER BY").expect("order by");
        let where_at = sql.find("WHERE").expect("where");
        assert!(
            where_at < group && group < having && having < order,
            "{sql}"
        );
    }

    #[test]
    fn subquery_conditions_render_as_wrapped_selects() {
        // Demo `subqueryConditions`, reduced to one `in` and one `exists`.
        let out = render(
            r#"JoinWrappers.lambda(UserDO.class)
                .selectAll(UserDO.class)
                .in(UserDO::getId, AddressDO.class, sub -> sub
                    .select(AddressDO::getUserId)
                    .eq(AddressDO::getStatus, 1))
                .exists(AreaDO.class, sub -> sub
                    .select(AreaDO::getId)
                    .eq(AreaDO::getProvince, "浙江省"))
                .eq(UserDO::getSex, 1)"#,
            &user_cols(),
            mysql(),
        );
        let sql = out.sql.expect("the chain renders");
        assert!(
            sql.contains("t.id IN (SELECT user_id FROM user_address WHERE status = 1)"),
            "{sql}"
        );
        assert!(
            sql.contains("EXISTS (SELECT id FROM area WHERE province = '浙江省')"),
            "{sql}"
        );
    }

    #[test]
    fn a_raw_join_fragment_passes_through_verbatim() {
        // Demo `queryStringExt`: `leftJoin("user_address addr on t.id = addr.user_id")`.
        let out = render(
            r#"JoinWrappers.query(UserDO.class)
                .selectAll(UserDO.class)
                .leftJoin("user_address addr on t.id = addr.user_id")
                .eq("t.id", 1)"#,
            &user_cols(),
            mysql(),
        );
        let sql = out.sql.expect("the chain renders");
        assert!(
            sql.contains("LEFT JOIN user_address addr on t.id = addr.user_id"),
            "{sql}"
        );
    }

    #[test]
    fn an_unresolvable_join_entity_refuses_the_render() {
        // `Car` is not in the projection: a guessed table name would be wrong
        // SQL, so the chain stays unresolved and names the class.
        let out = render(
            r#"JoinWrappers.lambda(DeviceInfoEntity.class)
                .selectAll(DeviceInfoEntity.class)
                .leftJoin(Car.class, Car::getDeviceId, DeviceInfoEntity::getId)
                .eq(DeviceInfoEntity::getId, 1)"#,
            &device_cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Unresolved);
        assert_eq!(out.sql, None, "{:?}", out.sql);
        assert!(
            out.message_keys
                .contains(&"wrapper.join_table_unknown".to_string()),
            "{:?}",
            out.message_keys
        );
    }

    #[test]
    fn a_sub_table_consumer_join_renders_a_sub_select() {
        // Demo `joinExtAndSubTable`'s second join: the joined relation is a
        // sub-select, with the sub-alias from the lambda's `setAlias` and the
        // ON columns following A47. The reference renders
        // `LEFT JOIN (SELECT … FROM sub tt WHERE …) tt ON tt.left = main.right`.
        let out = render(
            r#"JoinWrappers.lambda(DeviceInfoEntity.class)
                .selectAll(DeviceInfoEntity.class)
                .leftJoin(StationInfoEntity.class, t -> t
                    .setAlias("tt")
                    .selectAll()
                    .ge(StationInfoEntity::getMerchantId, 0),
                    StationInfoEntity::getStationId, DeviceInfoEntity::getStationId)"#,
            &device_cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Exact, "{out:?}");
        let sql = out.sql.expect("the chain renders");
        // The sub-select carries its own FROM under the sub-alias.
        assert!(
            sql.contains("LEFT JOIN (SELECT tt.station_id, tt.station_name, tt.merchant_id FROM station_info tt WHERE tt.merchant_id >= 0) tt"),
            "{sql}"
        );
        // The ON sides: joined column on the left, main column on the right.
        assert!(sql.contains("ON tt.station_id = t.station_id"), "{sql}");
        // And the outer FROM keeps the main table.
        assert!(sql.contains("FROM car_device_info t"), "{sql}");
    }

    #[test]
    fn a_sub_table_consumer_without_set_alias_uses_the_class_alias() {
        // The reference's fallback: no `setAlias` in the lambda → the child
        // class decapitalised.
        let out = render(
            r#"JoinWrappers.lambda(DeviceInfoEntity.class)
                .selectAll(DeviceInfoEntity.class)
                .leftJoin(StationInfoEntity.class, t -> t
                    .selectAll()
                    .ge(StationInfoEntity::getMerchantId, 0),
                    StationInfoEntity::getStationId, DeviceInfoEntity::getStationId)"#,
            &device_cols(),
            mysql(),
        );
        let sql = out.sql.expect("the chain renders");
        assert!(
            sql.contains("LEFT JOIN (SELECT stationInfoEntity.station_id"),
            "{sql}"
        );
        assert!(sql.contains(") stationInfoEntity ON"), "{sql}");
    }

    #[test]
    fn a_column_whose_entity_never_joined_is_unresolved() {
        // `eq(AddressDO::getStatus, …)` on a chain that never joined AddressDO:
        // there is no table those columns belong to.
        let out = render(
            r#"JoinWrappers.lambda(UserDO.class)
                .selectAll(UserDO.class)
                .eq(Car::getStatus, 1)"#,
            &user_cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Unresolved);
        assert_eq!(out.sql, None);
        assert!(
            out.message_keys
                .contains(&"wrapper.join_table_unknown".to_string()),
            "{:?}",
            out.message_keys
        );
    }

    #[test]
    fn condition_columns_take_the_main_alias_even_for_joined_entities() {
        // The reference's `QueryWrapperParser` prefixes every condition column
        // with `columnPrefix` (the main alias) whatever entity the receiver
        // names — unlike the ON sides, which follow A47. Demo `updateJoinExt`
        // has the shape: `.innerJoin(AddressDO.class, …).eq(AddressDO::getStatus, 1)`.
        let out = render(
            r#"JoinWrappers.lambda(UserDO.class)
                .selectAll(UserDO.class)
                .leftJoin(AddressDO.class, AddressDO::getUserId, UserDO::getId)
                .eq(AddressDO::getStatus, 1)
                .orderByAsc(AddressDO::getTel)"#,
            &user_cols(),
            mysql(),
        );
        let sql = out.sql.expect("the chain renders");
        assert!(sql.contains("WHERE t.status = 1"), "{sql}");
        assert!(sql.contains("ORDER BY t.tel ASC"), "{sql}");
        // The ON sides still follow their own entities.
        assert!(sql.contains("ON addressDO.user_id = t.id"), "{sql}");
    }

    #[test]
    fn select_all_expands_the_main_table_regardless_of_the_class_argument() {
        // The reference's `parseSelectAll` uses `table.allColumnNames` and
        // `mainAlias` whatever entity the call names — `selectAll(Other.class)`
        // still lists the main table's columns. Ported from
        // `MpjLambdaConverter.parseSelectAll`.
        let out = render(
            r#"JoinWrappers.lambda(UserDO.class)
                .selectAll(AddressDO.class)
                .leftJoin(AddressDO.class, AddressDO::getUserId, UserDO::getId)
                .eq(UserDO::getId, 1)"#,
            &user_cols(),
            mysql(),
        );
        let sql = out.sql.expect("the chain renders");
        assert!(
            sql.starts_with("SELECT t.id, t.name, t.sex, t.age, t.head_img FROM"),
            "{sql}"
        );
        // And the prefix form (`selectAll(X.class, "p")` → `p + column`).
        let out = render(
            r#"JoinWrappers.lambda(UserDO.class)
                .selectAll(UserDO.class, "u.")
                .leftJoin(AddressDO.class, AddressDO::getUserId, UserDO::getId)
                .eq(UserDO::getId, 1)"#,
            &user_cols(),
            mysql(),
        );
        let sql = out.sql.expect("the chain renders");
        assert!(sql.contains("SELECT t.u.id, t.u.name,"), "{sql}");
    }

    #[test]
    fn a_custom_keyword_join_is_structural_not_partial() {
        // `join("FULL JOIN", …)` cannot be dropped from the FROM clause: a
        // single-table statement would answer a different question, so the
        // chain stays unresolved rather than partial-with-SQL.
        let out = render(
            r#"JoinWrappers.lambda(UserDO.class)
                .selectAll(UserDO.class)
                .join("FULL JOIN", "user_address addr on t.id = addr.user_id")
                .eq(UserDO::getId, 1)"#,
            &user_cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Unresolved);
        assert_eq!(out.sql, None);
    }

    #[test]
    fn condition_level_unsupported_methods_stay_partial() {
        // The line the structural rule must not cross: `.apply(...)` narrows a
        // filter, it does not change the row shape, so the statement is still
        // the reader's query minus one condition.
        for method in ["apply", "having"] {
            let text =
                format!(r#"new LambdaQueryWrapper<User>().{method}("x").eq(User::getId, 1)"#);
            let out = render(&text, &cols(), mysql());
            assert_eq!(
                out.confidence,
                Confidence::Partial,
                "{method} narrows a filter; it does not lose structure"
            );
            assert!(out.sql.is_some(), "{method}");
        }
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
