//! Mapper interface (Java) → facts (T3-1).
//!
//! The scan is byte-wise and single-pass over the text: comments and string
//! literals are skipped, the first top-level type declaration is taken, its
//! member list is split at `;`/`}` boundaries, and each member is read for its
//! annotations, name and parameters. Nothing here needs PSI, a symbol table or
//! the file system; annotated SQL is a plain string, exactly as MyBatis reads it.

use super::scan::Positions;
use super::{Named, StatementKind};
use openidea_plugin::Range;

/// What the top-level type declares itself as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeKind {
    Interface,
    Class,
    Enum,
    Record,
}

impl TypeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TypeKind::Interface => "interface",
            TypeKind::Class => "class",
            TypeKind::Enum => "enum",
            TypeKind::Record => "record",
        }
    }
}

/// `@Select` / `@Insert` / `@Update` / `@Delete` SQL on one method.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JavaSql {
    pub kind: StatementKind,
    /// The annotation's string values joined with one space, trimmed — the
    /// text MyBatis builds (`String.join(" ", value)`).
    pub sql: String,
    /// Absolute byte spans of every string literal that makes up `sql`.
    pub strings: Vec<(usize, usize)>,
    /// The same spans as UTF-16 ranges.
    pub ranges: Vec<Range>,
    /// 0-based line of the annotation name.
    pub line: u32,
}

/// `@SelectProvider`-style indirection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JavaProvider {
    /// `type = Foo.class` (the literal's text range, name as written).
    pub provider_type: Option<Named>,
    /// `method = "name"`.
    pub method: Option<Named>,
}

/// One method of the mapper interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JavaMethod {
    pub name: Named,
    /// 0-based line of the declaration name.
    pub line: u32,
    /// Effective parameter names: `@Param("…")` wins, else the parameter's last
    /// identifier.
    pub params: Vec<Named>,
    pub sql: Vec<JavaSql>,
    pub provider: Option<JavaProvider>,
    /// `@ResultMap("…")` ids.
    pub result_maps: Vec<Named>,
    /// `@Results(id = "…")` ids this method declares.
    pub result_ids: Vec<Named>,
}

/// Everything one Java file states for the mapper graph.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JavaFacts {
    pub path: String,
    pub package: Option<String>,
    /// `import a.b.C;` pairs: (simple name, fully qualified name).
    pub imports: Vec<(String, String)>,
    pub type_name: Option<Named>,
    pub type_kind: Option<TypeKind>,
    /// `package.Type`.
    pub fqn: Option<String>,
    /// `@Mapper`, a `*Mapper` name, or a MyBatis(-Plus/MPJ) base interface.
    pub is_mapper: bool,
    /// `extends` / `implements` targets, resolved to fully qualified names when
    /// an import or the same package explains the simple name.
    pub supertypes: Vec<Named>,
    pub methods: Vec<JavaMethod>,
}

const MAPPER_BASES: [&str; 6] = [
    "com.baomidou.mybatisplus.core.mapper.BaseMapper",
    "com.baomidou.mybatisplus.extension.conditions.BaseMapper",
    "com.github.yulichang.base.MPJBaseMapper",
    "com.github.yulichang.base.Mapper",
    "com.baomidou.mybatisplus.mapper.BaseMapper",
    "org.apache.ibatis.annotations.Mapper",
];

const SQL_ANNOTATIONS: [(&str, StatementKind); 4] = [
    ("Select", StatementKind::Select),
    ("Insert", StatementKind::Insert),
    ("Update", StatementKind::Update),
    ("Delete", StatementKind::Delete),
];

const PROVIDER_ANNOTATIONS: [(&str, StatementKind); 4] = [
    ("SelectProvider", StatementKind::Select),
    ("InsertProvider", StatementKind::Insert),
    ("UpdateProvider", StatementKind::Update),
    ("DeleteProvider", StatementKind::Delete),
];

const KEYWORDS: [&str; 12] = [
    "if",
    "for",
    "while",
    "switch",
    "catch",
    "new",
    "return",
    "super",
    "this",
    "try",
    "synchronized",
    "assert",
];

/// Whether `b` continues a Java identifier. Bytes, not chars: the byte scanner
/// slices the text by these indices, so a UTF-8 lead byte must never count.
fn ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// The same test on a decoded char, for the char-based helpers.
fn ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The index just past the string or char literal opening at `i`.
fn skip_string(bytes: &[u8], mut i: usize) -> usize {
    let quote = bytes[i];
    i += 1;
    while i < bytes.len() {
        match bytes[i] {
            // Java literals cannot span lines; an unterminated quote ends at
            // the newline so one editing glitch cannot swallow the file.
            b'\n' | b'\r' => return i,
            b'\\' => i += 2,
            b if b == quote => return i + 1,
            _ => i += 1,
        }
    }
    i.min(bytes.len())
}

/// The index just past the `//…` or `/*…*/` comment opening at `i`.
fn skip_comment(bytes: &[u8], mut i: usize) -> usize {
    if bytes.get(i + 1) == Some(&b'/') {
        bytes[i + 2..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |n| i + 2 + n)
    } else {
        i += 2;
        while i + 1 < bytes.len() && (bytes[i] != b'*' || bytes[i + 1] != b'/') {
            i += 1;
        }
        (i + 2).min(bytes.len())
    }
}

fn skip_space(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

#[derive(Clone, Debug)]
struct Ann {
    /// Simple annotation name.
    name: String,
    /// Byte span of the arguments between the parentheses (relative to the
    /// scanned text).
    args: Option<(usize, usize)>,
    /// Byte offset of the `@`.
    at: usize,
}

/// Each `@Name(…)` in `text`: the simple name and its argument span, minding
/// nesting, strings and comments.
fn annotations_of(text: &str) -> Vec<Ann> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' => i = skip_string(bytes, i),
            b'/' if matches!(bytes.get(i + 1), Some(b'/' | b'*')) => i = skip_comment(bytes, i),
            b'@' => {
                let start = i;
                let mut j = i + 1;
                while j < bytes.len() && (ident_byte(bytes[j]) || bytes[j] == b'.') {
                    j += 1;
                }
                while j > start + 1 && !text.is_char_boundary(j) {
                    j -= 1;
                }
                let simple = text[start + 1..j]
                    .rsplit('.')
                    .next()
                    .unwrap_or("")
                    .to_owned();
                let k = skip_space(bytes, j);
                if bytes.get(k) == Some(&b'(') {
                    let Some(close) = matching(bytes, k) else {
                        break;
                    };
                    out.push(Ann {
                        name: simple,
                        args: Some((k + 1, close)),
                        at: start,
                    });
                    i = close + 1;
                } else {
                    out.push(Ann {
                        name: simple,
                        args: None,
                        at: start,
                    });
                    i = j.max(start + 1);
                }
            }
            _ => i += 1,
        }
    }
    out
}

/// Index of the bracket closing the one at `open`, minding strings, comments,
/// parentheses, braces and brackets together.
fn matching(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' => i = skip_string(bytes, i),
            b'/' if matches!(bytes.get(i + 1), Some(b'/' | b'*')) => i = skip_comment(bytes, i),
            b'(' | b'{' | b'[' => {
                depth += 1;
                i += 1;
            }
            b')' | b'}' | b']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(i);
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Declaration {
    name: (usize, usize),
    parens: (usize, usize),
}

/// The method-like declaration in `segment`: the identifier before the first
/// parenthesis that is not part of an annotation's arguments.
fn declaration_of(segment: &str) -> Option<Declaration> {
    let bytes = segment.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' => i = skip_string(bytes, i),
            b'/' if matches!(bytes.get(i + 1), Some(b'/' | b'*')) => i = skip_comment(bytes, i),
            b'@' => {
                i += 1;
                while i < bytes.len() && (ident_byte(bytes[i]) || bytes[i] == b'.') {
                    i += 1;
                }
                let k = skip_space(bytes, i);
                if bytes.get(k) == Some(&b'(') {
                    i = matching(bytes, k)? + 1;
                }
            }
            b'(' => {
                let before = segment[..i].trim_end();
                let start = before
                    .char_indices()
                    .rev()
                    .find(|&(_, c)| !ident(c))
                    .map_or(0, |(p, c)| p + c.len_utf8());
                if start >= before.len() {
                    return None;
                }
                let close = matching(bytes, i)?;
                return Some(Declaration {
                    name: (start, before.len()),
                    parens: (i, close),
                });
            }
            _ => i += 1,
        }
    }
    None
}

/// The trailing identifier of `part` and its byte span.
fn last_ident(part: &str) -> Option<(String, usize, usize)> {
    let chars: Vec<(usize, char)> = part.char_indices().collect();
    let mut end = chars.len();
    while end > 0 && !ident(chars[end - 1].1) {
        end -= 1;
    }
    if end == 0 {
        return None;
    }
    let mut start = end;
    while start > 0 && ident(chars[start - 1].1) {
        start -= 1;
    }
    let (s, _) = chars[start];
    let (e, c) = chars[end - 1];
    Some((part[s..e + c.len_utf8()].to_owned(), s, e + c.len_utf8()))
}

/// Parameter parts of a declaration's list, split at top-level commas with
/// generics, arrays, annotations and strings taken into account.
fn param_parts(list: &str) -> Vec<(usize, usize)> {
    let bytes = list.as_bytes();
    let mut parts = Vec::new();
    let (mut round, mut angle) = (0usize, 0usize);
    let mut start = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' => i = skip_string(bytes, i),
            b'/' if matches!(bytes.get(i + 1), Some(b'/' | b'*')) => i = skip_comment(bytes, i),
            b'(' | b'{' | b'[' => {
                round += 1;
                i += 1;
            }
            b')' | b'}' | b']' => {
                round = round.saturating_sub(1);
                i += 1;
            }
            b'<' if i > 0 && ident_byte(bytes[i - 1]) => {
                angle += 1;
                i += 1;
            }
            b'>' if angle > 0 => {
                angle -= 1;
                i += 1;
            }
            b',' if round == 0 && angle == 0 => {
                parts.push((start, i));
                start = i + 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    if start < list.len() {
        parts.push((start, list.len()));
    }
    parts
}

/// The effective name of a parameter part and its byte span within `list`.
fn param_span(list: &str, part: (usize, usize)) -> Option<(String, usize, usize)> {
    let (s, e) = part;
    let text = &list[s..e];
    if let Some(q) = text.find("@Param") {
        let bytes = text.as_bytes();
        let open = skip_space(bytes, q + 6);
        if bytes.get(open) == Some(&b'(') {
            let mut i = skip_space(bytes, open + 1);
            if bytes.get(i) == Some(&b'"') {
                i += 1;
                if let Some(n) = text[i..].find('"') {
                    let end = i + n;
                    if end > i {
                        return Some((text[i..end].to_owned(), s + i, s + end));
                    }
                }
            }
        }
    }
    last_ident(text).map(|(name, ps, pe)| (name, s + ps, s + pe))
}

/// String literals in `args`: `(value, value start, value end)`, quotes
/// excluded. Unterminated literals are skipped.
fn string_literals(args: &str) -> Vec<(String, usize, usize)> {
    let bytes = args.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            let end = skip_string(bytes, i);
            if end > i + 1 && bytes[end - 1] == b'"' {
                out.push((args[i + 1..end - 1].to_owned(), i + 1, end - 1));
                i = end;
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    out
}

/// The first string-valued `key = "…"` at nesting depth 0.
fn string_arg(args: &str, key: &str) -> Option<(String, usize, usize)> {
    let bytes = args.as_bytes();
    let mut i = 0;
    let mut depth = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' => i = skip_string(bytes, i),
            b'/' if matches!(bytes.get(i + 1), Some(b'/' | b'*')) => i = skip_comment(bytes, i),
            b'(' | b'{' | b'[' => {
                depth += 1;
                i += 1;
            }
            b')' | b'}' | b']' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b if depth == 0 && b.is_ascii_alphabetic() => {
                let start = i;
                while i < bytes.len() && ident_byte(bytes[i]) {
                    i += 1;
                }
                let word = &args[start..i];
                let after = skip_space(bytes, i);
                if word == key && bytes.get(after) == Some(&b'=') {
                    let mut v = skip_space(bytes, after + 1);
                    if bytes.get(v) == Some(&b'"') {
                        v += 1;
                        let end = args[v..].find('"').map(|n| v + n)?;
                        return Some((args[v..end].to_owned(), v, end));
                    }
                }
            }
            _ => i += 1,
        }
    }
    None
}

/// `type = a.b.C.class`: the class name as written and the span of that name.
fn class_literal(args: &str) -> Option<(String, usize, usize)> {
    let bytes = args.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' => i = skip_string(bytes, i),
            b'/' if matches!(bytes.get(i + 1), Some(b'/' | b'*')) => i = skip_comment(bytes, i),
            b if is_name_byte(b) => {
                let start = i;
                while i < bytes.len() && (is_name_byte(bytes[i]) || bytes[i] == b'.') {
                    i += 1;
                }
                let name = &args[start..i];
                if name.ends_with(".class") {
                    let cut = name.len() - ".class".len();
                    if cut > 0 {
                        return Some((name[..cut].to_owned(), start, start + cut));
                    }
                }
                // `Foo . class` with spaces is not legal Java, so the name is
                // the whole chain ending in `.class` or nothing.
            }
            _ => i += 1,
        }
    }
    None
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$')
}

/// `extends`/`implements` targets in a type header, as written (generics
/// stripped).
fn supertype_texts(header: &str, base: usize) -> Vec<(String, usize, usize)> {
    let bytes = header.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' => i = skip_string(bytes, i),
            b'/' if matches!(bytes.get(i + 1), Some(b'/' | b'*')) => i = skip_comment(bytes, i),
            b'@' => {
                // An annotation on the type (e.g. `@Mapper`) may hold commas.
                i += 1;
                while i < bytes.len() && (ident_byte(bytes[i]) || bytes[i] == b'.') {
                    i += 1;
                }
                let k = skip_space(bytes, i);
                if bytes.get(k) == Some(&b'(') {
                    i = matching(bytes, k).map_or(bytes.len(), |c| c + 1);
                }
            }
            b if b.is_ascii_alphabetic() => {
                let start = i;
                while i < bytes.len() && ident_byte(bytes[i]) {
                    i += 1;
                }
                let word = &header[start..i];
                if word != "extends" && word != "implements" {
                    continue;
                }
                let clause_start = i;
                let mut end = bytes.len();
                let mut j = i;
                let mut round = 0usize;
                while j < bytes.len() {
                    match bytes[j] {
                        b'<' => {
                            round += 1;
                            j += 1;
                        }
                        b'>' => {
                            round = round.saturating_sub(1);
                            j += 1;
                        }
                        b'"' | b'\'' => j = skip_string(bytes, j),
                        b'/' if matches!(bytes.get(j + 1), Some(b'/' | b'*')) => {
                            j = skip_comment(bytes, j)
                        }
                        b if round == 0 && b.is_ascii_alphabetic() => {
                            let w = j;
                            let mut k = j;
                            while k < bytes.len() && ident_byte(bytes[k]) {
                                k += 1;
                            }
                            let next = &header[w..k];
                            if next == "implements" || next == "extends" {
                                end = w;
                                break;
                            }
                            j = k;
                        }
                        _ => j += 1,
                    }
                }
                for (s, e) in split_commas(header, clause_start, end) {
                    if let Some((name, ns, ne)) = trailing_name(&header[s..e]) {
                        out.push((name, base + s + ns, base + s + ne));
                    }
                }
                i = end;
            }
            _ => i += 1,
        }
    }
    out
}

/// Top-level comma splits inside `start..end` (generic angle depth ignored —
/// a type argument can hold a comma: `A<B, C, D>`).
fn split_commas(text: &str, start: usize, end: usize) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut angle = 0usize;
    let mut s = start;
    let mut i = start;
    while i < end {
        match bytes[i] {
            b'<' => {
                angle += 1;
                i += 1;
            }
            b'>' => {
                angle = angle.saturating_sub(1);
                i += 1;
            }
            b',' if angle == 0 => {
                out.push((s, i));
                s = i + 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    if s < end {
        out.push((s, end));
    }
    out
}

/// The last dotted name in `part`, before any `<` (its generics).
fn trailing_name(part: &str) -> Option<(String, usize, usize)> {
    let cut = part.find('<').unwrap_or(part.len());
    let head = &part[..cut];
    let trimmed_end = head.trim_end().len();
    let chars: Vec<(usize, char)> = head[..trimmed_end].char_indices().collect();
    let mut end = chars.len();
    while end > 0 && !(ident(chars[end - 1].1) || chars[end - 1].1 == '.') {
        end -= 1;
    }
    if end == 0 {
        return None;
    }
    let mut start = end;
    while start > 0 && (ident(chars[start - 1].1) || chars[start - 1].1 == '.') {
        start -= 1;
    }
    let (s, _) = chars[start];
    let (e, c) = chars[end - 1];
    let name = &head[s..e + c.len_utf8()];
    let name = name.trim_matches('.').to_owned();
    if name.is_empty() {
        return None;
    }
    Some((name, s, e + c.len_utf8()))
}

/// Member spans of a type body: at `;` and after a member's block closes.
///
/// A block only ends a member when it opens at member level and outside
/// parentheses — `@Select({"a", "b"})` opens a brace inside the annotation's
/// argument list, and that brace must not cut the member in two.
fn member_spans(text: &str, start: usize, end: usize) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut paren = 0usize;
    let mut blocks: Vec<bool> = Vec::new();
    let mut s = start;
    let mut i = start;
    while i < end {
        match bytes[i] {
            b'"' | b'\'' => i = skip_string(bytes, i),
            b'/' if matches!(bytes.get(i + 1), Some(b'/' | b'*')) => i = skip_comment(bytes, i),
            b'(' => {
                paren += 1;
                i += 1;
            }
            b')' => {
                paren = paren.saturating_sub(1);
                i += 1;
            }
            b'{' => {
                blocks.push(paren == 0);
                depth += 1;
                i += 1;
            }
            b'}' => {
                let body = blocks.pop().unwrap_or(false);
                depth = depth.saturating_sub(1);
                if depth == 0 && body {
                    out.push((s, i + 1));
                    s = i + 1;
                }
                i += 1;
            }
            b';' if depth == 0 => {
                out.push((s, i));
                s = i + 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    if s < end {
        out.push((s, end));
    }
    out
}

fn is_keyword(name: &str) -> bool {
    KEYWORDS.contains(&name)
}

/// The qualified name in `text[start..end]`, trimmed.
fn trim_name(text: &str, start: usize, end: usize) -> String {
    text[start..end].trim().to_owned()
}

/// Reads one Java file. Non-mapper types still yield their names, so the graph
/// can link `type:` keys; statement facts only exist for a mapper.
pub fn index_java(path: &str, text: &str) -> JavaFacts {
    let mut facts = JavaFacts {
        path: path.to_owned(),
        ..JavaFacts::default()
    };
    let bytes = text.as_bytes();
    let mut positions = Positions::new(text);

    // ---- header: package, imports, the first top-level type ----
    let mut package: Option<String> = None;
    let mut type_kind: Option<TypeKind> = None;
    let mut type_name_span: Option<(usize, usize)> = None;
    let mut header_span: Option<(usize, usize)> = None;
    let mut body: Option<(usize, usize)> = None;
    let mut i = 0;
    while i < bytes.len() && body.is_none() {
        match bytes[i] {
            b'"' | b'\'' => i = skip_string(bytes, i),
            b'/' if matches!(bytes.get(i + 1), Some(b'/' | b'*')) => {
                i = skip_comment(bytes, i);
            }
            b'{' | b'}' | b';' => {
                i += 1;
            }
            b'@' => {
                // Annotations are part of the header segment; skip their args.
                i += 1;
                while i < bytes.len() && (ident_byte(bytes[i]) || bytes[i] == b'.') {
                    i += 1;
                }
                let k = skip_space(bytes, i);
                if bytes.get(k) == Some(&b'(') {
                    i = matching(bytes, k).map_or(bytes.len(), |c| c + 1);
                }
            }
            b if b.is_ascii_alphabetic() => {
                let word_start = i;
                while i < bytes.len() && ident_byte(bytes[i]) {
                    i += 1;
                }
                let word = &text[word_start..i];
                match word {
                    "package" => {
                        let end = find_byte(bytes, i, b';');
                        package = Some(trim_name(text, i, end));
                        i = end;
                    }
                    "import" => {
                        let end = find_byte(bytes, i, b';');
                        let mut name = text[i..end].trim();
                        if let Some(stripped) = name.strip_prefix("static ") {
                            name = stripped.trim();
                        }
                        let name = name.strip_suffix(".*").unwrap_or(name).to_owned();
                        if let Some(simple) = name.rsplit('.').next() {
                            facts.imports.push((simple.to_owned(), name.clone()));
                        }
                        i = end;
                    }
                    "interface" | "class" | "enum" | "record" => {
                        let kind = match word {
                            "interface" => TypeKind::Interface,
                            "enum" => TypeKind::Enum,
                            "record" => TypeKind::Record,
                            _ => TypeKind::Class,
                        };
                        let name_start = skip_space(bytes, i);
                        let mut name_end = name_start;
                        while name_end < bytes.len() && ident_byte(bytes[name_end]) {
                            name_end += 1;
                        }
                        if name_end > name_start {
                            type_kind = Some(kind);
                            type_name_span = Some((name_start, name_end));
                            header_span = Some((name_end, bytes.len()));
                            // The type body: the next `{` outside strings.
                            let mut j = name_end;
                            loop {
                                match bytes.get(j) {
                                    None => break,
                                    Some(b'"') | Some(b'\'') => j = skip_string(bytes, j),
                                    Some(b'/') if matches!(bytes.get(j + 1), Some(b'/' | b'*')) => {
                                        j = skip_comment(bytes, j)
                                    }
                                    Some(b'{') => {
                                        if let Some(close) = matching(bytes, j) {
                                            body = Some((j + 1, close));
                                            header_span = Some((name_end, j));
                                        }
                                        break;
                                    }
                                    _ => j += 1,
                                }
                            }
                            i = j;
                        }
                    }
                    _ => {}
                }
            }
            _ => i += 1,
        }
    }

    if let (Some((ns, ne)), Some(kind)) = (type_name_span, type_kind) {
        facts.type_kind = Some(kind);
        facts.type_name = Some(Named {
            name: text[ns..ne].to_owned(),
            range: positions.range(ns, ne),
        });
        let simple = &text[ns..ne];
        facts.fqn = Some(match &package {
            Some(p) if !p.is_empty() => format!("{p}.{simple}"),
            _ => simple.to_owned(),
        });
        if let Some((hs, he)) = header_span {
            let header = &text[hs..he];
            let annotated = annotations_of(header).iter().any(|a| a.name == "Mapper");
            for (raw, s, e) in supertype_texts(header, hs) {
                let resolved = resolve_type(&raw, &package, &facts.imports);
                facts.supertypes.push(Named {
                    name: resolved,
                    range: positions.range(s, e),
                });
            }
            facts.is_mapper = annotated
                || simple.ends_with("Mapper")
                || simple.ends_with("Dao")
                || facts
                    .supertypes
                    .iter()
                    .any(|s| MAPPER_BASES.contains(&s.name.as_str()));
        }
        facts.package = package.clone();
    }

    let Some((body_start, body_end)) = body else {
        return facts;
    };
    for (start, end) in member_spans(text, body_start, body_end) {
        let segment = &text[start..end];
        let anns = annotations_of(segment);
        let Some(decl) = declaration_of(segment) else {
            continue;
        };
        let name = &segment[decl.name.0..decl.name.1];
        if name.is_empty() || is_keyword(name) {
            continue;
        }
        let list = &segment[decl.parens.0 + 1..decl.parens.1];
        let mut method = JavaMethod {
            name: Named {
                name: name.to_owned(),
                range: positions.range(start + decl.name.0, start + decl.name.1),
            },
            line: positions.at(start + decl.name.0).line,
            params: Vec::new(),
            sql: Vec::new(),
            provider: None,
            result_maps: Vec::new(),
            result_ids: Vec::new(),
        };

        for part in param_parts(list) {
            if let Some((param, s, e)) = param_span(list, part) {
                let span = (start + decl.parens.0 + 1 + s, start + decl.parens.0 + 1 + e);
                method.params.push(Named {
                    name: param,
                    range: positions.range(span.0, span.1),
                });
            }
        }

        for ann in &anns {
            if let Some(&(_, kind)) = SQL_ANNOTATIONS.iter().find(|(n, _)| *n == ann.name) {
                let Some((as_, ae)) = ann.args else { continue };
                let args = &segment[as_..ae];
                let literals = string_literals(args);
                if literals.is_empty() {
                    continue;
                }
                let sql = literals
                    .iter()
                    .map(|l| l.0.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
                    .trim()
                    .to_owned();
                if sql.is_empty() {
                    continue;
                }
                let mut strings = Vec::new();
                let mut ranges = Vec::new();
                for (_, s, e) in &literals {
                    strings.push((start + as_ + s, start + as_ + e));
                    ranges.push(positions.range(start + as_ + s, start + as_ + e));
                }
                let line = positions.at(start + ann.at).line;
                method.sql.push(JavaSql {
                    kind,
                    sql,
                    strings,
                    ranges,
                    line,
                });
            } else if PROVIDER_ANNOTATIONS.iter().any(|(n, _)| *n == ann.name) {
                let (args, base) = match ann.args {
                    Some((s, e)) => (&segment[s..e], Some(s)),
                    None => ("", None),
                };
                let mut provider = JavaProvider {
                    provider_type: None,
                    method: None,
                };
                if let Some(base) = base {
                    if let Some((raw, s, e)) = class_literal(args) {
                        let resolved = resolve_type(&raw, &package, &facts.imports);
                        provider.provider_type = Some(Named {
                            name: resolved,
                            range: positions.range(start + base + s, start + base + e),
                        });
                    }
                    if let Some((value, s, e)) = string_arg(args, "method") {
                        provider.method = Some(Named {
                            name: value,
                            range: positions.range(start + base + s, start + base + e),
                        });
                    }
                }
                if method.provider.is_none() {
                    method.provider = Some(provider);
                }
            } else if ann.name == "ResultMap" {
                if let Some((s, e)) = ann.args {
                    for (value, ls, le) in string_literals(&segment[s..e]) {
                        method.result_maps.push(Named {
                            name: value,
                            range: positions.range(start + s + ls, start + s + le),
                        });
                    }
                }
            } else if ann.name == "Results"
                && let Some((s, e)) = ann.args
                && let Some((value, ls, le)) = string_arg(&segment[s..e], "id")
            {
                method.result_ids.push(Named {
                    name: value,
                    range: positions.range(start + s + ls, start + s + le),
                });
            }
        }
        facts.methods.push(method);
    }
    facts
}

fn find_byte(bytes: &[u8], from: usize, byte: u8) -> usize {
    bytes[from..]
        .iter()
        .position(|&b| b == byte)
        .map_or(bytes.len(), |i| from + i)
}

/// Resolves a type as written: a qualified name stays as is, a simple name
/// follows its import, otherwise it lives in the same package.
pub fn resolve_type(raw: &str, package: &Option<String>, imports: &[(String, String)]) -> String {
    let raw = raw.trim();
    if raw.contains('.') {
        return raw.to_owned();
    }
    if let Some((_, fqn)) = imports.iter().find(|(simple, _)| simple == raw) {
        return fqn.clone();
    }
    match package {
        Some(p) if !p.is_empty() => format!("{p}.{raw}"),
        _ => raw.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn chinese_comments_do_not_break_the_java_facts() {
        let source = r#"package com.acme;
/** 金仓 GoldenDB 方言：中文注释 */
public interface GoldenDbDeviceMapper extends BaseMapper<GoldenDbDevice> {
    @Delete("DELETE FROM `gdb_device` ORDER BY `id` DESC LIMIT #{n}")
    int deleteNewest(@Param("n") int n);
}
"#;
        let facts = index_java("/p/GoldenDbDeviceMapper.java", source);
        assert_eq!(facts.fqn.as_deref(), Some("com.acme.GoldenDbDeviceMapper"));
        assert!(facts.is_mapper);
        assert!(
            facts.methods.iter().any(|m| m.name.name == "deleteNewest"),
            "{:?}",
            facts
                .methods
                .iter()
                .map(|m| &m.name.name)
                .collect::<Vec<_>>()
        );
    }

    use super::*;

    const JAVA: &str = r#"package com.acme.user;

import com.baomidou.mybatisplus.core.mapper.BaseMapper;
import org.apache.ibatis.annotations.*;

@Mapper
public interface UserMapper extends BaseMapper<User> {
    @Select("select id, name from t_user where id = #{id}")
    User findById(@Param("id") Long id);

    @UpdateProvider(type = SqlFactory.class, method = "updateSql")
    int update(User user);

    @Select({"select count(1)", "from t_user"})
    long count(@Param("deptId") Long deptId, String name);

    @Results(id = "userMap")
    @ResultMap("userMap")
    java.util.List<User> all();
}
"#;

    #[test]
    fn reads_type_package_and_supertypes() {
        let facts = index_java("/p/src/main/java/com/acme/user/UserMapper.java", JAVA);
        assert_eq!(facts.package.as_deref(), Some("com.acme.user"));
        assert_eq!(facts.fqn.as_deref(), Some("com.acme.user.UserMapper"));
        assert_eq!(facts.type_kind, Some(TypeKind::Interface));
        assert!(facts.is_mapper);
        assert_eq!(
            facts
                .supertypes
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            vec!["com.baomidou.mybatisplus.core.mapper.BaseMapper"]
        );
    }

    #[test]
    fn reads_methods_parameters_and_annotated_sql() {
        let facts = index_java("UserMapper.java", JAVA);
        let names: Vec<&str> = facts.methods.iter().map(|m| m.name.name.as_str()).collect();
        assert_eq!(names, vec!["findById", "update", "count", "all"]);
        assert_eq!(facts.methods[0].sql[0].kind, StatementKind::Select);
        assert_eq!(
            facts.methods[0].sql[0].sql,
            "select id, name from t_user where id = #{id}"
        );
        assert_eq!(facts.methods[0].params[0].name, "id");
        assert_eq!(facts.methods[2].sql[0].sql, "select count(1) from t_user");
        assert_eq!(
            facts.methods[2]
                .params
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec!["deptId", "name"]
        );
    }

    #[test]
    fn reads_provider_annotations_and_result_map_ids() {
        let facts = index_java("UserMapper.java", JAVA);
        let update = &facts.methods[1];
        let provider = update.provider.as_ref().unwrap();
        assert_eq!(
            provider.provider_type.as_ref().unwrap().name,
            "com.acme.user.SqlFactory"
        );
        assert_eq!(provider.method.as_ref().unwrap().name, "updateSql");
        assert_eq!(facts.methods[3].result_ids[0].name, "userMap");
        assert_eq!(facts.methods[3].result_maps[0].name, "userMap");
    }

    #[test]
    fn parameter_span_points_at_the_param_string() {
        let facts = index_java("UserMapper.java", JAVA);
        let range = facts.methods[0].params[0].range;
        let line = JAVA.lines().nth(range.start.line as usize).unwrap();
        assert_eq!(
            &line[range.start.character as usize..range.end.character as usize],
            "id"
        );
        assert!(line.contains("@Param(\"id\")"));
    }

    #[test]
    fn a_non_mapper_type_is_still_named() {
        let facts = index_java("Model.java", "package a.b;\npublic class Model { int x; }");
        assert_eq!(facts.fqn.as_deref(), Some("a.b.Model"));
        assert_eq!(facts.type_kind, Some(TypeKind::Class));
        assert!(!facts.is_mapper);
        assert!(facts.methods.is_empty());
    }
}
