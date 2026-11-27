//! Rendering: SqlNode tree → SQL text + ordered parameter values (T1-1).
//!
//! Whitespace follows the reference engine exactly: sibling segments are joined
//! with a single space (`StringJoiner(" ")`), the final SQL is trimmed at both
//! ends, and a `<trim>` body keeps its own literal whitespace (only the
//! `prefix`/`suffix` step removes the overridden prefix/suffix). Nothing is
//! re-formatted or squeezed beyond that.
//!
//! `${...}` tokens are substituted during the walk (`ew.*` fragments are
//! spliced verbatim; every other expression goes through the restricted OGNL
//! subset). `#{...}` tokens are converted to `?` in a final pass and their
//! values are collected in SQL order.

use std::collections::BTreeMap;

use serde_json::Value;

use super::Note;
use super::ognl;
use super::script::{ForeachNode, SqlNode, TrimNode};
use super::token::{ParameterToken, parse_tokens};

pub const KEY_EW_MISSING: &str = "xml.ew.missing_segment";
pub const KEY_FOREACH_NULL: &str = "xml.foreach.null_collection";
pub const KEY_FOREACH_NOT_ITERABLE: &str = "xml.foreach.not_iterable";
pub const KEY_BIND_FAILED: &str = "xml.bind.failed";

pub struct RenderCtx {
    pub bindings: BTreeMap<String, Value>,
    pub unique: usize,
    pub partial: bool,
    pub unresolved: Option<(String, String)>,
    pub notes: Vec<Note>,
    pub dollar_substitutions: usize,
    /// Condition switches the page set (C07): condition id → truth. An id in
    /// this map decides its `<if>` / `<when>`; an id absent from it falls back
    /// to the OGNL evaluation, exactly as the reference's
    /// `DynamicContext.conditionResult(test)` does (its `n` map).
    pub conditions: BTreeMap<String, bool>,
}

impl RenderCtx {
    pub fn new(params: &Value) -> Self {
        let mut bindings = BTreeMap::new();
        match params {
            Value::Object(map) => {
                for (k, v) in map {
                    bindings.insert(k.clone(), v.clone());
                }
            }
            Value::Null => {}
            other => {
                bindings.insert("value".to_string(), other.clone());
            }
        }
        bindings.insert("_parameter".to_string(), params.clone());
        Self {
            bindings,
            unique: 0,
            partial: false,
            unresolved: None,
            notes: Vec::new(),
            dollar_substitutions: 0,
            conditions: BTreeMap::new(),
        }
    }

    fn note(&mut self, key: &str, detail: impl Into<String>) {
        self.notes.push(Note {
            message_key: key.to_string(),
            detail: detail.into(),
        });
    }

    fn fail(&mut self, key: &str, detail: impl Into<String>) {
        if self.unresolved.is_none() {
            self.unresolved = Some((key.to_string(), detail.into()));
        }
    }
}

/// Output segments are appended here.
pub trait Sink {
    fn append(&mut self, sql: &str);
}

pub struct RootSink {
    pub buf: String,
}

impl Sink for RootSink {
    fn append(&mut self, sql: &str) {
        if sql.is_empty() {
            return;
        }
        if !self.buf.is_empty() {
            self.buf.push(' ');
        }
        self.buf.push_str(sql);
    }
}

struct TrimSink<'a> {
    buf: String,
    delegate: &'a mut dyn Sink,
}

impl Sink for TrimSink<'_> {
    fn append(&mut self, sql: &str) {
        if sql.is_empty() {
            return;
        }
        if let Some(last) = self.buf.chars().last() {
            let first = sql.chars().next().unwrap_or(' ');
            if needs_separator(last, first) {
                self.buf.push(' ');
            }
        }
        self.buf.push_str(sql);
    }
}

fn needs_separator(last: char, first: char) -> bool {
    (ends_operand(last) && starts_identifier(first)) || (is_operator(last) && first == '(')
}

fn is_operator(c: char) -> bool {
    "=<>!+-*/%|&^".contains(c)
}

fn starts_identifier(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

fn ends_operand(c: char) -> bool {
    starts_identifier(c) || matches!(c, ')' | ']' | '}' | '?' | '\'' | '"' | '`')
}

struct PrefixedSink<'a> {
    delegate: &'a mut dyn Sink,
    prefix: String,
    applied: bool,
}

impl Sink for PrefixedSink<'_> {
    fn append(&mut self, sql: &str) {
        if !self.applied && !sql.trim().is_empty() {
            let sep = if self
                .prefix
                .chars()
                .last()
                .map(|c| c.is_alphanumeric())
                .unwrap_or(false)
            {
                format!("{} ", self.prefix)
            } else {
                self.prefix.clone()
            };
            self.delegate.append(&sep);
            self.applied = true;
        }
        self.delegate.append(sql);
    }
}

struct FilteredSink<'a> {
    delegate: &'a mut dyn Sink,
    item: Option<String>,
    index: Option<String>,
    unique: usize,
}

impl Sink for FilteredSink<'_> {
    fn append(&mut self, sql: &str) {
        let rewritten = parse_tokens(sql, "#{", "}", &mut |content| {
            let mut new_content = content.to_string();
            if let Some(item) = &self.item
                && let Some(rewritten) =
                    rewrite_name(content, item, &format!("__frch_{item}_{}", self.unique))
            {
                new_content = rewritten;
            }
            if new_content == content
                && let Some(index) = &self.index
                && let Some(rewritten) =
                    rewrite_name(content, index, &format!("__frch_{index}_{}", self.unique))
            {
                new_content = rewritten;
            }
            format!("#{{{new_content}}}")
        });
        self.delegate.append(rewritten.trim());
    }
}

/// Rewrites `item` / `item.x` to `__frch_item_N` at the start of a `#{}` body.
/// The name must be followed by `.`, `,`, `:`, whitespace or end-of-string
/// (so `itemName` is not touched).
fn rewrite_name(content: &str, name: &str, replacement: &str) -> Option<String> {
    let start = content
        .char_indices()
        .find(|(_, c)| !c.is_whitespace())
        .map(|(i, _)| i)?;
    let rest = &content[start..];
    if !rest.starts_with(name) {
        return None;
    }
    let after = start + name.len();
    let ok = match content[after..].chars().next() {
        None => true,
        Some(c) => matches!(c, '.' | ',' | ':') || c.is_whitespace(),
    };
    if !ok {
        return None;
    }
    Some(format!("{replacement}{}", &content[after..]))
}

/// Applies the tree and returns the un-tokenised SQL.
pub fn render_tree(node: &SqlNode, ctx: &mut RenderCtx) -> String {
    let mut root = RootSink { buf: String::new() };
    render_node(node, ctx, &mut root, "");
    root.buf.trim().to_string()
}

/// The id of a condition node: its position in the tree, dotted.
///
/// A path (not a running counter) is what makes the id stable: the collection
/// pass and the render pass walk the same tree, but the render pass skips the
/// contents of a false `<if>`, so a counter would drift apart between the two.
/// See [`collect_conditions`] for the segment scheme.
fn child_path(path: &str, segment: &str) -> String {
    if path.is_empty() {
        segment.to_string()
    } else {
        format!("{path}.{segment}")
    }
}

fn render_node(node: &SqlNode, ctx: &mut RenderCtx, sink: &mut dyn Sink, path: &str) {
    match node {
        SqlNode::Mixed(children) => {
            for (index, child) in children.iter().enumerate() {
                render_node(child, ctx, sink, &child_path(path, &index.to_string()));
            }
        }
        SqlNode::StaticText(text) => sink.append(text),
        SqlNode::Text(text) => render_text(text, ctx, sink),
        SqlNode::If { test, contents } => match condition_truth(path, test, ctx) {
            Some(true) => render_node(contents, ctx, sink, &child_path(path, "c")),
            Some(false) => {}
            None => {}
        },
        SqlNode::Choose { whens, otherwise } => {
            for (index, (test, contents)) in whens.iter().enumerate() {
                let branch = child_path(path, &format!("w{index}"));
                if condition_truth(&branch, test, ctx) == Some(true) {
                    render_node(contents, ctx, sink, &child_path(&branch, "c"));
                    return;
                }
            }
            if let Some(default) = otherwise {
                render_node(default, ctx, sink, &child_path(path, "o"));
            }
        }
        SqlNode::Bind { name, value } => match ognl::evaluate(value, &ctx.bindings) {
            Ok(v) => {
                ctx.bindings.insert(name.clone(), v);
            }
            Err(e) => {
                ctx.note(KEY_BIND_FAILED, format!("{name} = {value}: {}", e.detail));
                ctx.fail(e.message_key, e.detail);
            }
        },
        SqlNode::Trim(trim) => {
            let mut buffer = TrimSink {
                buf: String::new(),
                delegate: &mut *sink,
            };
            render_node(&trim.contents, ctx, &mut buffer, &child_path(path, "c"));
            let result = apply_trim(&buffer.buf, trim);
            buffer.delegate.append(&result);
        }
        SqlNode::Foreach(foreach) => render_foreach(foreach, ctx, sink, &child_path(path, "c")),
    }
}

/// The truth of one condition node: the page's switch when it set one, the
/// expression otherwise. `None` is the fail-closed answer (unsupported OGNL).
fn condition_truth(id: &str, test: &str, ctx: &mut RenderCtx) -> Option<bool> {
    if let Some(switched) = ctx.conditions.get(id) {
        return Some(*switched);
    }
    evaluate_test(test, ctx)
}

fn evaluate_test(test: &str, ctx: &mut RenderCtx) -> Option<bool> {
    match ognl::evaluate(test, &ctx.bindings) {
        Ok(value) => Some(ognl::is_true(&value)),
        Err(e) => {
            // Fail-closed: an unsupported expression is never treated as true.
            ctx.fail(e.message_key, format!("{test}: {}", e.detail));
            None
        }
    }
}

fn render_text(text: &str, ctx: &mut RenderCtx, sink: &mut dyn Sink) {
    let rendered = parse_tokens(text, "${", "}", &mut |content| {
        ctx.dollar_substitutions += 1;
        render_dollar(content, ctx)
    });
    sink.append(&rendered);
}

fn render_dollar(content: &str, ctx: &mut RenderCtx) -> String {
    let is_ew = is_ew_placeholder(content);
    match ognl::evaluate(content, &ctx.bindings) {
        Ok(Value::Null) => {
            if is_ew {
                ctx.partial = true;
                ctx.note(KEY_EW_MISSING, content);
            }
            String::new()
        }
        Ok(value) => value_to_sql(&value),
        Err(e) => {
            ctx.fail(e.message_key, format!("${{{content}}}: {}", e.detail));
            String::new()
        }
    }
}

/// `${ew.customSqlSegment}` / `${ew.sqlSelect}` / `${ew.sqlSet}`: a fragment
/// rendered elsewhere and spliced verbatim.
pub fn is_ew_placeholder(content: &str) -> bool {
    let trimmed = content.trim();
    let Some((var, kind)) = trimmed.split_once('.') else {
        return false;
    };
    if var.is_empty()
        || !var
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
    {
        return false;
    }
    matches!(kind, "customSqlSegment" | "sqlSelect" | "sqlSet")
}

pub fn value_to_sql(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn render_foreach(foreach: &ForeachNode, ctx: &mut RenderCtx, sink: &mut dyn Sink, path: &str) {
    let collection = match ctx.bindings.get(&foreach.collection_expression).cloned() {
        Some(v) => v,
        None => match ognl::evaluate(&foreach.collection_expression, &ctx.bindings) {
            Ok(v) => v,
            Err(e) => {
                ctx.partial = true;
                ctx.note(
                    KEY_FOREACH_NOT_ITERABLE,
                    format!("{}: {}", foreach.collection_expression, e.detail),
                );
                ctx.fail(e.message_key, e.detail);
                return;
            }
        },
    };
    let items: Vec<(Value, Value)> = match collection {
        Value::Null => {
            if !foreach.nullable {
                ctx.partial = true;
                ctx.note(KEY_FOREACH_NULL, foreach.collection_expression.clone());
            }
            return;
        }
        Value::Array(values) => values
            .into_iter()
            .enumerate()
            .map(|(i, v)| (Value::Number((i as i64).into()), v))
            .collect(),
        Value::Object(map) => map
            .into_iter()
            .map(|(k, v)| (Value::String(k), v))
            .collect(),
        other => {
            ctx.partial = true;
            ctx.note(
                KEY_FOREACH_NOT_ITERABLE,
                format!("{}: {}", foreach.collection_expression, type_label(&other)),
            );
            return;
        }
    };
    if items.is_empty() {
        return;
    }
    if let Some(open) = &foreach.open {
        sink.append(open);
    }
    let mut first = true;
    for (index, item) in items {
        let unique = ctx.unique;
        ctx.unique += 1;
        if let Some(name) = &foreach.index {
            ctx.bindings.insert(name.clone(), index.clone());
            ctx.bindings
                .insert(format!("__frch_{name}_{unique}"), index.clone());
        }
        if let Some(name) = &foreach.item {
            ctx.bindings.insert(name.clone(), item.clone());
            ctx.bindings
                .insert(format!("__frch_{name}_{unique}"), item.clone());
        }
        let separator = if first {
            None
        } else {
            foreach.separator.clone()
        };
        let mut prefixed = PrefixedSink {
            delegate: &mut *sink,
            prefix: separator.unwrap_or_default(),
            applied: false,
        };
        {
            let mut filtered = FilteredSink {
                delegate: &mut prefixed,
                item: foreach.item.clone(),
                index: foreach.index.clone(),
                unique,
            };
            render_node(&foreach.contents, ctx, &mut filtered, path);
        }
        if first {
            first = !prefixed.applied;
        }
    }
    if let Some(close) = &foreach.close {
        sink.append(close);
    }
    if let Some(name) = &foreach.item {
        ctx.bindings.remove(name);
    }
    if let Some(name) = &foreach.index {
        ctx.bindings.remove(name);
    }
}

fn type_label(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn apply_trim(buffer: &str, trim: &TrimNode) -> String {
    let mut sql = buffer.trim().to_string();
    let upper = sql.to_uppercase();
    if !upper.is_empty() {
        sql = apply_prefix(&sql, &upper, trim);
        sql = apply_suffix(&sql, &upper, trim);
    }
    sql
}

fn apply_prefix(sql: &str, upper: &str, trim: &TrimNode) -> String {
    let mut result = sql.to_string();
    for to_remove in &trim.prefix_overrides {
        let probe = to_remove.to_uppercase();
        if upper.starts_with(&probe) {
            let cut = to_remove.trim().chars().count();
            result = result.chars().skip(cut).collect();
            break;
        }
    }
    match &trim.prefix {
        Some(prefix) => format!("{prefix} {result}"),
        None => result,
    }
}

fn apply_suffix(sql: &str, upper: &str, trim: &TrimNode) -> String {
    let mut result = sql.to_string();
    for to_remove in &trim.suffix_overrides {
        let probe = to_remove.to_uppercase();
        let trimmed = to_remove.trim().to_uppercase();
        if upper.ends_with(&probe) || upper.ends_with(&trimmed) {
            let cut = to_remove.trim().chars().count();
            let keep = result.chars().count().saturating_sub(cut);
            result = result.chars().take(keep).collect();
            break;
        }
    }
    match &trim.suffix {
        Some(suffix) => format!("{result} {suffix}"),
        None => result,
    }
}

/// Converts `#{...}` to `?` and collects the bound values in SQL order.
pub fn parse_parameters(sql: &str, bindings: &BTreeMap<String, Value>) -> (String, Vec<Value>) {
    let (rendered, values, _) = parse_parameters_meta(sql, bindings);
    (rendered, values)
}

/// One `#{}` placeholder the render consumed, in SQL order (C06).
///
/// The name is the property as written (`id`, `user.name`) and the two types
/// are whatever the token declared (`#{id,jdbcType=BIGINT}`); `None` means the
/// statement did not say. The values themselves come back from the same call,
/// so `metas[i]` belongs to `values[i]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterMeta {
    pub name: String,
    pub jdbc_type: Option<String>,
    pub java_type: Option<String>,
}

/// [`parse_parameters`] plus the placeholder metadata the preview pane shows.
pub fn parse_parameters_meta(
    sql: &str,
    bindings: &BTreeMap<String, Value>,
) -> (String, Vec<Value>, Vec<ParameterMeta>) {
    let mut values = Vec::new();
    let mut metas = Vec::new();
    let rendered = parse_tokens(sql, "#{", "}", &mut |content| {
        let token = ParameterToken::parse(content);
        values.push(resolve_path(bindings, &token.property));
        let attribute = |name: &str| {
            token
                .attributes
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.clone())
        };
        metas.push(ParameterMeta {
            name: token.property.clone(),
            jdbc_type: attribute("jdbcType"),
            java_type: attribute("javaType"),
        });
        "?".to_string()
    });
    (rendered, values, metas)
}

/// How a toggleable node is tagged in the model (C07).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConditionKind {
    /// A standalone `<if>`.
    If,
    /// One `<when>` of a `<choose>`.
    When,
}

impl ConditionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::If => "if",
            Self::When => "when",
        }
    }
}

/// One condition switch the preview page draws (C07).
///
/// `id` is the node's position in the tree (dotted path: a `Mixed` child by
/// index, `c` for a node's contents, `wN` for the Nth `<when>`, `o` for an
/// `<otherwise>`). A path — not a counter — is what keeps the id stable between
/// the collection pass and a render pass that skips false branches. `label` is
/// the `test` expression, exactly what the reference's `XmlConditionView` shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConditionView {
    pub id: String,
    pub label: String,
    pub enabled: bool,
    pub kind: ConditionKind,
    /// The `<choose>` this branch belongs to (its own path), empty for `<if>`.
    /// The page can then say whose branches exclude each other.
    pub group: String,
}

/// The default truth of a `test` expression before anything is evaluated, the
/// reference's `defaultTruth`: a null check starts off, everything else starts
/// on. The preview's whole point is showing the statement's shape, so the
/// branches that add clauses are on until the reader says otherwise.
fn default_truth(test: &str) -> bool {
    !test.contains("== null")
}

/// Collects every condition switch of `node`, in document order (C07).
///
/// Defaults follow the reference: each `<if>` / `<when>` starts on unless its
/// test is a null check, and a `<choose>` with more than one branch on keeps
/// the first branch that carries a `#{}` parameter (otherwise the first) —
/// mutually exclusive branches must not all render at once.
pub fn collect_conditions(node: &SqlNode) -> Vec<ConditionView> {
    collect_conditions_with(node, &BTreeMap::new())
}

/// [`collect_conditions`] with the page's switches applied over the defaults.
pub fn collect_conditions_with(
    node: &SqlNode,
    switched: &BTreeMap<String, bool>,
) -> Vec<ConditionView> {
    let mut collector = Collector::default();
    collector.walk(node, "");
    collector.settle();
    for view in &mut collector.views {
        if let Some(enabled) = switched.get(&view.id) {
            view.enabled = *enabled;
        }
    }
    collector.views
}

#[derive(Default)]
struct Collector {
    views: Vec<ConditionView>,
    /// Per `<choose>`: `(view index, #{} count)` for each `<when>`, in branch
    /// order. Only groups with more than one branch are recorded.
    groups: Vec<Vec<(usize, usize)>>,
}

impl Collector {
    fn walk(&mut self, node: &SqlNode, path: &str) {
        match node {
            SqlNode::Mixed(children) => {
                for (index, child) in children.iter().enumerate() {
                    self.walk(child, &child_path(path, &index.to_string()));
                }
            }
            SqlNode::If { test, contents } => {
                self.push(path, test, ConditionKind::If, "");
                self.walk(contents, &child_path(path, "c"));
            }
            SqlNode::Choose { whens, otherwise } => {
                let group_id = path.to_string();
                let group = if whens.len() > 1 {
                    self.groups.push(Vec::new());
                    Some(self.groups.len() - 1)
                } else {
                    None
                };
                for (index, (test, contents)) in whens.iter().enumerate() {
                    let branch = child_path(path, &format!("w{index}"));
                    let view = self.push(&branch, test, ConditionKind::When, &group_id);
                    if let Some(group) = group {
                        self.groups[group].push((view, count_parameters(contents)));
                    }
                    self.walk(contents, &child_path(&branch, "c"));
                }
                if let Some(default) = otherwise {
                    self.walk(default, &child_path(path, "o"));
                }
            }
            SqlNode::Trim(trim) => self.walk(&trim.contents, &child_path(path, "c")),
            SqlNode::Foreach(foreach) => self.walk(&foreach.contents, &child_path(path, "c")),
            SqlNode::StaticText(_) | SqlNode::Text(_) | SqlNode::Bind { .. } => {}
        }
    }

    fn push(&mut self, path: &str, test: &str, kind: ConditionKind, group: &str) -> usize {
        self.views.push(ConditionView {
            id: path.to_string(),
            label: test.to_string(),
            enabled: default_truth(test),
            kind,
            group: group.to_string(),
        });
        self.views.len() - 1
    }

    /// Keeps one default branch per `<choose>`: the first that carries a
    /// `#{}` parameter, else the first. Switches the page sends are applied on
    /// top, so this only decides where the page starts.
    fn settle(&mut self) {
        for members in &self.groups {
            let on: Vec<usize> = members
                .iter()
                .map(|(index, _)| *index)
                .filter(|index| self.views[*index].enabled)
                .collect();
            if on.len() < 2 {
                continue;
            }
            let keep = members
                .iter()
                .find(|(index, count)| self.views[*index].enabled && *count > 0)
                .map(|(index, _)| *index)
                .unwrap_or(on[0]);
            for index in on {
                if index != keep {
                    self.views[index].enabled = false;
                }
            }
        }
    }
}

/// How many `#{}` placeholders a node subtree carries (used to pick a
/// `<choose>` default branch).
///
/// A `#{…}` does not make a text node dynamic (only `${…}` does), so the token
/// can sit in a `StaticText` node as well; both are scanned.
fn count_parameters(node: &SqlNode) -> usize {
    fn count_placeholders(text: &str) -> usize {
        let mut count = 0;
        let _ = parse_tokens(text, "#{", "}", &mut |_| {
            count += 1;
            String::new()
        });
        count
    }
    match node {
        SqlNode::Mixed(children) => children.iter().map(count_parameters).sum(),
        SqlNode::Text(text) | SqlNode::StaticText(text) => count_placeholders(text),
        SqlNode::If { contents, .. } => count_parameters(contents),
        SqlNode::Trim(trim) => count_parameters(&trim.contents),
        SqlNode::Foreach(foreach) => count_parameters(&foreach.contents),
        SqlNode::Choose { whens, otherwise } => {
            whens
                .iter()
                .map(|(_, contents)| count_parameters(contents))
                .sum::<usize>()
                + otherwise.as_deref().map(count_parameters).unwrap_or(0)
        }
        SqlNode::Bind { .. } => 0,
    }
}

/// Resolves `a.b`, `a[0].b`, `a['k']` against the binding map.
/// Missing links resolve to `null` (Map semantics), never to an error.
pub fn resolve_path(bindings: &BTreeMap<String, Value>, path: &str) -> Value {
    let path = path.trim();
    if path.is_empty() {
        return Value::Null;
    }
    let mut root_end = path.len();
    for (i, c) in path.char_indices() {
        if c == '.' || c == '[' {
            root_end = i;
            break;
        }
    }
    let root = &path[..root_end];
    let mut current = bindings.get(root).cloned().unwrap_or(Value::Null);
    let mut pos = root_end;
    while pos < path.len() {
        let rest = &path[pos..];
        if let Some(after_dot) = rest.strip_prefix('.') {
            let end = after_dot.find(['.', '[']).unwrap_or(after_dot.len());
            let name = &after_dot[..end];
            current = match &current {
                Value::Object(map) => map.get(name).cloned().unwrap_or(Value::Null),
                Value::Null => Value::Null,
                _ => Value::Null,
            };
            pos += 1 + end;
        } else if let Some(after_open) = rest.strip_prefix('[') {
            let Some(close) = after_open.find(']') else {
                return Value::Null;
            };
            let key = after_open[..close].trim();
            let key = key.trim_matches('\'').trim_matches('"');
            current = match &current {
                Value::Object(map) => map.get(key).cloned().unwrap_or(Value::Null),
                Value::Array(items) => match key.parse::<usize>() {
                    Ok(i) => items.get(i).cloned().unwrap_or(Value::Null),
                    Err(_) => Value::Null,
                },
                _ => Value::Null,
            };
            pos += 1 + close + 1;
        } else {
            break;
        }
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::xml::script::{SqlNode, build};
    use crate::core::xml::xmltree::parse_document;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn render(body: &str, params: Value) -> (String, Vec<Value>, RenderCtx) {
        let text = format!("<select id=\"s\">{body}</select>");
        let node = build(&parse_document(&text).unwrap()).unwrap();
        let mut ctx = RenderCtx::new(&params);
        let raw = render_tree(&node, &mut ctx);
        let (sql, values) = parse_parameters(&raw, &ctx.bindings);
        (sql, values, ctx)
    }

    #[test]
    fn plain_statement_keeps_parameter_order() {
        let (sql, values, _) = render(
            "SELECT * FROM t WHERE a = #{a} AND b = #{b}",
            json!({"a": 1, "b": "x"}),
        );
        assert_eq!(sql, "SELECT * FROM t WHERE a = ? AND b = ?");
        assert_eq!(values, vec![json!(1), json!("x")]);
    }

    #[test]
    fn if_expands_only_when_true() {
        let (sql, _, _) = render(
            "<if test=\"id != null\">WHERE id = #{id}</if>",
            json!({"id": 1}),
        );
        assert_eq!(sql, "WHERE id = ?");
        let (sql, _, _) = render("<if test=\"id != null\">WHERE id = #{id}</if>", json!({}));
        assert_eq!(sql, "");
    }

    #[test]
    fn where_strips_the_leading_and_with_mysql_like_spacing() {
        // The reference removes `AND` (trimmed length 3) and keeps the space
        // that followed it, so `WHERE` + one space + remainder has two spaces.
        let (sql, _, _) = render("<where> AND id = #{id} </where>", json!({"id": 1}));
        assert_eq!(sql, "WHERE  id = ?");
    }

    #[test]
    fn where_with_or_and_lowercase() {
        let (sql, _, _) = render("<where>or a = 1</where>", json!({}));
        assert_eq!(sql, "WHERE  a = 1");
    }

    #[test]
    fn where_empty_body_emits_nothing() {
        let (sql, _, _) = render("<where>  </where>", json!({}));
        assert_eq!(sql, "");
    }

    #[test]
    fn set_strips_trailing_comma() {
        let (sql, _, _) = render("<set>a = #{a}, b = #{b},</set>", json!({"a": 1, "b": 2}));
        assert_eq!(sql, "SET a = ?, b = ?");
    }

    #[test]
    fn set_empty_body_emits_nothing() {
        let (sql, _, _) = render("<set> </set>", json!({}));
        assert_eq!(sql, "");
    }

    #[test]
    fn custom_trim_prefix_and_suffix_overrides() {
        // `B x,` with prefixOverrides `A |B ` removes `B` (trimmed length 1),
        // leaving " x," and then drops the trailing comma.
        let (sql, _, _) = render(
            "<trim prefix=\"PREFIX\" prefixOverrides=\"A |B \" suffix=\"END\" suffixOverrides=\",\">B x,</trim>",
            json!({}),
        );
        assert_eq!(sql, "PREFIX  x END");
    }

    #[test]
    fn foreach_basic_expansion_and_frch_rewrite() {
        let (sql, values, _) = render(
            "<foreach collection=\"list\" item=\"x\" open=\"(\" separator=\",\" close=\")\">#{x}</foreach>",
            json!({"list": ["a", "b"]}),
        );
        assert_eq!(sql, "( ? , ? )");
        assert_eq!(values, vec![json!("a"), json!("b")]);
    }

    #[test]
    fn foreach_nested_property_and_name_prefix_guard() {
        let (_, values, _) = render(
            "<foreach collection=\"list\" item=\"x\" open=\"(\" separator=\",\" close=\")\">#{x.id}</foreach>",
            json!({"list": [{"id": 1}]}),
        );
        assert_eq!(values, vec![json!(1)]);
        let (sql, _, ctx) = render(
            "<foreach collection=\"list\" item=\"x\" open=\"(\" separator=\",\" close=\")\">#{xname}</foreach>",
            json!({"list": ["a"]}),
        );
        assert_eq!(sql, "( ? )");
        // `xname` was not rewritten: no binding exists, so the value is null.
        assert_eq!(ctx.bindings.get("xname"), None);
    }

    #[test]
    fn foreach_index_binding() {
        let (sql, values, _) = render(
            "<foreach collection=\"list\" item=\"x\" index=\"i\" separator=\",\">#{i}:#{x}</foreach>",
            json!({"list": ["a", "b"]}),
        );
        assert_eq!(sql, "?:? , ?:?");
        assert_eq!(values, vec![json!(0), json!("a"), json!(1), json!("b")]);
    }

    #[test]
    fn foreach_map_iterates_entries() {
        let (_, values, _) = render(
            "<foreach collection=\"m\" item=\"v\" index=\"k\" separator=\",\">#{k}=#{v}</foreach>",
            json!({"m": {"a": 1, "b": 2}}),
        );
        assert_eq!(values, vec![json!("a"), json!(1), json!("b"), json!(2)]);
    }

    #[test]
    fn foreach_empty_or_null_collection_emits_nothing() {
        let (sql, _, ctx) = render(
            "<foreach collection=\"list\" open=\"(\" close=\")\">#{x}</foreach>",
            json!({"list": []}),
        );
        assert_eq!(sql, "");
        assert!(!ctx.partial);
        let (sql, _, ctx) = render(
            "<foreach collection=\"list\" open=\"(\" close=\")\" item=\"x\">#{x}</foreach>",
            json!({}),
        );
        assert_eq!(sql, "");
        assert!(ctx.partial);
    }

    #[test]
    fn foreach_separator_is_lazy_when_first_item_renders_nothing() {
        // Only the second item passes the condition, so no separator is
        // emitted (the reference's `first` flag stays true until content).
        let (sql, values, _) = render(
            "<foreach collection=\"list\" item=\"x\" separator=\"or\" open=\"(\" close=\")\">\
             <if test=\"x &gt; 1\">#{x}</if></foreach>",
            json!({"list": [1, 2]}),
        );
        assert_eq!(sql, "( ? )");
        assert_eq!(values, vec![json!(2)]);
    }

    #[test]
    fn choose_picks_first_matching_branch_or_otherwise() {
        let body = "<choose><when test=\"a == 1\">A</when><when test=\"a == 2\">B</when><otherwise>C</otherwise></choose>";
        assert_eq!(render(body, json!({"a": 1})).0, "A");
        assert_eq!(render(body, json!({"a": 2})).0, "B");
        assert_eq!(render(body, json!({"a": 3})).0, "C");
    }

    #[test]
    fn bind_is_visible_to_later_tests_and_parameters() {
        let (sql, values, _) = render(
            "<bind name=\"pattern\" value=\"name\"/><if test=\"pattern != null\">#{pattern}</if>",
            json!({"name": "Ada"}),
        );
        assert_eq!(sql, "?");
        assert_eq!(values, vec![json!("Ada")]);
    }

    #[test]
    fn dollar_substitution_is_verbatim_and_null_is_empty() {
        let (sql, _, ctx) = render("SELECT ${col} FROM ${absent}", json!({"col": "id"}));
        assert_eq!(sql, "SELECT id FROM");
        assert_eq!(ctx.dollar_substitutions, 2);
        assert!(!ctx.partial);
    }

    #[test]
    fn escaped_dollar_is_left_alone() {
        let (sql, _, _) = render("SELECT \\${col} FROM t", json!({"col": "id"}));
        assert_eq!(sql, "SELECT \\${col} FROM t");
    }

    #[test]
    fn ew_segment_is_spliced_verbatim() {
        let (sql, _, ctx) = render(
            "SELECT * FROM t ${ew.customSqlSegment}",
            json!({"ew": {"customSqlSegment": "WHERE (status = 'A')"}}),
        );
        assert_eq!(sql, "SELECT * FROM t WHERE (status = 'A')");
        assert!(!ctx.partial);
    }

    #[test]
    fn missing_ew_segment_marks_partial_and_emits_nothing() {
        let (sql, _, ctx) = render("SELECT * FROM t ${ew.customSqlSegment}", json!({}));
        assert_eq!(sql, "SELECT * FROM t");
        assert!(ctx.partial);
        assert_eq!(ctx.notes[0].message_key, KEY_EW_MISSING);
    }

    #[test]
    fn unsupported_test_expression_is_unresolved_not_true() {
        let (_, _, ctx) = render(
            "<if test=\"@java.lang.Math@max(1,2) &gt; 0\">A</if>",
            json!({}),
        );
        let (key, _) = ctx.unresolved.expect("must be unresolved");
        assert_eq!(key, ognl::KEY_UNSUPPORTED);
    }

    #[test]
    fn unsupported_dollar_expression_is_unresolved() {
        let (_, _, ctx) = render("SELECT ${list.contains(1)}", json!({"list": [1]}));
        let (key, _) = ctx.unresolved.expect("must be unresolved");
        assert_eq!(key, ognl::KEY_UNSUPPORTED);
    }

    #[test]
    fn interior_whitespace_of_a_text_node_is_preserved() {
        let (sql, _, _) = render("SELECT a,b\nFROM t", json!({}));
        assert_eq!(sql, "SELECT a,b\nFROM t");
    }

    #[test]
    fn cdata_and_entities_reach_the_sql() {
        // CDATA is literal text (its `<` must not be parsed as markup), while
        // `&lt;` in plain text is decoded to `<`.
        let (sql, _, _) = render("<![CDATA[a < b]]> AND x &lt; 3", json!({}));
        assert_eq!(sql, "a < b  AND x < 3");
    }

    // C07: condition views and the switches the preview page sets ----------

    fn tree(body: &str) -> SqlNode {
        let text = format!("<select id=\"s\">{body}</select>");
        build(&parse_document(&text).unwrap()).unwrap()
    }

    /// Renders `body` with the page's switches, keyed by the `test` label.
    fn render_switched(body: &str, params: Value, switches: &[(&str, bool)]) -> String {
        let node = tree(body);
        let views = collect_conditions(&node);
        let mut map: BTreeMap<String, bool> = views
            .iter()
            .map(|view| (view.id.clone(), view.enabled))
            .collect();
        for (label, value) in switches {
            let view = views
                .iter()
                .find(|view| view.label == *label)
                .unwrap_or_else(|| panic!("no condition labelled {label}"));
            map.insert(view.id.clone(), *value);
        }
        let mut ctx = RenderCtx::new(&params);
        ctx.conditions = map;
        let raw = render_tree(&node, &mut ctx);
        parse_parameters(&raw, &ctx.bindings).0
    }

    #[test]
    fn conditions_are_numbered_by_position_and_labelled_by_test() {
        let node = tree(
            "WHERE 1=1 <if test=\"a != null\">AND a=1</if><choose><when test=\"b != null\">AND b=1</when><when test=\"c != null\">AND c=1</when><otherwise>AND d=1</otherwise></choose>",
        );
        let views = collect_conditions(&node);
        assert_eq!(views.len(), 3);
        assert_eq!(views[0].label, "a != null");
        assert_eq!(views[0].kind, ConditionKind::If);
        assert_eq!(views[0].group, "");
        // The path scheme: `<select>` is a Mixed, so its children are numbered
        // by index; the `<choose>` branches are `wN` under the choose's path.
        assert_eq!(views[0].id, "1");
        assert_eq!(views[1].id, "2.w0");
        assert_eq!(views[2].id, "2.w1");
        assert_eq!(views[1].kind, ConditionKind::When);
        assert_eq!(views[1].group, "2");
        assert_eq!(views[2].group, "2");
        // Ids are unique, which the reference's test-expression ids are not.
        let ids: std::collections::BTreeSet<&str> =
            views.iter().map(|view| view.id.as_str()).collect();
        assert_eq!(ids.len(), views.len());
    }

    #[test]
    fn a_null_check_starts_off_and_everything_else_starts_on() {
        let node = tree(
            "<if test=\"id != null\">A</if><if test=\"nickname == null\">B</if><if test=\"name\">C</if>",
        );
        let views = collect_conditions(&node);
        assert!(views[0].enabled);
        assert!(!views[1].enabled, "the reference's defaultTruth");
        assert!(views[2].enabled);
    }

    #[test]
    fn a_choose_keeps_one_default_branch_and_prefers_the_one_with_parameters() {
        let node = tree(
            "<choose><when test=\"kind != null\">AND kind = 1</when><when test=\"value != null\">AND value = #{value}</when><otherwise>AND 1 = 1</otherwise></choose>",
        );
        let views = collect_conditions(&node);
        assert_eq!(views.len(), 2, "an `<otherwise>` is not a condition");
        assert!(!views[0].enabled, "the branch without parameters loses");
        assert!(views[1].enabled);
    }

    #[test]
    fn a_switch_decides_its_branch_whatever_the_expression_says() {
        let body = "<if test=\"id != null\">WHERE id = #{id}</if>";
        // With an empty parameter map the expression is false, but the default
        // truth (the statement's shape) has the branch on.
        assert_eq!(render_switched(body, json!({}), &[]), "WHERE id = ?");
        // The page turns it off, and the branch disappears even though the
        // parameters would now satisfy it.
        assert_eq!(
            render_switched(body, json!({"id": 1}), &[("id != null", false)]),
            ""
        );
        // And a null check can be turned on against its expression.
        let body = "<if test=\"nickname == null\">WHERE nickname IS NULL</if>";
        assert_eq!(render_switched(body, json!({}), &[]), "");
        assert_eq!(
            render_switched(body, json!({}), &[("nickname == null", true)]),
            "WHERE nickname IS NULL"
        );
    }

    #[test]
    fn a_false_branch_does_not_desynchronise_the_ids_of_its_siblings() {
        // The render pass skips a false branch's contents, but the collection
        // pass walks them: a counter would drift, a path cannot. The sibling's
        // switch must keep matching its own node.
        let body = "<if test=\"a != null\"><if test=\"b != null\">A</if></if><if test=\"c != null\">C</if>";
        let node = tree(body);
        let views = collect_conditions(&node);
        let c = views
            .iter()
            .find(|view| view.label == "c != null")
            .expect("the sibling condition");
        let mut map: BTreeMap<String, bool> =
            views.iter().map(|v| (v.id.clone(), v.enabled)).collect();
        map.insert(c.id.clone(), false);
        let mut ctx = RenderCtx::new(&json!({}));
        ctx.conditions = map;
        assert_eq!(render_tree(&node, &mut ctx), "A");
    }

    #[test]
    fn parameter_metas_follow_the_sql_order_and_keep_their_declared_types() {
        let node = tree("WHERE a = #{a} AND b = #{b,jdbcType=VARCHAR,javaType=java.lang.String}");
        let mut ctx = RenderCtx::new(&json!({"a": 1, "b": "x"}));
        let raw = render_tree(&node, &mut ctx);
        let (sql, values, metas) = parse_parameters_meta(&raw, &ctx.bindings);
        assert_eq!(sql, "WHERE a = ? AND b = ?");
        assert_eq!(values, vec![json!(1), json!("x")]);
        assert_eq!(metas.len(), 2);
        assert_eq!(metas[0].name, "a");
        assert_eq!(metas[0].jdbc_type, None);
        assert_eq!(metas[1].name, "b");
        assert_eq!(metas[1].jdbc_type.as_deref(), Some("VARCHAR"));
        assert_eq!(metas[1].java_type.as_deref(), Some("java.lang.String"));
    }
}
