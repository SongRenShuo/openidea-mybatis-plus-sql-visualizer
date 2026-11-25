//! Annotation SQL rendering (T1-4): `@Select`/`@Update`/`@Insert`/`@Delete`
//! and `<script>` bodies.
//!
//! Lane L1 owns this file; see `docs/TASKS.md` T1-4.
//!
//! Provider annotations (`@SelectProvider` / `@InsertProvider` / …) are out of
//! scope: they require evaluating user code, which a text-level extractor
//! cannot do. Occurrences inside comments or string literals are matched like
//! real annotations (the extractor has no PSI); the navigation lane can prefer
//! PSI results where it has them.
//!
//! Two layers, deliberately separate:
//!
//! 1. [`parse_annotations`] works on **Java source text** (no PSI): it extracts
//!    the SQL constant of each statement annotation together with the byte
//!    range of the annotation and the line / offset of the annotated method,
//!    which the navigation and diagnostics lanes need;
//! 2. [`render_annotated`] turns an extracted statement into SQL. Plain
//!    annotation SQL is handled as one text node (`${}` / `#{}` behave exactly
//!    like mapper text; `<` and `>` need no escaping), while a `<script>` body
//!    is parsed and rendered through the same node tree as mapper XML.
//!
//! MyBatis joins array values with a space (`String.join(" ", value)`) and
//! concatenates `+`-joined constants without a separator; both rules are
//! covered by tests.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::xml::RenderOutput;
use super::xml::render_nodes;
use super::xml::script::{SqlNode, build_script};
use super::xml::token::contains_token;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnnotationKind {
    Select,
    Insert,
    Update,
    Delete,
}

impl AnnotationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Select => "select",
            Self::Insert => "insert",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }

    pub fn from_annotation_name(name: &str) -> Option<Self> {
        match name {
            "Select" => Some(Self::Select),
            "Insert" => Some(Self::Insert),
            "Update" => Some(Self::Update),
            "Delete" => Some(Self::Delete),
            _ => None,
        }
    }
}

/// One statement found in Java source text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnnotatedStatement {
    pub kind: AnnotationKind,
    /// SQL constant, already joined per MyBatis rules.
    pub sql: String,
    /// 1-based line of the annotation itself.
    pub line: usize,
    /// Byte offset of the `@`.
    pub annotation_start: usize,
    /// Byte offset just past the annotation's closing `)`.
    pub annotation_end: usize,
    pub method_name: Option<String>,
    /// 1-based line of the annotated method declaration.
    pub method_line: Option<usize>,
    /// Byte offset of the method name.
    pub method_start: Option<usize>,
    /// True when the SQL constant starts with `<script>`.
    pub is_script: bool,
}

/// Extracts every `@Select` / `@Insert` / `@Update` / `@Delete` with a string
/// (or string-array) value. Malformed annotations are skipped, never panicked.
pub fn parse_annotations(java_text: &str) -> Vec<AnnotatedStatement> {
    let bytes = java_text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'@' {
            i += 1;
            continue;
        }
        if i > 0 && is_identifier_byte(bytes[i - 1]) {
            i += 1;
            continue;
        }
        let Some((kind, name_end)) = annotation_name(java_text, i + 1) else {
            i += 1;
            continue;
        };
        let Some(pos) = skip_whitespace(java_text, name_end) else {
            i += 1;
            continue;
        };
        if pos >= bytes.len() || bytes[pos] != b'(' {
            i += 1;
            continue;
        }
        let Some((sql, after_args)) = parse_annotation_value(java_text, pos + 1) else {
            i += 1;
            continue;
        };
        let Some(after_close) = skip_whitespace(java_text, after_args) else {
            i += 1;
            continue;
        };
        if after_close >= bytes.len() || bytes[after_close] != b')' {
            i += 1;
            continue;
        }
        let annotation_end = after_close + 1;
        let (method_name, method_start) = find_method_after(java_text, annotation_end);
        out.push(AnnotatedStatement {
            kind,
            is_script: sql.trim_start().starts_with("<script>"),
            sql,
            line: line_of(java_text, i),
            annotation_start: i,
            annotation_end,
            method_line: method_start.map(|p| line_of(java_text, p)),
            method_name,
            method_start,
        });
        i = annotation_end;
    }
    out
}

/// Renders an extracted annotation statement.
pub fn render_annotated(statement: &AnnotatedStatement, params: &Value) -> RenderOutput {
    if statement.is_script {
        return match build_script(&statement.sql) {
            Ok(node) => render_nodes(&node, params, false, Vec::new()),
            Err(e) => RenderOutput::unresolved(e.message_key, e.detail),
        };
    }
    let node = if contains_token(&statement.sql, "${", "}") {
        SqlNode::Mixed(vec![SqlNode::Text(statement.sql.clone())])
    } else {
        SqlNode::Mixed(vec![SqlNode::StaticText(statement.sql.clone())])
    };
    render_nodes(&node, params, false, Vec::new())
}

fn is_identifier_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

fn annotation_name(text: &str, start: usize) -> Option<(AnnotationKind, usize)> {
    let bytes = text.as_bytes();
    let mut end = start;
    while end < bytes.len() && is_identifier_byte(bytes[end]) {
        end += 1;
    }
    let name = &text[start..end];
    AnnotationKind::from_annotation_name(name).map(|kind| (kind, end))
}

fn skip_whitespace(text: &str, mut pos: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
        pos += 1;
    }
    Some(pos)
}

/// Parses the annotation value (single string, `+` concatenation, or `{...}`
/// array) starting right after the opening `(`. Returns the joined SQL and the
/// offset just past the value (before the closing `)`).
fn parse_annotation_value(text: &str, pos: usize) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    let mut pos = skip_whitespace(text, pos)?;
    if pos < bytes.len() && bytes[pos] == b'{' {
        pos += 1;
        let mut items = Vec::new();
        loop {
            pos = skip_whitespace(text, pos)?;
            if pos >= bytes.len() {
                return None;
            }
            if bytes[pos] == b'}' {
                pos += 1;
                break;
            }
            let (item, next) = parse_constant(text, pos)?;
            items.push(item);
            pos = skip_whitespace(text, next)?;
            if pos < bytes.len() && bytes[pos] == b',' {
                pos += 1;
                continue;
            }
            if pos < bytes.len() && bytes[pos] == b'}' {
                pos += 1;
                break;
            }
            return None;
        }
        return Some((items.join(" "), pos));
    }
    parse_constant(text, pos)
}

/// Parses `"a"`, `"a" + "b"` (no separator, as Java concatenates).
fn parse_constant(text: &str, pos: usize) -> Option<(String, usize)> {
    let (first, mut next) = parse_string_literal(text, pos)?;
    let mut joined = first;
    loop {
        let probe = skip_whitespace(text, next)?;
        if probe < text.len() && text.as_bytes()[probe] == b'+' {
            let after_plus = skip_whitespace(text, probe + 1)?;
            let (part, after) = parse_string_literal(text, after_plus)?;
            joined.push_str(&part);
            next = after;
        } else {
            return Some((joined, next));
        }
    }
}

fn parse_string_literal(text: &str, mut pos: usize) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    if pos >= bytes.len() || bytes[pos] != b'"' {
        return None;
    }
    pos += 1;
    let mut out = String::new();
    while pos < bytes.len() {
        let b = bytes[pos];
        if b == b'"' {
            return Some((out, pos + 1));
        }
        if b == b'\\' {
            pos += 1;
            if pos >= bytes.len() {
                return None;
            }
            let esc = bytes[pos];
            pos += 1;
            match esc {
                b'n' => out.push('\n'),
                b't' => out.push('\t'),
                b'r' => out.push('\r'),
                b'"' => out.push('"'),
                b'\'' => out.push('\''),
                b'\\' => out.push('\\'),
                b'b' => out.push('\u{8}'),
                b'f' => out.push('\u{c}'),
                b'u' => {
                    if pos + 4 > bytes.len() {
                        return None;
                    }
                    let hex = &text[pos..pos + 4];
                    let value = u32::from_str_radix(hex, 16).ok()?;
                    out.push(char::from_u32(value)?);
                    pos += 4;
                }
                other => out.push(other as char),
            }
            continue;
        }
        let ch = text[pos..].chars().next()?;
        out.push(ch);
        pos += ch.len_utf8();
    }
    None
}

/// Skips annotations and modifiers after an annotation, then reports the method
/// name and its offset. Stops at `;` / `{` / `}` without a parameter list.
fn find_method_after(text: &str, from: usize) -> (Option<String>, Option<usize>) {
    let bytes = text.as_bytes();
    let mut pos = from;
    let mut last_ident: Option<(String, usize)> = None;
    while pos < bytes.len() {
        let b = bytes[pos];
        if b.is_ascii_whitespace() {
            pos += 1;
            continue;
        }
        if b == b'@' {
            pos += 1;
            while pos < bytes.len() && is_identifier_byte(bytes[pos]) {
                pos += 1;
            }
            let probe = match skip_whitespace(text, pos) {
                Some(p) => p,
                None => break,
            };
            if probe < bytes.len() && bytes[probe] == b'(' {
                match skip_balanced(text, probe) {
                    Some(after) => pos = after,
                    None => break,
                }
            }
            continue;
        }
        if b == b'(' {
            return match last_ident {
                Some((name, start)) => (Some(name), Some(start)),
                None => (None, None),
            };
        }
        if b == b';' || b == b'{' || b == b'}' {
            return (None, None);
        }
        if is_identifier_byte(b) {
            let start = pos;
            while pos < bytes.len() && is_identifier_byte(bytes[pos]) {
                pos += 1;
            }
            last_ident = Some((text[start..pos].to_string(), start));
            continue;
        }
        if b == b'<'
            && let Some(after) = skip_angle(text, pos)
        {
            pos = after;
            continue;
        }
        pos += 1;
    }
    (None, None)
}

fn skip_balanced(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut pos = start;
    while pos < bytes.len() {
        match bytes[pos] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(pos + 1);
                }
            }
            b'"' => {
                let (_, next) = parse_string_literal(text, pos)?;
                pos = next;
                continue;
            }
            _ => {}
        }
        pos += 1;
    }
    None
}

fn skip_angle(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut pos = start;
    let mut depth = 0i32;
    while pos < bytes.len() {
        match bytes[pos] {
            b'<' => depth += 1,
            b'>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(pos + 1);
                }
            }
            b'\n' => return None,
            _ => {}
        }
        pos += 1;
    }
    None
}

fn line_of(text: &str, offset: usize) -> usize {
    let end = offset.min(text.len());
    text[..end].matches('\n').count() + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::xml::Confidence;
    use serde_json::json;

    const JAVA: &str = r#"
package com.example;

public interface UserMapper {
    @Select("SELECT id, name FROM users WHERE id = #{id}")
    User findById(@Param("id") Long id);

    @Insert({
        "INSERT INTO users (id, name)",
        "VALUES (#{id}, #{name})"
    })
    int insert(User user);

    @Update("UPDATE users SET name = #{name}" +
            " WHERE id = #{id}")
    int rename(@Param("name") String name, @Param("id") Long id);

    @Delete("DELETE FROM users WHERE id = #{id}")
    int deleteById(Long id);

    @Select("<script>SELECT * FROM users <where><if test='name != null'>name = #{name}</if></where></script>")
    java.util.List<User> search(@Param("name") String name);
}
"#;

    #[test]
    fn finds_all_four_annotation_kinds_in_order() {
        let statements = parse_annotations(JAVA);
        assert_eq!(statements.len(), 5);
        assert_eq!(statements[0].kind, AnnotationKind::Select);
        assert_eq!(statements[1].kind, AnnotationKind::Insert);
        assert_eq!(statements[2].kind, AnnotationKind::Update);
        assert_eq!(statements[3].kind, AnnotationKind::Delete);
        assert_eq!(statements[4].kind, AnnotationKind::Select);
    }

    #[test]
    fn single_string_value() {
        let statements = parse_annotations(JAVA);
        assert_eq!(
            statements[0].sql,
            "SELECT id, name FROM users WHERE id = #{id}"
        );
        assert!(!statements[0].is_script);
    }

    #[test]
    fn array_value_is_joined_with_a_space() {
        let statements = parse_annotations(JAVA);
        assert_eq!(
            statements[1].sql,
            "INSERT INTO users (id, name) VALUES (#{id}, #{name})"
        );
    }

    #[test]
    fn plus_concatenation_has_no_separator() {
        let statements = parse_annotations(JAVA);
        assert_eq!(
            statements[2].sql,
            "UPDATE users SET name = #{name} WHERE id = #{id}"
        );
    }

    #[test]
    fn method_name_and_lines_are_reported() {
        let statements = parse_annotations(JAVA);
        assert_eq!(statements[0].method_name.as_deref(), Some("findById"));
        assert_eq!(statements[0].method_line, Some(6));
        assert_eq!(statements[0].line, 5);
        assert_eq!(statements[1].method_name.as_deref(), Some("insert"));
        assert_eq!(statements[2].method_name.as_deref(), Some("rename"));
    }

    #[test]
    fn annotation_and_method_offsets_point_at_the_source() {
        let statements = parse_annotations(JAVA);
        let first = &statements[0];
        assert!(JAVA[first.annotation_start..].starts_with("@Select"));
        assert!(JAVA[..first.annotation_end].ends_with(")"));
        assert!(JAVA[first.method_start.unwrap()..].starts_with("findById"));
    }

    #[test]
    fn script_body_is_flagged_and_rendered_with_dynamic_tags() {
        let statements = parse_annotations(JAVA);
        let script = &statements[4];
        assert!(script.is_script);
        let out = render_annotated(script, &json!({"name": "Ada"}));
        assert_eq!(out.confidence, Confidence::Exact);
        assert_eq!(out.sql, "SELECT * FROM users  WHERE name = ?");
        assert_eq!(out.params, vec![json!("Ada")]);
    }

    #[test]
    fn script_without_matching_branch_renders_the_base_select() {
        let statements = parse_annotations(JAVA);
        let out = render_annotated(&statements[4], &json!({}));
        assert_eq!(out.sql, "SELECT * FROM users");
        assert_eq!(out.confidence, Confidence::Exact);
    }

    #[test]
    fn plain_annotation_renders_parameters_in_order() {
        let statements = parse_annotations(JAVA);
        let out = render_annotated(&statements[0], &json!({"id": 9}));
        assert_eq!(out.sql, "SELECT id, name FROM users WHERE id = ?");
        assert_eq!(out.params, vec![json!(9)]);
        assert_eq!(out.confidence, Confidence::Exact);
    }

    #[test]
    fn plain_annotation_dollar_substitution() {
        let java = r#"interface M { @Select("SELECT ${col} FROM t") java.util.List<X> all(); }"#;
        let statements = parse_annotations(java);
        assert_eq!(statements.len(), 1);
        let out = render_annotated(&statements[0], &json!({"col": "id"}));
        assert_eq!(out.sql, "SELECT id FROM t");
    }

    #[test]
    fn plain_annotation_keeps_unescaped_angle_brackets() {
        let java = r#"interface M { @Select("SELECT * FROM t WHERE a < #{b} AND c > 1") X f(); }"#;
        let statements = parse_annotations(java);
        let out = render_annotated(&statements[0], &json!({"b": 1}));
        assert_eq!(out.sql, "SELECT * FROM t WHERE a < ? AND c > 1");
    }

    #[test]
    fn escapes_inside_the_string_are_decoded() {
        let java = "interface M { @Select(\"SELECT \\\"x\\\" \\nFROM t\") X f(); }";
        let statements = parse_annotations(java);
        assert_eq!(statements[0].sql, "SELECT \"x\" \nFROM t");
    }

    #[test]
    fn unicode_escape_is_decoded() {
        let java = r#"interface M { @Select("SELECT \u0041") X f(); }"#;
        let statements = parse_annotations(java);
        assert_eq!(statements[0].sql, "SELECT A");
    }

    #[test]
    fn ignores_annotations_without_a_string_value() {
        let java = "class A { @Deprecated void x() {} @Select void y() {} }";
        assert!(parse_annotations(java).is_empty());
    }

    #[test]
    fn ignores_unterminated_annotations_without_panicking() {
        let java = "@Select(\"SELECT 1";
        assert!(parse_annotations(java).is_empty());
        let java = "@Select({ \"a\", 1 }) X f();";
        assert!(parse_annotations(java).is_empty());
    }

    #[test]
    fn skips_intervening_parameter_annotations_before_the_method() {
        let java = "interface M {\n  @Select(\"SELECT 1\")\n  @Override\n  java.util.List<X> find(\n    @Param(\"a\") int a\n  );\n}";
        let statements = parse_annotations(java);
        assert_eq!(statements[0].method_name.as_deref(), Some("find"));
        assert_eq!(statements[0].method_line, Some(4));
    }

    #[test]
    fn multiple_statements_keep_source_order() {
        let java = "interface M {\n@Select(\"a\") X a();\n@Delete(\"b\") X b();\n}";
        let statements = parse_annotations(java);
        assert_eq!(statements[0].sql, "a");
        assert_eq!(statements[1].sql, "b");
        assert!(statements[0].annotation_start < statements[1].annotation_start);
    }

    #[test]
    fn script_with_unknown_element_is_unresolved() {
        let java =
            r#"interface M { @Select("<script>SELECT <include refid='x'/></script>") X f(); }"#;
        let statements = parse_annotations(java);
        let out = render_annotated(&statements[0], &json!({}));
        assert_eq!(out.confidence, Confidence::Unresolved);
        assert_eq!(out.sql, "");
    }

    #[test]
    fn empty_array_value_yields_empty_sql() {
        let java = r#"interface M { @Select({}) X f(); }"#;
        let statements = parse_annotations(java);
        assert_eq!(statements[0].sql, "");
        assert_eq!(render_annotated(&statements[0], &json!({})).sql, "");
    }

    #[test]
    fn at_sign_inside_a_string_is_not_an_annotation() {
        let java = "class A { String email = \"a@b\"; @Select(\"ok\") X f(); }";
        let statements = parse_annotations(java);
        assert_eq!(statements.len(), 1);
        assert_eq!(statements[0].sql, "ok");
    }
}
