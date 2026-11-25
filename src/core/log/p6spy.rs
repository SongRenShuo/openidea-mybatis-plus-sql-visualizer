//! p6spy family.
//!
//! The reference repository has no standalone p6spy SQL parser: p6spy appears
//! in `LogDurationParser` (`timestamp|ms|statement|sql`) and
//! `ExecutionTimeParser` (`elapsed N ms`). This module turns those two known
//! shapes into first-class entries:
//!
//! - `1723456789012|123|statement|select * from t where id = 1`
//! - `#1723456789012 | elapsed 5 ms | exec | INSERT INTO ...`
//! - `... | statement | SELECT ... | SELECT ... |` (the 5th field is the
//!   effective SQL with values, used as the restored form)
//!
//! `result` / `resultset` / `commit` / `rollback` rows carry no SQL; their
//! duration, when present, is attached to the adjacent entry.

use super::model::{Confidence, Family, message_keys};
use super::parser::{Emitter, PendingEntry, cap_sql};
use super::text::{classify, starts_with_sql_keyword};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Category {
    Sql,
    Other,
}

pub(crate) fn looks_like_row(line: &str) -> bool {
    if line.matches('|').count() < 2 {
        return false;
    }
    let lower = line.to_ascii_lowercase();
    CATEGORIES.iter().any(|(name, _)| lower.contains(name))
}

const CATEGORIES: &[(&str, Category)] = &[
    ("statement", Category::Sql),
    ("exec", Category::Sql),
    ("batch", Category::Sql),
    ("update", Category::Sql),
    ("resultsets", Category::Other),
    ("resultset", Category::Other),
    ("result", Category::Other),
    ("commit", Category::Other),
    ("rollback", Category::Other),
    ("info", Category::Other),
    ("debug", Category::Other),
    ("error", Category::Other),
];

pub(crate) fn feed(line_no: usize, line: &str, emitter: &mut Emitter<'_>) -> bool {
    if line.matches('|').count() < 2 {
        return false;
    }
    let fields: Vec<&str> = line.split('|').collect();
    let Some((index, category)) = category_index(&fields) else {
        return false;
    };
    let duration = p6spy_duration(&fields[..=index]).or_else(|| super::duration::extract(line));

    if category == Category::Other {
        if let Some(duration) = duration {
            emitter.notice_duration(line_no, duration);
        }
        return true;
    }

    let raw = fields.get(index + 1).map(|s| s.trim()).unwrap_or("");
    if raw.is_empty() {
        if let Some(duration) = duration {
            emitter.notice_duration(line_no, duration);
        }
        return true;
    }
    let effective = fields
        .get(index + 2)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && starts_with_sql_keyword(s));

    let (raw_sql, cut) = cap_sql(raw, emitter.opts.max_sql_len);
    let sql = match effective {
        Some(value) => cap_sql(value, emitter.opts.max_sql_len).0,
        None => raw_sql.clone(),
    };
    let mut message_keys = Vec::new();
    let mut confidence = Confidence::Exact;
    if cut {
        confidence = Confidence::Partial;
        message_keys.push(message_keys::ENTRY_SQL_TRUNCATED.to_string());
    } else if effective.is_none() && raw_sql.contains('?') {
        // p6spy's own parameter row is a separate `result` row; without the
        // effective SQL the placeholders cannot be restored.
        confidence = Confidence::Partial;
        message_keys.push(message_keys::ENTRY_PARAMS_MISSING.to_string());
    }

    let timestamp = super::timestamp::parse_line(line, emitter.opts.timestamp_offset_minutes)
        .or_else(|| epoch_from(&fields[..=index]));

    emitter.emit(PendingEntry {
        family: Family::P6Spy,
        line_no,
        timestamp_ms: timestamp,
        duration_ms: duration,
        raw_sql: raw_sql.clone(),
        sql,
        params: Vec::new(),
        class: classify(&raw_sql),
        confidence,
        affected_rows: None,
        raw_line: (confidence != Confidence::Exact).then(|| line.to_string()),
        message_keys,
    });
    true
}

fn category_index(fields: &[&str]) -> Option<(usize, Category)> {
    for (index, field) in fields.iter().enumerate() {
        let token = field.trim().to_ascii_lowercase();
        if let Some((_, category)) = CATEGORIES.iter().find(|(name, _)| *name == token) {
            return Some((index, *category));
        }
    }
    None
}

/// p6spy duration: `elapsed 5 ms` in one of the leading fields, or the small
/// integer field of `epoch|ms|statement`.
fn p6spy_duration(fields: &[&str]) -> Option<f64> {
    for field in fields {
        if let Some(value) = super::duration::extract(field) {
            return Some(value);
        }
    }
    for field in fields {
        let token = field.trim();
        if token.len() <= 9
            && token.bytes().all(|b| b.is_ascii_digit())
            && !token.is_empty()
            && let Ok(value) = token.parse::<f64>()
        {
            return Some(value);
        }
    }
    None
}

/// Epoch fallback: a 13-digit run (ms) or 10-digit run (seconds).
fn epoch_from(fields: &[&str]) -> Option<i64> {
    for field in fields {
        let bytes = field.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i].is_ascii_digit() {
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                let len = i - start;
                if len == 13 {
                    return field[start..i].parse::<i64>().ok();
                }
                if len == 10 {
                    return field[start..i]
                        .parse::<i64>()
                        .ok()
                        .map(|seconds| seconds * 1000);
                }
            } else {
                i += 1;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::log::model::ParseOptions;
    use crate::core::log::parse;

    #[test]
    fn pipe_row_with_effective_sql() {
        let entries = parse(
            "1723456789012|123|statement|select * from t where id = ?|select * from t where id = 7|",
            &ParseOptions::default(),
        )
        .entries;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].family, Family::P6Spy);
        assert_eq!(entries[0].raw_sql, "select * from t where id = ?");
        assert_eq!(entries[0].sql, "select * from t where id = 7");
        assert_eq!(entries[0].duration_ms, Some(123.0));
        assert_eq!(entries[0].timestamp_ms, Some(1_723_456_789_012));
    }

    #[test]
    fn log_prefixed_elapsed_exec_row() {
        let entries = parse(
            "2026-08-31 17:31:00.123  INFO 1234 --- [pool-2-thread-1] p6spy : #1725000000000 | elapsed 5 ms | exec | INSERT INTO user_order (user_id, amount) VALUES (?, ?)",
            &ParseOptions::default(),
        )
        .entries;
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].sql,
            "INSERT INTO user_order (user_id, amount) VALUES (?, ?)"
        );
        assert_eq!(entries[0].duration_ms, Some(5.0));
        assert_eq!(entries[0].class, super::super::model::EntryClass::Insert);
        assert_eq!(entries[0].confidence, Confidence::Partial);
    }

    #[test]
    fn result_row_only_contributes_duration() {
        let analysis = parse(
            "1723456789012|123|statement|select 1|select 1|\n1723456789013|7|result|",
            &ParseOptions::default(),
        );
        assert_eq!(analysis.entries.len(), 1);
        assert_eq!(analysis.entries[0].duration_ms, Some(123.0));
    }
}
