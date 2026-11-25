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

pub mod graph;
mod java;
pub(crate) mod scan;
mod xml;

pub use graph::MapperGraph;
pub use java::{JavaFacts, JavaMethod, JavaProvider, JavaSql, TypeKind, index_java};
pub use xml::{XmlFacts, XmlResultMap, XmlStatement, index_xml};

use openidea_plugin::Range;
use openidea_plugin::indexing::{Anchor, Document, DocumentIndexer, Edges};

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

    for statement in &facts.statements {
        let Some(id) = &statement.id else { continue };
        let stmt = statement_key(ns, &id.name);
        edges.definitions.push(anchor(stmt, id.range));
        // A statement id names the interface method that runs it; the method
        // may live in another module, and the host matches it by key.
        let as_java = java_key(ns, &id.name);
        edges.references.push(anchor(as_java.clone(), id.range));
        edges.markers.push(anchor(as_java, id.range));
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
            let stmt = statement_key(fqn, &name.name);
            edges.references.push(anchor(stmt.clone(), name.range));
            edges.markers.push(anchor(stmt, name.range));
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
        if let Some(provider) = &method.provider {
            if let Some(provider_type) = &provider.provider_type {
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
    }
    edges
}

impl DocumentIndexer for crate::Main {
    fn index(&mut self, doc: &Document) -> Edges {
        // The index is background work with its own budget (2 MiB, 16 files a
        // batch): it stays on so Go to Declaration keeps working when the
        // interactive providers are switched off.
        facts_for(&doc.path, &doc.text)
            .map(|facts| build_edges(&facts))
            .unwrap_or_default()
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
}
