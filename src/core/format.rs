//! SQL formatting per dialect (T1-8); formatting must be idempotent.
//!
//! Behavioural port of the IntelliJ edition's `ui/highlight/SqlFormatter.kt`
//! DML path (read-only behaviour baseline; rewritten in Rust). Produces a
//! stable, readable layout: keywords cased per dialect, one major clause per
//! line, column lists broken out, subqueries indented, string literals and
//! quoted identifiers preserved verbatim.
//!
//! **Idempotency is the hard requirement**: `format(format(x)) == format(x)`
//! for every input. The formatter works purely on a token stream, so re-running
//! it on its own output re-tokenizes the same tokens and applies the same rules.

use crate::core::dialect::{Dialect, KeywordCase};

/// Keywords that start a new line (major clauses). `JOIN` is handled specially
/// in the loop, but is listed here for readability of the set.
const NEW_LINE_KEYWORDS: &[&str] = &[
    "SELECT", "FROM", "WHERE", "GROUP", "ORDER", "HAVING", "LIMIT", "UPDATE", "SET", "DELETE",
    "INSERT", "INTO", "VALUES", "UNION", "JOIN", "LEFT", "RIGHT", "INNER", "FULL", "CROSS",
];

/// Join modifiers that keep `JOIN` on the same line.
const JOIN_QUALIFIERS: &[&str] = &[
    "LEFT", "RIGHT", "INNER", "FULL", "CROSS", "OUTER", "NATURAL",
];

/// Function names whose `(` is glued (no space before the parenthesis).
const FUNCTION_NAMES: &[&str] = &[
    "COUNT",
    "SUM",
    "MAX",
    "MIN",
    "AVG",
    "COALESCE",
    "CAST",
    "CONVERT",
    "CONCAT",
    "NULLIF",
    "IFNULL",
    "LENGTH",
    "SUBSTRING",
    "SUBSTR",
    "REPLACE",
    "UPPER",
    "LOWER",
    "TRIM",
    "LTRIM",
    "RTRIM",
    "ROUND",
    "ABS",
    "FLOOR",
    "CEIL",
    "CEILING",
    "NOW",
    "CURDATE",
    "DATE_FORMAT",
    "STR_TO_DATE",
    "GROUP_CONCAT",
    "JSON_EXTRACT",
    "DATEDIFF",
    "DATEADD",
];

/// Recognised SQL keywords (uppercase) — used to case identifiers-as-keywords.
const KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "GROUP",
    "BY",
    "ORDER",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "FETCH",
    "FIRST",
    "NEXT",
    "ROWS",
    "ROW",
    "ONLY",
    "DISTINCT",
    "AS",
    "ON",
    "USING",
    "JOIN",
    "LEFT",
    "RIGHT",
    "INNER",
    "OUTER",
    "FULL",
    "CROSS",
    "NATURAL",
    "AND",
    "OR",
    "NOT",
    "IN",
    "EXISTS",
    "IS",
    "NULL",
    "TRUE",
    "FALSE",
    "LIKE",
    "BETWEEN",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "UNION",
    "ALL",
    "INTERSECT",
    "EXCEPT",
    "INSERT",
    "INTO",
    "VALUES",
    "VALUE",
    "UPDATE",
    "SET",
    "DELETE",
    "CREATE",
    "ALTER",
    "DROP",
    "TABLE",
    "INDEX",
    "PRIMARY",
    "KEY",
    "FOREIGN",
    "REFERENCES",
    "CONSTRAINT",
    "DEFAULT",
    "UNIQUE",
    "CHECK",
    "ADD",
    "COLUMN",
    "COUNT",
    "SUM",
    "AVG",
    "MIN",
    "MAX",
    "COALESCE",
    "CAST",
    "CONVERT",
    "ASC",
    "DESC",
    "WITH",
    "RECURSIVE",
    "OVER",
    "PARTITION",
    "WINDOW",
    "RETURNING",
    "DUPLICATE",
    "CONFLICT",
    "DO",
    "NOTHING",
    "EXCLUDED",
    "MERGE",
    "MATCHED",
    "TOP",
    "IF",
    "IFNULL",
    "NULLIF",
    "SUBSTRING",
    "TRIM",
    "UPPER",
    "LOWER",
    "ROUND",
    "ABS",
    "NOW",
    "CURRENT_DATE",
    "CURRENT_TIME",
    "CURRENT_TIMESTAMP",
    "CURRENT_USER",
    "INTEGER",
    "INT",
    "BIGINT",
    "SMALLINT",
    "VARCHAR",
    "CHAR",
    "TEXT",
    "DATE",
    "TIME",
    "TIMESTAMP",
    "BOOLEAN",
    "DECIMAL",
    "NUMERIC",
    "FLOAT",
    "DOUBLE",
    "REAL",
    "BLOB",
    "BEGIN",
    "COMMIT",
    "ROLLBACK",
    "TRANSACTION",
    "START",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Keyword,
    Word,
    Number,
    Str,
    QuotedIdent,
    LineComment,
    BlockComment,
    Punct,
    Op,
}

#[derive(Debug, Clone)]
struct Token {
    kind: Kind,
    text: String,
}

fn is_keyword(word: &str) -> bool {
    let up = word.to_ascii_uppercase();
    KEYWORDS.contains(&up.as_str())
}

fn tokenize(sql: &str) -> Vec<Token> {
    let chars: Vec<char> = sql.chars().collect();
    let n = chars.len();
    let mut i = 0usize;
    let mut toks: Vec<Token> = Vec::new();

    while i < n {
        let c = chars[i];

        if c.is_whitespace() {
            i += 1;
            continue;
        }

        // Single-quoted string literal (with '' escape).
        if c == '\'' {
            let start = i;
            i += 1;
            while i < n {
                if chars[i] == '\'' {
                    if i + 1 < n && chars[i + 1] == '\'' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            toks.push(Token {
                kind: Kind::Str,
                text: chars[start..i].iter().collect(),
            });
            continue;
        }

        // Quoted identifier: "…" / `…` / […]
        if c == '"' || c == '`' || c == '[' {
            let close = if c == '[' { ']' } else { c };
            let start = i;
            i += 1;
            while i < n {
                if chars[i] == close {
                    if close != '[' && i + 1 < n && chars[i + 1] == close {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            toks.push(Token {
                kind: Kind::QuotedIdent,
                text: chars[start..i].iter().collect(),
            });
            continue;
        }

        // Line comment.
        if c == '-' && i + 1 < n && chars[i + 1] == '-' {
            let start = i;
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            toks.push(Token {
                kind: Kind::LineComment,
                text: chars[start..i].iter().collect(),
            });
            continue;
        }

        // Block comment.
        if c == '/' && i + 1 < n && chars[i + 1] == '*' {
            let start = i;
            i += 2;
            while i < n {
                if chars[i] == '*' && i + 1 < n && chars[i + 1] == '/' {
                    i += 2;
                    break;
                }
                i += 1;
            }
            toks.push(Token {
                kind: Kind::BlockComment,
                text: chars[start..i].iter().collect(),
            });
            continue;
        }

        // Number.
        if c.is_ascii_digit() || (c == '.' && i + 1 < n && chars[i + 1].is_ascii_digit()) {
            let start = i;
            while i < n && chars[i].is_ascii_digit() {
                i += 1;
            }
            if i < n && chars[i] == '.' {
                i += 1;
                while i < n && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
            if i < n && (chars[i] == 'e' || chars[i] == 'E') {
                let save = i;
                i += 1;
                if i < n && (chars[i] == '+' || chars[i] == '-') {
                    i += 1;
                }
                let ds = i;
                while i < n && chars[i].is_ascii_digit() {
                    i += 1;
                }
                if i == ds {
                    i = save;
                }
            }
            toks.push(Token {
                kind: Kind::Number,
                text: chars[start..i].iter().collect(),
            });
            continue;
        }

        // Word (identifier or keyword).
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < n && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$') {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            let kind = if is_keyword(&text) {
                Kind::Keyword
            } else {
                Kind::Word
            };
            toks.push(Token { kind, text });
            continue;
        }

        // Multi-char operators.
        if i + 1 < n {
            let two: String = chars[i..i + 2].iter().collect();
            if matches!(two.as_str(), "<=" | ">=" | "<>" | "!=" | "||" | "::" | ":=") {
                toks.push(Token {
                    kind: Kind::Op,
                    text: two,
                });
                i += 2;
                continue;
            }
        }

        // Structural punctuation.
        if matches!(c, '(' | ')' | ',' | '.' | ';') {
            toks.push(Token {
                kind: Kind::Punct,
                text: c.to_string(),
            });
            i += 1;
            continue;
        }

        // Any other single character is an operator.
        toks.push(Token {
            kind: Kind::Op,
            text: c.to_string(),
        });
        i += 1;
    }

    toks
}

struct Fmt {
    out: String,
    depth: usize,
    line_indent: usize,
    paren_indents: Vec<usize>,
    paren_newlined: bool,
    pending_space: bool,
    at_line_start: bool,
    prev_upper: String,
    current_clause: String,
    case: KeywordCase,
}

impl Fmt {
    fn flush_space(&mut self) {
        if self.pending_space
            && !self.at_line_start
            && !self.out.is_empty()
            && !self.out.ends_with(' ')
        {
            self.out.push(' ');
        }
        self.pending_space = false;
    }

    fn newline(&mut self, indent: usize) {
        self.out.push('\n');
        for _ in 0..indent {
            self.out.push(' ');
        }
        self.line_indent = indent;
        self.at_line_start = true;
        self.pending_space = false;
        if self.depth > 0 {
            self.paren_newlined = true;
        }
    }

    fn sub_indent(&self) -> usize {
        if self.depth > 0 {
            self.paren_indents.last().copied().unwrap_or(0) + 4
        } else {
            0
        }
    }

    fn kw(&self, upper: &str) -> String {
        match self.case {
            KeywordCase::Upper => upper.to_string(),
            KeywordCase::Lower => upper.to_ascii_lowercase(),
        }
    }

    fn push_keyword(&mut self, upper: &str) {
        let kw = self.kw(upper);
        self.out.push_str(&kw);
        self.at_line_start = false;
        self.pending_space = true;
    }
}

/// Format `sql` for `dialect`. Idempotent: `format(format(x)) == format(x)`.
pub fn format(sql: &str, dialect: &Dialect) -> String {
    let tokens = tokenize(sql);
    if tokens.is_empty() {
        return sql.trim().to_string();
    }

    let mut f = Fmt {
        out: String::with_capacity(sql.len() + 64),
        depth: 0,
        line_indent: 0,
        paren_indents: Vec::new(),
        paren_newlined: false,
        pending_space: false,
        at_line_start: true,
        prev_upper: String::new(),
        current_clause: String::new(),
        case: dialect.capabilities.keyword_case,
    };

    for tok in &tokens {
        let upper = tok.text.to_ascii_uppercase();
        match tok.kind {
            Kind::Keyword => {
                if upper == "JOIN" {
                    let qualifier = JOIN_QUALIFIERS.contains(&f.prev_upper.as_str());
                    if qualifier {
                        f.flush_space();
                    } else if !f.at_line_start {
                        let ind = f.sub_indent();
                        f.newline(ind);
                    }
                    f.push_keyword(&upper);
                    f.current_clause = "JOIN".to_string();
                } else if NEW_LINE_KEYWORDS.contains(&upper.as_str()) {
                    if !f.at_line_start {
                        let ind = f.sub_indent();
                        f.newline(ind);
                    }
                    f.pending_space = false;
                    f.current_clause = upper.clone();
                    f.push_keyword(&upper);
                    if upper == "SELECT" || upper == "SET" {
                        let ind = f.line_indent + 4;
                        f.newline(ind);
                    }
                } else if upper == "AND" || upper == "OR" {
                    if f.depth == 0 {
                        if !f.at_line_start {
                            f.newline(4);
                        }
                    } else if f.paren_newlined {
                        let ind = f.paren_indents.last().copied().unwrap_or(0) + 8;
                        if !f.at_line_start {
                            f.newline(ind);
                        }
                    } else {
                        f.flush_space();
                    }
                    f.push_keyword(&upper);
                } else {
                    f.flush_space();
                    f.push_keyword(&upper);
                }
            }
            Kind::Punct => match tok.text.as_str() {
                "(" => {
                    if !FUNCTION_NAMES.contains(&f.prev_upper.as_str()) {
                        f.flush_space();
                    }
                    f.out.push('(');
                    f.depth += 1;
                    f.paren_indents.push(f.line_indent);
                    f.paren_newlined = false;
                    f.pending_space = false;
                    f.at_line_start = false;
                }
                ")" => {
                    if f.paren_newlined && f.depth > 0 {
                        let ind = f.paren_indents.last().copied().unwrap_or(0);
                        f.newline(ind);
                    }
                    f.out.push(')');
                    if f.depth > 0 {
                        f.depth -= 1;
                        f.paren_indents.pop();
                    }
                    f.paren_newlined = false;
                    f.pending_space = true;
                    f.at_line_start = false;
                }
                "," => {
                    if f.out.ends_with(' ') {
                        f.out.pop();
                    }
                    f.out.push(',');
                    let in_column_list =
                        f.depth == 0 && (f.current_clause == "SELECT" || f.current_clause == "SET");
                    if in_column_list && !f.at_line_start {
                        f.newline(4);
                    } else {
                        f.pending_space = true;
                        f.at_line_start = false;
                    }
                }
                "." => {
                    f.out.push('.');
                    f.pending_space = false;
                    f.at_line_start = false;
                }
                ";" => {
                    if f.out.ends_with(' ') {
                        f.out.pop();
                    }
                    f.out.push(';');
                    f.at_line_start = false;
                    f.pending_space = true;
                }
                _ => {
                    f.flush_space();
                    f.out.push_str(&tok.text);
                    f.pending_space = true;
                    f.at_line_start = false;
                }
            },
            Kind::Op => {
                f.flush_space();
                f.out.push_str(&tok.text);
                f.pending_space = true;
                f.at_line_start = false;
            }
            Kind::LineComment | Kind::BlockComment => {
                if !f.at_line_start {
                    f.out.push('\n');
                }
                f.out.push_str(&tok.text);
                f.out.push('\n');
                f.at_line_start = true;
                f.pending_space = false;
            }
            Kind::Word | Kind::Number | Kind::Str | Kind::QuotedIdent => {
                f.flush_space();
                f.out.push_str(&tok.text);
                f.pending_space = true;
                f.at_line_start = false;
            }
        }
        f.prev_upper = upper;
    }

    f.out.trim().to_string()
}

#[cfg(test)]
mod tests {
    //! Idempotency corpus (T1-8 acceptance): `format(format(x)) == format(x)`
    //! plus structural assertions on real output strings.
    //!
    //! Parity source: IntelliJ `SqlFormatterTest.kt` (behaviour baseline).

    use super::*;
    use crate::core::dialect::by_name;

    fn d(name: &str) -> &'static Dialect {
        by_name(name).unwrap_or_else(|| panic!("dialect {name} missing"))
    }

    /// Assert idempotency and return the first-pass output.
    fn idem(sql: &str, dialect: &str) -> String {
        let once = format(sql, d(dialect));
        let twice = format(&once, d(dialect));
        assert_eq!(
            twice, once,
            "not idempotent for:\n{sql}\n--- once ---\n{once}"
        );
        once
    }

    #[test]
    fn idempotent_simple_select() {
        let out = idem("select id,name from t where a=1", "MySQL");
        assert!(out.starts_with("SELECT\n"), "got: {out}");
        assert!(out.contains("FROM t"), "got: {out}");
        assert!(out.contains("WHERE a = 1"), "got: {out}");
    }

    #[test]
    fn idempotent_join() {
        let out = idem(
            "select a.id,b.name from t1 a left join t2 b on a.id=b.id where a.x>0",
            "MySQL",
        );
        assert!(out.contains("LEFT JOIN t2 b"), "got: {out}");
        assert!(out.contains("ON a.id = b.id"), "got: {out}");
    }

    #[test]
    fn idempotent_subquery_in_where() {
        let out = idem(
            "select * from t where id in (select id from u where status=1)",
            "PostgreSQL",
        );
        assert_eq!(
            out,
            "SELECT\n    *\nFROM t\nWHERE id IN (\n    SELECT\n        id\n    FROM u\n    WHERE status = 1\n)"
        );
    }

    #[test]
    fn idempotent_case_expression() {
        let out = idem(
            "select case when a=1 then 'x' else 'y' end as k from t",
            "MySQL",
        );
        assert!(
            out.contains("CASE WHEN a = 1 THEN 'x' ELSE 'y' END"),
            "got: {out}"
        );
    }

    #[test]
    fn idempotent_union() {
        let out = idem("select id from a union all select id from b", "MySQL");
        assert!(out.contains("UNION ALL"), "got: {out}");
    }

    #[test]
    fn idempotent_nested_subquery_and_join() {
        idem(
            "select * from (select id from a where x in (select y from b)) s join c on s.id=c.id",
            "Oracle",
        );
    }

    #[test]
    fn idempotent_insert_values() {
        let out = idem("insert into t(a,b) values(1,'x')", "MySQL");
        assert!(out.contains("INSERT"), "got: {out}");
        assert!(out.contains("VALUES (1, 'x')"), "got: {out}");
    }

    #[test]
    fn idempotent_update() {
        let out = idem("update t set a=1,b='x' where id=2", "MySQL");
        assert!(out.contains("UPDATE t"), "got: {out}");
        assert!(out.contains("SET"), "got: {out}");
    }

    #[test]
    fn idempotent_delete() {
        let out = idem("delete from t where id=1", "MySQL");
        assert!(out.starts_with("DELETE"), "got: {out}");
    }

    #[test]
    fn idempotent_group_order_having() {
        let out = idem(
            "select dept,count(*) from t group by dept having count(*)>1 order by dept desc",
            "MySQL",
        );
        assert!(out.contains("GROUP BY dept"), "got: {out}");
        assert!(out.contains("ORDER BY dept DESC"), "got: {out}");
    }

    #[test]
    fn strings_and_quoted_idents_preserved() {
        let out = idem(
            "select `weird col` from t where note='A?--not a comment' and x=\"Y\"",
            "MySQL",
        );
        assert!(out.contains("'A?--not a comment'"), "got: {out}");
        assert!(out.contains("`weird col`"), "got: {out}");
        assert!(out.contains("\"Y\""), "got: {out}");
    }

    #[test]
    fn comments_are_isolated_on_own_line() {
        let out = idem("select id /* keep me */ from t -- trailing", "MySQL");
        assert!(out.contains("/* keep me */"), "got: {out}");
        assert!(out.contains("-- trailing"), "got: {out}");
        // Idempotency is the real check (done inside idem()).
    }

    #[test]
    fn already_formatted_is_stable() {
        let sql = "SELECT\n    id\nFROM t\nWHERE a = 1";
        assert_eq!(format(sql, d("MySQL")), sql);
    }

    #[test]
    fn empty_input() {
        assert_eq!(format("   ", d("MySQL")), "");
        assert_eq!(format("", d("MySQL")), "");
    }
}
