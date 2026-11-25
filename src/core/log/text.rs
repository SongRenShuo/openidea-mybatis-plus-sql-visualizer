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
