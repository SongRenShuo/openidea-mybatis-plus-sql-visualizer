//! Mapper↔XML document graph (T3-1) and its provider-facing helpers.
//!
//! The graph is built from text only: [`index_xml`] and [`index_java`] read a
//! document into facts, [`build_edges`] turns one document's facts into the
//! keys the host resolves, and [`graph::MapperGraph`] answers lookups across
//! documents of a test layout (single module, multi-module, MPJ).
//!
//! ## Keys
//!
//! | key | definition | references |
//! | --- | --- | --- |
//! | `mapper:<namespace>` | `<mapper namespace>` (XML), mapper interface name (Java) | |
//! | `stmt:<namespace>.<id>` | `<select\|insert\|update\|delete id>` (XML) | Java method name |
//! | `frag:<namespace>.<id>` | `<sql id>` (XML) | `<include refid>` |
//! | `rm:<namespace>.<id>` | `<resultMap id>` (XML), `@Results(id)` (Java) | `resultMap=`/`extends=`/`@ResultMap` |
//! | `java:<fqn>#<method>` | Java method name | `<select id>` whose namespace is the interface |
//! | `type:<fqn>` | Java type name | `resultMap type`, `resultType`, `ofType`, `@XxxProvider(type=…)` |
//!
//! `namespace` is the mapper XML's namespace, i.e. the mapper interface's
//! fully qualified name; matching is by that string alone, so a multi-module
//! or MPJ layout needs no file-system scan.
//!
//! Edges are name-level: a reference whose target is absent still resolves once
//! the target document is indexed, which the host does on its own.
//!
//! ## Method locations
//!
//! [`index_java`] answers "what does this mapper declare" for the graph;
//! [`JavaMethodIndex`] answers "where is this method" for a log row or a
//! statement id. Both read text, neither needs PSI.

pub mod graph;
mod java;
mod java_methods;
pub(crate) mod scan;
mod xml;
mod xml_statements;

pub use graph::MapperGraph;
pub use java::{JavaFacts, JavaMethod, JavaProvider, JavaSql, TypeKind, index_java};
pub use java_methods::{JavaIndexBudget, JavaMethodIndex, JavaMethodLocation};
pub use xml::{XmlFacts, XmlResultMap, XmlStatement, index_xml};
pub use xml_statements::{XmlStatementIndex, XmlStatementLocation};

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use openidea_plugin::Range;
use openidea_plugin::indexing::{Anchor, Document, DocumentIndexer, Edges};

/// The process-wide method index the log panel queries while it builds a push.
///
/// One per process, like the panel's own store: the host hands documents in
/// through [`DocumentIndexer`] (project open, then every change), and the panel
/// only ever reads it, so a push never waits on a file read.
static JAVA_INDEX: OnceLock<Mutex<JavaMethodIndex>> = OnceLock::new();
/// The process-wide mapper XML statement locations, filled the same way.
static XML_INDEX: OnceLock<Mutex<XmlStatementIndex>> = OnceLock::new();

/// The shared Java method index.
///
/// Take this guard once per push and query it there: every lookup is a hash
/// lookup over the in-memory index, never a file read.
pub fn java_methods() -> MutexGuard<'static, JavaMethodIndex> {
    JAVA_INDEX
        .get_or_init(|| Mutex::new(JavaMethodIndex::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The shared mapper XML statement index.
pub fn xml_statements() -> MutexGuard<'static, XmlStatementIndex> {
    XML_INDEX
        .get_or_init(|| Mutex::new(XmlStatementIndex::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Feeds one document's text into the index its extension belongs to.
///
/// Returns how many methods or statements it contributed (0 for anything
/// else). Re-indexing a path replaces what it contributed before, so a save
/// never doubles a file's entries.
pub fn index_source(path: &str, text: &str) -> usize {
    let started = std::time::Instant::now();
    let lower = path.to_ascii_lowercase();
    let contributed = if lower.ends_with(".java") {
        java_methods().add_file(path, text)
    } else if lower.ends_with(".xml") {
        xml_statements().add_file(path, text)
    } else {
        0
    };
    // Cost counters only: they never gate anything, they are what `index-stats`
    // reports when someone asks whether a big project pays for this.
    let micros = started.elapsed().as_micros() as u64;
    COST.documents.fetch_add(1, Ordering::Relaxed);
    COST.bytes.fetch_add(text.len() as u64, Ordering::Relaxed);
    COST.micros.fetch_add(micros, Ordering::Relaxed);
    COST.slowest_micros.fetch_max(micros, Ordering::Relaxed);
    contributed
}

/// Empties both indexes (the test harness; the host refills them on open).
pub fn clear_indexes() {
    *java_methods() = JavaMethodIndex::new();
    *xml_statements() = XmlStatementIndex::new();
    COST.documents.store(0, Ordering::Relaxed);
    COST.bytes.store(0, Ordering::Relaxed);
    COST.micros.store(0, Ordering::Relaxed);
    COST.slowest_micros.store(0, Ordering::Relaxed);
}

/// What indexing has cost so far: documents handed in, their bytes, and the
/// wall time spent inside [`index_source`].
#[derive(Default)]
struct IndexCost {
    documents: AtomicU64,
    bytes: AtomicU64,
    micros: AtomicU64,
    slowest_micros: AtomicU64,
}

static COST: IndexCost = IndexCost {
    documents: AtomicU64::new(0),
    bytes: AtomicU64::new(0),
    micros: AtomicU64::new(0),
    slowest_micros: AtomicU64::new(0),
};

/// What the two process indexes hold.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IndexStats {
    pub java_files: usize,
    pub java_methods: usize,
    pub java_skipped: usize,
    pub xml_files: usize,
    pub xml_statements: usize,
    pub xml_skipped: usize,
    /// Documents indexed since the indexes were last cleared.
    pub documents: u64,
    /// Their total size in bytes.
    pub bytes: u64,
    /// Total wall time spent indexing them (microseconds).
    pub micros: u64,
    /// The slowest single document (microseconds).
    pub slowest_micros: u64,
}

/// A snapshot of both indexes.
pub fn index_stats() -> IndexStats {
    let java = java_methods();
    let mut stats = IndexStats {
        java_files: java.file_count(),
        java_methods: java.len(),
        java_skipped: java.skipped_files(),
        ..IndexStats::default()
    };
    drop(java);
    let xml = xml_statements();
    stats.xml_files = xml.file_count();
    stats.xml_statements = xml.len();
    stats.xml_skipped = xml.skipped_files();
    stats.documents = COST.documents.load(Ordering::Relaxed);
    stats.bytes = COST.bytes.load(Ordering::Relaxed);
    stats.micros = COST.micros.load(Ordering::Relaxed);
    stats.slowest_micros = COST.slowest_micros.load(Ordering::Relaxed);
    stats
}

/// A name the graph refers to, with the UTF-16 range of the name itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Named {
    pub name: String,
    pub range: Range,
}

/// The statement element kinds MyBatis executes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StatementKind {
    Select,
    Insert,
    Update,
    Delete,
}

impl StatementKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StatementKind::Select => "select",
            StatementKind::Insert => "insert",
            StatementKind::Update => "update",
            StatementKind::Delete => "delete",
        }
    }

    pub fn from_tag(tag: &str) -> Option<Self> {
        match tag {
            "select" => Some(StatementKind::Select),
            "insert" => Some(StatementKind::Insert),
            "update" => Some(StatementKind::Update),
            "delete" => Some(StatementKind::Delete),
            _ => None,
        }
    }
}

/// One document's facts, whichever language they came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Facts {
    Xml(XmlFacts),
    Java(JavaFacts),
}

impl Facts {
    pub fn path(&self) -> &str {
        match self {
            Facts::Xml(f) => &f.path,
            Facts::Java(f) => &f.path,
        }
    }
}

impl From<XmlFacts> for Facts {
    fn from(value: XmlFacts) -> Self {
        Facts::Xml(value)
    }
}

impl From<JavaFacts> for Facts {
    fn from(value: JavaFacts) -> Self {
        Facts::Java(value)
    }
}

/// Reads `text` as mapper XML or a Java source by its file extension.
pub fn facts_for(path: &str, text: &str) -> Option<Facts> {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".xml") {
        Some(Facts::Xml(index_xml(path, text)))
    } else if lower.ends_with(".java") {
        Some(Facts::Java(index_java(path, text)))
    } else {
        None
    }
}

pub fn mapper_key(namespace: &str) -> String {
    format!("mapper:{namespace}")
}

pub fn statement_key(namespace: &str, id: &str) -> String {
    format!("stmt:{namespace}.{id}")
}

pub fn fragment_key(namespace: &str, id: &str) -> String {
    format!("frag:{namespace}.{id}")
}

pub fn result_map_key(namespace: &str, id: &str) -> String {
    format!("rm:{namespace}.{id}")
}

pub fn java_key(fqn: &str, method: &str) -> String {
    format!("java:{fqn}#{method}")
}

pub fn type_key(fqn: &str) -> String {
    format!("type:{fqn}")
}

/// A reference written `id` or `namespace.id`: the key is
/// `<prefix>:<namespace>.<id>` either way.
pub fn scoped_key(prefix: &str, namespace: &str, reference: &str) -> String {
    match reference.rsplit_once('.') {
        Some(_) => format!("{prefix}:{reference}"),
        None => format!("{prefix}:{namespace}.{reference}"),
    }
}

fn anchor(key: String, range: Range) -> Anchor {
    Anchor { key, range }
}

/// Turns one document's facts into the definitions, references, aliases and
/// markers the host keeps in its project graph.
pub fn build_edges(facts: &Facts) -> Edges {
    match facts {
        Facts::Xml(f) => xml_edges(f),
        Facts::Java(f) => java_edges(f),
    }
}

fn xml_edges(facts: &XmlFacts) -> Edges {
    let mut edges = Edges::default();
    for type_ref in &facts.type_refs {
        edges
            .references
            .push(anchor(type_key(&type_ref.name), type_ref.range));
    }
    let Some(namespace) = &facts.namespace else {
        return edges;
    };
    let ns = namespace.name.as_str();
    edges
        .definitions
        .push(anchor(mapper_key(ns), namespace.range));
    // Root namespace gutter marker → the interface declaration. The host draws
    // markers from its own graph (`navigation_markers` never calls the plugin),
    // so the gutter stays responsive while a big project is still indexing.
    edges.markers.push(anchor(type_key(ns), namespace.range));

    for statement in &facts.statements {
        let Some(id) = &statement.id else { continue };
        let stmt = statement_key(ns, &id.name);
        edges.definitions.push(anchor(stmt, id.range));
        // A statement id names the interface method that runs it; the method
        // may live in another module, and the host matches it by key. The
        // marker uses the same key, so the gutter icon comes back as soon as
        // the interface is indexed — and never blocks on a plugin call.
        let as_java = java_key(ns, &id.name);
        edges.markers.push(anchor(as_java.clone(), id.range));
        edges.references.push(anchor(as_java, id.range));
        if let Some(result_map) = &statement.result_map {
            edges.references.push(anchor(
                scoped_key("rm", ns, &result_map.name),
                result_map.range,
            ));
        }
    }
    for fragment in &facts.fragments {
        edges
            .definitions
            .push(anchor(fragment_key(ns, &fragment.name), fragment.range));
    }
    for include in &facts.includes {
        edges
            .references
            .push(anchor(scoped_key("frag", ns, &include.name), include.range));
    }
    for result_map in &facts.result_maps {
        let Some(id) = &result_map.id else { continue };
        let rm = result_map_key(ns, &id.name);
        edges.definitions.push(anchor(rm.clone(), id.range));
        if let Some(extends) = &result_map.extends {
            edges
                .references
                .push(anchor(scoped_key("rm", ns, &extends.name), extends.range));
            // The parent's members are the child's too: keys under the child
            // resolve through the parent, the way inherited members do.
            edges.aliases.push(openidea_plugin::indexing::Alias {
                prefix: format!("{rm}#"),
                target: format!("{}#", scoped_key("rm", ns, &extends.name)),
            });
        }
    }
    for reference in &facts.result_map_refs {
        // Already pushed for statement attributes; associations/collections/
        // `extends` add theirs here.
        let from_statement = facts
            .statements
            .iter()
            .any(|s| s.result_map.as_ref() == Some(reference));
        let from_map = facts
            .result_maps
            .iter()
            .any(|m| m.extends.as_ref() == Some(reference));
        if !from_statement && !from_map {
            edges.references.push(anchor(
                scoped_key("rm", ns, &reference.name),
                reference.range,
            ));
        }
    }
    edges
}

fn java_edges(facts: &JavaFacts) -> Edges {
    let mut edges = Edges::default();
    let Some(fqn) = facts.fqn.as_deref() else {
        return edges;
    };
    let Some(type_name) = &facts.type_name else {
        return edges;
    };
    edges
        .definitions
        .push(anchor(type_key(fqn), type_name.range));
    if facts.is_mapper {
        edges
            .definitions
            .push(anchor(mapper_key(fqn), type_name.range));
    }
    for supertype in &facts.supertypes {
        // Methods declared on a base mapper are visible through this interface.
        edges.aliases.push(openidea_plugin::indexing::Alias {
            prefix: format!("java:{fqn}#"),
            target: format!("java:{}#", supertype.name),
        });
    }
    for method in &facts.methods {
        let name = &method.name;
        edges
            .definitions
            .push(anchor(java_key(fqn, &name.name), name.range));
        if facts.is_mapper {
            // The mapper method's gutter marker goes the other way, to the XML
            // statement that implements it; the host resolves both keys from
            // its graph without calling the plugin.
            let stmt = statement_key(fqn, &name.name);
            edges.markers.push(anchor(stmt.clone(), name.range));
            edges.references.push(anchor(stmt, name.range));
        }
        for result_map in &method.result_maps {
            edges.references.push(anchor(
                result_map_key(fqn, &result_map.name),
                result_map.range,
            ));
        }
        for result_id in &method.result_ids {
            edges.definitions.push(anchor(
                result_map_key(fqn, &result_id.name),
                result_id.range,
            ));
        }
        if let Some(provider) = &method.provider
            && let Some(provider_type) = &provider.provider_type
        {
            edges
                .references
                .push(anchor(type_key(&provider_type.name), provider_type.range));
            if let Some(method_name) = &provider.method {
                edges.references.push(anchor(
                    java_key(&provider_type.name, &method_name.name),
                    method_name.range,
                ));
            }
        }
    }
    edges
}

impl DocumentIndexer for crate::Main {
    fn index(&mut self, doc: &Document) -> Edges {
        // The index is background work with its own budget (2 MiB, 16 files a
        // batch): it stays on so Go to Declaration keeps working when the
        // interactive providers are switched off.
        //
        // The same document feed maintains the log panel's two location indexes
        // (lane L28). It is the host's only bulk path — project open, then every
        // changed file — so the panel never needs a workspace scan of its own,
        // and both indexes cap themselves (files, bytes) and count what they
        // refused instead of growing without bound.
        index_source(&doc.path, &doc.text);
        let mut edges = facts_for(&doc.path, &doc.text)
            .map(|facts| build_edges(&facts))
            .unwrap_or_default();
        // `nav.showLineMarkers` off: publish no gutter markers. The setting is
        // read at index time, so it applies to files indexed after the change
        // (the settings page already says a reopen is needed).
        if !crate::providers::setting_on(crate::providers::nav::MARKERS_SETTING) {
            edges.markers.clear();
        }
        edges
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<mapper namespace="com.acme.UserMapper">
  <resultMap id="BaseMap" type="com.acme.User"/>
  <sql id="cols">id, name</sql>
  <select id="findById" resultMap="BaseMap">select <include refid="cols"/> from t where id=#{id}</select>
</mapper>"#;

    const JAVA: &str = r#"package com.acme;
@Mapper
public interface UserMapper {
  @Select("select 1") int ping();
  User findById(Long id);
}"#;

    #[test]
    fn xml_keys_are_the_agreed_shape() {
        let edges = build_edges(&Facts::Xml(index_xml("m.xml", XML)));
        let keys: Vec<&str> = edges.definitions.iter().map(|a| a.key.as_str()).collect();
        assert!(keys.contains(&"mapper:com.acme.UserMapper"));
        assert!(keys.contains(&"stmt:com.acme.UserMapper.findById"));
        assert!(keys.contains(&"frag:com.acme.UserMapper.cols"));
        assert!(keys.contains(&"rm:com.acme.UserMapper.BaseMap"));
        let refs: Vec<&str> = edges.references.iter().map(|a| a.key.as_str()).collect();
        assert!(refs.contains(&"frag:com.acme.UserMapper.cols"));
        assert!(refs.contains(&"rm:com.acme.UserMapper.BaseMap"));
        assert!(refs.contains(&"java:com.acme.UserMapper#findById"));
        // Gutter markers: the host resolves these keys from its graph; the
        // statement row points at the interface method, the root at the type.
        let markers: Vec<&str> = edges.markers.iter().map(|a| a.key.as_str()).collect();
        assert_eq!(
            markers,
            vec![
                "type:com.acme.UserMapper",
                "java:com.acme.UserMapper#findById"
            ]
        );
    }

    #[test]
    fn the_marker_switch_clears_markers_from_the_published_edges() {
        let _guard = crate::views::test_state_lock();
        let host = openidea_plugin::mock::MockHost::install();
        let document = Document {
            path: "/p/src/main/resources/mapper/UserMapper.xml".into(),
            text: XML.into(),
            symbols: Vec::new(),
        };
        let mut plugin = crate::Main;
        assert!(
            !crate::indexer::DocumentIndexer::index(&mut plugin, &document)
                .markers
                .is_empty(),
            "on by default"
        );
        host.set_setting(crate::providers::nav::MARKERS_SETTING, "false");
        assert!(
            crate::indexer::DocumentIndexer::index(&mut plugin, &document)
                .markers
                .is_empty(),
            "nav.showLineMarkers 关闭后不再发布标记"
        );
        clear_indexes();
    }

    #[test]
    fn java_keys_are_the_agreed_shape() {
        let edges = build_edges(&Facts::Java(index_java("UserMapper.java", JAVA)));
        let keys: Vec<&str> = edges.definitions.iter().map(|a| a.key.as_str()).collect();
        assert!(keys.contains(&"type:com.acme.UserMapper"));
        assert!(keys.contains(&"mapper:com.acme.UserMapper"));
        assert!(keys.contains(&"java:com.acme.UserMapper#findById"));
        assert!(keys.contains(&"java:com.acme.UserMapper#ping"));
    }

    #[test]
    fn the_two_documents_share_every_cross_file_key() {
        let mut graph = MapperGraph::new();
        graph.add_xml(index_xml("m.xml", XML));
        graph.add_java(index_java("UserMapper.java", JAVA));
        graph.add_java(index_java(
            "User.java",
            "package com.acme;\npublic class User {}\n",
        ));
        for key in [
            "mapper:com.acme.UserMapper",
            "stmt:com.acme.UserMapper.findById",
            "java:com.acme.UserMapper#findById",
            "type:com.acme.User",
        ] {
            assert!(
                !graph.definitions(key).is_empty(),
                "no definition for {key}"
            );
        }
        assert_eq!(
            graph.resolve("stmt:com.acme.UserMapper.findById")[0].0,
            "m.xml"
        );
        assert_eq!(
            graph.resolve("java:com.acme.UserMapper#findById")[0].0,
            "UserMapper.java"
        );
    }

    #[test]
    fn result_map_extends_becomes_an_alias() {
        let xml = r#"<mapper namespace="n"><resultMap id="Base" type="a.A"/><resultMap id="Child" extends="Base"/></mapper>"#;
        let edges = build_edges(&Facts::Xml(index_xml("m.xml", xml)));
        assert!(
            edges
                .aliases
                .iter()
                .any(|a| a.prefix == "rm:n.Child#" && a.target == "rm:n.Base#")
        );
        assert!(
            edges.references.iter().any(|a| a.key == "rm:n.Base"),
            "the extends value is a reference"
        );
    }

    #[test]
    fn a_java_only_mapper_still_has_java_keys() {
        let mut graph = MapperGraph::new();
        graph.add_java(index_java("UserMapper.java", JAVA));
        assert!(
            graph
                .definitions("stmt:com.acme.UserMapper.findById")
                .is_empty()
        );
        assert_eq!(
            graph.resolve("java:com.acme.UserMapper#ping")[0].0,
            "UserMapper.java"
        );
    }

    // Process-level indexes (lane L28) ------------------------------------
    //
    // These mutate process-wide state, so they serialise on the same lock the
    // panel's tests take: two tests of different modules must never refill the
    // index while the other one reads it.

    #[test]
    fn index_source_routes_by_extension_and_replaces_a_reloaded_path() {
        let _guard = crate::views::test_state_lock();
        clear_indexes();
        assert_eq!(
            index_source("/project/src/main/java/com/acme/UserMapper.java", JAVA),
            2,
            "ping and findById"
        );
        assert_eq!(
            index_source("/project/UserMapper.xml", XML),
            1,
            "one statement"
        );
        assert_eq!(index_source("/project/README.md", "# hi"), 0);
        let stats = index_stats();
        assert_eq!(stats.java_files, 1);
        assert_eq!(stats.java_methods, 2);
        assert_eq!(stats.xml_files, 1);
        assert_eq!(stats.xml_statements, 1);

        // The same path reloaded with one method fewer: replace, not append.
        let shorter =
            "package com.acme;\n@Mapper\npublic interface UserMapper {\n  int ping();\n}\n";
        index_source("/project/src/main/java/com/acme/UserMapper.java", shorter);
        let stats = index_stats();
        assert_eq!(stats.java_files, 1);
        assert_eq!(stats.java_methods, 1, "the dropped method is gone");
        assert!(
            java_methods()
                .lookup_unique("com.acme.UserMapper#findById")
                .is_none()
        );
        clear_indexes();
    }

    #[test]
    fn the_shared_index_answers_the_panel_and_counts_refused_files() {
        let _guard = crate::views::test_state_lock();
        clear_indexes();
        index_source("/project/src/main/java/com/acme/UserMapper.java", JAVA);
        index_source("/project/UserMapper.xml", XML);
        {
            let java = java_methods();
            let hit = java
                .lookup_unique("c.acme.UserMapper#findById")
                .expect("a shortened class name resolves");
            assert!(hit.file.ends_with("UserMapper.java"));
            assert_eq!(hit.display_line(), 5, "0-based line 4 is findById");
        }
        {
            let xml = xml_statements();
            let hit = xml
                .lookup_unique("com.acme.UserMapper", "findById")
                .expect("the XML statement resolves");
            assert_eq!(hit.display_line(), 4, "0-based line 3 is the select tag");
        }
        // A build-output path is refused and counted, never indexed.
        index_source("/project/target/generated/Schema.java", "class S {}");
        assert_eq!(index_stats().java_skipped, 1);
        clear_indexes();
        assert_eq!(index_stats(), IndexStats::default());
    }
}
