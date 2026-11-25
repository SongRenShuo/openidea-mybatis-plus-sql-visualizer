//! MyBatis console family (reference `MyBatisLogParser`).
//!
//! Supported shapes:
//! - `==> Preparing: SELECT ...` / `==> Parameters: 1(Long), abc(String)`
//! - native `Preparing:` / `Parameters:`
//! - `==>  Executing: ...` (`statementType="STATEMENT"`, no parameters)
//! - `<==      Total: N` closes a parameterless statement and records the count
//! - multi-line SQL: bare rows after `Preparing:` are appended

use super::model::{Confidence, Family, message_keys};
use super::params::{Param, parse_mybatis, render_param};
use super::parser::{Emitter, PendingEntry, cap_sql, is_continuation_candidate};
use super::restore::restore;
use super::text::{after_ci, classify, find_ci, is_log_framework_line};

#[derive(Default)]
pub(crate) struct MyBatisState {
    pending_sql: Option<String>,
    pending_line_no: usize,
    pending_raw_line: String,
    pending_timestamp: Option<i64>,
    start_duration: Option<f64>,
    params: Vec<Param>,
    wait_sql: bool,
    wait_params: bool,
    continuing: bool,
    truncated: bool,
}

impl MyBatisState {
    pub(crate) fn continuing(&self) -> bool {
        self.continuing
    }

    pub(crate) fn feed(
        &mut self,
        line_no: usize,
        line: &str,
        emitter: &mut Emitter<'_>,
        opts: &super::model::ParseOptions,
    ) -> bool {
        if let Some(after) = after_ci(line, "preparing:") {
            self.close(emitter, None);
            let rest = line[after..].trim();
            if rest.is_empty() {
                self.wait_sql = true;
                self.wait_params = false;
                self.continuing = false;
            } else {
                self.start(rest, line_no, line, emitter);
            }
            return true;
        }

        if let Some(after) = after_ci(line, "parameters:") {
            if self.wait_sql || self.pending_sql.is_none() {
                self.wait_sql = false;
                self.wait_params = false;
                emitter.unparsed(line_no, line, message_keys::UNPARSED_ORPHAN_PARAMETERS);
                return true;
            }
            let rest = line[after..].trim();
            if rest.is_empty() {
                self.wait_params = true;
                return true;
            }
            self.params.extend(parse_mybatis(rest));
            self.close(emitter, None);
            return true;
        }

        // `SimpleStatementHandler`: `==>  Executing: <sql>` — already complete.
        if let Some(after) = after_ci(line, "executing:") {
            self.close(emitter, None);
            let sql = line[after..].trim();
            if sql.is_empty() {
                return true;
            }
            let (raw_sql, truncated) = cap_sql(sql, opts.max_sql_len);
            let restored = restore(&raw_sql, &[]);
            emitter.emit(PendingEntry {
                family: Family::MyBatis,
                line_no,
                timestamp_ms: emitter.current_timestamp(),
                duration_ms: emitter.current_duration(),
                raw_sql: raw_sql.clone(),
                sql: restored.sql,
                params: Vec::new(),
                class: classify(&raw_sql),
                confidence: if truncated {
                    Confidence::Partial
                } else {
                    Confidence::Exact
                },
                affected_rows: None,
                raw_line: truncated.then(|| line.to_string()),
                message_keys: if truncated {
                    vec![message_keys::ENTRY_SQL_TRUNCATED.to_string()]
                } else {
                    Vec::new()
                },
            });
            return true;
        }

        // `<== Total: N` closes a parameterless statement and records the row
        // count on the entry emitted just before it.
        if let Some(after) = after_ci(line, "total:") {
            let rows = parse_leading_u64(&line[after..]);
            if self.pending_sql.is_some() {
                self.close(emitter, None);
            }
            if let Some(rows) = rows {
                emitter.attach_affected_rows(line_no, rows);
            }
            if let Some(duration) = emitter.current_duration() {
                emitter.notice_duration(line_no, duration);
            }
            return true;
        }

        if self.wait_sql {
            if line.trim().is_empty() {
                return true;
            }
            if is_log_framework_line(line) || is_foreign_statement_line(line) {
                return false;
            }
            self.wait_sql = false;
            self.start(line.trim(), line_no, line, emitter);
            return true;
        }

        if self.wait_params {
            self.wait_params = false;
            if line.trim().is_empty() {
                return true;
            }
            if find_ci(line, "preparing:").is_some() {
                // The statement has no parameters and the next statement starts
                // here; close and process this line as a new statement.
                self.close(emitter, None);
                return self.feed(line_no, line, emitter, opts);
            }
            if is_log_framework_line(line) || is_foreign_statement_line(line) {
                // The statement really has no parameters (MyBatis prints an
                // empty `Parameters:` prefix for some versions). Close and let
                // the line be processed by its own family.
                self.close(emitter, None);
                return false;
            }
            self.params.extend(parse_mybatis(line.trim()));
            self.close(emitter, None);
            return true;
        }

        if self.continuing {
            if is_continuation_candidate(line) {
                self.append_continuation(line);
                return true;
            }
            self.continuing = false;
        }
        false
    }

    pub(crate) fn append_continuation(&mut self, line: &str) -> bool {
        if !self.continuing {
            return false;
        }
        let Some(current) = self.pending_sql.as_mut() else {
            self.continuing = false;
            return false;
        };
        if current.len() + 1 + line.len() > 20_000 {
            self.continuing = false;
            self.truncated = true;
            return true;
        }
        current.push('\n');
        current.push_str(line.trim_end());
        true
    }

    fn start(&mut self, sql: &str, line_no: usize, line: &str, emitter: &Emitter<'_>) {
        self.pending_sql = Some(sql.to_string());
        self.pending_line_no = line_no;
        self.pending_raw_line = line.to_string();
        self.pending_timestamp = emitter.current_timestamp();
        self.start_duration = emitter.current_duration();
        self.params.clear();
        self.wait_sql = false;
        self.wait_params = false;
        self.continuing = true;
        self.truncated = false;
    }

    /// Emit the pending statement (if any) and reset.
    pub(crate) fn close(&mut self, emitter: &mut Emitter<'_>, extra_key: Option<&str>) {
        let Some(sql) = self.pending_sql.take() else {
            self.continuing = false;
            self.wait_params = false;
            return;
        };
        let truncated = self.truncated;
        self.continuing = false;
        self.wait_params = false;
        self.wait_sql = false;
        self.truncated = false;

        let params = std::mem::take(&mut self.params);
        let (raw_sql, cut) = cap_sql(&sql, emitter.opts.max_sql_len);
        let restored = restore(&raw_sql, &params);
        let mut message_keys = Vec::new();
        let mut confidence = Confidence::Exact;

        if cut || truncated {
            confidence = Confidence::Partial;
            message_keys.push(message_keys::ENTRY_SQL_TRUNCATED.to_string());
        }
        if restored.placeholders == 0 && !params.is_empty() {
            confidence = Confidence::Partial;
            message_keys.push(message_keys::ENTRY_PARAMS_UNUSED.to_string());
        } else if restored.used < restored.placeholders {
            confidence = Confidence::Partial;
            message_keys.push(message_keys::ENTRY_PARAMS_MISSING.to_string());
        }
        if let Some(key) = extra_key {
            confidence = Confidence::Partial;
            message_keys.push(key.to_string());
        }

        let rendered = params.iter().map(render_param).collect();
        emitter.emit(PendingEntry {
            family: Family::MyBatis,
            line_no: self.pending_line_no,
            timestamp_ms: self.pending_timestamp,
            duration_ms: self.start_duration,
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
        self.start_duration = None;
    }
}

fn parse_leading_u64(text: &str) -> Option<u64> {
    let mut value = 0u64;
    let mut digits = 0;
    for b in text.bytes() {
        if b.is_ascii_digit() {
            value = value.saturating_mul(10).saturating_add(u64::from(b - b'0'));
            digits += 1;
        } else if b == b' ' || b == b'\t' {
            if digits > 0 {
                break;
            }
        } else {
            break;
        }
    }
    (digits > 0).then_some(value)
}

/// Lines belonging to another family must not be taken as a MyBatis SQL block.
fn is_foreign_statement_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    line.trim_start().starts_with("/*")
        || find_ci(line, "hibernate:").is_some()
        || find_ci(line, "binding parameter").is_some()
        || find_ci(line, "setting sql statement parameter value").is_some()
        || lower.contains("executing prepared sql")
        || find_ci(line, "calling stored procedure").is_some()
        || line.contains('|') && super::p6spy::looks_like_row(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::log::model::ParseOptions;
    use crate::core::log::parse;

    fn run(lines: &[&str]) -> Vec<super::super::model::LogEntry> {
        parse(&lines.join("\n"), &ParseOptions::default()).entries
    }

    #[test]
    fn pairs_preparing_and_parameters() {
        let entries = run(&[
            "==>  Preparing: SELECT id, name FROM user WHERE id = ? AND status = ?",
            "==> Parameters: 1(Long), active(String)",
        ]);
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].sql,
            "SELECT id, name FROM user WHERE id = 1 AND status = 'active'"
        );
        assert_eq!(entries[0].params, vec!["1", "'active'"]);
        assert_eq!(entries[0].confidence, Confidence::Exact);
    }

    #[test]
    fn parameterless_statement_closes_on_total() {
        let entries = run(&["==>  Preparing: SELECT COUNT(*) FROM user", "==> Total: 1"]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].sql, "SELECT COUNT(*) FROM user");
        assert_eq!(entries[0].affected_rows, Some(1));
        assert_eq!(entries[0].class, super::super::model::EntryClass::Select);
    }

    #[test]
    fn multiline_sql_is_joined() {
        let entries = run(&[
            "2026-08-15 10:00:00.123 INFO x - ==>  Preparing: SELECT id",
            "  , name FROM user",
            " WHERE id = ?",
            "2026-08-15 10:00:00.124 INFO x - ==> Parameters: 7(Long)",
        ]);
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].sql,
            "SELECT id\n  , name FROM user\n WHERE id = 7"
        );
    }
}
