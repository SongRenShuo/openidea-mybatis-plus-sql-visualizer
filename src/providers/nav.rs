//! `definition` for mapper XML and Java (T3-2); the gutter markers are the
//! document indexer's (`indexer::build_edges` → `Edges.markers`, drawn by the
//! host from its own graph).
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
use openidea_plugin::providers::{Document, Providers};
use openidea_plugin::{Location, Position, Range};

pub const SETTING: &str = "nav.enabled";
/// The gutter markers have their own switch, on by default like IntelliJ. The
/// host draws them from the document indexer's `Edges.markers`; this switch is
/// read while indexing (`indexer::DocumentIndexer`), so it takes effect on the
/// next index of a file.
pub const MARKERS_SETTING: &str = "nav.showLineMarkers";

impl Providers for crate::Main {
    fn definition(&mut self, doc: &Document, at: Position) -> Vec<Location> {
        if !super::setting_on(SETTING) {
            return Vec::new();
        }
        definition_in(doc, at)
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

/// A target inside another file: the index gives the symbol's own line and
/// UTF-16 column, so the caret lands on the name rather than the line start.
fn other_file(path: &str, line: u32, character: u32) -> Location {
    let position = Position::new(line, character);
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
        .map(|hit| other_file(&hit.file, hit.line, hit.character))
        .collect()
}

/// The type declarations `namespace` names, across the project.
fn class_targets(namespace: &str) -> Vec<Location> {
    crate::indexer::java_methods()
        .class_location(namespace)
        .into_iter()
        .map(|hit| other_file(&hit.file, hit.line, hit.character))
        .collect()
}

/// The statements `namespace.id` names, across the project.
fn statement_targets(namespace: &str, id: &str) -> Vec<Location> {
    crate::indexer::xml_statements()
        .lookup(namespace, id)
        .into_iter()
        .map(|hit| other_file(&hit.file, hit.line, hit.character))
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
        // The root `<mapper namespace="…">` names the interface: the jump the
        // reference's root marker offers, and the only one a method-less
        // mapper still has.
        if element.name == "mapper"
            && let Some(namespace) = value_at(element, "namespace", offset)
        {
            for target in class_targets(namespace) {
                push_target(&mut out, target);
            }
        }
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

    /// The interface the XML namespace names, indexed so statement/root
    /// markers have a jump target.
    const JAVA: &str = "package com.acme;
public interface UserMapper {
  User findById(int id);
}
";

    fn index_java() {
        crate::indexer::clear_indexes();
        crate::indexer::index_source("/p/mapper/UserMapper.java", JAVA);
    }

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
        // The global indexes are process-wide; other tests clear them.
        let _guard = crate::views::test_state_lock();
        // Nothing is stored: the schema default (`nav.enabled = true`) answers.
        let _host = MockHost::install();
        index_java();
        let mut plugin = crate::Main;
        let document = doc(XML);
        assert_eq!(
            Providers::definition(&mut plugin, &document, at(XML, "cols\"/>")).len(),
            1,
            "navigation is on by default"
        );

        // An explicit `false` in the settings store wins over the default.
        let host = MockHost::install();
        host.set_setting(SETTING, "false");
        let mut plugin = crate::Main;
        assert!(Providers::definition(&mut plugin, &document, at(XML, "cols\"/>")).is_empty());
    }

    #[test]
    fn definition_resolves_includes_and_result_maps() {
        let _guard = crate::views::test_state_lock();
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

        // The root namespace jumps to the interface.
        index_java();
        let root = Providers::definition(&mut plugin, &document, at(XML, "com.acme.UserMapper"));
        assert_eq!(root.len(), 1);
        assert_eq!(root[0].path, "/p/mapper/UserMapper.java");

        // A word that is neither a reference nor resolvable answers nothing.
        let nowhere = Providers::definition(&mut plugin, &document, at(XML, "resultMap"));
        assert!(nowhere.is_empty());
    }

    #[test]
    fn marker_keys_pair_the_statement_and_the_root_with_the_interface() {
        let _guard = crate::views::test_state_lock();
        // The gutter markers are the indexer's (`Edges.markers`); the host
        // resolves their keys against its own graph. The source side says
        // `java:<namespace>#<id>` for a statement row and `type:<namespace>`
        // for the root, and `stmt:<namespace>.<id>` from the Java side.
        let edges = crate::indexer::build_edges(&crate::indexer::Facts::Xml(
            crate::indexer::index_xml("/p/mapper/UserMapper.xml", XML),
        ));
        let keys: Vec<(&str, u32)> = edges
            .markers
            .iter()
            .map(|anchor| (anchor.key.as_str(), anchor.range.start.line))
            .collect();
        assert_eq!(
            keys,
            vec![
                ("type:com.acme.UserMapper", 0),
                ("java:com.acme.UserMapper#findById", 2),
            ],
            "root + statement row"
        );

        let java = crate::indexer::build_edges(&crate::indexer::Facts::Java(
            crate::indexer::index_java("/p/mapper/UserMapper.java", JAVA),
        ));
        let keys: Vec<(&str, u32)> = java
            .markers
            .iter()
            .map(|anchor| (anchor.key.as_str(), anchor.range.start.line))
            .collect();
        assert_eq!(
            keys,
            vec![
                ("mapper:com.acme.UserMapper", 1),
                ("stmt:com.acme.UserMapper.findById", 2),
            ],
            "interface declaration → XML root, then every method → its statement"
        );

        // Ctrl+B still resolves the result map and the fragment locally.
        let host = MockHost::install();
        host.set_setting(SETTING, "true");
        index_java();
        let mut plugin = crate::Main;
        let result_map = Providers::definition(&mut plugin, &doc(XML), at(XML, "BaseMap\">"));
        assert_eq!(result_map.len(), 1);
        assert_eq!(result_map[0].range.start.line, 5);
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
        let edges = crate::indexer::build_edges(&crate::indexer::Facts::Xml(
            crate::indexer::index_xml(&document.path, &document.text),
        ));
        assert!(edges.markers.is_empty(), "非 mapper 文档不出标记");
    }
}
