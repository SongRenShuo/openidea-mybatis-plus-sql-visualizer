//! In-place SQL preview on hover (lane L37).
//!
//! OpenIDEA has no plugin-reachable rich-content popup, so the closest thing to
//! the IntelliJ edition's `SqlPreviewDialog` is the platform `hover` provider:
//! the host asks for Markdown when the caret rests on a symbol, and this module
//! answers with the one statement behind that symbol, rendered through
//! [`crate::core::xml`].
//!
//! Trigger ranges (nothing else answers, so a hover never pops an empty card):
//!
//! | document | caret on | shown |
//! | --- | --- | --- |
//! | mapper XML | a `<select\|insert\|update\|delete>` start tag (id value, element name, attributes) | that statement's SQL |
//! | mapper XML | a `<sql id>` start tag | how often this file `<include>`s it |
//! | mapper XML | a `<resultMap id>` start tag | its `type` / `extends` and how many statements use it |
//! | Java | a mapper interface method name | the unique XML statement of the same `namespace`+`id`, rendered |
//! | Java | a mapper method name with several candidate statements | "N candidates, cannot choose one" |
//!
//! The provider renders **one** statement, never scans the project: the only
//! cross-document step (Java → XML) is a hash lookup in the process-wide
//! [`crate::indexer`] statement index the document indexer already fills, plus a
//! single `Project::read_file` of the file that index named. Everything stays
//! inside the edit-path budget.
//!
//! The render contract is the same as everywhere else: `exact` / `partial` carry
//! SQL, `unresolved` never does — it names the reason as `[message_key] detail`,
//! the workbench's own convention.

use serde_json::json;

use openidea_plugin::hover::{Document, Hover, HoverProvider};
use openidea_plugin::{Position, Range};

use crate::core::xml::{Confidence, RenderOutput};

// Chrome strings live in `src/i18n.rs`; core render reasons stay message keys.
const TITLE_SQL: &str = "hover.title.sql";
const TITLE_FRAGMENT: &str = "hover.title.fragment";
const TITLE_RESULTMAP: &str = "hover.title.resultmap";
const TITLE_METHOD: &str = "hover.title.method";
const FRAGMENT_USED_PREFIX: &str = "hover.fragment.used_prefix";
const FRAGMENT_USED_SUFFIX: &str = "hover.fragment.used_suffix";
const RESULTMAP_TYPE_PREFIX: &str = "hover.resultmap.type_prefix";
const RESULTMAP_EXTENDS_PREFIX: &str = "hover.resultmap.extends_prefix";
const RESULTMAP_USED_PREFIX: &str = "hover.resultmap.used_prefix";
const SQL_TRUNCATED_PREFIX: &str = "hover.sql.truncated_prefix";
const SQL_TRUNCATED_SUFFIX: &str = "hover.sql.truncated_suffix";
const AMBIGUOUS_PREFIX: &str = "hover.java.ambiguous_prefix";
const AMBIGUOUS_SUFFIX: &str = "hover.java.ambiguous_suffix";

/// Same key the capture path reads; the hover keeps its own copy so it does not
/// depend on the log module.
const MAX_SQL_LENGTH: &str = "log.maxSqlLength";
const MAX_SQL_DEFAULT: i64 = 20_000;
const MAX_SQL_FLOOR: i64 = 256;

impl HoverProvider for crate::Main {
    fn hover(&mut self, doc: &Document, at: Position) -> Option<Hover> {
        hover_in(doc, at)
    }
}

/// The pure hover function, separated from the trait so tests drive it directly.
pub fn hover_in(doc: &Document, at: Position) -> Option<Hover> {
    let offset = openidea_plugin::text::offset(&doc.text, at)?;
    if super::is_xml(&doc.path) {
        xml_hover(doc, offset)
    } else if super::is_java(&doc.path) {
        java_hover(doc, at)
    } else {
        None
    }
}

// --- mapper XML ---------------------------------------------------------

fn xml_hover(doc: &Document, offset: usize) -> Option<Hover> {
    // A non-mapper XML never reaches the tag scan.
    if !doc.text.contains("<mapper") {
        return None;
    }
    let elements = crate::indexer::scan::elements(&doc.text);
    // The start tag whose `[start, content_start)` covers the caret. Start tags
    // never overlap, so at most one matches; content (SQL body) is excluded.
    let element = elements
        .iter()
        .find(|e| e.start <= offset && offset < e.content_start)?;
    let mut positions = crate::indexer::scan::Positions::new(&doc.text);

    if crate::core::xml::STATEMENT_TAGS.contains(&element.name) {
        let attr = element.attr("id")?;
        let id = attr.value.trim();
        if id.is_empty() {
            return None;
        }
        let out = crate::core::xml::render_statement(&doc.text, id, &json!({}));
        let range = positions.range(attr.value_start, attr.value_end);
        return Some(statement_card(out).range(range));
    }

    match element.name {
        "sql" => {
            let attr = element.attr("id")?;
            let id = attr.value.trim();
            if id.is_empty() {
                return None;
            }
            let range = positions.range(attr.value_start, attr.value_end);
            Some(fragment_card(doc, id).range(range))
        }
        "resultMap" => {
            let attr = element.attr("id")?;
            let id = attr.value.trim();
            if id.is_empty() {
                return None;
            }
            let range = positions.range(attr.value_start, attr.value_end);
            result_map_card(doc, element, id).map(|hover| hover.range(range))
        }
        _ => None,
    }
}

// --- Java → XML ---------------------------------------------------------

fn java_hover(doc: &Document, at: Position) -> Option<Hover> {
    let facts = crate::indexer::index_java(&doc.path, &doc.text);
    let fqn = facts.fqn.as_deref()?;
    let method = facts
        .methods
        .iter()
        .find(|method| contains(&method.name.range, at))?;
    let name = method.name.name.trim();
    if name.is_empty() {
        return None;
    }
    let range = method.name.range;

    // Resolve inside the lock, copy out, drop the guard before touching a file.
    let hit = {
        let xml = crate::indexer::xml_statements();
        let hits = xml.lookup(fqn, name);
        match hits.len() {
            0 => Lookup::None,
            1 => Lookup::One {
                file: hits[0].file.clone(),
                id: hits[0].id.clone(),
            },
            n => Lookup::Many(n),
        }
    };

    match hit {
        Lookup::None => None,
        Lookup::Many(count) => Some(ambiguous_card(name, count).range(range)),
        Lookup::One { file, id } => {
            let text = read_project_file(&file)?;
            let out = crate::core::xml::render_statement(&text, &id, &json!({}));
            Some(statement_card(out).range(range))
        }
    }
}

/// What the statement index said about one `(namespace, method)` pair.
enum Lookup {
    None,
    One { file: String, id: String },
    Many(usize),
}

/// Reads a mapper file that the index named, through the project capability.
/// Absent project or unreadable file → `None` (no card).
fn read_project_file(path: &str) -> Option<String> {
    let project = openidea_plugin::host::project_of(path)?;
    project.read_file(path).ok()
}

// --- cards --------------------------------------------------------------

fn statement_card(out: RenderOutput) -> Hover {
    let confidence = confidence_name(out.confidence);
    let mut head = format!("**{}** · `{}`", crate::i18n::t(TITLE_SQL), confidence);

    // `partial` puts a short reason summary on the first line, per the contract.
    if out.confidence == Confidence::Partial {
        let keys = key_spans(&out);
        if !keys.is_empty() {
            head.push_str(" · ");
            head.push_str(&keys.join(", "));
        }
    }
    let mut blocks = vec![head];

    if out.confidence == Confidence::Unresolved {
        blocks.extend(reason_lines(&out));
    } else {
        blocks.push(sql_block(&out.sql));
    }
    Hover::new(blocks.join("\n\n"))
}

fn ambiguous_card(method: &str, count: usize) -> Hover {
    let head = format!(
        "**{}** · {}",
        crate::i18n::t(TITLE_METHOD),
        inline_code(method)
    );
    let sentence = format!(
        "{} {} {}",
        crate::i18n::t(AMBIGUOUS_PREFIX),
        count,
        crate::i18n::t(AMBIGUOUS_SUFFIX)
    );
    Hover::new(format!("{head}\n\n{sentence}"))
}

fn fragment_card(doc: &Document, id: &str) -> Hover {
    let facts = crate::indexer::index_xml(&doc.path, &doc.text);
    let namespace = facts.namespace.as_ref().map(|named| named.name.as_str());
    let count = facts
        .includes
        .iter()
        .filter(|include| refers_to(&include.name, id, namespace))
        .count();
    let head = format!(
        "**{}** · {}",
        crate::i18n::t(TITLE_FRAGMENT),
        inline_code(id)
    );
    let line = format!(
        "{} {} {}",
        crate::i18n::t(FRAGMENT_USED_PREFIX),
        count,
        crate::i18n::t(FRAGMENT_USED_SUFFIX)
    );
    Hover::new(format!("{head}\n\n{line}"))
}

fn result_map_card(
    doc: &Document,
    element: &crate::indexer::scan::Element<'_>,
    id: &str,
) -> Option<Hover> {
    let facts = crate::indexer::index_xml(&doc.path, &doc.text);
    let namespace = facts.namespace.as_ref().map(|named| named.name.as_str());
    let mut lines: Vec<String> = Vec::new();

    if let Some(type_attr) = element.attr("type") {
        let type_name = type_attr.value.trim();
        if !type_name.is_empty() {
            lines.push(format!(
                "{} {}",
                crate::i18n::t(RESULTMAP_TYPE_PREFIX),
                inline_code(type_name)
            ));
        }
    }
    if let Some(extends) = element.attr("extends") {
        let parent = extends.value.trim();
        if !parent.is_empty() {
            lines.push(format!(
                "{} {}",
                crate::i18n::t(RESULTMAP_EXTENDS_PREFIX),
                inline_code(parent)
            ));
        }
    }

    // Statements that name this map, plus nested association/collection/case
    // references (which `result_map_refs` repeats for statements; de-dupe).
    let statement_refs: Vec<&str> = facts
        .statements
        .iter()
        .filter_map(|statement| statement.result_map.as_ref().map(|rm| rm.name.as_str()))
        .collect();
    let used = facts
        .statements
        .iter()
        .filter(|statement| {
            statement
                .result_map
                .as_ref()
                .is_some_and(|rm| refers_to(&rm.name, id, namespace))
        })
        .count()
        + facts
            .result_map_refs
            .iter()
            .filter(|reference| {
                refers_to(&reference.name, id, namespace)
                    && !statement_refs.contains(&reference.name.as_str())
            })
            .count();
    if used > 0 {
        lines.push(format!(
            "{} {}",
            crate::i18n::t(RESULTMAP_USED_PREFIX),
            used
        ));
    }

    // No type, no parent, no user: nothing worth a card.
    if lines.is_empty() {
        return None;
    }
    let head = format!(
        "**{}** · {}",
        crate::i18n::t(TITLE_RESULTMAP),
        inline_code(id)
    );
    Some(Hover::new(format!("{head}\n\n{}", lines.join("\n"))))
}

// --- markdown helpers ---------------------------------------------------

fn sql_block(sql: &str) -> String {
    let cap = crate::settings::get_i64(MAX_SQL_LENGTH, MAX_SQL_DEFAULT).max(MAX_SQL_FLOOR) as usize;
    let total = sql.chars().count();
    let truncated = total > cap;
    let body: String = if truncated {
        sql.chars().take(cap).collect()
    } else {
        sql.to_string()
    };
    // A fence longer than any run of backticks in the SQL cannot be closed early.
    let fence = "`".repeat(longest_run(&body, '`').max(2) + 1);
    let mut out = format!("{fence}sql\n{body}\n{fence}");
    if truncated {
        out.push('\n');
        out.push_str(&format!(
            "{} {} {}",
            crate::i18n::t(SQL_TRUNCATED_PREFIX),
            cap,
            crate::i18n::t(SQL_TRUNCATED_SUFFIX)
        ));
    }
    out
}

fn confidence_name(confidence: Confidence) -> &'static str {
    match confidence {
        Confidence::Exact => "exact",
        Confidence::Partial => "partial",
        Confidence::Unresolved => "unresolved",
    }
}

/// `` `key` `` for every reason key, in order, deduplicated by `message_keys`.
fn key_spans(out: &RenderOutput) -> Vec<String> {
    out.message_keys
        .iter()
        .map(|key| format!("`{key}`"))
        .collect()
}

/// `[message_key] detail` lines, plus any key that has no note. The detail is
/// wrapped in an inline code span so markdown characters in an expression or an
/// id cannot restyle the card.
fn reason_lines(out: &RenderOutput) -> Vec<String> {
    let mut lines: Vec<String> = out
        .notes
        .iter()
        .map(|note| {
            let detail = inline_code(&note.detail);
            if detail.is_empty() {
                format!("[{}]", note.message_key)
            } else {
                format!("[{}] {}", note.message_key, detail)
            }
        })
        .collect();
    for key in &out.message_keys {
        if !out.notes.iter().any(|note| &note.message_key == key) {
            lines.push(format!("[{key}]"));
        }
    }
    lines
}

/// A markdown inline code span that survives any backtick run in `text`.
fn inline_code(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return String::new();
    }
    let fence = "`".repeat(longest_run(&collapsed, '`') + 1);
    format!("{fence}{collapsed}{fence}")
}

fn longest_run(text: &str, needle: char) -> usize {
    let mut longest = 0usize;
    let mut current = 0usize;
    for ch in text.chars() {
        if ch == needle {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

fn contains(range: &Range, at: Position) -> bool {
    range.start <= at && at < range.end
}

/// Does `reference` (bare `id` or `namespace.id`) name the local `id`?
fn refers_to(reference: &str, local: &str, namespace: Option<&str>) -> bool {
    match (reference.rsplit_once('.'), namespace) {
        (Some((ns, id)), Some(namespace)) => id == local && ns == namespace,
        _ => reference == local,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    use openidea_plugin::providers::Document;

    const MAPPER: &str = r#"<mapper namespace="com.acme.UserMapper">
    <sql id="cols">id, name</sql>
    <select id="findById" resultType="User">select <include refid="cols"/> from users where id = #{id}</select>
    <update id="touch">update users set name = #{name} where id = #{id}</update>
    <resultMap id="BaseMap" type="com.acme.User">
        <id column="id" property="id"/>
    </resultMap>
</mapper>"#;

    fn xml_doc(text: &str) -> Document {
        Document {
            path: "/project/src/main/resources/UserMapper.xml".into(),
            language: "xml".into(),
            version: 1,
            text: text.into(),
        }
    }

    fn java_doc(path: &str, text: &str) -> Document {
        Document {
            path: path.into(),
            language: "java".into(),
            version: 1,
            text: text.into(),
        }
    }

    /// The UTF-16 position 1-based into `needle`'s first occurrence.
    fn at(text: &str, needle: &str) -> Position {
        let offset = text.find(needle).expect("needle in text") + 1;
        openidea_plugin::text::position(text, offset)
    }

    #[test]
    fn hover_xml_statement_id_shows_sql_and_confidence() {
        let _host = MockHost::install();
        let hover = hover_in(&xml_doc(MAPPER), at(MAPPER, "findById")).expect("a hover");
        assert!(hover.markdown.contains("**SQL**"), "{}", hover.markdown);
        assert!(hover.markdown.contains("`exact`"), "{}", hover.markdown);
        assert!(hover.markdown.contains("```sql"), "{}", hover.markdown);
        assert!(
            hover.markdown.contains("from users where id = ?"),
            "{}",
            hover.markdown
        );
        // The range is the id value, so the host highlights the key.
        let range = hover.range.expect("range");
        assert_eq!(range.start.line, 2);
        assert_eq!(
            range.end.character - range.start.character,
            "findById".len() as u32
        );
    }

    #[test]
    fn hover_on_the_element_name_and_attributes_also_answers() {
        let _host = MockHost::install();
        // Caret on the tag name `<select` and on `resultType=` both sit in the
        // start tag, so both answer with the statement card.
        for needle in ["<select", "resultType"] {
            let hover = hover_in(&xml_doc(MAPPER), at(MAPPER, needle)).expect(needle);
            assert!(
                hover.markdown.contains("```sql"),
                "{needle}: {}",
                hover.markdown
            );
        }
    }

    #[test]
    fn hover_inside_the_sql_body_is_none() {
        let _host = MockHost::install();
        // "from users" is statement content, not the start tag.
        assert!(hover_in(&xml_doc(MAPPER), at(MAPPER, "from users")).is_none());
    }

    #[test]
    fn hover_unresolved_has_reason_but_no_sql() {
        let _host = MockHost::install();
        // An unclosed mapper: the statement tag is visible, rendering is not.
        let xml =
            "<mapper namespace=\"com.acme.UserMapper\"><select id=\"broken\">select * from users";
        let hover = hover_in(&xml_doc(xml), at(xml, "broken")).expect("a hover");
        assert!(
            hover.markdown.contains("`unresolved`"),
            "{}",
            hover.markdown
        );
        assert!(
            !hover.markdown.contains("```sql"),
            "no SQL block: {}",
            hover.markdown
        );
        assert!(
            hover.markdown.contains("[xml.parse.failed]"),
            "a reason key: {}",
            hover.markdown
        );
    }

    #[test]
    fn hover_partial_marks_the_first_line_and_still_shows_sql() {
        let _host = MockHost::install();
        // Cross-namespace include that this single document cannot resolve.
        let xml = r#"<mapper namespace="com.acme.UserMapper">
    <select id="usesOther">select * from t <include refid="other.ns.cols"/></select>
</mapper>"#;
        let hover = hover_in(&xml_doc(xml), at(xml, "usesOther")).expect("a hover");
        let first = hover.markdown.lines().next().unwrap();
        assert!(first.contains("`partial`"), "{first}");
        assert!(first.contains("xml.include.not_found"), "{first}");
        assert!(hover.markdown.contains("```sql"), "{}", hover.markdown);
    }

    #[test]
    fn hover_fragment_reports_include_count() {
        let _host = MockHost::install();
        let hover = hover_in(&xml_doc(MAPPER), at(MAPPER, "cols\"")).expect("a hover");
        assert!(
            hover.markdown.contains("SQL fragment"),
            "{}",
            hover.markdown
        );
        assert!(hover.markdown.contains("`cols`"), "{}", hover.markdown);
        // One `<include refid="cols"/>` in the document.
        assert!(hover.markdown.contains('1'), "{}", hover.markdown);
    }

    #[test]
    fn hover_result_map_reports_type_and_usage() {
        let _host = MockHost::install();
        let hover = hover_in(&xml_doc(MAPPER), at(MAPPER, "BaseMap")).expect("a hover");
        assert!(hover.markdown.contains("Result map"), "{}", hover.markdown);
        assert!(
            hover.markdown.contains("com.acme.User"),
            "{}",
            hover.markdown
        );
        // The select names resultType, not resultMap, so nothing uses BaseMap:
        // the card still shows because it has a type.
        assert!(hover.markdown.contains("`BaseMap`"), "{}", hover.markdown);
    }

    #[test]
    fn hover_non_mapper_xml_is_none() {
        let _host = MockHost::install();
        let doc = Document {
            path: "/project/pom.xml".into(),
            language: "xml".into(),
            version: 1,
            text: "<project><select id=\"x\">select 1</select></project>".into(),
        };
        assert!(hover_in(&doc, Position::new(0, 10)).is_none());
    }

    #[test]
    fn hover_other_extensions_are_none() {
        let _host = MockHost::install();
        let doc = Document {
            path: "/project/README.md".into(),
            language: "markdown".into(),
            version: 1,
            text: "select id from t".into(),
        };
        assert!(hover_in(&doc, Position::new(0, 1)).is_none());
    }

    const JAVA: &str = r#"package com.acme;
public interface UserMapper {
    User findById(Long id);
}
"#;

    fn seed_indexes(xml_files: &[(&str, &str)], java_files: &[(&str, &str)]) {
        crate::indexer::clear_indexes();
        for (path, text) in xml_files {
            crate::indexer::index_source(path, text);
        }
        for (path, text) in java_files {
            crate::indexer::index_source(path, text);
        }
    }

    #[test]
    fn hover_java_method_finds_the_unique_xml_statement() {
        let _host = MockHost::install();
        let _guard = crate::views::test_state_lock();
        let xml_path = "/project/src/main/resources/UserMapper.xml";
        _host.add_file(xml_path, MAPPER);
        seed_indexes(&[(xml_path, MAPPER)], &[]);

        let doc = java_doc("/project/src/main/java/com/acme/UserMapper.java", JAVA);
        let hover = hover_in(&doc, at(JAVA, "findById")).expect("a hover");
        assert!(hover.markdown.contains("```sql"), "{}", hover.markdown);
        assert!(
            hover.markdown.contains("where id = ?"),
            "{}",
            hover.markdown
        );
        crate::indexer::clear_indexes();
    }

    #[test]
    fn hover_java_method_with_several_candidates_does_not_guess() {
        let _host = MockHost::install();
        let _guard = crate::views::test_state_lock();
        let a = "/project/module-a/UserMapper.xml";
        let b = "/project/module-b/UserMapper.xml";
        _host.add_file(a, MAPPER);
        _host.add_file(b, MAPPER);
        seed_indexes(&[(a, MAPPER), (b, MAPPER)], &[]);

        let doc = java_doc("/project/src/main/java/com/acme/UserMapper.java", JAVA);
        let hover = hover_in(&doc, at(JAVA, "findById")).expect("a hover");
        assert!(hover.markdown.contains('2'), "{}", hover.markdown);
        assert!(
            !hover.markdown.contains("```sql"),
            "no SQL when ambiguous: {}",
            hover.markdown
        );
        crate::indexer::clear_indexes();
    }

    #[test]
    fn hover_java_method_without_an_xml_statement_is_none() {
        let _host = MockHost::install();
        let _guard = crate::views::test_state_lock();
        seed_indexes(&[], &[]);
        let doc = java_doc("/project/src/main/java/com/acme/UserMapper.java", JAVA);
        assert!(hover_in(&doc, at(JAVA, "findById")).is_none());
        crate::indexer::clear_indexes();
    }

    #[test]
    fn hover_java_non_method_position_is_none() {
        let _host = MockHost::install();
        let _guard = crate::views::test_state_lock();
        let xml_path = "/project/src/main/resources/UserMapper.xml";
        _host.add_file(xml_path, MAPPER);
        seed_indexes(&[(xml_path, MAPPER)], &[]);
        let doc = java_doc("/project/src/main/java/com/acme/UserMapper.java", JAVA);
        // On the interface name, not a method name.
        assert!(hover_in(&doc, at(JAVA, "UserMapper")).is_none());
        crate::indexer::clear_indexes();
    }

    #[test]
    fn hover_renders_one_statement_within_the_budget() {
        let _host = MockHost::install();
        let started = std::time::Instant::now();
        let hover = hover_in(&xml_doc(MAPPER), at(MAPPER, "findById"));
        let elapsed = started.elapsed();
        assert!(hover.is_some());
        // Loose ceiling: the soft provider budget is 50 ms for one statement.
        assert!(elapsed.as_millis() < 50, "took {elapsed:?}");
    }

    #[test]
    fn hover_truncates_very_long_sql_and_says_so() {
        let _host = MockHost::install();
        _host.set_setting("log.maxSqlLength", "256");
        let padding = "x".repeat(600);
        let xml = format!(
            "<mapper namespace=\"n\"><select id=\"big\">select '{padding}' from t</select></mapper>"
        );
        let hover = hover_in(&xml_doc(&xml), at(&xml, "big")).expect("a hover");
        assert!(hover.markdown.contains("truncated"), "{}", hover.markdown);
        // The visible SQL body is capped near 256 characters.
        let body_chars = hover
            .markdown
            .split("```sql\n")
            .nth(1)
            .and_then(|rest| rest.split("\n```").next())
            .map(|body| body.chars().count())
            .unwrap_or(0);
        assert!(body_chars <= 256, "body was {body_chars} chars");
    }

    #[test]
    fn inline_code_survives_backticks_and_newlines() {
        assert_eq!(inline_code("a\nb"), "`a b`");
        assert_eq!(inline_code("x`y"), "``x`y``");
        assert_eq!(inline_code(""), "");
    }
}
