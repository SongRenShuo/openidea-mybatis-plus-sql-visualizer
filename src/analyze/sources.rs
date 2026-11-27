//! Which source a piece of text is, and the one function each source renders
//! through.
//!
//! Three statement sources live outside mapper XML: statement annotations
//! (`@Select("…")`), iBatis 2 `<sqlMap>` documents, and MyBatis-Plus wrapper
//! chains. Each already had a renderer and tests but no caller; this module is
//! the single place that answers "which of them is this text, and which
//! statement of it" so the `preview-sql` command and the `sqlv_render_sql` MCP
//! tool cannot drift apart.
//!
//! It stays pure: no `host::`, no project access. The command finds the entity
//! class and the file; this module only reads what it is handed.
//!
//! # Not guessing
//!
//! Every "which one" question has a defined answer or none:
//! - an annotated method is chosen by name, by the caret's method span, or
//!   because the file has exactly one — never "the first of several";
//! - a wrapper's entity comes from the generic argument in the source, never
//!   from the variable name;
//! - a wrapper render whose field→column projection was derived (no
//!   `@TableField`) is downgraded to `partial`, because the project's
//!   `mapUnderscoreToCamelCase` setting is not visible from text.

use serde_json::Value;

use super::wrapper::{self, EntityColumns, WrapperOutput};
use crate::core::annotation::AnnotatedStatement;
use crate::core::dialect::Dialect;
use crate::core::ibatis;
use crate::core::xml::{Confidence, Note, RenderOutput, xmltree};

/// The wrapper classes whose generic argument names the entity.
const WRAPPER_TOKENS: [&str; 4] = [
    "LambdaUpdateWrapper",
    "LambdaQueryWrapper",
    "UpdateWrapper",
    "QueryWrapper",
];

/// The factory whose call names the entity in `Wrappers.<User>lambdaQuery()`.
const WRAPPERS_FACTORY: &str = "Wrappers";

/// The wrapper's entity class could not be found, so there is no column mapping.
pub const KEY_ENTITY_UNKNOWN: &str = "wrapper.entity.not_found";
/// Some field→column mappings (or the table) came from MyBatis-Plus defaults.
pub const KEY_COLUMN_DERIVED: &str = "wrapper.column.derived";
/// One free-text note from the wrapper renderer.
pub const KEY_WRAPPER_NOTE: &str = "wrapper.render.note";
/// An update wrapper with no condition has nothing to render.
pub const KEY_EMPTY_FRAGMENT: &str = "wrapper.empty_fragment";

/// No statement annotation with a string value was found.
pub const KEY_ANNOTATION_NOT_FOUND: &str = "annotation.not_found";
/// The named method carries no statement annotation.
pub const KEY_ANNOTATION_METHOD_NOT_FOUND: &str = "annotation.method.not_found";
/// Several statements and no method (or caret) to pick one with.
pub const KEY_ANNOTATION_METHOD_REQUIRED: &str = "annotation.method.required";
/// Overloads of the named method each carry a statement.
pub const KEY_ANNOTATION_METHOD_AMBIGUOUS: &str = "annotation.method.ambiguous";

/// A `<sqlMap>` with several statements and no `#id` to pick one.
pub const KEY_IBATIS_STATEMENT_REQUIRED: &str = "ibatis.statement.required";

// ---------------------------------------------------------------------------
// Source detection
// ---------------------------------------------------------------------------

/// True when `text` is a wrapper chain rather than plain SQL.
///
/// The class name alone is not enough: `import …QueryWrapper;` and a Javadoc
/// mention both contain it. A chain needs a construction (`new …Wrapper`,
/// `Wrappers.`) or at least one `.method(…)` call.
pub fn looks_like_wrapper(text: &str) -> bool {
    let has_token = WRAPPER_TOKENS.iter().any(|token| text.contains(token))
        || text.contains(&format!("{WRAPPERS_FACTORY}."));
    if !has_token {
        return false;
    }
    text.contains("Wrappers.")
        || WRAPPER_TOKENS
            .iter()
            .any(|token| text.contains(&format!("new {token}")))
        || has_call(text)
}

/// True when the document's root element is `<sqlMap>` (iBatis 2.x).
pub fn looks_like_ibatis(xml: &str) -> bool {
    match xmltree::parse_document(xml) {
        Ok(root) => root.name == "sqlMap",
        // An unparseable document still counts when it opens a `<sqlMap`, so the
        // iBatis parser reports its own, more specific failure.
        Err(_) => xml.contains("<sqlMap"),
    }
}

/// The entity class named by a wrapper construction, if the source says it.
///
/// `new LambdaQueryWrapper<User>()` → `User`; `Wrappers.<User>lambdaQuery()` →
/// `User`; `Wrappers.lambdaQuery(User.class)` → `User`; a diamond
/// (`new LambdaQueryWrapper<>()`) with no other mention → `None`.
pub fn wrapper_entity_class(text: &str) -> Option<String> {
    for token in WRAPPER_TOKENS
        .iter()
        .chain(std::iter::once(&WRAPPERS_FACTORY))
    {
        let mut from = 0usize;
        while let Some(found) = text[from..].find(token) {
            let after = from + found + token.len();
            from = after;
            if let Some(entity) = generic_argument(&text[after..]) {
                return Some(entity);
            }
        }
    }
    class_literal(text)
}

/// The `User` in `User.class`, when it is not one of the wrapper classes.
fn class_literal(text: &str) -> Option<String> {
    let mut from = 0usize;
    while let Some(found) = text[from..].find(".class") {
        let end = from + found;
        from = end + ".class".len();
        let ident: String = text[..end]
            .chars()
            .rev()
            .take_while(|ch| ch.is_alphanumeric() || *ch == '_' || *ch == '$')
            .collect::<Vec<char>>()
            .into_iter()
            .rev()
            .collect();
        if ident.is_empty()
            || !ident
                .chars()
                .next()
                .is_some_and(|first| first.is_uppercase())
        {
            continue;
        }
        if WRAPPER_TOKENS.contains(&ident.as_str()) {
            continue;
        }
        return Some(super::entity::simple_name(&ident));
    }
    None
}

/// Reads `<Type>` (or `.<Type>`) at the start of `text`, skipping whitespace.
fn generic_argument(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() && (bytes[index].is_ascii_whitespace() || bytes[index] == b'.') {
        index += 1;
    }
    if index >= bytes.len() || bytes[index] != b'<' {
        return None;
    }
    let mut depth = 0i32;
    let mut end = index;
    while end < bytes.len() {
        match bytes[end] {
            b'<' => depth += 1,
            b'>' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        end += 1;
    }
    if end >= bytes.len() {
        return None;
    }
    let inner = &text[index + 1..end];
    // Nested generics: the outer type is what names the entity.
    let outer = inner.split('<').next().unwrap_or(inner).trim();
    if !is_type_token(outer) {
        return None;
    }
    Some(super::entity::simple_name(outer))
}

/// A bare or qualified Java type name (`User`, `com.example.User`).
fn is_type_token(text: &str) -> bool {
    !text.is_empty()
        && text.split('.').all(|segment| {
            let mut chars = segment.chars();
            match chars.next() {
                Some(first) if first.is_alphabetic() || first == '_' || first == '$' => {}
                _ => return false,
            }
            chars.all(|ch| ch.is_alphanumeric() || ch == '_' || ch == '$')
        })
}

/// A `.method(…)` call anywhere in the text (string literals skipped).
fn has_call(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => index = skip_quoted(bytes, index, b'"'),
            b'\'' => index = skip_quoted(bytes, index, b'\''),
            b'.' => {
                let mut cursor = index + 1;
                while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                    cursor += 1;
                }
                let start = cursor;
                while cursor < bytes.len()
                    && (bytes[cursor].is_ascii_alphanumeric()
                        || bytes[cursor] == b'_'
                        || bytes[cursor] == b'$')
                {
                    cursor += 1;
                }
                if cursor > start {
                    let mut probe = cursor;
                    while probe < bytes.len() && bytes[probe].is_ascii_whitespace() {
                        probe += 1;
                    }
                    if probe < bytes.len() && bytes[probe] == b'(' {
                        return true;
                    }
                }
                index = cursor.max(index + 1);
            }
            _ => index += 1,
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Wrapper
// ---------------------------------------------------------------------------

/// Renders a wrapper chain, mapping the wrapper module's result onto the shared
/// [`RenderOutput`].
///
/// `derived` says the field→column projection came from MyBatis-Plus defaults
/// rather than `@TableField`: an otherwise `exact` render is then `partial`,
/// with [`KEY_COLUMN_DERIVED`] explaining why.
pub fn wrapper_render(
    text: &str,
    columns: &EntityColumns,
    derived: bool,
    dialect: &Dialect,
) -> RenderOutput {
    let out = wrapper::render(text, columns, dialect);
    from_wrapper(out, derived)
}

fn from_wrapper(out: WrapperOutput, derived: bool) -> RenderOutput {
    let mut message_keys = out.message_keys.clone();
    let mut notes: Vec<Note> = out
        .notes
        .iter()
        .map(|detail| note(KEY_WRAPPER_NOTE, detail.clone()))
        .collect();
    let downgrade = derived && out.confidence == wrapper::Confidence::Exact;
    if downgrade {
        push_key(&mut message_keys, KEY_COLUMN_DERIVED);
        notes.push(note(
            KEY_COLUMN_DERIVED,
            "the table or some columns come from MyBatis-Plus defaults (no @TableName / @TableField)",
        ));
    }
    let confidence = match out.confidence {
        wrapper::Confidence::Unresolved => Confidence::Unresolved,
        wrapper::Confidence::Partial => Confidence::Partial,
        wrapper::Confidence::Exact if downgrade => Confidence::Partial,
        wrapper::Confidence::Exact => Confidence::Exact,
    };
    // An update wrapper with no condition has nothing to emit; that is an
    // unresolved render, not an empty `partial` one.
    let sql = out.sql.unwrap_or_default();
    if confidence != Confidence::Unresolved && sql.trim().is_empty() {
        push_key(&mut message_keys, KEY_EMPTY_FRAGMENT);
        notes.push(note(KEY_EMPTY_FRAGMENT, "no condition to render"));
        return RenderOutput {
            sql: String::new(),
            params: Vec::new(),
            confidence: Confidence::Unresolved,
            notes,
            message_keys,
        };
    }
    RenderOutput {
        sql,
        params: Vec::new(),
        confidence,
        notes,
        message_keys,
    }
}

/// The unresolved render for a wrapper with no usable field/column projection.
///
/// `reason` says why there is none ("is not in the project index", "declares no
/// column this reader understands", "pass `entity`"), so the reader is not left
/// with a key and nothing else.
pub fn entity_unknown(source: &str, class: Option<&str>, reason: &str) -> RenderOutput {
    let detail = match class {
        Some(class) => format!("{source}: the entity class `{class}` {reason}"),
        None => format!(
            "{source}: the wrapper names no entity class; write `new LambdaQueryWrapper<Entity>(…)`"
        ),
    };
    RenderOutput::unresolved(KEY_ENTITY_UNKNOWN, detail)
}

// ---------------------------------------------------------------------------
// iBatis
// ---------------------------------------------------------------------------

/// Every statement id of a `<sqlMap>` document, or the parse failure.
pub fn ibatis_statement_ids(xml: &str) -> Result<Vec<String>, RenderOutput> {
    match ibatis::parse(xml) {
        Ok(model) => Ok(model.statements.keys().cloned().collect()),
        Err(error) => Err(RenderOutput::unresolved(error.message_key, error.detail)),
    }
}

/// The stored id for a requested one.
///
/// `core::ibatis` stores statements under their bare `id` attribute (the
/// namespace is only applied to `<sql>` refids), so a user typing
/// `User.selectById` is matched through its last segment. A suffix match is
/// accepted only when exactly one id fits; ambiguity is `None`, never a pick.
pub fn ibatis_resolve_id(ids: &[String], requested: &str) -> Option<String> {
    let requested = requested.trim();
    if requested.is_empty() {
        return None;
    }
    if let Some(exact) = ids.iter().find(|id| id.as_str() == requested) {
        return Some(exact.clone());
    }
    let suffix = format!(".{requested}");
    let matches: Vec<&String> = ids.iter().filter(|id| id.ends_with(&suffix)).collect();
    if let [only] = matches.as_slice() {
        return Some((*only).clone());
    }
    let segment = requested.rsplit('.').next().unwrap_or(requested);
    if segment != requested
        && let Some(exact) = ids.iter().find(|id| id.as_str() == segment)
    {
        return Some(exact.clone());
    }
    None
}

// ---------------------------------------------------------------------------
// Annotations
// ---------------------------------------------------------------------------

/// `Class#method` / `namespace.method` → the bare method name.
pub fn method_name(raw: &str) -> String {
    let after_hash = raw.rsplit('#').next().unwrap_or(raw);
    after_hash
        .rsplit('.')
        .next()
        .unwrap_or(after_hash)
        .trim()
        .to_string()
}

/// The method names a file offers, for an error the reader can act on.
pub fn annotation_methods(statements: &[AnnotatedStatement]) -> Vec<String> {
    statements
        .iter()
        .map(|statement| match &statement.method_name {
            Some(name) => name.clone(),
            None => format!("line {}", statement.line),
        })
        .collect()
}

/// Picks the one annotated statement to render.
///
/// Order: the named method, else the statement whose method span holds the
/// caret, else the file's only statement. Anything else is `unresolved` with
/// the list to choose from — never "the first one".
pub fn annotation_pick<'a>(
    java: &str,
    statements: &'a [AnnotatedStatement],
    method: Option<&str>,
    caret: Option<usize>,
    source: &str,
) -> Result<&'a AnnotatedStatement, RenderOutput> {
    if statements.is_empty() {
        return Err(RenderOutput::unresolved(
            KEY_ANNOTATION_NOT_FOUND,
            format!("{source}: no @Select / @Insert / @Update / @Delete with a string value"),
        ));
    }
    let available = annotation_methods(statements).join(", ");
    if let Some(raw) = method.map(str::trim).filter(|name| !name.is_empty()) {
        let wanted = method_name(raw);
        let matches: Vec<&AnnotatedStatement> = statements
            .iter()
            .filter(|statement| statement.method_name.as_deref() == Some(wanted.as_str()))
            .collect();
        return match matches.as_slice() {
            [] => Err(RenderOutput::unresolved(
                KEY_ANNOTATION_METHOD_NOT_FOUND,
                format!("{source}: no annotated method `{wanted}`; available: {available}"),
            )),
            [only] => Ok(only),
            several => Err(RenderOutput::unresolved(
                KEY_ANNOTATION_METHOD_AMBIGUOUS,
                format!(
                    "{source}: `{wanted}` has {} annotated overloads (lines {})",
                    several.len(),
                    several
                        .iter()
                        .map(|statement| statement.line.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )),
        };
    }
    if let Some(caret) = caret
        && let Some(found) = statements.iter().find(|statement| {
            let start = statement.annotation_start;
            let end = method_end(java, statement);
            caret >= start && caret < end
        })
    {
        return Ok(found);
    }
    match statements {
        [only] => Ok(only),
        _ => Err(RenderOutput::unresolved(
            KEY_ANNOTATION_METHOD_REQUIRED,
            format!(
                "{source}: {} annotated statements and the caret is not inside one; available: {available}",
                statements.len()
            ),
        )),
    }
}

/// Parses a Java source text into its annotated statements.
pub fn annotation_statements(java: &str) -> Vec<AnnotatedStatement> {
    crate::core::annotation::parse_annotations(java)
}

/// End of the method a statement annotates: past its `{ … }` body, or past its
/// `;` in an interface. Falls back to the annotation's end.
fn method_end(text: &str, statement: &AnnotatedStatement) -> usize {
    let bytes = text.as_bytes();
    let from = statement.method_start.unwrap_or(statement.annotation_end);
    let Some(open) = text
        .get(from..)
        .and_then(|rest| rest.find('('))
        .map(|found| from + found)
    else {
        return statement.annotation_end;
    };
    let Some(after_params) = skip_group(bytes, open, b'(', b')') else {
        return statement.annotation_end;
    };
    // `throws A, B {` sits between the parameter list and the body.
    let mut index = after_params;
    while index < bytes.len() {
        match bytes[index] {
            b'{' => return skip_group(bytes, index, b'{', b'}').unwrap_or(bytes.len()),
            b';' => return index + 1,
            b'"' => {
                index = skip_quoted(bytes, index, b'"');
                continue;
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = skip_block_comment(bytes, index);
                continue;
            }
            _ => index += 1,
        }
    }
    statement.annotation_end
}

/// One `[key]` note.
fn note(key: &str, detail: impl Into<String>) -> Note {
    Note {
        message_key: key.to_string(),
        detail: detail.into(),
    }
}

fn push_key(keys: &mut Vec<String>, key: &str) {
    if !keys.iter().any(|held| held == key) {
        keys.push(key.to_string());
    }
}

fn skip_group(bytes: &[u8], start: usize, open: u8, close: u8) -> Option<usize> {
    let mut depth = 0i32;
    let mut index = start;
    while index < bytes.len() {
        match bytes[index] {
            byte if byte == open => depth += 1,
            byte if byte == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(index + 1);
                }
            }
            b'"' => {
                index = skip_quoted(bytes, index, b'"');
                continue;
            }
            b'\'' => {
                index = skip_quoted(bytes, index, b'\'');
                continue;
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
                continue;
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = skip_block_comment(bytes, index);
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn skip_block_comment(bytes: &[u8], start: usize) -> usize {
    let mut index = start + 2;
    while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/') {
        index += 1;
    }
    (index + 2).min(bytes.len())
}

fn skip_quoted(bytes: &[u8], start: usize, quote: u8) -> usize {
    let mut index = start + 1;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            index += 2;
            continue;
        }
        if bytes[index] == quote {
            return index + 1;
        }
        index += 1;
    }
    bytes.len()
}

/// Renders an annotation statement with the given parameter values.
pub fn render_annotation(statement: &AnnotatedStatement, params: &Value) -> RenderOutput {
    crate::core::annotation::render_annotated(statement, params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::dialect;
    use serde_json::json;

    fn mysql() -> &'static Dialect {
        dialect::by_name("MySQL").expect("MySQL dialect")
    }

    fn user_columns() -> EntityColumns {
        EntityColumns::new("User", "sys_user")
            .with_column("id", "id", "bigint")
            .with_column("name", "name", "varchar")
    }

    const JAVA: &str = r#"
public interface UserMapper {
    @Select("SELECT id, name FROM users WHERE id = #{id}")
    User findById(@Param("id") Long id);

    @Insert("INSERT INTO users (id, name) VALUES (#{id}, #{name})")
    int insert(User user);

    String notAStatement(Long id);
}
"#;

    // -----------------------------------------------------------------------
    // Detection
    // -----------------------------------------------------------------------

    #[test]
    fn wrapper_detection_needs_a_chain_not_only_a_class_name() {
        assert!(looks_like_wrapper(
            r#"new LambdaQueryWrapper<User>().eq(User::getId, 1)"#
        ));
        assert!(looks_like_wrapper("new QueryWrapper<>()"));
        assert!(looks_like_wrapper(
            "Wrapper<User> w = new QueryWrapper<>();"
        ));
        assert!(looks_like_wrapper(
            "Wrappers.<User>lambdaQuery().eq(User::getName, \"a\")"
        ));
        assert!(!looks_like_wrapper(
            "import com.baomidou.mybatisplus.core.conditions.query.QueryWrapper;"
        ));
        assert!(!looks_like_wrapper("SELECT * FROM users"));
        assert!(!looks_like_wrapper(
            "/** uses LambdaQueryWrapper for queries */"
        ));
    }

    #[test]
    fn wrapper_entity_comes_from_the_generic_argument_or_the_class_literal() {
        assert_eq!(
            wrapper_entity_class("new LambdaQueryWrapper<User>()").as_deref(),
            Some("User")
        );
        assert_eq!(
            wrapper_entity_class("new LambdaQueryWrapper<com.example.User>()").as_deref(),
            Some("User")
        );
        assert_eq!(
            wrapper_entity_class("Wrappers.<User>lambdaQuery()").as_deref(),
            Some("User")
        );
        assert_eq!(
            wrapper_entity_class("Wrappers.lambdaQuery(User.class)").as_deref(),
            Some("User")
        );
        assert_eq!(
            wrapper_entity_class("LambdaQueryWrapper<User> q = new LambdaQueryWrapper<>();")
                .as_deref(),
            Some("User")
        );
        assert_eq!(wrapper_entity_class("new LambdaQueryWrapper<>()"), None);
        assert_eq!(
            wrapper_entity_class("new QueryWrapper<User>(){}.eq(\"a\",1)").as_deref(),
            Some("User")
        );
    }

    #[test]
    fn ibatis_documents_are_recognized_by_their_root_element() {
        assert!(looks_like_ibatis(
            r#"<sqlMap namespace="User"><select id="a">SELECT 1</select></sqlMap>"#
        ));
        assert!(looks_like_ibatis("<sqlMap namespace=\"a\">"));
        assert!(!looks_like_ibatis(
            r#"<mapper namespace="a"><select id="b">SELECT 1</select></mapper>"#
        ));
        assert!(!looks_like_ibatis("SELECT 1"));
    }

    // -----------------------------------------------------------------------
    // Annotations
    // -----------------------------------------------------------------------

    #[test]
    fn pick_by_method_name_accepts_the_qualified_form() {
        let statements = annotation_statements(JAVA);
        let picked =
            annotation_pick(JAVA, &statements, Some("UserMapper#insert"), None, "<t>").unwrap();
        assert_eq!(picked.method_name.as_deref(), Some("insert"));
    }

    #[test]
    fn pick_by_caret_uses_the_method_span() {
        let statements = annotation_statements(JAVA);
        let insert = statements
            .iter()
            .position(|statement| statement.method_name.as_deref() == Some("insert"))
            .unwrap();
        let caret = statements[insert].annotation_start + 3;
        let picked = annotation_pick(JAVA, &statements, None, Some(caret), "<t>").unwrap();
        assert_eq!(picked.method_name.as_deref(), Some("insert"));
    }

    #[test]
    fn a_caret_in_a_plain_method_is_not_attributed_to_the_previous_statement() {
        let statements = annotation_statements(JAVA);
        let caret = JAVA.find("String notAStatement").unwrap() + 2;
        let error = annotation_pick(JAVA, &statements, None, Some(caret), "<t>").unwrap_err();
        assert_eq!(error.confidence, Confidence::Unresolved);
        assert!(
            error.notes[0]
                .detail
                .contains("available: findById, insert"),
            "{:?}",
            error.notes
        );
    }

    #[test]
    fn a_single_statement_file_needs_no_method() {
        let java = r#"interface M { @Select("SELECT 1") int one(); }"#;
        let statements = annotation_statements(java);
        let picked = annotation_pick(JAVA, &statements, None, None, "<t>").unwrap();
        assert_eq!(picked.sql, "SELECT 1");
    }

    #[test]
    fn several_statements_without_a_method_are_unresolved_not_first_wins() {
        let statements = annotation_statements(JAVA);
        let error = annotation_pick(JAVA, &statements, None, None, "<t>").unwrap_err();
        assert_eq!(error.confidence, Confidence::Unresolved);
        assert_eq!(error.message_keys, [KEY_ANNOTATION_METHOD_REQUIRED]);
        assert_eq!(error.sql, "");
    }

    #[test]
    fn an_unknown_method_lists_what_is_available() {
        let statements = annotation_statements(JAVA);
        let error = annotation_pick(JAVA, &statements, Some("missing"), None, "<t>").unwrap_err();
        assert_eq!(error.message_keys, [KEY_ANNOTATION_METHOD_NOT_FOUND]);
        assert!(error.notes[0].detail.contains("findById, insert"));
    }

    #[test]
    fn overloads_are_ambiguous_not_arbitrary() {
        let java = r#"interface M {
            @Select("SELECT 1") int find(Long id);
            @Select("SELECT 2") int find(String name);
        }"#;
        let statements = annotation_statements(java);
        let error = annotation_pick(java, &statements, Some("find"), None, "<t>").unwrap_err();
        assert_eq!(error.message_keys, [KEY_ANNOTATION_METHOD_AMBIGUOUS]);
    }

    #[test]
    fn a_text_without_annotations_says_so() {
        let error = annotation_pick("", &[], Some("find"), None, "<t>").unwrap_err();
        assert_eq!(error.message_keys, [KEY_ANNOTATION_NOT_FOUND]);
    }

    #[test]
    fn annotation_render_keeps_parameters_in_order() {
        let statements = annotation_statements(JAVA);
        let out = render_annotation(&statements[0], &json!({"id": 7}));
        assert_eq!(out.sql, "SELECT id, name FROM users WHERE id = ?");
        assert_eq!(out.params, vec![json!(7)]);
        assert_eq!(out.confidence, Confidence::Exact);
    }

    // -----------------------------------------------------------------------
    // Wrapper
    // -----------------------------------------------------------------------

    #[test]
    fn a_declared_projection_renders_exact() {
        let out = wrapper_render(
            r#"new LambdaQueryWrapper<User>().eq(User::getName, "Ada")"#,
            &user_columns(),
            false,
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Exact, "{:?}", out.notes);
        assert_eq!(out.sql, "SELECT * FROM sys_user WHERE name = 'Ada'");
    }

    #[test]
    fn a_derived_projection_is_partial_with_the_reason() {
        let out = wrapper_render(
            r#"new LambdaQueryWrapper<User>().eq(User::getName, "Ada")"#,
            &user_columns(),
            true,
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Partial);
        assert!(out.message_keys.contains(&KEY_COLUMN_DERIVED.to_string()));
        assert!(out.sql.contains("FROM sys_user"));
    }

    #[test]
    fn an_unknown_field_is_unresolved_and_carries_no_sql() {
        let out = wrapper_render(
            "new LambdaQueryWrapper<User>().eq(User::getMissing, 1)",
            &user_columns(),
            false,
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Unresolved);
        assert_eq!(out.sql, "");
    }

    #[test]
    fn an_update_wrapper_with_no_condition_is_unresolved() {
        let out = wrapper_render(
            "new LambdaUpdateWrapper<User>()",
            &user_columns(),
            false,
            mysql(),
        );
        assert_eq!(out.confidence, Confidence::Unresolved);
        assert_eq!(out.sql, "");
        assert!(out.message_keys.contains(&KEY_EMPTY_FRAGMENT.to_string()));
    }

    #[test]
    fn entity_unknown_names_the_class_and_the_reason() {
        let out = entity_unknown("<p>", Some("User"), "is not in the project index");
        assert_eq!(out.confidence, Confidence::Unresolved);
        assert!(out.notes[0].detail.contains("`User`"));
        assert!(out.notes[0].detail.contains("not in the project index"));
        assert!(
            entity_unknown("<p>", None, "unused").notes[0]
                .detail
                .contains("<Entity>")
        );
    }

    // -----------------------------------------------------------------------
    // iBatis
    // -----------------------------------------------------------------------

    const SQLMAP: &str = r#"<sqlMap namespace="User">
    <select id="selectAll">SELECT * FROM users</select>
    <select id="selectById">SELECT * FROM users WHERE id = #id#</select>
</sqlMap>"#;

    #[test]
    fn ibatis_ids_are_the_bare_id_attributes() {
        let ids = ibatis_statement_ids(SQLMAP).unwrap();
        assert_eq!(ids, ["selectAll", "selectById"]);
    }

    #[test]
    fn an_ibatis_id_resolves_from_the_qualified_form_or_its_suffix() {
        let ids = ibatis_statement_ids(SQLMAP).unwrap();
        assert_eq!(
            ibatis_resolve_id(&ids, "selectById").as_deref(),
            Some("selectById")
        );
        assert_eq!(
            ibatis_resolve_id(&ids, "User.selectAll").as_deref(),
            Some("selectAll")
        );
        assert_eq!(ibatis_resolve_id(&ids, "nope"), None);
    }

    #[test]
    fn a_shared_ibatis_suffix_is_not_picked() {
        let ids = vec!["A.find".to_string(), "B.find".to_string()];
        assert_eq!(ibatis_resolve_id(&ids, "find"), None);
    }

    #[test]
    fn a_broken_sqlmap_reports_the_parse_failure() {
        let error = ibatis_statement_ids("<mapper namespace=\"a\"/>").unwrap_err();
        assert_eq!(error.confidence, Confidence::Unresolved);
        assert_eq!(error.message_keys, ["ibatis.root_invalid"]);
    }
}
