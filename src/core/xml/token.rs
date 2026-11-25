//! Generic token parsing and `#{}` parameter-token decoding.
//!
//! Mirrors MyBatis `GenericTokenParser`:
//! - `\<open>` / `\<close>` are escapes: the backslash is dropped and the token
//!   is emitted verbatim;
//! - an open token without a matching close token leaves the remainder as is;
//! - an empty input is returned unchanged.

/// Scans `open..close`, replacing every token body with `handler(&body)`.
pub fn parse_tokens(
    text: &str,
    open: &str,
    close: &str,
    handler: &mut dyn FnMut(&str) -> String,
) -> String {
    if text.is_empty() || open.is_empty() || close.is_empty() {
        return text.to_string();
    }
    let bytes = text.as_bytes();
    let mut result = String::with_capacity(text.len());
    let mut offset = 0usize;
    loop {
        let start = match text[offset..].find(open) {
            Some(i) => offset + i,
            None => break,
        };
        if start > 0 && bytes[start - 1] == b'\\' {
            result.push_str(&text[offset..start - 1]);
            result.push_str(open);
            offset = start + open.len();
        } else {
            result.push_str(&text[offset..start]);
            offset = start + open.len();
            let mut end = text[offset..].find(close).map(|i| offset + i);
            while let Some(e) = end {
                if e > offset && bytes[e - 1] == b'\\' {
                    result.push_str(&text[offset..e - 1]);
                    result.push_str(close);
                    offset = e + close.len();
                    end = text[offset..].find(close).map(|i| offset + i);
                } else {
                    break;
                }
            }
            match end {
                None => {
                    result.push_str(&text[start..]);
                    offset = text.len();
                }
                Some(e) => {
                    result.push_str(&handler(&text[offset..e]));
                    offset = e + close.len();
                }
            }
        }
        if offset >= text.len() {
            break;
        }
    }
    result.push_str(&text[offset..]);
    result
}

/// Resolves `\open` escapes without rewriting tokens (used to detect whether a
/// text node is dynamic).
pub fn contains_token(text: &str, open: &str, close: &str) -> bool {
    let mut hit = false;
    let _ = parse_tokens(text, open, close, &mut |body| {
        hit = true;
        body.to_string()
    });
    hit
}

/// One `#{...}` token split into its property name and extra attributes.
///
/// The first comma segment without `=` is the property; the rest are
/// `key=value` attributes (`jdbcType`, `javaType`, `typeHandler`, `mode`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterToken {
    pub property: String,
    pub attributes: Vec<(String, String)>,
    pub original: String,
}

impl ParameterToken {
    pub fn parse(token: &str) -> Self {
        let mut property = String::new();
        let mut attributes = Vec::new();
        for segment in token.split(',') {
            let part = segment.trim();
            if part.is_empty() {
                continue;
            }
            match part.find('=') {
                Some(eq) if eq > 0 => {
                    let key = part[..eq].trim().to_string();
                    let value = part[eq + 1..].trim().to_string();
                    if !key.is_empty() {
                        attributes.push((key, value));
                    }
                }
                _ => {
                    if property.is_empty() {
                        property = part.to_string();
                    }
                }
            }
        }
        Self {
            property,
            attributes,
            original: token.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrap(body: &str) -> String {
        format!("[{body}]")
    }

    #[test]
    fn replaces_every_token() {
        let out = parse_tokens("#{a} + #{b} = #{c}", "#{", "}", &mut |b| wrap(b));
        assert_eq!(out, "[a] + [b] = [c]");
    }

    #[test]
    fn escaped_open_token_is_kept_verbatim() {
        let out = parse_tokens("\\#{name} 不替换", "#{", "}", &mut |b| wrap(b));
        assert_eq!(out, "#{name} 不替换");
    }

    #[test]
    fn escaped_close_token_is_kept_inside_the_token() {
        let out = parse_tokens("#{a\\}b}", "#{", "}", &mut |b| wrap(b));
        assert_eq!(out, "a}[b]");
    }

    #[test]
    fn unclosed_token_leaves_the_remainder() {
        let out = parse_tokens("前缀 #{unclosed", "#{", "}", &mut |b| wrap(b));
        assert_eq!(out, "前缀 #{unclosed");
    }

    #[test]
    fn plain_text_and_empty_input_are_unchanged() {
        assert_eq!(
            parse_tokens("plain text", "#{", "}", &mut wrap),
            "plain text"
        );
        assert_eq!(parse_tokens("", "#{", "}", &mut wrap), "");
    }

    #[test]
    fn detects_dynamic_text_but_not_escapes() {
        assert!(contains_token("a ${x} b", "${", "}"));
        assert!(!contains_token("a \\${x} b", "${", "}"));
        assert!(!contains_token("plain", "${", "}"));
    }

    #[test]
    fn parameter_token_keeps_property_and_attributes() {
        let token = ParameterToken::parse("id,jdbcType=BIGINT, javaType=java.lang.Long");
        assert_eq!(token.property, "id");
        assert_eq!(
            token.attributes,
            vec![
                ("jdbcType".to_string(), "BIGINT".to_string()),
                ("javaType".to_string(), "java.lang.Long".to_string()),
            ]
        );
    }

    #[test]
    fn parameter_token_with_only_property() {
        let token = ParameterToken::parse(" user.name ");
        assert_eq!(token.property, "user.name");
        assert!(token.attributes.is_empty());
    }
}
