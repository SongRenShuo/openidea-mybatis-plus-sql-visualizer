//! Parameter binding (`?` / `:name`, typing, LIKE, ANSI dates) — T1-7.
//!
//! Behavioural port of the IntelliJ edition's `ParameterReplacer.kt` +
//! `ParamQuote.kt` + `PreQuotedLiteral.kt` + `SqlDateLiteral.kt` for the pure
//! binding step. Reference sources are read-only behaviour baselines; this is a
//! Rust rewrite.
//!
//! ## Input model
//!
//! `params: &[serde_json::Value]` carries typed values:
//! - native JSON: number → numeric literal, bool → `TRUE`/`FALSE`, null →
//!   `NULL`, string → single-quoted literal, array → comma-joined literal list
//!   (for `IN (...)`);
//! - typed objects for types JSON cannot express:
//!   - `{"$date": "2026-11-07"}` / `{"type":"date","value":"…"}`,
//!   - `{"$timestamp": "2026-11-07T13:20:14"}` / `{"type":"timestamp", …}`,
//!   - `{"$bytes": [1,2,3]}` or `{"$bytes":"0A1B"}` (hex),
//!   - `{"type":"number"|"string"|"bool"|"null", "value": …}`.
//! - named binding: an object carrying `"name"` (and `"value"`) binds that SQL
//!   `:name`; positional `?` still consumes `params` in order.
//!
//! ## Semantics carried over from the reference
//! - pre-quoted string (`'…'`) is emitted verbatim — never double-escaped;
//! - `'` inside a string is escaped to `''`;
//! - date/timestamp → ANSI `DATE '…'` / `TIMESTAMP '…'` when the dialect
//!   supports it (SQL Server / SQLite fall back to a quoted string);
//! - LIKE patterns are strings (always quoted); `%` / `_` are not escaped.
//!
//! Unknown value shapes and missing parameters degrade to `partial` and keep a
//! placeholder; `unresolved` (no SQL) is not produced by `bind`.

use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;

use crate::core::dialect::Dialect;

/// Result confidence. `unresolved` exists for API symmetry; `bind` never
/// emits it (it always returns some SQL, possibly degraded to `partial`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Exact,
    Partial,
    Unresolved,
}

/// A resolved parameter value, recorded in [`BoundSql::params`].
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum SqlLiteral {
    Number(String),
    Bool(bool),
    Null,
    Str(String),
    Date(String),
    Timestamp(String),
    Bytes(Vec<u8>),
    List(Vec<SqlLiteral>),
    /// A value whose type could not be determined (degraded to partial).
    Unknown(Value),
}

/// Binding result.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BoundSql {
    pub sql: String,
    pub params: Vec<SqlLiteral>,
    pub confidence: Confidence,
    pub warnings: Vec<String>,
    pub message_keys: Vec<String>,
}

/// Per-call mutable state collecting confidence / message keys.
struct Ctx<'a> {
    dialect: &'a Dialect,
    confidence: Confidence,
    message_keys: Vec<String>,
    warnings: Vec<String>,
}

impl Ctx<'_> {
    /// Downgrade to `partial` and record a message key (deduplicated).
    fn degrade(&mut self, key: &str) {
        if self.confidence == Confidence::Exact {
            self.confidence = Confidence::Partial;
        }
        if !self.message_keys.iter().any(|k| k == key) {
            self.message_keys.push(key.to_string());
        }
    }

    /// Record an informational message key without downgrading confidence.
    fn note(&mut self, key: &str) {
        if !self.message_keys.iter().any(|k| k == key) {
            self.message_keys.push(key.to_string());
        }
    }
}

/// Bind positional `?` and named `:name` parameters into `sql` for `dialect`.
pub fn bind(sql: &str, params: &[Value], dialect: &Dialect) -> BoundSql {
    let mut ctx = Ctx {
        dialect,
        confidence: Confidence::Exact,
        message_keys: Vec::new(),
        warnings: Vec::new(),
    };

    // Map `:name` → value (only objects carrying a `"name"` string).
    let mut named: HashMap<&str, &Value> = HashMap::new();
    for p in params {
        if let Value::Object(map) = p {
            if let Some(Value::String(n)) = map.get("name") {
                named.insert(n.as_str(), p);
            }
        }
    }

    let chars: Vec<char> = sql.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(sql.len() + 32);
    let mut bound: Vec<SqlLiteral> = Vec::new();
    let mut pos_idx = 0usize;
    let mut i = 0usize;
    let mut in_string = false;

    while i < n {
        let c = chars[i];

        // Inside a single-quoted string literal: only `''` escaping matters.
        if in_string {
            if c == '\'' {
                if i + 1 < n && chars[i + 1] == '\'' {
                    out.push('\'');
                    out.push('\'');
                    i += 2;
                    continue;
                }
                in_string = false;
            }
            out.push(c);
            i += 1;
            continue;
        }

        // Line comment `-- … EOL`.
        if c == '-' && i + 1 < n && chars[i + 1] == '-' {
            while i < n && chars[i] != '\n' {
                out.push(chars[i]);
                i += 1;
            }
            continue;
        }

        // Block comment `/* … */`.
        if c == '/' && i + 1 < n && chars[i + 1] == '*' {
            out.push('/');
            out.push('*');
            i += 2;
            while i < n {
                if chars[i] == '*' && i + 1 < n && chars[i + 1] == '/' {
                    out.push('*');
                    out.push('/');
                    i += 2;
                    break;
                }
                out.push(chars[i]);
                i += 1;
            }
            continue;
        }

        if c == '\'' {
            in_string = true;
            out.push(c);
            i += 1;
            continue;
        }

        // Positional placeholder.
        if c == '?' {
            if pos_idx < params.len() {
                let (lit, rendered) = render_literal(&params[pos_idx], &mut ctx);
                bound.push(lit);
                out.push_str(&rendered);
            } else {
                ctx.degrade("params.missing_positional");
                out.push('?');
            }
            pos_idx += 1;
            i += 1;
            continue;
        }

        // Named placeholder `:name` (skip `::` cast; requires named params).
        if c == ':' && (i == 0 || chars[i - 1] != ':') && !named.is_empty() {
            let mut j = i + 1;
            while j < n && chars[j].is_whitespace() {
                j += 1;
            }
            let start = j;
            if j < n && (chars[j].is_alphabetic() || chars[j] == '_') {
                while j < n && (chars[j].is_alphanumeric() || chars[j] == '_') {
                    j += 1;
                }
                let name: String = chars[start..j].iter().collect();
                if let Some(v) = named.get(name.as_str()) {
                    let (lit, rendered) = render_literal(v, &mut ctx);
                    bound.push(lit);
                    out.push_str(&rendered);
                } else {
                    // Keep the original `: name` span, degrade.
                    ctx.degrade("params.missing_named");
                    for ch in &chars[i..j] {
                        out.push(*ch);
                    }
                }
                i = j;
                continue;
            }
        }

        out.push(c);
        i += 1;
    }

    BoundSql {
        sql: out,
        params: bound,
        confidence: ctx.confidence,
        warnings: ctx.warnings,
        message_keys: ctx.message_keys,
    }
}

/// Resolve one JSON value to `(SqlLiteral, rendered SQL)`.
fn render_literal(v: &Value, ctx: &mut Ctx) -> (SqlLiteral, String) {
    use Value::*;

    // Typed wrappers (objects).
    if let Object(map) = v {
        if let Some(inner) = map.get("$date") {
            return render_temporal(inner, TemporalKind::Date, ctx);
        }
        if let Some(inner) = map.get("$timestamp") {
            return render_temporal(inner, TemporalKind::Timestamp, ctx);
        }
        if let Some(inner) = map.get("$bytes") {
            return render_bytes(inner, ctx);
        }
        if let Some(Value::String(t)) = map.get("type") {
            let raw = map.get("value").cloned().unwrap_or(Value::Null);
            return match t.as_str() {
                "date" => render_temporal(&raw, TemporalKind::Date, ctx),
                "timestamp" => render_temporal(&raw, TemporalKind::Timestamp, ctx),
                "bytes" => render_bytes(&raw, ctx),
                "number" => render_number(&raw, ctx),
                "string" | "text" => render_string(&raw, ctx),
                "bool" | "boolean" => render_bool(&raw, ctx),
                "null" => (SqlLiteral::Null, "NULL".to_string()),
                _ => {
                    ctx.degrade("params.unknown_type");
                    render_fallback_string(&raw, ctx)
                }
            };
        }
        // Named wrapper `{"name": …, "value": …}` → bind its value.
        if map.contains_key("name") {
            if let Some(raw) = map.get("value") {
                return render_literal(raw, ctx);
            }
        }
        // Unrecognised object.
        ctx.degrade("params.unknown_type");
        let text = v.to_string();
        return (SqlLiteral::Unknown(v.clone()), quote_str(&text));
    }

    match v {
        Null => (SqlLiteral::Null, "NULL".to_string()),
        Bool(b) => render_bool_value(*b, ctx),
        Number(num) => (SqlLiteral::Number(num.to_string()), num.to_string()),
        String(s) => {
            let (lit, rendered) = render_plain_string(s);
            (lit, rendered)
        }
        Array(items) => {
            let mut lits = Vec::with_capacity(items.len());
            let mut parts = Vec::with_capacity(items.len());
            for it in items {
                let (l, r) = render_literal(it, ctx);
                lits.push(l);
                parts.push(r);
            }
            (SqlLiteral::List(lits), parts.join(", "))
        }
        Object(_) => unreachable!("handled above"),
    }
}

fn render_number(v: &Value, ctx: &mut Ctx) -> (SqlLiteral, String) {
    match v {
        Value::Number(num) => (SqlLiteral::Number(num.to_string()), num.to_string()),
        Value::String(s) if is_numeric(s.trim()) => (
            SqlLiteral::Number(s.trim().to_string()),
            s.trim().to_string(),
        ),
        other => {
            ctx.degrade("params.unknown_type");
            render_fallback_string(other, ctx)
        }
    }
}

fn render_bool(v: &Value, ctx: &mut Ctx) -> (SqlLiteral, String) {
    match v {
        Value::Bool(b) => render_bool_value(*b, ctx),
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" => render_bool_value(true, ctx),
            "false" => render_bool_value(false, ctx),
            "1" => render_bool_value(true, ctx),
            "0" => render_bool_value(false, ctx),
            _ => {
                ctx.degrade("params.unknown_type");
                (SqlLiteral::Str(s.clone()), quote_str(s))
            }
        },
        other => {
            ctx.degrade("params.unknown_type");
            render_fallback_string(other, ctx)
        }
    }
}

fn render_bool_value(b: bool, ctx: &mut Ctx) -> (SqlLiteral, String) {
    if ctx.dialect.capabilities.supports_boolean_literal {
        (
            SqlLiteral::Bool(b),
            if b {
                "TRUE".to_string()
            } else {
                "FALSE".to_string()
            },
        )
    } else {
        // Oracle / DM have no boolean literal — emit 1/0 but disclose it.
        ctx.degrade("params.boolean_literal_unsupported");
        (
            SqlLiteral::Bool(b),
            if b { "1".to_string() } else { "0".to_string() },
        )
    }
}

fn render_string(v: &Value, ctx: &mut Ctx) -> (SqlLiteral, String) {
    match v {
        Value::String(s) => render_plain_string(s),
        other => {
            ctx.degrade("params.unknown_type");
            render_fallback_string(other, ctx)
        }
    }
}

/// Render an already-validated string value (native or via typed wrapper).
fn render_plain_string(s: &str) -> (SqlLiteral, String) {
    let trimmed = s.trim();
    if is_complete_pre_quoted(trimmed) {
        // User already supplied a complete quoted literal — never re-escape.
        return (SqlLiteral::Str(trimmed.to_string()), trimmed.to_string());
    }
    (SqlLiteral::Str(s.to_string()), quote_str(s))
}

/// Fallback for a value we cannot type: render its JSON text as a string literal.
fn render_fallback_string(v: &Value, _ctx: &mut Ctx) -> (SqlLiteral, String) {
    let text = match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    (SqlLiteral::Unknown(v.clone()), quote_str(&text))
}

enum TemporalKind {
    Date,
    Timestamp,
}

fn render_temporal(v: &Value, kind: TemporalKind, ctx: &mut Ctx) -> (SqlLiteral, String) {
    let raw = match v {
        Value::String(s) => s.trim().to_string(),
        other => {
            ctx.degrade("params.unknown_type");
            return render_fallback_string(other, ctx);
        }
    };
    if raw.is_empty() {
        ctx.degrade("params.unknown_date");
        return (SqlLiteral::Str(raw.clone()), quote_str(&raw));
    }

    let ansi = ctx.dialect.capabilities.supports_ansi_date_literal;

    // Decide whether the text is a pure date or a timestamp shape.
    let is_date = is_date_shape(&raw);
    let is_ts = is_ts_shape(&raw);

    if !ansi {
        // SQL Server / SQLite: ANSI typed literal unsupported → quoted string.
        ctx.note("params.date_quoted_fallback");
        let lit = if is_ts {
            SqlLiteral::Timestamp(raw.clone())
        } else {
            SqlLiteral::Date(raw.clone())
        };
        return (lit, quote_str(&raw));
    }

    // Prefer the requested kind, but accept the other shape (reference D4:
    // TIMESTAMP-family accepts a pure date → DATE literal, and vice versa).
    match kind {
        TemporalKind::Date => {
            if is_date {
                (SqlLiteral::Date(raw.clone()), format!("DATE '{raw}'"))
            } else if is_ts {
                let norm = normalize_ts(&raw);
                (
                    SqlLiteral::Timestamp(norm.clone()),
                    format!("TIMESTAMP '{norm}'"),
                )
            } else {
                ctx.degrade("params.unknown_date");
                (SqlLiteral::Str(raw.clone()), quote_str(&raw))
            }
        }
        TemporalKind::Timestamp => {
            if is_ts {
                let norm = normalize_ts(&raw);
                (
                    SqlLiteral::Timestamp(norm.clone()),
                    format!("TIMESTAMP '{norm}'"),
                )
            } else if is_date {
                (SqlLiteral::Date(raw.clone()), format!("DATE '{raw}'"))
            } else {
                ctx.degrade("params.unknown_date");
                (SqlLiteral::Str(raw.clone()), quote_str(&raw))
            }
        }
    }
}

fn render_bytes(v: &Value, ctx: &mut Ctx) -> (SqlLiteral, String) {
    match parse_bytes(v) {
        Some(bytes) => {
            let hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
            (SqlLiteral::Bytes(bytes), format!("X'{hex}'"))
        }
        None => {
            ctx.degrade("params.bytes_invalid");
            render_fallback_string(v, ctx)
        }
    }
}

fn parse_bytes(v: &Value) -> Option<Vec<u8>> {
    match v {
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                let n = it.as_u64()?;
                if n > 255 {
                    return None;
                }
                out.push(n as u8);
            }
            Some(out)
        }
        Value::String(s) => {
            let t = s.trim();
            let t = t
                .strip_prefix("0x")
                .or_else(|| t.strip_prefix("0X"))
                .unwrap_or(t);
            if t.is_empty() || t.len() % 2 != 0 {
                return None;
            }
            let bs = t.as_bytes();
            let mut out = Vec::with_capacity(bs.len() / 2);
            let mut i = 0;
            while i < bs.len() {
                let hi = hex_val(bs[i])?;
                let lo = hex_val(bs[i + 1])?;
                out.push(hi * 16 + lo);
                i += 2;
            }
            Some(out)
        }
        _ => None,
    }
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// ------------------------------------------------------------------
// Lexical helpers (no regex crate: hand-rolled shape checks)
// ------------------------------------------------------------------

/// `'…'` complete literal: trim, length ≥ 2, starts and ends with `'`.
fn is_complete_pre_quoted(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2 && b[0] == b'\'' && b[b.len() - 1] == b'\''
}

/// Single-quote a value, escaping interior `'` to `''`.
fn quote_str(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn is_numeric(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let b = s.as_bytes();
    let mut i = 0;
    if b[0] == b'-' || b[0] == b'+' {
        i = 1;
    }
    let mut digits = 0;
    let mut dot = false;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_digit() {
            digits += 1;
        } else if c == b'.' && !dot {
            dot = true;
        } else {
            return false;
        }
        i += 1;
    }
    digits > 0
}

fn is_date_shape(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b[..4].iter().all(|c| c.is_ascii_digit())
        && b[5..7].iter().all(|c| c.is_ascii_digit())
        && b[8..10].iter().all(|c| c.is_ascii_digit())
}

fn is_ts_shape(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 16 {
        return false;
    }
    if !is_date_shape(&s[..10]) {
        return false;
    }
    if b[10] != b' ' && b[10] != b'T' {
        return false;
    }
    if !(b[11].is_ascii_digit()
        && b[12].is_ascii_digit()
        && b[13] == b':'
        && b[14].is_ascii_digit()
        && b[15].is_ascii_digit())
    {
        return false;
    }
    let mut i = 16;
    if i + 2 < b.len() && b[i] == b':' {
        if !(b[i + 1].is_ascii_digit() && b[i + 2].is_ascii_digit()) {
            return false;
        }
        i += 3;
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
    }
    i == b.len()
}

/// Normalise an ISO timestamp: `T` → space, add `:00` seconds if omitted.
fn normalize_ts(s: &str) -> String {
    let mut out = if s.len() > 10 && s.as_bytes()[10] == b'T' {
        format!("{} {}", &s[..10], &s[11..])
    } else {
        s.to_string()
    };
    // `yyyy-MM-dd HH:mm` (16 chars, no seconds) → append ":00".
    if out.len() == 16 && is_ts_shape(&out) {
        out.push_str(":00");
    }
    out
}

#[cfg(test)]
mod tests;
