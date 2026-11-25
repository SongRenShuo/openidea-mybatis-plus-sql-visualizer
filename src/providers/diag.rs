//! Diagnostics for mapper SQL (T3-5): unknown columns and quote style.
//!
//! Two rules, both conservative:
//!
//! - **Unknown column** is reported only when the table's columns are known
//!   (from [`crate::providers::set_column_lookup`], the schema lane) and the
//!   column sits in a position this parser reads confidently (INSERT column
//!   list, `SET` left-hand side, a plain SELECT list item). No columns, no
//!   report — a missing schema must never look like a typo.
//! - **Quote style** is checked only when `sql.dialect` names a dialect this
//!   file knows: backticks for MySQL/MariaDB, double quotes for PostgreSQL/
//!   Oracle, brackets or double quotes for SQL Server. Double quotes are not
//!   flagged: in MySQL they are string literals, and guessing would be noise.
//!
//! Messages are English and carry a stable `code` (`diag.unknown_column`,
//! `diag.quote_style`) so the settings/i18n lane can translate them.

use crate::indexer::StatementKind;
use openidea_plugin::Severity;
use openidea_plugin::diagnostics::{Diagnostic, DiagnosticsProvider, Document};

pub const DIALECT_SETTING: &str = "sql.dialect";

impl DiagnosticsProvider for crate::Main {
    fn diagnostics(&mut self, doc: &Document) -> Vec<Diagnostic> {
        if !(super::is_xml(&doc.path) || super::is_java(&doc.path)) {
            return Vec::new();
        }
        diagnose(&doc.text, &super::dialect_setting(), &super::columns_for)
    }
}

/// The quote family of a dialect name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Family {
    MySql,
    Postgres,
    Oracle,
    SqlServer,
    Unknown,
}

fn family(dialect: &str) -> Family {
    match dialect.trim().to_ascii_lowercase().as_str() {
        "mysql" | "mariadb" => Family::MySql,
        "postgres" | "postgresql" | "pg" | "gaussdb" | "greenplum" | "kingbase" => Family::Postgres,
        "oracle" | "dm" | "dameng" => Family::Oracle,
        "sqlserver" | "mssql" | "sql server" => Family::SqlServer,
        _ => Family::Unknown,
    }
}

struct Finding {
    start: usize,
    end: usize,
    severity: Severity,
    message: String,
    code: &'static str,
}

/// Diagnostics for one mapper XML document (or a Java file with annotated SQL).
pub fn diagnose(
    text: &str,
    dialect: &str,
    columns: &dyn Fn(&str) -> Option<Vec<String>>,
) -> Vec<Diagnostic> {
    let family = family(dialect);
    let mut findings: Vec<Finding> = Vec::new();
    for region in super::sqltext::regions(text) {
        for (start, end) in region.spans {
            quote_findings(text, start, end, dialect, family, &mut findings);
            if let Some(kind) = region.kind {
                column_findings(text, start, end, kind, columns, &mut findings);
            }
        }
    }
    findings.sort_by_key(|f| (f.start, f.end, f.code));
    findings.dedup_by(|a, b| a.start == b.start && a.end == b.end && a.code == b.code);
    let mut positions = crate::indexer::scan::Positions::new(text);
    findings
        .into_iter()
        .map(|finding| {
            Diagnostic::new(
                positions.range(finding.start, finding.end),
                finding.severity,
                finding.message,
            )
            .code(finding.code)
        })
        .collect()
}

fn identifier_like(value: &str) -> bool {
    !value.trim().is_empty()
        && value
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '$' | '.'))
        && value
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
}

fn quote_findings(
    text: &str,
    start: usize,
    end: usize,
    dialect: &str,
    family: Family,
    out: &mut Vec<Finding>,
) {
    if family == Family::Unknown {
        return;
    }
    let bytes = text.as_bytes();
    let mut i = start;
    while i < end {
        let (close, allowed, label) = match bytes[i] {
            b'`' => (b'`', Family::MySql, "Backtick"),
            b'[' => (b']', Family::SqlServer, "Bracket"),
            // A double quote is a string literal in MySQL and valid quoting
            // elsewhere: never report it.
            b'"' | b'\'' => {
                i = skip_quoted(bytes, i, end);
                continue;
            }
            _ => {
                i += 1;
                continue;
            }
        };
        let Some(close_at) = bytes[i + 1..end]
            .iter()
            .position(|&b| b == close)
            .map(|n| i + 1 + n)
        else {
            i = skip_quoted(bytes, i, end);
            continue;
        };
        let inner = &text[i + 1..close_at];
        if identifier_like(inner) && family != allowed {
            out.push(Finding {
                start: i,
                end: close_at + 1,
                severity: Severity::Warning,
                message: format!("{label} quoting is not valid for {dialect}"),
                code: "diag.quote_style",
            });
        }
        i = close_at + 1;
    }
}

/// Just past the literal opening at `i` (its closing quote, or the end).
fn skip_quoted(bytes: &[u8], i: usize, end: usize) -> usize {
    let quote = bytes[i];
    let mut j = i + 1;
    while j < end {
        if bytes[j] == quote {
            return j + 1;
        }
        j += 1;
    }
    end
}

fn column_findings(
    text: &str,
    start: usize,
    end: usize,
    kind: StatementKind,
    columns: &dyn Fn(&str) -> Option<Vec<String>>,
    out: &mut Vec<Finding>,
) {
    let columns_of = |table: &str| -> Option<Vec<String>> {
        columns(table).map(|names| names.into_iter().map(|n| n.to_ascii_lowercase()).collect())
    };
    match kind {
        StatementKind::Insert => {
            let Some((table, table_end)) = after_keyword(text, start, end, "insert", "into") else {
                return;
            };
            let Some(names) = columns_of(&table) else {
                return;
            };
            for (name, name_start, name_end) in parenthesised_items(text, table_end, end) {
                if name.contains('#') || !identifier_like(&name) {
                    continue;
                }
                if !names.contains(&name.to_ascii_lowercase()) {
                    out.push(unknown_column(&name, &table, name_start, name_end));
                }
            }
        }
        StatementKind::Update => {
            let Some((table, table_end)) = after_keyword(text, start, end, "update", "") else {
                return;
            };
            let Some(names) = columns_of(&table) else {
                return;
            };
            let Some(set_at) = find_word(text, "set", table_end, end) else {
                return;
            };
            let assignments_end = find_word(text, "where", set_at + 3, end).unwrap_or(end);
            for (item_start, item_end) in split_commas(text, set_at + 3, assignments_end) {
                let Some(eq) = text[item_start..item_end].find('=') else {
                    continue;
                };
                let lhs = &text[item_start..item_start + eq];
                let Some((name, name_start, name_end)) = column_token(lhs, item_start) else {
                    continue;
                };
                if !names.contains(&name.to_ascii_lowercase()) {
                    out.push(unknown_column(&name, &table, name_start, name_end));
                }
            }
        }
        StatementKind::Select => {
            let Some(from_at) = find_word(text, "from", start, end) else {
                return;
            };
            let Some((table, _)) = after_keyword(text, from_at, end, "from", "") else {
                return;
            };
            let Some(names) = columns_of(&table) else {
                return;
            };
            for (item_start, item_end) in split_commas(text, start + "select".len(), from_at) {
                let item = &text[item_start..item_end];
                if item.contains('(') || item.trim().is_empty() {
                    continue;
                }
                let Some((name, name_start, name_end)) = column_token(item, item_start) else {
                    continue;
                };
                if !names.contains(&name.to_ascii_lowercase()) {
                    out.push(unknown_column(&name, &table, name_start, name_end));
                }
            }
        }
        StatementKind::Delete => {}
    }
}

fn unknown_column(name: &str, table: &str, start: usize, end: usize) -> Finding {
    Finding {
        start,
        end,
        severity: Severity::Error,
        message: format!("Unknown column \"{name}\" for table {table}"),
        code: "diag.unknown_column",
    }
}

/// `keyword [second] <name>` inside `start..end`: the name and the offset just
/// past it.
fn after_keyword(
    text: &str,
    start: usize,
    end: usize,
    keyword: &str,
    second: &str,
) -> Option<(String, usize)> {
    let first = find_word(text, keyword, start, end)?;
    let mut i = super::sqltext::skip_ws(text, first + keyword.len());
    if !second.is_empty() {
        if !find_word(text, second, i, end).is_some_and(|at| at == i) {
            return None;
        }
        i = super::sqltext::skip_ws(text, i + second.len());
    }
    let (name_start, name_end) = super::sqltext::ident_span(text, i)?;
    let name = super::sqltext::unquote(&text[name_start..name_end]).to_owned();
    (!name.is_empty()).then_some((name, name_end))
}

/// The offset of `word` in `start..end` at a word boundary.
fn find_word(text: &str, word: &str, start: usize, end: usize) -> Option<usize> {
    let mut from = start;
    while let Some(at) = super::sqltext::find_ci(text, word, from) {
        if at >= end {
            return None;
        }
        let before_ok = at == 0
            || !text.as_bytes()[at - 1].is_ascii_alphanumeric() && text.as_bytes()[at - 1] != b'_';
        let after = at + word.len();
        let after_ok = after >= text.len()
            || !text.as_bytes()[after].is_ascii_alphanumeric() && text.as_bytes()[after] != b'_';
        if before_ok && after_ok {
            return Some(at);
        }
        from = at + 1;
    }
    None
}

/// Items of a parenthesised list right after `from`, as (text, start, end).
fn parenthesised_items(text: &str, from: usize, end: usize) -> Vec<(String, usize, usize)> {
    let i = super::sqltext::skip_ws(text, from);
    if i >= end || text.as_bytes().get(i) != Some(&b'(') {
        return Vec::new();
    }
    let Some(close) = text[i..end].find(')').map(|n| i + n) else {
        return Vec::new();
    };
    split_commas(text, i + 1, close)
        .into_iter()
        .filter_map(|(s, e)| {
            let (ts, te) = crate::indexer::scan::trim_span(text, s, e);
            (ts < te).then(|| (text[ts..te].to_owned(), ts, te))
        })
        .collect()
}

/// Commas at nesting depth 0 inside `start..end`.
fn split_commas(text: &str, start: usize, end: usize) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut s = start;
    let mut i = start;
    while i < end {
        let b = bytes[i];
        if let Some(q) = quote {
            if b == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' | b'"' | b'`' => {
                quote = Some(b);
                i += 1;
            }
            b'(' | b'[' => {
                depth += 1;
                i += 1;
            }
            b')' | b']' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b',' if depth == 0 => {
                out.push((s, i));
                s = i + 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    if s < end {
        out.push((s, end));
    }
    out
}

/// The column name in a SELECT list item or SET left-hand side, with the span
/// of the name itself. Aliases (`id AS x`), table prefixes (`t.id`), quoting
/// and literals are handled here.
fn column_token(part: &str, base: usize) -> Option<(String, usize, usize)> {
    let part = part.trim_start();
    let lead = part.len() - part.trim_start().len();
    let first = part.split_whitespace().next()?;
    if first.is_empty() || first.contains('(') || first.starts_with('\'') || first.starts_with('*')
    {
        return None;
    }
    // `t.id` → `id`; `.` inside a quoted name is part of the name.
    let (offset, token) = match first.rfind('.') {
        Some(dot)
            if !first.starts_with('`') && !first.starts_with('"') && !first.starts_with('[') =>
        {
            (dot + 1, &first[dot + 1..])
        }
        _ => (0, first),
    };
    let token_start = base + lead + offset;
    let span = super::sqltext::ident_span(part, offset)?;
    let name = super::sqltext::unquote(&part[span.0..span.1]).to_owned();
    if !identifier_like(&name) {
        return None;
    }
    Some((name, token_start, token_start + token.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    use openidea_plugin::providers::Document;

    fn schema(table: &str) -> Option<Vec<String>> {
        match table {
            "t_user" => Some(vec!["id".into(), "name".into(), "created_at".into()]),
            _ => None,
        }
    }

    fn none(_: &str) -> Option<Vec<String>> {
        None
    }

    fn codes(diagnostics: &[Diagnostic]) -> Vec<&str> {
        diagnostics
            .iter()
            .filter_map(|d| d.code.as_deref())
            .collect()
    }

    #[test]
    fn without_a_schema_no_column_is_ever_unknown() {
        let xml = "<mapper namespace=\"n\"><select id=\"a\">select id, nmae from t_user where nmae = 1</select></mapper>";
        assert!(diagnose(xml, "mysql", &none).is_empty());
    }

    #[test]
    fn insert_column_list_is_checked_against_the_table() {
        let xml = "<mapper namespace=\"n\"><insert id=\"a\">insert into t_user (id, nmae) values (1, 'x')</insert></mapper>";
        let found = diagnose(xml, "", &schema);
        assert_eq!(codes(&found), vec!["diag.unknown_column"]);
        assert!(found[0].message.contains("nmae"));
        assert_eq!(found[0].severity, Severity::Error);
    }

    #[test]
    fn select_and_update_columns_are_checked() {
        let select = "<mapper namespace=\"n\"><select id=\"a\">select id, nmae from t_user</select></mapper>";
        assert_eq!(
            codes(&diagnose(select, "", &schema)),
            vec!["diag.unknown_column"]
        );
        let update = "<mapper namespace=\"n\"><update id=\"a\">update t_user set nmae = #{n}, name = #{name}</update></mapper>";
        let found = diagnose(update, "", &schema);
        assert_eq!(codes(&found), vec!["diag.unknown_column"]);
    }

    #[test]
    fn known_columns_and_aliases_stay_quiet() {
        let xml = "<mapper namespace=\"n\"><select id=\"a\">select u.id, u.name as n, created_at from t_user u</select></mapper>";
        assert!(diagnose(xml, "", &schema).is_empty());
    }

    #[test]
    fn a_table_the_schema_does_not_know_is_quiet() {
        let xml = "<mapper namespace=\"n\"><select id=\"a\">select whatever from t_other</select></mapper>";
        assert!(diagnose(xml, "", &schema).is_empty());
    }

    #[test]
    fn quote_style_follows_the_dialect() {
        let backtick =
            "<mapper namespace=\"n\"><select id=\"a\">select `id` from t_user</select></mapper>";
        let bracket =
            "<mapper namespace=\"n\"><select id=\"a\">select [id] from t_user</select></mapper>";
        assert!(diagnose(backtick, "mysql", &none).is_empty());
        assert_eq!(
            codes(&diagnose(backtick, "postgres", &none)),
            vec!["diag.quote_style"]
        );
        assert_eq!(
            codes(&diagnose(bracket, "mysql", &none)),
            vec!["diag.quote_style"]
        );
        assert!(diagnose(bracket, "sqlserver", &none).is_empty());
        // No dialect: no quote opinion.
        assert!(diagnose(backtick, "", &none).is_empty());
    }

    #[test]
    fn a_non_mapper_root_and_string_literals_are_not_reported() {
        assert!(
            diagnose(
                "<beans><select id=\"x\">select `id`</select></beans>",
                "postgres",
                &none
            )
            .is_empty()
        );
        let xml =
            "<mapper namespace=\"n\"><select id=\"a\">select 'a ` b' from t_user</select></mapper>";
        assert!(diagnose(xml, "postgres", &none).is_empty());
    }

    #[test]
    fn the_trait_impl_uses_the_dialect_setting() {
        let host = MockHost::install();
        let xml =
            "<mapper namespace=\"n\"><select id=\"a\">select [id] from t_user</select></mapper>";
        let document = Document {
            path: "/p/M.xml".into(),
            language: "xml".into(),
            version: 1,
            text: xml.into(),
        };
        let mut plugin = crate::Main;
        assert!(DiagnosticsProvider::diagnostics(&mut plugin, &document).is_empty());
        host.set_setting(DIALECT_SETTING, "\"mysql\"");
        assert_eq!(
            DiagnosticsProvider::diagnostics(&mut plugin, &document).len(),
            1
        );
        assert!(
            DiagnosticsProvider::diagnostics(
                &mut plugin,
                &Document {
                    path: "/p/README.md".into(),
                    language: "markdown".into(),
                    version: 1,
                    text: "x".into()
                }
            )
            .is_empty()
        );
    }
}
