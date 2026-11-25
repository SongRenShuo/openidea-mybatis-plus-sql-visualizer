//! Minimal XML tree used by the mapper / annotation / iBatis chains.
//!
//! The plugin may not add dependencies, so the whole XML surface needed here
//! (elements, attributes with single or double quotes, text, CDATA, comments,
//! the five predefined entities plus numeric references, DOCTYPE skip) is
//! implemented in this file. Behaviour follows `org.w3c.dom` as used by the
//! reference implementation: comments are nodes (and are dropped later by the
//! script builder), CDATA content is decoded as plain text, and entity
//! references are resolved before the SQL layer sees the text.

/// Parsed element. Attribute order is preserved (diagnostics rely on it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<XmlNode>,
}

/// A node inside an element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XmlNode {
    Element(Element),
    Text(String),
    Comment(String),
}

impl Element {
    /// Attribute value, `None` when the attribute is absent (DOM
    /// `getAttribute` returns `""`; the two are different for iBatis).
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// DOM `getAttribute` semantics: missing attribute is the empty string.
    pub fn attr_or_empty(&self, name: &str) -> &str {
        self.attr(name).unwrap_or("")
    }

    pub fn child_elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|n| match n {
            XmlNode::Element(e) => Some(e),
            _ => None,
        })
    }

    /// DOM `textContent`: all text below this element, tags stripped.
    pub fn text_content(&self) -> String {
        let mut out = String::new();
        for child in &self.children {
            match child {
                XmlNode::Text(t) => out.push_str(t),
                XmlNode::Element(e) => out.push_str(&e.text_content()),
                XmlNode::Comment(_) => {}
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlError {
    pub message: String,
    pub offset: usize,
}

impl XmlError {
    fn new(message: impl Into<String>, offset: usize) -> Self {
        Self {
            message: message.into(),
            offset,
        }
    }
}

/// Parse a document and return its root element.
pub fn parse_document(text: &str) -> Result<Element, XmlError> {
    let mut parser = Parser { text, pos: 0 };
    parser.skip_prologue()?;
    let root = parser.parse_element()?;
    parser.skip_misc()?;
    Ok(root)
}

struct Parser<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn rest(&self) -> &'a str {
        &self.text[self.pos..]
    }

    fn bytes(&self) -> &'a [u8] {
        self.text.as_bytes()
    }

    fn eof(&self) -> bool {
        self.pos >= self.text.len()
    }

    fn skip_ws(&mut self) {
        while !self.eof() && self.bytes()[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    /// Skips the XML declaration, comments and the DOCTYPE before the root.
    fn skip_prologue(&mut self) -> Result<(), XmlError> {
        loop {
            self.skip_ws();
            if self.rest().starts_with("<?") {
                self.skip_until("?>")?;
            } else if self.rest().starts_with("<!--") {
                self.skip_until("-->")?;
            } else if starts_with_ignore_case(self.rest(), "<!doctype") {
                self.skip_doctype()?;
            } else {
                return Ok(());
            }
        }
    }

    /// Skips misc nodes after the root element (comments, PIs, whitespace).
    fn skip_misc(&mut self) -> Result<(), XmlError> {
        loop {
            self.skip_ws();
            if self.rest().starts_with("<?") {
                self.skip_until("?>")?;
            } else if self.rest().starts_with("<!--") {
                self.skip_until("-->")?;
            } else {
                return Ok(());
            }
        }
    }

    fn skip_until(&mut self, marker: &str) -> Result<(), XmlError> {
        match self.rest().find(marker) {
            Some(i) => {
                self.pos += i + marker.len();
                Ok(())
            }
            None => Err(XmlError::new(format!("unterminated {marker}"), self.pos)),
        }
    }

    /// Skips `<!DOCTYPE ...>`, including an optional `[...]` internal subset.
    fn skip_doctype(&mut self) -> Result<(), XmlError> {
        let bytes = self.bytes();
        let mut i = self.pos;
        while i < bytes.len() && bytes[i] != b'>' && bytes[i] != b'[' {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'[' {
            i += 1;
            while i < bytes.len() && bytes[i] != b']' {
                i += 1;
            }
            i += 1;
            while i < bytes.len() && bytes[i] != b'>' {
                i += 1;
            }
        }
        if i >= bytes.len() {
            return Err(XmlError::new("unterminated DOCTYPE", self.pos));
        }
        self.pos = i + 1;
        Ok(())
    }

    fn parse_element(&mut self) -> Result<Element, XmlError> {
        if self.eof() || self.bytes()[self.pos] != b'<' {
            return Err(XmlError::new("expected element", self.pos));
        }
        self.pos += 1;
        let name = self.parse_name()?;
        let mut attrs = Vec::new();
        loop {
            self.skip_ws();
            if self.eof() {
                return Err(XmlError::new("unexpected end of input in tag", self.pos));
            }
            if self.rest().starts_with("/>") {
                self.pos += 2;
                return Ok(Element {
                    name,
                    attrs,
                    children: Vec::new(),
                });
            }
            if self.bytes()[self.pos] == b'>' {
                self.pos += 1;
                break;
            }
            let attr_name = self.parse_name()?;
            self.skip_ws();
            if self.eof() || self.bytes()[self.pos] != b'=' {
                return Err(XmlError::new(
                    format!("attribute {attr_name} lacks a value"),
                    self.pos,
                ));
            }
            self.pos += 1;
            self.skip_ws();
            let quote = if self.eof() {
                return Err(XmlError::new("unexpected end of input", self.pos));
            } else {
                self.bytes()[self.pos]
            };
            if quote != b'"' && quote != b'\'' {
                return Err(XmlError::new("attribute value must be quoted", self.pos));
            }
            self.pos += 1;
            let start = self.pos;
            while !self.eof() && self.bytes()[self.pos] != quote {
                self.pos += 1;
            }
            if self.eof() {
                return Err(XmlError::new("unterminated attribute value", start));
            }
            let raw = &self.text[start..self.pos];
            self.pos += 1;
            attrs.push((attr_name, decode_entities(raw)));
        }

        let mut children = Vec::new();
        loop {
            if self.eof() {
                return Err(XmlError::new(
                    format!("unclosed element <{name}>"),
                    self.pos,
                ));
            }
            if self.rest().starts_with("</") {
                self.pos += 2;
                let close = self.parse_name()?;
                self.skip_ws();
                if self.eof() || self.bytes()[self.pos] != b'>' {
                    return Err(XmlError::new("malformed closing tag", self.pos));
                }
                self.pos += 1;
                if close != name {
                    return Err(XmlError::new(
                        format!("closing tag </{close}> does not match <{name}>"),
                        self.pos,
                    ));
                }
                return Ok(Element {
                    name,
                    attrs,
                    children,
                });
            }
            if self.rest().starts_with("<!--") {
                self.pos += 4;
                let start = self.pos;
                match self.rest().find("-->") {
                    Some(i) => {
                        let body = self.text[start..start + i].to_string();
                        self.pos = start + i + 3;
                        children.push(XmlNode::Comment(body));
                    }
                    None => return Err(XmlError::new("unterminated comment", start)),
                }
                continue;
            }
            if self.rest().starts_with("<![CDATA[") {
                self.pos += 9;
                let start = self.pos;
                match self.rest().find("]]>") {
                    Some(i) => {
                        let body = self.text[start..start + i].to_string();
                        self.pos = start + i + 3;
                        children.push(XmlNode::Text(body));
                    }
                    None => return Err(XmlError::new("unterminated CDATA section", start)),
                }
                continue;
            }
            if self.rest().starts_with("<?") {
                self.skip_until("?>")?;
                continue;
            }
            if self.rest().starts_with("<!") {
                // Declaration inside content (e.g. `<!ENTITY` when the caller
                // passed a bare fragment): skip to the next `>`.
                self.skip_until(">")?;
                continue;
            }
            if self.bytes()[self.pos] == b'<' {
                let child = self.parse_element()?;
                children.push(XmlNode::Element(child));
                continue;
            }
            let start = self.pos;
            while !self.eof() && self.bytes()[self.pos] != b'<' {
                self.pos += 1;
            }
            let raw = &self.text[start..self.pos];
            children.push(XmlNode::Text(decode_entities(raw)));
        }
    }

    fn parse_name(&mut self) -> Result<String, XmlError> {
        let start = self.pos;
        while !self.eof() {
            let b = self.bytes()[self.pos];
            if b.is_ascii_whitespace()
                || matches!(b, b'>' | b'/' | b'=' | b'<' | b'"' | b'\'' | b'[' | b']')
            {
                break;
            }
            self.pos += 1;
        }
        if self.pos == start {
            return Err(XmlError::new("expected a name", start));
        }
        Ok(self.text[start..self.pos].to_string())
    }
}

fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.len() >= prefix.len() && text[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// Decodes the predefined entities and numeric character references.
/// Unknown entities are kept verbatim (DOM would reject them; keeping them
/// makes a diagnostic readable instead of failing the whole render).
pub fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'&' {
            let next = text[i..].chars().next().unwrap_or('&');
            out.push(next);
            i += next.len_utf8();
            continue;
        }
        let semi = match text[i..].find(';') {
            Some(rel) if rel <= 12 => i + rel,
            _ => {
                out.push('&');
                i += 1;
                continue;
            }
        };
        let name = &text[i + 1..semi];
        let decoded = match name {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => {
                if let Some(hex) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                    u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
                } else if let Some(dec) = name.strip_prefix('#') {
                    dec.parse::<u32>().ok().and_then(char::from_u32)
                } else {
                    None
                }
            }
        };
        match decoded {
            Some(c) => {
                out.push(c);
                i = semi + 1;
            }
            None => {
                out.push('&');
                i += 1;
            }
        }
    }
    out
}

/// Serialises an element back to XML text (used by tests and diagnostics).
pub fn to_xml(element: &Element) -> String {
    let mut out = String::new();
    write_element(element, &mut out);
    out
}

fn write_element(element: &Element, out: &mut String) {
    out.push('<');
    out.push_str(&element.name);
    for (k, v) in &element.attrs {
        out.push(' ');
        out.push_str(k);
        out.push_str("=\"");
        out.push_str(&escape_attr(v));
        out.push('"');
    }
    if element.children.is_empty() {
        out.push_str("/>");
        return;
    }
    out.push('>');
    for child in &element.children {
        match child {
            XmlNode::Element(e) => write_element(e, out),
            XmlNode::Text(t) => out.push_str(&escape_text(t)),
            XmlNode::Comment(c) => {
                out.push_str("<!--");
                out.push_str(c);
                out.push_str("-->");
            }
        }
    }
    out.push_str("</");
    out.push_str(&element.name);
    out.push('>');
}

fn escape_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attr(text: &str) -> String {
    escape_text(text)
        .replace('"', "&quot;")
        .replace('\n', "&#10;")
        .replace('\r', "&#13;")
        .replace('\t', "&#9;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_attributes_with_both_quote_styles() {
        let el = parse_document(r#"<mapper namespace='a' note="b">x</mapper>"#).unwrap();
        assert_eq!(el.name, "mapper");
        assert_eq!(el.attr("namespace"), Some("a"));
        assert_eq!(el.attr("note"), Some("b"));
        assert_eq!(el.attr("missing"), None);
        assert_eq!(el.attr_or_empty("missing"), "");
    }

    #[test]
    fn decodes_predefined_and_numeric_entities() {
        let el = parse_document(r#"<s>a &lt; b &amp;&amp; c &#65; &#x42;</s>"#).unwrap();
        assert_eq!(el.text_content(), "a < b && c A B");
    }

    #[test]
    fn cdata_is_text_and_is_not_entity_decoded() {
        let el = parse_document("<s><![CDATA[where a &lt; b]]></s>").unwrap();
        assert_eq!(el.text_content(), "where a &lt; b");
    }

    #[test]
    fn comments_are_nodes_and_do_not_leak_into_text_content() {
        let el = parse_document("<s>a<!-- c -->b</s>").unwrap();
        assert_eq!(el.text_content(), "ab");
        assert!(matches!(el.children[1], XmlNode::Comment(_)));
    }

    #[test]
    fn doctype_with_internal_subset_is_skipped() {
        let text = "<!DOCTYPE mapper [<!ENTITY x \"y\">]><mapper/>";
        let el = parse_document(text).unwrap();
        assert_eq!(el.name, "mapper");
    }

    #[test]
    fn self_closing_and_nested_elements() {
        let el = parse_document("<a><b/><c><d>t</d></c></a>").unwrap();
        assert_eq!(el.child_elements().count(), 2);
        assert_eq!(el.child_elements().nth(1).unwrap().text_content(), "t");
    }

    #[test]
    fn malformed_input_reports_an_error_instead_of_panicking() {
        assert!(parse_document("<a><b></a>").is_err());
        assert!(parse_document("<a").is_err());
        assert!(parse_document("").is_err());
    }
}
