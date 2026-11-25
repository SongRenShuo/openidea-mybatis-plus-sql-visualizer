//! Hibernate / JPA console family (reference `HibernateLogParser`).
//!
//! Supported shapes:
//! - `Hibernate: select ...` (SQL on the same line or the next line)
//! - `/* load com.example.User */ select ...` (comment prefix, possibly multi-line)
//! - bare SQL rows when `ParseOptions::recognize_bare_sql` is on
//! - `binding parameter [1] as [BIGINT] - [1]` (one row per parameter)
//! - logback / Spring Boot rows of the Hibernate loggers themselves, which
//!   carry no `Hibernate:` marker:
//!
//!   ```text
//!   2026-11-26 10:00:01.123 DEBUG 4242 --- [nio-8080-exec-1] org.hibernate.SQL : select …
//!   2026-11-26 10:00:01.124 TRACE 4242 --- [nio-8080-exec-1] org.hibernate.orm.jdbc.bind : binding parameter [1] as [BIGINT] - [42]
//!   ```
//!
//! Not covered:
//! - schema/DML-DDL rows (`org.hibernate.tool.schema…`), and DDL through the
//!   SQL logger in general: a logback row is opened only by a
//!   SELECT/INSERT/UPDATE/DELETE/MERGE/WITH/CALL/REPLACE body.
//! - Hibernate 6.4+ binding syntax `binding parameter (1:BIGINT) -> [42]`; the
//!   row is consumed as Hibernate noise, so the statement stays `partial`.
//! - Patterns without a level token (`%d %logger - %msg`): such a row cannot be
//!   told apart from a business log line that happens to mention SQL.
//! - A logger row whose message body is empty and whose SQL starts on the next
//!   (unprefixed) line: only the bare-row rule sees that SQL.

use super::model::{Confidence, Family, ParseOptions, message_keys};
use super::params::{Param, parse_binding, render_param};
use super::parser::{Emitter, PendingEntry, cap_sql, is_continuation_candidate};
use super::restore::restore;
use super::text::{
    HibernateLoggerKind, after_ci, classify, find_ci, hibernate_logger_row, is_log_framework_line,
    starts_with_sql_keyword,
};

#[derive(Default)]
pub(crate) struct HibernateState {
    collecting: bool,
    in_comment: bool,
    buffer: String,
    first_line_no: usize,
    first_raw_line: String,
    timestamp: Option<i64>,
    duration: Option<f64>,
    params: Vec<(usize, Param)>,
    truncated: bool,
}

impl HibernateState {
    pub(crate) fn collecting(&self) -> bool {
        self.collecting
    }

    pub(crate) fn feed(
        &mut self,
        line_no: usize,
        line: &str,
        emitter: &mut Emitter<'_>,
        opts: &ParseOptions,
    ) -> bool {
        if let Some(after) = after_ci(line, "hibernate:") {
            self.close(emitter);
            let rest = line[after..].trim();
            if rest.starts_with("/*") {
                self.start_comment(rest, line_no, line, emitter);
            } else if rest.is_empty() {
                self.begin_block(line_no, line, emitter, false);
            } else {
                self.begin_block(line_no, line, emitter, false);
                self.buffer.push_str(rest);
            }
            return true;
        }

        if line.trim_start().starts_with("/*") {
            self.close(emitter);
            self.start_comment(line.trim_start(), line_no, line, emitter);
            return true;
        }

        // A logback-prefixed row of a Hibernate logger: `org.hibernate.SQL`
        // prints the statement, `org.hibernate.orm.jdbc.bind` one bound value.
        if let Some(row) = hibernate_logger_row(line) {
            match row.kind {
                HibernateLoggerKind::Statement => {
                    self.close(emitter);
                    let rest = line[row.message_start..].trim();
                    if rest.starts_with("/*") {
                        self.start_comment(rest, line_no, line, emitter);
                    } else {
                        self.begin_block(line_no, line, emitter, false);
                        self.buffer.push_str(rest);
                    }
                }
                HibernateLoggerKind::Parameter => {
                    // Same bookkeeping as the unprefixed `binding parameter`
                    // row: it belongs to whichever statement is open.
                    if self.collecting
                        && let Some((index, param)) = parse_binding(line)
                    {
                        self.params.push((index, param));
                    }
                }
            }
            return true;
        }

        if let Some((index, param)) = parse_binding(line) {
            if self.collecting {
                self.params.push((index, param));
            }
            // A binding row without a Hibernate statement is noise; consuming
            // it keeps MyBatis/Hibernate rows from leaking into `unparsed`.
            return true;
        }

        if self.in_comment {
            if let Some(end) = line.find("*/") {
                self.in_comment = false;
                let after = line[end + 2..].trim();
                if starts_with_sql_keyword(after) {
                    if !self.buffer.is_empty() {
                        self.buffer.push(' ');
                    }
                    self.buffer.push_str(after);
                }
            }
            return true;
        }

        if self.collecting {
            if is_log_framework_line(line) && !starts_with_sql_keyword(line.trim()) {
                self.close(emitter);
                return false;
            }
            if starts_with_sql_keyword(line.trim()) {
                self.close(emitter);
                self.begin_block(line_no, line, emitter, false);
                self.buffer.push_str(line.trim());
                return true;
            }
            if looks_like_foreign(line) {
                self.close(emitter);
                return false;
            }
            if !line.trim().is_empty() {
                if self.buffer.len() + 1 + line.trim().len() > opts.max_sql_len {
                    self.truncated = true;
                } else {
                    if !self.buffer.is_empty() {
                        self.buffer.push(' ');
                    }
                    self.buffer.push_str(line.trim());
                }
            }
            return true;
        }

        if opts.recognize_bare_sql && starts_with_sql_keyword(line.trim()) {
            self.begin_block(line_no, line, emitter, false);
            self.buffer.push_str(line.trim());
            return true;
        }
        false
    }

    pub(crate) fn append_continuation(&mut self, line: &str) -> bool {
        if !self.collecting || self.in_comment || !is_continuation_candidate(line) {
            return false;
        }
        if self.buffer.len() + 1 + line.trim().len() > 20_000 {
            self.truncated = true;
            return true;
        }
        if !self.buffer.is_empty() {
            self.buffer.push(' ');
        }
        self.buffer.push_str(line.trim());
        true
    }

    fn begin_block(&mut self, line_no: usize, line: &str, emitter: &Emitter<'_>, in_comment: bool) {
        self.collecting = true;
        self.in_comment = in_comment;
        self.buffer.clear();
        self.first_line_no = line_no;
        self.first_raw_line = line.to_string();
        self.timestamp = emitter.current_timestamp();
        self.duration = emitter.current_duration();
        self.params.clear();
        self.truncated = false;
    }

    /// A line that starts with `/*` (the `Hibernate:` prefix may already be
    /// stripped). Handles both same-line and cross-line comment blocks.
    fn start_comment(&mut self, text: &str, line_no: usize, line: &str, emitter: &Emitter<'_>) {
        match text.find("*/") {
            None => self.begin_block(line_no, line, emitter, true),
            Some(end) => {
                let after = text[end + 2..].trim();
                if starts_with_sql_keyword(after) {
                    self.begin_block(line_no, line, emitter, false);
                    self.buffer.push_str(after);
                } else {
                    self.collecting = false;
                    self.in_comment = false;
                }
            }
        }
    }

    pub(crate) fn close(&mut self, emitter: &mut Emitter<'_>) {
        if !self.collecting {
            return;
        }
        self.collecting = false;
        let in_comment = std::mem::take(&mut self.in_comment);
        let truncated = self.truncated;
        self.truncated = false;
        let sql = std::mem::take(&mut self.buffer);
        if sql.trim().is_empty() {
            self.params.clear();
            return;
        }
        let mut params = std::mem::take(&mut self.params);
        params.sort_by_key(|(index, _)| *index);
        let params: Vec<Param> = params.into_iter().map(|(_, param)| param).collect();

        let (raw_sql, cut) = cap_sql(sql.trim(), emitter.opts.max_sql_len);
        let restored = restore(&raw_sql, &params);
        let mut message_keys = Vec::new();
        let mut confidence = Confidence::Exact;
        if cut || truncated {
            confidence = Confidence::Partial;
            message_keys.push(message_keys::ENTRY_SQL_TRUNCATED.to_string());
        }
        if in_comment {
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
            family: Family::Hibernate,
            line_no: self.first_line_no,
            timestamp_ms: self.timestamp,
            duration_ms: self.duration,
            raw_sql: raw_sql.clone(),
            sql: restored.sql,
            params: rendered,
            class: classify(&raw_sql),
            confidence,
            affected_rows: None,
            raw_line: (confidence != Confidence::Exact).then(|| self.first_raw_line.clone()),
            message_keys,
            thread: emitter.current_thread(),
        });
        self.first_line_no = 0;
        self.first_raw_line.clear();
        self.timestamp = None;
        self.duration = None;
    }
}

fn looks_like_foreign(line: &str) -> bool {
    find_ci(line, "preparing:").is_some()
        || find_ci(line, "parameters:").is_some()
        || find_ci(line, "setting sql statement parameter value").is_some()
        || find_ci(line, "executing prepared sql").is_some()
        || super::p6spy::looks_like_row(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::log::model::{EntryClass, ParseOptions};
    use crate::core::log::{parse, parse_with_sources};

    fn run(lines: &[&str]) -> Vec<super::super::model::LogEntry> {
        parse(&lines.join("\n"), &ParseOptions::default()).entries
    }

    #[test]
    fn hibernate_sql_with_binding_parameters() {
        let entries = run(&[
            "Hibernate: select user0_.id, user0_.name from users user0_ where user0_.id=? and user0_.name=?",
            "binding parameter [1] as [BIGINT] - [42]",
            "binding parameter [2] as [VARCHAR] - [John Smith]",
        ]);
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].sql,
            "select user0_.id, user0_.name from users user0_ where user0_.id=42 and user0_.name='John Smith'"
        );
        assert_eq!(entries[0].params, vec!["42", "'John Smith'"]);
    }

    #[test]
    fn comment_prefix_and_timestamp_literal() {
        let entries = run(&[
            "/* load com.example.User */ select * from users where created_at > ?",
            "binding parameter [1] as [TIMESTAMP] - [2025-01-01 12:00:00.0]",
        ]);
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].sql,
            "select * from users where created_at > TIMESTAMP '2025-01-01 12:00:00.0'"
        );
    }

    #[test]
    fn bare_sql_row_is_recognized() {
        let entries = run(&["select 1 from dual"]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].family, Family::Hibernate);
        assert_eq!(entries[0].sql, "select 1 from dual");
    }

    // -----------------------------------------------------------------------
    // logback / Spring Boot rows of the Hibernate loggers (no `Hibernate:`)
    // -----------------------------------------------------------------------

    #[test]
    fn logback_sql_row_and_bind_row() {
        let entries = run(&[
            "2026-11-26 10:00:01.123 DEBUG 4242 --- [nio-8080-exec-1] org.hibernate.SQL : select u.id, u.name from users u where u.id = ?",
            "2026-11-26 10:00:01.124 TRACE 4242 --- [nio-8080-exec-1] org.hibernate.orm.jdbc.bind : binding parameter [1] as [BIGINT] - [42]",
        ]);
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.family, Family::Hibernate);
        assert_eq!(entry.line_no, 1);
        assert_eq!(entry.class, EntryClass::Select);
        assert_eq!(entry.confidence, Confidence::Exact);
        assert_eq!(
            entry.raw_sql,
            "select u.id, u.name from users u where u.id = ?"
        );
        assert_eq!(
            entry.sql,
            "select u.id, u.name from users u where u.id = 42"
        );
        assert_eq!(entry.params, vec!["42"]);
        assert_eq!(entry.duration_ms, None, "没有闭合行就不编时长");
    }

    #[test]
    fn logback_multiline_sql_keeps_continuation_lines() {
        let entries = run(&[
            "2026-11-26 10:00:02.001 DEBUG 4242 --- [nio-8080-exec-2] org.hibernate.SQL : select id, name",
            "    from users",
            "    where name = ? and id > ?",
            "2026-11-26 10:00:02.002 TRACE 4242 --- [nio-8080-exec-2] org.hibernate.orm.jdbc.bind : binding parameter [2] as [BIGINT] - [100]",
            "2026-11-26 10:00:02.003 TRACE 4242 --- [nio-8080-exec-2] org.hibernate.orm.jdbc.bind : binding parameter [1] as [VARCHAR] - [alice]",
        ]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].line_no, 1);
        assert_eq!(
            entries[0].raw_sql,
            "select id, name from users where name = ? and id > ?"
        );
        assert_eq!(
            entries[0].sql,
            "select id, name from users where name = 'alice' and id > 100"
        );
        assert_eq!(entries[0].params, vec!["'alice'", "100"]);
        assert_eq!(entries[0].confidence, Confidence::Exact);
    }

    #[test]
    fn logback_sql_statement_logger_tail_is_recognized() {
        let entries = run(&[
            "2026-11-26 10:00:03.001 DEBUG 4242 --- [main] o.h.e.jdbc.spi.SqlStatementLogger : select id from users where id = ?",
            "2026-11-26 10:00:03.002 TRACE 4242 --- [main] org.hibernate.orm.jdbc.bind : binding parameter [1] as [BIGINT] - [7]",
        ]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].sql, "select id from users where id = 7");
    }

    #[test]
    fn logback_padded_logger_column_is_recognized() {
        // Spring Boot 默认 pattern 把 logger 右补空格成 `%-40.40logger{39}` 一列。
        let entries = run(&[
            "2026-11-26 10:00:06.001  DEBUG 4242 --- [nio-8080-exec-4] org.hibernate.SQL                       : select id from users where id = ?",
            "2026-11-26 10:00:06.002  TRACE 4242 --- [nio-8080-exec-4] org.hibernate.orm.jdbc.bind              : binding parameter [1] as [BIGINT] - [9]",
        ]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].sql, "select id from users where id = 9");
        assert_eq!(entries[0].confidence, Confidence::Exact);
    }

    #[test]
    fn logback_rows_carry_thread_and_correlation_id_but_no_statement_id() {
        let text = "\
2026-11-26 10:00:04.001 DEBUG 4242 --- [nio-8080-exec-1] [traceId=4bf92f3577b34da6] org.hibernate.SQL : select id from users where request_id = 'tx-9'
2026-11-26 10:00:04.002 TRACE 4242 --- [nio-8080-exec-1] [traceId=4bf92f3577b34da6] org.hibernate.orm.jdbc.bind : binding parameter [1] as [BIGINT] - [42]";
        let parsed = parse_with_sources(text, &ParseOptions::default());
        assert_eq!(parsed.analysis.entries.len(), 1);
        let source = parsed.sources.get(1).expect("statement row provenance");
        assert_eq!(source.thread.as_deref(), Some("nio-8080-exec-1"));
        assert_eq!(
            source.request_id.as_deref(),
            Some("4bf92f3577b34da6"),
            "前缀里的 traceId 取到，SQL 谓词里的 request_id 不算"
        );
        assert!(
            source.source.is_none(),
            "org.hibernate.SQL 是 logger 名不是 statement id"
        );
    }

    #[test]
    fn a_business_row_mentioning_sql_is_not_a_hibernate_row() {
        let entries = run(&[
            "2026-11-26 10:00:05.001 INFO 4242 --- [main] c.e.Foo : order-service - select * from users",
            "2026-11-26 10:00:05.002 INFO 4242 --- [main] c.e.Foo : replayed SQL: select * from users",
        ]);
        assert!(
            entries.is_empty(),
            "logger 末段不是 Hibernate 日志器，不当 SQL"
        );
    }
}
