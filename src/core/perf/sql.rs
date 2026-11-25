//! Dependency-free SQL scanning shared by the performance rules and the
//! generators.
//!
//! The port cannot use JSqlParser, so this module keeps a byte-range aware
//! tokenizer plus enough structure recovery for the rules: statement kind,
//! parenthesis depth, top-level clause positions, table references and simple
//! column predicates. It is deliberately conservative: when the shape is not
//! recognised, callers treat the input as unknown instead of guessing.
//!
//! Notes:
//! - comments (`--`, `/* */`) are dropped before analysis; string literals and
//!   quoted identifiers are single tokens, so keywords inside them never
//!   trigger a rule;
//! - `depth[i]` is the parenthesis nesting of token `i`; top level is `0`;
//! - byte ranges are kept for `Finding::range` (UTF-8 offsets).

/// Token classes the scanner distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenKind {
    /// Unquoted identifier or keyword.
    Word,
    /// Quoted identifier; the payload is the opening quote character
    /// (`"`, `` ` `` or `[`).
    QuotedIdent(char),
    /// Single-quoted string literal (also `N'...'` and `E'...'` prefixes).
    Str,
    /// Numeric literal.
    Number,
    /// Bind placeholder: `?`, `:name`, `$1`, `#{...}`, `${...}`.
    Param,
    /// Structural punctuation: `( ) , ; . *`.
    Punct(char),
    /// Operator.
    Op,
}

/// One lexical token with its original byte span.
#[derive(Debug, Clone)]
pub(crate) struct Token {
    pub kind: TokenKind,
    /// Raw source text (quotes included for literals and quoted identifiers).
    pub text: String,
    /// Unescaped payload for strings and quoted identifiers (`None` otherwise).
    pub value: Option<String>,
    pub start: usize,
    pub end: usize,
    /// ASCII upper-case form of `text`; convenient for keyword comparison.
    pub upper: String,
}

impl Token {
    pub fn is_word(&self, word: &str) -> bool {
        self.kind == TokenKind::Word && self.upper == word
    }

    pub fn is_punct(&self, ch: char) -> bool {
        self.kind == TokenKind::Punct(ch)
    }

    /// Identifier payload: unescaped name for quoted identifiers, raw text for
    /// words. Returns `None` for literals, numbers, params and punctuation.
    pub fn ident_name(&self) -> Option<&str> {
        match self.kind {
            TokenKind::Word => Some(&self.text),
            TokenKind::QuotedIdent(_) => self.value.as_deref(),
            _ => None,
        }
    }

    pub fn string_value(&self) -> Option<&str> {
        match self.kind {
            TokenKind::Str => self.value.as_deref(),
            _ => None,
        }
    }

    pub fn number_value(&self) -> Option<&str> {
        match self.kind {
            TokenKind::Number => Some(&self.text),
            _ => None,
        }
    }
}

struct Scanner<'a> {
    sql: &'a str,
    chars: Vec<(usize, char)>,
    i: usize,
}

impl<'a> Scanner<'a> {
    fn new(sql: &'a str) -> Self {
        Self {
            sql,
            chars: sql.char_indices().collect(),
            i: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.i).map(|(_, c)| *c)
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.chars.get(self.i + n).map(|(_, c)| *c)
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.i += 1;
        }
        c
    }

    fn byte(&self) -> usize {
        match self.chars.get(self.i) {
            Some((b, _)) => *b,
            None => self.sql.len(),
        }
    }
}

fn is_word_start(c: char) -> bool {
    c.is_alphabetic() || matches!(c, '_' | '$' | '@' | '#')
}

fn is_word_continue(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '$' | '@' | '#')
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || matches!(c, '_' | '$' | '@' | '#')
}

/// Tokenize `sql`, dropping whitespace and comments.
pub(crate) fn tokenize(sql: &str) -> Vec<Token> {
    let mut sc = Scanner::new(sql);
    let mut out = Vec::new();
    loop {
        while let Some(c) = sc.peek() {
            if c.is_whitespace() {
                sc.bump();
            } else {
                break;
            }
        }
        let Some(c) = sc.peek() else { break };

        // Comments.
        if c == '-' && sc.peek_at(1) == Some('-') {
            while let Some(ch) = sc.bump() {
                if ch == '\n' {
                    break;
                }
            }
            continue;
        }
        if c == '/' && sc.peek_at(1) == Some('*') {
            sc.bump();
            sc.bump();
            while let Some(ch) = sc.bump() {
                if ch == '*' && sc.peek() == Some('/') {
                    sc.bump();
                    break;
                }
            }
            continue;
        }

        let start = sc.byte();
        let kind;
        let mut value = None;

        if c == '\'' {
            kind = TokenKind::Str;
            value = Some(scan_quoted(&mut sc, '\'', '\''));
        } else if c == '"' || c == '`' {
            kind = TokenKind::QuotedIdent(c);
            value = Some(scan_quoted(&mut sc, c, c));
        } else if c == '[' {
            kind = TokenKind::QuotedIdent('[');
            value = Some(scan_quoted(&mut sc, '[', ']'));
        } else if c == '?' {
            sc.bump();
            kind = TokenKind::Param;
        } else if c == ':' && sc.peek_at(1) != Some(':') {
            sc.bump();
            while let Some(ch) = sc.peek() {
                if is_ident_start(ch) || ch.is_ascii_digit() {
                    sc.bump();
                } else {
                    break;
                }
            }
            kind = TokenKind::Param;
        } else if c == '$' && sc.peek_at(1).is_some_and(|ch| ch.is_ascii_digit()) {
            sc.bump();
            while sc.peek().is_some_and(|ch| ch.is_ascii_digit()) {
                sc.bump();
            }
            kind = TokenKind::Param;
        } else if (c == '#' || c == '$') && sc.peek_at(1) == Some('{') {
            sc.bump();
            sc.bump();
            while let Some(ch) = sc.bump() {
                if ch == '}' {
                    break;
                }
            }
            kind = TokenKind::Param;
        } else if c.is_ascii_digit()
            || (c == '.' && sc.peek_at(1).is_some_and(|d| d.is_ascii_digit()))
        {
            scan_number(&mut sc);
            kind = TokenKind::Number;
        } else if is_word_start(c) {
            scan_word(&mut sc);
            let word = &sql[start..sc.byte()];
            // String prefixes (`N'...'`, `E'...'`, `X'...'`) stay one token.
            if word.len() == 1
                && matches!(
                    word.as_bytes()[0],
                    b'N' | b'n' | b'E' | b'e' | b'X' | b'x' | b'B' | b'b'
                )
                && sc.peek() == Some('\'')
            {
                value = Some(scan_quoted(&mut sc, '\'', '\''));
                kind = TokenKind::Str;
            } else {
                kind = TokenKind::Word;
            }
        } else if matches!(c, '(' | ')' | ',' | ';' | '.' | '*') {
            sc.bump();
            kind = TokenKind::Punct(c);
        } else {
            // Operators, longest match first.
            let two = format!("{}{}", c, sc.peek_at(1).unwrap_or('\0'));
            const TWO: &[&str] = &["<=", ">=", "<>", "!=", "||", "::", ":=", "->"];
            if TWO.contains(&two.as_str()) {
                sc.bump();
                sc.bump();
            } else {
                sc.bump();
            }
            kind = TokenKind::Op;
        }

        let end = sc.byte();
        let text = sql[start..end].to_string();
        let upper = text.to_ascii_uppercase();
        out.push(Token {
            kind,
            text,
            value,
            start,
            end,
            upper,
        });
    }
    out
}

/// Consume a quoted region starting at the opening quote.
fn scan_quoted(sc: &mut Scanner<'_>, open: char, close: char) -> String {
    debug_assert_eq!(sc.peek(), Some(open));
    sc.bump();
    let mut value = String::new();
    while let Some(ch) = sc.bump() {
        if ch == '\\' && open == '\'' {
            // MySQL backslash escapes inside string literals.
            if let Some(next) = sc.bump() {
                value.push(next);
            }
            continue;
        }
        if ch == close {
            if sc.peek() == Some(close) {
                sc.bump();
                value.push(close);
                continue;
            }
            break;
        }
        value.push(ch);
    }
    value
}

fn scan_number(sc: &mut Scanner<'_>) {
    while sc.peek().is_some_and(|c| c.is_ascii_digit()) {
        sc.bump();
    }
    if sc.peek() == Some('.') && sc.peek_at(1).is_some_and(|c| c.is_ascii_digit()) {
        sc.bump();
        while sc.peek().is_some_and(|c| c.is_ascii_digit()) {
            sc.bump();
        }
    }
    if sc.peek().is_some_and(|c| c == 'e' || c == 'E') {
        let save = sc.i;
        sc.bump();
        if sc.peek().is_some_and(|c| c == '+' || c == '-') {
            sc.bump();
        }
        if sc.peek().is_some_and(|c| c.is_ascii_digit()) {
            while sc.peek().is_some_and(|c| c.is_ascii_digit()) {
                sc.bump();
            }
        } else {
            sc.i = save;
        }
    }
}

fn scan_word(sc: &mut Scanner<'_>) {
    while let Some(c) = sc.peek() {
        if is_word_continue(c) {
            sc.bump();
        } else {
            break;
        }
    }
}

/// Split `sql` into its top-level statements.
///
/// This reuses [`tokenize`] instead of a second lexer, so a `;` inside a string
/// literal, a quoted identifier or a comment never separates statements, and a
/// `;` nested in parentheses (a procedure body) is left alone. Each returned
/// slice is trimmed, keeps its trailing `;`, and starts at the first token, so
/// leading comments are dropped; a text with no top-level `;` yields one slice.
pub(crate) fn split_statements(sql: &str) -> Vec<&str> {
    let tokens = tokenize(sql);
    let mut out = Vec::new();
    let mut depth = 0u32;
    let mut start: Option<usize> = None;
    let mut last_end = 0usize;
    let mut has_content = false;

    for token in &tokens {
        if token.is_punct(';') && depth == 0 {
            push_statement(&mut out, sql, start.take(), token.end, has_content);
            has_content = false;
            continue;
        }
        if start.is_none() {
            start = Some(token.start);
        }
        last_end = token.end;
        has_content = true;
        match token.kind {
            TokenKind::Punct('(') => depth += 1,
            TokenKind::Punct(')') => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    if let Some(start) = start {
        push_statement(&mut out, sql, Some(start), last_end, has_content);
    }
    out
}

fn push_statement<'a>(
    out: &mut Vec<&'a str>,
    sql: &'a str,
    start: Option<usize>,
    end: usize,
    has_content: bool,
) {
    let (Some(start), true) = (start, has_content) else {
        return;
    };
    if end <= start {
        return;
    }
    let text = sql[start..end].trim();
    if !text.is_empty() {
        out.push(text);
    }
}

/// Statement kinds the rules and generators care about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StmtKind {
    Select,
    Insert,
    Update,
    Delete,
    Merge,
    Create,
    Other,
}

/// Keyword tokens that must never be mistaken for a column reference.
pub(crate) const KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "GROUP",
    "ORDER",
    "BY",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "UNION",
    "ALL",
    "DISTINCT",
    "JOIN",
    "LEFT",
    "RIGHT",
    "INNER",
    "OUTER",
    "FULL",
    "CROSS",
    "NATURAL",
    "ON",
    "USING",
    "AS",
    "AND",
    "OR",
    "NOT",
    "IN",
    "IS",
    "NULL",
    "LIKE",
    "BETWEEN",
    "EXISTS",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "VALUES",
    "SET",
    "UPDATE",
    "DELETE",
    "INSERT",
    "INTO",
    "CREATE",
    "TABLE",
    "TEMPORARY",
    "TEMP",
    "WITH",
    "RECURSIVE",
    "ASC",
    "DESC",
    "FETCH",
    "NEXT",
    "FIRST",
    "ROWS",
    "ONLY",
    "FOR",
    "TOP",
    "WINDOW",
    "OVER",
    "PARTITION",
    "RETURNING",
];

pub(crate) fn is_keyword(word: &str) -> bool {
    KEYWORDS.contains(&word.to_ascii_uppercase().as_str())
}

/// A recovered table reference (`schema.table AS alias`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TableRef {
    pub name: String,
    pub alias: Option<String>,
    pub pos: usize,
}

/// A column reference, optionally qualified by a table alias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ColumnRef {
    pub alias: Option<String>,
    pub column: String,
    pub pos: usize,
}

/// Parsed SQL plus the structural facts rules and scores need.
#[derive(Debug, Clone)]
pub(crate) struct Analysis {
    pub sql: String,
    pub tokens: Vec<Token>,
    /// Parenthesis nesting per token.
    pub depth: Vec<u32>,
    pub stmt: StmtKind,
    /// Byte offset where the statement body starts (after leading comments /
    /// CTE preamble for `WITH`, used for `range` on statement-level findings).
    pub body_start: usize,
    /// Index of the main (outermost) `SELECT`, when the statement is a query.
    pub main_select: Option<usize>,
}

impl Analysis {
    pub fn parse(sql: &str) -> Self {
        let tokens = tokenize(sql);
        let mut depth = Vec::with_capacity(tokens.len());
        let mut current: u32 = 0;
        for tok in &tokens {
            if tok.is_punct(')') {
                current = current.saturating_sub(1);
            }
            depth.push(current);
            if tok.is_punct('(') {
                current += 1;
            }
        }

        let mut stmt = StmtKind::Other;
        let mut body_start = 0;
        let mut main_select = None;

        let first = tokens
            .iter()
            .position(|t| t.kind == TokenKind::Word)
            .unwrap_or(0);
        if let Some(tok) = tokens.get(first) {
            body_start = tok.start;
            match tok.upper.as_str() {
                "SELECT" => {
                    stmt = StmtKind::Select;
                    main_select = Some(first);
                }
                "INSERT" => stmt = StmtKind::Insert,
                "UPDATE" => stmt = StmtKind::Update,
                "DELETE" => stmt = StmtKind::Delete,
                "MERGE" => stmt = StmtKind::Merge,
                "CREATE" => stmt = StmtKind::Create,
                "WITH" => {
                    if let Some((idx, kind)) = find_main_after_with(&tokens, &depth) {
                        stmt = kind;
                        body_start = tokens[idx].start;
                        if kind == StmtKind::Select {
                            main_select = Some(idx);
                        }
                    }
                }
                _ => {}
            }
        }

        Self {
            sql: sql.to_string(),
            tokens,
            depth,
            stmt,
            body_start,
            main_select,
        }
    }

    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    pub fn is_top(&self, i: usize) -> bool {
        self.depth.get(i).copied().unwrap_or(1) == 0
    }

    pub fn word_at(&self, i: usize) -> Option<&str> {
        self.tokens.get(i).and_then(|t| match t.kind {
            TokenKind::Word => Some(t.upper.as_str()),
            _ => None,
        })
    }

    pub fn is_word_at(&self, i: usize, word: &str) -> bool {
        self.tokens.get(i).is_some_and(|t| t.is_word(word))
    }

    pub fn is_top_word(&self, i: usize, word: &str) -> bool {
        self.is_top(i) && self.is_word_at(i, word)
    }

    pub fn find_top(&self, word: &str) -> Option<usize> {
        self.find_top_after(word, 0)
    }

    pub fn find_top_after(&self, word: &str, after: usize) -> Option<usize> {
        (after..self.len()).find(|&i| self.is_top_word(i, word))
    }

    /// Index of the first token of a consecutive top-level keyword pair.
    pub fn find_top_seq_after(&self, parts: &[&str], after: usize) -> Option<usize> {
        if parts.is_empty() {
            return None;
        }
        (after..self.len()).find(|&i| {
            if !self.is_top_word(i, parts[0]) {
                return false;
            }
            parts
                .iter()
                .enumerate()
                .all(|(k, p)| i + k < self.len() && self.is_top_word(i + k, p))
        })
    }

    pub fn find_top_seq(&self, parts: &[&str]) -> Option<usize> {
        self.find_top_seq_after(parts, 0)
    }

    /// Content span after a clause keyword ending at `kw_start + kw_len`,
    /// stopping before the first top-level clause keyword at or after it.
    pub fn clause_end(&self, from: usize) -> usize {
        const ENDERS: &[&str] = &[
            "WHERE",
            "GROUP",
            "HAVING",
            "ORDER",
            "LIMIT",
            "OFFSET",
            "UNION",
            "FETCH",
            "FOR",
            "RETURNING",
            "SET",
            "VALUES",
        ];
        (from..self.len())
            .find(|&i| self.is_top(i) && ENDERS.iter().any(|w| self.is_word_at(i, w)))
            .unwrap_or(self.len())
    }

    /// All table references at the statement's top level (`FROM` list and
    /// `JOIN` operands). Subquery operands are skipped as opaque.
    pub fn table_refs(&self) -> Vec<TableRef> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.len() {
            if self.is_top_word(i, "FROM") {
                let (refs, next) = self.read_table_list(i + 1);
                out.extend(refs);
                i = next;
                continue;
            }
            if self.is_top_word(i, "JOIN")
                && let Some((tr, next)) = self.read_table_ref(i + 1)
            {
                out.push(tr);
                i = next;
                continue;
            }
            i += 1;
        }
        dedupe_tables(out)
    }

    fn read_table_list(&self, start: usize) -> (Vec<TableRef>, usize) {
        let mut out = Vec::new();
        let mut i = start;
        loop {
            if i >= self.len() {
                break;
            }
            let (tr, next) = match self.read_table_ref(i) {
                Some(v) => v,
                None => break,
            };
            out.push(tr);
            i = next;
            if i < self.len() && self.is_top(i) && self.tokens[i].is_punct(',') {
                i += 1;
                continue;
            }
            break;
        }
        (out, i)
    }

    fn read_table_ref(&self, start: usize) -> Option<(TableRef, usize)> {
        let mut i = start;
        // `LATERAL` / `ONLY` noise.
        while i < self.len()
            && self
                .word_at(i)
                .is_some_and(|w| matches!(w, "LATERAL" | "ONLY"))
        {
            i += 1;
        }
        if i >= self.len() {
            return None;
        }
        if self.tokens[i].is_punct('(') {
            // Subquery source: skip the balanced group; no table name to record.
            let mut level = 0i32;
            while i < self.len() {
                if self.tokens[i].is_punct('(') {
                    level += 1;
                } else if self.tokens[i].is_punct(')') {
                    level -= 1;
                    if level == 0 {
                        break;
                    }
                }
                i += 1;
            }
            return None;
        }
        let first = self.tokens.get(i)?;
        let mut name = first.ident_name()?.to_string();
        let mut end = i + 1;
        // Qualified name `schema.table`.
        while end + 1 < self.len()
            && self.tokens[end].is_punct('.')
            && self.tokens[end + 1].ident_name().is_some()
        {
            name.push('.');
            name.push_str(self.tokens[end + 1].ident_name().unwrap_or_default());
            end += 2;
        }
        let mut alias = None;
        if end < self.len() {
            if self.is_word_at(end, "AS") {
                if let Some(a) = self.tokens.get(end + 1).and_then(|t| t.ident_name()) {
                    alias = Some(a.to_string());
                    end += 2;
                }
            } else if let Some(a) = self.tokens.get(end).and_then(|t| t.ident_name()) {
                let upper = a.to_ascii_uppercase();
                if !is_keyword(&upper) {
                    alias = Some(a.to_string());
                    end += 1;
                }
            }
        }
        Some((
            TableRef {
                name,
                alias,
                pos: first.start,
            },
            end,
        ))
    }

    /// Read a column reference starting at `i` (`col`, `alias.col`, quoted
    /// forms). Returns `None` for keywords, functions (`name(`) and literals.
    pub fn read_column_ref(&self, i: usize) -> Option<ColumnRef> {
        let tok = self.tokens.get(i)?;
        let name = tok.ident_name()?;
        if tok.kind == TokenKind::Word && is_keyword(name) {
            return None;
        }
        if self.tokens.get(i + 1).is_some_and(|t| t.is_punct('(')) {
            return None;
        }
        // `alias.column`
        if i + 2 < self.len()
            && self.tokens[i + 1].is_punct('.')
            && self.tokens[i + 2].ident_name().is_some()
        {
            let column = self.tokens[i + 2].ident_name()?.to_string();
            return Some(ColumnRef {
                alias: Some(name.to_string()),
                column,
                pos: tok.start,
            });
        }
        // `schema.table.column` → drop the leading schema qualifier.
        if i + 4 < self.len()
            && self.tokens[i + 1].is_punct('.')
            && self.tokens[i + 3].is_punct('.')
            && self.tokens[i + 2].ident_name().is_some()
            && self.tokens[i + 4].ident_name().is_some()
        {
            let column = self.tokens[i + 4].ident_name()?.to_string();
            return Some(ColumnRef {
                alias: Some(self.tokens[i + 2].ident_name()?.to_string()),
                column,
                pos: tok.start,
            });
        }
        Some(ColumnRef {
            alias: None,
            column: name.to_string(),
            pos: tok.start,
        })
    }

    /// True when a value position (right side of a predicate) is a literal or
    /// placeholder, i.e. not another column.
    pub fn is_value_at(&self, i: usize) -> bool {
        match self.tokens.get(i).map(|t| t.kind) {
            Some(TokenKind::Str | TokenKind::Number | TokenKind::Param) => true,
            Some(TokenKind::Word) => {
                // `NULL`, `TRUE`, `FALSE` are values; anything else could be a column.
                self.word_at(i)
                    .is_some_and(|w| matches!(w, "NULL" | "TRUE" | "FALSE"))
            }
            _ => false,
        }
    }

    /// Is there a top-level `WITH` preamble?
    pub fn has_cte(&self) -> bool {
        self.tokens.first().is_some_and(|t| t.is_word("WITH"))
    }

    /// Number of CTEs in a `WITH` preamble (top-level commas + 1).
    pub fn cte_count(&self) -> usize {
        if !self.has_cte() {
            return 0;
        }
        let body = self.body_start;
        let mut count = 0usize;
        let mut in_cte = false;
        for i in 0..self.len() {
            if !self.is_top(i) {
                continue;
            }
            if self.tokens[i].start >= body {
                break;
            }
            if self.is_word_at(i, "AS") {
                in_cte = true;
            } else if in_cte && self.tokens[i].is_punct(',') {
                count += 1;
                in_cte = false;
            }
        }
        count + 1
    }

    /// Max parenthesis depth at which a `SELECT` appears. The outermost query
    /// is `0`; one nested subquery is `1`, and so on.
    pub fn subquery_depth(&self) -> u32 {
        (0..self.len())
            .filter(|&i| self.word_at(i) == Some("SELECT"))
            .map(|i| self.depth[i])
            .max()
            .unwrap_or(0)
    }

    /// Byte span covering the statement body (from the first keyword to the
    /// last non-`;` token).
    pub fn stmt_range(&self) -> (u32, u32) {
        let start = self.body_start as u32;
        let end = self
            .tokens
            .iter()
            .rev()
            .find(|t| !t.is_punct(';'))
            .map(|t| t.end as u32)
            .unwrap_or(start);
        (start, end)
    }
}

fn find_main_after_with(tokens: &[Token], depth: &[u32]) -> Option<(usize, StmtKind)> {
    // Skip the CTE definitions: the main statement is the first
    // SELECT/INSERT/UPDATE/DELETE at depth 0 that is not preceded by `AS (`.
    for (i, tok) in tokens.iter().enumerate() {
        if depth[i] != 0 || tok.kind != TokenKind::Word {
            continue;
        }
        let kind = match tok.upper.as_str() {
            "SELECT" => StmtKind::Select,
            "INSERT" => StmtKind::Insert,
            "UPDATE" => StmtKind::Update,
            "DELETE" => StmtKind::Delete,
            "MERGE" => StmtKind::Merge,
            _ => continue,
        };
        // A CTE body starts with SELECT but sits inside parentheses (depth > 0),
        // so a depth-0 hit is the main statement.
        return Some((i, kind));
    }
    None
}

fn dedupe_tables(refs: Vec<TableRef>) -> Vec<TableRef> {
    let mut out: Vec<TableRef> = Vec::new();
    for tr in refs {
        if !out.iter().any(|t| {
            t.name.eq_ignore_ascii_case(&tr.name)
                && match (&t.alias, &tr.alias) {
                    (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
                    (None, None) => true,
                    _ => false,
                }
        }) {
            out.push(tr);
        }
    }
    out
}

/// Predicate kinds that matter for index advice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PredKind {
    Eq,
    Range,
    In,
    PrefixLike,
    LeadingLike,
    Between,
    OrderBy,
}

/// A predicate column found in `WHERE` or `ORDER BY`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Predicate {
    pub table: Option<String>,
    pub column: String,
    pub kind: PredKind,
    pub pos: usize,
}

/// Resolve the table a column reference belongs to using the statement's
/// table aliases. Unqualified columns resolve to the single table when there is
/// exactly one; otherwise they stay unresolved.
pub(crate) fn resolve_table(refs: &[TableRef], col: &ColumnRef) -> Option<String> {
    if let Some(alias) = &col.alias {
        for tr in refs {
            if tr
                .alias
                .as_deref()
                .is_some_and(|a| a.eq_ignore_ascii_case(alias))
                || tr.name.eq_ignore_ascii_case(alias)
                || tr
                    .name
                    .rsplit('.')
                    .next()
                    .is_some_and(|n| n.eq_ignore_ascii_case(alias))
            {
                return Some(tr.name.clone());
            }
        }
        return None;
    }
    if refs.len() == 1 {
        return Some(refs[0].name.clone());
    }
    let mut candidates = refs
        .iter()
        .filter(|tr| column_belongs(&tr.name, &col.column));
    if let Some(first) = candidates.next()
        && candidates.next().is_none()
    {
        return Some(first.name.clone());
    }
    None
}

/// Without schema metadata, an unqualified column is ambiguous; this is only a
/// placeholder hook kept explicit (always false) so the code never pretends to
/// know a column's table.
fn column_belongs(_table: &str, _column: &str) -> bool {
    false
}

/// Collect index-relevant predicates from the top-level `WHERE`, `ORDER BY`
/// and `GROUP BY` clauses.
pub(crate) fn predicates(sa: &Analysis) -> Vec<Predicate> {
    let refs = sa.table_refs();
    let mut out = Vec::new();

    if let Some(where_idx) = sa.find_top("WHERE") {
        let end = sa.clause_end(where_idx + 1);
        let mut i = where_idx + 1;
        while i < end {
            let Some(col) = sa.read_column_ref(i) else {
                i += 1;
                continue;
            };
            let after = ref_end(sa, i);
            let Some(op_tok) = sa.tokens.get(after) else {
                break;
            };
            let table = resolve_table(&refs, &col);
            let mut push = |kind: PredKind, at: usize| {
                out.push(Predicate {
                    table: table.clone(),
                    column: col.column.clone(),
                    kind,
                    pos: at,
                });
            };
            if op_tok.is_word("BETWEEN") {
                push(PredKind::Between, col.pos);
            } else if op_tok.is_word("IN") {
                push(PredKind::In, col.pos);
            } else if op_tok.is_word("LIKE") {
                let start = match sa.tokens.get(after + 1).and_then(|t| t.string_value()) {
                    Some(v) => {
                        if v.starts_with('%') {
                            PredKind::LeadingLike
                        } else {
                            PredKind::PrefixLike
                        }
                    }
                    None => PredKind::PrefixLike,
                };
                push(start, col.pos);
            } else if matches!(op_tok.kind, TokenKind::Op) {
                let is_eq = matches!(op_tok.text.as_str(), "=" | "==");
                // Only literal/placeholder comparisons are index candidates.
                let right_is_value = sa.is_value_at(after + 1);
                if right_is_value {
                    push(if is_eq { PredKind::Eq } else { PredKind::Range }, col.pos);
                }
            }
            i = after;
        }
    }

    if let Some(order_idx) = sa.find_top_seq(&["ORDER", "BY"]) {
        let end = sa.clause_end(order_idx + 2);
        let mut i = order_idx + 2;
        while i < end {
            if let Some(col) = sa.read_column_ref(i) {
                let table = resolve_table(&refs, &col);
                out.push(Predicate {
                    table,
                    column: col.column.clone(),
                    kind: PredKind::OrderBy,
                    pos: col.pos,
                });
                i += 1;
                continue;
            }
            i += 1;
        }
    }

    out
}

/// End token index of a column reference started at `i`.
fn ref_end(sa: &Analysis, i: usize) -> usize {
    if i + 2 < sa.len() && sa.tokens[i + 1].is_punct('.') && sa.tokens[i + 2].ident_name().is_some()
    {
        i + 3
    } else {
        i + 1
    }
}
