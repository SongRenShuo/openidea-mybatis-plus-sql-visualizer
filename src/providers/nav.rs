//! `definition` and `line_markers` for mapper XML and Java (T3-2).
//!
//! Both are name-level and same-document: the provider only sees one document
//! and must answer inside the editing budget, so it never searches the project.
//! Cross-file jumps (XML statement ↔ Java method, a fragment in another file)
//! come from the document index built by [`crate::indexer`], which the host
//! resolves from memory.
//!
//! Off by default (`nav.enabled = false`): the official MyBatis navigator and
//! the host's own SQL attributes overlap here, and a user who installed the
//! plugin for rendering should not get gutters and ⌘B targets unasked.

use crate::indexer::scan::{self, Positions};
use crate::indexer::{index_java, index_xml};
use openidea_plugin::providers::{Document, LineMarker, Providers};
use openidea_plugin::{Location, Position, Range};

pub const SETTING: &str = "nav.enabled";

impl Providers for crate::Main {
    fn definition(&mut self, doc: &Document, at: Position) -> Vec<Location> {
        if !super::setting_on(SETTING) {
            return Vec::new();
        }
        definition_in(doc, at)
    }

    fn line_markers(&mut self, doc: &Document) -> Vec<LineMarker> {
        if !super::setting_on(SETTING) {
            return Vec::new();
        }
        markers_in(doc)
    }
}

/// Same-document targets at `at`, whichever mapper language the document is.
pub fn definition_in(doc: &Document, at: Position) -> Vec<Location> {
    let Some(offset) = openidea_plugin::text::offset(&doc.text, at) else {
        return Vec::new();
    };
    if super::is_xml(&doc.path) {
        xml_definition(doc, offset)
    } else if super::is_java(&doc.path) {
        java_definition(doc, at)
    } else {
        Vec::new()
    }
}

fn location(doc: &Document, range: Range) -> Location {
    Location {
        path: doc.path.clone(),
        range,
    }
}

fn inside(attr: &scan::Attr<'_>, offset: usize) -> bool {
    attr.value_start <= offset && offset < attr.value_end
}

fn xml_definition(doc: &Document, offset: usize) -> Vec<Location> {
    let text = &doc.text;
    let facts = index_xml(&doc.path, text);
    let elements = scan::elements(text);
    let mut out = Vec::new();
    for element in &elements {
        match element.name {
            "include" => {
                if let Some(referenced) = value_at(element, "refid", offset)
                    && let Some(fragment) = facts.local_fragment(referenced)
                {
                    out.push(location(doc, fragment.range));
                }
            }
            "resultMap" => {
                for name in ["extends", "resultMap"] {
                    if let Some(referenced) = value_at(element, name, offset)
                        && let Some(result_map) = facts.local_result_map(referenced)
                    {
                        out.push(location(doc, result_map.range));
                    }
                }
            }
            "association" | "collection" | "case" => {
                if let Some(referenced) = value_at(element, "resultMap", offset)
                    && let Some(result_map) = facts.local_result_map(referenced)
                {
                    out.push(location(doc, result_map.range));
                }
            }
            other => {
                if crate::indexer::StatementKind::from_tag(other).is_some()
                    && let Some(referenced) = value_at(element, "resultMap", offset)
                    && let Some(result_map) = facts.local_result_map(referenced)
                {
                    out.push(location(doc, result_map.range));
                }
            }
        }
    }
    out
}

/// The value of attribute `name` when the caret sits in it.
fn value_at<'a>(element: &scan::Element<'a>, name: &str, offset: usize) -> Option<&'a str> {
    let attr = element.attr(name)?;
    inside(attr, offset).then_some(attr.value)
}

fn java_definition(doc: &Document, at: Position) -> Vec<Location> {
    let facts = index_java(&doc.path, &doc.text);
    let mut out = Vec::new();
    for method in &facts.methods {
        for reference in &method.result_maps {
            if contains(&reference.range, at) {
                // `@ResultMap("x")` → the `@Results(id = "x")` in this file.
                for other in &facts.methods {
                    for id in &other.result_ids {
                        if id.name == reference.name {
                            out.push(location(doc, id.range));
                        }
                    }
                }
            }
        }
    }
    out
}

fn contains(range: &Range, at: Position) -> bool {
    range.start <= at && at < range.end
}

/// Gutter markers for the XML document: statement rows that jump to the
/// fragment or result map they use, and fragment/result-map rows that jump to
/// the first statement using them — all within this file.
pub fn markers_in(doc: &Document) -> Vec<LineMarker> {
    if !super::is_xml(&doc.path) {
        return Vec::new();
    }
    let facts = index_xml(&doc.path, &doc.text);
    let mut out: Vec<LineMarker> = Vec::new();
    let mut lines: Vec<u32> = Vec::new();
    let mut push = |marker: LineMarker| {
        if !lines.contains(&marker.line) {
            lines.push(marker.line);
            out.push(marker);
        }
    };

    for statement in &facts.statements {
        let Some(id) = &statement.id else { continue };
        let mut target: Option<(&str, &crate::indexer::Named)> = None;
        if let Some(result_map) = &statement.result_map
            && let Some(found) = facts.local_result_map(&result_map.name)
        {
            target = Some(("result map", found));
        }
        if target.is_none() {
            for include in &statement.includes {
                if let Some(found) = facts.local_fragment(&include.name) {
                    target = Some(("sql fragment", found));
                    break;
                }
            }
        }
        if let Some((kind, found)) = target {
            push(LineMarker {
                line: id.range.start.line,
                icon: Some("navigate".to_owned()),
                tooltip: format!("MyBatis: go to {kind} {}", found.name),
                target: Some(location(doc, found.range)),
            });
        }
    }

    // Reverse: the fragment and result-map lines point at their first user.
    for fragment in &facts.fragments {
        let user = facts.statements.iter().find(|statement| {
            statement
                .includes
                .iter()
                .any(|include| refers_to(include.name.as_str(), &fragment.name, &facts.namespace))
        });
        if let Some(user) = user
            && let Some(id) = &user.id
        {
            push(LineMarker {
                line: fragment.range.start.line,
                icon: Some("navigate".to_owned()),
                tooltip: format!("MyBatis: used by statement {}", id.name),
                target: Some(location(doc, id.range)),
            });
        }
    }
    for result_map in &facts.result_maps {
        let Some(id) = &result_map.id else { continue };
        let user = facts.statements.iter().find(|statement| {
            statement
                .result_map
                .as_ref()
                .is_some_and(|reference| refers_to(&reference.name, &id.name, &facts.namespace))
        });
        if let Some(user) = user
            && let Some(user_id) = &user.id
        {
            push(LineMarker {
                line: id.range.start.line,
                icon: Some("navigate".to_owned()),
                tooltip: format!("MyBatis: used by statement {}", user_id.name),
                target: Some(location(doc, user_id.range)),
            });
        }
    }
    out
}

fn refers_to(reference: &str, local: &str, namespace: &Option<crate::indexer::Named>) -> bool {
    match (reference.rsplit_once('.'), namespace) {
        (Some((ns, id)), Some(namespace)) => id == local && ns == namespace.name,
        _ => reference == local,
    }
}

/// UTF-16 range of a byte span, for callers that keep offsets.
pub fn range_of(text: &str, start: usize, end: usize) -> Range {
    let mut positions = Positions::new(text);
    positions.range(start, end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    use openidea_plugin::providers::Document;

    const XML: &str = "<mapper namespace=\"com.acme.UserMapper\">\n  <sql id=\"cols\">id, name</sql>\n  <select id=\"findById\" resultMap=\"BaseMap\">\n    select <include refid=\"cols\"/> from t_user\n  </select>\n  <resultMap id=\"BaseMap\" type=\"com.acme.User\">\n    <id column=\"id\" property=\"id\"/>\n  </resultMap>\n</mapper>";

    fn doc(text: &str) -> Document {
        Document {
            path: "/p/mapper/UserMapper.xml".into(),
            language: "xml".into(),
            version: 1,
            text: text.into(),
        }
    }

    fn at(text: &str, needle: &str) -> Position {
        let offset = text.find(needle).expect("needle in text") + 1;
        openidea_plugin::text::position(text, offset)
    }

    #[test]
    fn nothing_is_answered_while_the_setting_is_off() {
        let _host = MockHost::install();
        let mut plugin = crate::Main;
        let document = doc(XML);
        let refid = at(XML, "cols\"/>");
        assert!(Providers::definition(&mut plugin, &document, refid).is_empty());
        assert!(Providers::line_markers(&mut plugin, &document).is_empty());
    }

    #[test]
    fn definition_resolves_includes_and_result_maps() {
        let host = MockHost::install();
        host.set_setting(SETTING, "true");
        let mut plugin = crate::Main;
        let document = doc(XML);

        let include = Providers::definition(&mut plugin, &document, at(XML, "cols\"/>"));
        assert_eq!(include.len(), 1);
        assert_eq!(
            &XML.lines().nth(1).unwrap()[include[0].range.start.character as usize..][..4],
            "cols"
        );

        let result_map = Providers::definition(&mut plugin, &document, at(XML, "BaseMap\">"));
        assert_eq!(result_map.len(), 1);
        assert_eq!(result_map[0].range.start.line, 5);

        // A word that is neither a reference nor resolvable answers nothing.
        let nowhere = Providers::definition(&mut plugin, &document, at(XML, "namespace"));
        assert!(nowhere.is_empty());
    }

    #[test]
    fn markers_go_both_ways_within_the_file() {
        let host = MockHost::install();
        host.set_setting(SETTING, "true");
        let mut plugin = crate::Main;
        let markers = Providers::line_markers(&mut plugin, &doc(XML));
        assert!(!markers.is_empty());
        assert_eq!(markers.len(), 3, "statement, fragment and result map rows");
        let statement_line = markers
            .iter()
            .find(|m| m.line == 2)
            .expect("statement marker");
        // The statement names a result map, which wins over its include.
        assert_eq!(statement_line.target.as_ref().unwrap().range.start.line, 5);
        let fragment_line = markers
            .iter()
            .find(|m| m.line == 1)
            .expect("fragment marker");
        assert_eq!(fragment_line.target.as_ref().unwrap().range.start.line, 2);
        let map_line = markers
            .iter()
            .find(|m| m.line == 5)
            .expect("result map marker");
        assert_eq!(map_line.target.as_ref().unwrap().range.start.line, 2);
    }

    #[test]
    fn java_result_map_reference_finds_the_results_id() {
        let host = MockHost::install();
        host.set_setting(SETTING, "true");
        let mut plugin = crate::Main;
        let text = "package a;\npublic interface M {\n  @Results(id = \"m1\") @ResultMap(\"m1\")\n  java.util.List<X> all();\n}\n";
        let document = Document {
            path: "/p/M.java".into(),
            language: "java".into(),
            version: 1,
            text: text.into(),
        };
        let inside_result_map =
            openidea_plugin::text::position(text, text.rfind("m1").unwrap() + 1);
        let found = Providers::definition(&mut plugin, &document, inside_result_map);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].range.start.line, 2);
    }

    #[test]
    fn a_non_mapper_document_answers_nothing_even_when_enabled() {
        let host = MockHost::install();
        host.set_setting(SETTING, "true");
        let mut plugin = crate::Main;
        let document = Document {
            path: "/p/pom.xml".into(),
            language: "xml".into(),
            version: 1,
            text: "<project><select id=\"x\"/></project>".into(),
        };
        assert!(Providers::definition(&mut plugin, &document, Position::new(0, 5)).is_empty());
        assert!(Providers::line_markers(&mut plugin, &document).is_empty());
    }
}
