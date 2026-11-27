//! Mapper XML rendering (T1-1~T1-3): dynamic tags, restricted OGNL, includes.
//!
//! Lane L1 owns this directory; see `docs/TASKS.md` T1-1~T1-3.
//!
//! # Public contract (other lanes build on this)
//!
//! - [`Confidence`] is serialised as `"exact" | "partial" | "unresolved"`
//!   (snake_case), matching the engine contract in `docs/ARCHITECTURE.md`;
//! - [`RenderOutput::sql`] uses `?` placeholders; [`RenderOutput::params`] holds
//!   the bound values in SQL order;
//! - `confidence == Unresolved` always comes with an empty `sql` — never a
//!   half-rendered statement;
//! - every message key is lowercase with dots (`xml.include.not_found`,
//!   `ognl.unsupported`, …); user-visible text is localised by `src/i18n.rs`.
//!
//! Entry points:
//! - [`render_statement`] — one mapper document, one statement id;
//! - [`MapperSet`] — several documents (cross-namespace includes);
//! - [`crate::core::annotation`] and [`crate::core::ibatis`] for the other two
//!   statement sources.

pub mod include;
pub mod intercept;
pub mod ognl;
pub mod param;
pub mod render;
pub mod script;
pub mod token;
pub mod xmltree;

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use xmltree::Element;

pub const KEY_PARSE_FAILED: &str = "xml.parse.failed";
pub const KEY_ROOT_INVALID: &str = "xml.mapper.root_invalid";
pub const KEY_STATEMENT_NOT_FOUND: &str = "xml.statement.not_found";
pub const KEY_STATEMENT_AMBIGUOUS: &str = "xml.statement.ambiguous";
pub const KEY_STATEMENT_DUPLICATE: &str = "xml.statement.duplicate";
pub const KEY_FILE_UNPARSEABLE: &str = "xml.file.unparseable";

/// The four statement tags of a MyBatis mapper.
pub const STATEMENT_TAGS: [&str; 4] = ["select", "insert", "update", "delete"];

/// Result confidence. `Unresolved` means "could not render"; it never carries
/// SQL, only the reason (`notes` / `message_keys`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Exact,
    Partial,
    Unresolved,
}

/// One diagnostic attached to a render. `message_key` is stable and localised
/// elsewhere; `detail` is the machine-readable context (id, expression, …).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub message_key: String,
    pub detail: String,
}

/// A rendered statement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderOutput {
    pub sql: String,
    pub params: Vec<Value>,
    pub confidence: Confidence,
    pub notes: Vec<Note>,
    pub message_keys: Vec<String>,
}

impl RenderOutput {
    pub(crate) fn unresolved(key: &str, detail: impl Into<String>) -> Self {
        Self {
            sql: String::new(),
            params: Vec::new(),
            confidence: Confidence::Unresolved,
            notes: vec![Note {
                message_key: key.to_string(),
                detail: detail.into(),
            }],
            message_keys: vec![key.to_string()],
        }
    }

    /// Assembles a render plus the `#{}` placeholder metadata (C06).
    ///
    /// The names come from the un-tokenised SQL, which is why assembly has to
    /// run here: once `parse_parameters` turned a placeholder into `?`, the
    /// name is gone. `RenderOutput` keeps its public shape — the extra list is
    /// the caller's, so no other render path has to know about it.
    pub(crate) fn assemble_detailed(
        ctx: render::RenderCtx,
        raw_sql: &str,
    ) -> (Self, Vec<render::ParameterMeta>) {
        let mut notes = ctx.notes;
        let mut message_keys: Vec<String> = Vec::new();
        fn push_key(key: &str, keys: &mut Vec<String>) {
            if !keys.iter().any(|k| k == key) {
                keys.push(key.to_string());
            }
        }
        if let Some((key, detail)) = ctx.unresolved {
            notes.push(Note {
                message_key: key.clone(),
                detail,
            });
            push_key(&key, &mut message_keys);
            return (
                Self {
                    sql: String::new(),
                    params: Vec::new(),
                    confidence: Confidence::Unresolved,
                    notes: finalize_notes(notes),
                    message_keys,
                },
                Vec::new(),
            );
        }
        for note in &notes {
            push_key(&note.message_key, &mut message_keys);
        }
        let (sql, params, metas) = render::parse_parameters_meta(raw_sql, &ctx.bindings);
        let confidence = if ctx.partial {
            Confidence::Partial
        } else {
            Confidence::Exact
        };
        (
            Self {
                sql,
                params,
                confidence,
                notes: finalize_notes(notes),
                message_keys,
            },
            metas,
        )
    }
}

fn finalize_notes(notes: Vec<Note>) -> Vec<Note> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for note in notes {
        let key = format!("{}\u{0}{}", note.message_key, note.detail);
        if seen.insert(key) {
            out.push(note);
        }
    }
    out
}

/// What decides a statement's `<if>` / `<when>` branches during a render
/// (C07).
///
/// The preview page can switch a branch on or off, and its default is the
/// statement's *shape* (every branch that adds a clause on). Every other
/// caller — commands, MCP, providers — must keep evaluating the expressions,
/// because it renders the SQL a real call would produce. Three states keep the
/// two apart instead of silently changing what the shared render means.
#[derive(Debug, Clone, Copy)]
pub enum ConditionSwitches<'a> {
    /// Evaluate the `test` expressions (the historical behaviour).
    Evaluate,
    /// The preview's default truth (see `render::collect_conditions`).
    Defaults,
    /// The page's own switches: ids absent from the map keep their default.
    Page(&'a BTreeMap<String, bool>),
}

/// A rendered statement with the two lists the preview pane draws: the
/// `#{}` placeholders in SQL order (C06) and the condition switches of the
/// statement's tree (C07).
///
/// [`RenderOutput`] keeps its public shape for every other caller; this type is
/// what the workbench asks for.
#[derive(Debug, Clone, PartialEq)]
pub struct DetailedRender {
    pub output: RenderOutput,
    pub params: Vec<render::ParameterMeta>,
    pub conditions: Vec<render::ConditionView>,
}

impl DetailedRender {
    fn plain(output: RenderOutput) -> Self {
        Self {
            output,
            params: Vec::new(),
            conditions: Vec::new(),
        }
    }
}

/// Renders one statement of one mapper document.
///
/// `statement_id` accepts the bare id (`findById`) or the namespace-qualified
/// form (`com.example.UserMapper.findById`). Cross-namespace includes are not
/// resolvable here and degrade to the missing-include placeholder; use
/// [`MapperSet`] for that.
pub fn render_statement(xml: &str, statement_id: &str, params: &Value) -> RenderOutput {
    render_statement_detailed(xml, statement_id, params).output
}

/// [`render_statement`] with the placeholder and condition lists (C06/C07).
pub fn render_statement_detailed(xml: &str, statement_id: &str, params: &Value) -> DetailedRender {
    render_statement_detailed_with(xml, statement_id, params, ConditionSwitches::Evaluate)
}

/// [`render_statement_detailed`] with an explicit switch policy.
pub fn render_statement_detailed_with(
    xml: &str,
    statement_id: &str,
    params: &Value,
    switched: ConditionSwitches<'_>,
) -> DetailedRender {
    let root = match xmltree::parse_document(xml) {
        Ok(root) => root,
        Err(e) => {
            return DetailedRender::plain(RenderOutput::unresolved(KEY_PARSE_FAILED, e.message));
        }
    };
    if root.name != "mapper" {
        return DetailedRender::plain(RenderOutput::unresolved(
            KEY_ROOT_INVALID,
            format!("root element is <{}>, expected <mapper>", root.name),
        ));
    }
    let namespace = root.attr("namespace");
    let Some(element) = find_statement(&root, namespace, statement_id) else {
        return DetailedRender::plain(RenderOutput::unresolved(
            KEY_STATEMENT_NOT_FOUND,
            statement_id.to_string(),
        ));
    };
    let fragments = include::collect_fragments(&root);
    render_element(
        &element,
        &fragments,
        &BTreeMap::new(),
        namespace,
        params,
        switched,
    )
}

fn find_statement(root: &Element, namespace: Option<&str>, statement_id: &str) -> Option<Element> {
    let mut matches = root
        .child_elements()
        .filter(|e| STATEMENT_TAGS.contains(&e.name.as_str()))
        .filter(|e| {
            let id = e.attr_or_empty("id");
            if id == statement_id {
                return true;
            }
            namespace
                .map(|ns| format!("{ns}.{id}") == statement_id)
                .unwrap_or(false)
        });
    let first = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(first.clone())
}

fn render_element(
    element: &Element,
    local: &BTreeMap<String, Element>,
    cross: &BTreeMap<String, Element>,
    namespace: Option<&str>,
    params: &Value,
    switched: ConditionSwitches<'_>,
) -> DetailedRender {
    let mut statement = element.clone();
    let expansion = include::expand_includes(&mut statement, local, cross, namespace);
    if let Some((key, detail)) = expansion.fatal {
        let mut notes = expansion.notes;
        notes.push(Note {
            message_key: key.clone(),
            detail,
        });
        let mut keys = vec![key];
        for note in &notes {
            if !keys.iter().any(|k| k == &note.message_key) {
                keys.push(note.message_key.clone());
            }
        }
        return DetailedRender::plain(RenderOutput {
            sql: String::new(),
            params: Vec::new(),
            confidence: Confidence::Unresolved,
            notes: finalize_notes(notes),
            message_keys: keys,
        });
    }
    let node = match script::build(&statement) {
        Ok(node) => node,
        Err(e) => {
            return DetailedRender::plain(RenderOutput::unresolved(e.message_key, e.detail));
        }
    };
    render_nodes_detailed(&node, params, expansion.partial, expansion.notes, switched)
}

/// Renders an already-built node tree (used by annotations).
pub fn render_nodes(
    node: &script::SqlNode,
    params: &Value,
    partial: bool,
    notes: Vec<Note>,
) -> RenderOutput {
    render_nodes_detailed(node, params, partial, notes, ConditionSwitches::Evaluate).output
}

/// [`render_nodes`] with the condition switches and the detail lists (C06/C07).
///
/// `switched` decides the branches: [`ConditionSwitches::Evaluate`] keeps the
/// historical behaviour (the `test` expressions decide), `Defaults` renders the
/// statement's shape for the preview, and `Page` layers the reader's switches
/// on top of those defaults. Whichever way it is called, the render runs on the
/// *effective* switches, so the SQL and the `conditions` list never disagree.
pub fn render_nodes_detailed(
    node: &script::SqlNode,
    params: &Value,
    partial: bool,
    notes: Vec<Note>,
    switched: ConditionSwitches<'_>,
) -> DetailedRender {
    // Collected before the render: the collection pass walks the whole tree,
    // while the render pass skips false branches. Both number the nodes by
    // position, so the two agree on every id.
    let defaults = render::collect_conditions(node);
    let mut effective: BTreeMap<String, bool> = BTreeMap::new();
    match switched {
        ConditionSwitches::Evaluate => {}
        ConditionSwitches::Defaults => {
            for view in &defaults {
                effective.insert(view.id.clone(), view.enabled);
            }
        }
        ConditionSwitches::Page(page) => {
            for view in &defaults {
                let enabled = page.get(&view.id).copied().unwrap_or(view.enabled);
                effective.insert(view.id.clone(), enabled);
            }
        }
    }
    let conditions = render::collect_conditions_with(node, &effective);
    let mut ctx = render::RenderCtx::new(params);
    ctx.partial = partial;
    ctx.notes = notes;
    ctx.conditions = effective;
    let raw = render::render_tree(node, &mut ctx);
    let (output, params) = RenderOutput::assemble_detailed(ctx, &raw);
    DetailedRender {
        output,
        params,
        conditions,
    }
}

/// One loaded mapper document inside a [`MapperSet`].
struct MapperFile {
    namespace: Option<String>,
    fragments: BTreeMap<String, Element>,
    root: Element,
}

/// A statement located in a file (index into `MapperSet::files`).
struct StatementEntry {
    id: String,
    qualified: String,
    file: usize,
}

/// A load-time problem (file that cannot be parsed, duplicate id, …).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadIssue {
    pub path: String,
    pub message_key: String,
    pub detail: String,
}

/// Several mapper documents, so `namespace.id` includes resolve across files.
pub struct MapperSet {
    files: Vec<MapperFile>,
    fragments: BTreeMap<String, Element>,
    statements: Vec<StatementEntry>,
}

impl MapperSet {
    /// Parses every `(path, text)` pair. Files that fail to parse are reported
    /// in the returned issues and skipped; loading never panics.
    pub fn load(docs: &[(&str, &str)]) -> (Self, Vec<LoadIssue>) {
        let mut issues = Vec::new();
        let mut files: Vec<MapperFile> = Vec::new();
        let mut fragments: BTreeMap<String, Element> = BTreeMap::new();
        let mut statements: Vec<StatementEntry> = Vec::new();
        for (path, text) in docs {
            let root = match xmltree::parse_document(text) {
                Ok(root) => root,
                Err(e) => {
                    issues.push(LoadIssue {
                        path: (*path).to_string(),
                        message_key: KEY_FILE_UNPARSEABLE.to_string(),
                        detail: e.message,
                    });
                    continue;
                }
            };
            if root.name != "mapper" {
                issues.push(LoadIssue {
                    path: (*path).to_string(),
                    message_key: KEY_ROOT_INVALID.to_string(),
                    detail: format!("root element is <{}>", root.name),
                });
                continue;
            }
            let namespace = root.attr("namespace").map(|s| s.to_string());
            let local = include::collect_fragments(&root);
            let file_index = files.len();
            // Only qualified keys are shared across files: a bare refid means
            // "current namespace" in MyBatis, so registering `id` globally
            // could silently bind a fragment from an unrelated mapper.
            if let Some(ns) = &namespace {
                for (id, element) in &local {
                    let qualified = format!("{ns}.{id}");
                    match fragments.entry(qualified) {
                        std::collections::btree_map::Entry::Occupied(entry) => {
                            issues.push(LoadIssue {
                                path: (*path).to_string(),
                                message_key: KEY_STATEMENT_DUPLICATE.to_string(),
                                detail: entry.key().clone(),
                            });
                        }
                        std::collections::btree_map::Entry::Vacant(slot) => {
                            slot.insert(element.clone());
                        }
                    }
                }
            }
            for element in root.child_elements() {
                if !STATEMENT_TAGS.contains(&element.name.as_str()) {
                    continue;
                }
                let id = element.attr_or_empty("id").to_string();
                if id.is_empty() {
                    continue;
                }
                let qualified = match &namespace {
                    Some(ns) => format!("{ns}.{id}"),
                    None => id.clone(),
                };
                if statements.iter().any(|entry| entry.qualified == qualified) {
                    issues.push(LoadIssue {
                        path: (*path).to_string(),
                        message_key: KEY_STATEMENT_DUPLICATE.to_string(),
                        detail: qualified,
                    });
                    continue;
                }
                statements.push(StatementEntry {
                    id,
                    qualified,
                    file: file_index,
                });
            }
            files.push(MapperFile {
                namespace,
                fragments: local,
                root,
            });
        }
        (
            Self {
                files,
                fragments,
                statements,
            },
            issues,
        )
    }

    /// All statement ids in both bare and qualified form (for tests and UI).
    pub fn statement_ids(&self) -> Vec<String> {
        self.statements
            .iter()
            .flat_map(|e| {
                if e.id == e.qualified {
                    vec![e.id.clone()]
                } else {
                    vec![e.id.clone(), e.qualified.clone()]
                }
            })
            .collect()
    }

    /// Renders `qualified_id` (`namespace.id`) or a bare id unique in the set.
    pub fn render(&self, qualified_id: &str, params: &Value) -> RenderOutput {
        self.render_detailed(qualified_id, params, ConditionSwitches::Evaluate)
            .output
    }

    /// [`MapperSet::render`] with the placeholder and condition lists, and an
    /// explicit branch policy (C06/C07).
    pub fn render_detailed(
        &self,
        qualified_id: &str,
        params: &Value,
        switched: ConditionSwitches<'_>,
    ) -> DetailedRender {
        let exact: Vec<&StatementEntry> = self
            .statements
            .iter()
            .filter(|e| e.qualified == qualified_id)
            .collect();
        let matches: Vec<&StatementEntry> = if exact.is_empty() {
            self.statements
                .iter()
                .filter(|e| e.id == qualified_id)
                .collect()
        } else {
            exact
        };
        match matches.len() {
            0 => DetailedRender::plain(RenderOutput::unresolved(
                KEY_STATEMENT_NOT_FOUND,
                qualified_id.to_string(),
            )),
            1 => {
                let entry = matches[0];
                let file = &self.files[entry.file];
                let element = file
                    .root
                    .child_elements()
                    .find(|e| {
                        STATEMENT_TAGS.contains(&e.name.as_str())
                            && e.attr_or_empty("id") == entry.id
                    })
                    .cloned();
                let Some(element) = element else {
                    return DetailedRender::plain(RenderOutput::unresolved(
                        KEY_STATEMENT_NOT_FOUND,
                        qualified_id.to_string(),
                    ));
                };
                render_element(
                    &element,
                    &file.fragments,
                    &self.fragments,
                    file.namespace.as_deref(),
                    params,
                    switched,
                )
            }
            _ => DetailedRender::plain(RenderOutput::unresolved(
                KEY_STATEMENT_AMBIGUOUS,
                qualified_id.to_string(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Collapses runs of whitespace. XML text nodes keep the source indentation
    /// and sibling segments are joined with one space, exactly like the
    /// reference engine; assertions on multi-line fixtures compare the
    /// whitespace-normalised form.
    fn compact(sql: &str) -> String {
        sql.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    const MAPPER: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE mapper PUBLIC "-//mybatis.org//DTD Mapper 3.0//EN" "http://mybatis.org/dtd/mybatis-3-mapper.dtd">
<mapper namespace="com.example.UserMapper">
    <sql id="cols">id, name</sql>
    <select id="findById" resultType="User">
        SELECT <include refid="cols"/> FROM users WHERE id = #{id}
    </select>
    <update id="update">
        <set>
            <if test="name != null">name = #{name},</if>
            <if test="age != null">age = #{age},</if>
        </set>
        WHERE id = #{id}
    </update>
    <insert id="insert">
        INSERT INTO users (id, name) VALUES (#{id}, #{name})
    </insert>
    <delete id="deleteById">DELETE FROM users WHERE id = #{id}</delete>
</mapper>"#;

    #[test]
    fn renders_a_statement_by_bare_id() {
        let out = render_statement(MAPPER, "findById", &json!({"id": 3}));
        assert_eq!(out.confidence, Confidence::Exact);
        assert_eq!(compact(&out.sql), "SELECT id, name FROM users WHERE id = ?");
        assert_eq!(out.params, vec![json!(3)]);
        assert!(out.notes.is_empty());
    }

    #[test]
    fn renders_a_statement_by_qualified_id() {
        let out = render_statement(MAPPER, "com.example.UserMapper.findById", &json!({"id": 3}));
        assert_eq!(out.confidence, Confidence::Exact);
        assert!(out.sql.contains("FROM users"));
    }

    #[test]
    fn update_uses_set_and_drops_the_last_comma() {
        let out = render_statement(
            MAPPER,
            "update",
            &json!({"name": "Ada", "age": 36, "id": 3}),
        );
        assert_eq!(out.confidence, Confidence::Exact);
        assert_eq!(compact(&out.sql), "SET name = ?, age = ? WHERE id = ?");
        assert_eq!(out.params, vec![json!("Ada"), json!(36), json!(3)]);
    }

    #[test]
    fn update_with_one_branch_still_drops_the_comma() {
        let out = render_statement(MAPPER, "update", &json!({"name": "Ada", "id": 3}));
        assert_eq!(compact(&out.sql), "SET name = ? WHERE id = ?");
    }

    #[test]
    fn missing_statement_is_unresolved_with_empty_sql() {
        let out = render_statement(MAPPER, "nope", &json!({}));
        assert_eq!(out.confidence, Confidence::Unresolved);
        assert_eq!(out.sql, "");
        assert_eq!(out.message_keys, vec![KEY_STATEMENT_NOT_FOUND]);
    }

    #[test]
    fn malformed_xml_is_unresolved() {
        let out = render_statement("<mapper><select id='a'>", "a", &json!({}));
        assert_eq!(out.confidence, Confidence::Unresolved);
        assert_eq!(out.message_keys, vec![KEY_PARSE_FAILED]);
    }

    #[test]
    fn wrong_root_is_unresolved() {
        let out = render_statement("<configuration/>", "a", &json!({}));
        assert_eq!(out.confidence, Confidence::Unresolved);
        assert_eq!(out.message_keys, vec![KEY_ROOT_INVALID]);
    }

    #[test]
    fn confidence_serialises_snake_case() {
        assert_eq!(
            serde_json::to_string(&Confidence::Unresolved).unwrap(),
            "\"unresolved\""
        );
        assert_eq!(
            serde_json::to_string(&Confidence::Partial).unwrap(),
            "\"partial\""
        );
    }

    const CROSS_A: &str = r#"<mapper namespace="a">
        <sql id="shared">id, name</sql>
        <select id="one"><include refid="shared"/> FROM t</select>
    </mapper>"#;

    const CROSS_B: &str = r#"<mapper namespace="b">
        <select id="two">SELECT <include refid="a.shared"/> FROM t</select>
        <select id="three">SELECT <include refid="missing.frag"/> FROM t</select>
    </mapper>"#;

    #[test]
    fn mapper_set_resolves_cross_namespace_include() {
        let (set, issues) = MapperSet::load(&[("a.xml", CROSS_A), ("b.xml", CROSS_B)]);
        assert!(issues.is_empty(), "{issues:?}");
        let out = set.render("b.two", &json!({}));
        assert_eq!(out.confidence, Confidence::Exact);
        assert_eq!(compact(&out.sql), "SELECT id, name FROM t");
    }

    #[test]
    fn mapper_set_missing_cross_include_is_partial_with_placeholder() {
        let (set, _) = MapperSet::load(&[("a.xml", CROSS_A), ("b.xml", CROSS_B)]);
        let out = set.render("b.three", &json!({}));
        assert_eq!(out.confidence, Confidence::Partial);
        assert_eq!(
            compact(&out.sql),
            "SELECT -- [mpsv] include not found: missing.frag FROM t"
        );
        assert!(
            out.message_keys
                .contains(&include::KEY_INCLUDE_NOT_FOUND.to_string())
        );
    }

    #[test]
    fn mapper_set_cycle_is_unresolved() {
        let xml = r#"<mapper namespace="a">
            <sql id="one"><include refid="two"/></sql>
            <sql id="two"><include refid="one"/></sql>
            <select id="s">SELECT <include refid="one"/></select>
        </mapper>"#;
        let (set, _) = MapperSet::load(&[("a.xml", xml)]);
        let out = set.render("a.s", &json!({}));
        assert_eq!(out.confidence, Confidence::Unresolved);
        assert_eq!(out.sql, "");
        assert_eq!(out.message_keys, vec![include::KEY_INCLUDE_CYCLE]);
    }

    #[test]
    fn mapper_set_reports_unparseable_files_and_duplicates() {
        let (set, issues) = MapperSet::load(&[
            ("bad.xml", "<mapper>"),
            (
                "a.xml",
                "<mapper namespace=\"a\"><select id=\"s\">1</select></mapper>",
            ),
            (
                "a2.xml",
                "<mapper namespace=\"a\"><select id=\"s\">2</select></mapper>",
            ),
        ]);
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].message_key, KEY_FILE_UNPARSEABLE);
        assert_eq!(issues[1].message_key, KEY_STATEMENT_DUPLICATE);
        assert_eq!(set.render("a.s", &json!({})).sql, "1");
    }

    #[test]
    fn bare_refid_does_not_resolve_into_another_namespace() {
        let no_namespace = r#"<mapper><sql id="cols">x</sql><select id="s">SELECT <include refid="cols"/> FROM t</select></mapper>"#;
        let other = r#"<mapper namespace="b"><select id="t">SELECT <include refid="cols"/> FROM t</select></mapper>"#;
        let (set, _) = MapperSet::load(&[("a.xml", no_namespace), ("b.xml", other)]);
        // Same-file fragment still resolves.
        assert_eq!(compact(&set.render("s", &json!({})).sql), "SELECT x FROM t");
        // A bare refid in namespace `b` must not pick up the unrelated file's
        // fragment, even though it is the only `cols` in the set.
        let out = set.render("b.t", &json!({}));
        assert_eq!(out.confidence, Confidence::Partial);
        assert!(out.sql.contains("include not found: cols"));
    }

    #[test]
    fn mapper_set_unknown_id_is_unresolved() {
        let (set, _) = MapperSet::load(&[("a.xml", CROSS_A)]);
        let out = set.render("a.nope", &json!({}));
        assert_eq!(out.confidence, Confidence::Unresolved);
        assert_eq!(out.sql, "");
    }

    #[test]
    fn includes_nested_properties_and_dynamic_tags_are_expanded() {
        let xml = r#"<mapper namespace="n">
            <sql id="body"><if test="flag">${col} = #{v}</if></sql>
            <select id="s">SELECT <include refid="body"><property name="col" value="status"/></include> FROM t</select>
        </mapper>"#;
        let out = render_statement(xml, "s", &json!({"flag": true, "v": "A"}));
        assert_eq!(out.confidence, Confidence::Exact);
        assert_eq!(compact(&out.sql), "SELECT status = ? FROM t");
        assert_eq!(out.params, vec![json!("A")]);
    }

    #[test]
    fn where_foreach_statement_matches_reference_spacing() {
        let xml = r#"<mapper namespace="n"><select id="s">
            SELECT * FROM t
            <where><foreach collection="ids" item="id" open="id IN (" separator="," close=")">#{id}</foreach></where>
        </select></mapper>"#;
        let out = render_statement(xml, "s", &json!({"ids": [1, 2, 3]}));
        assert_eq!(out.confidence, Confidence::Exact);
        assert_eq!(compact(&out.sql), "SELECT * FROM t WHERE id IN (?,?,?)");
        assert_eq!(out.params, vec![json!(1), json!(2), json!(3)]);
    }
}
