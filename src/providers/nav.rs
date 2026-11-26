//! `definition` and `line_markers` for mapper XML and Java (T3-2).
//!
//! A document answers inside the editing budget, so it never searches the
//! project. Two kinds of answer are on offer:
//!
//! * **same-document** — `include refid`, `resultMap=`/`extends=` and a Java
//!   `@ResultMap`, resolved against the document's own text;
//! * **cross-file** — the statement id of a `<select|insert|update|delete>`
//!   names the interface method that runs it, and a method name names the
//!   XML statement that implements it. Those are resolved through the two
//!   process indexes [`crate::indexer::java_methods`] and
//!   [`crate::indexer::xml_statements`], which the host fills from this plugin's
//!   `documentIndexers` feed (project open, then every change). The host keeps
//!   its own copy of the same graph for its indexed navigation; these
//!   providers use the in-process one so the answer is also available the
//!   instant a file is opened.
//!
//! On by default (`nav.enabled = true`, `nav.showLineMarkers = true`), like the
//! IntelliJ edition: the SQL in a mapper is only useful if you can jump between
//! its statement, its fragment and the Java method. Turning them off leaves
//! rendering untouched.

use crate::indexer::scan::{self, Positions};
use crate::indexer::{index_java, index_xml};
use openidea_plugin::providers::{Document, LineMarker, Providers};
use openidea_plugin::{Location, Position, Range};

pub const SETTING: &str = "nav.enabled";
/// The gutter markers have their own switch, on by default like IntelliJ.
pub const MARKERS_SETTING: &str = "nav.showLineMarkers";

impl Providers for crate::Main {
    fn definition(&mut self, doc: &Document, at: Position) -> Vec<Location> {
        if !super::setting_on(SETTING) {
            return Vec::new();
        }
        definition_in(doc, at)
    }

    fn line_markers(&mut self, doc: &Document) -> Vec<LineMarker> {
        if !super::setting_on(SETTING) || !super::setting_on(MARKERS_SETTING) {
            return Vec::new();
        }
        markers_in(doc)
    }
}

/// Every target the document at `at` has: the same-document references first,
/// then whatever the in-process indexes know about the counterpart file.
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

/// A target inside another file: the index gives a line, not a range, and the
/// caret is the caller's to place.
fn other_file(path: &str, line: u32) -> Location {
    let position = Position::new(line, 0);
    Location {
        path: path.into(),
        range: Range::new(position, position),
    }
}

/// The interface methods `namespace.statementId` names, across the project.
fn method_targets(namespace: Option<&crate::indexer::Named>, id: &str) -> Vec<Location> {
    let Some(namespace) = namespace else {
        return Vec::new();
    };
    crate::indexer::java_methods()
        .resolve_statement(&namespace.name, id)
        .into_iter()
        .map(|hit| other_file(&hit.file, hit.line))
        .collect()
}

/// The statements `namespace.id` names, across the project.
fn statement_targets(namespace: &str, id: &str) -> Vec<Location> {
    crate::indexer::xml_statements()
        .lookup(namespace, id)
        .into_iter()
        .map(|hit| other_file(&hit.file, hit.line))
        .collect()
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
                let is_statement = crate::indexer::StatementKind::from_tag(other).is_some();
                if !is_statement {
                    continue;
                }
                if let Some(referenced) = value_at(element, "resultMap", offset)
                    && let Some(result_map) = facts.local_result_map(referenced)
                {
                    out.push(location(doc, result_map.range));
                }
                // `id` names the interface method that runs the statement. The
                // Java side may live in another module, so the index decides.
                if let Some(id) = value_at(element, "id", offset) {
                    for target in method_targets(facts.namespace.as_ref(), id) {
                        push_target(&mut out, target);
                    }
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
        if contains(&method.name.range, at) {
            // The method name names the statement it runs, in a mapper XML of
            // this project.
            if let Some(fqn) = facts.fqn.as_deref() {
                for target in statement_targets(fqn, &method.name.name) {
                    push_target(&mut out, target);
                }
            }
        }
    }
    out
}

/// Appends `target` unless the same path and range is already answered.
fn push_target(out: &mut Vec<Location>, target: Location) {
    let same = out
        .iter()
        .any(|known| known.path == target.path && known.range == target.range);
    if !same {
        out.push(target);
    }
}

fn contains(range: &Range, at: Position) -> bool {
    range.start <= at && at < range.end
}

/// Gutter markers for the XML document: statement rows that jump to the
/// fragment, result map or interface method they use, and fragment/result-map
/// rows that jump to the first statement using them.
///
/// The Java document gets its own set: a mapper method row jumps to the
/// statement that implements it, when this project has one.
pub fn markers_in(doc: &Document) -> Vec<LineMarker> {
    if super::is_xml(&doc.path) {
        xml_markers(doc)
    } else if super::is_java(&doc.path) {
        java_markers(doc)
    } else {
        Vec::new()
    }
}

fn xml_markers(doc: &Document) -> Vec<LineMarker> {
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
        } else {
            // Nothing in this file: the statement's method, if it is indexed.
            if let Some(target) = method_targets(facts.namespace.as_ref(), &id.name)
                .into_iter()
                .next()
            {
                push(LineMarker {
                    line: id.range.start.line,
                    icon: Some("navigate".to_owned()),
                    tooltip: format!("MyBatis: go to mapper method {}", id.name),
                    target: Some(target),
                });
            }
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

fn java_markers(doc: &Document) -> Vec<LineMarker> {
    let facts = index_java(&doc.path, &doc.text);
    let Some(fqn) = facts.fqn.as_deref() else {
        return Vec::new();
    };
    let mut out: Vec<LineMarker> = Vec::new();
    let mut lines: Vec<u32> = Vec::new();
    for method in &facts.methods {
        let Some(target) = statement_targets(fqn, &method.name.name).into_iter().next() else {
            continue;
        };
        let line = method.name.range.start.line;
        if lines.contains(&line) {
            continue;
        }
        lines.push(line);
        out.push(LineMarker {
            line,
            icon: Some("navigate".to_owned()),
            tooltip: format!("MyBatis: go to statement {}", method.name.name),
            target: Some(target),
        });
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
    fn the_setting_defaults_to_on_and_an_explicit_false_turns_it_off() {
        // Nothing is stored: the schema default (`nav.enabled = true`) answers.
        let _host = MockHost::install();
        let mut plugin = crate::Main;
        let document = doc(XML);
        assert_eq!(
            Providers::definition(&mut plugin, &document, at(XML, "cols\"/>")).len(),
            1,
            "navigation is on by default"
        );
        assert!(
            !Providers::line_markers(&mut plugin, &document).is_empty(),
            "markers are on by default too"
        );

        // An explicit `false` in the settings store wins over the default.
        let host = MockHost::install();
        host.set_setting(SETTING, "false");
        let mut plugin = crate::Main;
        assert!(Providers::definition(&mut plugin, &document, at(XML, "cols\"/>")).is_empty());
        assert!(Providers::line_markers(&mut plugin, &document).is_empty());

        // The marker switch alone silences the gutter, not Ctrl+B.
        let host = MockHost::install();
        host.set_setting(MARKERS_SETTING, "false");
        let mut plugin = crate::Main;
        assert_eq!(
            Providers::definition(&mut plugin, &document, at(XML, "cols\"/>")).len(),
            1
        );
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
