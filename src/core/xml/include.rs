//! `<include>` expansion (T1-3).
//!
//! Same shape as MyBatis' `XMLIncludeTransformer`: includes are inlined before
//! the node tree is built, so a fragment body may itself contain includes and
//! dynamic tags.
//!
//! - `refid` may contain `${name}` placeholders resolved from the include's own
//!   `<property name value/>` children;
//! - property substitution applies to text and attribute values of the copied
//!   fragment (`#{...}` is untouched);
//! - a bare refid looks in the current file first, then in the same namespace
//!   across the mapper set; a dotted `namespace.id` looks in the mapper set;
//! - a missing fragment leaves a `-- [mpsv] include not found: <refid>`
//!   placeholder and marks the result `partial`;
//! - a cycle (`A -> B -> A`) and nesting deeper than 20 are `unresolved`
//!   (`xml.include.cycle` / `xml.include.too_deep`).

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use super::Note;
use super::token::parse_tokens;
use super::xmltree::{Element, XmlNode};

pub const MAX_DEPTH: usize = 20;
pub const KEY_INCLUDE_NOT_FOUND: &str = "xml.include.not_found";
pub const KEY_INCLUDE_CYCLE: &str = "xml.include.cycle";
pub const KEY_INCLUDE_TOO_DEEP: &str = "xml.include.too_deep";

pub struct ExpandOutcome {
    pub partial: bool,
    pub notes: Vec<Note>,
    pub fatal: Option<(String, String)>,
}

struct ExpandCtx {
    partial: bool,
    notes: Vec<Note>,
    fatal: Option<(String, String)>,
}

/// Expands every include below `root` in place.
pub fn expand_includes(
    root: &mut Element,
    local: &BTreeMap<String, Element>,
    cross: &BTreeMap<String, Element>,
    namespace: Option<&str>,
) -> ExpandOutcome {
    let mut ctx = ExpandCtx {
        partial: false,
        notes: Vec::new(),
        fatal: None,
    };
    let chain = BTreeSet::new();
    root.children = expand_list(
        std::mem::take(&mut root.children),
        local,
        cross,
        namespace,
        0,
        &chain,
        &mut ctx,
    );
    ExpandOutcome {
        partial: ctx.partial,
        notes: ctx.notes,
        fatal: ctx.fatal,
    }
}

#[allow(clippy::too_many_arguments)]
fn expand_list(
    nodes: Vec<XmlNode>,
    local: &BTreeMap<String, Element>,
    cross: &BTreeMap<String, Element>,
    namespace: Option<&str>,
    depth: usize,
    chain: &BTreeSet<String>,
    ctx: &mut ExpandCtx,
) -> Vec<XmlNode> {
    let mut out = Vec::new();
    for node in nodes {
        match node {
            XmlNode::Element(element) if element.name == "include" => {
                expand_one(
                    element, local, cross, namespace, depth, chain, ctx, &mut out,
                );
            }
            XmlNode::Element(mut element) => {
                element.children = expand_list(
                    std::mem::take(&mut element.children),
                    local,
                    cross,
                    namespace,
                    depth,
                    chain,
                    ctx,
                );
                out.push(XmlNode::Element(element));
            }
            other => out.push(other),
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn expand_one(
    include: Element,
    local: &BTreeMap<String, Element>,
    cross: &BTreeMap<String, Element>,
    namespace: Option<&str>,
    depth: usize,
    chain: &BTreeSet<String>,
    ctx: &mut ExpandCtx,
    out: &mut Vec<XmlNode>,
) {
    if ctx.fatal.is_some() {
        return;
    }
    if depth >= MAX_DEPTH {
        ctx.fatal = Some((
            KEY_INCLUDE_TOO_DEEP.to_string(),
            format!("include nesting deeper than {MAX_DEPTH} (possible cycle)"),
        ));
        return;
    }
    let props = property_map(&include);
    let refid = resolve_refid(include.attr_or_empty("refid"), &props);
    if refid.is_empty() {
        ctx.partial = true;
        ctx.notes.push(Note {
            message_key: KEY_INCLUDE_NOT_FOUND.to_string(),
            detail: "<include> without a refid attribute".to_string(),
        });
        return;
    }
    if chain.contains(&refid) {
        ctx.fatal = Some((
            KEY_INCLUDE_CYCLE.to_string(),
            format!("include cycle detected: {refid}"),
        ));
        return;
    }
    let target = local.get(&refid).or_else(|| cross.get(&refid)).or_else(|| {
        namespace.and_then(|ns| {
            if refid.contains('.') {
                None
            } else {
                cross.get(&format!("{ns}.{refid}"))
            }
        })
    });
    let Some(target) = target else {
        ctx.partial = true;
        ctx.notes.push(Note {
            message_key: KEY_INCLUDE_NOT_FOUND.to_string(),
            detail: refid.clone(),
        });
        out.push(XmlNode::Text(format!(
            "-- [mpsv] include not found: {refid}"
        )));
        return;
    };
    let mut next_chain = chain.clone();
    next_chain.insert(refid.clone());
    let mut copies: Vec<XmlNode> = target
        .children
        .iter()
        .map(|node| {
            let mut copy = node.clone();
            apply_properties(&mut copy, &props);
            copy
        })
        .collect();
    copies = expand_list(copies, local, cross, namespace, depth + 1, &next_chain, ctx);
    out.extend(copies);
}

fn property_map(include: &Element) -> BTreeMap<String, String> {
    let mut props = BTreeMap::new();
    for child in include.child_elements() {
        if child.name == "property" {
            let name = child.attr_or_empty("name");
            if !name.is_empty() {
                props.insert(name.to_string(), child.attr_or_empty("value").to_string());
            }
        }
    }
    props
}

pub(crate) fn resolve_refid(refid: &str, props: &BTreeMap<String, String>) -> String {
    parse_tokens(refid, "${", "}", &mut |name| {
        props
            .get(name)
            .cloned()
            .unwrap_or_else(|| format!("${{{name}}}"))
    })
}

fn apply_properties(node: &mut XmlNode, props: &BTreeMap<String, String>) {
    match node {
        XmlNode::Text(text) => *text = replace_props(text, props),
        XmlNode::Element(element) => {
            for (_, value) in element.attrs.iter_mut() {
                *value = replace_props(value, props);
            }
            for child in element.children.iter_mut() {
                apply_properties(child, props);
            }
        }
        XmlNode::Comment(_) => {}
    }
}

fn replace_props(text: &str, props: &BTreeMap<String, String>) -> String {
    parse_tokens(text, "${", "}", &mut |name| {
        props
            .get(name)
            .cloned()
            .unwrap_or_else(|| format!("${{{name}}}"))
    })
}

/// Collects the `<sql id>` fragments of one mapper document.
pub fn collect_fragments(root: &Element) -> BTreeMap<String, Element> {
    let mut fragments = BTreeMap::new();
    for child in root.child_elements() {
        if child.name == "sql" {
            let id = child.attr_or_empty("id");
            if !id.is_empty() {
                fragments.insert(id.to_string(), child.clone());
            }
        }
    }
    fragments
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::xml::xmltree::parse_document;

    fn expand(xml: &str) -> ExpandOutcome {
        let mut root = parse_document(xml).unwrap();
        let local = collect_fragments(&parse_document(xml).unwrap());
        let namespace = root.attr("namespace").map(|s| s.to_string());
        expand_includes(&mut root, &local, &BTreeMap::new(), namespace.as_deref())
    }

    #[test]
    fn same_file_include_is_inlined_with_properties() {
        let mut root = parse_document(
            "<mapper namespace=\"ns\"><sql id=\"cols\">${alias}.a, ${alias}.b</sql>\
             <select id=\"s\">SELECT <include refid=\"cols\"><property name=\"alias\" value=\"t\"/></include> FROM t</select></mapper>",
        )
        .unwrap();
        let local = collect_fragments(
            &parse_document(
                "<mapper namespace=\"ns\"><sql id=\"cols\">${alias}.a, ${alias}.b</sql></mapper>",
            )
            .unwrap(),
        );
        let outcome = expand_includes(&mut root, &local, &BTreeMap::new(), Some("ns"));
        assert!(!outcome.partial);
        assert!(outcome.fatal.is_none());
        let statement = root.child_elements().find(|e| e.name == "select").unwrap();
        assert!(statement.text_content().contains("t.a, t.b"));
        assert!(!statement.text_content().contains("include"));
    }

    #[test]
    fn missing_include_leaves_a_placeholder_and_partial() {
        let outcome = expand(
            "<mapper><select id=\"s\">SELECT <include refid=\"nope\"/> FROM t</select></mapper>",
        );
        assert!(outcome.partial);
        assert_eq!(outcome.notes[0].message_key, KEY_INCLUDE_NOT_FOUND);
    }

    #[test]
    fn cycle_is_fatal() {
        let xml = "<mapper namespace=\"n\">\
                   <sql id=\"a\"><include refid=\"b\"/></sql>\
                   <sql id=\"b\"><include refid=\"a\"/></sql>\
                   <select id=\"s\">SELECT <include refid=\"a\"/></select></mapper>";
        let mut root = parse_document(xml).unwrap();
        let local = collect_fragments(&parse_document(xml).unwrap());
        let outcome = expand_includes(&mut root, &local, &BTreeMap::new(), Some("n"));
        assert_eq!(outcome.fatal.unwrap().0, KEY_INCLUDE_CYCLE);
    }

    #[test]
    fn refid_property_substitution() {
        let mut props = BTreeMap::new();
        props.insert("which".to_string(), "cols".to_string());
        assert_eq!(resolve_refid("${which}", &props), "cols");
        assert_eq!(resolve_refid("${absent}", &props), "${absent}");
    }

    #[test]
    fn fragment_collection_keeps_ids() {
        let root = parse_document("<mapper><sql id=\"a\">x</sql><sql>x</sql></mapper>").unwrap();
        let fragments = collect_fragments(&root);
        assert_eq!(fragments.len(), 1);
        assert!(fragments.contains_key("a"));
    }
}
