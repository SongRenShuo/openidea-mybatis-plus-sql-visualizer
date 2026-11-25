//! Single-pass dispatcher plus the post-pass index (dedupe / N+1 / slow /
//! errors).
//!
//! The pass is O(total characters): every line is inspected by each family's
//! state machine, and repeated statements are grouped through `HashMap`
//! indexes, never by rescanning earlier entries.

use std::collections::HashMap;

use super::model::{
    Confidence, DuplicateGroup, EntryClass, Family, LogAnalysis, LogEntry, NPlusOneGroup,
    ParseOptions, UnparsedLine, message_keys,
};
use super::text::{
    classify, is_error_line, is_log_framework_line, is_stack_continuation, normalize_ws,
    parameterize_literals, starts_with_sql_keyword, truncate_chars, unparsed_line,
};

/// Upper bound on `unparsed_lines`; a pathological log must not produce an
/// unbounded JSON payload. Further rows are dropped (the count is still visible
/// through the missing tail).
const MAX_UNPARSED: usize = 1000;

/// One statement ready to be finalised by [`Emitter`].
pub(crate) struct PendingEntry {
    pub family: Family,
    pub line_no: usize,
    pub timestamp_ms: Option<i64>,
    pub duration_ms: Option<f64>,
    pub raw_sql: String,
    pub sql: String,
    pub params: Vec<String>,
    pub class: EntryClass,
    pub confidence: Confidence,
    pub affected_rows: Option<u64>,
    pub raw_line: Option<String>,
    pub message_keys: Vec<String>,
    /// Console thread that started the statement, when the line carried one.
    /// Kept on the pending state (not the closing line) so a late `Total:` row
    /// from another thread cannot claim this statement.
    pub thread: Option<String>,
}

/// Which mybatis close row produced a count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CloseRow {
    /// `<==      Total: N` — result rows returned to the caller.
    Total,
    /// `<==    Updates: N` — rows changed by a DML statement.
    Updates,
}

/// Shared output sink. Families push `PendingEntry` values; the emitter owns
/// the small amount of cross-line state (last entry, pending duration).
pub(crate) struct Emitter<'a> {
    pub opts: &'a ParseOptions,
    pub entries: Vec<LogEntry>,
    pub unparsed: Vec<UnparsedLine>,
    current_timestamp: Option<i64>,
    current_duration: Option<f64>,
    current_thread: Option<String>,
    pending_duration: Option<(f64, usize)>,
    last_entry: Option<usize>,
    /// Last entry emitted per console thread, so a close row is resolved by
    /// thread instead of by "whatever was emitted last".
    last_by_thread: HashMap<String, usize>,
}

impl<'a> Emitter<'a> {
    fn new(opts: &'a ParseOptions) -> Self {
        Self {
            opts,
            entries: Vec::new(),
            unparsed: Vec::new(),
            current_timestamp: None,
            current_duration: None,
            current_thread: None,
            pending_duration: None,
            last_entry: None,
            last_by_thread: HashMap::new(),
        }
    }

    pub(crate) fn current_duration(&self) -> Option<f64> {
        self.current_duration
    }

    pub(crate) fn current_timestamp(&self) -> Option<i64> {
        self.current_timestamp
    }

    pub(crate) fn current_thread(&self) -> Option<String> {
        self.current_thread.clone()
    }

    pub(crate) fn emit(&mut self, mut pending: PendingEntry) {
        if pending.duration_ms.is_none() {
            pending.duration_ms = self.current_duration;
        }
        if pending.duration_ms.is_none()
            && let Some((duration, line_no)) = self.pending_duration
            && pending.line_no.saturating_sub(line_no) <= 3
        {
            pending.duration_ms = Some(duration);
            self.pending_duration = None;
        }
        if pending.confidence != Confidence::Exact && pending.raw_line.is_none() {
            pending.raw_line = Some(pending.raw_sql.clone());
        }
        let thread_key = pending
            .thread
            .take()
            .or_else(|| self.current_thread.clone())
            .unwrap_or_default();
        let entry = LogEntry {
            family: pending.family,
            line_no: pending.line_no,
            timestamp_ms: pending.timestamp_ms,
            duration_ms: pending.duration_ms,
            sql: pending.sql,
            params: pending.params,
            class: pending.class,
            confidence: pending.confidence,
            raw_sql: pending.raw_sql,
            affected_rows: pending.affected_rows,
            result_rows: None,
            raw_line: pending.raw_line,
            message_keys: pending.message_keys,
        };
        self.entries.push(entry);
        let index = self.entries.len() - 1;
        self.last_entry = Some(index);
        self.last_by_thread.insert(thread_key, index);
    }

    /// Emit an error row. Per the confidence model, unresolved rows never carry
    /// SQL, so `sql` stays empty and the original line goes to `raw_line`.
    pub(crate) fn emit_error(&mut self, line_no: usize, line: &str) {
        let text = line.trim();
        let entry = LogEntry {
            family: Family::Unknown,
            line_no,
            timestamp_ms: self.current_timestamp,
            duration_ms: None,
            sql: String::new(),
            params: Vec::new(),
            class: EntryClass::Error,
            confidence: Confidence::Unresolved,
            raw_sql: String::new(),
            affected_rows: None,
            result_rows: None,
            raw_line: Some(text.to_string()),
            message_keys: vec![message_keys::ENTRY_SQL_MISSING.to_string()],
        };
        self.entries.push(entry);
        self.last_entry = Some(self.entries.len() - 1);
    }

    pub(crate) fn unparsed(&mut self, line_no: usize, line: &str, key: &str) {
        if self.unparsed.len() >= MAX_UNPARSED {
            return;
        }
        self.unparsed.push(unparsed_line(line_no, line, key));
    }

    /// A duration was found on a line that produced no entry (p6spy `result`
    /// row). Attach it to the immediately preceding entry when they are
    /// adjacent, otherwise remember it for the next emission.
    pub(crate) fn notice_duration(&mut self, line_no: usize, duration: f64) {
        if let Some(index) = self.last_entry {
            let last = &mut self.entries[index];
            if last.duration_ms.is_none() && line_no.saturating_sub(last.line_no) <= 3 {
                last.duration_ms = Some(duration);
                return;
            }
        }
        self.pending_duration = Some((duration, line_no));
    }

    /// Bookkeeping for a mybatis close row (`<== Total:` / `<== Updates:`):
    /// the count goes to the statement of the same thread, and its duration is
    /// the timestamp gap between the `Preparing:` line and this row.
    pub(crate) fn close_row(&mut self, line_no: usize, rows: Option<u64>, kind: CloseRow) {
        let Some(index) = self.close_row_target() else {
            return;
        };
        let close_timestamp = self.current_timestamp;
        let measured = self.current_duration;
        let entry = &mut self.entries[index];
        if entry.family != Family::MyBatis || line_no.saturating_sub(entry.line_no) > 10 {
            return;
        }
        if let Some(rows) = rows {
            match kind {
                CloseRow::Total => entry.result_rows.get_or_insert(rows),
                CloseRow::Updates => entry.affected_rows.get_or_insert(rows),
            };
        }
        if entry.duration_ms.is_none() {
            entry.duration_ms = measured.or_else(|| match (entry.timestamp_ms, close_timestamp) {
                // Both timestamps come from the same log clock; a negative gap
                // means the rows were reordered and is not a duration.
                (Some(start), Some(end)) if end >= start => Some((end - start) as f64),
                _ => None,
            });
        }
    }

    /// Entry a close row belongs to: the last one emitted from the same console
    /// thread. Without a thread token the previous entry is the only candidate,
    /// which keeps prefix-less single-threaded logs working.
    fn close_row_target(&self) -> Option<usize> {
        match self.current_thread.as_deref() {
            Some(thread) => self.last_by_thread.get(thread).copied(),
            None => self.last_entry,
        }
    }

    fn clear_line_context(&mut self) {
        self.current_timestamp = None;
        self.current_duration = None;
        self.current_thread = None;
    }
}

/// Stateful pass over the log text.
pub struct Parser<'a> {
    opts: &'a ParseOptions,
    emitter: Emitter<'a>,
    mybatis: super::mybatis::MyBatisState,
    hibernate: super::hibernate::HibernateState,
    jdbc: super::jdbc::JdbcState,
}

impl<'a> Parser<'a> {
    pub fn new(opts: &'a ParseOptions) -> Self {
        Self {
            opts,
            emitter: Emitter::new(opts),
            mybatis: super::mybatis::MyBatisState::default(),
            hibernate: super::hibernate::HibernateState::default(),
            jdbc: super::jdbc::JdbcState::default(),
        }
    }

    /// Feed one logical line (1-based `line_no`).
    pub fn feed(&mut self, line_no: usize, line: &str) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return;
        }
        self.emitter.current_timestamp =
            super::timestamp::parse_line(line, self.opts.timestamp_offset_minutes);
        self.emitter.current_duration = super::duration::extract(line);
        self.emitter.current_thread = super::text::thread_token(line);

        // Error signatures first: MyBatis `### Error` rows start with `###` but
        // are the *head* of an error block, not a stack continuation.
        if is_error_line(line) {
            self.flush_open();
            self.emitter.emit_error(line_no, line);
            return;
        }
        if is_stack_continuation(line) {
            // Stack frames and MyBatis `###` tails are part of an error block;
            // they are neither SQL nor "unparsed SQL".
            return;
        }

        if self.feed_mybatis(line_no, line)
            || self.feed_hibernate(line_no, line)
            || self.feed_jdbc(line_no, line)
            || self.feed_p6spy(line_no, line)
        {
            return;
        }

        // No family recognised the line. It may still be a continuation of the
        // statement that is currently open (formatted SQL in mapper files).
        if self.feed_continuation(line) {
            return;
        }

        if let Some(duration) = self.emitter.current_duration {
            self.emitter.notice_duration(line_no, duration);
        }
        if starts_with_sql_keyword(trimmed) {
            self.emitter
                .unparsed(line_no, line, message_keys::UNPARSED_UNRECOGNIZED_SQL);
        } else if super::text::find_ci(line, "preparing:").is_some() {
            self.emitter
                .unparsed(line_no, line, message_keys::UNPARSED_ORPHAN_PREPARING);
        } else if super::text::find_ci(line, "parameters:").is_some() {
            self.emitter
                .unparsed(line_no, line, message_keys::UNPARSED_ORPHAN_PARAMETERS);
        } else if is_log_framework_line(line) {
            // Business log row: noise, not an unparsed SQL line.
        }
    }

    fn feed_mybatis(&mut self, line_no: usize, line: &str) -> bool {
        let mut state = std::mem::take(&mut self.mybatis);
        let consumed = state.feed(line_no, line, &mut self.emitter, self.opts);
        self.mybatis = state;
        consumed
    }

    fn feed_hibernate(&mut self, line_no: usize, line: &str) -> bool {
        let mut state = std::mem::take(&mut self.hibernate);
        let consumed = state.feed(line_no, line, &mut self.emitter, self.opts);
        self.hibernate = state;
        consumed
    }

    fn feed_jdbc(&mut self, line_no: usize, line: &str) -> bool {
        let mut state = std::mem::take(&mut self.jdbc);
        let consumed = state.feed(line_no, line, &mut self.emitter);
        self.jdbc = state;
        consumed
    }

    fn feed_p6spy(&mut self, line_no: usize, line: &str) -> bool {
        super::p6spy::feed(line_no, line, &mut self.emitter)
    }

    /// Bare continuation routed to whichever statement was opened last.
    fn feed_continuation(&mut self, line: &str) -> bool {
        if self.jdbc.continuing() && self.jdbc.append_continuation(line, &mut self.emitter) {
            return true;
        }
        if self.hibernate.collecting() && self.hibernate.append_continuation(line) {
            return true;
        }
        if self.mybatis.continuing() && self.mybatis.append_continuation(line) {
            return true;
        }
        false
    }

    /// Close every open statement before the pass ends or an error row arrives.
    pub fn flush_open(&mut self) {
        let mut state = std::mem::take(&mut self.mybatis);
        state.close(&mut self.emitter, None);
        self.mybatis = state;

        let mut state = std::mem::take(&mut self.hibernate);
        state.close(&mut self.emitter);
        self.hibernate = state;

        let mut state = std::mem::take(&mut self.jdbc);
        state.close(&mut self.emitter);
        self.jdbc = state;
    }

    /// Finish the pass and build the analysis indexes.
    pub fn finish(mut self) -> LogAnalysis {
        self.emitter.clear_line_context();
        self.flush_open();
        let mut entries = self.emitter.entries;
        entries.sort_by_key(|e| e.line_no);

        let duplicates = build_duplicates(&entries, self.opts);
        let n_plus_one = build_n_plus_one(&entries, self.opts);
        let slow = entries
            .iter()
            .filter(|e| {
                e.duration_ms
                    .is_some_and(|d| d >= self.opts.slow_threshold_ms)
            })
            .cloned()
            .collect();
        let errors = entries
            .iter()
            .filter(|e| e.class == EntryClass::Error)
            .cloned()
            .collect();
        let mut unparsed_lines = self.emitter.unparsed;
        unparsed_lines.sort_by_key(|u| u.line_no);

        LogAnalysis {
            entries,
            duplicates,
            n_plus_one,
            slow,
            errors,
            unparsed_lines,
        }
    }
}

/// Parse a whole log text in one pass.
pub fn parse(text: &str, opts: &ParseOptions) -> LogAnalysis {
    let mut parser = Parser::new(opts);
    for (index, line) in text.lines().enumerate() {
        parser.feed(index + 1, line);
    }
    parser.finish()
}

fn build_duplicates(entries: &[LogEntry], opts: &ParseOptions) -> Vec<DuplicateGroup> {
    let mut groups: Vec<DuplicateGroup> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut last_timestamps: Vec<Option<i64>> = Vec::new();

    for entry in entries {
        if !entry.has_sql() {
            continue;
        }
        let key = if opts.ignore_params_in_dedupe {
            structure_template(&entry.sql)
        } else {
            normalize_ws(&entry.sql)
        };
        let mut reused = false;
        if let Some(&group_index) = index.get(&key) {
            let split = match (
                opts.dedupe_window_ms,
                entry.timestamp_ms,
                last_timestamps[group_index],
            ) {
                (Some(window), Some(current), Some(previous)) if window > 0 => {
                    current.saturating_sub(previous) >= window
                }
                _ => false,
            };
            if !split {
                let group = &mut groups[group_index];
                group.count += 1;
                group.last_line_no = entry.line_no;
                group.line_nos.push(entry.line_no);
                if entry.timestamp_ms.is_some() {
                    last_timestamps[group_index] = entry.timestamp_ms;
                }
                reused = true;
            }
        }
        if !reused {
            let group_index = groups.len();
            groups.push(DuplicateGroup {
                key: key.clone(),
                sql: entry.sql.clone(),
                params: entry.params.clone(),
                count: 1,
                first_line_no: entry.line_no,
                last_line_no: entry.line_no,
                line_nos: vec![entry.line_no],
            });
            index.insert(key, group_index);
            last_timestamps.push(entry.timestamp_ms);
        }
    }
    groups.retain(|group| group.count >= 2);
    groups
}

fn build_n_plus_one(entries: &[LogEntry], opts: &ParseOptions) -> Vec<NPlusOneGroup> {
    let threshold = opts.n_plus_one_threshold.max(2);
    let mut groups: Vec<NPlusOneGroup> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for entry in entries {
        if !entry.has_sql() {
            continue;
        }
        let template = structure_template(&entry.sql);
        match index.get(&template) {
            Some(&group_index) => {
                let group = &mut groups[group_index];
                group.count += 1;
                group.last_line_no = entry.line_no;
                group.line_nos.push(entry.line_no);
            }
            None => {
                index.insert(template.clone(), groups.len());
                groups.push(NPlusOneGroup {
                    template,
                    sample_sql: entry.sql.clone(),
                    count: 1,
                    first_line_no: entry.line_no,
                    last_line_no: entry.line_no,
                    line_nos: vec![entry.line_no],
                });
            }
        }
    }
    groups.retain(|group| group.count >= threshold);
    groups.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then(a.first_line_no.cmp(&b.first_line_no))
    });
    groups
}

/// Literal-parameterised, whitespace-normalised SQL used by N+1 and by
/// "ignore parameters" dedupe (reference `structureFingerprint`).
fn structure_template(sql: &str) -> String {
    let normalised = normalize_ws(sql);
    let without_terminator = normalised.trim_end_matches(';').trim();
    parameterize_literals(without_terminator)
}

/// Convenience re-export used by tests and by the panel when it needs the same
/// class as the parser.
pub fn classify_sql(sql: &str) -> EntryClass {
    classify(sql)
}

/// Truncate SQL to the configured cap, recording the diagnostic key.
pub(crate) fn cap_sql(raw: &str, max_len: usize) -> (String, bool) {
    truncate_chars(raw, max_len)
}

/// True when `line` may be appended to a multi-line SQL block.
pub(crate) fn is_continuation_candidate(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty()
        || is_stack_continuation(line)
        || is_error_line(line)
        || is_log_framework_line(line)
    {
        return false;
    }
    if starts_with_sql_keyword(t) {
        // A bare statement start opens its own entry; do not swallow it into a
        // previous multi-line block.
        return false;
    }
    // `Name: value` headers (Hibernate:, Executing ..., Setting ...) are not SQL.
    if header_like(t) {
        return false;
    }
    true
}

fn header_like(t: &str) -> bool {
    let bytes = t.as_bytes();
    if bytes.is_empty() || !(bytes[0].is_ascii_alphabetic()) {
        return false;
    }
    let mut i = 1;
    while i < bytes.len()
        && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'.')
    {
        i += 1;
    }
    if i < 3 {
        return false;
    }
    while i < bytes.len() && bytes[i] == b' ' {
        i += 1;
    }
    i < bytes.len() && bytes[i] == b':'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuation_rejects_stack_and_headers() {
        assert!(!is_continuation_candidate(
            "\tat com.example.Foo.bar(Foo.java:1)"
        ));
        assert!(!is_continuation_candidate("Hibernate: select 1"));
        assert!(!is_continuation_candidate(
            "2026-08-15 10:00:00.000 INFO x - started"
        ));
        assert!(is_continuation_candidate("FROM t WHERE id = ?"));
        assert!(is_continuation_candidate("and name = ?"));
    }

    #[test]
    fn template_folds_literals_but_not_identifiers() {
        assert_eq!(
            structure_template("select * from t2024 where id = 1 and name = 'a'"),
            "select * from t2024 where id = ? and name = ?"
        );
    }
}
