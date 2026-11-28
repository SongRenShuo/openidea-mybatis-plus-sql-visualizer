//! Entity class source → the field/column projection the wrapper renderer needs.
//!
//! `analyze::wrapper` renders a `LambdaQueryWrapper` chain only when it is told
//! which Java field is which column ([`EntityColumns`]). That mapping lives in
//! the entity class: `@TableName` names the table, `@TableField` renames a
//! column. This module reads it from **source text** — the plugin has no PSI —
//! so both the preview command (which finds the class in the project) and the
//! MCP tool (which is handed the text) can build the same projection.
//!
//! # What is declared and what is derived
//!
//! A column is *declared* when `@TableField("col")` / `@TableId("col")` says
//! so; a table is declared when `@TableName` says so. Everything else follows
//! MyBatis-Plus's own defaults (camelCase → snake_case for both), which is what
//! the library really does — but it also depends on the project's
//! `mapUnderscoreToCamelCase` setting, which this text-level reader cannot see.
//! The model therefore records [`EntityColumn::declared`] and
//! [`EntityModel::fully_declared`], and the wrapper render treats a derived
//! projection as `partial` instead of `exact` (see `analyze::sources`).
//!
//! Everything that is not a column is skipped and reported:
//! `static` / `transient` fields, collections and arrays, `@TableField(exist =
//! false)`, and method declarations. Skipping is a decision, not a guess: a
//! skipped field never reaches the SQL.

use super::wrapper::EntityColumns;

/// One Java field of an entity class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityColumn {
    /// The Java field name (`userName`, not the getter).
    pub field: String,
    /// The column name those rows live in.
    pub column: String,
    /// Best-effort SQL type from the Java type; empty when it is not mapped.
    pub sql_type: String,
    /// True when `@TableField` / `@TableId` named the column.
    pub declared: bool,
}

/// The entity projection read from one class body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityModel {
    /// Simple class name (`User`, never `com.example.User`).
    pub class_name: String,
    /// The table the wrapper's `SELECT` names.
    pub table: String,
    /// True when `@TableName` named the table.
    pub table_declared: bool,
    pub columns: Vec<EntityColumn>,
    /// Fields that were skipped, each with the reason (`static`, `collection`, …).
    pub skipped: Vec<String>,
    /// The column `@TableId` names, when any field declared it. The join-update
    /// renderer excludes it from `setUpdateEntity`'s column expansion, the way
    /// the reference filters `!it.isId`.
    pub primary_key: Option<String>,
}

impl EntityModel {
    /// The projection `analyze::wrapper` renders against.
    pub fn columns(&self) -> EntityColumns {
        let mut columns = EntityColumns::new(&self.class_name, &self.table);
        columns.primary_key = self.primary_key.clone();
        for column in &self.columns {
            columns = columns.with_column(&column.field, &column.column, &column.sql_type);
        }
        columns
    }

    /// True when every column and the table came from an annotation, so the
    /// render depends on no project setting.
    pub fn fully_declared(&self) -> bool {
        self.table_declared && self.columns.iter().all(|column| column.declared)
    }

    /// Field names in declaration order (for the `partial` note).
    pub fn derived_fields(&self) -> Vec<String> {
        self.columns
            .iter()
            .filter(|column| !column.declared)
            .map(|column| column.field.clone())
            .collect()
    }
}

/// Reads `class_name`'s projection from its source text.
///
/// `class_name` may be qualified; only its last segment is kept.
pub fn parse_entity(class_name: &str, java: &str) -> EntityModel {
    let simple = simple_name(class_name);
    let declared_table = annotation_value(java, "TableName");
    let table = match &declared_table {
        Some(table) => table.clone(),
        None => camel_to_snake(&simple),
    };
    let mut columns = Vec::new();
    let mut skipped = Vec::new();
    let mut primary_key = None;
    for (annotations, declaration) in field_declarations(java) {
        match read_field(&annotations, &declaration) {
            Ok(column) => {
                if primary_key.is_none() && annotation_args(&annotations, "TableId").is_some() {
                    primary_key = Some(column.column.clone());
                }
                columns.push(column);
            }
            Err(reason) => skipped.push(reason),
        }
    }
    EntityModel {
        class_name: simple,
        table,
        table_declared: declared_table.is_some(),
        columns,
        skipped,
        primary_key,
    }
}

/// `com.example.user.User` → `User`; also strips a trailing `.class`.
pub fn simple_name(class_name: &str) -> String {
    let name = class_name.trim().trim_end_matches(".class");
    name.rsplit(['.', '$']).next().unwrap_or(name).to_string()
}

/// MyBatis-Plus's `camelToUnderline`: `userName` → `user_name`,
/// `URLPath` → `url_path`, `userID` → `user_id`.
pub fn camel_to_snake(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(chars.len() + 4);
    for (index, ch) in chars.iter().enumerate() {
        if ch.is_uppercase() {
            let previous = index.checked_sub(1).map(|i| chars[i]);
            let next = chars.get(index + 1);
            let boundary = previous.is_some_and(|p| p.is_lowercase() || p.is_numeric())
                || (previous.is_some_and(|p| p.is_uppercase())
                    && next.is_some_and(|n| n.is_lowercase()));
            if boundary && !out.is_empty() && !out.ends_with('_') {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(*ch);
        }
    }
    out
}

/// Best-effort SQL type for a Java field type; empty when it is not a scalar.
fn sql_type_for(java_type: &str) -> String {
    let bare = java_type.trim();
    let name = simple_name(bare);
    match name.as_str() {
        "String" | "CharSequence" | "Character" | "char" => "varchar",
        "Integer" | "int" => "int",
        "Long" | "long" | "BigInteger" => "bigint",
        "Short" | "short" => "smallint",
        "Byte" | "byte" => "tinyint",
        "Boolean" | "boolean" => "tinyint",
        "Double" | "double" | "Float" | "float" | "BigDecimal" => "decimal",
        "LocalDate" | "Date" => "date",
        "LocalDateTime" => "datetime",
        "LocalTime" | "Time" => "time",
        "Timestamp" | "Instant" | "ZonedDateTime" | "OffsetDateTime" => "timestamp",
        _ if bare.ends_with("[]") && bare.starts_with("byte") => "varbinary",
        _ => "",
    }
    .to_string()
}

/// One `;`-terminated statement of the class body: its annotations and the
/// remaining declaration text. Statements that are not fields (methods, …) are
/// returned too; [`read_field`] rejects them.
fn field_declarations(java: &str) -> Vec<(Vec<String>, String)> {
    let bytes = java.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => index = skip_quoted(bytes, index, b'"'),
            b'\'' => index = skip_quoted(bytes, index, b'\''),
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
            }
            b'{' => {
                depth += 1;
                if depth == 1 {
                    // The class body opens here; nothing before it is a field.
                    start = index + 1;
                }
                index += 1;
            }
            b'}' => {
                depth = (depth - 1).max(0);
                if depth == 0 {
                    start = index + 1;
                }
                index += 1;
            }
            b';' if depth == 1 => {
                let statement = &java[start..index];
                let trimmed = statement.trim();
                if !trimmed.is_empty() {
                    let (annotations, rest) = split_annotations(trimmed);
                    out.push((annotations, rest));
                }
                start = index + 1;
                index += 1;
            }
            _ => index += 1,
        }
    }
    out
}

/// Separates leading `@Name` / `@Name(...)` spans from a declaration.
fn split_annotations(text: &str) -> (Vec<String>, String) {
    let bytes = text.as_bytes();
    let mut annotations = Vec::new();
    let mut rest = String::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'"' || bytes[index] == b'\'' {
            // A literal is copied through: an `@` inside it is not an annotation.
            let quote = bytes[index];
            let end = skip_quoted(bytes, index, quote);
            rest.push_str(&text[index..end]);
            index = end;
            continue;
        }
        if bytes[index] == b'@' {
            let name_start = index + 1;
            let mut cursor = name_start;
            while cursor < bytes.len()
                && (bytes[cursor].is_ascii_alphanumeric() || bytes[cursor] == b'_')
            {
                cursor += 1;
            }
            let name = &text[name_start..cursor];
            if name.is_empty() {
                rest.push('@');
                index += 1;
                continue;
            }
            let mut probe = cursor;
            while probe < bytes.len() && bytes[probe].is_ascii_whitespace() {
                probe += 1;
            }
            let mut end = cursor;
            if probe < bytes.len() && bytes[probe] == b'(' {
                match skip_parens(bytes, probe) {
                    Some(after) => end = after,
                    None => {
                        rest.push_str(&text[index..]);
                        break;
                    }
                }
            }
            annotations.push(format!("@{}", &text[name_start..end]));
            index = end;
            continue;
        }
        let ch = text[index..].chars().next().unwrap_or('\0');
        rest.push(ch);
        index += ch.len_utf8();
    }
    (annotations, rest)
}

fn skip_parens(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut index = start;
    while index < bytes.len() {
        match bytes[index] {
            b'(' => depth += 1,
            b')' => {
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
            _ => {}
        }
        index += 1;
    }
    None
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

/// Turns one class-body statement into a column, or the reason it is not one.
fn read_field(annotations: &[String], declaration: &str) -> Result<EntityColumn, String> {
    // Drop the initializer; a field's name precedes `=`. A `(` before it means
    // this is a method declaration (an interface-style entity), not a field.
    let head = declaration.split('=').next().unwrap_or(declaration).trim();
    if head.contains('(') {
        return Err(format!("method: {}", declaration.trim()));
    }
    let tokens: Vec<&str> = head.split_whitespace().collect();
    if tokens.len() < 2 {
        return Err(format!("not a field: {}", declaration.trim()));
    }
    if tokens.iter().any(|token| {
        matches!(
            *token,
            "static" | "transient" | "class" | "interface" | "enum"
        )
    }) {
        return Err(format!("static or nested type: {}", declaration.trim()));
    }
    let field = tokens[tokens.len() - 1].trim_matches(|c: char| c == ';' || c == ',');
    if !is_identifier(field) {
        return Err(format!("no field name: {}", declaration.trim()));
    }
    let java_type = tokens[tokens.len() - 2];
    if java_type.contains('<') || java_type.ends_with("[]") && !java_type.starts_with("byte") {
        return Err(format!("collection field: {field}"));
    }

    // `@TableField(exist = false)` is not a column.
    if annotation_args(annotations, "TableField").is_some_and(|args| {
        args.contains("exist") && (args.contains("exist = false") || args.contains("exist=false"))
    }) {
        return Err(format!("@TableField(exist = false): {field}"));
    }
    let declared = annotation_args(annotations, "TableField")
        .and_then(|args| first_string(&args))
        .or_else(|| annotation_args(annotations, "TableId").and_then(|args| first_string(&args)));
    let column = match &declared {
        Some(column) => column.clone(),
        None => camel_to_snake(field),
    };
    Ok(EntityColumn {
        field: field.to_string(),
        column,
        sql_type: sql_type_for(java_type),
        declared: declared.is_some(),
    })
}

fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) if first.is_alphabetic() || first == '_' || first == '$' => {}
        _ => return false,
    }
    chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

/// The argument list of `@<name>(…)` in a declaration's annotations.
fn annotation_args(annotations: &[String], name: &str) -> Option<String> {
    annotations.iter().find_map(|annotation| {
        let rest = annotation.strip_prefix('@')?;
        let rest = rest.strip_prefix(name)?;
        let inner = rest.trim().strip_prefix('(')?.strip_suffix(')')?;
        Some(inner.to_string())
    })
}

/// Every argument list of `@<name>(…)` occurrences, in source order.
fn annotation_arg_lists(java: &str, name: &str) -> Vec<String> {
    let needle = format!("@{name}");
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(found) = java[from..].find(&needle) {
        let start = from + found + needle.len();
        from = start;
        let bytes = java.as_bytes();
        let mut probe = start;
        while probe < bytes.len() && bytes[probe].is_ascii_whitespace() {
            probe += 1;
        }
        if probe >= bytes.len() || bytes[probe] != b'(' {
            continue;
        }
        let Some(end) = skip_parens(bytes, probe) else {
            continue;
        };
        out.push(java[probe + 1..end - 1].to_string());
        from = end;
    }
    out
}

/// The table of `@TableName("t")` / `@TableName(value = "t")`.
///
/// `@TableName(schema = "app")` names no table: the positional or `value`
/// literal is what the annotation means, and anything else leaves the table
/// derived (a decision, reported through [`EntityModel::table_declared`]).
fn annotation_value(java: &str, name: &str) -> Option<String> {
    for args in annotation_arg_lists(java, name) {
        let trimmed = args.trim();
        if trimmed.starts_with('"') {
            return first_string(trimmed);
        }
        if let Some(equals) = value_equals(&args) {
            return first_string(&args[equals..]);
        }
    }
    None
}

/// The offset of the `=` that assigns the `value` attribute, if it is named.
fn value_equals(args: &str) -> Option<usize> {
    let bytes = args.as_bytes();
    let mut index = 0usize;
    while index + 5 <= bytes.len() {
        if &args[index..index + 5] == "value" {
            let boundary_before = index == 0
                || bytes[index - 1].is_ascii_whitespace()
                || bytes[index - 1] == b','
                || bytes[index - 1] == b'(';
            if boundary_before {
                let mut probe = index + 5;
                while probe < bytes.len() && bytes[probe].is_ascii_whitespace() {
                    probe += 1;
                }
                if probe < bytes.len() && bytes[probe] == b'=' {
                    return Some(probe + 1);
                }
            }
        }
        index += 1;
    }
    None
}

/// The first `"…"` literal in `text`, with Java escapes decoded.
fn first_string(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let start = bytes.iter().position(|b| *b == b'"')?;
    let end = skip_quoted(bytes, start, b'"');
    let raw = text.get(start + 1..end.checked_sub(1)?)?;
    Some(unescape(raw))
}

fn unescape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENTITY: &str = r#"
package com.example.entity;

import com.baomidou.mybatisplus.annotation.TableField;
import com.baomidou.mybatisplus.annotation.TableId;
import com.baomidou.mybatisplus.annotation.TableName;

@TableName("sys_user")
public class User extends BaseEntity {
    @TableId("id")
    private Long id;

    @TableField("user_name")
    private String userName;

    private Integer age;

    @TableField(exist = false)
    private String token;

    private static final long serialVersionUID = 1L;

    private transient String cache;

    private java.util.List<Role> roles;

    public String getUserName() {
        return userName;
    }

    public void setUserName(String userName) {
        this.userName = userName;
    }
}
"#;

    #[test]
    fn declared_table_and_columns_are_read() {
        let model = parse_entity("com.example.entity.User", ENTITY);
        assert_eq!(model.class_name, "User");
        assert_eq!(model.table, "sys_user");
        assert!(model.table_declared);
        let fields: Vec<&str> = model.columns.iter().map(|c| c.field.as_str()).collect();
        assert_eq!(fields, ["id", "userName", "age"]);
        assert_eq!(model.columns[0].column, "id");
        assert_eq!(model.columns[1].column, "user_name");
        assert_eq!(model.columns[2].column, "age");
        assert!(model.columns[0].declared);
        assert!(model.columns[1].declared);
        assert!(!model.columns[2].declared, "age has no @TableField");
        assert!(!model.fully_declared(), "age derives its column");
        assert_eq!(model.columns[1].sql_type, "varchar");
        assert_eq!(model.columns[2].sql_type, "int");
    }

    #[test]
    fn static_transient_collection_and_exist_false_are_skipped_not_rendered() {
        let model = parse_entity("User", ENTITY);
        let skipped = model.skipped.join("\n");
        assert!(skipped.contains("serialVersionUID"), "{skipped}");
        assert!(skipped.contains("cache"), "{skipped}");
        assert!(skipped.contains("roles"), "{skipped}");
        assert!(skipped.contains("token"), "{skipped}");
        assert!(model.columns.iter().all(|c| c.field != "token"));
        assert!(model.columns.iter().all(|c| c.field != "roles"));
        // Methods with a body never reach the field reader at all.
        assert!(model.columns.iter().all(|c| c.field != "getUserName"));
    }

    #[test]
    fn a_class_without_annotations_uses_mybatis_plus_defaults() {
        let java = "public class OrderItem {\n  private Long id;\n  private String productName;\n}";
        let model = parse_entity("OrderItem", java);
        assert_eq!(model.table, "order_item");
        assert!(!model.table_declared);
        assert_eq!(model.columns[1].column, "product_name");
        assert!(!model.fully_declared());
        assert_eq!(model.derived_fields(), ["id", "productName"]);
    }

    #[test]
    fn fully_declared_is_true_only_when_every_column_is_annotated() {
        let java = r#"
@TableName("t_user")
class U {
    @TableField("uid") private Long id;
}"#;
        let model = parse_entity("U", java);
        assert!(model.fully_declared(), "{model:?}");
    }

    #[test]
    fn table_name_value_form_and_schema_form() {
        let model = parse_entity("U", r#"@TableName(value = "t_user") class U { }"#);
        assert_eq!(model.table, "t_user");
        assert!(model.table_declared);

        let no_literal = parse_entity("UserProfile", r#"@TableName(schema = "app") class U { }"#);
        assert_eq!(no_literal.table, "user_profile");
        assert!(!no_literal.table_declared);
    }

    #[test]
    fn camel_to_snake_matches_the_library_rule() {
        assert_eq!(camel_to_snake("userName"), "user_name");
        assert_eq!(camel_to_snake("createdAt"), "created_at");
        assert_eq!(camel_to_snake("userID"), "user_id");
        assert_eq!(camel_to_snake("id"), "id");
        assert_eq!(camel_to_snake("URLPath"), "url_path");
        assert_eq!(camel_to_snake("a1B"), "a1_b");
    }

    #[test]
    fn simple_name_strips_package_and_class_suffix() {
        assert_eq!(simple_name("com.example.User"), "User");
        assert_eq!(simple_name("com.example.User.class"), "User");
        assert_eq!(simple_name("Outer$Inner"), "Inner");
    }

    #[test]
    fn the_projection_carries_every_kept_field() {
        let model = parse_entity("User", ENTITY);
        let columns = model.columns();
        assert_eq!(columns.table, "sys_user");
        assert_eq!(columns.column_for("userName"), Some("user_name"));
        assert_eq!(columns.column_for("token"), None);
    }

    #[test]
    fn a_bodyless_class_yields_no_columns_and_never_panics() {
        let model = parse_entity("Empty", "public class Empty { }");
        assert!(model.columns.is_empty());
        assert_eq!(model.table, "empty");
        assert!(parse_entity("Empty", "").columns.is_empty());
    }
}
