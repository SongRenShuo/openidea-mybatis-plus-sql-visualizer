//! Text-only Java method index: `class#method` → file:line, without PSI.
//!
//! The platform exposes no type resolution to the plugin, so the only way to
//! point at the method a log row or a mapper statement came from is to read
//! Java sources as text. This module does exactly that and nothing else:
//!
//! - `package` + top-level `class` / `interface` / `enum` / `record` +
//!   nested types → fully qualified names;
//! - member-level method declarations (modifiers, generics, `throws`,
//!   `@Override`) → name, line, parameter count;
//! - comments, string literals, char literals and text blocks (`"""…"""`) are
//!   skipped, so a method name mentioned in prose or in a SQL string is never
//!   a hit.
//!
//! It is a scanner, not a parser: a truncated or syntactically broken file is
//! skipped and counted, never a panic. Everything operates on `&str` handed in
//! by the host (the crate is wasm; there is no file system here), and the
//! budget mirrors the document indexer's own (2 MiB a document).
//!
//! ## Matching is conservative
//!
//! Lookups never pick one of several candidates. Spring shortens package
//! prefixes in the logger name (`com.example.myapp` → `c.e.m`), so a class is
//! matched segment-wise from the right: a written segment that is a single
//! letter matches a real segment starting with that letter, everything else
//! must be equal. Ambiguity (two `UserMapper` types, an overload set) yields
//! more than one location; [`JavaMethodIndex::lookup_unique`] then answers
//! `None` instead of guessing.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::core::log::SourceRef;

/// Directory names never indexed, wherever they appear in a path.
///
/// `target`/`build`/`out`/`node_modules` are build output; `.worktrees` keeps a
/// development checkout's other lanes out of its own index.
const EXCLUDED_DIRS: [&str; 9] = [
    "target",
    "build",
    "out",
    "bin",
    "node_modules",
    ".git",
    ".gradle",
    ".worktrees",
    ".idea",
];

/// Keyword-led statements that are not method declarations.
const NOT_METHODS: [&str; 12] = [
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

/// Caps for one project's Java index. The per-file cap is the document
/// indexer's own 2 MiB; the totals keep a generated-source-heavy repository
/// (protobuf builders, `target/` copies) from growing the wasm heap without
/// bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JavaIndexBudget {
    pub max_files: usize,
    pub max_file_bytes: usize,
    pub max_total_bytes: usize,
}

impl Default for JavaIndexBudget {
    fn default() -> Self {
        Self {
            max_files: 4096,
            max_file_bytes: 2 * 1024 * 1024,
            max_total_bytes: 16 * 1024 * 1024,
        }
    }
}

/// One method declaration, as the text states it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct JavaMethodLocation {
    /// Path exactly as the host handed it in.
    pub file: String,
    /// `com.acme.mapper.UserMapper`, nested types joined with `.`.
    pub fqn: String,
    /// Last segment of [`Self::fqn`].
    pub class_simple: String,
    pub method: String,
    /// 0-based, the same convention as `Position::line` (`Range`).
    pub line: u32,
    /// Declared parameter count, which is what tells overloads apart.
    pub arity: usize,
}

impl JavaMethodLocation {
    /// 1-based line, for anything a human reads (`Foo.java:42`).
    pub fn display_line(&self) -> u32 {
        self.line + 1
    }
}

/// A type declaration found while scanning one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TypeKind {
    Class,
    Interface,
    Enum,
    Record,
}

/// One file's accounting, so a reload can drop exactly its methods.
#[derive(Debug, Clone, Default)]
struct FileSlot {
    bytes: usize,
    methods: Vec<usize>,
    /// `(simple, fqn)` of every type the file declared.
    declared: Vec<(String, String)>,
}

/// The resolved method/class index of one project.
#[derive(Debug, Clone, Default)]
pub struct JavaMethodIndex {
    budget: JavaIndexBudget,
    /// Method slots; `None` is a location dropped by [`Self::remove_file`].
    methods: Vec<Option<JavaMethodLocation>>,
    /// `method` → slots. Keyed by the bare name because a class is matched
    /// segment-wise afterwards: that makes a lookup cost "methods with this name",
    /// not "every class in the project".
    by_method: HashMap<String, Vec<usize>>,
    /// `Simple` → fully qualified names, including types that declare no
    /// method (a marker interface still resolves).
    classes: HashMap<String, Vec<String>>,
    files: HashMap<String, FileSlot>,
    bytes: usize,
    dead: usize,
    skipped: usize,
}

impl JavaMethodIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_budget(budget: JavaIndexBudget) -> Self {
        Self {
            budget,
            ..Self::default()
        }
    }

    /// Number of live method declarations.
    pub fn len(&self) -> usize {
        self.methods.len() - self.dead
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Files currently contributing to the index.
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Files refused by the budget, the exclusion list, or truncated syntax.
    pub fn skipped_files(&self) -> usize {
        self.skipped
    }

    /// Bytes of source text currently indexed.
    pub fn indexed_bytes(&self) -> usize {
        self.bytes
    }

    /// Indexes one Java source. Returns how many methods it added.
    ///
    /// Re-adding the same path replaces that file's methods. A path outside the
    /// rules (not `.java`, excluded directory, over budget) adds nothing and
    /// counts as skipped.
    pub fn add_file(&mut self, path: &str, text: &str) -> usize {
        if !path.to_ascii_lowercase().ends_with(".java") || excluded_path(path) {
            self.skipped += 1;
            return 0;
        }
        let known = self.files.contains_key(path);
        let too_many_files = !known && self.files.len() >= self.budget.max_files;
        if text.len() > self.budget.max_file_bytes
            || too_many_files
            || self.bytes.saturating_add(text.len()) > self.budget.max_total_bytes
        {
            self.skipped += 1;
            return 0;
        }
        if known {
            self.remove_file(path);
        }

        let (locations, declared, truncated) = scan_file(path, text);
        if truncated {
            self.skipped += 1;
        }
        let mut slots = Vec::with_capacity(locations.len());
        for location in locations {
            let index = self.methods.len();
            self.by_method
                .entry(location.method.clone())
                .or_default()
                .push(index);
            self.methods.push(Some(location));
            slots.push(index);
        }
        for (simple, fqn) in &declared {
            let known = self.classes.entry(simple.clone()).or_default();
            if !known.contains(fqn) {
                known.push(fqn.clone());
            }
        }
        self.bytes += text.len();
        self.files.insert(
            path.to_owned(),
            FileSlot {
                bytes: text.len(),
                methods: slots.clone(),
                declared,
            },
        );
        slots.len()
    }

    /// Drops every method `path` contributed, leaving the rest addressable.
    pub fn remove_file(&mut self, path: &str) -> usize {
        let Some(slot) = self.files.remove(path) else {
            return 0;
        };
        self.bytes = self.bytes.saturating_sub(slot.bytes);
        for (simple, fqn) in &slot.declared {
            let still_declared = self
                .files
                .values()
                .any(|other| other.declared.iter().any(|(_, name)| name == fqn));
            if still_declared {
                continue;
            }
            if let Some(fqns) = self.classes.get_mut(simple) {
                fqns.retain(|existing| existing != fqn);
                if fqns.is_empty() {
                    self.classes.remove(simple);
                }
            }
        }
        for index in &slot.methods {
            if let Some(entry) = self.methods.get_mut(*index) {
                if entry.take().is_some() {
                    self.dead += 1;
                }
            }
        }
        if self.dead > 64 && self.dead * 2 > self.methods.len() {
            self.compact();
        }
        slot.methods.len()
    }

    /// Rewrites the slots and every lookup map without the dead entries.
    fn compact(&mut self) {
        let mut methods = Vec::with_capacity(self.len());
        let mut remap: HashMap<usize, usize> = HashMap::new();
        for (index, slot) in self.methods.iter().enumerate() {
            if let Some(location) = slot {
                remap.insert(index, methods.len());
                methods.push(Some(location.clone()));
            }
        }
        self.by_method.clear();
        let keys: Vec<(String, usize)> = methods
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                slot.as_ref()
                    .map(|location| (location.method.clone(), index))
            })
            .collect();
        for (method, index) in keys {
            self.by_method.entry(method).or_default().push(index);
        }
        for slot in self.files.values_mut() {
            let kept: Vec<usize> = slot
                .methods
                .iter()
                .filter_map(|index| remap.get(index).copied())
                .collect();
            slot.methods = kept;
        }
        self.methods = methods;
        self.dead = 0;
    }

    /// Methods named by `target`, written `FQN#method` or `Simple#method`.
    ///
    /// Overloads and same-named classes all come back: the caller decides, this
    /// index never picks one.
    pub fn lookup(&self, target: &str) -> Vec<&JavaMethodLocation> {
        let Some((class, method)) = target.split_once('#') else {
            return Vec::new();
        };
        let class = class.trim();
        if class.is_empty() || method.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut seen: HashSet<(&str, u32)> = HashSet::new();
        let Some(slots) = self.by_method.get(method) else {
            return out;
        };
        for slot in slots {
            let Some(Some(location)) = self.methods.get(*slot) else {
                continue;
            };
            if !class_matches(class, location) {
                continue;
            }
            if seen.insert((location.file.as_str(), location.line)) {
                out.push(location);
            }
        }
        out
    }

    /// The single location `target` names, or `None` when the answer is
    /// ambiguous or absent. Never guesses.
    pub fn lookup_unique(&self, target: &str) -> Option<&JavaMethodLocation> {
        let only = self.lookup(target);
        (only.len() == 1).then(|| only[0])
    }

    /// Fully qualified names `name` may denote.
    ///
    /// `name` may be a simple name (`UserMapper`), a full name, or the
    /// abbreviated prefix Spring prints in a logger name
    /// (`c.e.m.mapper.UserMapper`).
    pub fn lookup_class(&self, name: &str) -> Vec<String> {
        let name = name.trim();
        if name.is_empty() {
            return Vec::new();
        }
        let mut out: Vec<String> = Vec::new();
        if let Some(exact) = self.classes.get(name) {
            out.extend(exact.iter().cloned());
        }
        if name.contains('.') {
            // FQN or abbreviated FQN: compare segments from the right.
            for (_, fqns) in &self.classes {
                for fqn in fqns {
                    if out.contains(fqn) {
                        continue;
                    }
                    if dotted_suffix_matches(name, fqn) {
                        out.push(fqn.clone());
                    }
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// `namespace#statementId` → Java methods: the mapper XML↔interface join.
    ///
    /// `namespace` is the interface's fully qualified name; the method is
    /// matched by name only (parameters live in XML, not in the statement id),
    /// so a statement id that overloads several methods returns them all.
    pub fn resolve_statement(
        &self,
        namespace: &str,
        statement_id: &str,
    ) -> Vec<&JavaMethodLocation> {
        self.lookup(&format!("{namespace}#{statement_id}"))
    }

    /// Java methods a log row's logger-derived source points at.
    pub fn lookup_source(&self, source: &SourceRef) -> Vec<&JavaMethodLocation> {
        self.lookup(&source.key())
    }

    /// The single method a log row points at, or `None` when ambiguous.
    pub fn lookup_source_unique(&self, source: &SourceRef) -> Option<&JavaMethodLocation> {
        self.lookup_unique(&source.key())
    }
}

/// Does `written` (FQN, simple name or abbreviated FQN) denote `location`?
fn class_matches(written: &str, location: &JavaMethodLocation) -> bool {
    written == location.fqn
        || written == location.class_simple
        || (written.contains('.') && dotted_suffix_matches(written, &location.fqn))
}

/// `written` is a suffix of `real` (segment by segment), where a single-letter
/// written segment is Spring's shortening of a longer one.
fn dotted_suffix_matches(written: &str, real: &str) -> bool {
    let written: Vec<&str> = written.split('.').collect();
    let real: Vec<&str> = real.split('.').collect();
    if written.len() > real.len() {
        return false;
    }
    let mut offset = real.len() - written.len();
    for segment in &written {
        let candidate = real[offset];
        offset += 1;
        if *segment == candidate {
            continue;
        }
        // `c` for `com`, `e` for `example`: only a single letter shortens, and
        // only to a longer segment it starts.
        let mut chars = segment.chars();
        let only = chars.next();
        if chars.next().is_some() || only.is_none() {
            return false;
        }
        let only = only.unwrap_or('\0');
        if only.is_lowercase() && candidate.starts_with(only) && candidate.len() > 1 {
            continue;
        }
        return false;
    }
    true
}

/// True when any path segment is a build-output or tooling directory.
fn excluded_path(path: &str) -> bool {
    let normalised = path.replace('\\', "/");
    normalised
        .split('/')
        .any(|segment| EXCLUDED_DIRS.contains(&segment))
}

/// Reads one file: its methods, the types it declares, and whether the text
/// ended inside a type body (a half-written file cannot be trusted).
fn scan_file(path: &str, text: &str) -> (Vec<JavaMethodLocation>, Vec<(String, String)>, bool) {
    let mut scan = FileScan {
        path,
        text,
        package: package_of(text),
        methods: Vec::new(),
        declared: Vec::new(),
        truncated: false,
    };
    scan.scan_types(0, text.len(), None);
    (scan.methods, scan.declared, scan.truncated)
}

/// One file's scan state; a struct because the walk is recursive in two
/// dimensions (declaration regions and type bodies).
struct FileScan<'a> {
    path: &'a str,
    text: &'a str,
    package: Option<String>,
    methods: Vec<JavaMethodLocation>,
    /// `(simple, fqn)` of every declared type, method-less ones included.
    declared: Vec<(String, String)>,
    truncated: bool,
}

impl FileScan<'_> {
    /// Walks a region that may contain type declarations, recursing into every
    /// type body it finds.
    fn scan_types(&mut self, start: usize, end: usize, prefix: Option<&str>) {
        let text = self.text;
        let mut cursor = start;
        while cursor < end {
            let Some((kind, name_start, name_end, open)) = next_type_decl(text, cursor, end) else {
                break;
            };
            let close = match matching(text.as_bytes(), open) {
                Some(close) => close,
                _ => {
                    self.truncated = true;
                    break;
                }
            };
            let simple = &text[name_start..name_end];
            let fqn = match prefix {
                Some(parent) => format!("{parent}.{simple}"),
                None => match self.package.as_deref() {
                    Some(package) if !package.is_empty() => format!("{package}.{simple}"),
                    _ => simple.to_owned(),
                },
            };
            if !self.declared.iter().any(|(_, known)| known == &fqn) {
                self.declared.push((simple.to_owned(), fqn.clone()));
            }
            self.scan_members(open + 1, close, &fqn, simple, kind);
            cursor = close + 1;
        }
    }

    /// Records the methods declared directly in one type body.
    fn scan_members(
        &mut self,
        body_start: usize,
        body_end: usize,
        fqn: &str,
        simple: &str,
        kind: TypeKind,
    ) {
        let text = self.text;
        // An enum's constant list is not a member list; members start after the
        // first `;` at top level. Without one there are only constants.
        let start = match kind {
            TypeKind::Enum => match enum_members_start(text, body_start, body_end) {
                Some(start) => start,
                None => return,
            },
            _ => body_start,
        };
        for (member_start, member_end) in member_spans(text, start, body_end) {
            let segment = &text[member_start..member_end];
            // A nested type is a member too, but its declarations belong to it.
            if next_type_decl(segment, 0, segment.len()).is_some() {
                self.scan_types(member_start, member_end, Some(fqn));
                continue;
            }
            let Some(decl) = method_decl_of(segment) else {
                continue;
            };
            let name = &segment[decl.name.0..decl.name.1];
            if name == simple {
                // A constructor is not a statement method.
                continue;
            }
            self.methods.push(JavaMethodLocation {
                file: self.path.to_owned(),
                fqn: fqn.to_owned(),
                class_simple: simple.to_owned(),
                method: name.to_owned(),
                line: line_of(text, member_start + decl.name.0),
                arity: decl.arity,
            });
        }
    }
}

fn package_of(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' => i = skip_literal(bytes, i),
            b'/' if matches!(bytes.get(i + 1), Some(b'/' | b'*')) => i = skip_comment(bytes, i),
            b if ident_char(b as char) => {
                let start = i;
                while i < bytes.len() && ident_char(bytes[i] as char) {
                    i += 1;
                }
                if &text[start..i] != "package" {
                    continue;
                }
                let name_start = skip_trivia(bytes, i);
                let mut end = name_start;
                while end < bytes.len() && !matches!(bytes[end], b';' | b'{') {
                    end += 1;
                }
                let name = text[name_start..end].trim();
                return (!name.is_empty()).then(|| name.to_owned());
            }
            _ => i += 1,
        }
    }
    None
}

/// Finds the next `class` / `interface` / `enum` / `record` declaration in
/// `start..end`.
///
/// Returns `(kind, name span, index of the body's `{`)`. A `X.class` literal
/// (the word preceded by `.`) and the `Class<…>` type are not declarations.
fn next_type_decl(text: &str, start: usize, end: usize) -> Option<(TypeKind, usize, usize, usize)> {
    let bytes = text.as_bytes();
    let end = end.min(bytes.len());
    let mut i = start;
    while i < end {
        match bytes[i] {
            b'"' | b'\'' => i = skip_literal(bytes, i),
            b'/' if matches!(bytes.get(i + 1), Some(b'/' | b'*')) => i = skip_comment(bytes, i),
            b'@' => {
                i += 1;
                while i < end && (ident_char(bytes[i] as char) || bytes[i] == b'.') {
                    i += 1;
                }
                let args = skip_trivia(bytes, i);
                if bytes.get(args) == Some(&b'(')
                    && let Some(close) = matching(bytes, args)
                {
                    i = close + 1;
                }
            }
            b if ident_char(b as char) => {
                let word_start = i;
                while i < end && ident_char(bytes[i] as char) {
                    i += 1;
                }
                let kind = match &text[word_start..i] {
                    "class" => TypeKind::Class,
                    "interface" => TypeKind::Interface,
                    "enum" => TypeKind::Enum,
                    "record" => TypeKind::Record,
                    _ => continue,
                };
                // `Foo.class`, `obj.getClass()`: a type literal, not a type.
                let before = text[..word_start].trim_end();
                if before.ends_with('.') || before.ends_with('(') || before.ends_with("new") {
                    continue;
                }
                let name_start = skip_trivia(bytes, i);
                let mut name_end = name_start;
                while name_end < end && ident_char(bytes[name_end] as char) {
                    name_end += 1;
                }
                if name_end == name_start {
                    continue;
                }
                let Some(open) = body_open(text, name_end, end) else {
                    continue;
                };
                return Some((kind, name_start, name_end, open));
            }
            _ => i += 1,
        }
    }
    None
}

/// Index of the `{` opening a type body, searching past `extends`/`implements`
/// clauses and a record's component list.
fn body_open(text: &str, from: usize, end: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = from;
    while i < end {
        match bytes[i] {
            b'"' | b'\'' => i = skip_literal(bytes, i),
            b'/' if matches!(bytes.get(i + 1), Some(b'/' | b'*')) => i = skip_comment(bytes, i),
            b'{' => return Some(i),
            b';' => return None,
            b'(' => i = matching(bytes, i).map_or(end, |close| close + 1),
            b'@' => {
                i += 1;
                while i < end && (ident_char(bytes[i] as char) || bytes[i] == b'.') {
                    i += 1;
                }
                let args = skip_trivia(bytes, i);
                if bytes.get(args) == Some(&b'(')
                    && let Some(close) = matching(bytes, args)
                {
                    i = close + 1;
                }
            }
            _ => i += 1,
        }
    }
    None
}

/// Offset just past the enum constant list's `;`.
fn enum_members_start(text: &str, start: usize, end: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = start;
    let mut depth = 0usize;
    while i < end {
        match bytes[i] {
            b'"' | b'\'' => i = skip_literal(bytes, i),
            b'/' if matches!(bytes.get(i + 1), Some(b'/' | b'*')) => i = skip_comment(bytes, i),
            b'{' | b'(' | b'[' => {
                depth += 1;
                i += 1;
            }
            b'}' | b')' | b']' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b';' if depth == 0 => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// A method declaration's name span and parameter count.
#[derive(Debug, Clone, Copy)]
struct MethodDecl {
    name: (usize, usize),
    arity: usize,
}

/// The method declaration in `segment`, or `None` for a field, an initializer
/// or anything else that only looks like a call.
fn method_decl_of(segment: &str) -> Option<MethodDecl> {
    let bytes = segment.as_bytes();
    let mut i = 0;
    // The first `(` that is not an annotation's argument list.
    let paren = loop {
        if i >= bytes.len() {
            return None;
        }
        match bytes[i] {
            b'"' | b'\'' => i = skip_literal(bytes, i),
            b'/' if matches!(bytes.get(i + 1), Some(b'/' | b'*')) => i = skip_comment(bytes, i),
            b'@' => {
                i += 1;
                while i < bytes.len() && (ident_char(bytes[i] as char) || bytes[i] == b'.') {
                    i += 1;
                }
                let args = skip_trivia(bytes, i);
                if bytes.get(args) == Some(&b'(') {
                    i = matching(bytes, args)? + 1;
                }
            }
            b'(' => break i,
            _ => i += 1,
        }
    };
    let head = &segment[..paren];
    // `Foo f = bar(1);` is a field with an initializer, not a method.
    if has_top_level_eq(head) {
        return None;
    }
    let (name, name_start, name_end) = last_ident(head)?;
    if NOT_METHODS.contains(&name.as_str()) {
        return None;
    }
    if head[..name_start].trim_end().ends_with('.') {
        return None;
    }
    let close = matching(bytes, paren)?;
    let after = segment[close + 1..].trim_start();
    let declaration_like = after.starts_with('{')
        || after.starts_with(';')
        || after.starts_with("throws")
        || after.is_empty();
    if !declaration_like {
        return None;
    }
    let arity = param_parts(&segment[paren + 1..close]).len();
    Some(MethodDecl {
        name: (name_start, name_end),
        arity,
    })
}

/// True when `head` holds an `=` outside generics, strings and annotations.
fn has_top_level_eq(head: &str) -> bool {
    let bytes = head.as_bytes();
    let mut i = 0;
    let mut angle = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' => i = skip_literal(bytes, i),
            b'/' if matches!(bytes.get(i + 1), Some(b'/' | b'*')) => i = skip_comment(bytes, i),
            b'@' => {
                i += 1;
                while i < bytes.len() && (ident_char(bytes[i] as char) || bytes[i] == b'.') {
                    i += 1;
                }
                let args = skip_trivia(bytes, i);
                if bytes.get(args) == Some(&b'(')
                    && let Some(close) = matching(bytes, args)
                {
                    i = close + 1;
                }
            }
            b'<' if i > 0 && ident_char(bytes[i - 1] as char) => {
                angle += 1;
                i += 1;
            }
            b'>' if angle > 0 => {
                angle = angle.saturating_sub(1);
                i += 1;
            }
            b'=' if angle == 0 => return true,
            _ => i += 1,
        }
    }
    false
}

/// Member spans of a type body, cut at `;` and after a member's block closes.
///
/// A block ends a member only when it opens at member level and outside
/// parentheses, so `@Select({"a", "b"})` cannot cut its member in two. Text
/// blocks are one literal even when they contain braces.
fn member_spans(text: &str, start: usize, end: usize) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut paren = 0usize;
    let mut blocks: Vec<bool> = Vec::new();
    let mut member_start = start;
    let mut i = start;
    while i < end {
        match bytes[i] {
            b'"' | b'\'' => i = skip_literal(bytes, i),
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
                    out.push((member_start, i + 1));
                    member_start = i + 1;
                }
                i += 1;
            }
            b';' if depth == 0 => {
                out.push((member_start, i));
                member_start = i + 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    if member_start < end {
        out.push((member_start, end));
    }
    out
}

/// Parameter parts split at top-level commas, with generics, arrays and
/// annotations taken into account (mirrors the mapper index's own rule).
fn param_parts(list: &str) -> Vec<(usize, usize)> {
    let bytes = list.as_bytes();
    let mut parts = Vec::new();
    let (mut round, mut angle) = (0usize, 0usize);
    let mut start = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' => i = skip_literal(bytes, i),
            b'/' if matches!(bytes.get(i + 1), Some(b'/' | b'*')) => i = skip_comment(bytes, i),
            b'(' | b'{' | b'[' => {
                round += 1;
                i += 1;
            }
            b')' | b'}' | b']' => {
                round = round.saturating_sub(1);
                i += 1;
            }
            b'<' if i > 0 && ident_char(bytes[i - 1] as char) => {
                angle += 1;
                i += 1;
            }
            b'>' if angle > 0 => {
                angle = angle.saturating_sub(1);
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
    if list[start..].trim().is_empty() {
        return parts;
    }
    parts.push((start, list.len()));
    parts
}

fn last_ident(text: &str) -> Option<(String, usize, usize)> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut end = chars.len();
    while end > 0 && !ident_char(chars[end - 1].1) {
        end -= 1;
    }
    if end == 0 {
        return None;
    }
    let mut start = end;
    while start > 0 && ident_char(chars[start - 1].1) {
        start -= 1;
    }
    let (s, _) = chars[start];
    let (e, c) = chars[end - 1];
    Some((text[s..e + c.len_utf8()].to_owned(), s, e + c.len_utf8()))
}

fn ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

fn skip_trivia(bytes: &[u8], mut i: usize) -> usize {
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if bytes.get(i) == Some(&b'/') && matches!(bytes.get(i + 1), Some(b'/' | b'*')) {
            i = skip_comment(bytes, i);
            continue;
        }
        return i;
    }
}

/// Past the `//…`, `/*…*/` comment opening at `i`.
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

/// Past the string, char or text-block literal opening at `i`.
///
/// Java literals cannot span lines (text blocks can), and an unterminated
/// literal stops at the newline so one editing glitch cannot swallow the file.
fn skip_literal(bytes: &[u8], i: usize) -> usize {
    if bytes[i] == b'"' && bytes.get(i + 1) == Some(&b'"') && bytes.get(i + 2) == Some(&b'"') {
        return find_bytes(bytes, i + 3, b"\"\"\"").map_or(bytes.len(), |at| at + 3);
    }
    let quote = bytes[i];
    let mut j = i + 1;
    while j < bytes.len() {
        match bytes[j] {
            b'\n' | b'\r' => return j,
            b'\\' => j += 2,
            b if b == quote => return j + 1,
            _ => j += 1,
        }
    }
    bytes.len()
}

fn find_bytes(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || from >= bytes.len() {
        return None;
    }
    bytes[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| from + offset)
}

/// Index of the bracket closing the one at `open`, minding literals, comments
/// and all three bracket kinds together.
fn matching(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' => i = skip_literal(bytes, i),
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

/// 0-based line of `offset`.
fn line_of(text: &str, offset: usize) -> u32 {
    text.as_bytes()[..offset.min(text.len())]
        .iter()
        .filter(|&&b| b == b'\n')
        .count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAPPER_PATH: &str = "src/main/java/com/example/myapp/mapper/UserMapper.java";

    const MAPPER: &str = r#"package com.example.myapp.mapper;

import java.util.List;
import org.apache.ibatis.annotations.Mapper;
import org.apache.ibatis.annotations.Param;
import org.apache.ibatis.annotations.Select;

@Mapper
public interface UserMapper extends BaseMapper<User> {
    @Override
    User selectById(Long id);

    List<User> selectByCondition(@Param("name") String name, int limit);

    // 注释里的 fakeInComment 不是方法
    @Select("select 'fakeInString' from dual")
    String fromString();

    <T> T genericOne(User entity) throws Exception;

    int overloaded(Long id);

    int overloaded(Long id, String name);

    default String defaultValue() {
        return "x";
    }

    interface Inner {
        int innerCount();
    }

    class Nested<T> {
        public Nested() {
        }

        T value() {
            return null;
        }
    }
}
"#;

    const GUARD: &str = r#"package com.example.guard;

public class Guard {
    // void fakeInLineComment() { }
    /* int fakeInBlockComment() { return 0; } */

    private static final String QUERY = "select fakeInString() from t";

    private static final String SCRIPT = """
        class FakeInTextBlock {
            void fakeInTextBlock() { }
        }
        """;

    private final java.util.function.Function<String, String> normalise = value -> value.trim();

    static {
        java.util.Objects.requireNonNull(QUERY);
    }

    public int realMethod(int a) {
        return a;
    }
}
"#;

    const KINDS: &str = r#"package com.example.kinds;

public enum Status {
    ACTIVE,
    DISABLED(1) {
        @Override
        public int rank() { return 2; }
    };

    public int rank() { return 0; }
}

record Point(int x, int y) {
    public int sum() { return x + y; }
}
"#;

    fn line_of(text: &str, needle: &str) -> u32 {
        text.lines()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("fixture 里没有 {needle}")) as u32
    }

    fn mapper_index() -> JavaMethodIndex {
        let mut index = JavaMethodIndex::new();
        assert!(index.add_file(MAPPER_PATH, MAPPER) > 0);
        index
    }

    #[test]
    fn methods_carry_file_and_zero_based_line() {
        let index = mapper_index();
        let found = index.lookup("com.example.myapp.mapper.UserMapper#selectById");
        assert_eq!(found.len(), 1);
        let location = found[0];
        assert_eq!(location.file, MAPPER_PATH);
        assert_eq!(location.fqn, "com.example.myapp.mapper.UserMapper");
        assert_eq!(location.class_simple, "UserMapper");
        assert_eq!(location.arity, 1);
        assert_eq!(location.line, line_of(MAPPER, "User selectById(Long id);"));
        assert_eq!(location.display_line(), location.line + 1);
    }

    #[test]
    fn annotations_generics_and_default_methods_are_recognised() {
        let index = mapper_index();
        let generic = index
            .lookup_unique("UserMapper#genericOne")
            .expect("泛型方法");
        assert_eq!(generic.arity, 1);
        assert_eq!(generic.fqn, "com.example.myapp.mapper.UserMapper");
        let default = index
            .lookup_unique("UserMapper#defaultValue")
            .expect("default 方法");
        assert_eq!(default.arity, 0);
        let with_params = index
            .lookup_unique("UserMapper#selectByCondition")
            .expect("@Param 方法");
        assert_eq!(with_params.arity, 2);
    }

    #[test]
    fn overloads_are_all_returned_and_the_unique_lookup_declines() {
        let index = mapper_index();
        let mut arity: Vec<usize> = index
            .lookup("com.example.myapp.mapper.UserMapper#overloaded")
            .iter()
            .map(|location| location.arity)
            .collect();
        arity.sort_unstable();
        assert_eq!(arity, vec![1, 2], "重载全部返回，不猜一个");
        assert!(index.lookup_unique("UserMapper#overloaded").is_none());
    }

    #[test]
    fn comments_and_strings_never_produce_methods() {
        let mut index = JavaMethodIndex::new();
        index.add_file("Guard.java", GUARD);
        for (target, why) in [
            ("Guard#fakeInLineComment", "行注释"),
            ("Guard#fakeInBlockComment", "块注释"),
            ("Guard#fakeInString", "字符串里的 SQL"),
            ("Guard#FakeInTextBlock", "文本块里的 class"),
            ("Guard#fakeInTextBlock", "文本块里的方法"),
            ("Guard#trim", "lambda 里的方法调用"),
            ("Guard#requireNonNull", "静态初始化块里的调用"),
        ] {
            assert!(index.lookup(target).is_empty(), "{why}：{target}");
        }
        assert_eq!(index.lookup("Guard#realMethod")[0].arity, 1);
        assert_eq!(index.len(), 1, "只有 realMethod 一个真方法");
    }

    #[test]
    fn nested_types_get_their_own_fqn_and_constructors_are_skipped() {
        let index = mapper_index();
        let inner = index
            .lookup_unique("com.example.myapp.mapper.UserMapper.Inner#innerCount")
            .expect("嵌套接口的方法");
        assert_eq!(inner.class_simple, "Inner");
        assert!(
            index.lookup("UserMapper#Nested").is_empty(),
            "构造器不是方法"
        );
        assert_eq!(index.lookup("UserMapper.Nested#value").len(), 1);
        assert_eq!(
            index.lookup_class("Nested"),
            vec!["com.example.myapp.mapper.UserMapper.Nested".to_owned()]
        );
    }

    #[test]
    fn enums_index_methods_but_not_constant_bodies() {
        let mut index = JavaMethodIndex::new();
        index.add_file("Status.java", KINDS);
        let ranks = index.lookup("com.example.kinds.Status#rank");
        assert_eq!(ranks.len(), 1, "常量体里的 rank 不算");
        assert_eq!(
            ranks[0].line,
            line_of(KINDS, "public int rank() { return 0; }")
        );
        let sum = index
            .lookup_unique("com.example.kinds.Point#sum")
            .expect("record 的成员方法");
        assert_eq!(sum.fqn, "com.example.kinds.Point");
    }

    #[test]
    fn a_truncated_file_is_skipped_and_counted() {
        let mut index = JavaMethodIndex::new();
        let added = index.add_file(
            "Broken.java",
            "package p;\npublic class Broken {\n    void ok() {\n",
        );
        assert_eq!(added, 0);
        assert_eq!(index.skipped_files(), 1);
        assert!(index.lookup("Broken#ok").is_empty());
    }

    #[test]
    fn budget_and_exclusions_are_enforced() {
        let mut index = JavaMethodIndex::with_budget(JavaIndexBudget {
            max_file_bytes: 16,
            ..JavaIndexBudget::default()
        });
        assert_eq!(index.add_file("pom.xml", "<project/>"), 0);
        assert_eq!(index.add_file("m/target/gen/Foo.java", GUARD), 0);
        assert_eq!(index.add_file("m/src/Foo.java", GUARD), 0, "超过单文件预算");
        assert_eq!(index.skipped_files(), 3);
        assert!(index.is_empty());

        let mut index = JavaMethodIndex::with_budget(JavaIndexBudget {
            max_files: 1,
            ..JavaIndexBudget::default()
        });
        assert!(index.add_file("a/One.java", "package a; class One { void m() {} }") > 0);
        assert_eq!(
            index.add_file("a/Two.java", "package a; class Two { void m() {} }"),
            0
        );
        assert_eq!(index.file_count(), 1);
    }

    #[test]
    fn removing_a_file_drops_its_methods_and_its_classes() {
        let mut index = mapper_index();
        assert!(index.remove_file(MAPPER_PATH) > 0);
        assert!(index.lookup("UserMapper#selectById").is_empty());
        assert!(index.lookup_class("UserMapper").is_empty());
        assert!(index.is_empty());
        assert_eq!(index.file_count(), 0);
        assert_eq!(index.remove_file(MAPPER_PATH), 0, "再删一次是空操作");
    }

    #[test]
    fn reindexing_the_same_path_replaces_its_methods() {
        let mut index = mapper_index();
        let before = index.len();
        index.add_file(
            MAPPER_PATH,
            "package com.example.myapp.mapper;\ninterface UserMapper {\n  int renamed();\n}\n",
        );
        assert_eq!(index.len(), 1, "旧方法全部失效，不是累加");
        assert!(index.lookup("UserMapper#renamed").len() == 1);
        assert!(index.lookup("UserMapper#selectById").is_empty());
        assert!(before > 1);
    }

    #[test]
    fn an_abbreviated_logger_class_still_resolves() {
        let index = mapper_index();
        let line = "2026-11-25 21:00:01.001 DEBUG 4242 --- [nio-8080-exec-1] c.e.m.mapper.UserMapper.selectById : ==>  Preparing: SELECT 1";
        let source = crate::core::log::source_of_line(line)
            .source
            .expect("logger 来源");
        assert_eq!(source.class_name, "c.e.m.mapper.UserMapper");
        let found = index.lookup_source_unique(&source);
        let location = found.expect("缩写包名按段匹配");
        assert_eq!(location.file, MAPPER_PATH);
        assert_eq!(location.method, "selectById");
    }

    #[test]
    fn a_simple_name_with_two_owners_is_not_resolved_to_one() {
        let mut index = JavaMethodIndex::new();
        index.add_file(
            "a/UserMapper.java",
            "package a; interface UserMapper { int find(); }",
        );
        index.add_file(
            "b/UserMapper.java",
            "package b; interface UserMapper { int find(); }",
        );
        assert_eq!(index.lookup_class("UserMapper").len(), 2);
        assert_eq!(index.lookup("UserMapper#find").len(), 2);
        assert!(index.lookup_unique("UserMapper#find").is_none());
    }

    #[test]
    fn a_mapper_xml_namespace_resolves_to_the_interface_method() {
        let index = mapper_index();
        let xml = r#"<mapper namespace="com.example.myapp.mapper.UserMapper">
  <select id="selectById" resultType="com.example.myapp.model.User">select * from user where id = #{id}</select>
</mapper>"#;
        let facts = crate::indexer::index_xml("resources/mapper/UserMapper.xml", xml);
        let namespace = facts.namespace.as_ref().expect("namespace").name.clone();
        let found = index.resolve_statement(&namespace, "selectById");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].file, MAPPER_PATH);
        assert_eq!(found[0].line, line_of(MAPPER, "User selectById(Long id);"));
    }

    #[test]
    fn an_unknown_statement_id_returns_nothing() {
        let index = mapper_index();
        assert!(
            index
                .resolve_statement("com.example.myapp.mapper.UserMapper", "nope")
                .is_empty()
        );
        assert!(
            index
                .resolve_statement("com.example.other.Mapper", "selectById")
                .is_empty()
        );
        assert!(index.lookup("no-separator").is_empty());
        assert!(index.lookup("#method").is_empty());
    }
}
