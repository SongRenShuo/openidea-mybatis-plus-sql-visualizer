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
    /// The column `@TableId` names, when the source declared it. The join
    /// update's `setUpdateEntity` expands every non-id column, the way the
    /// reference filters `!it.isId`; `None` expands them all (the honest
    /// approximation when the source never said which column is the id).
    pub primary_key: Option<String>,
}

impl EntityColumns {
    pub fn new(class_name: &str, table: &str) -> Self {
        EntityColumns {
            class_name: class_name.to_string(),
            table: table.to_string(),
            columns: Vec::new(),
            related: Vec::new(),
            primary_key: None,
        }
    }

    /// Names the entity's id column (builder style).
    pub fn with_primary_key(mut self, column: &str) -> Self {
        self.primary_key = Some(column.to_string());
        self
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
    /// A `JoinWrappers.delete(C)` chain: the multi-table DELETE the reference's
    /// `DeleteJoinConverter` builds (`DELETE t, t1 FROM user t … WHERE …`).
    Delete,
}

/// One SET assignment of a join-update chain, in call order. The reference's
/// `UpdateJoinConverter.parseSet` produces the same two shapes: an explicit
/// `column = value` pair or a raw fragment it already rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetItem {
    /// `set(col, val)` / `setIfExists(col, val)`: the qualified column (the
    /// main alias prefix, or a caller-written `t1.tel` verbatim) and its value.
    Assign { column: String, value: Value },
    /// `setSql`/`setApply`/`setIncrBy`/`setDecrBy`: a fully rendered fragment.
    Raw(String),
}

/// One entry of the multi-table DELETE's target list, resolved against the
/// joins at render time (the reference resolves `deleteAll()` the same way).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteTarget {
    /// The main table's alias.
    Main,
    /// `delete(X.class)`: the class's own default alias — the main entity
    /// resolves to the main alias, a joined one to its decapitalised class
    /// name (the reference ignores an explicit join alias here, on purpose).
    Class(String),
    /// `delete("t1")`: the caller's string, verbatim.
    Raw(String),
    /// `deleteAll()`: the main alias plus every join alias.
    All,
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
    /// already single-line, with the parameter sources its own `?`s stand for,
    /// in SQL order. The parentheses come from the operator.
    SubQuery(String, Vec<String>),
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
    /// `selectCollection` calls kept for the post-pass that builds their
    /// sub-select joins: `(arguments, joins parsed so far)` — the position the
    /// collection join takes in the FROM clause, in call order. The reference
    /// processes collections after the whole chain is read (they reuse the ON
    /// conditions of the same-child-class `leftJoin`, which may come later).
    pub collection_calls: Vec<(Vec<String>, usize)>,
    /// `from(sub-lambda)`'s rendered sub-select (T-M21-07): the FROM clause
    /// declares `(SELECT … FROM table subAlias WHERE …) mainAlias` instead of
    /// the bare table, the outer alias being the main table's alias. `None`
    /// when the chain has no `from` call (or it refused to render).
    pub from_sub: Option<String>,
    /// `around(first, last)`'s first fragment (T-M21-09, the MPJ official
    /// `Join.around`): rendered before `SELECT`, the way the reference's
    /// `rawFirst` leads the statement.
    pub around_first: Option<String>,
    /// `around(first, last)`'s last fragment — or the template form's rendered
    /// text: rendered after `ORDER BY`, before `LIMIT` (MP's LAST segment
    /// position, which the reference's `rawAround` matches).
    pub around_last: Option<String>,
    /// Parameter sources the `around` last fragment contributed, in SQL order.
    pub around_params: Vec<Value>,
    /// `GROUP BY` columns / fragments.
    pub group_by: Vec<String>,
    /// `having("…", args)` with `{i}` already replaced by the rendered values.
    pub having: Option<String>,
    /// Parameters the HAVING fragment contributed, in SQL order.
    pub having_params: Vec<Value>,
    /// The main table's alias (`setAlias("x")`); MPJ's default is `t`.
    pub main_alias: Option<String>,
    pub distinct: bool,
    /// A join-update chain's SET assignments, in call order.
    pub sets: Vec<SetItem>,
    /// Parameter sources the SET side contributed, in SQL order (SET precedes
    /// WHERE, so these come before the condition parameters).
    pub set_params: Vec<Value>,
    /// A `JoinWrappers.delete` chain's delete targets, in call order.
    pub delete_targets: Vec<DeleteTarget>,
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
    /// The column is a complete expression the renderer passes through verbatim
    /// (`selectFunc`'s template result, `selectSub`'s sub-select) — no alias
    /// prefix, no quoting, the reference's `rawExpr` channel.
    pub raw_expr: bool,
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
                    match value {
                        Value::Param(source) => out.push(source.clone()),
                        // The `?`s inside a rendered sub-select stand for the
                        // sources the sub-render collected, already in SQL
                        // order (the tree order is the statement order).
                        Value::SubQuery(_, params) => out.extend(params.iter().cloned()),
                        _ => {}
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
    render_inner(text, columns, dialect, disabled, alias, false)
}

/// A `selectSub` lambda rendered as its own statement: method-reference values
/// resolve as bare columns (`eq(RefundEntity::getOrderId, OrderEntity::getId)`
/// → `order_id = id`), the way the reference's `QueryWrapperParser` treats a
/// `ColumnArg` right side — there is no alias context inside a sub-select, so
/// the column renders without one.
fn render_sub_statement(text: &str, columns: &EntityColumns, dialect: &Dialect) -> WrapperOutput {
    render_inner(text, columns, dialect, &Default::default(), None, true)
}

fn render_inner(
    text: &str,
    columns: &EntityColumns,
    dialect: &Dialect,
    disabled: &std::collections::BTreeSet<String>,
    alias: Option<&str>,
    sub_column_values: bool,
) -> WrapperOutput {
    // MPJ recognition must come from the whole text, not from the call list: a
    // chain written `JoinWrappers.lambda(User.class)` names its factory in the
    // *call* (`lambda`), while `new MPJLambdaWrapper<User>()` names it in the
    // construction — neither contains the plain `LambdaQueryWrapper` token.
    let mpj = text.contains("MPJLambdaWrapper")
        || text.contains("MPJQueryWrapper")
        || text.contains("JoinWrappers")
        || text.contains("MPJWrappers");

    let mut ctx = Ctx {
        columns,
        notes: Vec::new(),
        message_keys: Vec::new(),
        partial: false,
        unresolved: false,
        mpj,
        main_alias: alias.unwrap_or("t").to_string(),
        join_aliases: Vec::new(),
        join_classes: Vec::new(),
        on_mode: false,
        sub_column_values,
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

    // `selectCollection` sub-select joins: built after the whole chain is read,
    // because each collection reuses the ON conditions of the same-child-class
    // `leftJoin` — which the source may declare after the collection call.
    if mpj && !chain.collection_calls.is_empty() {
        apply_collections(&mut chain, &mut ctx);
    }

    // Missing table is a hard stop: the statement can't be built.
    if chain.table.is_none() {
        ctx.unresolved = true;
        push_key(&mut ctx, "wrapper.table_unknown");
        ctx.notes
            .push("the entity table is unknown; pass EntityColumns.table".to_string());
    }
    if chain.kind == WrapperKind::Update && !mpj {
        ctx.partial = true;
        push_key(&mut ctx, "wrapper.update_fragment_only");
        ctx.notes.push(
            "update wrappers render only the condition fragment; UPDATE/SET come from the mapper"
                .to_string(),
        );
    }
    // A join-update with no usable SET call is not an UPDATE: the statement
    // would be syntactically wrong, not just less specific. The reference
    // warns about the dropped calls instead; here the whole render refuses.
    if chain.kind == WrapperKind::Update && mpj && chain.sets.is_empty() {
        ctx.unresolved = true;
        push_key(&mut ctx, "wrapper.update_set_missing");
        ctx.notes.push(
            "no `set` call of this join-update chain could be resolved; an UPDATE without SET is not a statement".to_string(),
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
        // A join-update chain (`JoinWrappers.update(C)`) renders the whole
        // UPDATE — joins, SET and WHERE — the way the reference's
        // `UpdateJoinConverter` does. A plain update wrapper keeps the
        // fragment-only shape: UPDATE/SET come from the mapper.
        (_, WrapperKind::Update) if mpj => Some(render_update(&effective, dialect)),
        (_, WrapperKind::Delete) => Some(render_delete(&effective, columns, dialect)),
        (_, WrapperKind::Update) => render_fragment(&effective, dialect),
    };

    // The parameter order follows the statement: SET (`t.col = ?`) precedes
    // WHERE, and HAVING comes after the conditions.
    let mut params = Vec::new();
    for value in &chain.set_params {
        if let Value::Param(source) = value {
            params.push(source.clone());
        }
    }
    // The `setUpdateEntity` approximation and `set(col, ?)`-style entries
    // carry their runtime value as an Assign, not a raw fragment.
    for item in &chain.sets {
        if let SetItem::Assign {
            value: Value::Param(source),
            ..
        } = item
        {
            params.push(source.clone());
        }
    }
    params.extend(param_sources(&effective.conditions));
    params.extend(chain.having_params.iter().filter_map(|value| match value {
        Value::Param(source) => Some(source.clone()),
        _ => None,
    }));
    // The around fragment renders after HAVING and before LIMIT, so its
    // parameters join the order there.
    params.extend(chain.around_params.iter().filter_map(|value| match value {
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
    /// The entity class each parsed join belongs to, parallel to
    /// `chain.joins` — `None` for raw fragments and collection joins. The
    /// `selectCollection` post-pass matches a collection to the same-child-class
    /// join it replaces through this list.
    join_classes: Vec<Option<String>>,
    /// True while parsing an ON lambda: the second argument of a comparison is
    /// a column reference, not a value.
    on_mode: bool,
    /// True inside a `selectSub` sub-statement: a method-reference value
    /// resolves as a bare column (no alias — a sub-select has no alias
    /// context), the reference's `ColumnArg` column-comparison semantics.
    sub_column_values: bool,
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
    // The MPJ join-update/delete factories name their statement family in the
    // factory call; check them first, before the substring `Wrapper` rules
    // below could mistake them for query chains.
    if text.contains("JoinWrappers.update(") || text.contains("JoinWrappers.updateAndNull(") {
        return Some(WrapperKind::Update);
    }
    if text.contains("JoinWrappers.delete(") {
        return Some(WrapperKind::Delete);
    }
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
            // Factory / meta calls that build the wrapper, not conditions. A
            // non-MPJ chain's `delete` is the mapper's consumption point, a
            // no-op here; the join-delete's `delete(X.class, …)` names the
            // tables to delete.
            "lambdaQuery" | "query" | "lambdaUpdate" | "update" | "lambda" => {}
            "delete" if !ctx.mpj || call.args.is_empty() => {}
            name @ ("delete" | "deleteAll") if ctx.mpj => {
                parse_delete_call(name, &call.args, chain)
            }
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
                    values: vec![Value::SubQuery(sql, Vec::new())],
                }));
                *pending = Connector::And;
            }
            // A join-update's SET family and a join-delete's target list (the
            // reference's UpdateJoinConverter / DeleteJoinConverter). Only MPJ
            // chains carry them; a plain update wrapper keeps its mapper-owned
            // SET and stays fragment-only.
            name if ctx.mpj
                && matches!(
                    name,
                    "set"
                        | "setIfExists"
                        | "setSql"
                        | "setApply"
                        | "setIncrBy"
                        | "setDecrBy"
                        | "setUpdateEntity"
                        | "setUpdateEntityAndNull"
                ) =>
            {
                parse_set_call(name, &call.args, ctx, chain)
            }
            name if is_join_method(name) => parse_join(name, &call.args, chain, ctx),
            name @ ("selectAll" | "select" | "selectAs" | "selectIgnore" | "selectCount"
            | "selectSum" | "selectMax" | "selectMin" | "selectAvg") => {
                parse_select(name, &call.args, ctx, chain)
            }
            "selectFunc" => parse_select_func(&call.args, ctx, chain),
            "selectSub" if ctx.mpj => parse_select_sub(&call.args, ctx, chain),
            // `selectCollection` builds its sub-select join after the whole chain
            // is read (it reuses the same-child-class `leftJoin`'s ON conditions,
            // which may be declared later), so the call is recorded and applied
            // by `apply_collections`.
            "selectCollection" if ctx.mpj => {
                chain
                    .collection_calls
                    .push((call.args.clone(), chain.joins.len()));
            }
            // `selectAssociation` is a result-mapping marker only: the leftJoin
            // already expresses the association and the reference emits no SQL
            // for it (T-M7-04).
            "selectAssociation" if ctx.mpj => {}
            "from" if ctx.mpj => parse_from(&call.args, ctx, chain),
            "around" if ctx.mpj => parse_around(&call.args, ctx, chain),
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

// ---------------------------------------------------------------------------
// MPJ: the join-update SET family and the join-delete targets
// ---------------------------------------------------------------------------

/// A bare `true`/`false` first argument: the condition prefix of
/// `set(cond, col, val)`, which the reference strips before reading the
/// column and the value.
fn is_boolean_flag(raw: &str) -> bool {
    matches!(raw.trim(), "true" | "false")
}

/// Dispatches the join-update SET family to its shape. The reference keeps
/// the same split in `UpdateJoinConverter.parseSet`.
fn parse_set_call(name: &str, args: &[String], ctx: &mut Ctx, chain: &mut WrapperChain) {
    match name {
        "set" | "setIfExists" => parse_set(args, ctx, chain),
        "setSql" | "setApply" => parse_set_sql(name, args, ctx, chain),
        "setIncrBy" | "setDecrBy" => parse_set_incr(name, args, ctx, chain),
        _ => parse_set_entity(ctx, chain),
    }
}

/// `set(col, val)` / `setIfExists(col, val)` — the same rules the reference's
/// `parseSet` documents: a boolean first argument is a condition prefix, a
/// method-reference column always takes the main alias (cross-table ownership
/// is statically unknowable), a string column that already carries `t1.tel`
/// passes through verbatim, and a value taken from another column (the
/// three-argument form) is dropped with a note — the reference leaves that
/// entry out of the SET list too.
fn parse_set(args: &[String], ctx: &mut Ctx, chain: &mut WrapperChain) {
    let start = if args.len() >= 3 && is_boolean_flag(&args[0]) {
        1
    } else {
        0
    };
    let Some(column) = args.get(start).and_then(|a| render_set_column(a, ctx)) else {
        drop_set_call(ctx, chain, "set", args);
        return;
    };
    match args.get(start + 1) {
        Some(value) if value.contains("::") => {
            ctx.partial = true;
            push_key(ctx, "wrapper.update_set_unparsed");
            ctx.notes.push(format!(
                "`set({column}, …)` takes its value from a column; the SET entry is dropped"
            ));
            chain.unsupported.push("set".to_string());
        }
        Some(value) => {
            let value = parse_value_in(value, ctx);
            chain.sets.push(SetItem::Assign { column, value });
        }
        None => drop_set_call(ctx, chain, "set", args),
    }
}

/// `setSql("t.update_by = {0}", v)` / `setApply(cond, "t.name = concat(%s, …)",
/// c -> c.accept(col), …)`: the fragment renders verbatim with `{i}` replaced
/// by the argument values and, for `setApply`, the `%s` slot by the column the
/// accept-lambda names — the demo's documented shape. Runtime values render
/// as `?` and join the SET-side parameter order (SET precedes WHERE in the
/// statement).
fn parse_set_sql(name: &str, args: &[String], ctx: &mut Ctx, chain: &mut WrapperChain) {
    let start = if args.len() >= 2 && is_boolean_flag(&args[0]) {
        1
    } else {
        0
    };
    let Some(template) = args.get(start).and_then(|a| string_literal(a)) else {
        drop_set_call(ctx, chain, name, args);
        return;
    };
    let mut out = template;
    let mut params: Vec<Value> = Vec::new();
    let mut rest_index = start + 1;
    if out.contains("%s") {
        let column = args
            .get(rest_index)
            .and_then(|body| accept_argument(body))
            .and_then(|raw| render_set_column(&raw, ctx));
        match column {
            Some(column) => {
                out = out.replace("%s", &column);
                rest_index += 1;
            }
            None => {
                ctx.partial = true;
                push_key(ctx, "wrapper.update_set_unparsed");
                ctx.notes.push(
                    "`setApply(...)` has a `%s` slot its lambda does not fill; it stays verbatim"
                        .to_string(),
                );
            }
        }
    }
    for (index, arg) in args.iter().enumerate().skip(rest_index) {
        let rendered = match parse_value_in(arg, ctx) {
            Value::Param(source) => {
                params.push(Value::Param(source));
                "?".to_string()
            }
            value => render_value(&value, ctx.dialect),
        };
        out = out.replace(&format!("{{{}}}", index - rest_index), &rendered);
    }
    chain.set_params.extend(params);
    chain.sets.push(SetItem::Raw(out));
}

/// `setIncrBy(col, n)` / `setDecrBy(col, n)` render `col = col ± n` against
/// the main alias, the reference's raw-fragment shape. A runtime `n` renders
/// `?` and joins the SET-side parameter order.
fn parse_set_incr(name: &str, args: &[String], ctx: &mut Ctx, chain: &mut WrapperChain) {
    let start = if args.len() >= 3 && is_boolean_flag(&args[0]) {
        1
    } else {
        0
    };
    let Some(column) = args.get(start).and_then(|a| render_set_column(a, ctx)) else {
        drop_set_call(ctx, chain, name, args);
        return;
    };
    let Some(value) = args.get(start + 1) else {
        drop_set_call(ctx, chain, name, args);
        return;
    };
    let op = if name == "setIncrBy" { "+" } else { "-" };
    let rendered = match parse_value_in(value, ctx) {
        Value::Param(source) => {
            chain.set_params.push(Value::Param(source));
            "?".to_string()
        }
        value => render_value(&value, ctx.dialect),
    };
    chain
        .sets
        .push(SetItem::Raw(format!("{column} = {column} {op} {rendered}")));
}

/// `setUpdateEntity(entity)` / `setUpdateEntityAndNull(entity)`: the entity's
/// runtime field values are statically unknowable, so every non-id column of
/// the main table renders `t.col = ?` — the reference's documented
/// approximation (`AndNull` also includes null fields, statically
/// indistinguishable, so the two share one shape). The id column is excluded
/// when the projection named one.
fn parse_set_entity(ctx: &mut Ctx, chain: &mut WrapperChain) {
    let alias = ctx.main_alias.clone();
    let primary_key = ctx.columns.primary_key.clone();
    for (_, column, _) in &ctx.columns.columns {
        if primary_key.as_deref() == Some(column) {
            continue;
        }
        chain.sets.push(SetItem::Assign {
            column: format!("{alias}.{column}"),
            value: Value::Param(format!("entity.{column}")),
        });
    }
}

/// A SET call the renderer could not turn into an assignment: kept out of the
/// SET list with a note, the way the reference collects `unparsedSets`.
fn drop_set_call(ctx: &mut Ctx, chain: &mut WrapperChain, name: &str, args: &[String]) {
    ctx.partial = true;
    push_key(ctx, "wrapper.update_set_unparsed");
    ctx.notes.push(format!(
        "`{name}({})` could not be resolved to a SET entry; it is dropped",
        args.join(", ")
    ));
    chain.unsupported.push(name.to_string());
}

/// A SET column: a method reference always qualifies by the main alias (the
/// reference's ownership approximation); a string column with its own
/// `t1.tel` qualifier passes through verbatim; a bare string column gets the
/// main alias.
fn render_set_column(raw: &str, ctx: &Ctx) -> Option<String> {
    let alias = ctx.main_alias.clone();
    if let Some(column) = string_literal(raw) {
        return Some(if column.contains('.') {
            column
        } else {
            format!("{alias}.{column}")
        });
    }
    let (class, getter) = parse_field_ref(raw)?;
    let entity = ctx.columns.find(&class)?;
    let field = field_from_getter(&getter);
    let column = entity.column_for(&field)?.to_string();
    Some(format!("{alias}.{column}"))
}

/// The argument of the first `.accept(...)` call in a lambda body
/// (`c -> c.accept(UserDO::getName)` → `UserDO::getName`), the column slot
/// the demo's `setApply` fills.
fn accept_argument(body: &str) -> Option<String> {
    let start = body.find(".accept(")? + ".accept(".len();
    let rest = &body[start..];
    let end = rest.find(')')?;
    let arg = rest[..end].trim();
    (!arg.is_empty()).then(|| arg.to_string())
}

/// The delete targets of a `JoinWrappers.delete` chain: `delete(X.class)` /
/// `delete("t1")` / `deleteAll()`, kept symbolic and resolved against the
/// joins at render time — `deleteAll` may appear before the join calls.
fn parse_delete_call(name: &str, args: &[String], chain: &mut WrapperChain) {
    if name == "deleteAll" {
        chain.delete_targets.push(DeleteTarget::All);
        return;
    }
    for arg in args {
        if let Some(class) = class_literal(arg) {
            chain.delete_targets.push(DeleteTarget::Class(class));
        } else if let Some(table) = string_literal(arg) {
            chain.delete_targets.push(DeleteTarget::Raw(table));
        }
    }
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
        ctx.join_classes.push(None);
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
        ctx.join_classes.push(Some(class.clone()));
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

    ctx.join_classes.push(Some(class.clone()));
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
                    raw_expr: false,
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
                raw_expr: false,
            });
        }
    }
}

/// `selectFunc(template, func -> func.accept(col).values(v), alias)` → the
/// template with every `%s` replaced by the accepted columns and every `{i}` by
/// the bound values, rendered as one verbatim SELECT item (`CHAR_LENGTH(t.sex)
/// AS name_len`). Mirrors the reference's `parseSelectFunc` (T-M7-03):
///
/// - `%s` columns take the main alias prefix (`columnPrefix`), whatever entity
///   the receiver names — the same rule MPJ condition columns follow.
/// - `{i}` values render literals as written, runtime expressions as `?`.
/// - An enum first argument (`DefaultFuncEnum.SUM`) maps to the built-in
///   function; an unknown enum drops the column silently, exactly as the
///   reference does.
/// - A template left with a `%s` after substitution (fewer accepted columns
///   than placeholders) cannot render completely and is dropped.
fn parse_select_func(args: &[String], ctx: &mut Ctx, chain: &mut WrapperChain) {
    let (func_columns, func_values) = match args.get(1) {
        Some(lambda) => extract_func_args(lambda, ctx),
        None => (Vec::new(), Vec::new()),
    };
    let as_name = args.get(2).and_then(|arg| next_as_arg(arg));

    // Enum form (`selectFunc(DefaultFuncEnum.SUM, col, alias)`): the first
    // argument is a bare reference, not a string template.
    if let Some(source) = args.first().filter(|arg| string_literal(arg).is_none()) {
        let tail = source.trim().rsplit('.').next().unwrap_or(source.trim());
        let Some(keyword) = enum_function(tail) else {
            // The reference drops an unknown built-in silently; the statement
            // loses the column, which `Partial` says out loud.
            ctx.partial = true;
            push_key(ctx, "wrapper.unsupported_method");
            ctx.notes.push(format!(
                "`selectFunc` function `{tail}` is not a built-in; the column is dropped"
            ));
            chain.unsupported.push("selectFunc".to_string());
            return;
        };
        let Some(column) = func_columns.first() else {
            ctx.partial = true;
            push_key(ctx, "wrapper.unsupported_method");
            ctx.notes
                .push("`selectFunc` names no column to wrap; the column is dropped".to_string());
            chain.unsupported.push("selectFunc".to_string());
            return;
        };
        chain.select.push(SelectColumn {
            alias: String::new(),
            column: format!("{keyword}({column})"),
            as_name,
            function: None,
            raw_expr: true,
        });
        return;
    }

    // Template form: `%s` for the columns, `{i}` for the bound values.
    let Some(template) = args.first().and_then(|arg| string_literal(arg)) else {
        ctx.partial = true;
        push_key(ctx, "wrapper.unsupported_method");
        ctx.notes
            .push("`selectFunc` needs a template string or a built-in function".to_string());
        chain.unsupported.push("selectFunc".to_string());
        return;
    };
    let mut expr = template;
    for column in &func_columns {
        if !expr.contains("%s") {
            break;
        }
        expr = expr.replacen("%s", column, 1);
    }
    for (index, value) in func_values.iter().enumerate() {
        let rendered = match parse_value_in(value, ctx) {
            Value::Param(source) => {
                // A runtime value inside the SELECT list would splice a `?` into
                // the middle of the statement's parameter order — the same
                // boundary the sub-select renderer honors. Refuse rather than
                // misalign the preview's parameter rows.
                ctx.unresolved = true;
                push_key(ctx, "wrapper.structural_method");
                ctx.notes.push(format!(
                    "`selectFunc` value `{source}` is a runtime expression; its `?` cannot be lined up with the statement's parameter order, so no SQL is emitted"
                ));
                ctx.partial = false;
                chain.unsupported.push("selectFunc".to_string());
                return;
            }
            value => render_value(&value, ctx.dialect),
        };
        expr = expr.replace(&format!("{{{index}}}"), &rendered);
    }
    if expr.contains("%s") {
        // Fewer accepted columns than `%s` placeholders: the reference drops the
        // column rather than emit a broken expression.
        ctx.partial = true;
        push_key(ctx, "wrapper.unsupported_method");
        ctx.notes.push(
            "`selectFunc` accepted fewer columns than the template's `%s` placeholders; the column is dropped"
                .to_string(),
        );
        chain.unsupported.push("selectFunc".to_string());
        return;
    }
    chain.select.push(SelectColumn {
        alias: String::new(),
        column: expr,
        as_name,
        function: None,
        raw_expr: true,
    });
}

/// A `selectFunc` third argument: a string literal is the alias itself, a
/// `VO::getProperty` reference contributes the property name.
fn next_as_arg(arg: &str) -> Option<String> {
    if let Some(literal) = string_literal(arg) {
        return Some(literal);
    }
    let (_, getter) = parse_field_ref(arg)?;
    Some(field_from_getter(&getter))
}

/// MPJ built-in function enum name → SQL function name (the reference's
/// `enumFunctionOf`); `None` for anything outside the built-ins.
fn enum_function(name: &str) -> Option<&'static str> {
    match name.trim().to_uppercase().as_str() {
        "SUM" => Some("SUM"),
        "COUNT" => Some("COUNT"),
        "MAX" => Some("MAX"),
        "MIN" => Some("MIN"),
        "AVG" => Some("AVG"),
        "LEN" => Some("LENGTH"),
        "CONCAT" => Some("CONCAT"),
        _ => None,
    }
}

/// The columns and bound values a `selectFunc` consumer declares. Two shapes:
/// the consumer lambda `c -> c.accept(UserDO::getSex).values(0)` — every
/// `accept` argument resolves to its qualified column, every `values` argument
/// is kept for the `{i}` substitution — and the enum form's bare column
/// reference (`selectFunc(DefaultFuncEnum.COUNT, UserDO::getId, "cnt")`).
fn extract_func_args(lambda: &str, ctx: &mut Ctx) -> (Vec<String>, Vec<String>) {
    let body = strip_lambda_prefix(lambda);
    let mut columns = Vec::new();
    let mut values = Vec::new();
    let calls = extract_calls(&body);
    if calls.is_empty() {
        // No consumer lambda: a bare field reference is the column itself.
        if let Some(column) = resolve_column(&body, ctx) {
            columns.push(column);
        } else {
            ctx.unresolved = true;
            push_key(ctx, "wrapper.unknown_field");
            ctx.notes.push(format!(
                "cannot map `selectFunc` column `{body}` to a column"
            ));
            ctx.partial = false;
        }
        return (columns, values);
    }
    for call in calls {
        match call.name.as_str() {
            "accept" => {
                let Some(raw) = call.args.first() else {
                    continue;
                };
                match resolve_column(raw, ctx) {
                    Some(column) => columns.push(column),
                    None => {
                        ctx.unresolved = true;
                        push_key(ctx, "wrapper.unknown_field");
                        ctx.notes.push(format!(
                            "cannot map `selectFunc` column `{raw}` to a column"
                        ));
                        ctx.partial = false;
                    }
                }
            }
            "values" => values.extend(call.args.iter().cloned()),
            _ => {}
        }
    }
    (columns, values)
}

/// `selectSub(SubClass.class, sub -> sub.select(...).eq(...), alias)` →
/// `(SELECT ... FROM sub_table WHERE ...) AS alias` — a sub-select column of the
/// SELECT list (the reference's `parseSelectSub`, M15). The lambda renders as
/// its own single-table statement against the sub entity's projection, bare
/// columns: the reference parses it with `QueryWrapperParser(subTable)` and no
/// alias prefix, and a correlated right side (`eq(RefundEntity::getOrderId,
/// OrderEntity::getId)`) renders bare (`order_id = id`) — static analysis
/// cannot tell the outer table apart, the reference's own test says so.
///
/// A runtime value inside the sub-select would splice a `?` into the middle of
/// the statement's parameter order — the text alone cannot line the preview's
/// parameter rows up — so the render refuses (documented boundary; the
/// reference registers those params but its own ordering is unreliable).
fn parse_select_sub(args: &[String], ctx: &mut Ctx, chain: &mut WrapperChain) {
    let refuse = |ctx: &mut Ctx, chain: &mut WrapperChain, note: String| {
        ctx.unresolved = true;
        push_key(ctx, "wrapper.structural_method");
        ctx.notes.push(note);
        ctx.partial = false;
        chain.unsupported.push("selectSub".to_string());
    };
    let Some(lambda) = args.get(1) else {
        refuse(
            ctx,
            chain,
            "`selectSub` needs a sub-query lambda".to_string(),
        );
        return;
    };
    let Some(alias) = args.get(2).and_then(|arg| next_as_arg(arg)) else {
        // The reference silently drops the column when the alias is missing;
        // the statement would then return one column fewer than the reader
        // asked for, so this refuses instead.
        refuse(
            ctx,
            chain,
            "`selectSub` needs an alias argument to render the sub-select column".to_string(),
        );
        return;
    };
    let Some(class) = args.first().and_then(|arg| class_literal(arg)) else {
        refuse(
            ctx,
            chain,
            "`selectSub` names no sub-entity class this renderer can read".to_string(),
        );
        return;
    };
    let Some(mut sub_entity) = ctx.columns.find(&class).cloned() else {
        refuse(
            ctx,
            chain,
            format!(
                "the `selectSub` entity `{class}` is not in the projection; its table name is needed for the sub-select"
            ),
        );
        return;
    };
    // The sub-lambda may reference any projected entity (a correlated condition
    // names the outer table's class): widen the projection to everything the
    // main chain knows.
    let mut related = Vec::with_capacity(ctx.columns.related.len() + 1);
    related.push(ctx.columns.clone());
    related.extend(ctx.columns.related.iter().cloned());
    sub_entity.related = related;
    let body = strip_lambda_prefix(lambda);
    let out = render_sub_statement(&body, &sub_entity, ctx.dialect);
    if out.confidence == Confidence::Unresolved || !out.params.is_empty() {
        refuse(
            ctx,
            chain,
            "`selectSub`'s sub-select could not be rendered as a literal statement; no SQL is emitted"
                .to_string(),
        );
        return;
    }
    let Some(sub_sql) = out.sql else {
        refuse(
            ctx,
            chain,
            "`selectSub`'s sub-select could not be rendered as a literal statement; no SQL is emitted"
                .to_string(),
        );
        return;
    };
    chain.select.push(SelectColumn {
        alias: String::new(),
        // The reference renders `rawExpr = "($subSql)"` — the parentheses are
        // part of the expression, not the operator's.
        column: format!("({sub_sql})"),
        as_name: Some(alias),
        function: None,
        raw_expr: true,
    });
}

/// `from(sub-lambda)` (T-M21-07, the MPJ official
/// `JoinAbstractLambdaWrapper.from(MFunction)`): the FROM clause declares the
/// rendered sub-select instead of the table —
/// `FROM (SELECT tb.id, … FROM user tb WHERE tb.id >= 0 LIMIT 10) t` — with the
/// outer alias being the main table's alias. The sub-alias comes from the
/// lambda's `setAlias`, defaulting to `tb` (the reference's default).
///
/// A runtime value inside the sub-select would splice a `?` before the WHERE
/// clause's parameters — the reference renders it but collects no param source
/// for the sub-select, so the preview rows would misalign; this refuses instead.
fn parse_from(args: &[String], ctx: &mut Ctx, chain: &mut WrapperChain) {
    let refuse = |ctx: &mut Ctx, chain: &mut WrapperChain, note: String| {
        ctx.unresolved = true;
        push_key(ctx, "wrapper.structural_method");
        ctx.notes.push(note);
        ctx.partial = false;
        chain.unsupported.push("from".to_string());
    };
    let Some(lambda) = args.first() else {
        refuse(ctx, chain, "`from` needs a sub-query lambda".to_string());
        return;
    };
    let body = strip_lambda_prefix(lambda);
    let sub_alias = extract_string_call(&body, "setAlias").unwrap_or_else(|| "tb".to_string());
    let inner = remove_call(&body, "setAlias");
    let joined = JoinWrappers::rehead(&inner, &ctx.columns.class_name);
    let sub = render_with_alias(
        &joined,
        ctx.columns,
        ctx.dialect,
        &Default::default(),
        Some(&sub_alias),
    );
    if sub.confidence != Confidence::Exact {
        refuse(
            ctx,
            chain,
            "`from`'s sub-select could not be rendered as a literal statement; no SQL is emitted"
                .to_string(),
        );
        return;
    }
    chain.from_sub = sub.sql;
}

/// `around(first, last)` (T-M21-09, the MPJ official `Join.around(firstSql,
/// lastSql)`, 1.4.11+). Two shapes, the reference's `parseAround`:
///
/// - the official two-literal form `around("first", "last")` — the first
///   fragment leads the statement (before `SELECT`), the last renders after
///   `ORDER BY`, before `LIMIT`;
/// - the template form `around("sql {0}", v, …)` — the `{i}` placeholders bind
///   the remaining arguments and the result is the last fragment (the first
///   argument doubles as the template).
fn parse_around(args: &[String], ctx: &mut Ctx, chain: &mut WrapperChain) {
    let Some(first) = args.first().and_then(|arg| string_literal(arg)) else {
        // The reference drops an unrenderable fragment silently; the statement
        // would then lose a piece of its shape, which `Partial` says out loud.
        ctx.partial = true;
        push_key(ctx, "wrapper.unsupported_method");
        ctx.notes
            .push("`around` needs a string fragment; the call is dropped".to_string());
        chain.unsupported.push("around".to_string());
        return;
    };
    // The official two-literal form: the template carries no `{i}` placeholder
    // and both arguments are literals.
    if !first.contains('{')
        && let Some(last) = args.get(1).and_then(|arg| string_literal(arg))
    {
        chain.around_first = Some(first);
        chain.around_last = Some(last);
        return;
    }
    // The template form: `{i}` binds the remaining arguments.
    let mut last = first;
    let mut params: Vec<Value> = Vec::new();
    for (index, arg) in args.iter().enumerate().skip(1) {
        let rendered = match parse_value_in(arg, ctx) {
            Value::Param(source) => {
                params.push(Value::Param(source));
                ctx.partial = true;
                push_key(ctx, "wrapper.param_value");
                "?".to_string()
            }
            value => render_value(&value, ctx.dialect),
        };
        last = last.replace(&format!("{{{}}}", index - 1), &rendered);
    }
    chain.around_last = Some(last);
    chain.around_params = params;
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
    let Some((sub, sub_params)) = subquery_sql(&class, &args[2], ctx) else {
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
        values: vec![Value::SubQuery(sub, sub_params)],
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
    let Some((sub, sub_params)) = subquery_sql(&class, &args[1], ctx) else {
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
        values: vec![Value::SubQuery(sub, sub_params)],
    }));
    *pending = Connector::And;
}

/// The lambda's body rendered as a standalone SELECT, single-line, with the
/// parameter sources of its own `?`s. `None` when the class has no projection
/// or the sub-select is not fully literal. A sub-render with runtime values
/// (`eq(Field, variable)` → `?`) is accepted and the sources ride along in
/// SQL order — the order of the condition tree *is* the order of the `?`s in
/// the statement. Any other gap the sub-render reports (a dropped condition,
/// an unmapped field) still refuses: embedding a partially-blind sub-select
/// would read as a complete one.
fn subquery_sql(class: &str, lambda: &str, ctx: &Ctx) -> Option<(String, Vec<String>)> {
    let entity = ctx.columns.find(class)?;
    // The sub-render sees the sub entity as its own table plus every entity
    // the main chain projects: a method-reference value may name the main
    // entity too (`eq(RefundEntity::getOrderId, OrderEntity::getId)` renders
    // `order_id = id`, the bare right column the reference's own test
    // asserts), while columns render bare — there is no alias context inside
    // a sub-select.
    let mut projection = entity.clone();
    for related in std::iter::once(ctx.columns).chain(ctx.columns.related.iter()) {
        if related.class_name != entity.class_name {
            projection = projection.with_related(related.clone());
        }
    }
    let body = strip_lambda_prefix(lambda);
    let out = render_sub_statement(&body, &projection, ctx.dialect);
    let sql = out.sql?;
    if out.confidence != Confidence::Exact && out.params.is_empty() {
        return None;
    }
    Some((sql, out.params))
}

// ---------------------------------------------------------------------------
// MPJ: selectCollection post-pass
// ---------------------------------------------------------------------------

/// Builds every `selectCollection`'s sub-select join (the reference's
/// `parseCollection`, P06/P07 + T-M21-03 + T-1800):
///
/// ```text
/// LEFT JOIN (SELECT <cols> FROM <child> <alias>) <alias> ON <main-side> = <child-side>
/// ```
///
/// - The columns come from the map lambda's `id`/`result` calls (`*` when it
///   names none); a nested `collection` call recurses as a LEFT JOIN with the
///   reference's FK convention `grandchild.{parent}_id = {parent}.id`.
/// - The ON conditions are reused from the same-child-class `leftJoin` in the
///   chain — which the collection *replaces* (the plain join is dropped, T-M15-06)
///   — swapped so the main-table column leads, with the join's alias renamed to
///   the collection alias (the prefix form names a different one).
/// - The free form (first argument a VO method reference, no child class) is a
///   result-mapping marker only and produces no SQL, exactly as the reference
///   drops it from `collectionCalls`.
fn apply_collections(chain: &mut WrapperChain, ctx: &mut Ctx) {
    let calls = std::mem::take(&mut chain.collection_calls);
    for (args, position) in calls {
        // Form detection: arg0 a class → the normal form; arg0 a string prefix
        // with arg1 a class → the prefix form; anything else is the free form.
        let (child, alias, map) = if let Some(class) = args.first().and_then(|a| class_literal(a)) {
            let alias = ctx.columns.alias_of(&class);
            (class, alias, args.get(2).cloned())
        } else if let (Some(prefix), Some(class)) = (
            args.first().and_then(|a| string_literal(a)),
            args.get(1).and_then(|a| class_literal(a)),
        ) {
            (class, prefix, args.get(3).cloned())
        } else {
            continue;
        };
        let Some(entity) = ctx.columns.find(&child).cloned() else {
            ctx.unresolved = true;
            push_key(ctx, "wrapper.structural_method");
            ctx.notes.push(format!(
                "the `selectCollection` child entity `{child}` is not in the projection; its table name is needed for the sub-select"
            ));
            ctx.partial = false;
            chain.unsupported.push("selectCollection".to_string());
            return;
        };
        let Some(sub_select) = collection_sub_select(map.as_deref(), &entity, &alias, ctx) else {
            // `collection_sub_select` already marked the reason.
            ctx.partial = false;
            chain.unsupported.push("selectCollection".to_string());
            return;
        };
        // The ON: the same-child-class join's conditions, alias-renamed and
        // swapped; the join itself is replaced by the collection (dropped).
        let on = match ctx
            .join_classes
            .iter()
            .position(|class| class.as_deref() == Some(child.as_str()))
        {
            Some(index) => {
                ctx.join_classes.remove(index);
                let join = chain.joins.remove(index);
                swap_collection_on(join.on, &join.alias, &alias)
            }
            None => Vec::new(),
        };
        ctx.join_classes
            .insert(position.min(ctx.join_classes.len()), None);
        chain.joins.insert(
            position.min(chain.joins.len()),
            Join {
                kind: JoinKind::Left,
                table: String::new(),
                alias,
                on,
                raw: None,
                sub_select: Some(sub_select),
            },
        );
    }
}

/// The map lambda's `id`/`result` columns and the nested `collection` joins,
/// rendered as `SELECT <cols> FROM <child> <alias>` (+ the nested LEFT JOINs).
/// `None` marks the chain unresolved (an unmapped column or an unprojected
/// nested entity — the reference snake-case-guesses the table name there, this
/// renderer refuses to).
fn collection_sub_select(
    map: Option<&str>,
    entity: &EntityColumns,
    alias: &str,
    ctx: &mut Ctx,
) -> Option<String> {
    let mut columns: Vec<String> = Vec::new();
    let mut nested: Vec<String> = Vec::new();
    if let Some(map_text) = map {
        let body = strip_lambda_prefix(map_text);
        for call in extract_calls(&body) {
            match call.name.as_str() {
                "id" | "result" => {
                    let Some(raw) = call.args.first() else {
                        continue;
                    };
                    match collection_column(raw, ctx) {
                        Some(column) => columns.push(column),
                        None => {
                            ctx.unresolved = true;
                            push_key(ctx, "wrapper.unknown_field");
                            ctx.notes.push(format!(
                                "cannot map the `selectCollection` sub-select column `{raw}` to a column"
                            ));
                            return None;
                        }
                    }
                }
                "collection" => {
                    let Some(grandchild) = call
                        .args
                        .first()
                        .and_then(|arg| class_literal(arg))
                        .or_else(|| {
                            // The psi layer may hand the class over as a bare
                            // identifier; accept that shape too.
                            call.args.first().and_then(|arg| {
                                let text = arg.trim();
                                is_ident(text).then(|| text.to_string())
                            })
                        })
                    else {
                        ctx.unresolved = true;
                        push_key(ctx, "wrapper.structural_method");
                        ctx.notes.push(
                            "the `selectCollection` nested collection names no entity class this renderer can read"
                                .to_string(),
                        );
                        return None;
                    };
                    let Some(grand_entity) = ctx.columns.find(&grandchild).cloned() else {
                        ctx.unresolved = true;
                        push_key(ctx, "wrapper.structural_method");
                        ctx.notes.push(format!(
                            "the `selectCollection` nested entity `{grandchild}` is not in the projection; its table name is needed for the sub-select"
                        ));
                        return None;
                    };
                    let grand_alias = ctx.columns.alias_of(&grandchild);
                    // The map lambda is the last lambda-shaped argument.
                    let grand_map = call
                        .args
                        .iter()
                        .rev()
                        .find(|arg| arg.contains("->"))
                        .cloned();
                    let grand_sub = collection_sub_select(
                        grand_map.as_deref(),
                        &grand_entity,
                        &grand_alias,
                        ctx,
                    )?;
                    // The reference's FK convention (R7): the grandchild's column
                    // named after the parent alias + `_id` joins the parent's id.
                    nested.push(format!(
                        "LEFT JOIN ({grand_sub}) {grand_alias} ON {grand_alias}.{alias}_id = {alias}.id"
                    ));
                }
                _ => {}
            }
        }
    }
    if columns.is_empty() {
        columns.push("*".to_string());
    }
    let select = format!(
        "SELECT {} FROM {} {}",
        columns.join(", "),
        entity.table,
        alias
    );
    Some(if nested.is_empty() {
        select
    } else {
        format!("{select} {}", nested.join(" "))
    })
}

/// One `id`/`result` argument → the bare column name (the sub-select renders
/// unqualified columns, exactly as the reference's `renderCollectionSubSelect`).
fn collection_column(raw: &str, ctx: &Ctx) -> Option<String> {
    let (class, getter) = parse_field_ref(raw)?;
    let field = field_from_getter(&getter);
    let entity = ctx.columns.find(&class)?;
    entity.column_for(&field).map(str::to_string)
}

/// The reused join's ON conditions, prepared for the collection join: every
/// column token carrying the join's alias is renamed to the collection alias
/// (the prefix form names a different one, T-1800 Bug3), then each column-to-
/// column equality is swapped so the main-table column leads — the reference's
/// unconditional swap, harmless for equality.
fn swap_collection_on(
    nodes: Vec<CondNode>,
    join_alias: &str,
    collection_alias: &str,
) -> Vec<CondNode> {
    let rename = |token: &str| -> String {
        match token.split_once('.') {
            Some((alias, rest)) if alias == join_alias => {
                format!("{collection_alias}.{rest}")
            }
            _ => token.to_string(),
        }
    };
    nodes
        .into_iter()
        .map(|node| match node {
            CondNode::Leaf(mut leaf) => {
                leaf.column = rename(&leaf.column);
                let swappable = leaf.op == Op::Eq
                    && !leaf.column.is_empty()
                    && leaf.values.len() == 1
                    && matches!(leaf.values[0], Value::Column(_));
                if let Some(Value::Column(right)) = leaf.values.first_mut() {
                    *right = rename(right);
                }
                if swappable {
                    let right = match leaf.values.pop() {
                        Some(Value::Column(right)) => right,
                        _ => unreachable!("checked above"),
                    };
                    let left = std::mem::take(&mut leaf.column);
                    leaf.column = right;
                    leaf.values.push(Value::Column(left));
                }
                CondNode::Leaf(leaf)
            }
            other => other,
        })
        .collect()
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
/// And the `selectSub` rule: inside a sub-statement a method-reference value is
/// a bare column of whatever entity the receiver names (`OrderEntity::getId` →
/// `id`), no alias.
fn parse_value_in(raw: &str, ctx: &mut Ctx) -> Value {
    if ctx.on_mode
        && let Some((alias, column)) = resolve_qualified(raw, ctx)
    {
        return Value::Column(format!("{alias}.{column}"));
    }
    // MPJ compares column-to-column wherever a method reference appears in
    // the value slot — the reference's `QueryWrapperParser.simple()` resolves
    // both sides as columns, not only inside ON lambdas. Non-MPJ chains keep
    // the plain-wrapper rule (the value position is a runtime value there).
    if ctx.mpj
        && !ctx.sub_column_values
        && let Some((alias, column)) = resolve_qualified(raw, ctx)
    {
        return Value::Column(format!("{alias}.{column}"));
    }
    if ctx.sub_column_values
        && let Some(column) = resolve_sub_column(raw, ctx)
    {
        return Value::Column(column);
    }
    parse_value(raw)
}

/// A method-reference value inside a `selectSub` sub-statement → the bare
/// column name, resolved against the projection the sub-statement was given
/// (which `parse_select_sub` widened to every projected entity). `None` for
/// anything that is not a resolvable field reference.
fn resolve_sub_column(raw: &str, ctx: &Ctx) -> Option<String> {
    let (class, getter) = parse_field_ref(raw)?;
    let field = field_from_getter(&getter);
    let entity = ctx.columns.find(&class)?;
    entity.column_for(&field).map(str::to_string)
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
    // `around(first, …)`'s first fragment leads the statement
    // (`/* hint */ SELECT …`, the reference's `rawFirst` position).
    let mut sql = String::new();
    if let Some(first) = &chain.around_first {
        sql.push_str(first);
        sql.push(' ');
    }
    // T-M21-07: `from(sub-lambda)` replaces the FROM target with the rendered
    // sub-select, aliased by the main table's alias.
    if let (Some(sub), Some(alias)) = (&chain.from_sub, chain.main_alias.as_deref()) {
        sql.push_str(&format!(
            "SELECT {} FROM ({sub}) {alias}",
            render_select_list(chain, columns, dialect)
        ));
    } else {
        sql.push_str(&match chain.table.as_deref() {
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
        });
    }
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
    // A raw expression (`selectFunc`'s template result, `selectSub`'s
    // sub-select) is already the complete SQL text: the reference renders it
    // through `rawExpr` verbatim — no alias prefix, no quoting.
    let expr = if column.raw_expr {
        column.column.clone()
    } else {
        let qualified = if column.alias.is_empty() {
            render_column_name(&column.column, dialect)
        } else {
            render_column_name(&format!("{}.{}", column.alias, column.column), dialect)
        };
        match column.function {
            Some(function) => format!("{function}({qualified})"),
            None => qualified,
        }
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

/// The join-update statement: `UPDATE <table> <alias> [JOIN…] SET … WHERE …`.
/// The clause order is the reference's `generateUpdate`: joins before SET,
/// then the WHERE, then `last` verbatim at the end. Only `last` carries over
/// from the tail clauses — GROUP BY/HAVING/ORDER BY are not part of an
/// UPDATE.
fn render_update(chain: &WrapperChain, dialect: &Dialect) -> String {
    let alias = chain.main_alias.as_deref().unwrap_or("t");
    let table = chain.table.as_deref().unwrap_or("");
    let mut sql = format!("UPDATE {} {alias}", quote(table, dialect));
    for join in &chain.joins {
        sql.push(' ');
        sql.push_str(&render_join(join, dialect));
    }
    let sets: Vec<String> = chain
        .sets
        .iter()
        .map(|item| match item {
            SetItem::Assign { column, value } => format!(
                "{} = {}",
                render_column_name(column, dialect),
                render_value(value, dialect)
            ),
            SetItem::Raw(fragment) => fragment.clone(),
        })
        .collect();
    sql.push_str(&format!(" SET {}", sets.join(", ")));
    if let Some(where_sql) = render_where(chain, dialect) {
        sql.push(' ');
        sql.push_str(&where_sql);
    }
    if let Some(last) = &chain.last {
        sql.push(' ');
        sql.push_str(last);
    }
    sql
}

/// The join-delete statement: `DELETE <targets> FROM <table> <alias>
/// [JOIN…] [WHERE …]`, the reference's `generateDelete` multi-table shape.
/// `deleteAll()` expands to the main alias plus every join alias; a class
/// target renders its own default alias (the main entity → the main alias),
/// even when the join declared a different one — the reference does the same,
/// documented as its own behaviour.
fn render_delete(chain: &WrapperChain, columns: &EntityColumns, dialect: &Dialect) -> String {
    let alias = chain.main_alias.as_deref().unwrap_or("t");
    let mut targets: Vec<String> = Vec::new();
    let push_target = |targets: &mut Vec<String>, name: String| {
        if !targets.iter().any(|existing| existing == &name) {
            targets.push(name);
        }
    };
    for target in &chain.delete_targets {
        match target {
            DeleteTarget::Main => push_target(&mut targets, alias.to_string()),
            DeleteTarget::All => {
                push_target(&mut targets, alias.to_string());
                for join in &chain.joins {
                    push_target(&mut targets, join.alias.clone());
                }
            }
            DeleteTarget::Raw(raw) => push_target(&mut targets, raw.clone()),
            DeleteTarget::Class(class) => {
                let name = if columns
                    .find(class)
                    .is_some_and(|e| e.class_name == columns.class_name)
                {
                    alias.to_string()
                } else {
                    columns.alias_of(class)
                };
                push_target(&mut targets, name);
            }
        }
    }
    if targets.is_empty() {
        targets.push(alias.to_string());
    }
    let table = chain.table.as_deref().unwrap_or("");
    let mut sql = format!(
        "DELETE {} FROM {} {alias}",
        targets.join(", "),
        quote(table, dialect)
    );
    for join in &chain.joins {
        sql.push(' ');
        sql.push_str(&render_join(join, dialect));
    }
    if let Some(where_sql) = render_where(chain, dialect) {
        sql.push(' ');
        sql.push_str(&where_sql);
    }
    if let Some(last) = &chain.last {
        sql.push(' ');
        sql.push_str(last);
    }
    sql
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
    // `around(…, last)`'s last fragment renders between ORDER BY and LIMIT —
    // MP's LAST segment position, which the reference's `rawAround` matches
    // (T-M21-09).
    if let Some(around) = &chain.around_last {
        out.push(' ');
        out.push_str(around);
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
        let Some(Value::SubQuery(sql, _)) = c.values.first() else {
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
        Value::SubQuery(..) => String::new(),
    }
}

fn render_value(v: &Value, dialect: &Dialect) -> String {
    match v {
        Value::Column(name) => render_column_name(name, dialect),
        Value::SubQuery(sql, _) => sql.clone(),
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
            .with_column("headImg", "head_img", "varchar")
            .with_primary_key("id");
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

    /// The reference's own `selectSub` parity fixture
    /// (`SqlPreviewServiceOfficialApiTest.testOfficialApiUnionAndSelectSub`):
    /// `OrderEntity` + `RefundEntity`, with a correlated sub-select condition.
    fn order_cols() -> EntityColumns {
        let order = EntityColumns::new("OrderEntity", "order_entity")
            .with_column("id", "id", "bigint")
            .with_column("refundAmount", "refund_amount", "varchar");
        let refund = EntityColumns::new("RefundEntity", "refund_entity")
            .with_column("id", "id", "bigint")
            .with_column("orderId", "order_id", "bigint")
            .with_column("amount", "amount", "varchar");
        order.with_related(refund)
    }

    #[test]
    fn select_func_template_form_renders_the_bound_expression() {
        // Demo `aggregateExt`: the template's `%s` takes the main-alias prefix,
        // the alias argument renders `AS name_len`, exactly the reference's
        // `parseSelectFunc`.
        let out = render(
            r#"JoinWrappers.lambda(UserDO.class)
                .select(UserDO::getSex)
                .selectFunc("CHAR_LENGTH(%s)", c -> c.accept(UserDO::getSex), "name_len")
                .selectFunc("IFNULL(%s, 0)", c -> c.accept(UserDO::getSex), "age_or_sex")
                .leftJoin(AddressDO.class, AddressDO::getUserId, UserDO::getId)
                .groupBy(UserDO::getSex)"#,
            &user_cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Exact, "{out:?}");
        let sql = out.sql.expect("the chain renders");
        assert!(
            sql.contains(
                "SELECT t.sex, CHAR_LENGTH(t.sex) AS name_len, IFNULL(t.sex, 0) AS age_or_sex FROM",
            ),
            "{sql}"
        );
    }

    #[test]
    fn select_func_enum_form_maps_the_built_ins() {
        // Demo `distinctAndFunc`: `DefaultFuncEnum.COUNT` / `MAX` map to their SQL
        // functions over the main-alias column.
        let out = render(
            r#"JoinWrappers.lambda(UserDO.class)
                .distinct()
                .select(UserDO::getSex)
                .selectFunc(DefaultFuncEnum.COUNT, UserDO::getId, "cnt")
                .selectFunc(DefaultFuncEnum.MAX, UserDO::getAge, "maxAge")"#,
            &user_cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Exact, "{out:?}");
        let sql = out.sql.expect("the chain renders");
        assert!(sql.contains("COUNT(t.id) AS cnt"), "{sql}");
        assert!(sql.contains("MAX(t.age) AS maxAge"), "{sql}");
    }

    #[test]
    fn select_func_len_maps_to_length_and_plain_chains_render_bare_columns() {
        // Reference `QueryWrapperParserSelectTest`: `LEN` → `LENGTH`; a plain
        // (non-MPJ) chain has no alias context, so the column renders bare.
        let out = render(
            r#"new LambdaQueryWrapper<User>()
                .selectFunc(DefaultFuncEnum.LEN, f -> f.accept(User::getName), "nameLen")"#,
            &cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Exact, "{out:?}");
        assert_eq!(
            out.sql.as_deref(),
            Some("SELECT LENGTH(name) AS nameLen FROM sys_user")
        );
    }

    #[test]
    fn select_func_with_a_runtime_bound_value_refuses_the_render() {
        // A runtime `{i}` value would splice a `?` into the SELECT list's
        // parameter order — the same boundary the sub-select renderer honors.
        let out = render(
            r#"JoinWrappers.lambda(UserDO.class)
                .selectFunc("IFNULL(%s, {0})", c -> c.accept(UserDO::getSex).values(minAge), "x")"#,
            &user_cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Unresolved, "{out:?}");
        assert_eq!(out.sql, None);
    }

    #[test]
    fn select_func_with_an_unknown_builtin_degrades_to_partial() {
        // The reference drops an unknown built-in's column silently; the port
        // keeps the SQL and says the loss out loud (`Partial`).
        let out = render(
            r#"JoinWrappers.lambda(UserDO.class)
                .select(UserDO::getSex)
                .selectFunc(DefaultFuncEnum.NOPE, UserDO::getId, "x")"#,
            &user_cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Partial, "{out:?}");
        assert!(out.sql.is_some());
        assert!(out.chain.unsupported.contains(&"selectFunc".to_string()));
    }

    #[test]
    fn select_sub_renders_a_literal_sub_select_column() {
        // Demo `selectSubQuery`: the lambda renders as its own single-table
        // statement, bare columns, wrapped and aliased in the SELECT list.
        let out = render(
            r#"JoinWrappers.lambda(UserDO.class)
                .selectAll(UserDO.class)
                .selectSub(AddressDO.class, sub -> sub
                    .select(AddressDO::getTel)
                    .eq(AddressDO::getUserId, 1), UserDTO::getTel)
                .leftJoin(AddressDO.class, AddressDO::getUserId, UserDO::getId)
                .eq(UserDO::getId, 1)"#,
            &user_cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Exact, "{out:?}");
        let sql = out.sql.expect("the chain renders");
        assert!(
            sql.contains("(SELECT tel FROM user_address WHERE user_id = 1) AS tel",),
            "{sql}"
        );
        // `user` is a reserved word — the reference quotes the table name too.
        assert!(
            sql.contains("FROM `user` t LEFT JOIN user_address addressDO"),
            "{sql}"
        );
    }

    #[test]
    fn select_sub_correlated_condition_renders_the_outer_column_bare() {
        // Reference `SqlPreviewServiceOfficialApiTest`: `eq(RefundEntity::getOrderId,
        // OrderEntity::getId)` → `order_id = id` — static analysis cannot tell the
        // outer table apart, so the right side renders bare (the reference's own
        // 近似说明).
        let out = render(
            r#"JoinWrappers.lambda(OrderEntity.class)
                .selectAll(OrderEntity.class)
                .selectSub(RefundEntity.class, r -> r
                    .select(RefundEntity::getAmount)
                    .eq(RefundEntity::getOrderId, OrderEntity::getId), OrderEntity::getRefundAmount)
                .eq(OrderEntity::getId, 1001)"#,
            &order_cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Exact, "{out:?}");
        assert_eq!(
            out.sql.as_deref(),
            Some(
                "SELECT t.id, t.refund_amount, (SELECT amount FROM refund_entity WHERE order_id = id) AS refundAmount FROM order_entity t WHERE t.id = 1001"
            ),
            "{out:?}"
        );
    }

    #[test]
    fn select_sub_with_a_runtime_value_inside_refuses_the_render() {
        // A runtime value inside the sub-select would splice a `?` into the
        // middle of the statement's parameter order — kept unresolved until the
        // parameter-order scheme lands (documented boundary).
        let out = render(
            r#"JoinWrappers.lambda(UserDO.class)
                .selectAll(UserDO.class)
                .selectSub(AddressDO.class, sub -> sub
                    .select(AddressDO::getTel)
                    .eq(AddressDO::getUserId, userId), UserDTO::getTel)"#,
            &user_cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Unresolved, "{out:?}");
        assert_eq!(out.sql, None);
    }

    #[test]
    fn select_collection_replaces_the_same_child_join_with_a_sub_select() {
        // Demo `selectJoinCollection`: the collection's sub-select join reuses the
        // same-child-class leftJoin's ON conditions (swapped so the main column
        // leads) and *replaces* that join — the reference's T-M15-06 dedup.
        let out = render(
            r#"JoinWrappers.lambda(UserDO.class)
                .selectAll(UserDO.class)
                .selectCollection(AddressDO.class, UserDTO::getAddressList)
                .leftJoin(AddressDO.class, AddressDO::getUserId, UserDO::getId)
                .eq(UserDO::getId, 1)"#,
            &user_cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Exact, "{out:?}");
        let sql = out.sql.expect("the chain renders");
        assert!(
            sql.contains("LEFT JOIN (SELECT * FROM user_address addressDO) addressDO ON t.id = addressDO.user_id"),
            "{sql}"
        );
        // The plain join it replaced must not render twice.
        assert!(
            !sql.contains("LEFT JOIN user_address addressDO ON"),
            "{sql}"
        );
    }

    #[test]
    fn select_collection_reuses_a_lambda_on_join_s_conditions() {
        // Demo `selectCollectionWithFilter`: the lambda-ON join's column
        // comparison is swapped (main column leads) and its value condition kept.
        let out = render(
            r#"JoinWrappers.lambda(UserDO.class)
                .selectAll(UserDO.class)
                .selectCollection(AddressDO.class, UserDTO::getAddressList)
                .leftJoin(AddressDO.class, on -> on
                    .eq(AddressDO::getUserId, UserDO::getId)
                    .eq(AddressDO::getStatus, 1))
                .eq(UserDO::getSex, 1)"#,
            &user_cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Exact, "{out:?}");
        let sql = out.sql.expect("the chain renders");
        assert!(
            sql.contains("LEFT JOIN (SELECT * FROM user_address addressDO) addressDO ON t.id = addressDO.user_id AND addressDO.status = 1"),
            "{sql}"
        );
    }

    #[test]
    fn select_collection_prefix_form_uses_the_prefix_alias_and_map_columns() {
        // T-M21-03 prefix form (`QueryLabel.java`): the prefix is the sub-select's
        // table alias and the map lambda's id/result calls are its columns; the
        // reused join's alias is renamed to the prefix (T-1800 Bug3).
        let out = render(
            r#"JoinWrappers.lambda(UserDO.class)
                .selectAll(UserDO.class)
                .selectCollection("addr", AddressDO.class, UserDTO::getAddressList, m -> m
                    .id(AddressDO::getId)
                    .result(AddressDO::getTel))
                .leftJoin(AddressDO.class, AddressDO::getUserId, UserDO::getId)
                .eq(UserDO::getId, 1)"#,
            &user_cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Exact, "{out:?}");
        let sql = out.sql.expect("the chain renders");
        assert!(
            sql.contains(
                "LEFT JOIN (SELECT id, tel FROM user_address addr) addr ON t.id = addr.user_id"
            ),
            "{sql}"
        );
    }

    #[test]
    fn select_collection_free_form_is_a_mapping_marker_without_sql() {
        // T-M21-03 free form (first argument a VO method reference, no child
        // class): no sub-select, no join — the reference drops it the same way.
        let out = render(
            r#"JoinWrappers.lambda(UserDO.class)
                .selectAll(UserDO.class)
                .selectCollection(UserDTO::getAddressList, m -> m.id(AddressDO::getId))
                .eq(UserDO::getId, 1)"#,
            &user_cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Exact, "{out:?}");
        let sql = out.sql.expect("the chain renders");
        assert!(!sql.contains("LEFT JOIN"), "{sql}");
    }

    #[test]
    fn select_collection_with_an_unprojected_child_refuses_the_render() {
        // 不猜: the reference snake-case-guesses an unknown child's table name;
        // this renderer refuses (a guessed table is wrong SQL).
        let out = render(
            r#"JoinWrappers.lambda(UserDO.class)
                .selectAll(UserDO.class)
                .selectCollection(RecordEntity.class, UserDTO::getRecords)
                .leftJoin(RecordEntity.class, RecordEntity::getUserId, UserDO::getId)"#,
            &user_cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Unresolved, "{out:?}");
        assert_eq!(out.sql, None);
    }

    #[test]
    fn select_association_is_a_mapping_marker_only() {
        // Demo `selectAssociation`: the leftJoin already expresses the
        // association; the reference emits no SQL for the call (T-M7-04) — the
        // chain renders exactly as if it were absent.
        let with_it = render(
            r#"JoinWrappers.lambda(UserDO.class)
                .selectAll(UserDO.class)
                .selectAssociation(AddressDO.class, UserDTO::getDefaultAddress)
                .leftJoin(AddressDO.class, AddressDO::getUserId, UserDO::getId)
                .eq(UserDO::getId, 1)"#,
            &user_cols(),
            mysql(),
        );
        let without_it = render(
            r#"JoinWrappers.lambda(UserDO.class)
                .selectAll(UserDO.class)
                .leftJoin(AddressDO.class, AddressDO::getUserId, UserDO::getId)
                .eq(UserDO::getId, 1)"#,
            &user_cols(),
            mysql(),
        );
        assert_eq!(with_it.confidence, Confidence::Exact, "{with_it:?}");
        assert_eq!(with_it.sql, without_it.sql, "{with_it:?}");
    }

    #[test]
    fn from_renders_the_sub_query_as_the_from_target() {
        // Reference `mpjJoin_from子查询主表` (T-M21-07): the sub-lambda renders
        // under its own alias and the FROM clause declares `(sub-select) t` —
        // the outer alias is the main table's.
        let out = render(
            r#"JoinWrappers.lambda(DeviceInfoEntity.class)
                .selectAll()
                .from(from -> from
                    .setAlias("tb")
                    .selectAll()
                    .ge(DeviceInfoEntity::getId, 0)
                    .last("LIMIT 10"))
                .eq(DeviceInfoEntity::getId, 1001)"#,
            &device_cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Exact, "{out:?}");
        assert_eq!(
            out.sql.as_deref(),
            Some(
                "SELECT t.id, t.station_id, t.device_name FROM (SELECT tb.id, tb.station_id, tb.device_name FROM car_device_info tb WHERE tb.id >= 0 LIMIT 10) t WHERE t.id = 1001"
            ),
            "{out:?}"
        );
    }

    #[test]
    fn from_without_set_alias_defaults_to_tb_and_keeps_later_joins() {
        // Demo `joinFromAndPage` (minus around): no `setAlias` in the lambda →
        // the reference's `tb` default; joins after `from` still render.
        let out = render(
            r#"JoinWrappers.lambda(DeviceInfoEntity.class)
                .selectAll()
                .from(from -> from
                    .selectAll()
                    .ge(DeviceInfoEntity::getId, 0)
                    .last("LIMIT 100"))
                .leftJoin(StationInfoEntity.class, StationInfoEntity::getStationId, DeviceInfoEntity::getStationId)"#,
            &device_cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Exact, "{out:?}");
        let sql = out.sql.expect("the chain renders");
        assert!(
            sql.contains("FROM (SELECT tb.id, tb.station_id, tb.device_name FROM car_device_info tb WHERE tb.id >= 0 LIMIT 100) t"),
            "{sql}"
        );
        assert!(
            sql.contains("LEFT JOIN station_info stationInfoEntity ON stationInfoEntity.station_id = t.station_id"),
            "{sql}"
        );
    }

    #[test]
    fn around_two_literal_form_leads_and_trails_the_statement() {
        // Reference `mpjJoin_around双字符串形态` (T-M21-09): the first fragment
        // renders before SELECT, the last after WHERE (before LIMIT).
        let out = render(
            r#"JoinWrappers.lambda(DeviceInfoEntity.class)
                .around("/* around hint */", "FOR UPDATE")
                .eq(DeviceInfoEntity::getId, 1)"#,
            &device_cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Exact, "{out:?}");
        assert_eq!(
            out.sql.as_deref(),
            Some(
                "/* around hint */ SELECT t.id, t.station_id, t.device_name FROM car_device_info t WHERE t.id = 1 FOR UPDATE"
            ),
            "{out:?}"
        );
    }

    #[test]
    fn around_template_form_binds_arguments_and_renders_between_order_and_limit() {
        // Reference `mpjJoin_around位置与参数替换` (T-M21-09): the `{i}` template
        // binds the remaining arguments; the fragment renders after ORDER BY,
        // before LIMIT.
        let out = render(
            r#"JoinWrappers.lambda(DeviceInfoEntity.class)
                .orderByAsc(DeviceInfoEntity::getId)
                .around("AND t.id > {0}", 5)
                .last("LIMIT 10")"#,
            &device_cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Exact, "{out:?}");
        assert_eq!(
            out.sql.as_deref(),
            Some(
                "SELECT t.id, t.station_id, t.device_name FROM car_device_info t ORDER BY t.id ASC AND t.id > 5 LIMIT 10"
            ),
            "{out:?}"
        );
    }

    #[test]
    fn around_with_a_runtime_template_value_degrades_to_partial_in_order() {
        // A runtime `{0}` value renders `?` and joins the parameter order after
        // the conditions (the fragment sits after ORDER BY).
        let out = render(
            r#"JoinWrappers.lambda(DeviceInfoEntity.class)
                .eq(DeviceInfoEntity::getId, 1)
                .around("AND t.id > {0}", minId)"#,
            &device_cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Partial, "{out:?}");
        let sql = out.sql.as_ref().expect("partial carries SQL");
        assert!(sql.ends_with("AND t.id > ?"), "{sql}");
        assert_eq!(out.params, vec!["minId"], "{out:?}");
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

    // MPJ: join-update / join-delete (the reference's UpdateJoinConverter /
    // DeleteJoinConverter, demo-repo parity) -----------------------------

    #[test]
    fn a_join_update_renders_the_whole_statement() {
        // demo `updateJoinExt` in full: setUpdateEntity (entity values are
        // statically unknowable → every non-id column renders `t.col = ?`),
        // a cross-table `set` (the reference approximates by the main alias),
        // the three-argument form (dropped, with a note — the reference does
        // the same), incr/decr and the `%s` slot of setApply.
        let out = render(
            r#"JoinWrappers.update(UserDO.class)
                .setUpdateEntity(new UserDO().setAge(20))
                .set(AddressDO::getTel, "13800000000")
                .set(UserDO::getHeadImg, UserDO::getName, "addr")
                .setIncrBy(UserDO::getAge, 1)
                .setDecrBy(true, UserDO::getSex, 1)
                .setApply(true, "t.name = concat(%s, '_u')", c -> c.accept(UserDO::getName))
                .innerJoin(AddressDO.class, AddressDO::getUserId, UserDO::getId)
                .eq(AddressDO::getStatus, 1)"#,
            &user_cols(),
            mysql(),
        );
        // The dropped three-argument `set` narrows the SET list, so the
        // statement stays partial, like the reference's warning.
        assert_eq!(out.confidence, Confidence::Partial, "{out:?}");
        let sql = out.sql.as_deref().expect("the chain renders");
        assert_eq!(
            sql,
            "UPDATE `user` t \
             INNER JOIN user_address addressDO ON addressDO.user_id = t.id \
             SET t.name = ?, t.sex = ?, t.age = ?, t.head_img = ?, \
             t.tel = '13800000000', t.age = t.age + 1, t.sex = t.sex - 1, \
             t.name = concat(t.name, '_u') \
             WHERE t.status = 1"
        );
        // SET parameters precede the condition's.
        assert_eq!(
            out.params,
            vec!["entity.name", "entity.sex", "entity.age", "entity.head_img"],
            "{:?}",
            out.params
        );
    }

    #[test]
    fn a_join_update_takes_its_alias_from_set_alias() {
        // The reference's own test: `setAlias("u")` retitles the statement,
        // the SET and the condition; the lambda ON keeps the entity aliases.
        let out = render(
            r#"JoinWrappers.update(UserDO.class)
                .setAlias("u")
                .leftJoin(AddressDO.class, on -> on
                    .eq(AddressDO::getUserId, UserDO::getId))
                .set(UserDO::getName, "x")
                .eq(UserDO::getId, 1)"#,
            &user_cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Exact, "{out:?}");
        assert_eq!(
            out.sql.as_deref().expect("the chain renders"),
            "UPDATE `user` u \
             LEFT JOIN user_address addressDO ON addressDO.user_id = u.id \
             SET u.name = 'x' \
             WHERE u.id = 1"
        );
    }

    #[test]
    fn a_join_update_passes_raw_joins_and_qualified_columns_through() {
        // The reference's own tests: a boolean prefix before `set`, a raw SQL
        // join fragment, a caller-qualified `t1.tel` SET column and condition.
        let out = render(
            r#"JoinWrappers.update(UserDO.class)
                .set(true, UserDO::getName, "x")
                .set("t1.tel", "138")
                .leftJoin("address t1 on t1.user_id = t.id")
                .eq("t1.status", "1")"#,
            &user_cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Exact, "{out:?}");
        assert_eq!(
            out.sql.as_deref().expect("the chain renders"),
            "UPDATE `user` t \
             LEFT JOIN address t1 on t1.user_id = t.id \
             SET t.name = 'x', t1.tel = '138' \
             WHERE t1.status = '1'"
        );
    }

    #[test]
    fn a_join_update_with_only_unusable_set_calls_refuses() {
        // An UPDATE without a usable SET is not a statement: refuse instead
        // of rendering one that would fail to parse in a real database.
        let out = render(
            r#"JoinWrappers.update(UserDO.class)
                .set(UserDO::getHeadImg, UserDO::getName, "addr")
                .eq(UserDO::getId, 1)"#,
            &user_cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Unresolved, "{out:?}");
        assert_eq!(out.sql, None);
        assert!(
            out.message_keys
                .contains(&"wrapper.update_set_missing".to_string()),
            "{:?}",
            out.message_keys
        );
    }

    #[test]
    fn a_join_delete_renders_its_targets_and_joins() {
        // demo `deleteJoinExt`, all three target shapes: no delete call
        // (main alias only), explicit classes (`AddressDO.class` resolves to
        // its own default alias — the join's explicit `addr` stays on the
        // FROM side, exactly as the reference documents) and `deleteAll()`.
        let main_only = render(
            r#"JoinWrappers.delete(UserDO.class)
                .innerJoin(AddressDO.class, AddressDO::getUserId, UserDO::getId)
                .eq(AddressDO::getStatus, 0)"#,
            &user_cols(),
            mysql(),
        );
        assert_eq!(
            main_only.sql.as_deref().expect("the chain renders"),
            "DELETE t FROM `user` t \
             INNER JOIN user_address addressDO ON addressDO.user_id = t.id \
             WHERE t.status = 0"
        );

        let explicit = render(
            r#"JoinWrappers.delete(UserDO.class)
                .delete(UserDO.class, AddressDO.class)
                .innerJoin(AddressDO.class, "addr", AddressDO::getUserId, UserDO::getId)
                .isNull(UserDO::getHeadImg)"#,
            &user_cols(),
            mysql(),
        );
        assert_eq!(
            explicit.sql.as_deref().expect("the chain renders"),
            "DELETE t, addressDO FROM `user` t \
             INNER JOIN user_address addr ON addr.user_id = t.id \
             WHERE t.head_img IS NULL"
        );

        let all = render(
            r#"JoinWrappers.delete(UserDO.class)
                .deleteAll()
                .leftJoin(AddressDO.class, AddressDO::getUserId, UserDO::getId)
                .eq(UserDO::getId, 1001)"#,
            &user_cols(),
            mysql(),
        );
        assert_eq!(
            all.sql.as_deref().expect("the chain renders"),
            "DELETE t, addressDO FROM `user` t \
             LEFT JOIN user_address addressDO ON addressDO.user_id = t.id \
             WHERE t.id = 1001"
        );
    }

    #[test]
    fn a_subquery_condition_with_runtime_values_embeds_them_in_order() {
        // The rule the previous version refused on: a sub-select whose own
        // conditions carry runtime values renders its `?`s in place, and the
        // parameter sources ride along in statement order. A sub-select with
        // a *dropped* gap would still refuse (the sub-render stays partial
        // with no parameters to embed).
        let out = render(
            r#"JoinWrappers.lambda(UserDO.class)
                .selectAll(UserDO.class)
                .in(UserDO::getId, AddressDO.class, sub -> sub
                    .select(AddressDO::getUserId)
                    .eq(AddressDO::getStatus, minAge))
                .exists(AreaDO.class, sub -> sub
                    .select(AreaDO::getId)
                    .eq(AreaDO::getProvince, "浙江省"))
                .eq(UserDO::getSex, 1)"#,
            &user_cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Exact, "{out:?}");
        let sql = out.sql.as_deref().expect("the chain renders");
        assert!(
            sql.contains("t.id IN (SELECT user_id FROM user_address WHERE status = ?)"),
            "{sql}"
        );
        assert!(
            sql.contains("EXISTS (SELECT id FROM area WHERE province = '浙江省')"),
            "{sql}"
        );
        assert_eq!(out.params, vec!["minAge"], "{:?}", out.params);
    }

    #[test]
    fn a_subquery_method_reference_value_renders_the_bare_column() {
        // The reference's own selectSub-style case: the value column belongs
        // to the main entity and renders bare inside the sub-select
        // (`eq(RefundEntity::getOrderId, OrderEntity::getId)` → `order_id = id`).
        let out = render(
            r#"JoinWrappers.lambda(UserDO.class)
                .selectAll(UserDO.class)
                .in(UserDO::getId, AddressDO.class, sub -> sub
                    .select(AddressDO::getUserId)
                    .eq(AddressDO::getUserId, UserDO::getId))"#,
            &user_cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Exact, "{out:?}");
        let sql = out.sql.as_deref().expect("the chain renders");
        assert!(
            sql.contains("t.id IN (SELECT user_id FROM user_address WHERE user_id = id)"),
            "{sql}"
        );
    }

    #[test]
    fn an_mpj_condition_value_resolves_as_a_column() {
        // The reference's `QueryWrapperParser.simple()` resolves a method
        // reference in the value slot as a column, wherever it appears —
        // not only inside ON lambdas.
        let out = render(
            r#"JoinWrappers.lambda(UserDO.class)
                .selectAll(UserDO.class)
                .ge(UserDO::getAge, UserDO::getId)"#,
            &user_cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Exact, "{out:?}");
        let sql = out.sql.as_deref().expect("the chain renders");
        assert!(sql.contains("WHERE t.age >= t.id"), "{sql}");
    }

    #[test]
    fn a_subquery_with_a_dropped_gap_still_refuses() {
        // `apply` narrows the sub-select with a fragment this renderer cannot
        // expand; the sub-render comes back partial with no parameter to
        // embed, so embedding it would read as a complete sub-select.
        let out = render(
            r#"JoinWrappers.lambda(UserDO.class)
                .selectAll(UserDO.class)
                .in(UserDO::getId, AddressDO.class, sub -> sub
                    .select(AddressDO::getUserId)
                    .apply("status > 0"))
                .eq(UserDO::getSex, 1)"#,
            &user_cols(),
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Unresolved, "{out:?}");
        assert_eq!(out.sql, None);
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
