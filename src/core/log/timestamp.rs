//! Timestamp extraction without a date-time crate (the crate has no chrono and
//! we do not add dependencies).
//!
//! Supports the shapes used by the reference `LogTimestampParser`:
//! - `2024-01-15 10:30:45.123` / `2024-01-15T10:30:45`
//! - `2024-01-15T10:30:45.123+0800` / `...+08:00` / `...Z`
//! - nginx `15/Jan/2024:10:30:45 +0800`
//!
//! Timestamps without an explicit zone use `default_offset_minutes` (east of
//! UTC). The panel passes the machine offset; the default of the parse options
//! is 0 so the function is deterministic in tests and in wasm.

/// Parse the timestamp at the start of `line` (leading whitespace allowed).
pub(crate) fn parse_line(line: &str, default_offset_minutes: i32) -> Option<i64> {
    if let Some(ts) = parse_prefix(line, default_offset_minutes) {
        return Some(ts);
    }
    // Reference strategy 2: the timestamp may sit before a level keyword.
    for level in ["DEBUG", "TRACE", "INFO", "WARN", "ERROR"] {
        if let Some(idx) = line.find(level) {
            if idx == 0 {
                continue;
            }
            if let Some(ts) = parse_prefix(&line[..idx], default_offset_minutes) {
                return Some(ts);
            }
        }
    }
    None
}

fn parse_prefix(text: &str, default_offset_minutes: i32) -> Option<i64> {
    let s = text.trim_start();
    if s.len() < 10 {
        return None;
    }
    if let Some(ts) = parse_nginx(s) {
        return Some(ts);
    }
    parse_iso(s, default_offset_minutes)
}

/// `YYYY-MM-DD[ T]HH:MM:SS[.fff][Z|+HH:MM|+HHMM]`
fn parse_iso(s: &str, default_offset_minutes: i32) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let year = parse_digits(&b[0..4])? as i64;
    if b[4] != b'-' {
        return None;
    }
    let month = parse_digits(&b[5..7])?;
    if b[7] != b'-' {
        return None;
    }
    let day = parse_digits(&b[8..10])?;
    if !(b[10] == b' ' || b[10] == b'T') {
        return None;
    }
    let hour = parse_digits(&b[11..13])?;
    if b[13] != b':' {
        return None;
    }
    let minute = parse_digits(&b[14..16])?;
    if b[16] != b':' {
        return None;
    }
    let second = parse_digits(&b[17..19])?;
    if !valid_civil(year, month, day, hour, minute, second) {
        return None;
    }

    let mut i = 19;
    let mut millis = 0u32;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let start = i;
        let mut value = 0u32;
        let mut digits = 0u32;
        while i < b.len() && b[i].is_ascii_digit() {
            if digits < 3 {
                value = value * 10 + u32::from(b[i] - b'0');
            }
            digits += 1;
            i += 1;
        }
        if i == start {
            return None;
        }
        millis = match digits {
            0 => 0,
            1 => value * 100,
            2 => value * 10,
            _ => value,
        };
    }

    let offset_minutes = if i < b.len() {
        match b[i] {
            b'Z' => 0,
            b'+' | b'-' => parse_zone(&b[i..])?,
            _ => default_offset_minutes,
        }
    } else {
        default_offset_minutes
    };

    Some(to_epoch_ms(
        year,
        month,
        day,
        hour,
        minute,
        second,
        millis,
        offset_minutes,
    ))
}

/// `dd/Mon/yyyy:HH:MM:SS +HHMM`
fn parse_nginx(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 26 || b[2] != b'/' || b[6] != b'/' || b[11] != b':' {
        return None;
    }
    let day = parse_digits(&b[0..2])?;
    let month = match &s[3..6] {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year = parse_digits(&b[7..11])? as i64;
    let hour = parse_digits(&b[12..14])?;
    let minute = parse_digits(&b[15..17])?;
    let second = parse_digits(&b[18..20])?;
    if !valid_civil(year, month, day, hour, minute, second) {
        return None;
    }
    let zone = s.get(21..)?.trim_start();
    let zone_bytes = zone.as_bytes();
    if zone_bytes.is_empty() || (zone_bytes[0] != b'+' && zone_bytes[0] != b'-') {
        return None;
    }
    let offset = parse_zone(zone_bytes)?;
    Some(to_epoch_ms(
        year, month, day, hour, minute, second, 0, offset,
    ))
}

/// `+0800`, `+08:00`; returns minutes east of UTC.
fn parse_zone(b: &[u8]) -> Option<i32> {
    let sign = match b.first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    if b.len() < 5 {
        return None;
    }
    let hh = parse_digits(&b[1..3])? as i32;
    let mm = if b[3] == b':' {
        if b.len() < 6 {
            return None;
        }
        parse_digits(&b[4..6])? as i32
    } else {
        parse_digits(&b[3..5])? as i32
    };
    if hh > 23 || mm > 59 {
        return None;
    }
    Some(sign * (hh * 60 + mm))
}

fn parse_digits(b: &[u8]) -> Option<u32> {
    if b.is_empty() || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut value = 0u32;
    for d in b {
        value = value * 10 + u32::from(d - b'0');
    }
    Some(value)
}

fn valid_civil(year: i64, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> bool {
    if !(1..=12).contains(&month) || day == 0 || hour > 23 || minute > 59 || second > 60 {
        return false;
    }
    day <= days_in_month(year, month)
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        _ => 28,
    }
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

#[allow(clippy::too_many_arguments)]
fn to_epoch_ms(
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    millis: u32,
    offset_minutes: i32,
) -> i64 {
    let days = days_from_civil(year, month, day);
    let seconds =
        days * 86_400 + i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second);
    (seconds - i64::from(offset_minutes) * 60) * 1000 + i64::from(millis)
}

/// Howard Hinnant's civil-from-days inverse; days since 1970-01-01.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = year - if month <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = i64::from((month + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_logback_prefix() {
        let line = "2024-01-15 10:30:45.123 [http] DEBUG c.s.Log - Preparing: select 1";
        assert_eq!(parse_line(line, 480), Some(1_705_285_845_123));
        assert_eq!(parse_line(line, 0), Some(1_705_314_645_123));
    }

    #[test]
    fn parses_iso_zone_variants() {
        assert_eq!(
            parse_line("2024-01-15T10:30:45.123+0800 [main] DEBUG x", 0),
            Some(1_705_285_845_123)
        );
        assert_eq!(
            parse_line("2024-01-15T10:30:45.123+08:00 [main] DEBUG x", 0),
            Some(1_705_285_845_123)
        );
        assert_eq!(
            parse_line("2024-01-15T02:30:45.123Z [main] DEBUG x", 0),
            Some(1_705_285_845_123)
        );
    }

    #[test]
    fn parses_nginx_shape() {
        assert_eq!(
            parse_line("15/Jan/2024:10:30:45 +0800 [main] INFO - x", 0),
            Some(1_705_285_845_000)
        );
    }

    #[test]
    fn rejects_invalid_dates_and_plain_text() {
        assert_eq!(parse_line("2024-02-30 10:00:00 INFO x", 0), None);
        assert_eq!(parse_line("2024-13-01 10:00:00 INFO x", 0), None);
        assert_eq!(parse_line("==>  Preparing: select 1", 0), None);
        assert_eq!(parse_line("", 0), None);
    }
}
