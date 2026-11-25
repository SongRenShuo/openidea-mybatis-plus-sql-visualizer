//! Where a log row came from, read off the row itself.
//!
//! The platform gives us no PSI, so the only place a mapper class/method can be
//! recovered from is the console prefix. MyBatis logs every statement under the
//! *statement id* as logger name (`<mapper FQN>.<method>`), which is why a
//! Spring Boot row looks like
//!
//! ```text
//! 2026-11-25 21:00:01.001 DEBUG 4242 --- [nio-8080-exec-1] c.e.m.mapper.UserMapper.selectById : ==>  Preparing: SELECT …
//! ```
//!
//! Three facts come out of that prefix and nowhere else:
//!
//! | field | source | example |
//! | --- | --- | --- |
//! | [`SourceRef`] | the logger name | `c.e.m.mapper.UserMapper` / `selectById` |
//! | `thread` | the logback `--- [name]` bracket | `nio-8080-exec-1` |
//! | `request_id` | an MDC pair in the prefix | `traceId=4bf92f35…` |
//!
//! Nothing here guesses. No marker, no prefix, no logger name → empty fields.
//! The class name is kept **as written**: Spring shortens leading packages
//! (`com.example.myapp` → `c.e.m`), so it is not necessarily a loadable FQN;
//! [`crate::indexer`] matches it segment-wise instead.
//!
//! ## Which families get a logger source
//!
//! Only MyBatis needs a `LOGGER : ==>  Preparing:` marker to produce
//! [`SourceRef`]: Hibernate (`org.hibernate.SQL`), JdbcTemplate
//! (`…JdbcTemplate`) and p6spy (`p6spy`) print a logger name too, but it is a
//! logger, not a statement id, and its last segment is a class — so the
//! logger-derived fields stay empty for them, exactly as the reference
//! implementation's PSI path had no source for them either.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::model::LogAnalysis;
use super::text::{find_ci, thread_token};

/// Marker keywords that start a MyBatis message body. Their position is the
/// boundary between "log prefix" (logger, thread, MDC) and "SQL" — text after
/// it can hold `request_id = 'x'` in a WHERE clause and must not be read as a
/// correlation id.
const FAMILY_MARKERS: [&str; 5] = [
    "preparing:",
    "parameters:",
    "executing:",
    "total:",
    "updates:",
];

/// Message arrows of the MyBatis console (`==> Preparing:` / `<== Total:`).
const ARROWS: [&str; 2] = ["==>", "<=="];

/// Correlation-id keys, lower-case (`x-request-id` keeps its hyphens).
const REQUEST_KEYS: [&str; 6] = [
    "traceid",
    "trace_id",
    "requestid",
    "request_id",
    "x-request-id",
    "x-requestid",
];

/// A quoted or unquoted correlation id longer than this is a hex dump, not an
/// id; the tail is dropped rather than stored.
const MAX_ID_LEN: usize = 128;

fn ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The logger-derived class and method of one statement.
///
/// `class_name` is the class part of the logger name **as written**, so
/// `c.e.m.mapper.UserMapper` keeps its abbreviations. `class_simple` is its
/// last segment (`UserMapper`), which is what a Java file declares.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceRef {
    /// Logger name without its last segment, verbatim.
    pub class_name: String,
    /// Last segment of [`Self::class_name`].
    pub class_simple: String,
    /// Logger name's last segment (`selectById`).
    pub method: String,
}

impl SourceRef {
    /// `class_name#method` — the way [`crate::indexer`] keys Java methods.
    pub fn key(&self) -> String {
        format!("{}#{}", self.class_name, self.method)
    }
}

/// Provenance of one log line. Every field is optional: a row that states
/// nothing yields `None`, never a guess.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceRef>,
    /// Console thread, e.g. `nio-8080-exec-1`. A thread is not a request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    /// Correlation id printed next to the row (`traceId=`, `requestId:`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

impl LineSource {
    pub fn is_empty(&self) -> bool {
        self.source.is_none() && self.thread.is_none() && self.request_id.is_none()
    }
}

/// Per-entry provenance, addressable by the entry's `line_no`.
///
/// Rows that stated nothing are absent rather than present-and-empty, so a
/// 100k-line log costs memory only for the statements that carry a source.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogSources {
    /// `(line_no, provenance)`, ascending by `line_no`.
    items: Vec<(usize, LineSource)>,
}

impl LogSources {
    /// Provenance of the entry at `line_no`, when that row stated any.
    pub fn get(&self, line_no: usize) -> Option<&LineSource> {
        self.items
            .binary_search_by_key(&line_no, |(line, _)| *line)
            .ok()
            .map(|index| &self.items[index].1)
    }

    /// Logger-derived class/method of the entry at `line_no`.
    pub fn source(&self, line_no: usize) -> Option<&SourceRef> {
        self.get(line_no)?.source.as_ref()
    }

    /// `(line_no, provenance)` pairs in ascending order.
    pub fn iter(&self) -> impl Iterator<Item = (usize, &LineSource)> {
        self.items.iter().map(|(line, source)| (*line, source))
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// A parsed log plus the provenance of each entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedLog {
    pub analysis: LogAnalysis,
    pub sources: LogSources,
}

/// Byte offset where a MyBatis message body starts, when the row has one.
fn message_start(line: &str) -> Option<usize> {
    let mut best: Option<usize> = None;
    let mut consider = |at: usize| {
        best = Some(best.map_or(at, |current: usize| current.min(at)));
    };
    for marker in FAMILY_MARKERS {
        if let Some(at) = find_ci(line, marker) {
            consider(at);
        }
    }
    for arrow in ARROWS {
        if let Some(at) = find_ci(line, arrow) {
            consider(at);
        }
    }
    best
}

/// `prefix` is everything before the message body. The logger sits at its end,
/// separated by ` : ` (logback), ` - ` or nothing.
fn logger_of(prefix: &str) -> Option<SourceRef> {
    let mut head = prefix;
    // Drop the arrow (`==> Preparing:`), then the separator after the logger.
    for arrow in ARROWS {
        if let Some(at) = head.rfind(arrow) {
            head = &head[..at];
        }
    }
    let head = head.trim_end();
    let head = head.trim_end_matches(':').trim_end();
    let head = head.trim_end_matches('-').trim_end();
    let token = head.rsplit(|c: char| c.is_whitespace()).next()?;

    let segments: Vec<&str> = token.split('.').collect();
    if segments.len() < 2 {
        return None;
    }
    if segments
        .iter()
        .any(|segment| segment.is_empty() || !segment.chars().all(ident_char))
    {
        return None;
    }
    let method = segments[segments.len() - 1];
    // A method name starts lower-case; a logger whose last segment is a class
    // (`org.hibernate.SQL`, `…JdbcTemplate`, `p6spy`) has no method part and
    // must yield nothing.
    let first = method.chars().next()?;
    if first == '_' || !first.is_lowercase() {
        return None;
    }
    Some(SourceRef {
        class_name: segments[..segments.len() - 1].join("."),
        class_simple: segments[segments.len() - 2].to_owned(),
        method: method.to_owned(),
    })
}

fn is_thread_name(candidate: &str) -> bool {
    let trimmed = candidate.trim();
    !trimmed.is_empty()
        && !trimmed.chars().all(|c| c.is_ascii_digit())
        && !trimmed
            .get(..4)
            .is_some_and(|head| head.eq_ignore_ascii_case("tid:"))
}

/// The console thread. The logback prefix (`… --- [thread] logger : …`) states
/// it unambiguously, so that bracket wins; formats without `---` fall back to
/// the MyBatis-keyword-bounded scan in [`thread_token`].
fn thread_of(line: &str) -> Option<String> {
    if let Some(marker) = find_ci(line, "---") {
        let rest = line[marker + 3..].trim_start();
        if let Some(bracket) = rest.strip_prefix('[')
            && let Some(close) = bracket.find(']')
        {
            let name = &bracket[..close];
            if is_thread_name(name) {
                return Some(name.trim().to_owned());
            }
        }
    }
    thread_token(line)
}

fn is_key_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')
}

fn is_value_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':')
}

/// First correlation id in `text`.
///
/// Only the log prefix is scanned (see [`source_of_line`]): inside a message
/// body `request_id = 'x'` is a SQL predicate, not a correlation id, and a
/// wrong id is worse than no id.
fn request_id_of(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !is_key_char(bytes[i]) {
            i += 1;
            continue;
        }
        let key_start = i;
        while i < bytes.len() && is_key_char(bytes[i]) {
            i += 1;
        }
        let key = text[key_start..i].to_ascii_lowercase();
        if !REQUEST_KEYS.contains(&key.as_str()) {
            continue;
        }
        let mut at = i;
        while at < bytes.len() && bytes[at] == b' ' {
            at += 1;
        }
        if !matches!(bytes.get(at), Some(b'=') | Some(b':')) {
            continue;
        }
        at += 1;
        while at < bytes.len() && bytes[at] == b' ' {
            at += 1;
        }
        if at >= bytes.len() {
            continue;
        }
        if matches!(bytes[at], b'"' | b'\'') {
            let quote = bytes[at];
            let start = at + 1;
            let Some(len) = bytes[start..].iter().position(|&b| b == quote) else {
                continue;
            };
            let value = &text[start..(start + len).min(start + MAX_ID_LEN)];
            if !value.is_empty() {
                return Some(value.to_owned());
            }
            continue;
        }
        let start = at;
        while at < bytes.len() && is_value_char(bytes[at]) {
            at += 1;
        }
        let value = &text[start..at.min(start + MAX_ID_LEN)];
        // `?` / empty means a placeholder or a predicate, not an id.
        if value.is_empty() || value == "?" {
            continue;
        }
        if !value
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        {
            continue;
        }
        return Some(value.to_owned());
    }
    None
}

/// Reads one log line's provenance.
///
/// The logger source is produced only for a MyBatis row (one of
/// [`FAMILY_MARKERS`] or an `==>`/`<==` arrow present), because that marker is
/// what makes the logger name a statement id. Thread and correlation id are
/// read whenever the row states them, which keeps them available to the other
/// three families too.
pub fn source_of_line(line: &str) -> LineSource {
    let start = message_start(line);
    let prefix = match start {
        Some(at) => &line[..at],
        None => line,
    };
    LineSource {
        source: start.and_then(|_| logger_of(prefix)),
        thread: thread_of(line),
        request_id: request_id_of(prefix),
    }
}

/// Provenance of every analysis entry, joined by `LogEntry::line_no`.
///
/// One pass over `text`, so a 100k-line log costs the same as the parse itself;
/// rows that stated nothing are not stored.
pub fn sources_for(analysis: &LogAnalysis, text: &str) -> LogSources {
    let mut wanted: Vec<usize> = analysis.entries.iter().map(|entry| entry.line_no).collect();
    wanted.sort_unstable();
    wanted.dedup();

    let mut items = Vec::new();
    let mut cursor = 0usize;
    for (index, line) in text.lines().enumerate() {
        let line_no = index + 1;
        while cursor < wanted.len() && wanted[cursor] < line_no {
            cursor += 1;
        }
        if cursor >= wanted.len() {
            break;
        }
        if wanted[cursor] != line_no {
            continue;
        }
        let source = source_of_line(line);
        if source.is_empty() {
            continue;
        }
        items.push((line_no, source));
    }
    LogSources { items }
}

/// Line number → provenance lookup for callers that already hold the analysis.
///
/// Kept for symmetry with [`sources_for`]: same join rule, no second pass over
/// the text (the caller supplies `(line_no, line)` pairs it already has).
pub fn by_line(pairs: impl IntoIterator<Item = (usize, LineSource)>) -> LogSources {
    let mut map: HashMap<usize, LineSource> = HashMap::new();
    for (line_no, source) in pairs {
        if source.is_empty() {
            continue;
        }
        map.entry(line_no).or_insert(source);
    }
    let mut items: Vec<(usize, LineSource)> = map.into_iter().collect();
    items.sort_unstable_by_key(|(line, _)| *line);
    LogSources { items }
}
