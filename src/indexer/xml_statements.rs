//! `namespace` + statement id → the mapper XML file and line that declares it.
//!
//! "Jump to Mapper XML" needs a location, and [`JavaMethodIndex`](super::JavaMethodIndex)
//! only answers Java methods: `resolve_statement` maps a statement id to the
//! interface method, which is a different place. [`MapperGraph`](super::MapperGraph)
//! does answer XML ranges, but it rebuilds every document's edges on each
//! lookup, which is too much for a per-row query on the panel's push path.
//!
//! This is the small index that asks the one question the panel asks:
//! `(namespace, id)` → `(file, line)`. It reads the same text-only
//! [`index_xml`](super::index_xml) facts the document indexer already builds and
//! stores four fields per statement, so a 4k-file mapper tree costs a few
//! hundred kilobytes, not a graph.
//!
//! Namespace matching follows the Java side's rule (`class_matches`): written
//! exactly, or segment-wise from the right with Spring's single-letter
//! shortening (`c.e.m.mapper.UserMapper`). An ambiguous answer is `None` — the
//! panel never picks one of several.

use std::collections::HashMap;

use super::java_methods::dotted_suffix_matches;
use super::xml::index_xml;

/// Files refused by the cap or by an excluded path.
const MAX_FILES: usize = 4096;

/// One statement element, as the XML states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlStatementLocation {
    /// Path exactly as the host handed it in.
    pub file: String,
    /// The file's `<mapper namespace>`, verbatim.
    pub namespace: String,
    /// `<select|insert|update|delete id="…">`.
    pub id: String,
    /// 0-based line of the start tag.
    pub line: u32,
}

impl XmlStatementLocation {
    /// 1-based line, for anything a human reads (`UserMapper.xml:42`).
    pub fn display_line(&self) -> u32 {
        self.line + 1
    }
}

/// Statement declarations of one project's mapper XML.
#[derive(Debug, Clone, Default)]
pub struct XmlStatementIndex {
    /// `id` → slots. The namespace is compared afterwards, so a lookup costs
    /// "statements with this id", not "every statement in the project".
    by_id: HashMap<String, Vec<usize>>,
    statements: Vec<Option<XmlStatementLocation>>,
    files: HashMap<String, Vec<usize>>,
    skipped: usize,
}

impl XmlStatementIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Live statement declarations (removed files' slots are dropped at reload).
    pub fn len(&self) -> usize {
        self.statements.len()
    }

    pub fn is_empty(&self) -> bool {
        self.statements.is_empty()
    }

    /// Files currently contributing.
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Files refused: over the cap, in an excluded directory, or a duplicate
    /// path over budget.
    pub fn skipped_files(&self) -> usize {
        self.skipped
    }

    /// Indexes one mapper XML. Returns how many statements it added.
    ///
    /// Re-adding the same path replaces that file's statements. A file that
    /// declares no `<mapper namespace>` contributes nothing.
    pub fn add_file(&mut self, path: &str, text: &str) -> usize {
        if !path.to_ascii_lowercase().ends_with(".xml") || excluded_path(path) {
            self.skipped += 1;
            return 0;
        }
        let known = self.files.contains_key(path);
        if !known && self.files.len() >= MAX_FILES {
            self.skipped += 1;
            return 0;
        }
        if known {
            self.remove_file(path);
        }

        let facts = index_xml(path, text);
        let Some(namespace) = facts.namespace.as_ref().map(|named| named.name.clone()) else {
            self.files.insert(path.to_owned(), Vec::new());
            return 0;
        };
        let mut slots = Vec::new();
        for statement in &facts.statements {
            let Some(id) = &statement.id else { continue };
            let slot = self.statements.len();
            self.by_id.entry(id.name.clone()).or_default().push(slot);
            self.statements.push(Some(XmlStatementLocation {
                file: path.to_owned(),
                namespace: namespace.clone(),
                id: id.name.clone(),
                line: statement.line,
            }));
            slots.push(slot);
        }
        let added = slots.len();
        self.files.insert(path.to_owned(), slots);
        added
    }

    /// Drops one file's statements. Returns how many were removed.
    pub fn remove_file(&mut self, path: &str) -> usize {
        let Some(slots) = self.files.remove(path) else {
            return 0;
        };
        let mut removed = 0;
        for slot in slots {
            if self.statements.get(slot).is_some_and(Option::is_some) {
                removed += 1;
            }
            if let Some(entry) = self.statements.get_mut(slot) {
                *entry = None;
            }
        }
        self.compact();
        removed
    }

    /// Rewrites the slot vectors without the dropped entries.
    fn compact(&mut self) {
        let mut remap: Vec<Option<usize>> = vec![None; self.statements.len()];
        let mut live: Vec<Option<XmlStatementLocation>> = Vec::with_capacity(self.statements.len());
        for (slot, entry) in self.statements.drain(..).enumerate() {
            if entry.is_some() {
                remap[slot] = Some(live.len());
                live.push(entry);
            }
        }
        self.statements = live;
        for slots in self.by_id.values_mut() {
            *slots = slots
                .iter()
                .filter_map(|slot| remap[*slot])
                .collect::<Vec<_>>();
        }
        self.by_id.retain(|_, slots| !slots.is_empty());
        for slots in self.files.values_mut() {
            *slots = slots.iter().filter_map(|slot| remap[*slot]).collect();
        }
    }

    /// Every statement of `namespace` with `id`.
    ///
    /// Overloads and same-named statements in several namespaces all come back:
    /// the caller decides, this index never picks one.
    pub fn lookup(&self, namespace: &str, id: &str) -> Vec<&XmlStatementLocation> {
        let namespace = namespace.trim();
        let id = id.trim();
        if namespace.is_empty() || id.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        let Some(slots) = self.by_id.get(id) else {
            return out;
        };
        for slot in slots {
            let Some(Some(location)) = self.statements.get(*slot) else {
                continue;
            };
            if namespace_matches(namespace, &location.namespace) {
                out.push(location);
            }
        }
        out
    }

    /// The single statement `(namespace, id)` names, or `None` when the answer
    /// is ambiguous or absent. Never guesses.
    pub fn lookup_unique(&self, namespace: &str, id: &str) -> Option<&XmlStatementLocation> {
        let only = self.lookup(namespace, id);
        (only.len() == 1).then(|| only[0])
    }
}

/// Does `written` (FQN or Spring-abbreviated FQN) denote `real`?
fn namespace_matches(written: &str, real: &str) -> bool {
    written == real || (written.contains('.') && dotted_suffix_matches(written, real))
}

/// True when any path segment is a build-output or tooling directory.
///
/// The same list the Java side uses; kept local so this module does not have to
/// widen `java_methods`' visibility for one predicate.
fn excluded_path(path: &str) -> bool {
    const EXCLUDED: [&str; 9] = [
        "target",
        "build",
        "out",
        "bin",
        "node_modules",
        ".git",
        ".gradle",
        ".worktrees",
        ".idea",
    ];
    path.replace('\\', "/")
        .split('/')
        .any(|segment| EXCLUDED.contains(&segment))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAPPER: &str = r#"<mapper namespace="com.example.mapper.UserMapper">
  <select id="findById">select 1</select>
  <insert id="insert">insert</insert>
</mapper>"#;

    #[test]
    fn a_statement_resolves_to_its_file_and_line() {
        let mut index = XmlStatementIndex::new();
        assert_eq!(index.add_file("/project/UserMapper.xml", MAPPER), 2);
        let hit = index
            .lookup_unique("com.example.mapper.UserMapper", "findById")
            .expect("the declaration");
        assert_eq!(hit.file, "/project/UserMapper.xml");
        assert_eq!(hit.display_line(), 2, "0-based line 1 is the select tag");
        assert!(
            index
                .lookup_unique("com.example.mapper.UserMapper", "nope")
                .is_none()
        );
    }

    #[test]
    fn a_shortened_namespace_still_matches() {
        let mut index = XmlStatementIndex::new();
        index.add_file("/project/UserMapper.xml", MAPPER);
        assert!(
            index
                .lookup_unique("c.e.mapper.UserMapper", "findById")
                .is_some(),
            "Spring's c.e.m shortening matches the real namespace"
        );
        assert!(
            index
                .lookup_unique("other.mapper.UserMapper", "findById")
                .is_none(),
            "a namespace whose segments differ is not a match"
        );
    }

    #[test]
    fn two_files_declaring_the_same_statement_are_ambiguous() {
        let mut index = XmlStatementIndex::new();
        index.add_file("/project/a/UserMapper.xml", MAPPER);
        index.add_file("/project/b/UserMapper.xml", MAPPER);
        assert_eq!(
            index
                .lookup("com.example.mapper.UserMapper", "findById")
                .len(),
            2
        );
        assert!(
            index
                .lookup_unique("com.example.mapper.UserMapper", "findById")
                .is_none(),
            "two candidates are never narrowed to one"
        );
    }

    #[test]
    fn re_adding_a_path_replaces_its_statements() {
        let mut index = XmlStatementIndex::new();
        index.add_file("/project/UserMapper.xml", MAPPER);
        let without = r#"<mapper namespace="com.example.mapper.UserMapper">
  <select id="findById">select 1</select>
</mapper>"#;
        index.add_file("/project/UserMapper.xml", without);
        assert_eq!(index.file_count(), 1);
        assert!(
            index
                .lookup_unique("com.example.mapper.UserMapper", "insert")
                .is_none(),
            "the dropped statement is gone"
        );
        assert!(
            index
                .lookup_unique("com.example.mapper.UserMapper", "findById")
                .is_some()
        );
    }

    #[test]
    fn excluded_paths_and_non_xml_files_are_skipped_and_counted() {
        let mut index = XmlStatementIndex::new();
        assert_eq!(index.add_file("/project/target/UserMapper.xml", MAPPER), 0);
        assert_eq!(index.add_file("/project/UserMapper.java", MAPPER), 0);
        assert_eq!(index.skipped_files(), 2);
        // An XML without `<mapper namespace>` contributes nothing and is not a
        // skip: it is simply not a mapper.
        assert_eq!(index.add_file("/project/pom.xml", "<project/>"), 0);
        assert_eq!(index.skipped_files(), 2);
    }
}
