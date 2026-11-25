//! iBatis 2.x tag-tree renderer.
//!
//! The state machine follows `DynamicSql.process`:
//! `doStartFragment → processBodyChildren → doEndFragment → doPrepend`, with
//! `REPEAT_BODY` driving `<iterate>`. Markers implement
//! `removeFirstPrepend`, so `<dynamic prepend="WHERE">` swallows the first
//! participating child's `prepend` instead of producing `WHERE AND …`.

use std::collections::BTreeMap;

use serde_json::Value;

use super::super::xml::{Confidence, Note, RenderOutput};
use super::token;
use super::{
    IbatisIssue, IbatisModel, IbatisNode, IbatisStatement, IbatisTag, IbatisText, KEY_DEGRADED,
    KEY_ITERATE_NOT_COLLECTION, KEY_PARAMETER_MAP_UNKNOWN, TagKind, apply_namespace,
};

const INCLUDE_BODY: i32 = 0;
const SKIP_BODY: i32 = 1;
const REPEAT_BODY: i32 = 2;

const NOT_COMPARABLE: i64 = i64::MIN;

struct IterateCtx {
    items: Vec<Value>,
    index: isize,
    property: String,
    parent: Option<usize>,
    allow_next: bool,
    is_final: bool,
    some_sub_elements_have_content: bool,
    is_prepend_enabled: bool,
}

impl IterateCtx {
    fn has_next(&self) -> bool {
        (self.index + 1) >= 0 && ((self.index + 1) as usize) < self.items.len()
    }

    fn is_last(&self) -> bool {
        (self.index + 1) as usize >= self.items.len()
    }
}

#[derive(Default)]
struct TagState {
    /// `(tag address, removeFirstPrepend)` with the last entry as the top.
    markers: Vec<(usize, bool)>,
    iterates: Vec<IterateCtx>,
    /// tag address → index into `iterates`.
    attributes: BTreeMap<usize, usize>,
    iterate_stack: Vec<usize>,
}

enum RenderError {
    IterateNotCollection {
        property: Option<String>,
        detail: String,
    },
    Other(String),
}

struct Renderer<'a> {
    params: &'a Value,
    model: &'a IbatisModel,
    state: TagState,
    /// Parameter names in SQL order.
    mappings: Vec<String>,
    warnings: Vec<IbatisIssue>,
    partial: bool,
}

pub fn render_statement(
    statement: &IbatisStatement,
    model: &IbatisModel,
    params: &Value,
) -> RenderOutput {
    let mut renderer = Renderer {
        params,
        model,
        state: TagState::default(),
        mappings: Vec::new(),
        warnings: Vec::new(),
        partial: false,
    };
    let sql = if statement.is_dynamic() {
        renderer.render_dynamic(statement)
    } else {
        renderer.render_static(statement)
    };

    let dollars = token::process_dollar(&sql, params);
    for warning in dollars.warnings {
        renderer.warn("ibatis.dollar.unclosed", warning);
    }
    let values: Vec<Value> = renderer
        .mappings
        .iter()
        .map(|name| token::lookup(params, name).unwrap_or(Value::Null))
        .collect();
    let mut notes: Vec<Note> = renderer
        .warnings
        .iter()
        .map(|w| Note {
            message_key: w.message_key.clone(),
            detail: w.detail.clone(),
        })
        .collect();
    notes.extend(statement.issues.iter().map(|i| Note {
        message_key: i.message_key.clone(),
        detail: i.detail.clone(),
    }));
    let partial = renderer.partial || !statement.issues.is_empty() || !renderer.warnings.is_empty();
    let mut message_keys = Vec::new();
    for note in &notes {
        if !message_keys.contains(&note.message_key) {
            message_keys.push(note.message_key.clone());
        }
    }
    RenderOutput {
        sql: dollars.text,
        params: values,
        confidence: if partial {
            Confidence::Partial
        } else {
            Confidence::Exact
        },
        notes,
        message_keys,
    }
}

impl Renderer<'_> {
    fn warn(&mut self, key: &str, detail: impl Into<String>) {
        self.warnings.push(IbatisIssue {
            message_key: key.to_string(),
            detail: detail.into(),
        });
    }

    fn set_partial(&mut self) {
        self.partial = true;
    }

    // ------------------------------------------------------------- static path

    fn render_static(&mut self, statement: &IbatisStatement) -> String {
        let raw: String = statement
            .body
            .iter()
            .filter_map(|n| match n {
                IbatisNode::Text(t) => Some(t.raw.clone()),
                _ => None,
            })
            .collect();
        if let Some(map_id) = &statement.parameter_map {
            let declared = self.model.parameter_maps.get(map_id);
            let namespaced = apply_namespace(map_id, self.model.namespace.as_deref());
            let declared = declared.or_else(|| self.model.parameter_maps.get(&namespaced));
            match declared {
                Some(map) => {
                    let sites = count_parameter_sites(&raw);
                    for parameter in map.parameters.iter().take(sites) {
                        self.mappings.push(parameter.property.clone());
                    }
                    // A declared parameterMap keeps `#` verbatim; only CR/LF are
                    // normalised (tabs survive in this branch).
                    return raw.replace(['\r', '\n'], " ");
                }
                None => {
                    self.warn(KEY_PARAMETER_MAP_UNKNOWN, map_id.clone());
                    self.set_partial();
                }
            }
        }
        let normalized = normalize_all(&raw);
        let parsed = token::parse_inline(&normalized);
        for warning in parsed.warnings {
            self.warn("ibatis.inline.malformed", warning);
        }
        for mapping in parsed.mappings {
            self.mappings.push(mapping.property);
        }
        parsed.text
    }

    // ------------------------------------------------------------ dynamic path

    fn render_dynamic(&mut self, statement: &IbatisStatement) -> String {
        let mut out = String::new();
        for node in &statement.body {
            let result = match node {
                IbatisNode::Text(text) => self.append_text(text, &mut out),
                IbatisNode::Tag(tag) => self.process_tag(tag, &mut out),
            };
            if let Err(error) = result {
                let detail = match &error {
                    RenderError::IterateNotCollection { property, detail } => {
                        self.warn(
                            KEY_ITERATE_NOT_COLLECTION,
                            format!("{}: {detail}", property.clone().unwrap_or_default()),
                        );
                        detail.clone()
                    }
                    RenderError::Other(detail) => detail.clone(),
                };
                self.warn(KEY_DEGRADED, detail);
                self.set_partial();
                // Top-level text-node fallback, exactly like the reference.
                let fallback: String = statement
                    .body
                    .iter()
                    .filter_map(|n| match n {
                        IbatisNode::Text(t) => Some(t.raw.clone()),
                        _ => None,
                    })
                    .collect();
                return normalize_all(&fallback);
            }
        }
        out
    }

    fn process_children(
        &mut self,
        children: &[IbatisNode],
        out: &mut String,
    ) -> Result<(), RenderError> {
        for child in children {
            match child {
                IbatisNode::Text(text) => self.append_text(text, out)?,
                IbatisNode::Tag(tag) => self.process_tag(tag, out)?,
            }
        }
        Ok(())
    }

    fn append_text(&mut self, text: &IbatisText, out: &mut String) -> Result<(), RenderError> {
        let normalized = normalize_all(&text.raw);
        if is_blank(&normalized) {
            out.push_str(&normalized);
            return Ok(());
        }
        let mut body = normalized;
        if text.post_parse {
            if let Some(index) = self.state.iterate_stack.last().copied() {
                if self.state.iterates[index].allow_next {
                    self.advance(index);
                }
                body = self.iterate_property_replace(&body, index);
            }
        }
        let parsed = token::parse_inline(&body);
        for warning in parsed.warnings {
            self.warn("ibatis.inline.malformed", warning);
        }
        for mapping in parsed.mappings {
            self.mappings.push(mapping.property);
        }
        out.push_str(&parsed.text);
        Ok(())
    }

    fn advance(&mut self, index: usize) {
        let iterate = &mut self.state.iterates[index];
        iterate.index += 1;
        iterate.allow_next = false;
        if !iterate.has_next() {
            iterate.is_final = true;
        }
    }

    fn process_tag(&mut self, tag: &IbatisTag, out: &mut String) -> Result<(), RenderError> {
        let tag_id = tag as *const IbatisTag as usize;
        let mut response;
        loop {
            let mut body = String::new();
            response = self.do_start_fragment(tag, tag_id)?;
            if response != SKIP_BODY {
                self.process_children(&tag.children, &mut body)?;
                response = self.do_end_fragment(tag, tag_id, &mut body)?;
                self.do_prepend(tag, tag_id, &mut body);
                if response != SKIP_BODY && !body.is_empty() {
                    out.push_str(&body);
                }
            }
            if response != REPEAT_BODY {
                break;
            }
        }
        self.pop_marker(tag_id);
        if tag.kind == TagKind::Iterate {
            if let Some(index) = self.state.attributes.remove(&tag_id) {
                if self.state.iterate_stack.last().copied() == Some(index) {
                    self.state.iterate_stack.pop();
                } else if let Some(position) =
                    self.state.iterate_stack.iter().position(|i| *i == index)
                {
                    self.state.iterate_stack.remove(position);
                }
            }
        }
        Ok(())
    }

    fn do_start_fragment(&mut self, tag: &IbatisTag, tag_id: usize) -> Result<i32, RenderError> {
        match tag.kind {
            TagKind::Dynamic => {
                self.push_marker(tag_id, tag.rfp_initial);
                Ok(INCLUDE_BODY)
            }
            TagKind::Iterate => self.start_iterate(tag, tag_id),
            _ => {
                self.push_marker(tag_id, tag.rfp_initial);
                if self.is_condition(tag)? {
                    Ok(INCLUDE_BODY)
                } else {
                    Ok(SKIP_BODY)
                }
            }
        }
    }

    fn do_end_fragment(
        &mut self,
        tag: &IbatisTag,
        tag_id: usize,
        body: &mut String,
    ) -> Result<i32, RenderError> {
        if tag.kind == TagKind::Iterate {
            return self.end_iterate(tag, tag_id, body);
        }
        if tag.kind != TagKind::Dynamic {
            if let Some(index) = self.state.iterate_stack.last().copied() {
                if self.state.iterates[index].allow_next {
                    self.advance(index);
                }
            }
        }
        Ok(base_end_fragment(tag, body))
    }

    fn do_prepend(&mut self, tag: &IbatisTag, tag_id: usize, body: &mut String) {
        if tag.kind == TagKind::Iterate {
            if let Some(index) = self.state.attributes.get(&tag_id).copied() {
                if self.state.iterates[index].is_prepend_enabled {
                    self.base_prepend(tag, body);
                    self.state.iterates[index].is_prepend_enabled = false;
                }
            }
            return;
        }
        self.base_prepend(tag, body);
    }

    fn base_prepend(&mut self, tag: &IbatisTag, body: &mut String) {
        if tag.has_open() && tag.kind != TagKind::Iterate && has_content(body) {
            body.insert_str(0, tag.open.as_deref().unwrap_or(""));
        }
        if tag.has_prepend() && has_content(body) {
            if self.peek_parent_marker() {
                self.disable_parent_marker();
            } else {
                body.insert_str(0, tag.prepend.as_deref().unwrap_or(""));
            }
        }
    }

    fn start_iterate(&mut self, tag: &IbatisTag, tag_id: usize) -> Result<i32, RenderError> {
        if let Some(index) = self.state.attributes.get(&tag_id).copied() {
            if tag.remove_first_prepend.as_deref() == Some("iterate") {
                self.re_enable_self_marker();
            }
            return Ok(if self.state.iterates[index].has_next() {
                INCLUDE_BODY
            } else {
                SKIP_BODY
            });
        }
        let parent = self.state.iterate_stack.last().copied();
        self.push_marker(tag_id, tag.rfp_initial);
        let property = tag.property.clone().unwrap_or_default();
        let resolved_property = if property.is_empty() {
            property.clone()
        } else if let Some(parent_index) = parent {
            if self.state.iterates[parent_index].allow_next {
                self.advance(parent_index);
            }
            self.add_index_to_tag_property(parent_index, &property)
        } else {
            property.clone()
        };
        let collection = if property.is_empty() {
            self.params.clone()
        } else {
            token::lookup(self.params, &resolved_property).unwrap_or(Value::Null)
        };
        let items = match collection {
            Value::Array(values) => values,
            Value::Null => {
                return Err(RenderError::IterateNotCollection {
                    property: tag.property.clone(),
                    detail: "iterate property value is null".to_string(),
                });
            }
            other => {
                return Err(RenderError::IterateNotCollection {
                    property: tag.property.clone(),
                    detail: format!("not a collection: {}", type_label(&other)),
                });
            }
        };
        self.state.iterates.push(IterateCtx {
            items,
            index: -1,
            property: resolved_property,
            parent,
            allow_next: true,
            is_final: false,
            some_sub_elements_have_content: false,
            is_prepend_enabled: false,
        });
        let index = self.state.iterates.len() - 1;
        self.state.attributes.insert(tag_id, index);
        self.state.iterate_stack.push(index);
        Ok(if self.state.iterates[index].has_next() {
            INCLUDE_BODY
        } else {
            SKIP_BODY
        })
    }

    fn end_iterate(
        &mut self,
        tag: &IbatisTag,
        tag_id: usize,
        body: &mut String,
    ) -> Result<i32, RenderError> {
        let Some(index) = self.state.attributes.get(&tag_id).copied() else {
            return Err(RenderError::Other("iterate context missing".to_string()));
        };
        if !self.state.iterates[index].has_next() && !self.state.iterates[index].is_final {
            return Ok(base_end_fragment(tag, body));
        }
        if self.state.iterates[index].allow_next {
            self.advance(index);
        }
        if has_content(body) {
            if self.state.iterates[index].some_sub_elements_have_content {
                if tag.has_conjunction() {
                    body.insert_str(0, tag.conjunction.as_deref().unwrap_or(""));
                }
            } else {
                self.state.iterates[index].is_prepend_enabled = true;
                if tag.has_open() {
                    body.insert_str(0, tag.open.as_deref().unwrap_or(""));
                }
            }
            self.state.iterates[index].some_sub_elements_have_content = true;
        }
        if self.state.iterates[index].is_last()
            && self.state.iterates[index].some_sub_elements_have_content
            && tag.has_close()
        {
            body.push_str(tag.close.as_deref().unwrap_or(""));
        }
        self.state.iterates[index].allow_next = true;
        Ok(if self.state.iterates[index].is_final {
            base_end_fragment(tag, body)
        } else {
            REPEAT_BODY
        })
    }

    // ------------------------------------------------------------- conditions

    fn is_condition(&mut self, tag: &IbatisTag) -> Result<bool, RenderError> {
        let params = self.params;
        Ok(match tag.kind {
            TagKind::IsNull => self.is_null_value(tag),
            TagKind::IsNotNull => !self.is_null_value(tag),
            TagKind::IsEmpty => self.is_empty_value(tag),
            TagKind::IsNotEmpty => !self.is_empty_value(tag),
            TagKind::IsPropertyAvailable => self.is_property_available(tag),
            TagKind::IsNotPropertyAvailable => !self.is_property_available(tag),
            TagKind::IsParameterPresent => !params.is_null(),
            TagKind::IsNotParameterPresent => params.is_null(),
            TagKind::IsEqual => self.compare_tag(tag) == 0,
            TagKind::IsNotEqual => self.compare_tag(tag) != 0,
            TagKind::IsGreaterThan => self.compare_tag(tag) > 0,
            TagKind::IsGreaterEqual => self.compare_tag(tag) >= 0,
            TagKind::IsLessThan => self.compare_tag(tag) < 0,
            TagKind::IsLessEqual => self.compare_tag(tag) <= 0,
            TagKind::Dynamic | TagKind::Iterate => true,
        })
    }

    fn is_null_value(&mut self, tag: &IbatisTag) -> bool {
        if self.params.is_null() {
            return true;
        }
        match self.resolve_property(tag) {
            Some(property) => token::lookup(self.params, &property)
                .map(|v| v.is_null())
                .unwrap_or(true),
            None => self.params.is_null(),
        }
    }

    fn is_empty_value(&mut self, tag: &IbatisTag) -> bool {
        if self.params.is_null() {
            return true;
        }
        let value = match self.resolve_property(tag) {
            Some(property) => token::lookup(self.params, &property).unwrap_or(Value::Null),
            None => self.params.clone(),
        };
        match value {
            Value::Null => true,
            Value::String(s) => s.is_empty(),
            Value::Array(items) => items.is_empty(),
            Value::Object(map) => map.is_empty(),
            _ => false,
        }
    }

    fn is_property_available(&mut self, tag: &IbatisTag) -> bool {
        if self.params.is_null() {
            return false;
        }
        // Maps take the raw attribute name, bypassing property probing.
        if let Value::Object(map) = self.params {
            let raw = tag.property.clone().unwrap_or_default();
            if raw.is_empty() {
                return false;
            }
            return map.contains_key(&raw);
        }
        match self.resolve_property(tag) {
            Some(property) => token::lookup(self.params, &property).is_some(),
            None => false,
        }
    }

    fn compare_tag(&mut self, tag: &IbatisTag) -> i64 {
        let property = self.resolve_property(tag);
        let value1 = match &property {
            Some(property) => token::lookup(self.params, property).unwrap_or(Value::Null),
            None => self.params.clone(),
        };
        let value2 = match (&tag.compare_property, &tag.compare_value) {
            (Some(property), _) => token::lookup(self.params, property).unwrap_or(Value::Null),
            (None, Some(literal)) => Value::String(literal.clone()),
            (None, None) => {
                self.warn(
                    "ibatis.condition.no_compare_target",
                    format!(
                        "<{}> has neither compareProperty nor compareValue",
                        tag.kind.tag_name()
                    ),
                );
                return NOT_COMPARABLE;
            }
        };
        compare_values(&value1, &value2)
    }

    fn resolve_property(&mut self, tag: &IbatisTag) -> Option<String> {
        let property = tag.property.clone()?;
        let iterate = self.state.iterate_stack.last().copied();
        if let Some(index) = iterate {
            if self.state.iterates[index].allow_next {
                self.advance(index);
            }
        }
        if property.contains('[') {
            if let Some(index) = iterate {
                return Some(self.add_index_to_tag_property(index, &property));
            }
        }
        Some(property)
    }

    // ------------------------------------------------------- iterate property

    fn iterate_property_replace(&mut self, text: &str, index: usize) -> String {
        let mut buffer: Vec<char> = text.chars().collect();
        for marker in ['#', '$'] {
            let mut start: isize = 0;
            let mut end: isize = -1;
            while start > -1 && (start as usize) < buffer.len() {
                start = find_char(&buffer, marker, (end + 1) as usize)
                    .map(|i| i as isize)
                    .unwrap_or(-1);
                end = if start > -1 {
                    find_char(&buffer, marker, (start + 1) as usize)
                        .map(|i| i as isize)
                        .unwrap_or(-1)
                } else {
                    -1
                };
                if start > -1 && end > -1 {
                    let inner: String = buffer[(start + 1) as usize..end as usize].iter().collect();
                    let replacement = self.add_index_to_tag_property(index, &inner);
                    buffer.splice((start + 1) as usize..end as usize, replacement.chars());
                }
            }
        }
        buffer.into_iter().collect()
    }

    fn add_index_to_tag_property(&self, index: usize, tag_property: &str) -> String {
        self.process_tag_property(index, tag_property).0
    }

    fn process_tag_property(&self, index: usize, tag_property: &str) -> (String, usize) {
        match self.state.iterates[index].parent {
            Some(parent) => {
                let (text, at) = self.process_tag_property(parent, tag_property);
                self.add_index(index, &text, at)
            }
            None => self.add_index(index, tag_property, 0),
        }
    }

    fn add_index(&self, index: usize, input: &str, start_index: usize) -> (String, usize) {
        let end_property = format!("{}[", self.end_property(index));
        let from = start_index.min(input.len());
        let Some(relative) = input[from..].find(&end_property) else {
            return (input.to_string(), 0);
        };
        let property_index = from + relative;
        let after = property_index + end_property.len();
        if after >= input.len() || input.as_bytes()[after] != b']' {
            return (input.to_string(), 0);
        }
        let replacement = self.state.iterates[index].index;
        let out = format!("{}{}{}", &input[..after], replacement, &input[after..]);
        (out, after)
    }

    fn end_property(&self, index: usize) -> String {
        let iterate = &self.state.iterates[index];
        let Some(parent) = iterate.parent else {
            return iterate.property.clone();
        };
        let parent_property = &self.state.iterates[parent].property;
        match iterate.property.find(parent_property) {
            Some(ppi) => {
                let end1 = iterate.property[ppi..]
                    .find(']')
                    .map(|x| (ppi + x) as isize)
                    .unwrap_or(-1);
                let end2 = iterate.property[ppi..]
                    .find('.')
                    .map(|x| (ppi + x) as isize)
                    .unwrap_or(-1);
                let cut = ppi as isize + end1.max(end2) + 1;
                let cut = if cut < 0 { 0 } else { cut as usize };
                iterate.property.get(cut..).unwrap_or("").to_string()
            }
            None => iterate.property.clone(),
        }
    }

    // ----------------------------------------------------------------- markers

    fn push_marker(&mut self, tag_id: usize, rfp: bool) {
        self.state.markers.push((tag_id, rfp));
    }

    fn pop_marker(&mut self, tag_id: usize) {
        if let Some((top_id, _)) = self.state.markers.last() {
            if *top_id == tag_id {
                self.state.markers.pop();
            }
        }
    }

    fn peek_parent_marker(&self) -> bool {
        let len = self.state.markers.len();
        if len > 1 {
            self.state.markers[len - 2].1
        } else {
            false
        }
    }

    fn disable_parent_marker(&mut self) {
        let len = self.state.markers.len();
        if len > 1 {
            self.state.markers[len - 2].1 = false;
        }
    }

    fn re_enable_self_marker(&mut self) {
        if let Some(top) = self.state.markers.last_mut() {
            top.1 = true;
        }
    }
}

fn base_end_fragment(tag: &IbatisTag, body: &mut String) -> i32 {
    if tag.has_close() && tag.kind != TagKind::Iterate && has_content(body) {
        body.push_str(tag.close.as_deref().unwrap_or(""));
    }
    INCLUDE_BODY
}

fn find_char(buffer: &[char], target: char, from: usize) -> Option<usize> {
    if from >= buffer.len() {
        return None;
    }
    buffer[from..]
        .iter()
        .position(|c| *c == target)
        .map(|i| from + i)
}

/// Java `String.trim()` emptiness: code points `<= ' '`.
pub fn is_blank(text: &str) -> bool {
    text.chars().all(|c| c <= ' ')
}

fn has_content(body: &str) -> bool {
    !is_blank(body)
}

fn normalize_all(raw: &str) -> String {
    raw.replace(['\r', '\n', '\t'], " ")
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

fn as_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// `ConditionalTagHandler.compareValues`: null-vs-null is 0, any other
/// null pair is `NOT_COMPARABLE` (a negative value, so `<` / `<=` become true
/// for incomparable operands — the engine quirk is kept).
pub fn compare_values(left: &Value, right: &Value) -> i64 {
    if left.is_null() || right.is_null() {
        return if left.is_null() && right.is_null() {
            0
        } else {
            NOT_COMPARABLE
        };
    }
    if let (Some(a), Some(b)) = (as_number(left), as_number(right)) {
        return if a < b {
            -1
        } else if a > b {
            1
        } else {
            0
        };
    }
    let a = value_text(left);
    let b = value_text(right);
    match a.cmp(&b) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Null => String::new(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Parameter sites of the statement body: string-aware `?` plus inline `#x#`
/// pairs (used by the declared-`parameterMap` branch).
pub fn count_parameter_sites(text: &str) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let mut in_string = false;
    let mut question_marks = 0usize;
    let mut hashes = 0usize;
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            if c == '\'' {
                if i + 1 < chars.len() && chars[i + 1] == '\'' {
                    i += 2;
                    continue;
                }
                in_string = false;
            }
        } else {
            match c {
                '\'' => in_string = true,
                '?' => question_marks += 1,
                '#' => {
                    if i + 1 < chars.len() && chars[i + 1] == '#' {
                        i += 2;
                        continue;
                    }
                    hashes += 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    question_marks + hashes / 2
}

#[cfg(test)]
mod tests {
    use super::super::{IbatisModel, KEY_INCLUDE_NOT_FOUND, parse};
    use super::*;
    use serde_json::json;

    fn map_body() -> &'static str {
        r#"<sqlMap namespace="User">
            <sql id="cols">id, name</sql>

            <select id="static1">SELECT <include refid="cols"/> FROM users WHERE id = #id#</select>

            <select id="static2">SELECT * FROM $table$ WHERE name = #name:VARCHAR#</select>

            <select id="dyn1">
                SELECT * FROM users
                <dynamic prepend="WHERE">
                    <isNotNull property="name" prepend="AND">name = #name#</isNotNull>
                </dynamic>
            </select>

            <select id="dyn2">
                SELECT * FROM users
                <dynamic prepend="WHERE">
                    <isNotEmpty property="ids" prepend="AND">id IN
                        <iterate property="ids" open="(" close=")" conjunction=",">#ids[]#</iterate>
                    </isNotEmpty>
                </dynamic>
            </select>

            <select id="conds">
                SELECT * FROM users WHERE 1 = 1
                <isNull property="a" prepend="AND">a IS NULL</isNull>
                <isNotNull property="b" prepend="AND">b IS NOT NULL</isNotNull>
                <isEmpty property="c" prepend="AND">c EMPTY</isEmpty>
                <isNotEmpty property="d" prepend="AND">d NOT EMPTY</isNotEmpty>
                <isEqual property="age" compareValue="18" prepend="AND">age = 18</isEqual>
                <isNotEqual property="nam" compareValue="x" prepend="AND">nam &lt;&gt; x</isNotEqual>
                <isGreaterThan property="age" compareValue="10" prepend="AND">age &gt; 10</isGreaterThan>
                <isGreaterEqual property="age" compareValue="10" prepend="AND">age &gt;= 10</isGreaterEqual>
                <isLessThan property="age" compareValue="10" prepend="AND">age &lt; 10</isLessThan>
                <isLessEqual property="age" compareValue="10" prepend="AND">age &lt;= 10</isLessEqual>
                <isPropertyAvailable property="known" prepend="AND">has known</isPropertyAvailable>
                <isNotPropertyAvailable property="absent" prepend="AND">no absent</isNotPropertyAvailable>
                <isParameterPresent prepend="AND">param present</isParameterPresent>
                <isNotParameterPresent prepend="AND">param absent</isNotParameterPresent>
            </select>

            <select id="compareProp">
                SELECT * FROM t
                <dynamic prepend="WHERE">
                    <isEqual property="kind" compareProperty="expected" prepend="AND">same</isEqual>
                </dynamic>
            </select>

            <insert id="insertUser" parameterMap="userMap">
                INSERT INTO users (#id#, #name#)
                <selectKey keyProperty="id">SELECT MAX(id) + 1 FROM users</selectKey>
            </insert>

            <parameterMap id="userMap" class="User">
                <parameter property="id" jdbcType="INTEGER"/>
                <parameter property="name" jdbcType="VARCHAR"/>
            </parameterMap>
        </sqlMap>"#
    }

    fn model() -> IbatisModel {
        parse(map_body()).unwrap()
    }

    fn render(id: &str, params: Value) -> RenderOutput {
        let model = model();
        let statement = model.statements.get(id).unwrap();
        render_statement(statement, &model, &params)
    }

    fn compact(sql: &str) -> String {
        sql.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn static_statement_expands_include_and_hashes() {
        let out = render("static1", json!({"id": 5}));
        assert_eq!(out.confidence, Confidence::Exact);
        assert_eq!(compact(&out.sql), "SELECT id, name FROM users WHERE id = ?");
        assert_eq!(out.params, vec![json!(5)]);
    }

    #[test]
    fn static_statement_dollar_and_colon_syntax() {
        let out = render("static2", json!({"table": "users", "name": "Ada"}));
        assert_eq!(compact(&out.sql), "SELECT * FROM users WHERE name = ?");
        assert_eq!(out.params, vec![json!("Ada")]);
    }

    #[test]
    fn dollar_null_is_dropped() {
        let out = render("static2", json!({"table": null, "name": "Ada"}));
        assert_eq!(compact(&out.sql), "SELECT * FROM WHERE name = ?");
    }

    #[test]
    fn dynamic_prepend_is_composed_and_child_prepend_eaten() {
        let out = render("dyn1", json!({"name": "Ada"}));
        assert_eq!(out.confidence, Confidence::Exact);
        assert_eq!(compact(&out.sql), "SELECT * FROM users WHERE name = ?");
        assert_eq!(out.params, vec![json!("Ada")]);
    }

    #[test]
    fn dynamic_branch_is_skipped_when_property_is_null() {
        let out = render("dyn1", json!({}));
        assert_eq!(compact(&out.sql), "SELECT * FROM users");
        assert!(out.params.is_empty());
    }

    #[test]
    fn is_empty_branch_and_iterate() {
        let out = render("dyn2", json!({"ids": [7, 8]}));
        assert_eq!(compact(&out.sql), "SELECT * FROM users WHERE id IN (?,?)");
        assert_eq!(out.params, vec![json!(7), json!(8)]);
    }

    #[test]
    fn is_empty_skips_an_empty_collection() {
        let out = render("dyn2", json!({"ids": []}));
        assert_eq!(compact(&out.sql), "SELECT * FROM users");
        assert!(out.params.is_empty());
    }

    #[test]
    fn iterate_without_open_close_uses_conjunction() {
        let xml = r#"<sqlMap><select id="s">SELECT * FROM t WHERE id IN <iterate property="ids" conjunction=",">#ids[]#</iterate></select></sqlMap>"#;
        let out = super::super::render(xml, "s", &json!({"ids": [1, 2, 3]}));
        assert_eq!(compact(&out.sql), "SELECT * FROM t WHERE id IN ?,?,?");
        assert_eq!(out.params, vec![json!(1), json!(2), json!(3)]);
    }

    #[test]
    fn iterate_on_a_missing_collection_degrades_with_a_note() {
        let xml = r#"<sqlMap><select id="s">SELECT * FROM t WHERE id IN <iterate property="ids" open="(" close=")" conjunction=",">#ids[]#</iterate></select></sqlMap>"#;
        let out = super::super::render(xml, "s", &json!({}));
        assert!(
            out.message_keys
                .contains(&KEY_ITERATE_NOT_COLLECTION.to_string())
        );
        assert_eq!(out.confidence, Confidence::Partial);
    }

    #[test]
    fn is_null_and_is_not_null() {
        let out = render("conds", json!({"age": 10, "known": 1, "nam": "y"}));
        assert!(out.sql.contains("a IS NULL"));
        assert!(!out.sql.contains("b IS NOT NULL"));
    }

    #[test]
    fn is_empty_and_is_not_empty() {
        let out = render(
            "conds",
            json!({"age": 10, "c": "", "d": ["x"], "known": 1, "nam": "y"}),
        );
        assert!(out.sql.contains("c EMPTY"));
        assert!(out.sql.contains("d NOT EMPTY"));
        let out = render(
            "conds",
            json!({"age": 10, "c": "x", "d": [], "known": 1, "nam": "y"}),
        );
        assert!(!out.sql.contains("c EMPTY"));
        assert!(!out.sql.contains("d NOT EMPTY"));
    }

    #[test]
    fn comparison_tags() {
        let out = render("conds", json!({"age": 10, "known": 1, "nam": "y"}));
        assert!(out.sql.contains("age = 18") == false);
        assert!(out.sql.contains("nam <> x"));
        assert!(out.sql.contains("age > 10") == false);
        assert!(out.sql.contains("age >= 10"));
        assert!(out.sql.contains("age < 10") == false);
        assert!(out.sql.contains("age <= 10"));
    }

    #[test]
    fn comparison_on_integer_literals_uses_numeric_order() {
        // age 9 vs 10: lexicographic would say "9" > "10"; numeric says less.
        let out = render("conds", json!({"age": 9, "known": 1, "nam": "y"}));
        assert!(out.sql.contains("age < 10"));
        assert!(!out.sql.contains("age > 10"));
        assert!(!out.sql.contains("age >= 10"));
    }

    #[test]
    fn property_available_tags() {
        let out = render("conds", json!({"age": 10, "known": 1, "nam": "y"}));
        assert!(out.sql.contains("has known"));
        assert!(out.sql.contains("no absent"));
    }

    #[test]
    fn parameter_present_tags() {
        let out = render("conds", json!({"age": 10, "known": 1, "nam": "y"}));
        assert!(out.sql.contains("param present"));
        assert!(!out.sql.contains("param absent"));
    }

    #[test]
    fn compare_property_reads_the_second_value() {
        let out = render("compareProp", json!({"kind": "a", "expected": "a"}));
        assert!(out.sql.contains("same"));
        let out = render("compareProp", json!({"kind": "a", "expected": "b"}));
        assert!(!out.sql.contains("same"));
    }

    #[test]
    fn declared_parameter_map_keeps_hashes_and_binds_positionally() {
        let out = render("insertUser", json!({"id": 1, "name": "Ada"}));
        assert_eq!(compact(&out.sql), "INSERT INTO users (#id#, #name#)");
        assert_eq!(out.params, vec![json!(1), json!("Ada")]);
        assert!(!out.sql.contains("MAX(id)"));
    }

    #[test]
    fn missing_include_is_partial_with_placeholder() {
        let xml =
            r#"<sqlMap><select id="s">SELECT <include refid="gone"/> FROM t</select></sqlMap>"#;
        let out = super::super::render(xml, "s", &json!({}));
        assert_eq!(out.confidence, Confidence::Partial);
        assert!(out.sql.contains("-- [mpsv] include not found: gone"));
        assert!(
            out.message_keys
                .contains(&KEY_INCLUDE_NOT_FOUND.to_string())
        );
    }

    #[test]
    fn unknown_tag_is_partial_but_content_is_kept() {
        let xml = r#"<sqlMap><select id="s">A <weird>B</weird> C</select></sqlMap>"#;
        let out = super::super::render(xml, "s", &json!({}));
        assert_eq!(out.confidence, Confidence::Partial);
        assert_eq!(compact(&out.sql), "A B C");
        assert!(out.message_keys.contains(&"ibatis.unknown_tag".to_string()));
    }

    #[test]
    fn iterate_single_element_has_no_conjunction() {
        let xml = r#"<sqlMap><select id="s">SELECT * FROM t WHERE id IN <iterate property="ids" open="(" close=")" conjunction=",">#ids[]#</iterate></select></sqlMap>"#;
        let out = super::super::render(xml, "s", &json!({"ids": [2]}));
        assert_eq!(compact(&out.sql), "SELECT * FROM t WHERE id IN (?)");
        assert_eq!(out.params, vec![json!(2)]);
    }

    #[test]
    fn missing_statement_is_unresolved() {
        let out = super::super::render(map_body(), "nope", &json!({}));
        assert_eq!(out.confidence, Confidence::Unresolved);
        assert_eq!(out.sql, "");
    }

    #[test]
    fn invalid_root_is_unresolved() {
        let out = super::super::render("<mapper/>", "s", &json!({}));
        assert_eq!(out.confidence, Confidence::Unresolved);
        assert_eq!(out.message_keys[0], "ibatis.root_invalid");
    }

    #[test]
    fn bare_include_in_a_namespaced_file_is_found() {
        let xml = r#"<sqlMap namespace="N"><sql id="cols">a, b</sql><select id="s">SELECT <include refid="cols"/> FROM t</select></sqlMap>"#;
        let out = super::super::render(xml, "s", &json!({}));
        assert_eq!(compact(&out.sql), "SELECT a, b FROM t");
    }

    #[test]
    fn dotted_refid_is_reported_as_missing() {
        // The reference engine registers fragments under their raw id
        // (`useStatementNamespaces=false`), so a dotted refid is not found.
        let xml = r#"<sqlMap namespace="N"><sql id="cols">a, b</sql><select id="s">SELECT <include refid="N.cols"/> FROM t</select></sqlMap>"#;
        let out = super::super::render(xml, "s", &json!({}));
        assert_eq!(out.confidence, Confidence::Partial);
        assert!(out.sql.contains("-- [mpsv] include not found: N.cols"));
    }

    #[test]
    fn static_text_newlines_become_spaces() {
        let xml = "<sqlMap><select id=\"s\">SELECT a,\n  b\tFROM t</select></sqlMap>";
        let out = super::super::render(xml, "s", &json!({}));
        assert_eq!(out.sql, "SELECT a,   b FROM t");
    }

    #[test]
    fn count_parameter_sites_ignores_strings_and_escapes() {
        assert_eq!(count_parameter_sites("a = ? AND b = ?"), 2);
        assert_eq!(count_parameter_sites("a = '?' AND b = ?"), 1);
        assert_eq!(count_parameter_sites("x = #a# AND ## AND y = #b#"), 2);
        assert_eq!(count_parameter_sites("a = 'it''s ?' OR b = ?"), 1);
    }

    #[test]
    fn not_comparable_is_negative_so_less_than_matches() {
        assert!(compare_values(&json!(null), &json!(1)) < 0);
        assert_eq!(compare_values(&json!(null), &json!(null)), 0);
        assert!(compare_values(&json!(2), &json!(10)) < 0);
        assert!(compare_values(&json!("b"), &json!("a")) > 0);
    }
}
