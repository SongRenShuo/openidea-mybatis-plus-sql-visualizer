//! Spring JdbcTemplate console family (reference `JdbcTemplateLogParser`).
//!
//! Supported shapes:
//! - `Executing prepared SQL statement [SELECT ...]`
//! - `Executing SQL query [SELECT ...]`, `Executing SQL update [...]`,
//!   `Executing SQL batch update [...]`, `Calling stored procedure [{call ...}]`
//! - multi-line `[...]` bodies (string-aware bracket counting)
//! - `Setting SQL statement parameter value: column index 1, parameter value
//!   [0], value class [java.lang.Integer], SQL type unknown`
//! - `sql = SELECT ... , parameters = {a=1, b=x}` (NamedParameterJdbcTemplate)

use super::model::{Confidence, Family, message_keys};
use super::params::{Param, parse_jdbc_setting, parse_named_map, render_param, render_untyped};
use super::parser::{Emitter, PendingEntry, cap_sql};
use super::restore::restore;
use super::text::{classify, find_ci};

const ACTION_PHRASES: &[&str] = &[
    "executing prepared sql statement",
    "executing prepared sql update",
    "executing prepared sql query",
    "executing sql statement",
    "executing sql query",
    "executing sql update",
    "executing sql batch update",
    "calling stored procedure",
];

#[derive(Default)]
pub(crate) struct JdbcState {
    pending_sql: Option<String>,
    pending_line_no: usize,
    pending_raw_line: String,
    pending_timestamp: Option<i64>,
    pending_duration: Option<f64>,
    params: Vec<(usize, Param)>,
    collecting: bool,
    buffer: String,
    net: i32,
    in_string: bool,
    first_line_no: usize,
    first_raw_line: String,
    first_timestamp: Option<i64>,
    first_duration: Option<f64>,
    truncated: bool,
}

impl JdbcState {
    pub(crate) fn continuing(&self) -> bool {
        self.collecting
    }

    pub(crate) fn feed(&mut self, line_no: usize, line: &str, emitter: &mut Emitter<'_>) -> bool {
        if let Some(named) = find_named_row(line) {
            self.close(emitter);
            self.emit_named(line_no, line, named, emitter);
            return true;
        }

        if self.collecting {
            if self.is_boundary(line) {
                self.close(emitter);
            } else {
                self.append_collecting(line, emitter);
                return true;
            }
        }

        if let Some((phrase_start, phrase_len)) = find_action(line) {
            let after = phrase_start + phrase_len;
            let mut open = after;
            let bytes = line.as_bytes();
            while open < bytes.len() && (bytes[open] == b' ' || bytes[open] == b'\t') {
                open += 1;
            }
            if open < bytes.len() && bytes[open] == b'[' {
                self.close(emitter);
                let scan = scan_brackets(line, open + 1, 1, false);
                if let Some(close) = scan.close {
                    let sql = line[open + 1..close].trim();
                    if !sql.is_empty() {
                        self.pending_sql = Some(sql.to_string());
                        self.pending_line_no = line_no;
                        self.pending_raw_line = line.to_string();
                        self.pending_timestamp = emitter.current_timestamp();
                        self.pending_duration = emitter.current_duration();
                        self.params.clear();
                    }
                } else {
                    self.begin_collecting(
                        line_no,
                        line,
                        open + 1,
                        scan.net,
                        scan.in_string,
                        emitter,
                    );
                }
                return true;
            }
            // `Executing prepared SQL query` header row: no SQL text, noise.
            return true;
        }

        if let Some((index, param)) = parse_jdbc_setting(line) {
            if self.pending_sql.is_some() {
                self.params.push((index, param));
            }
            return true;
        }

        if self.pending_sql.is_some() && !self.params.is_empty() && !line.trim().is_empty() {
            // Any non-parameter row ends the statement; the row itself is noise
            // or belongs to another family (already offered to it first).
            self.close(emitter);
        }
        false
    }

    pub(crate) fn append_continuation(&mut self, line: &str, emitter: &mut Emitter<'_>) -> bool {
        if !self.collecting {
            return false;
        }
        if self.is_boundary(line) {
            self.close(emitter);
            return false;
        }
        self.append_collecting(line, emitter);
        true
    }

    fn is_boundary(&self, line: &str) -> bool {
        find_action(line).is_some()
            || find_named_row(line).is_some()
            || parse_jdbc_setting(line).is_some()
    }

    fn begin_collecting(
        &mut self,
        line_no: usize,
        line: &str,
        from: usize,
        net: i32,
        in_string: bool,
        emitter: &Emitter<'_>,
    ) {
        self.collecting = true;
        self.buffer.clear();
        self.buffer.push_str(&line[from..]);
        self.net = net;
        self.in_string = in_string;
        self.first_line_no = line_no;
        self.first_raw_line = line.to_string();
        self.first_timestamp = emitter.current_timestamp();
        self.first_duration = emitter.current_duration();
        self.truncated = false;
    }

    fn append_collecting(&mut self, line: &str, emitter: &mut Emitter<'_>) {
        if self.buffer.len() + line.len() + 1 > emitter.opts.max_sql_len {
            self.truncated = true;
            self.close(emitter);
            return;
        }
        let line_start = self.buffer.len();
        self.buffer.push('\n');
        self.buffer.push_str(line);
        let scan = scan_brackets(line, 0, self.net, self.in_string);
        self.net = scan.net;
        self.in_string = scan.in_string;
        if let Some(close) = scan.close {
            let sql = self.buffer[..line_start + 1 + close].trim().to_string();
            self.collecting = false;
            self.buffer.clear();
            self.net = 0;
            self.in_string = false;
            self.pending_sql = Some(sql);
            self.pending_line_no = self.first_line_no;
            self.pending_raw_line = self.first_raw_line.clone();
            self.pending_timestamp = self.first_timestamp;
            self.pending_duration = self.first_duration;
        }
    }

    fn emit_named(
        &mut self,
        line_no: usize,
        line: &str,
        named: NamedRow<'_>,
        emitter: &mut Emitter<'_>,
    ) {
        let (raw_sql, cut) = cap_sql(named.sql.trim(), emitter.opts.max_sql_len);
        let map = parse_named_map(named.map);
        let (sql, params, missing) = replace_named(&raw_sql, &map);
        let mut message_keys = Vec::new();
        let mut confidence = Confidence::Exact;
        if cut {
            confidence = Confidence::Partial;
            message_keys.push(message_keys::ENTRY_SQL_TRUNCATED.to_string());
        }
        if missing > 0 {
            confidence = Confidence::Partial;
            message_keys.push(message_keys::ENTRY_PARAMS_MISSING.to_string());
        }
        emitter.emit(PendingEntry {
            family: Family::JdbcTemplate,
            line_no,
            timestamp_ms: emitter.current_timestamp(),
            duration_ms: emitter.current_duration(),
            raw_sql: raw_sql.clone(),
            sql,
            params,
            class: classify(&raw_sql),
            confidence,
            affected_rows: None,
            raw_line: (confidence != Confidence::Exact).then(|| line.to_string()),
            message_keys,
        });
    }

    pub(crate) fn close(&mut self, emitter: &mut Emitter<'_>) {
        let truncated = self.truncated;
        let unclosed = self.collecting;
        if unclosed && self.pending_sql.is_none() && !self.buffer.trim().is_empty() {
            // Keep an unfinished `[...]` body: preserving the fragment beats
            // silently dropping the statement.
            self.pending_sql = Some(self.buffer.trim().to_string());
            self.pending_line_no = self.first_line_no;
            self.pending_raw_line = self.first_raw_line.clone();
            self.pending_timestamp = self.first_timestamp;
            self.pending_duration = self.first_duration;
        }
        self.truncated = false;
        self.collecting = false;
        self.buffer.clear();
        self.net = 0;
        self.in_string = false;

        let Some(sql) = self.pending_sql.take() else {
            self.params.clear();
            return;
        };
        let mut params = std::mem::take(&mut self.params);
        params.sort_by_key(|(index, _)| *index);
        let params: Vec<Param> = params.into_iter().map(|(_, param)| param).collect();

        let (raw_sql, cut) = cap_sql(&sql, emitter.opts.max_sql_len);
        let restored = restore(&raw_sql, &params);
        let mut message_keys = Vec::new();
        let mut confidence = Confidence::Exact;
        if cut || truncated {
            confidence = Confidence::Partial;
            message_keys.push(message_keys::ENTRY_SQL_TRUNCATED.to_string());
        }
        if unclosed {
            confidence = Confidence::Partial;
            message_keys.push(message_keys::ENTRY_MULTILINE_UNCLOSED.to_string());
        }
        if restored.used < restored.placeholders {
            confidence = Confidence::Partial;
            message_keys.push(message_keys::ENTRY_PARAMS_MISSING.to_string());
        }
        if restored.placeholders == 0 && !params.is_empty() {
            confidence = Confidence::Partial;
            message_keys.push(message_keys::ENTRY_PARAMS_UNUSED.to_string());
        }
        let rendered = params.iter().map(render_param).collect();
        emitter.emit(PendingEntry {
            family: Family::JdbcTemplate,
            line_no: self.pending_line_no,
            timestamp_ms: self.pending_timestamp,
            duration_ms: self.pending_duration,
            raw_sql: raw_sql.clone(),
            sql: restored.sql,
            params: rendered,
            class: classify(&raw_sql),
            confidence,
            affected_rows: None,
            raw_line: (confidence != Confidence::Exact).then(|| self.pending_raw_line.clone()),
            message_keys,
        });
        self.pending_line_no = 0;
        self.pending_raw_line.clear();
        self.pending_timestamp = None;
        self.pending_duration = None;
    }
}

/// `sql = ... , parameters = {...}` on one row.
struct NamedRow<'a> {
    sql: &'a str,
    map: &'a str,
}

fn find_named_row(line: &str) -> Option<NamedRow<'_>> {
    let mut from = 0;
    while let Some(rel) = find_ci(&line[from..], "sql") {
        let start = from + rel;
        let after = start + 3;
        let bytes = line.as_bytes();
        let mut i = after;
        while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'=' {
            let sql_start = i + 1;
            if let Some(params_rel) = find_ci(&line[sql_start..], ", parameters") {
                let sql_end = sql_start + params_rel;
                let mut j = sql_start + params_rel + ", parameters".len();
                while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'=' {
                    j += 1;
                    while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                        j += 1;
                    }
                }
                if j < bytes.len() && bytes[j] == b'{' {
                    let map_start = j + 1;
                    if let Some(close_rel) = line[map_start..].find('}') {
                        let map_end = map_start + close_rel;
                        return Some(NamedRow {
                            sql: line[sql_start..sql_end].trim(),
                            map: &line[map_start..map_end],
                        });
                    }
                }
            }
        }
        from = start + 3;
    }
    None
}

/// Replace `:name` references, returning `(sql, params, missing_count)`.
fn replace_named(sql: &str, map: &[(String, String)]) -> (String, Vec<String>, usize) {
    let mut out = String::with_capacity(sql.len());
    let bytes = sql.as_bytes();
    let mut params = Vec::new();
    let mut missing = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b':' && i + 1 < bytes.len() && bytes[i + 1] != b':' {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            if end > start {
                let name = &sql[start..end];
                match map.iter().find(|(key, _)| key == name) {
                    Some((_, value)) => {
                        if value.eq_ignore_ascii_case("null") {
                            out.push_str("NULL");
                            params.push("NULL".to_string());
                        } else {
                            let rendered = render_untyped(value);
                            out.push_str(&rendered);
                            params.push(rendered);
                        }
                    }
                    None => {
                        missing += 1;
                        out.push_str(&sql[i..end]);
                    }
                }
                i = end;
                continue;
            }
        }
        let ch_len = utf8_len(bytes[i]);
        out.push_str(&sql[i..(i + ch_len).min(bytes.len())]);
        i += ch_len;
    }
    (out, params, missing)
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

fn find_action(line: &str) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    for phrase in ACTION_PHRASES {
        if let Some(index) = find_ci(line, phrase) {
            if best.is_none_or(|(best_index, _)| index < best_index) {
                best = Some((index, phrase.len()));
            }
        }
    }
    best
}

struct BracketScan {
    net: i32,
    in_string: bool,
    close: Option<usize>,
}

/// String-aware bracket scan: `[` +1, `]` -1; brackets inside single-quoted
/// strings do not count, `''` is an escaped quote. Returns the first index where
/// the net count reaches zero (the message terminator).
fn scan_brackets(text: &str, from: usize, mut net: i32, mut in_string: bool) -> BracketScan {
    let bytes = text.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            if c == b'\'' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 2;
                    continue;
                }
                in_string = false;
            }
        } else if c == b'\'' {
            in_string = true;
        } else if c == b'[' {
            net += 1;
        } else if c == b']' {
            net -= 1;
            if net == 0 {
                return BracketScan {
                    net,
                    in_string,
                    close: Some(i),
                };
            }
        }
        i += 1;
    }
    BracketScan {
        net,
        in_string,
        close: None,
    }
}

#[cfg(test)]
mod tests {
    use crate::core::log::model::ParseOptions;
    use crate::core::log::parse;

    fn run(lines: &[&str]) -> Vec<super::super::model::LogEntry> {
        parse(&lines.join("\n"), &ParseOptions::default()).entries
    }

    #[test]
    fn prepared_statement_with_indexed_params() {
        let entries = run(&[
            "2026-08-24 15:46:42.444 DEBUG o.s.j.c.JdbcTemplate - Executing prepared SQL statement [SELECT \"owner\" FROM t WHERE deleted = ? AND id = ?]",
            "2026-08-24 15:46:42.444 TRACE o.s.j.c.StatementCreatorUtils - Setting SQL statement parameter value: column index 1, parameter value [0], value class [java.lang.Integer], SQL type unknown",
            "2026-08-24 15:46:42.444 TRACE o.s.j.c.StatementCreatorUtils - Setting SQL statement parameter value: column index 2, parameter value [abc], value class [java.lang.String], SQL type unknown",
        ]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].params, vec!["0", "'abc'"]);
        assert_eq!(
            entries[0].sql,
            "SELECT \"owner\" FROM t WHERE deleted = 0 AND id = 'abc'"
        );
    }

    #[test]
    fn multiline_brackets_keep_inner_brackets() {
        let entries = run(&[
            "Executing prepared SQL statement [SELECT a[1] FROM t WHERE x = ?",
            " and y = ?]",
            "Setting SQL statement parameter value: column index 1, parameter value [1], value class [java.lang.Integer], SQL type unknown",
            "Setting SQL statement parameter value: column index 2, parameter value [2], value class [java.lang.Integer], SQL type unknown",
        ]);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].sql.contains("a[1]"));
        assert_eq!(entries[0].sql, "SELECT a[1] FROM t WHERE x = 1\n and y = 2");
    }

    #[test]
    fn named_parameter_row() {
        let entries =
            run(&["sql = SELECT * FROM t WHERE a = :a and b = :b , parameters = {a=1, b=x}"]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].sql, "SELECT * FROM t WHERE a = 1 and b = 'x'");
        assert_eq!(entries[0].params, vec!["1", "'x'"]);
    }
}
