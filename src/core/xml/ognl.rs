//! Restricted OGNL subset for `test` / `bind` / `${...}` (T1-2).
//!
//! Supported grammar (everything else is rejected at parse time — never
//! evaluated as "true" and never panicking):
//!
//! ```text
//! expr   := or
//! or     := and (("or" | "||") and)*
//! and    := not (("and" | "&&") not)*
//! not    := ("not" | "!") not | cmp
//! cmp    := operand ( ("=="|"!="|">"|">="|"<"|"<="|"eq"|"neq"|"gt"|"gte"|"lt"|"lte") operand )?
//! operand:= ("-" | "+") operand | chain
//! chain  := primary ( "." ident | "[" expr "]" )*
//! primary:= number | string | true | false | null | ident | "(" expr ")"
//! ```
//!
//! Deliberately unsupported (parse error, `message_key = ognl.unsupported`):
//! method calls (`list.size()`), static references (`@java.lang.Math@max`),
//! constructors (`new ...`), OGNL variables (`#var`), ternary `?:`, arithmetic
//! (`+ - * / %` as binary operators), `in` / `not in`, lambdas, assignments.
//!
//! Values are `serde_json::Value`, the only representation the engine and the
//! MCP layer share. Property access follows map semantics: a missing key
//! resolves to `null` (never an error), which is how MyBatis sees a Map-based
//! parameter object.

use serde_json::Value;

pub const KEY_UNSUPPORTED: &str = "ognl.unsupported";
pub const KEY_EVAL_FAILED: &str = "ognl.eval_failed";

/// A parse/evaluation failure carrying a stable message key and a detail
/// string for diagnostics (never shown as-is to users).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OgnlError {
    pub message_key: &'static str,
    pub detail: String,
}

impl OgnlError {
    fn unsupported(detail: impl Into<String>) -> Self {
        Self {
            message_key: KEY_UNSUPPORTED,
            detail: detail.into(),
        }
    }

    fn eval(detail: impl Into<String>) -> Self {
        Self {
            message_key: KEY_EVAL_FAILED,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Literal(Value),
    Root(String),
    Chain(Box<Expr>, Vec<Step>),
    Not(Box<Expr>),
    Neg(Box<Expr>),
    Binary(Op, Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Property(String),
    Index(Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Number(String),
    Str(String),
    Sym(&'static str),
    Eof,
}

struct Lexer<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> Lexer<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, pos: 0 }
    }

    fn bytes(&self) -> &'a [u8] {
        self.text.as_bytes()
    }

    fn skip_ws(&mut self) {
        while self.pos < self.text.len() && self.bytes()[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn next(&mut self) -> Result<Tok, OgnlError> {
        self.skip_ws();
        if self.pos >= self.text.len() {
            return Ok(Tok::Eof);
        }
        let b = self.bytes()[self.pos];
        if b.is_ascii_digit() {
            return Ok(Tok::Number(self.read_number()));
        }
        if b == b'\'' || b == b'"' {
            return self.read_string(b);
        }
        if b.is_ascii_alphabetic() || b == b'_' || b == b'$' {
            let start = self.pos;
            while self.pos < self.text.len() {
                let c = self.bytes()[self.pos];
                if c.is_ascii_alphanumeric() || c == b'_' || c == b'$' {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            return Ok(Tok::Ident(self.text[start..self.pos].to_string()));
        }
        let two = &self.text[self.pos..(self.pos + 2).min(self.text.len())];
        let sym = match two {
            "==" => Some("=="),
            "!=" => Some("!="),
            ">=" => Some(">="),
            "<=" => Some("<="),
            "&&" => Some("&&"),
            "||" => Some("||"),
            _ => None,
        };
        if let Some(sym) = sym {
            self.pos += 2;
            return Ok(Tok::Sym(sym));
        }
        let single = match b {
            b'>' => ">",
            b'<' => "<",
            b'!' => "!",
            b'(' => "(",
            b')' => ")",
            b'[' => "[",
            b']' => "]",
            b'.' => ".",
            b',' => ",",
            b'+' => "+",
            b'-' => "-",
            b'*' => "*",
            b'/' => "/",
            b'%' => "%",
            b':' => ":",
            b'?' => "?",
            b'@' => "@",
            b'#' => "#",
            _ => {
                return Err(OgnlError::unsupported(format!(
                    "unexpected character '{}' at offset {}",
                    b as char, self.pos
                )));
            }
        };
        self.pos += 1;
        Ok(Tok::Sym(single))
    }

    fn read_number(&mut self) -> String {
        let start = self.pos;
        while self.pos < self.text.len() && self.bytes()[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        if self.pos < self.text.len()
            && self.bytes()[self.pos] == b'.'
            && self.pos + 1 < self.text.len()
            && self.bytes()[self.pos + 1].is_ascii_digit()
        {
            self.pos += 1;
            while self.pos < self.text.len() && self.bytes()[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
        }
        if self.pos < self.text.len()
            && (self.bytes()[self.pos] == b'e' || self.bytes()[self.pos] == b'E')
        {
            let save = self.pos;
            self.pos += 1;
            if self.pos < self.text.len()
                && (self.bytes()[self.pos] == b'+' || self.bytes()[self.pos] == b'-')
            {
                self.pos += 1;
            }
            if self.pos < self.text.len() && self.bytes()[self.pos].is_ascii_digit() {
                while self.pos < self.text.len() && self.bytes()[self.pos].is_ascii_digit() {
                    self.pos += 1;
                }
            } else {
                self.pos = save;
            }
        }
        self.text[start..self.pos].to_string()
    }

    fn read_string(&mut self, quote: u8) -> Result<Tok, OgnlError> {
        self.pos += 1;
        let mut out = String::new();
        while self.pos < self.text.len() {
            let b = self.bytes()[self.pos];
            if b == quote {
                self.pos += 1;
                return Ok(Tok::Str(out));
            }
            if b == b'\\' {
                self.pos += 1;
                if self.pos >= self.text.len() {
                    break;
                }
                let esc = self.bytes()[self.pos];
                self.pos += 1;
                match esc {
                    b'n' => out.push('\n'),
                    b't' => out.push('\t'),
                    b'r' => out.push('\r'),
                    b'\\' => out.push('\\'),
                    b'\'' => out.push('\''),
                    b'"' => out.push('"'),
                    other => out.push(other as char),
                }
                continue;
            }
            let ch = self.text[self.pos..].chars().next().unwrap_or('?');
            out.push(ch);
            self.pos += ch.len_utf8();
        }
        Err(OgnlError::unsupported("unterminated string literal"))
    }
}

struct Parser<'a> {
    lexer: Lexer<'a>,
    current: Tok,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Result<Self, OgnlError> {
        let mut lexer = Lexer::new(text);
        let current = lexer.next()?;
        Ok(Self { lexer, current })
    }

    fn advance(&mut self) -> Result<(), OgnlError> {
        self.current = self.lexer.next()?;
        Ok(())
    }

    fn is_ident(&self, name: &str) -> bool {
        matches!(&self.current, Tok::Ident(i) if i == name)
    }

    fn is_sym(&self, sym: &str) -> bool {
        matches!(&self.current, Tok::Sym(s) if *s == sym)
    }

    fn parse_expression(&mut self) -> Result<Expr, OgnlError> {
        let expr = self.parse_or()?;
        if !matches!(self.current, Tok::Eof) {
            return Err(OgnlError::unsupported(format!(
                "unexpected trailing token {:?}",
                self.current
            )));
        }
        Ok(expr)
    }

    fn parse_or(&mut self) -> Result<Expr, OgnlError> {
        let mut left = self.parse_and()?;
        loop {
            let is_or = self.is_ident("or") || self.is_sym("||");
            if !is_or {
                return Ok(left);
            }
            self.advance()?;
            let right = self.parse_and()?;
            left = Expr::Binary(Op::Or, Box::new(left), Box::new(right));
        }
    }

    fn parse_and(&mut self) -> Result<Expr, OgnlError> {
        let mut left = self.parse_cmp()?;
        loop {
            let is_and = self.is_ident("and") || self.is_sym("&&");
            if !is_and {
                return Ok(left);
            }
            self.advance()?;
            let right = self.parse_cmp()?;
            left = Expr::Binary(Op::And, Box::new(left), Box::new(right));
        }
    }

    /// Unary `!` / `not` binds tighter than any comparison, as in OGNL:
    /// `!a == b` is `(!a) == b`.
    fn parse_not(&mut self) -> Result<Expr, OgnlError> {
        if self.is_ident("not") || self.is_sym("!") {
            self.advance()?;
            let inner = self.parse_not()?;
            return Ok(Expr::Not(Box::new(inner)));
        }
        self.parse_chain()
    }

    fn parse_cmp(&mut self) -> Result<Expr, OgnlError> {
        let left = self.parse_operand()?;
        let op = match &self.current {
            Tok::Sym("==") => Some(Op::Eq),
            Tok::Sym("!=") => Some(Op::Ne),
            Tok::Sym(">") => Some(Op::Gt),
            Tok::Sym(">=") => Some(Op::Ge),
            Tok::Sym("<") => Some(Op::Lt),
            Tok::Sym("<=") => Some(Op::Le),
            Tok::Ident(word) => match word.as_str() {
                "eq" => Some(Op::Eq),
                "neq" => Some(Op::Ne),
                "gt" => Some(Op::Gt),
                "gte" => Some(Op::Ge),
                "lt" => Some(Op::Lt),
                "lte" => Some(Op::Le),
                _ => None,
            },
            _ => None,
        };
        let Some(op) = op else {
            return Ok(left);
        };
        self.advance()?;
        let right = self.parse_operand()?;
        Ok(Expr::Binary(op, Box::new(left), Box::new(right)))
    }

    fn parse_operand(&mut self) -> Result<Expr, OgnlError> {
        if self.is_sym("-") {
            self.advance()?;
            let inner = self.parse_operand()?;
            return Ok(Expr::Neg(Box::new(inner)));
        }
        if self.is_sym("+") {
            self.advance()?;
            return self.parse_operand();
        }
        self.parse_not()
    }

    fn parse_chain(&mut self) -> Result<Expr, OgnlError> {
        let mut expr = self.parse_primary()?;
        let mut steps = Vec::new();
        loop {
            if self.is_sym(".") {
                self.advance()?;
                let name = match &self.current {
                    Tok::Ident(name) => name.clone(),
                    other => {
                        return Err(OgnlError::unsupported(format!(
                            "expected a property name, got {other:?}"
                        )));
                    }
                };
                self.advance()?;
                if self.is_sym("(") {
                    return Err(OgnlError::unsupported(format!(
                        "method call .{name}() is outside the supported subset"
                    )));
                }
                steps.push(Step::Property(name));
                continue;
            }
            if self.is_sym("[") {
                self.advance()?;
                let index = self.parse_or()?;
                if !self.is_sym("]") {
                    return Err(OgnlError::unsupported("missing ']'"));
                }
                self.advance()?;
                steps.push(Step::Index(Box::new(index)));
                continue;
            }
            break;
        }
        if steps.is_empty() {
            Ok(expr)
        } else {
            expr = Expr::Chain(Box::new(expr), steps);
            Ok(expr)
        }
    }

    fn parse_primary(&mut self) -> Result<Expr, OgnlError> {
        match self.current.clone() {
            Tok::Number(raw) => {
                self.advance()?;
                let value = parse_number_value(&raw);
                Ok(Expr::Literal(value))
            }
            Tok::Str(s) => {
                self.advance()?;
                Ok(Expr::Literal(Value::String(s)))
            }
            Tok::Ident(name) => match name.as_str() {
                "true" => {
                    self.advance()?;
                    Ok(Expr::Literal(Value::Bool(true)))
                }
                "false" => {
                    self.advance()?;
                    Ok(Expr::Literal(Value::Bool(false)))
                }
                "null" => {
                    self.advance()?;
                    Ok(Expr::Literal(Value::Null))
                }
                "new" => Err(OgnlError::unsupported(
                    "constructor calls are not supported",
                )),
                "in" => Err(OgnlError::unsupported("the 'in' operator is not supported")),
                "and" | "or" | "not" | "eq" | "neq" | "gt" | "gte" | "lt" | "lte" => {
                    Err(OgnlError::unsupported(format!(
                        "operator '{name}' used without a left operand"
                    )))
                }
                _ => {
                    self.advance()?;
                    if self.is_sym("(") {
                        return Err(OgnlError::unsupported(format!(
                            "method call {name}() is outside the supported subset"
                        )));
                    }
                    Ok(Expr::Root(name))
                }
            },
            Tok::Sym("(") => {
                self.advance()?;
                let inner = self.parse_or()?;
                if !self.is_sym(")") {
                    return Err(OgnlError::unsupported("missing ')'"));
                }
                self.advance()?;
                Ok(inner)
            }
            Tok::Sym("@") => Err(OgnlError::unsupported(
                "static references (@Class@field / @Class@method) are not supported",
            )),
            Tok::Sym("#") => Err(OgnlError::unsupported(
                "OGNL variables (#var) are not supported",
            )),
            other => Err(OgnlError::unsupported(format!(
                "unexpected token {other:?} at expression start"
            ))),
        }
    }
}

fn parse_number_value(raw: &str) -> Value {
    if !raw.contains('.')
        && !raw.contains('e')
        && !raw.contains('E')
        && let Ok(i) = raw.parse::<i64>()
    {
        return Value::Number(i.into());
    }
    match raw.parse::<f64>() {
        Ok(f) => serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Err(_) => Value::Null,
    }
}

/// Parses `text` into an expression tree.
pub fn parse(text: &str) -> Result<Expr, OgnlError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(OgnlError::unsupported("empty expression"));
    }
    let mut parser = Parser::new(trimmed)?;
    parser.parse_expression()
}

/// Parses and evaluates an expression against the binding map.
pub fn evaluate(
    text: &str,
    bindings: &std::collections::BTreeMap<String, Value>,
) -> Result<Value, OgnlError> {
    let expr = parse(text)?;
    eval(&expr, bindings)
}

fn eval(
    expr: &Expr,
    bindings: &std::collections::BTreeMap<String, Value>,
) -> Result<Value, OgnlError> {
    match expr {
        Expr::Literal(v) => Ok(v.clone()),
        Expr::Root(name) => Ok(bindings.get(name).cloned().unwrap_or(Value::Null)),
        Expr::Neg(inner) => match eval(inner, bindings)? {
            Value::Number(n) => {
                let negated = match n.as_i64() {
                    Some(i) => Value::Number((-i).into()),
                    None => match n.as_f64() {
                        Some(f) => serde_json::Number::from_f64(-f)
                            .map(Value::Number)
                            .unwrap_or(Value::Null),
                        None => return Err(OgnlError::eval("not a number")),
                    },
                };
                Ok(negated)
            }
            other => Err(OgnlError::eval(format!(
                "unary minus on a non-number: {other}"
            ))),
        },
        Expr::Not(inner) => Ok(Value::Bool(!truthy(&eval(inner, bindings)?))),
        Expr::Chain(base, steps) => {
            let mut value = eval(base, bindings)?;
            for step in steps {
                value = eval_step(step, value, bindings)?;
            }
            Ok(value)
        }
        Expr::Binary(op, left, right) => match op {
            Op::And => {
                let l = eval(left, bindings)?;
                if !truthy(&l) {
                    return Ok(Value::Bool(false));
                }
                Ok(Value::Bool(truthy(&eval(right, bindings)?)))
            }
            Op::Or => {
                let l = eval(left, bindings)?;
                if truthy(&l) {
                    return Ok(Value::Bool(true));
                }
                Ok(Value::Bool(truthy(&eval(right, bindings)?)))
            }
            Op::Eq => Ok(Value::Bool(equals(
                &eval(left, bindings)?,
                &eval(right, bindings)?,
            ))),
            Op::Ne => Ok(Value::Bool(!equals(
                &eval(left, bindings)?,
                &eval(right, bindings)?,
            ))),
            Op::Lt | Op::Le | Op::Gt | Op::Ge => {
                let ordering = compare(&eval(left, bindings)?, &eval(right, bindings)?)?;
                let ok = match op {
                    Op::Lt => ordering < 0,
                    Op::Le => ordering <= 0,
                    Op::Gt => ordering > 0,
                    Op::Ge => ordering >= 0,
                    _ => unreachable!(),
                };
                Ok(Value::Bool(ok))
            }
        },
    }
}

fn eval_step(
    step: &Step,
    value: Value,
    bindings: &std::collections::BTreeMap<String, Value>,
) -> Result<Value, OgnlError> {
    match step {
        Step::Property(name) => match value {
            Value::Object(map) => {
                if let Some(value) = map.get(name) {
                    return Ok(value.clone());
                }
                // OGNL exposes the map size as the `size` property.
                if name == "size" {
                    Ok(Value::Number((map.len() as i64).into()))
                } else {
                    Ok(Value::Null)
                }
            }
            Value::Null => Ok(Value::Null),
            Value::Array(items) if name == "size" => Ok(Value::Number((items.len() as i64).into())),
            Value::String(s) if name == "size" => {
                Ok(Value::Number((s.chars().count() as i64).into()))
            }
            other => Err(OgnlError::eval(format!(
                "cannot read property '{name}' of {}",
                type_name(&other)
            ))),
        },
        Step::Index(index) => {
            let index_value = eval(index, bindings)?;
            match value {
                Value::Array(items) => {
                    let idx = index_value
                        .as_i64()
                        .ok_or_else(|| OgnlError::eval("array index is not an integer"))?;
                    if idx < 0 || idx as usize >= items.len() {
                        return Err(OgnlError::eval(format!(
                            "array index {idx} out of bounds (len {})",
                            items.len()
                        )));
                    }
                    Ok(items[idx as usize].clone())
                }
                Value::Object(map) => match index_value.as_str() {
                    Some(key) => Ok(map.get(key).cloned().unwrap_or(Value::Null)),
                    None => Err(OgnlError::eval("map key must be a string")),
                },
                other => Err(OgnlError::eval(format!(
                    "cannot index {}",
                    type_name(&other)
                ))),
            }
        }
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn equals(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Null, Value::Null) => true,
        (Value::Null, _) | (_, Value::Null) => false,
        (Value::String(_), Value::String(_)) => left == right,
        (Value::Bool(_), Value::Bool(_)) => left == right,
        (Value::Number(_), Value::Number(_)) => as_f64(left) == as_f64(right),
        (Value::Number(_), Value::String(_)) | (Value::String(_), Value::Number(_)) => {
            match (as_f64(left), as_f64(right)) {
                (Some(a), Some(b)) => a == b,
                _ => false,
            }
        }
        (Value::Array(_), Value::Array(_)) | (Value::Object(_), Value::Object(_)) => left == right,
        _ => false,
    }
}

/// Returns `-1`, `0` or `1`; an incomparable pair is an evaluation error.
fn compare(left: &Value, right: &Value) -> Result<i32, OgnlError> {
    match (left, right) {
        (Value::String(a), Value::String(b)) => Ok(match a.cmp(b) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        }),
        (Value::Null, _) | (_, Value::Null) => {
            Err(OgnlError::eval("ordering comparison against null"))
        }
        (a, b) => match (as_f64(a), as_f64(b)) {
            (Some(x), Some(y)) => Ok(if x < y {
                -1
            } else if x > y {
                1
            } else {
                0
            }),
            _ => Err(OgnlError::eval(format!(
                "incomparable operands: {} vs {}",
                type_name(a),
                type_name(b)
            ))),
        },
    }
}

/// OGNL `OgnlOps.booleanValue`: empty string / empty collection / zero are
/// false. Used by `and` / `or` / `not`.
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// MyBatis `ExpressionEvaluator.evaluateBoolean`: a Boolean keeps its value, a
/// Number is true when non-zero, anything else is true when non-null
/// (an empty string is true here — unlike [`truthy`]).
pub fn is_true(value: &Value) -> bool {
    match value {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        other => !other.is_null(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn bindings() -> BTreeMap<String, Value> {
        let mut map = BTreeMap::new();
        map.insert("id".into(), json!(7));
        map.insert("name".into(), json!("Ada"));
        map.insert("empty".into(), json!(""));
        map.insert("active".into(), json!(true));
        map.insert("none".into(), Value::Null);
        map.insert("list".into(), json!([10, 20, 30]));
        map.insert("map".into(), json!({"k": "v", "n": 2}));
        map.insert(
            "user".into(),
            json!({"name": "Bob", "address": {"city": "Sz"}}),
        );
        map.insert("zero".into(), json!(0));
        map
    }

    fn truth(text: &str) -> bool {
        let b = bindings();
        let value = evaluate(text, &b).unwrap_or_else(|e| panic!("{text}: {}", e.detail));
        is_true(&value)
    }

    #[test]
    fn numeric_comparisons() {
        assert!(truth("id == 7"));
        assert!(truth("id != 8"));
        assert!(truth("id > 6"));
        assert!(truth("id >= 7"));
        assert!(truth("id < 8"));
        assert!(truth("id <= 7"));
    }

    #[test]
    fn word_comparison_operators() {
        assert!(truth("id gt 6"));
        assert!(truth("id gte 7"));
        assert!(truth("id lt 8"));
        assert!(truth("id lte 7"));
        assert!(truth("id eq 7"));
        assert!(truth("id neq 8"));
    }

    #[test]
    fn null_checks_and_string_literals() {
        assert!(truth("none == null"));
        assert_eq!(evaluate("none != null", &bindings()).unwrap(), json!(false));
        assert!(truth("name == 'Ada'"));
        assert!(truth("name != \"Bob\""));
    }

    #[test]
    fn boolean_operators_short_circuit() {
        assert!(truth("active and id == 7"));
        assert!(truth("active && name == 'Ada'"));
        assert!(!truth("active && name == 'Bob'"));
        assert!(truth("!active == false"));
        assert!(truth("not (id == 8)"));
        assert!(truth("id == 8 or active"));
        assert!(!truth("id == 8 || id == 9"));
    }

    #[test]
    fn property_navigation_and_indexing() {
        assert!(truth("user.name == 'Bob'"));
        assert!(truth("user.address.city == 'Sz'"));
        assert!(truth("list[0] == 10"));
        assert!(truth("list[2] > list[1]"));
        assert!(truth("map['k'] == 'v'"));
        assert!(truth("map.k == 'v'"));
        assert_eq!(
            evaluate("user.address.city", &bindings()).unwrap(),
            json!("Sz")
        );
    }

    #[test]
    fn list_size_property() {
        assert!(truth("list.size == 3"));
        assert!(truth("list.size > 0"));
        assert!(truth("name.size == 3"));
        assert!(truth("map.size == 2"));
    }

    #[test]
    fn missing_map_key_is_null_not_an_error() {
        assert!(truth("absent == null"));
        assert!(truth("absent.deeply.nested == null"));
    }

    #[test]
    fn empty_string_is_true_for_test_but_false_for_operators() {
        assert!(truth("empty"));
        assert!(!truth("empty and true"));
        assert!(truth("not empty"));
    }

    #[test]
    fn zero_number_is_false() {
        assert!(!truth("zero"));
        assert!(truth("zero == 0"));
    }

    #[test]
    fn static_calls_are_rejected() {
        let err = parse("@java.lang.Math@max(1, 2)").unwrap_err();
        assert_eq!(err.message_key, KEY_UNSUPPORTED);
        assert!(matches!(
            parse("@java.lang.Math@PI").unwrap_err().message_key,
            KEY_UNSUPPORTED
        ));
    }

    #[test]
    fn method_calls_are_rejected() {
        for text in ["list.size()", "name.trim()", "user.getName()"] {
            assert_eq!(
                parse(text).unwrap_err().message_key,
                KEY_UNSUPPORTED,
                "{text}"
            );
        }
    }

    #[test]
    fn other_unsupported_syntax_is_rejected() {
        for text in [
            "id + 1 > 0",
            "id * 2 > 0",
            "id == 1 ? true : false",
            "new java.util.Date()",
            "#var",
            "id in (1,2)",
            "list[",
            "id ==",
            "(id == 1",
            "",
        ] {
            let err = parse(text).unwrap_err();
            assert_eq!(err.message_key, KEY_UNSUPPORTED, "{text}");
        }
    }

    #[test]
    fn incomparable_ordering_is_an_eval_error_not_a_false_negative() {
        let err = compare(&Value::Bool(true), &json!(1)).unwrap_err();
        assert_eq!(err.message_key, KEY_EVAL_FAILED);
        let err = compare(&Value::String("abc".into()), &json!(2)).unwrap_err();
        assert_eq!(err.message_key, KEY_EVAL_FAILED);
    }

    #[test]
    fn numbers_and_numeric_strings_compare_numerically() {
        assert!(truth("id == '7'"));
        assert!(truth("'10' > 9"));
        assert!(truth("'10' == 10.0"));
    }

    #[test]
    fn negative_literals() {
        assert!(truth("-1 < 0"));
        assert!(truth("id > -1"));
    }

    #[test]
    fn deep_truthiness_table() {
        // 20+ evaluable cases pinned in one table.
        let cases: &[(&str, bool)] = &[
            ("true", true),
            ("false", false),
            ("null", false),
            ("1", true),
            ("0", false),
            ("'x'", true),
            ("''", true), // test semantics: empty string is non-null => true
            ("('')", true),
            ("'' == null", false),
            ("name == 'Ada'", true),
            ("name == 'ada'", false),
            ("id >= 7 and id <= 7", true),
            ("id >= 7 and id <= 6", false),
            ("active or false", true),
            ("false or false", false),
            ("not false", true),
            ("!(id == 7)", false),
            ("list.size >= 1", true),
            ("list[1] == 20 and list[0] == 10", true),
            ("user.address.city != null", true),
            ("user.address.zip == null", true),
            ("map.size > 1", true),
            ("zero == false or true", true),
            ("id != null || absent", true),
        ];
        for (text, expected) in cases {
            assert_eq!(truth(text), *expected, "case: {text}");
        }
    }
}
