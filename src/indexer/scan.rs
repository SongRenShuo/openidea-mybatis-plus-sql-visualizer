//! A single-pass XML tag scanner shared by the document index and the editor
//! providers.
//!
//! It is deliberately not a conforming XML parser: mapper files are read as
//! text, comments/CDATA/DOCTYPE are skipped, and attributes are captured with
//! their byte offsets so callers can point at the exact value. Ranges are
//! UTF-16, built lazily by [`Positions`].

use openidea_plugin::{Position, Range};

/// One attribute of a start tag. `value_start..value_end` excludes the quotes.
pub(crate) struct Attr<'a> {
    pub name: &'a str,
    pub value: &'a str,
    pub value_start: usize,
    pub value_end: usize,
}

/// A start tag (self-closing or not) and, once its end tag was seen, its
/// content span.
pub(crate) struct Element<'a> {
    pub name: &'a str,
    pub attrs: Vec<Attr<'a>>,
    pub self_closing: bool,
    /// Byte offset of `<`.
    pub start: usize,
    /// First byte of the element's content (past the start tag's `>`).
    pub content_start: usize,
    /// Byte offset of `</…` (== `content_start` when nothing closed it).
    pub content_end: usize,
    /// Byte offset just past the end tag.
    pub end: usize,
    /// Index of the enclosing element in the same `Vec`.
    pub parent: Option<usize>,
}

impl<'a> Element<'a> {
    pub fn attr(&self, name: &str) -> Option<&Attr<'a>> {
        self.attrs.iter().find(|a| a.name == name)
    }
}

/// Advances forward through a text and answers UTF-16 positions; offsets asked
/// in the same order as they appear cost about one pass in total.
pub(crate) struct Positions<'a> {
    text: &'a str,
    at: usize,
    pos: Position,
}

impl<'a> Positions<'a> {
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            at: 0,
            pos: Position::new(0, 0),
        }
    }

    pub fn at(&mut self, offset: usize) -> Position {
        let offset = offset.min(self.text.len());
        if offset < self.at {
            return openidea_plugin::text::position(self.text, offset);
        }
        let mut line = self.pos.line;
        let mut character = self.pos.character;
        for c in self.text[self.at..offset].chars() {
            if c == '\n' {
                line += 1;
                character = 0;
            } else {
                character += c.len_utf16() as u32;
            }
        }
        self.at = offset;
        self.pos = Position::new(line, character);
        self.pos
    }

    pub fn range(&mut self, start: usize, end: usize) -> Range {
        let s = self.at(start);
        let e = self.at(end);
        Range::new(s, e)
    }
}

fn is_name_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b':' | b'-' | b'.')
}

/// End of the comment/CDATA/processing-instruction/DOCTYPE starting at `<`.
fn skip_declaration(bytes: &[u8], lt: usize) -> Option<usize> {
    let rest = &bytes[lt..];
    let (needle, from): (&[u8], usize) = if rest.starts_with(b"<!--") {
        (b"-->", 4)
    } else if rest.starts_with(b"<![CDATA[") {
        (b"]]>", 9)
    } else if rest.starts_with(b"<?") {
        (b"?>", 2)
    } else if rest.starts_with(b"<!") {
        (b">", 2)
    } else {
        return None;
    };
    let end = find_bytes(bytes, lt + from, needle)?;
    Some(end + needle.len())
}

/// First occurrence of `needle` at or after `from`.
pub(crate) fn find_bytes(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || from >= bytes.len() {
        return None;
    }
    bytes[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| from + i)
}

/// Reads one tag at `<lt`, returning the element (content filled later) and the
/// offset just past its `>`.
fn read_tag<'a>(text: &'a str, lt: usize) -> Option<(Element<'a>, usize)> {
    let bytes = text.as_bytes();
    let mut i = lt + 1;
    let name_start = i;
    while i < bytes.len() && is_name_char(bytes[i]) {
        i += 1;
    }
    if i == name_start {
        return None;
    }
    let name = &text[name_start..i];
    let mut attrs = Vec::new();
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            return None;
        }
        if bytes[i] == b'>' {
            return Some((
                Element {
                    name,
                    attrs,
                    self_closing: false,
                    start: lt,
                    content_start: i + 1,
                    content_end: i + 1,
                    end: i + 1,
                    parent: None,
                },
                i + 1,
            ));
        }
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'>') {
            return Some((
                Element {
                    name,
                    attrs,
                    self_closing: true,
                    start: lt,
                    content_start: i + 2,
                    content_end: i + 2,
                    end: i + 2,
                    parent: None,
                },
                i + 2,
            ));
        }
        let attr_name_start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && !matches!(bytes[i], b'=' | b'>' | b'/')
        {
            i += 1;
        }
        if i == attr_name_start {
            i += 1; // malformed byte: step over it rather than looping
            continue;
        }
        let attr_name = &text[attr_name_start..i];
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if bytes.get(i) != Some(&b'=') {
            continue; // a valueless attribute; nothing to point at
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            return None;
        }
        let (value_start, value_end, next) = match bytes[i] {
            quote @ (b'"' | b'\'') => {
                let close = bytes[i + 1..]
                    .iter()
                    .position(|&b| b == quote)
                    .map(|n| i + 1 + n)?;
                (i + 1, close, close + 1)
            }
            _ => {
                let start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                    i += 1;
                }
                (start, i, i)
            }
        };
        attrs.push(Attr {
            name: attr_name,
            value: &text[value_start..value_end],
            value_start,
            value_end,
        });
        i = next;
    }
}

/// Every start tag in `text`, in document order, with nesting.
pub(crate) fn elements(text: &str) -> Vec<Element<'_>> {
    let bytes = text.as_bytes();
    let mut out: Vec<Element<'_>> = Vec::new();
    let mut open: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let Some(rel) = find_bytes(bytes, i, b"<") else {
            break;
        };
        let lt = rel;
        if let Some(next) = skip_declaration(bytes, lt) {
            i = next;
            continue;
        }
        if bytes.get(lt + 1) == Some(&b'/') {
            let mut j = lt + 2;
            let name_start = j;
            while j < bytes.len() && is_name_char(bytes[j]) {
                j += 1;
            }
            let name = &text[name_start..j];
            let Some(gt) = bytes[j..].iter().position(|&b| b == b'>') else {
                break;
            };
            let after = j + gt + 1;
            while let Some(idx) = open.pop() {
                if out[idx].name == name {
                    out[idx].content_end = lt;
                    out[idx].end = after;
                    break;
                }
            }
            i = after;
            continue;
        }
        let Some((mut element, after)) = read_tag(text, lt) else {
            i = lt + 1;
            continue;
        };
        element.parent = open.last().copied();
        out.push(element);
        let idx = out.len() - 1;
        if !out[idx].self_closing {
            open.push(idx);
        }
        i = after;
    }
    for idx in open {
        out[idx].content_end = text.len();
        out[idx].end = text.len();
    }
    out
}

/// Raw text nodes inside `content`, in order. Whitespace-only nodes are dropped;
/// the spans stay untrimmed at the edges so callers can decide.
pub(crate) fn text_nodes(
    text: &str,
    content_start: usize,
    content_end: usize,
) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    if content_end <= content_start {
        return out;
    }
    let bytes = text.as_bytes();
    let mut pos = content_start;
    let mut i = content_start;
    while i < content_end {
        let Some(rel) = find_bytes(bytes, i, b"<") else {
            break;
        };
        let lt = rel;
        if lt > pos {
            let node = &text[pos..lt];
            if !node.trim().is_empty() {
                out.push((pos, lt));
            }
        }
        if bytes[lt..].starts_with(b"<![CDATA[") {
            // CDATA is text: the SQL written inside it is a text node too.
            let content = lt + 9;
            let Some(end) = find_bytes(bytes, content, b"]]>") else {
                break;
            };
            if end > content && !text[content..end].trim().is_empty() {
                out.push((content, end));
            }
            pos = end + 3;
            i = pos;
            continue;
        }
        if let Some(next) = skip_declaration(bytes, lt) {
            pos = next;
            i = next;
            continue;
        }
        if bytes.get(lt + 1) == Some(&b'/') {
            let Some(gt) = bytes[lt..].iter().position(|&b| b == b'>') else {
                break;
            };
            pos = lt + gt + 1;
            i = pos;
            continue;
        }
        match read_tag(text, lt) {
            Some((_, after)) => {
                pos = after;
                i = after;
            }
            None => i = lt + 1,
        }
    }
    if pos < content_end {
        let node = &text[pos..content_end];
        if !node.trim().is_empty() {
            out.push((pos, content_end));
        }
    }
    out
}

/// Trims ASCII whitespace off a byte span.
pub(crate) fn trim_span(text: &str, start: usize, end: usize) -> (usize, usize) {
    let bytes = text.as_bytes();
    let mut s = start.min(bytes.len());
    let mut e = end.min(bytes.len());
    while s < e && bytes[s].is_ascii_whitespace() {
        s += 1;
    }
    while e > s && bytes[e - 1].is_ascii_whitespace() {
        e -= 1;
    }
    (s, e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nesting_and_content_are_tracked() {
        let text = "<mapper><select id=\"a\">select <if test=\"x\">1</if></select></mapper>";
        let els = elements(text);
        assert_eq!(els.len(), 3);
        assert_eq!(els[0].name, "mapper");
        assert_eq!(els[1].name, "select");
        assert_eq!(els[1].parent, Some(0));
        assert_eq!(
            &text[els[1].content_start..els[1].content_end],
            "select <if test=\"x\">1</if>"
        );
        assert_eq!(els[2].parent, Some(1));
        assert_eq!(els[0].content_end, text.rfind("</mapper>").unwrap());
    }

    #[test]
    fn comments_and_cdata_hide_tags() {
        let text =
            "<mapper><!-- <select id=\"no\"/> --><sql id=\"s\"><![CDATA[a < b]]></sql></mapper>";
        let els = elements(text);
        assert_eq!(
            els.iter().map(|e| e.name).collect::<Vec<_>>(),
            vec!["mapper", "sql"]
        );
    }

    #[test]
    fn text_nodes_skip_whitespace_and_tags() {
        let text = "<select>\n  select\n  <if test=\"x\">and 1</if>\n</select>";
        let els = elements(text);
        let nodes = text_nodes(text, els[0].content_start, els[0].content_end);
        let texts: Vec<&str> = nodes.iter().map(|&(s, e)| &text[s..e]).collect();
        assert_eq!(texts, vec!["\n  select\n  ", "and 1"]);
    }

    #[test]
    fn attribute_values_may_hold_gt() {
        let text = "<select id=\"a\" test=\"x &gt; y\">t</select>";
        let els = elements(text);
        assert_eq!(els[0].attr("test").map(|a| a.value), Some("x &gt; y"));
        assert_eq!(els[0].attr("id").map(|a| a.value), Some("a"));
    }
}
