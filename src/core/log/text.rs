//! Small hand-written text helpers shared by the four family parsers.
//!
//! The crate deliberately has no `regex` dependency (see `AGENTS.md`: prefer
//! hand-written scanners in the wasm module), so every matcher here is a
//! linear scan over ASCII bytes.

use super::model::EntryClass;

/// Case-insensitive substring search. `needle` must be ASCII lower-case.
pub(crate) fn find_ci(haystack: &str, needle_lower: &str) -> Option<usize> {
    if needle_lower.is_empty() {
        return None;
    }
    let h = haystack.as_bytes();
    let n = needle_lower.as_bytes();
    if n.len() > h.len() {
        return None;
    }
    for i in 0..=(h.len() - n.len()) {
        let mut ok = true;
        for (k, expected) in n.iter().enumerate() {
            if !h[i + k].eq_ignore_ascii_case(expected) {
                ok = false;
                break;
            }
        }
        if ok {
            return Some(i);
        }
    }
    None
}

/// Byte index just after `needle`, or `None`.
pub(crate) fn after_ci(haystack: &str, needle_lower: &str) -> Option<usize> {
    find_ci(haystack, needle_lower).map(|i| i + needle_lower.len())
}

/// Byte index just after a configurable prefix.
///
/// `mixed` mirrors the "lenient prefix" switch: true searches the whole line
/// (`==> Preparing:`), false requires the trimmed line to start with it.
pub(crate) fn after_prefix(haystack: &str, needle_lower: &str, mixed: bool) -> Option<usize> {
    if needle_lower.is_empty() {
        return None;
    }
    if mixed {
        return after_ci(haystack, needle_lower);
    }
    let trimmed = haystack.trim_start();
    let start = haystack.len() - trimmed.len();
    let head = trimmed.as_bytes().get(..needle_lower.len())?;
    head.eq_ignore_ascii_case(needle_lower.as_bytes())
        .then_some(start + needle_lower.len())
}

/// Console thread token, used to keep a `<== Total:` / `<== Updates:` row with
/// the statement that produced it.
///
/// Spring Boot / logback rows carry the thread in brackets
/// (`2026-11-25 21:00:01.001 DEBUG 4242 --- [nio-8080-exec-1] … : ==>  Preparing: …`).
/// The token is only looked for **before** the family keyword that starts the
/// message body (`preparing:`, `parameters:`, `total:`, …), so a bracketed
/// fragment inside SQL text (`a[1]`) can never pass as a thread name. `TID:`
/// correlation ids and bare numbers are skipped and the bracket closest to the
/// message wins, mirroring the reference `LogThreadRouter.threadTokenOf`.
///
/// `None` means "not extractable"; callers then fall back to the single-slot
/// behaviour, which keeps prefix-less console formats working.
pub(crate) fn thread_token(line: &str) -> Option<String> {
    let boundary = [
        "preparing:",
        "parameters:",
        "executing:",
        "total:",
        "updates:",
    ]
    .iter()
    .filter_map(|marker| find_ci(line, marker))
    .min()?;
    // Every byte `find_ci` matches is ASCII, so `boundary` is a char boundary.
    let prefix = &line[..boundary];
    let mut token: Option<String> = None;
    let mut from = 0;
    while let Some(open) = prefix[from..].find('[') {
        let start = from + open + 1;
        let Some(close) = prefix[start..].find(']') else {
            break;
        };
        let candidate = &prefix[start..start + close];
        if is_thread_name(candidate) {
            token = Some(candidate.to_string());
        }
        from = start + close + 1;
    }
    token
}

fn is_thread_name(candidate: &str) -> bool {
    let trimmed = candidate.trim();
    !trimmed.is_empty()
        && !trimmed.chars().all(|c| c.is_ascii_digit())
        && !trimmed
            .get(..4)
            .is_some_and(|head| head.eq_ignore_ascii_case("tid:"))
}

/// First whitespace-delimited token, upper-cased for classification.
pub(crate) fn first_token_upper(sql: &str) -> String {
    let trimmed = sql.trim_start();
    let end = trimmed
        .find(|c: char| c.is_whitespace() || c == '(')
        .unwrap_or(trimmed.len());
    trimmed[..end].to_ascii_uppercase()
}

/// Classify a statement by its first token (reference `LogStatementClassifier`).
pub(crate) fn classify(sql: &str) -> EntryClass {
    match first_token_upper(sql).as_str() {
        "SELECT" | "WITH" => EntryClass::Select,
        "INSERT" => EntryClass::Insert,
        "UPDATE" => EntryClass::Update,
        "DELETE" => EntryClass::Delete,
        "CREATE" | "ALTER" | "RENAME" | "DROP" | "TRUNCATE" => EntryClass::Ddl,
        _ => EntryClass::Other,
    }
}

/// Does `text` start with a SQL statement keyword?
pub(crate) fn starts_with_sql_keyword(text: &str) -> bool {
    let t = text.trim_start();
    let upper = first_token_upper(t);
    matches!(
        upper.as_str(),
        "SELECT" | "INSERT" | "UPDATE" | "DELETE" | "MERGE" | "WITH" | "CALL" | "REPLACE"
    )
}

/// Lines that belong to a Java stack trace / error block and must never be
/// treated as SQL continuation (reference `SqlErrorLineRecognizer.isContinuationLine`).
pub(crate) fn is_stack_continuation(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("at ")
        || t.starts_with("Caused by:")
        || t.starts_with("... ")
        || t.starts_with("Suppressed:")
        || t.starts_with("###")
}

/// Spring Boot / Logback style prefix: `2026-08-22 00:34:11.251 DEBUG 37792 ---`.
///
/// Used to stop multi-line SQL collection from swallowing a business log row.
pub(crate) fn is_log_framework_line(line: &str) -> bool {
    let t = line.trim_start();
    let b = t.as_bytes();
    if b.len() < 24 {
        return false;
    }
    let date_ok = b.len() > 10
        && b[0..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
        && (b[10] == b' ' || b[10] == b'T');
    if !date_ok {
        return false;
    }
    // A level keyword must appear in the first 64 bytes (slice at a char
    // boundary; `get` returns `None` when byte 64 splits a multi-byte char).
    let head = t.get(..64).unwrap_or(t);
    ["DEBUG", "TRACE", "INFO", "WARN", "ERROR"]
        .iter()
        .any(|level| head.contains(level))
}

// ---------------------------------------------------------------------------
// Hibernate logback rows (`org.hibernate.SQL : select …`, `… jdbc.bind : …`)
// ---------------------------------------------------------------------------

/// Level keywords of logback / Spring Boot patterns. A row that carries one in
/// its prefix came out of a logging framework, not a bare console writer.
const LEVEL_TOKENS: [&str; 6] = ["DEBUG", "TRACE", "INFO", "WARN", "ERROR", "FATAL"];

/// Last segment of the Hibernate loggers that print the statement itself.
/// Logback shortens leading packages to fit `%logger{36}`
/// (`org.hibernate.engine.jdbc.spi.SqlStatementLogger` → `o.h.e.jdbc.spi.…`),
/// so only the tail is stable.
const HIBERNATE_SQL_LOGGER_TAILS: [&str; 2] = ["SQL", "SqlStatementLogger"];

/// `org.hibernate.orm.jdbc.bind` — one row per bound parameter.
const HIBERNATE_BIND_LOGGER_TAIL: &str = "bind";

/// SQL keywords that may open a Hibernate message body.
const SQL_MESSAGE_KEYWORDS: [&str; 8] = [
    "select", "insert", "update", "delete", "merge", "with", "call", "replace",
];

/// Which message body a Hibernate logger row carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HibernateLoggerKind {
    /// `org.hibernate.SQL` / `… SqlStatementLogger`: the body is SQL.
    Statement,
    /// `org.hibernate.orm.jdbc.bind`: the body is one `binding parameter` row.
    Parameter,
}

/// A `logger : message` row written by a Hibernate logger under logback.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HibernateLoggerRow {
    pub kind: HibernateLoggerKind,
    /// Byte offset where the message body starts (after `logger : `).
    pub message_start: usize,
}

/// Recognise a logback-prefixed Hibernate row.
///
/// ```text
/// 2026-11-26 10:00:01.123 DEBUG 4242 --- [nio-8080-exec-1] org.hibernate.SQL : select …
/// 2026-11-26 10:00:01.124 TRACE 4242 --- [nio-8080-exec-1] org.hibernate.orm.jdbc.bind : binding parameter [1] as [BIGINT] - [42]
/// ```
///
/// The row is accepted only when all three hold: a level token in the prefix
/// (so a business line that merely contains `x.y : select …` is not mistaken
/// for SQL), a dotted logger token right before the message, and that token's
/// last segment being one of the Hibernate loggers above.
pub(crate) fn hibernate_logger_row(line: &str) -> Option<HibernateLoggerRow> {
    let statement = message_body_start(line).filter(|&at| is_sql_logger_row(line, at));
    let parameter = find_ci(line, "binding parameter").filter(|&at| is_bind_logger_row(line, at));
    match (statement, parameter) {
        (Some(statement), Some(parameter)) if parameter < statement => Some(HibernateLoggerRow {
            kind: HibernateLoggerKind::Parameter,
            message_start: parameter,
        }),
        (Some(statement), _) => Some(HibernateLoggerRow {
            kind: HibernateLoggerKind::Statement,
            message_start: statement,
        }),
        (None, Some(parameter)) => Some(HibernateLoggerRow {
            kind: HibernateLoggerKind::Parameter,
            message_start: parameter,
        }),
        (None, None) => None,
    }
}

fn is_sql_logger_row(line: &str, at: usize) -> bool {
    is_framework_prefixed(&line[..at])
        && logger_tail_before(line, at)
            .is_some_and(|tail| HIBERNATE_SQL_LOGGER_TAILS.contains(&tail))
}

fn is_bind_logger_row(line: &str, at: usize) -> bool {
    is_framework_prefixed(&line[..at])
        && logger_tail_before(line, at) == Some(HIBERNATE_BIND_LOGGER_TAIL)
}

fn is_framework_prefixed(prefix: &str) -> bool {
    prefix
        .split(|c: char| c.is_whitespace() || c == '[' || c == ']' || c == ':')
        .any(|token| {
            LEVEL_TOKENS
                .iter()
                .any(|level| token.eq_ignore_ascii_case(level))
        })
}

/// Last segment of the logger token immediately before `at`, when that text is
/// `<logger> : ` or `<logger> - ` (logback default / Spring Boot pattern).
fn logger_tail_before(line: &str, at: usize) -> Option<&str> {
    let head = line.get(..at)?.trim_end();
    let head = head
        .strip_suffix(':')
        .or_else(|| head.strip_suffix('-'))?
        .trim_end();
    let token = head.rsplit(char::is_whitespace).next()?;
    if !token.contains('.')
        || !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'$'))
    {
        return None;
    }
    token.rsplit('.').next()
}

/// Earliest position of a SQL message body: a statement keyword, or the `/*`
/// of a `/* load com.example.User */` comment.
fn message_body_start(line: &str) -> Option<usize> {
    let mut best: Option<usize> = None;
    let mut consider = |at: usize| {
        if best.is_none_or(|current| at < current) {
            best = Some(at);
        }
    };
    let mut from = 0;
    while let Some(rel) = line[from..].find("/*") {
        let at = from + rel;
        if at == 0 || line.as_bytes()[at - 1].is_ascii_whitespace() {
            consider(at);
        }
        from = at + 2;
    }
    for keyword in SQL_MESSAGE_KEYWORDS {
        let mut from = 0;
        while let Some(rel) = find_ci(&line[from..], keyword) {
            let at = from + rel;
            if sql_word_boundary(line, at, keyword.len()) {
                consider(at);
            }
            from = at + keyword.len();
        }
    }
    best
}

/// `at..at + len` is a whole word: whitespace or `(` before it, and whitespace,
/// `(` `)` `,` or end of line after it.
fn sql_word_boundary(line: &str, at: usize, len: usize) -> bool {
    let b = line.as_bytes();
    if at > 0 {
        let prev = b[at - 1];
        if !(prev.is_ascii_whitespace() || prev == b'(') {
            return false;
        }
    }
    match b.get(at + len) {
        None => true,
        Some(next) => next.is_ascii_whitespace() || matches!(*next, b'(' | b')' | b',' | b';'),
    }
}

/// SQL error vocabulary, ported from the reference `SqlErrorLineRecognizer`.
///
/// Deliberately signature-based (class names and `###` blocks), not a generic
/// `error` keyword: business log rows saying "error" are not SQL failures.
const ERROR_SIGNATURES: &[&str] = &[
    "### error",
    "### cause",
    "### the error may",
    "### sql:",
    "org.apache.ibatis.exceptions",
    "mybatissystemexception",
    "bindingexception:",
    "typeexception:",
    "dataaccessexception:",
    "badsqlgrammarexception",
    "dataintegrityviolationexception",
    "duplicatekeyexception",
    "typemismatchdataaccessexception",
    "uncategorizedsqlexception",
    "jdbcconnectionexception:",
    "deadlockloserexception",
    "cannotcreatetransactionexception:",
    "sqlexception:",
    "sqlsyntaxerrorexception:",
    "sqlintegrityconstraintviolationexception:",
    "sqlrecoverableexception:",
    "sqltransientconnectionexception:",
    "sqltimeoutexception:",
    "communicationsexception:",
    "org.hibernate.exception",
    "persistenceexception:",
    "hibernateexception:",
    "constraintviolationexception:",
    "parameter index out of range",
    "could not set parameters",
    "error setting null for parameter",
    "invalid argument value",
];

/// Cheap pre-check plus lower-case scan over the signature list.
pub(crate) fn is_error_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    ERROR_SIGNATURES.iter().any(|sig| lower.contains(sig))
}

/// Collapse whitespace runs to a single space and trim (reference
/// `normalizeSqlWhitespace`).
pub(crate) fn normalize_ws(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut pending_space = false;
    for ch in sql.trim().chars() {
        if ch.is_whitespace() {
            pending_space = true;
        } else {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.push(ch);
        }
    }
    out
}

/// Fold string literals and bare numbers to `?` (reference
/// `structureFingerprint` / `parameterizeLiterals`). Identifier digits such as
/// `t2024` stay intact, and `IN (?,?)` keeps its element count.
pub(crate) fn parameterize_literals(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let bytes = sql.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\'' {
            let mut j = i + 1;
            while j < bytes.len() {
                if bytes[j] == b'\'' {
                    if j + 1 < bytes.len() && bytes[j + 1] == b'\'' {
                        j += 2;
                        continue;
                    }
                    break;
                }
                j += 1;
            }
            out.push('?');
            i = if j < bytes.len() { j + 1 } else { bytes.len() };
        } else if b.is_ascii_digit() && (i == 0 || !is_sql_word_char(bytes[i - 1])) {
            let mut j = i;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b'.') {
                j += 1;
            }
            if j < bytes.len() && is_sql_word_char(bytes[j]) {
                // `2024Q1` / `t2024` shape: the run is part of an identifier.
                out.push_str(&sql[i..j]);
            } else {
                out.push('?');
            }
            i = j;
        } else {
            // Copy one whole UTF-8 scalar (never split a multi-byte char).
            let ch_len = utf8_len(b);
            out.push_str(&sql[i..(i + ch_len).min(bytes.len())]);
            i += ch_len;
        }
    }
    out
}

fn is_sql_word_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

/// Truncate at a UTF-8 char boundary, returning `(text, truncated)`.
pub(crate) fn truncate_chars(s: &str, max: usize) -> (String, bool) {
    if s.chars().count() <= max {
        return (s.to_string(), false);
    }
    let end = s.char_indices().nth(max).map(|(i, _)| i).unwrap_or(s.len());
    (s[..end].to_string(), true)
}

/// Full numeric literal check (decimals and scientific notation).
pub(crate) fn is_numeric(value: &str) -> bool {
    let b = value.as_bytes();
    if b.is_empty() {
        return false;
    }
    let mut i = 0;
    if b[0] == b'-' || b[0] == b'+' {
        i += 1;
        if i == b.len() {
            return false;
        }
    }
    let mut digits = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
        digits += 1;
    }
    if digits == 0 {
        return false;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let mut frac = 0;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            frac += 1;
        }
        if frac == 0 {
            return false;
        }
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let mut exp = 0;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            exp += 1;
        }
        if exp == 0 {
            return false;
        }
    }
    i == b.len()
}

/// Single-quote SQL string escaping.
pub(crate) fn quote_sql(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push('\'');
        }
        out.push(ch);
    }
    out.push('\'');
    out
}

/// Build an unparsed record with a stable diagnostic key; long lines are
/// capped so one runaway log line cannot bloat the serialised payload.
pub(crate) fn unparsed_line(line_no: usize, text: &str, key: &str) -> super::model::UnparsedLine {
    let (text, _) = truncate_chars(text, 2000);
    super::model::UnparsedLine {
        line_no,
        text,
        message_key: key.to_string(),
    }
}
