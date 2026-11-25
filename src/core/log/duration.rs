//! Execution-time extraction (reference `ExecutionTimeParser`).
//!
//! Recognised shapes, checked in this order:
//! 1. p6spy `elapsed 5 ms` / `consumed 456ms`
//! 2. MyBatis-Plus `TimeCount: 789ms`
//! 3. generic `time: 100 ms` / `time=100ms`
//! 4. druid `executeMillis:123` / `"executeMillis":123`
//! 5. MyBatis-Plus `ExecuteTime: 456ms`
//! 6. trailing `789ms`
//!
//! All matchers are manual scans; the lower-case copy is only built after a
//! cheap keyword pre-check so the 100k-line path stays allocation-light.

/// Extract a duration in milliseconds from one log line.
pub(crate) fn extract(text: &str) -> Option<f64> {
    if text.len() < 3 {
        return None;
    }
    let quick = text.contains("ms")
        || text.contains("elapsed")
        || text.contains("Elapsed")
        || text.contains("consumed")
        || text.contains("TimeCount")
        || text.contains("executeMillis")
        || text.contains("ExecuteTime")
        || text.contains("time:")
        || text.contains("time=")
        || text.contains("TIME:")
        || text.contains("Time:");
    if !quick {
        return None;
    }
    let lower = text.to_ascii_lowercase();

    keyword_ms(&lower, "elapsed")
        .or_else(|| keyword_ms(&lower, "consumed"))
        .or_else(|| labelled_number(&lower, "timecount", true))
        .or_else(|| generic_time(&lower))
        .or_else(|| labelled_number(&lower, "executemillis", false))
        .or_else(|| labelled_number(&lower, "executetime", false))
        .or_else(|| trailing_ms(&lower))
}

/// `elapsed 5 ms` / `consumed: 456ms` — requires an `ms` unit.
fn keyword_ms(lower: &str, keyword: &str) -> Option<f64> {
    let mut from = 0;
    while let Some(rel) = lower[from..].find(keyword) {
        let start = from + rel;
        let before_ok = start == 0
            || !lower.as_bytes()[start - 1].is_ascii_alphanumeric()
                && lower.as_bytes()[start - 1] != b'_';
        if before_ok && let Some((value, rest)) = number_after(lower, start + keyword.len()) {
            let tail = &lower[rest..];
            let tail = tail.trim_start();
            if tail.starts_with("ms") {
                return Some(value);
            }
        }
        from = start + keyword.len();
    }
    None
}

/// `TimeCount: 789ms`, `ExecuteTime=789`, `executeMillis:123`.
fn labelled_number(lower: &str, label: &str, require_ms: bool) -> Option<f64> {
    let mut from = 0;
    while let Some(rel) = lower[from..].find(label) {
        let start = from + rel;
        let before_ok = start == 0
            || !lower.as_bytes()[start - 1].is_ascii_alphanumeric()
                && lower.as_bytes()[start - 1] != b'_';
        if before_ok {
            let after = start + label.len();
            let mut i = after;
            let b = lower.as_bytes();
            while i < b.len() && (b[i] == b' ' || b[i] == b'\'' || b[i] == b'"') {
                i += 1;
            }
            if i < b.len() && (b[i] == b':' || b[i] == b'=') {
                i += 1;
                while i < b.len() && b[i] == b' ' {
                    i += 1;
                }
                if let Some((value, rest)) = number_at(lower, i)
                    && (!require_ms || lower[rest..].trim_start().starts_with("ms"))
                {
                    return Some(value);
                }
            }
        }
        from = start + label.len();
    }
    None
}

/// `time: 100 ms` / `time=200ms` (word boundary, unit required).
fn generic_time(lower: &str) -> Option<f64> {
    let mut from = 0;
    while let Some(rel) = lower[from..].find("time") {
        let start = from + rel;
        let before_ok = start == 0
            || !lower.as_bytes()[start - 1].is_ascii_alphanumeric()
                && lower.as_bytes()[start - 1] != b'_';
        // Exclude the longer labels handled above.
        let tail = &lower[start + 4..];
        let tail_trim = tail.trim_start();
        let separator = tail_trim.starts_with(':') || tail_trim.starts_with('=');
        if before_ok && separator {
            let mut i = start + 4;
            let b = lower.as_bytes();
            while i < b.len() && b[i] == b' ' {
                i += 1;
            }
            if i < b.len() && (b[i] == b':' || b[i] == b'=') {
                i += 1;
                while i < b.len() && b[i] == b' ' {
                    i += 1;
                }
                if let Some((value, rest)) = number_at(lower, i)
                    && lower[rest..].trim_start().starts_with("ms")
                {
                    return Some(value);
                }
            }
        }
        from = start + 4;
    }
    None
}

/// `... ] 789ms` — number immediately followed by `ms` at end of line.
fn trailing_ms(lower: &str) -> Option<f64> {
    let t = lower.trim_end();
    if !t.ends_with("ms") {
        return None;
    }
    let b = t.as_bytes();
    let mut i = b.len() - 2;
    while i > 0 && b[i - 1] == b' ' {
        i -= 1;
    }
    let end = i;
    let mut start = end;
    while start > 0 && (b[start - 1].is_ascii_digit() || b[start - 1] == b'.') {
        start -= 1;
    }
    if start == end {
        return None;
    }
    let value = t[start..end].parse::<f64>().ok()?;
    // Require a delimiter before the number so `abc123ms` is not a duration.
    if start > 0 {
        let prev = b[start - 1];
        if prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'.' {
            return None;
        }
    }
    Some(value)
}

fn number_after(text: &str, from: usize) -> Option<(f64, usize)> {
    let b = text.as_bytes();
    let mut i = from;
    while i < b.len() && (b[i] == b' ' || b[i] == b':') {
        i += 1;
    }
    number_at(text, i)
}

fn number_at(text: &str, from: usize) -> Option<(f64, usize)> {
    let b = text.as_bytes();
    let mut i = from;
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        i += 1;
    }
    let start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == start {
        return None;
    }
    if i < b.len() && b[i] == b'.' {
        let mut j = i + 1;
        let frac_start = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > frac_start {
            i = j;
        }
    }
    let value = text[from..i].parse::<f64>().ok()?;
    Some((value, i))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p6spy_elapsed_and_consumed() {
        assert_eq!(extract("elapsed 5 ms"), Some(5.0));
        assert_eq!(
            extract(
                "2026-08-31 17:31:00.123 INFO x : #1725000000000 | elapsed 5 ms | statement | select 1"
            ),
            Some(5.0)
        );
        assert_eq!(extract("consumed 456ms"), Some(456.0));
    }

    #[test]
    fn mybatis_plus_shapes() {
        assert_eq!(extract("TimeCount: 789ms"), Some(789.0));
        assert_eq!(extract("ExecuteTime=1234"), Some(1234.0));
        assert_eq!(extract("\"executeMillis\":456"), Some(456.0));
    }

    #[test]
    fn generic_and_trailing() {
        assert_eq!(extract("time: 100 ms"), Some(100.0));
        assert_eq!(extract("select 1 ] 789ms"), Some(789.0));
    }

    #[test]
    fn ignores_plain_sql_and_identifiers() {
        assert_eq!(extract("select * from t where name = 'ms'"), None);
        assert_eq!(extract("update t set runtime = 1"), None);
        assert_eq!(extract(""), None);
    }
}
