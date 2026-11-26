//! L38: the mapper navigation a user expects from Ctrl+B and the gutter.
//!
//! The fixture is the shape the bug report describes: one mapper XML and the
//! mapper interface in another file of the same project. The document indexer
//! feed (`indexer::index_source`) is the only thing that runs first; it is what
//! the host calls on project open and on every change, so these tests exercise
//! the same state a real edit session has.
//!
//! Test names carry `providers` so `cargo test providers` covers them.

use openidea_plugin::providers::Document;
use openidea_plugin::{Position, text};
use openidea_plugin_mybatis_plus_sql_visualizer::indexer::{self, clear_indexes, index_source};
use openidea_plugin_mybatis_plus_sql_visualizer::providers::{nav, set_column_lookup};

const XML_PATH: &str = "/project/src/main/resources/mapper/UserMapper.xml";
const JAVA_PATH: &str = "/project/src/main/java/com/acme/mapper/UserMapper.java";

const XML: &str = r#"<mapper namespace="com.acme.mapper.UserMapper">
  <resultMap id="BaseMap" type="com.acme.User">
    <id column="id" property="id"/>
  </resultMap>
  <sql id="cols">id, name</sql>
  <select id="findById" resultMap="BaseMap">
    select <include refid="cols"/> from t_user where id = #{id}
  </select>
  <update id="touch">update t_user set updated_at = now()</update>
  <resultMap id="ChildMap" extends="BaseMap"/>
</mapper>"#;

const JAVA: &str = r#"package com.acme.mapper;
public interface UserMapper {
  com.acme.User findById(Long id);
  int touch(Long id);
}"#;

/// The process-wide indexes are shared by every test of this binary.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn xml_doc() -> Document {
    Document {
        path: XML_PATH.into(),
        language: "xml".into(),
        version: 1,
        text: XML.into(),
    }
}

fn java_doc() -> Document {
    Document {
        path: JAVA_PATH.into(),
        language: "java".into(),
        version: 1,
        text: JAVA.into(),
    }
}

/// The caret position inside the first `needle`.
fn at(document: &Document, needle: &str) -> Position {
    let offset = document.text.find(needle).expect("needle in text") + 1;
    text::position(&document.text, offset)
}

/// Feeds both files the way the host's document indexer does.
fn index_fixture() {
    clear_indexes();
    index_source(XML_PATH, XML);
    index_source(JAVA_PATH, JAVA);
}

#[test]
fn providers_setup_indexes_the_fixture() {
    let _guard = serial();
    index_fixture();
    let stats = indexer::index_stats();
    assert_eq!(stats.xml_statements, 2, "findById and touch");
    assert_eq!(stats.java_methods, 2, "findById and touch");
}

#[test]
fn providers_same_document_definition_still_resolves() {
    let _guard = serial();
    index_fixture();
    let document = xml_doc();

    let include = nav::definition_in(&document, at(&document, "cols\"/>"));
    assert_eq!(include.len(), 1, "refid → <sql> in this file");
    assert_eq!(include[0].range.start.line, 4);

    let result_map = nav::definition_in(&document, at(&document, "BaseMap\">"));
    assert_eq!(result_map.len(), 1, "resultMap= → <resultMap> in this file");
    assert_eq!(result_map[0].range.start.line, 1);

    let extends = nav::definition_in(&document, at(&document, "BaseMap\"/>"));
    assert_eq!(
        extends.len(),
        1,
        "extends= → the parent <resultMap> in this file"
    );
    assert_eq!(extends[0].range.start.line, 1);
}

#[test]
fn providers_definition_jumps_from_a_statement_id_to_the_interface_method() {
    let _guard = serial();
    index_fixture();
    let document = xml_doc();

    let found = nav::definition_in(&document, at(&document, "findById"));
    assert_eq!(found.len(), 1, "Ctrl+B on the statement id");
    assert_eq!(found[0].path, JAVA_PATH);
    assert_eq!(found[0].range.start.line, 2, "UserMapper#findById");

    let touch = nav::definition_in(&document, at(&document, "touch"));
    assert_eq!(touch.len(), 1);
    assert_eq!(touch[0].path, JAVA_PATH);
    assert_eq!(touch[0].range.start.line, 3, "UserMapper#touch");
}

#[test]
fn providers_definition_jumps_from_the_interface_method_to_the_statement() {
    let _guard = serial();
    index_fixture();
    let document = java_doc();

    let found = nav::definition_in(&document, at(&document, "findById"));
    assert_eq!(found.len(), 1, "Ctrl+B on the mapper method name");
    assert_eq!(found[0].path, XML_PATH);
    assert_eq!(
        found[0].range.start.line, 5,
        "the <select id=\"findById\"> tag"
    );

    let touch = nav::definition_in(&document, at(&document, "touch"));
    assert_eq!(touch.len(), 1);
    assert_eq!(touch[0].path, XML_PATH);
    assert_eq!(
        touch[0].range.start.line, 8,
        "the <update id=\"touch\"> tag"
    );
}

#[test]
fn providers_line_markers_cover_both_files() {
    let _guard = serial();
    index_fixture();

    let xml = nav::markers_in(&xml_doc());
    let mut lines: Vec<u32> = xml.iter().map(|marker| marker.line).collect();
    lines.sort_unstable();
    assert_eq!(
        lines,
        vec![1, 4, 5, 8],
        "result map row, fragment row, statement row (to its result map) and the \
         unbound statement row (to UserMapper#touch)"
    );
    let to_java = xml
        .iter()
        .find(|marker| marker.line == 8)
        .expect("the <update> row points at UserMapper#touch");
    assert_eq!(to_java.target.as_ref().unwrap().path, JAVA_PATH);
    assert_eq!(to_java.target.as_ref().unwrap().range.start.line, 3);

    let java = nav::markers_in(&java_doc());
    let java_lines: Vec<u32> = java.iter().map(|marker| marker.line).collect();
    assert_eq!(java_lines, vec![2, 3], "both interface methods");
    assert!(
        java.iter()
            .all(|marker| marker.target.as_ref().unwrap().path == XML_PATH),
        "each marker jumps into the mapper XML"
    );
    let find_by_id = java.iter().find(|marker| marker.line == 2).unwrap();
    assert_eq!(find_by_id.target.as_ref().unwrap().range.start.line, 5);
}

#[test]
fn providers_injections_are_on_by_default() {
    let _guard = serial();
    let _host = openidea_plugin::mock::MockHost::install();
    let mut plugin = openidea_plugin_mybatis_plus_sql_visualizer::Main;
    use openidea_plugin::language_features::Injections;
    let document = xml_doc();
    let injections = Injections::injections(&mut plugin, &document);
    assert!(
        !injections.is_empty(),
        "injections.enabled defaults to true, so the SQL text nodes are injected"
    );
    assert!(injections.iter().all(|i| i.language == "sql"));
}

fn table_columns(table: &str) -> Option<Vec<String>> {
    (table == "t_user").then(|| vec!["id".to_owned(), "name".to_owned()])
}

#[test]
fn providers_inlay_hints_are_on_by_default_and_gated() {
    let _guard = serial();
    let host = openidea_plugin::mock::MockHost::install();
    set_column_lookup(Some(table_columns));
    let mut plugin = openidea_plugin_mybatis_plus_sql_visualizer::Main;
    use openidea_plugin::Range;
    use openidea_plugin::inlay_hints::InlayHints;
    let document = Document {
        path: XML_PATH.into(),
        language: "xml".into(),
        version: 1,
        text: "<mapper namespace=\"n\"><insert id=\"a\">insert into t_user values (#{id})</insert></mapper>"
            .into(),
    };
    assert_eq!(
        InlayHints::inlay_hints(&mut plugin, &document, Range::default()).len(),
        1,
        "inlay.enabled defaults to true"
    );

    host.set_setting("inlay.enabled", "false");
    assert!(
        InlayHints::inlay_hints(&mut plugin, &document, Range::default()).is_empty(),
        "turning inlay.enabled off silences the hint"
    );
    set_column_lookup(None);
}
