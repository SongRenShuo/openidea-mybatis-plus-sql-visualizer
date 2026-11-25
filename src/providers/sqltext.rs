//! SQL regions of a document, for injections, inlay hints and diagnostics.
//!
//! A region is one statement (mapper XML element, or one annotated method) and
//! the byte spans of the SQL text inside it. Attribute values, tag names and
//! comments never become spans, which is why injections can safely colour
//! these ranges only.

use crate::indexer::scan;
use crate::indexer::{StatementKind, index_java};

/// One statement's SQL text spans (byte offsets into the document).
pub(crate) struct Region {
    /// The statement kind; `None` for a `<sql>` fragment, which is SQL text
    /// without a statement of its own.
    pub kind: Option<StatementKind>,
    pub spans: Vec<(usize, usize)>,
}

/// The SQL text of a mapper XML document, or of a Java file's annotated
/// methods. Non-mapper XML yields nothing.
pub(crate) fn regions(text: &str) -> Vec<Region> {
    let elements = scan::elements(text);
    if let Some(root) = elements.iter().find(|e| e.parent.is_none())
        && root.name == "mapper"
    {
        return xml_regions(text, &elements);
    }
    java_regions(text)
}

fn xml_regions(text: &str, elements: &[scan::Element<'_>]) -> Vec<Region> {
    let Some(root) = elements.iter().position(|e| e.parent.is_none()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for element in elements {
        if element.parent != Some(root) {
            continue;
        }
        let kind = match StatementKind::from_tag(element.name) {
            Some(kind) => Some(kind),
            // A `<sql>` fragment is SQL text too, but it is not a statement.
            None if element.name == "sql" => None,
            None => continue,
        };
        let spans = scan::text_nodes(text, element.content_start, element.content_end)
            .into_iter()
            .map(|(start, end)| scan::trim_span(text, start, end))
            .filter(|(start, end)| start < end)
            .collect::<Vec<_>>();
        if !spans.is_empty() {
            out.push(Region { kind, spans });
        }
    }
    out
}

fn java_regions(text: &str) -> Vec<Region> {
    let facts = index_java("", text);
    let mut out = Vec::new();
    for method in &facts.methods {
        for sql in &method.sql {
            if !sql.strings.is_empty() {
                out.push(Region {
                    kind: Some(sql.kind),
                    spans: sql.strings.clone(),
                });
            }
        }
    }
    out
}

/// Case-insensitive search of an ASCII needle.
pub(crate) fn find_ci(text: &str, needle: &str, from: usize) -> Option<usize> {
    let hay = text.as_bytes();
    let needle = needle.as_bytes();
    if needle.is_empty() || from >= hay.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle))
        .map(|i| from + i)
}

/// Ends just past the word starting at `i` (identifier characters and dots).
pub(crate) fn word_end(text: &str, i: usize) -> usize {
    let bytes = text.as_bytes();
    let mut j = i;
    while j < bytes.len()
        && (bytes[j].is_ascii_alphanumeric() || matches!(bytes[j], b'_' | b'$' | b'.'))
    {
        j += 1;
    }
    j
}

pub(crate) fn skip_ws(text: &str, i: usize) -> usize {
    let bytes = text.as_bytes();
    let mut j = i;
    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
        j += 1;
    }
    j
}

/// The bare value of an identifier: backticks, double quotes or brackets
/// removed.
pub(crate) fn unquote(name: &str) -> &str {
    let name = name.trim();
    for (open, close) in [('`', '`'), ('"', '"'), ('[', ']')] {
        if let Some(rest) = name.strip_prefix(open)
            && let Some(inner) = rest.strip_suffix(close)
        {
            return inner.trim();
        }
    }
    name
}

/// Byte span of an identifier starting at `i`, including its quoting.
pub(crate) fn ident_span(text: &str, i: usize) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    if i >= bytes.len() {
        return None;
    }
    match bytes[i] {
        b'`' | b'"' => {
            let close = bytes[i + 1..].iter().position(|&b| b == bytes[i])?;
            Some((i, i + 1 + close + 1))
        }
        b'[' => {
            let close = bytes[i + 1..].iter().position(|&b| b == b']')?;
            Some((i, i + 1 + close + 1))
        }
        _ => {
            let end = word_end(text, i);
            (end > i).then_some((i, end))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = "<mapper namespace=\"n\">\n  <select id=\"a\">\n    select id,\n    <if test=\"x &gt; 1\">name</if>\n    from t\n  </select>\n</mapper>";

    #[test]
    fn xml_regions_are_trimmed_text_nodes_without_attributes() {
        let regions = regions(XML);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].kind, Some(StatementKind::Select));
        let texts: Vec<&str> = regions[0].spans.iter().map(|&(s, e)| &XML[s..e]).collect();
        assert_eq!(texts, vec!["select id,", "name", "from t"]);
    }

    #[test]
    fn java_regions_come_from_sql_annotations() {
        let text = "package a;\ninterface M {\n  @Insert(\"insert into t (id) values (#{id})\")\n  int add(Integer id);\n}\n";
        let regions = regions(text);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].kind, Some(StatementKind::Insert));
        assert_eq!(
            &text[regions[0].spans[0].0..regions[0].spans[0].1],
            "insert into t (id) values (#{id})"
        );
    }

    #[test]
    fn a_non_mapper_root_has_no_regions() {
        assert!(regions("<beans><select id=\"x\">select 1</select></beans>").is_empty());
    }

    #[test]
    fn helpers_unquote_and_find() {
        assert_eq!(unquote("`id`"), "id");
        assert_eq!(unquote("[id]"), "id");
        assert_eq!(unquote("\"id\""), "id");
        assert_eq!(find_ci("INSERT INTO t", "insert", 0), Some(0));
        assert_eq!(ident_span("`a`.`b`", 0), Some((0, 3)));
    }
}
