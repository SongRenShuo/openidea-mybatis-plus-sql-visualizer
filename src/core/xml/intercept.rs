//! MyBatis-Plus interceptor semantics for the preview (C14).
//!
//! The reference edition resolves the entity (`TableInfo`) and then injects
//! clauses into its structured statement model: `TenantLineEnhancer`,
//! `PaginationSimulator`, `injectLogicDeleteCondition`. This build has no
//! entity and no SQL AST — a mapper statement is text with `?` placeholders —
//! so the same semantics are applied to the rendered text, and only when the
//! settings page turns them on:
//!
//! | interceptor | shape | statements |
//! | --- | --- | --- |
//! | logic delete | `AND deleted = 0` (a literal, like the reference) | SELECT / DELETE |
//! | tenant line | `AND tenant_id = ?` (a placeholder + its value) | SELECT / UPDATE / DELETE |
//! | pagination | `LIMIT ?, ?` / `LIMIT ? OFFSET ?` / `OFFSET ? ROWS FETCH NEXT ? ROWS ONLY` | SELECT |
//! | table prefix | `FROM sys_user` | every statement |
//!
//! Three rules keep this honest:
//!
//! - a column the statement already mentions is not injected a second time
//!   (the reference's `conditionReferencesColumn`), and the page is told so;
//! - a statement that already has `LIMIT` / `OFFSET` / `FETCH` is left alone;
//! - rewriting table names is the one heuristic here — the words after
//!   `FROM` / `JOIN` / `INTO` / `UPDATE` are taken to be tables. A rewrite sets
//!   `partial`, per the product rule that a guessed name is never `exact`.
//!
//! Insert statements get no tenant column: the reference adds it to the INSERT
//! column list through the entity's fields (`BatchOperationConverter`), and
//! aligning a column list with its value list from text alone would be guesswork.

use crate::core::dialect::{Dialect, PaginationStyle};

pub const KEY_LOGIC_DELETE: &str = "preview.intercept.logicDelete";
pub const KEY_TENANT: &str = "preview.intercept.tenant";
pub const KEY_PAGINATION: &str = "preview.intercept.pagination";
pub const KEY_TABLE_PREFIX: &str = "preview.intercept.tablePrefix";
pub const KEY_SKIP_EXISTING: &str = "preview.intercept.skipExisting";
pub const KEY_TABLE_NOT_PREFIXED: &str = "preview.intercept.tableNotPrefixed";
pub const KEY_PAGINATION_ORDER_BY: &str = "preview.intercept.paginationOrderBy";

/// A column and the value it is compared with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnValue {
    pub column: String,
    pub value: String,
}

/// The page the pagination interceptor simulates (`Page(current, size)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    pub current: i64,
    pub size: i64,
}

impl Page {
    fn offset(self) -> i64 {
        (self.current.max(1) - 1).saturating_mul(self.size.max(0))
    }
}

/// What the settings page asks the render to simulate.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InterceptConfig {
    pub logic_delete: Option<ColumnValue>,
    pub tenant: Option<ColumnValue>,
    pub pagination: Option<Page>,
    /// Non-empty means "prepend this to every table name".
    pub table_prefix: Option<String>,
}

impl InterceptConfig {
    /// True when nothing is switched on, so the render can skip the work.
    pub fn is_empty(&self) -> bool {
        self.logic_delete.is_none()
            && self.tenant.is_none()
            && self.pagination.is_none()
            && self
                .table_prefix
                .as_ref()
                .is_none_or(|prefix| prefix.trim().is_empty())
    }
}

/// A `?` the interceptor appended, with the name the parameter pane shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectedParam {
    pub name: String,
    pub value: String,
}

/// One thing the simulation did (or refused to do), keyed for the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterceptNote {
    pub message_key: &'static str,
    pub detail: String,
}

/// What the simulated statement looks like.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterceptOutcome {
    pub sql: String,
    /// The appended placeholders, in SQL order (they follow the statement's own
    /// `?`, so the parameter list stays aligned with the SQL).
    pub params: Vec<InjectedParam>,
    pub notes: Vec<InterceptNote>,
    /// A table name was rewritten: the render is `partial`.
    pub partial: bool,
}

impl InterceptOutcome {
    fn unchanged(sql: &str) -> Self {
        Self {
            sql: sql.to_string(),
            params: Vec::new(),
            notes: Vec::new(),
            partial: false,
        }
    }
}

/// Applies the switched-on interceptors to a rendered statement.
pub fn apply(sql: &str, dialect: Option<&Dialect>, config: &InterceptConfig) -> InterceptOutcome {
    if config.is_empty() || sql.trim().is_empty() {
        return InterceptOutcome::unchanged(sql);
    }
    let (body, terminator) = split_terminator(sql);
    let kind = statement_kind(body);
    if kind == StatementKind::Other {
        // A statement no interceptor rewrites (`WITH …` included: the shape
        // would need a real parser to place a WHERE).
        return InterceptOutcome::unchanged(sql);
    }

    let mut out = InterceptOutcome::unchanged(sql);
    let mut text = body.to_string();

    if let Some(prefix) = config
        .table_prefix
        .as_ref()
        .map(|prefix| prefix.trim())
        .filter(|prefix| !prefix.is_empty())
    {
        let (rewritten, skipped) = rewrite_tables(&text, prefix);
        if rewritten != text {
            out.partial = true;
            out.notes.push(InterceptNote {
                message_key: KEY_TABLE_PREFIX,
                detail: prefix.to_string(),
            });
            text = rewritten;
        }
        for name in skipped {
            out.notes.push(InterceptNote {
                message_key: KEY_TABLE_NOT_PREFIXED,
                detail: name,
            });
        }
    }

    if kind == StatementKind::Select || kind == StatementKind::Delete {
        if let Some(logic) = &config.logic_delete {
            if column_mentioned(&text, &logic.column) {
                out.notes.push(InterceptNote {
                    message_key: KEY_SKIP_EXISTING,
                    detail: logic.column.clone(),
                });
            } else {
                let condition = format!("{} = {}", logic.column, literal(&logic.value));
                text = append_condition(&text, &condition);
                out.notes.push(InterceptNote {
                    message_key: KEY_LOGIC_DELETE,
                    detail: condition,
                });
            }
        }
    }

    if matches!(
        kind,
        StatementKind::Select | StatementKind::Update | StatementKind::Delete
    ) {
        if let Some(tenant) = &config.tenant {
            if column_mentioned(&text, &tenant.column) {
                out.notes.push(InterceptNote {
                    message_key: KEY_SKIP_EXISTING,
                    detail: tenant.column.clone(),
                });
            } else {
                let condition = format!("{} = ?", tenant.column);
                text = append_condition(&text, &condition);
                out.params.push(InjectedParam {
                    name: tenant.column.clone(),
                    value: tenant.value.clone(),
                });
                out.notes.push(InterceptNote {
                    message_key: KEY_TENANT,
                    detail: condition,
                });
            }
        }
    }

    if kind == StatementKind::Select
        && let Some(page) = config.pagination
    {
        if has_top_level(&text, &["LIMIT", "OFFSET", "FETCH"]) {
            out.notes.push(InterceptNote {
                message_key: KEY_SKIP_EXISTING,
                detail: "LIMIT".to_string(),
            });
        } else {
            let style = dialect
                .map(|dialect| dialect.capabilities.pagination)
                .unwrap_or(PaginationStyle::Pg);
            let (clause, params) = pagination_clause(style, page);
            // SQL Server's OFFSET/FETCH needs an ORDER BY; without one the
            // simulated statement would not run there.
            let order_by_missing =
                style == PaginationStyle::SqlServerOffsetFetch && !has_top_level(&text, &["ORDER"]);
            text = append_clause(&text, &clause);
            out.params.extend(params);
            out.notes.push(InterceptNote {
                message_key: KEY_PAGINATION,
                detail: clause,
            });
            if order_by_missing {
                out.partial = true;
                out.notes.push(InterceptNote {
                    message_key: KEY_PAGINATION_ORDER_BY,
                    detail: String::new(),
                });
            }
        }
    }

    out.sql = format!("{text}{terminator}");
    out
}

/// The pagination clause with placeholders, in the dialect's shape, plus the
/// values in the order the placeholders appear.
fn pagination_clause(style: PaginationStyle, page: Page) -> (String, Vec<InjectedParam>) {
    let offset = page.offset();
    let size = page.size.max(0);
    let offset_param = || InjectedParam {
        name: "page.offset".to_string(),
        value: offset.to_string(),
    };
    let size_param = || InjectedParam {
        name: "page.size".to_string(),
        value: size.to_string(),
    };
    match style {
        PaginationStyle::MySql => ("LIMIT ?, ?".to_string(), vec![offset_param(), size_param()]),
        PaginationStyle::Pg => (
            "LIMIT ? OFFSET ?".to_string(),
            vec![size_param(), offset_param()],
        ),
        PaginationStyle::OracleFetch => (
            "OFFSET ? ROWS FETCH NEXT ? ROWS ONLY".to_string(),
            vec![offset_param(), size_param()],
        ),
        PaginationStyle::SqlServerOffsetFetch => (
            "OFFSET ? ROWS FETCH NEXT ? ROWS ONLY".to_string(),
            vec![offset_param(), size_param()],
        ),
    }
}

/// The statement's own literal: a number stays bare, anything else is quoted.
fn literal(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.eq_ignore_ascii_case("null") {
        return "NULL".to_string();
    }
    if is_number(trimmed) {
        return trimmed.to_string();
    }
    format!("'{}'", trimmed.replace('\'', "''"))
}

fn is_number(text: &str) -> bool {
    let bytes = text.as_bytes();
    let body = match bytes.first() {
        Some(b'+') | Some(b'-') => &bytes[1..],
        _ => bytes,
    };
    !body.is_empty()
        && body
            .iter()
            .all(|byte| byte.is_ascii_digit() || *byte == b'.')
        && body.iter().any(u8::is_ascii_digit)
        && body.iter().filter(|byte| **byte == b'.').count() <= 1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatementKind {
    Select,
    Insert,
    Update,
    Delete,
    Other,
}

fn statement_kind(sql: &str) -> StatementKind {
    match first_word(sql).as_str() {
        "SELECT" => StatementKind::Select,
        "INSERT" => StatementKind::Insert,
        "UPDATE" => StatementKind::Update,
        "DELETE" => StatementKind::Delete,
        _ => StatementKind::Other,
    }
}

fn first_word(sql: &str) -> String {
    scan_words(sql)
        .into_iter()
        .next()
        .map(|word| word.upper)
        .unwrap_or_default()
}

/// Appends `condition` as a WHERE conjunct: an existing WHERE gets `AND`, a
/// statement without one gets `WHERE`, and both keep any trailing clause
/// (`ORDER BY`, `GROUP BY`, `LIMIT`, …) after the condition.
fn append_condition(sql: &str, condition: &str) -> String {
    let words = scan_words(sql);
    let has_where = words
        .iter()
        .any(|word| word.depth == 0 && word.upper == "WHERE");
    let at = insertion_point(sql, &words);
    let head = sql[..at].trim_end();
    let tail = sql[at..].trim_start();
    let connector = if has_where { "AND" } else { "WHERE" };
    // A statement ending in a `--` comment must break the line first, or the
    // appended words would become part of that comment.
    let separator = if tail.is_empty() && ends_in_line_comment(head) {
        "\n"
    } else {
        " "
    };
    let mut out = format!("{head}{separator}{connector} {condition}");
    if !tail.is_empty() {
        out.push(' ');
        out.push_str(tail);
    }
    out
}

/// Appends a standalone clause (`LIMIT …`) at the very end.
///
/// The clause starts on its own line: the statement may end in a `--` comment,
/// and text appended to that line would become part of the comment.
fn append_clause(sql: &str, clause: &str) -> String {
    let head = sql.trim_end();
    match head.is_empty() {
        true => clause.to_string(),
        false => format!("{head}\n{clause}"),
    }
}

/// The byte offset a WHERE conjunct is inserted at: the start of the first
/// top-level trailing clause after it, else the end of the statement.
fn insertion_point(sql: &str, words: &[Word]) -> usize {
    const TRAILING: [&str; 10] = [
        "ORDER",
        "GROUP",
        "LIMIT",
        "OFFSET",
        "FETCH",
        "FOR",
        "WINDOW",
        "UNION",
        "HAVING",
        "RETURNING",
    ];
    words
        .iter()
        .find(|word| word.depth == 0 && TRAILING.contains(&word.upper.as_str()))
        .map(|word| word.start)
        .unwrap_or(sql.len())
}

/// True when the text's last line comment runs to its end.
fn ends_in_line_comment(sql: &str) -> bool {
    let bytes = sql.as_bytes();
    let mut index = 0usize;
    let mut inside = false;
    while index < bytes.len() {
        match bytes[index] {
            b'\'' | b'"' | b'`' => index = skip_quoted(bytes, index),
            b'-' if bytes.get(index + 1) == Some(&b'-') => {
                inside = true;
                index += 2;
            }
            b'\n' => {
                inside = false;
                index += 1;
            }
            _ => index += 1,
        }
    }
    inside
}

/// `(statement, terminator)` — the trailing semicolon is kept aside so clause
/// insertion never lands after it.
fn split_terminator(sql: &str) -> (&str, &str) {
    let trimmed = sql.trim_end();
    match trimmed.strip_suffix(';') {
        Some(body) => (body.trim_end(), ";"),
        None => (trimmed, ""),
    }
}

/// True when `column` already appears as a word anywhere in the statement
/// (`t.deleted` counts as `deleted`). Conservative: a hit means "do not
/// inject", exactly like the reference's `conditionReferencesColumn`.
fn column_mentioned(sql: &str, column: &str) -> bool {
    let leaf = column.rsplit('.').next().unwrap_or(column);
    let wanted = leaf.to_ascii_uppercase();
    scan_words(sql).iter().any(|word| word.upper == wanted)
}

fn has_top_level(sql: &str, keywords: &[&str]) -> bool {
    scan_words(sql)
        .iter()
        .any(|word| word.depth == 0 && keywords.contains(&word.upper.as_str()))
}

/// Prepends `prefix` to every table name in a `FROM` / `JOIN` / `INTO` /
/// `UPDATE` position.
///
/// Returns the rewritten text and the names it deliberately left alone (a
/// schema-qualified name, a comma-separated table list, a table function), so
/// the page can say why a statement does not look fully prefixed.
fn rewrite_tables(sql: &str, prefix: &str) -> (String, Vec<String>) {
    // Keywords that may follow FROM / JOIN / INTO / UPDATE without naming a
    // table (a subquery, a value list, another clause, a lock hint).
    const NOT_A_TABLE: [&str; 30] = [
        "SELECT", "VALUES", "SET", "WHERE", "ON", "USING", "AS", "JOIN", "INNER", "LEFT", "RIGHT",
        "FULL", "OUTER", "CROSS", "NATURAL", "GROUP", "ORDER", "LIMIT", "OFFSET", "FETCH", "UNION",
        "HAVING", "LATERAL", "DUAL", "ONLY", "WITH", "NOWAIT", "SKIP", "LOCKED", "SHARE",
    ];
    let words = scan_words(sql);
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for (index, word) in words.iter().enumerate() {
        let leads_with_table = match word.upper.as_str() {
            "FROM" | "JOIN" | "INTO" => true,
            // `UPDATE` is a table lead only as the statement's first word: the
            // same word ends `SELECT … FOR UPDATE`.
            "UPDATE" => index == 0,
            _ => false,
        };
        if !leads_with_table {
            continue;
        }
        let Some(next) = words.get(index + 1) else {
            continue;
        };
        if NOT_A_TABLE.contains(&next.upper.as_str()) {
            continue;
        }
        let name = &sql[next.start..next.end];
        let before = sql[..next.start].chars().next_back();
        let after = sql[next.end..].chars().next();
        if after == Some('(') {
            // A table function (`generate_series(…)`), not a table.
            continue;
        }
        let skipped_name = if before == Some('.') || after == Some('.') {
            // Schema-qualified: the prefix belongs on the table, but splitting
            // the qualified name is not what the setting promises.
            Some(name.to_string())
        } else if comma_follows(sql, next.end) {
            // `FROM a, b` — prefixing only `a` would produce a half-prefixed
            // statement, so the whole list is left alone.
            Some(name.to_string())
        } else {
            None
        };
        match skipped_name {
            Some(name) => skipped.push(name),
            None => edits.push((next.start, next.end, format!("{prefix}{name}"))),
        }
    }
    if edits.is_empty() {
        return (sql.to_string(), skipped);
    }
    let mut out = String::with_capacity(sql.len() + edits.len() * prefix.len());
    let mut cursor = 0;
    for (start, end, replacement) in edits {
        out.push_str(&sql[cursor..start]);
        out.push_str(&replacement);
        cursor = end;
    }
    out.push_str(&sql[cursor..]);
    (out, skipped)
}

fn comma_follows(sql: &str, from: usize) -> bool {
    sql[from..].chars().find(|c| !c.is_whitespace()) == Some(',')
}

/// A depth-annotated word of the statement, outside strings and comments.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Word {
    start: usize,
    end: usize,
    upper: String,
    depth: usize,
}

/// Every identifier-ish word of `sql`, outside `'…'` / `"…"` / `` `…` ``,
/// line comments and block comments, annotated with its parenthesis depth.
fn scan_words(sql: &str) -> Vec<Word> {
    let bytes = sql.as_bytes();
    let mut words = Vec::new();
    let mut depth = 0usize;
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'\'' | b'"' | b'`' => index = skip_quoted(bytes, index),
            b'-' if bytes.get(index + 1) == Some(&b'-') => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index < bytes.len()
                    && !(bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/'))
                {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
            }
            b'(' => {
                depth += 1;
                index += 1;
            }
            b')' => {
                depth = depth.saturating_sub(1);
                index += 1;
            }
            byte if is_word_start(byte) => {
                let start = index;
                while index < bytes.len() && is_word_char(bytes[index]) {
                    index += 1;
                }
                words.push(Word {
                    start,
                    end: index,
                    upper: sql[start..index].to_ascii_uppercase(),
                    depth,
                });
            }
            _ => index += 1,
        }
    }
    words
}

/// Skips a quoted span starting at `open`; doubled quotes and a backslash
/// escape stay inside it (MySQL accepts `\'`, the doubled quote is standard).
fn skip_quoted(bytes: &[u8], open: usize) -> usize {
    let quote = bytes[open];
    let mut index = open + 1;
    while index < bytes.len() {
        if bytes[index] == b'\\' && quote != b'`' {
            index += 2;
            continue;
        }
        if bytes[index] == quote {
            if bytes.get(index + 1) == Some(&quote) {
                index += 2;
                continue;
            }
            return index + 1;
        }
        index += 1;
    }
    bytes.len()
}

fn is_word_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_word_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$'
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::dialect;

    fn config() -> InterceptConfig {
        InterceptConfig::default()
    }

    fn mysql() -> &'static Dialect {
        dialect::by_name("MySQL").expect("MySQL")
    }

    #[test]
    fn an_empty_config_changes_nothing() {
        let sql = "SELECT * FROM user WHERE id = ?";
        let out = apply(sql, Some(mysql()), &config());
        assert_eq!(out.sql, sql);
        assert!(out.params.is_empty());
        assert!(out.notes.is_empty());
        assert!(!out.partial);
    }

    #[test]
    fn logic_delete_appends_and_or_where() {
        let mut cfg = config();
        cfg.logic_delete = Some(ColumnValue {
            column: "deleted".to_string(),
            value: "0".to_string(),
        });
        let out = apply("SELECT id FROM user WHERE name = ?", Some(mysql()), &cfg);
        assert_eq!(
            out.sql,
            "SELECT id FROM user WHERE name = ? AND deleted = 0"
        );
        assert_eq!(out.output_notes(), vec![KEY_LOGIC_DELETE]);
        let out = apply("DELETE FROM user", Some(mysql()), &cfg);
        assert_eq!(out.sql, "DELETE FROM user WHERE deleted = 0");
    }

    #[test]
    fn a_condition_lands_before_a_trailing_clause() {
        let mut cfg = config();
        cfg.logic_delete = Some(ColumnValue {
            column: "deleted".to_string(),
            value: "0".to_string(),
        });
        let out = apply(
            "SELECT id FROM user WHERE name = 'x' ORDER BY id DESC",
            Some(mysql()),
            &cfg,
        );
        assert_eq!(
            out.sql,
            "SELECT id FROM user WHERE name = 'x' AND deleted = 0 ORDER BY id DESC"
        );
    }

    #[test]
    fn an_existing_column_is_not_injected_twice() {
        let mut cfg = config();
        cfg.logic_delete = Some(ColumnValue {
            column: "deleted".to_string(),
            value: "0".to_string(),
        });
        let out = apply("SELECT id FROM user WHERE deleted = 0", Some(mysql()), &cfg);
        assert_eq!(out.sql, "SELECT id FROM user WHERE deleted = 0");
        assert_eq!(out.output_notes(), vec![KEY_SKIP_EXISTING]);
    }

    #[test]
    fn logic_delete_leaves_update_and_insert_alone() {
        let mut cfg = config();
        cfg.logic_delete = Some(ColumnValue {
            column: "deleted".to_string(),
            value: "0".to_string(),
        });
        let sql = "UPDATE user SET name = ? WHERE id = ?";
        assert_eq!(apply(sql, Some(mysql()), &cfg).sql, sql);
        let sql = "INSERT INTO user (name) VALUES (?)";
        assert_eq!(apply(sql, Some(mysql()), &cfg).sql, sql);
    }

    #[test]
    fn tenant_adds_a_placeholder_and_its_value() {
        let mut cfg = config();
        cfg.tenant = Some(ColumnValue {
            column: "tenant_id".to_string(),
            value: "7".to_string(),
        });
        let out = apply("SELECT id FROM user", Some(mysql()), &cfg);
        assert_eq!(out.sql, "SELECT id FROM user WHERE tenant_id = ?");
        assert_eq!(
            out.params,
            vec![InjectedParam {
                name: "tenant_id".to_string(),
                value: "7".to_string(),
            }]
        );
        let out = apply("UPDATE user SET name = ? WHERE id = ?", Some(mysql()), &cfg);
        assert_eq!(
            out.sql,
            "UPDATE user SET name = ? WHERE id = ? AND tenant_id = ?"
        );
        // The injected placeholder follows the statement's own, so the value
        // list stays aligned with the SQL.
        assert_eq!(out.params.len(), 1);
    }

    #[test]
    fn pagination_follows_the_dialect_shape() {
        let mut cfg = config();
        cfg.pagination = Some(Page {
            current: 3,
            size: 20,
        });
        // The clause starts on its own line (a statement may end in a comment).
        let out = apply("SELECT id FROM user", Some(mysql()), &cfg);
        assert_eq!(compact(&out.sql), "SELECT id FROM user LIMIT ?, ?");
        assert_eq!(
            out.params,
            vec![
                InjectedParam {
                    name: "page.offset".to_string(),
                    value: "40".to_string(),
                },
                InjectedParam {
                    name: "page.size".to_string(),
                    value: "20".to_string(),
                },
            ]
        );
        let pg = dialect::by_name("PostgreSQL").expect("PostgreSQL");
        let out = apply("SELECT id FROM user", Some(pg), &cfg);
        assert_eq!(compact(&out.sql), "SELECT id FROM user LIMIT ? OFFSET ?");
        assert_eq!(out.params[0].value, "20", "size first for LIMIT ? OFFSET ?");
        assert_eq!(out.params[1].value, "40");
        let oracle = dialect::by_name("Oracle").expect("Oracle");
        let out = apply("SELECT id FROM user", Some(oracle), &cfg);
        assert_eq!(
            compact(&out.sql),
            "SELECT id FROM user OFFSET ? ROWS FETCH NEXT ? ROWS ONLY"
        );
    }

    /// Whitespace-insensitive comparison for the clause-append tests.
    fn compact(sql: &str) -> String {
        sql.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn pagination_is_select_only_and_skips_an_existing_limit() {
        let mut cfg = config();
        cfg.pagination = Some(Page {
            current: 1,
            size: 10,
        });
        let sql = "UPDATE user SET a = ? WHERE id = ?";
        assert_eq!(apply(sql, Some(mysql()), &cfg).sql, sql);
        let out = apply("SELECT id FROM user LIMIT 5", Some(mysql()), &cfg);
        assert_eq!(out.sql, "SELECT id FROM user LIMIT 5");
        assert_eq!(out.output_notes(), vec![KEY_SKIP_EXISTING]);
    }

    #[test]
    fn sql_server_pagination_flags_the_missing_order_by() {
        let mut cfg = config();
        cfg.pagination = Some(Page {
            current: 1,
            size: 10,
        });
        let sql_server = dialect::by_name("SQL Server").expect("SQL Server");
        let out = apply("SELECT id FROM user", Some(sql_server), &cfg);
        assert!(out.partial, "OFFSET/FETCH without ORDER BY will not run");
        assert_eq!(
            out.output_notes(),
            vec![KEY_PAGINATION, KEY_PAGINATION_ORDER_BY]
        );
        let out = apply("SELECT id FROM user ORDER BY id", Some(sql_server), &cfg);
        assert!(!out.partial, "ORDER BY makes OFFSET/FETCH valid");
    }

    #[test]
    fn the_table_prefix_rewrites_from_join_update_and_into() {
        let mut cfg = config();
        cfg.table_prefix = Some("sys_".to_string());
        let out = apply(
            "SELECT u.id FROM user u JOIN dept d ON d.id = u.dept_id",
            Some(mysql()),
            &cfg,
        );
        assert_eq!(
            out.sql,
            "SELECT u.id FROM sys_user u JOIN sys_dept d ON d.id = u.dept_id"
        );
        assert!(out.partial, "table positions are identified heuristically");
        let out = apply("UPDATE user SET a = ? WHERE id = ?", Some(mysql()), &cfg);
        assert_eq!(out.sql, "UPDATE sys_user SET a = ? WHERE id = ?");
        let out = apply("INSERT INTO user (name) VALUES (?)", Some(mysql()), &cfg);
        assert_eq!(out.sql, "INSERT INTO sys_user (name) VALUES (?)");
        let out = apply("DELETE FROM user WHERE id = ?", Some(mysql()), &cfg);
        assert_eq!(out.sql, "DELETE FROM sys_user WHERE id = ?");
    }

    #[test]
    fn the_table_prefix_skips_subqueries_lists_and_qualified_names() {
        let mut cfg = config();
        cfg.table_prefix = Some("sys_".to_string());
        // A subquery's own FROM is found at its own depth.
        let out = apply("SELECT * FROM (SELECT id FROM user) t", Some(mysql()), &cfg);
        assert_eq!(out.sql, "SELECT * FROM (SELECT id FROM sys_user) t");
        // A comma list would end up half prefixed: left alone, with a note.
        let out = apply("SELECT * FROM user, dept", Some(mysql()), &cfg);
        assert_eq!(out.sql, "SELECT * FROM user, dept");
        assert_eq!(out.output_notes(), vec![KEY_TABLE_NOT_PREFIXED]);
        let out = apply("SELECT * FROM app.user", Some(mysql()), &cfg);
        assert_eq!(out.sql, "SELECT * FROM app.user");
        assert_eq!(out.output_notes(), vec![KEY_TABLE_NOT_PREFIXED]);
        // A table function is not a table.
        let out = apply("SELECT * FROM generate_series(1, 3)", Some(mysql()), &cfg);
        assert_eq!(out.sql, "SELECT * FROM generate_series(1, 3)");
        assert!(out.notes.is_empty());
    }

    #[test]
    fn a_for_update_lock_clause_is_not_a_table() {
        // `FOR UPDATE NOWAIT` ends a select; neither word starts a table.
        let mut cfg = config();
        cfg.table_prefix = Some("sys_".to_string());
        let out = apply("SELECT * FROM user FOR UPDATE NOWAIT", Some(mysql()), &cfg);
        assert_eq!(out.sql, "SELECT * FROM sys_user FOR UPDATE NOWAIT");
        assert_eq!(out.output_notes(), vec![KEY_TABLE_PREFIX]);
    }

    #[test]
    fn strings_and_comments_are_never_rewritten_or_injected() {
        let mut cfg = config();
        cfg.logic_delete = Some(ColumnValue {
            column: "deleted".to_string(),
            value: "0".to_string(),
        });
        cfg.table_prefix = Some("sys_".to_string());
        let sql = "SELECT * FROM user WHERE note = 'FROM x ORDER BY y' -- FROM z\n";
        let out = apply(sql, Some(mysql()), &cfg);
        assert!(out.sql.contains("FROM sys_user"), "{}", out.sql);
        assert!(out.sql.contains("'FROM x ORDER BY y'"), "{}", out.sql);
        assert!(out.sql.contains("-- FROM z"), "{}", out.sql);
    }

    #[test]
    fn a_trailing_semicolon_stays_last() {
        let mut cfg = config();
        cfg.logic_delete = Some(ColumnValue {
            column: "deleted".to_string(),
            value: "0".to_string(),
        });
        let out = apply("SELECT id FROM user;", Some(mysql()), &cfg);
        assert_eq!(out.sql, "SELECT id FROM user WHERE deleted = 0;");
    }

    #[test]
    fn a_with_statement_is_left_alone() {
        let mut cfg = config();
        cfg.logic_delete = Some(ColumnValue {
            column: "deleted".to_string(),
            value: "0".to_string(),
        });
        let sql = "WITH x AS (SELECT id FROM user) SELECT * FROM x";
        assert_eq!(apply(sql, Some(mysql()), &cfg).sql, sql);
    }

    /// The message keys the outcome carries, in order (test helper).
    impl InterceptOutcome {
        fn output_notes(&self) -> Vec<&'static str> {
            self.notes.iter().map(|note| note.message_key).collect()
        }
    }
}
