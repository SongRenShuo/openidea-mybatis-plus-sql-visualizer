//! iBatis 2.x `#` / `$` placeholder parsing.
//!
//! Follows `InlineParameterMapParser` (`#`) and `SimpleDynamicSql` (`$`):
//!
//! 1. a `#` token containing `:` uses the *simple* syntax (3 chunks =
//!    `name:jdbcType`, 5+ chunks = `name:jdbcType:nullValue`, anything else is
//!    a format error). **`#name:nullValue#` means jdbcType, not nullValue**;
//! 2. otherwise the *advanced* syntax
//!    `#prop,javaType=…,jdbcType=…,mode=…,nullValue=…,handler=…,numericScale=…#`;
//! 3. `##` / `$$` are literal `#` / `$`; an unclosed delimiter is kept verbatim
//!    with a warning (the engine throws — this chain degrades instead);
//! 4. `$x$` reads the parameter object itself when it is a scalar, otherwise the
//!    named property; a null value is dropped silently.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineMapping {
    pub property: String,
    pub jdbc_type: Option<String>,
    pub null_value: Option<String>,
    pub java_type: Option<String>,
    pub mode: Option<String>,
    pub numeric_scale: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineOutcome {
    pub text: String,
    pub mappings: Vec<InlineMapping>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DollarOutcome {
    pub text: String,
    pub site_count: usize,
    pub rendered_values: Vec<String>,
    pub warnings: Vec<String>,
}

/// Java `StringTokenizer` semantics: single-character delimiters, optional
/// delimiter tokens, never produces empty tokens.
fn tokenize(text: &str, delims: &str, return_delims: bool) -> Vec<String> {
    let delim_set: Vec<char> = delims.chars().collect();
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if delim_set.contains(&ch) {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            if return_delims {
                tokens.push(ch.to_string());
            }
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Parses every `#...#` site into `?` plus an ordered mapping list.
pub fn parse_inline(text: &str) -> InlineOutcome {
    let tokens = tokenize(text, "#", true);
    let mut out = String::new();
    let mut mappings = Vec::new();
    let mut warnings = Vec::new();
    let mut last: Option<&str> = None;
    let mut index = 0usize;
    while index < tokens.len() {
        let token = tokens[index].as_str();
        if last == Some("#") {
            if token == "#" {
                out.push('#');
                last = None;
                index += 1;
                continue;
            }
            let closed = index + 1 < tokens.len() && tokens[index + 1] == "#";
            if closed {
                match parse_mapping(token, &mut warnings) {
                    Some(mapping) => {
                        mappings.push(mapping);
                        out.push('?');
                    }
                    None => {
                        out.push('#');
                        out.push_str(token);
                        out.push('#');
                    }
                }
                index += 2;
            } else {
                warnings.push(format!(
                    "unclosed inline parameter (kept verbatim): #{token}"
                ));
                out.push('#');
                out.push_str(token);
                index += 1;
            }
            last = None;
            continue;
        }
        if token != "#" {
            out.push_str(token);
        }
        last = Some(token);
        index += 1;
    }
    InlineOutcome {
        text: out,
        mappings,
        warnings,
    }
}

fn parse_mapping(token: &str, warnings: &mut Vec<String>) -> Option<InlineMapping> {
    if token.contains(':') {
        old_mapping(token, warnings)
    } else {
        new_mapping(token, warnings)
    }
}

fn new_mapping(token: &str, warnings: &mut Vec<String>) -> Option<InlineMapping> {
    let fields = tokenize(token, "=,", false);
    if fields.is_empty() {
        warnings.push(format!(
            "inline parameter without a property name: #{token}#"
        ));
        return None;
    }
    let mut mapping = InlineMapping {
        property: fields[0].clone(),
        jdbc_type: None,
        null_value: None,
        java_type: None,
        mode: None,
        numeric_scale: None,
    };
    let mut index = 1usize;
    while index < fields.len() {
        if index + 1 >= fields.len() {
            warnings.push(format!(
                "inline parameter without name=value pairs: #{token}#"
            ));
            return None;
        }
        let field = fields[index].as_str();
        let value = fields[index + 1].clone();
        match field {
            "javaType" => mapping.java_type = Some(value),
            "jdbcType" => mapping.jdbc_type = Some(value),
            "mode" => mapping.mode = Some(value),
            "nullValue" => mapping.null_value = Some(value),
            "numericScale" => match value.parse::<i32>() {
                Ok(scale) if scale >= 0 => mapping.numeric_scale = Some(scale),
                _ => {
                    warnings.push(format!(
                        "numericScale is not a non-negative integer: #{token}#"
                    ));
                    return None;
                }
            },
            "handler" => {}
            other => {
                warnings.push(format!(
                    "unknown inline parameter field '{other}': #{token}#"
                ));
                return None;
            }
        }
        index += 2;
    }
    Some(mapping)
}

fn old_mapping(token: &str, warnings: &mut Vec<String>) -> Option<InlineMapping> {
    let fields = tokenize(token, ":", true);
    match fields.len() {
        3 => Some(InlineMapping {
            property: fields[0].clone(),
            jdbc_type: Some(fields[2].clone()),
            null_value: None,
            java_type: None,
            mode: None,
            numeric_scale: None,
        }),
        n if n >= 5 => {
            let null_value = fields[4..].concat();
            Some(InlineMapping {
                property: fields[0].clone(),
                jdbc_type: Some(fields[2].clone()),
                null_value: Some(null_value),
                java_type: None,
                mode: None,
                numeric_scale: None,
            })
        }
        _ => {
            warnings.push(format!(
                "inline parameter must have 3 or >=5 colon-separated chunks: #{token}#"
            ));
            None
        }
    }
}

/// Replaces `$x$` sites after the dynamic tags have been evaluated.
pub fn process_dollar(sql: &str, params: &serde_json::Value) -> DollarOutcome {
    let tokens = tokenize(sql, "$", true);
    let mut out = String::new();
    let mut rendered_values = Vec::new();
    let mut warnings = Vec::new();
    let mut site_count = 0usize;
    let mut last: Option<&str> = None;
    let mut index = 0usize;
    while index < tokens.len() {
        let token = tokens[index].as_str();
        if last == Some("$") {
            if token == "$" {
                out.push('$');
                last = None;
                index += 1;
                continue;
            }
            let closed = index + 1 < tokens.len() && tokens[index + 1] == "$";
            if !closed {
                warnings.push(format!(
                    "unclosed $ dynamic element (kept verbatim): ${token}"
                ));
                out.push('$');
                out.push_str(token);
                index += 1;
                last = None;
                continue;
            }
            site_count += 1;
            if let Some(value) = dollar_value(params, token) {
                // A null value is dropped silently (the engine's `if (value != null)`).
                if !value.is_null() {
                    let text = value_to_string(&value);
                    out.push_str(&text);
                    rendered_values.push(text);
                }
            }
            index += 2;
            last = None;
            continue;
        }
        if token != "$" {
            out.push_str(token);
        }
        last = Some(token);
        index += 1;
    }
    DollarOutcome {
        text: out,
        site_count,
        rendered_values,
        warnings,
    }
}

fn dollar_value(params: &serde_json::Value, name: &str) -> Option<serde_json::Value> {
    match params {
        serde_json::Value::Null => None,
        serde_json::Value::Object(_) => lookup(params, name),
        // Scalar / array parameter: `$x$` means the whole object.
        _ => Some(params.clone()),
    }
}

/// Resolves `a.b` / `a[0].b` / `a['k']` from an arbitrary JSON root.
pub fn lookup(root: &serde_json::Value, path: &str) -> Option<serde_json::Value> {
    use serde_json::Value;
    let path = path.trim();
    if path.is_empty() {
        return Some(root.clone());
    }
    let mut current = root.clone();
    let mut pos = 0usize;
    let bytes = path.as_bytes();
    // First segment.
    let start = pos;
    while pos < path.len() && bytes[pos] != b'.' && bytes[pos] != b'[' {
        pos += 1;
    }
    if pos > start {
        current = match &current {
            Value::Object(map) => map.get(&path[start..pos])?.clone(),
            _ => return None,
        };
    }
    while pos < path.len() {
        if bytes[pos] == b'.' {
            pos += 1;
            let start = pos;
            while pos < path.len() && bytes[pos] != b'.' && bytes[pos] != b'[' {
                pos += 1;
            }
            let name = &path[start..pos];
            current = match &current {
                Value::Object(map) => map.get(name)?.clone(),
                _ => return None,
            };
        } else if bytes[pos] == b'[' {
            let close = path[pos..].find(']')? + pos;
            let key = path[pos + 1..close]
                .trim()
                .trim_matches('\'')
                .trim_matches('"');
            current = match &current {
                Value::Object(map) => map.get(key)?.clone(),
                Value::Array(items) => items.get(key.parse::<usize>().ok()?)?.clone(),
                _ => return None,
            };
            pos = close + 1;
        } else {
            break;
        }
    }
    Some(current)
}

fn value_to_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn simple_hash_becomes_question_mark() {
        let out = parse_inline("SELECT * FROM t WHERE id = #id#");
        assert_eq!(out.text, "SELECT * FROM t WHERE id = ?");
        assert_eq!(out.mappings[0].property, "id");
    }

    #[test]
    fn advanced_hash_keeps_jdbc_type() {
        let out = parse_inline("#name,jdbcType=VARCHAR,handler=Some#");
        assert_eq!(out.text, "?");
        assert_eq!(out.mappings[0].property, "name");
        assert_eq!(out.mappings[0].jdbc_type.as_deref(), Some("VARCHAR"));
    }

    #[test]
    fn colon_syntax_uses_jdbc_type_not_null_value() {
        let out = parse_inline("#name:VARCHAR#");
        assert_eq!(out.text, "?");
        assert_eq!(out.mappings[0].jdbc_type.as_deref(), Some("VARCHAR"));
        assert!(out.mappings[0].null_value.is_none());
    }

    #[test]
    fn colon_syntax_with_null_value() {
        let out = parse_inline("#name:VARCHAR:N/A#");
        assert_eq!(out.mappings[0].null_value.as_deref(), Some("N/A"));
    }

    #[test]
    fn double_hash_is_a_literal_hash() {
        let out = parse_inline("SELECT ## FROM t");
        assert_eq!(out.text, "SELECT # FROM t");
        assert!(out.mappings.is_empty());
    }

    #[test]
    fn unclosed_hash_is_kept_with_a_warning() {
        let out = parse_inline("SELECT #id FROM t");
        assert_eq!(out.text, "SELECT #id FROM t");
        assert_eq!(out.warnings.len(), 1);
    }

    #[test]
    fn malformed_colon_syntax_warns() {
        let out = parse_inline("#name:VARCHAR#");
        assert_eq!(out.mappings.len(), 1);
        let out = parse_inline("#a::b#");
        assert!(out.mappings.is_empty());
        assert_eq!(out.text, "#a::b#");
        assert_eq!(out.warnings.len(), 1);
    }

    #[test]
    fn dollar_reads_the_property_and_drops_nulls() {
        let params = json!({"table": "users", "none": null});
        let out = process_dollar("SELECT * FROM $table$ $none$", &params);
        assert_eq!(out.text, "SELECT * FROM users ");
        assert_eq!(out.site_count, 2);
        assert_eq!(out.rendered_values, vec!["users".to_string()]);
    }

    #[test]
    fn double_dollar_is_a_literal() {
        let out = process_dollar("SELECT 1 $$ 2", &json!({}));
        assert_eq!(out.text, "SELECT 1 $ 2");
        assert_eq!(out.site_count, 0);
    }

    #[test]
    fn scalar_parameter_is_the_whole_dollar_value() {
        let out = process_dollar("SELECT $x$", &json!("abc"));
        assert_eq!(out.text, "SELECT abc");
    }

    #[test]
    fn lookup_supports_nested_paths_and_indexes() {
        let value = json!({"a": {"b": [{"c": 7}]}});
        assert_eq!(lookup(&value, "a.b[0].c"), Some(json!(7)));
        assert_eq!(lookup(&value, "a['b'][0]['c']"), Some(json!(7)));
        assert_eq!(lookup(&value, "a.missing"), None);
    }
}
