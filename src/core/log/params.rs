//! Parameter parsing and SQL literal rendering.
//!
//! Ported from the reference `MyBatisLogParser.parseParams`, `HibernateLogParser`
//! binding rows, `JdbcTemplateLogParser` parameter rows, `LogSqlRestorer` and
//! `SqlDateLiteral`.

use super::text::{find_ci, is_numeric, quote_sql};

/// One log parameter: value text plus an optional type annotation.
///
/// `value: None` means SQL `NULL`. `ty: None` means "no type annotation"
/// (older/third-party logs) and enables the numeric/boolean heuristic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Param {
    pub value: Option<String>,
    pub ty: Option<String>,
}

impl Param {
    pub(crate) fn null() -> Self {
        Self {
            value: None,
            ty: None,
        }
    }
}

/// Parse a MyBatis `Parameters:` payload: `1(Long), abc(String), null`.
///
/// Values may contain commas and parentheses (`a, b(String)`, `c(x)(String)`),
/// so the scanner takes the earliest `(TYPE)` that is followed by a comma or
/// the end of the text, mirroring the reference non-greedy regex.
pub(crate) fn parse_mybatis(text: &str) -> Vec<Param> {
    let mut out = Vec::new();
    let mut rest = text.trim();
    loop {
        rest = rest.trim_start_matches([',', ' ', '\t']);
        if rest.is_empty() {
            break;
        }
        if is_null_literal(rest) {
            out.push(Param::null());
            rest = skip_null_literal(rest);
            continue;
        }
        match find_typed(rest) {
            Some((value, ty, consumed)) => {
                out.push(Param {
                    value: if value.is_empty() {
                        None
                    } else {
                        Some(value.to_string())
                    },
                    ty: Some(ty.to_string()),
                });
                rest = &rest[consumed..];
            }
            None => break,
        }
    }
    if out.is_empty() && !text.trim().is_empty() {
        // No `(TYPE)` annotations at all (older or third-party logs): split on
        // commas, exactly like the reference fallback.
        for part in text.split(',') {
            let v = part.trim();
            if v.is_empty() {
                out.push(Param::null());
            } else {
                out.push(Param {
                    value: Some(v.to_string()),
                    ty: None,
                });
            }
        }
    }
    out
}

fn is_null_literal(rest: &str) -> bool {
    let b = rest.as_bytes();
    let Some(head) = rest.get(..4) else {
        return false;
    };
    if b.len() < 4 || !head.eq_ignore_ascii_case("null") {
        return false;
    }
    let mut i = 4;
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    i == b.len() || b[i] == b','
}

fn skip_null_literal(rest: &str) -> &str {
    let b = rest.as_bytes();
    let mut i = 4;
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    if i < b.len() && b[i] == b',' {
        i += 1;
    }
    &rest[i..]
}

/// Find the earliest `value(TYPE)` whose closing paren is followed by a comma
/// or end of text. Returns `(value, type, bytes_consumed)`.
fn find_typed(rest: &str) -> Option<(&str, &str, usize)> {
    let b = rest.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'('
            && let Some(type_end) = read_type(rest, i + 1)
        {
            let mut j = type_end + 1;
            while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
                j += 1;
            }
            if j == b.len() || b[j] == b',' {
                let value = rest[..i].trim();
                let ty = &rest[i + 1..type_end];
                let consumed = if j < b.len() { j + 1 } else { b.len() };
                return Some((value, ty, consumed));
            }
        }
        i += 1;
    }
    None
}

/// Read a Java type token (`Long`, `java.lang.Long`, `byte[]`) and return the
/// index of its closing `)`.
fn read_type(text: &str, from: usize) -> Option<usize> {
    let b = text.as_bytes();
    let mut i = from;
    if i >= b.len() {
        return None;
    }
    let first = b[i];
    if !(first.is_ascii_alphabetic() || first == b'_' || first == b'$') {
        return None;
    }
    i += 1;
    while i < b.len()
        && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$' || b[i] == b'.')
    {
        i += 1;
    }
    while i + 1 < b.len() && b[i] == b'[' && b[i + 1] == b']' {
        i += 2;
    }
    if i < b.len() && b[i] == b')' {
        Some(i)
    } else {
        None
    }
}

/// Parse a Hibernate binding row with the configured label — the only entry
/// point now that `log.hibernateParamPrefix` is a setting:
/// `binding parameter [1] as [BIGINT] - [1]` → `(1, Param)`.
pub(crate) fn parse_binding_with(line: &str, prefix_lower: &str) -> Option<(usize, Param)> {
    let start = find_ci(line, prefix_lower)?;
    let tail = &line[start..];
    let open = tail.find('[')?;
    let close = open + tail[open..].find(']')?;
    let index = tail[open + 1..close].trim().parse::<usize>().ok()?;

    let as_at = find_ci(&tail[close..], " as ")? + close;
    let type_open = as_at + tail[as_at..].find('[')?;
    let type_close = type_open + tail[type_open..].find(']')?;
    let ty = tail[type_open + 1..type_close].trim().to_string();

    let dash = tail[type_close..].find(" - [")? + type_close;
    let value_start = dash + 4;
    let value_end = tail.rfind(']')?;
    if value_end < value_start {
        return None;
    }
    let raw = &tail[value_start..value_end];
    let param = if raw.eq_ignore_ascii_case("null") {
        Param {
            value: None,
            ty: if ty.is_empty() { None } else { Some(ty) },
        }
    } else {
        Param {
            value: Some(raw.to_string()),
            ty: if ty.is_empty() { None } else { Some(ty) },
        }
    };
    Some((index, param))
}

/// Parse a JdbcTemplate parameter row:
/// `Setting SQL statement parameter value: column index 1, parameter value [0],
/// value class [java.lang.Integer], SQL type unknown` → `(1, Param)`.
pub(crate) fn parse_jdbc_setting(line: &str) -> Option<(usize, Param)> {
    let begin = find_ci(line, "setting sql statement parameter value:")?;
    let tail = &line[begin..];
    let idx_at = find_ci(tail, "column index ")?;
    let num_start = idx_at + "column index ".len();
    let num_end = tail[num_start..]
        .find(|c: char| !c.is_ascii_digit())
        .map(|n| num_start + n)
        .unwrap_or(tail.len());
    let index = tail[num_start..num_end].parse::<usize>().ok()?;

    let pv_at = find_ci(tail, "parameter value [")?;
    let pv_start = pv_at + "parameter value [".len();
    let class_at = tail.rfind("], value class [")?;
    if class_at < pv_start {
        return None;
    }
    let raw_value = &tail[pv_start..class_at];

    let class_start = class_at + "], value class [".len();
    let class_end = class_start + tail[class_start..].find(']')?;
    let class_name = tail[class_start..class_end].trim();

    if raw_value == "null" {
        return Some((index, Param::null()));
    }
    let ty = map_class_name(class_name);
    Some((
        index,
        Param {
            value: Some(raw_value.to_string()),
            ty,
        },
    ))
}

/// `java.lang.Integer` → `Integer`; `null` → no type.
pub(crate) fn map_class_name(class_name: &str) -> Option<String> {
    if class_name.is_empty() || class_name.eq_ignore_ascii_case("null") {
        return None;
    }
    let mut s = class_name;
    for prefix in [
        "java.lang.",
        "java.math.",
        "java.util.",
        "java.sql.",
        "java.time.",
    ] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest;
            break;
        }
    }
    Some(s.to_string())
}

/// Split a NamedParameterJdbcTemplate map body (`a=1, b=x,y`) on `, key=`
/// boundaries; values may contain commas and `=`.
pub(crate) fn parse_named_map(body: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let bytes = body.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b',' && key_equals_follows(body, i + 1) {
            push_named_pair(&mut out, &body[start..i]);
            start = i + 1;
        }
        i += 1;
    }
    push_named_pair(&mut out, &body[start..]);
    out
}

fn key_equals_follows(body: &str, mut i: usize) -> bool {
    let b = body.as_bytes();
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    let key_start = i;
    while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
        i += 1;
    }
    if i == key_start {
        return false;
    }
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    i < b.len() && b[i] == b'='
}

fn push_named_pair(out: &mut Vec<(String, String)>, chunk: &str) {
    let chunk = chunk.trim();
    if chunk.is_empty() {
        return;
    }
    if let Some(eq) = chunk.find('=') {
        let key = chunk[..eq].trim();
        let value = chunk[eq + 1..].trim();
        if !key.is_empty() {
            out.push((key.to_string(), value.to_string()));
        }
    }
}

/// Render one parameter as a SQL literal (reference `LogSqlRestorer.renderParam`).
pub(crate) fn render_param(param: &Param) -> String {
    let Some(value) = param.value.as_deref() else {
        return "NULL".to_string();
    };
    let Some(ty) = param.ty.as_deref() else {
        return render_untyped(value);
    };
    if let Some(lit) = render_temporal(value, ty) {
        return lit;
    }
    if is_plain_type(ty) {
        value.to_string()
    } else {
        quote_sql(value)
    }
}

/// Render an untyped value: numbers and booleans stay bare, `null` becomes
/// `NULL`, everything else is quoted.
pub(crate) fn render_untyped(value: &str) -> String {
    if value.eq_ignore_ascii_case("null") {
        return "NULL".to_string();
    }
    if is_numeric(value)
        || value.eq_ignore_ascii_case("true")
        || value.eq_ignore_ascii_case("false")
    {
        value.to_string()
    } else {
        quote_sql(value)
    }
}

fn simple_type(ty: &str) -> &str {
    match ty.rfind('.') {
        Some(i) => &ty[i + 1..],
        None => ty,
    }
}

/// Types rendered without quotes (reference `LogSqlRestorer.PLAIN_TYPES`).
fn is_plain_type(ty: &str) -> bool {
    matches!(
        simple_type(ty),
        "Long"
            | "Integer"
            | "Short"
            | "Byte"
            | "BigDecimal"
            | "BigInteger"
            | "Double"
            | "Float"
            | "Number"
            | "long"
            | "int"
            | "short"
            | "byte"
            | "double"
            | "float"
            | "Boolean"
            | "boolean"
            | "BIGINT"
            | "INTEGER"
            | "SMALLINT"
            | "TINYINT"
            | "FLOAT"
            | "DOUBLE"
            | "NUMERIC"
            | "DECIMAL"
            | "BIT"
            | "BOOLEAN"
            | "REAL"
    )
}

/// ANSI typed literal for the date family (reference `SqlDateLiteral.renderFor`).
///
/// `LocalTime` / `Time` / offset-aware values are deliberately not wrapped:
/// ANSI `TIME` is not portable and ANSI `TIMESTAMP` rejects zone suffixes.
fn render_temporal(value: &str, ty: &str) -> Option<String> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    match simple_type(ty) {
        "LocalDate" | "DATE" => is_date_shape(v).then(|| format!("DATE '{v}'")),
        "Date" => {
            if is_date_shape(v) {
                Some(format!("DATE '{v}'"))
            } else {
                is_timestamp_shape(v).then(|| format!("TIMESTAMP '{}'", normalize_timestamp(v)))
            }
        }
        "Timestamp" | "LocalDateTime" | "TIMESTAMP" | "DATETIME" => {
            if is_timestamp_shape(v) {
                Some(format!("TIMESTAMP '{}'", normalize_timestamp(v)))
            } else if is_date_shape(v) {
                Some(format!("DATE '{v}'"))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn is_date_shape(v: &str) -> bool {
    let b = v.as_bytes();
    b.len() == 10
        && b[0..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
}

fn is_timestamp_shape(v: &str) -> bool {
    let b = v.as_bytes();
    if b.len() < 16 {
        return false;
    }
    if !is_date_shape(&v[..10]) || (b[10] != b' ' && b[10] != b'T') {
        return false;
    }
    if !(b[11..13].iter().all(u8::is_ascii_digit)
        && b[13] == b':'
        && b[14..16].iter().all(u8::is_ascii_digit))
    {
        return false;
    }
    let mut i = 16;
    if i < b.len() && b[i] == b':' {
        if i + 2 >= b.len() || !b[i + 1..i + 3].iter().all(u8::is_ascii_digit) {
            return false;
        }
        i += 3;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == start || i - start > 9 {
            return false;
        }
    }
    i == b.len()
}

fn normalize_timestamp(v: &str) -> String {
    let mut out = if v.len() > 10 && v.as_bytes()[10] == b'T' {
        format!("{} {}", &v[..10], &v[11..])
    } else {
        v.to_string()
    };
    // `yyyy-MM-dd HH:mm` → append `:00` (ANSI literals need seconds).
    if out.len() == 16 {
        out.push_str(":00");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mybatis_params_with_commas_and_types() {
        let params = parse_mybatis("a, b(String), c(x)(String)");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].value.as_deref(), Some("a, b"));
        assert_eq!(params[0].ty.as_deref(), Some("String"));
        assert_eq!(params[1].value.as_deref(), Some("c(x)"));
    }

    #[test]
    fn mybatis_null_literal_in_the_middle() {
        let params = parse_mybatis("1(Long), null, abc(String)");
        assert_eq!(params.len(), 3);
        assert!(params[1].value.is_none());
        assert_eq!(render_param(&params[0]), "1");
        assert_eq!(render_param(&params[1]), "NULL");
        assert_eq!(render_param(&params[2]), "'abc'");
    }

    #[test]
    fn mybatis_fully_qualified_type_and_arrays() {
        let params = parse_mybatis("42(java.lang.Long), bytes(byte[])");
        assert_eq!(params.len(), 2);
        assert_eq!(render_param(&params[0]), "42");
        assert_eq!(render_param(&params[1]), "'bytes'");
    }

    #[test]
    fn fallback_without_type_annotations() {
        let params = parse_mybatis("1, abc");
        assert_eq!(render_param(&params[0]), "1");
        assert_eq!(render_param(&params[1]), "'abc'");
    }
}
