//! Preview workbench (T2-2), now the fifth tab of the SQL log panel (L23).
//!
//! The workbench mirrors the IntelliJ edition's "SQL preview" panel
//! (`SqlStandalonePreviewPanel` + `SqlPreviewPanel`): an input pane on top, a
//! preview pane below, a parameter column beside the SQL column, and the tabs
//! SQL / 原文 / 列信息. In the IntelliJ edition that panel is the fifth
//! (closable) tab of the SQL Log tool window, not a tool window of its own, so
//! this build mounts it on that panel's fifth tab (lane L42: the page is the
//! module `web/preview.js`, injected into `#panelPreview`; the same module is
//! the whole page of `web/preview.html`, the shell the API 1.18 dialog will
//! load).
//!
//! This module therefore owns the model and the op handling only; it posts no
//! view message of its own. The log panel carries [`preview_tab`] in its state
//! (`tabs.preview`) and routes `preview-*` messages here, so both halves share
//! one push channel and one page — and [`preview_target`] names the channel
//! when a second mount point exists.
//!
//! Two facts of the OpenIDEA platform shape this panel:
//!
//! - the dialect comes from the settings page, so the panel shows a read-only
//!   label (the IntelliJ panel has no dialect picker either);
//! - the plugin never executes user SQL, so "Execute SQL" stays disabled with
//!   the reason in its tooltip.

use std::sync::{Mutex, MutexGuard, OnceLock};

use openidea_plugin::{ContentDiff, Error, PickItem, host};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::core::perf::{self, Severity};
use crate::core::xml::{Confidence, MapperSet, RenderOutput};
use crate::core::{dialect, format};
use crate::i18n::t;
use crate::settings;

/// The input is SQL text, used verbatim.
pub const KIND_SQL: &str = "sql";
/// The input is a mapper XML fragment and is rendered into statements.
pub const KIND_XML: &str = "xml";

/// The schema-declared setting the dialect comes from (`settings.schema.json`).
const DIALECT_SETTING: &str = "dialect.default";
/// The key `commands::common` still reads; kept as a second source so a value
/// stored under either name is honoured.
const DIALECT_SETTING_LEGACY: &str = "sql.dialect";
/// Used when neither setting holds a value.
const FALLBACK_DIALECT: &str = "MySQL";

/// Lane L34: format the statement a hand-off put in the preview.
const FORMAT_ON_PREVIEW_SETTING: &str = "render.formatOnPreview";

/// How many project files one "load from file" pick lists.
const PICK_LIMIT: u32 = 200;

/// The diff viewer's file name, so its right side gets SQL highlighting (same
/// choice `commands::preview_sql_diff` makes).
const DIFF_FILE_NAME: &str = "sql";

/// The document name a pasted fragment is rendered under; it only shows up in
/// load diagnostics.
const FRAGMENT_DOC: &str = "<workbench>";

/// What the workbench shows: one statement with its restored parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkbenchModel {
    /// The executable SQL; empty for `unresolved` renders.
    pub sql: String,
    #[serde(default)]
    pub params: Vec<String>,
    pub dialect: String,
    /// `exact` / `partial` / `unresolved`; empty when nothing was rendered.
    #[serde(default)]
    pub confidence: String,
    /// Where the statement came from (mapper path, statement id), for the header.
    #[serde(default)]
    pub source: String,
    /// The text the user typed or loaded, verbatim (the "原文" tab).
    #[serde(default)]
    pub raw: String,
    /// Column names, when whatever produced the render supplied them; empty
    /// means "not available", never "no columns".
    #[serde(default)]
    pub columns: Vec<String>,
    /// `sql` or `xml`; empty before the first render.
    #[serde(default)]
    pub kind: String,
    /// Why an `unresolved`/`partial` render is what it is, one line per
    /// key/detail pair. Empty when the render was clean.
    #[serde(default)]
    pub reason: String,
    /// Localised performance-advice line for the rendered SQL.
    #[serde(default)]
    pub advice: String,
    /// Every statement id this render can switch between (lane L45). Empty for
    /// inline SQL, a log row, and a mapper that holds no statement: the page
    /// then hides the statement picker instead of showing a fake choice. A
    /// mapper with one statement also lists that one, so the picker can tell
    /// "no choice" (one entry) from "not a mapper" (none).
    #[serde(default)]
    pub statements: Vec<String>,
    /// The id of the statement `sql` was rendered from (lane L45); empty when
    /// the render is not a mapper statement.
    #[serde(default)]
    pub statement: String,
}

impl Default for WorkbenchModel {
    fn default() -> Self {
        Self {
            sql: String::new(),
            params: Vec::new(),
            dialect: FALLBACK_DIALECT.to_string(),
            confidence: String::new(),
            source: String::new(),
            raw: String::new(),
            columns: Vec::new(),
            kind: String::new(),
            reason: String::new(),
            advice: String::new(),
            statements: Vec::new(),
            statement: String::new(),
        }
    }
}

impl WorkbenchModel {
    pub fn new(sql: &str, params: Vec<String>, dialect: &str) -> Self {
        Self {
            sql: sql.to_string(),
            params,
            dialect: dialect.to_string(),
            ..Self::default()
        }
    }

    pub fn params_text(&self) -> String {
        self.params.join(", ")
    }

    /// What the Copy button puts on the clipboard: the executable SQL with SQL
    /// comments stripped, matching the reference's `SqlCommentStripper`: a
    /// copied statement is what runs, so `--` notes never travel with it.
    pub fn copy_text(&self) -> String {
        strip_sql_comments(&self.sql)
    }

    /// What Export hands over: the statement formatted for the current dialect
    /// (a no-op for unknown dialects).
    pub fn export_text(&self) -> String {
        match dialect::by_name(&self.dialect) {
            Some(found) => format::format(&self.sql, found),
            None => self.sql.clone(),
        }
    }

    /// The formatted form of the executable SQL for an explicit dialect name;
    /// empty when there is no SQL to format.
    pub fn formatted_sql(&self, dialect_name: &str) -> String {
        if !self.has_sql() {
            return String::new();
        }
        match dialect::by_name(dialect_name) {
            Some(found) => format::format(&self.sql, found),
            None => self.sql.clone(),
        }
    }

    pub fn has_sql(&self) -> bool {
        !self.sql.trim().is_empty()
    }

    /// The column tab's text; empty means the render carried no column info.
    pub fn columns_text(&self) -> String {
        self.columns.join("\n")
    }

    /// All fifteen dialect names, in the order `core::dialect` lists them.
    pub fn dialect_options() -> Vec<String> {
        dialect::all()
            .iter()
            .map(|item| item.name.to_string())
            .collect()
    }

    /// Switches the dialect; returns false (and keeps the old one) when the
    /// name is unknown. The page cannot call this: the dialect is read-only
    /// and comes from the settings page.
    pub fn with_dialect(&mut self, name: &str) -> bool {
        if dialect::by_name(name).is_none() {
            return false;
        }
        self.dialect = name.to_string();
        true
    }

    /// The display fields of the selected dialect (quote pair, pagination
    /// shape, keyword case). `None` when the name is not a known dialect.
    pub fn dialect_display(&self) -> Option<DialectDisplay> {
        let found = dialect::by_name(&self.dialect)?;
        let capabilities = &found.capabilities;
        Some(DialectDisplay {
            name: found.name.to_string(),
            quote_open: capabilities.quote_open.to_string(),
            quote_close: capabilities.quote_close.to_string(),
            pagination: format!("{:?}", capabilities.pagination),
            keyword_case: format!("{:?}", capabilities.keyword_case),
        })
    }

    /// Localised confidence badge, empty when the caller did not set one.
    pub fn confidence_label(&self) -> String {
        confidence_label(&self.confidence)
    }

    /// `(label, value)` rows for a declarative form rendering.
    pub fn display_fields(&self) -> Vec<(String, String)> {
        let mut fields = vec![
            (t("view.workbench.sql"), self.sql.clone()),
            (t("view.workbench.params"), self.params_text()),
            (t("view.workbench.dialect"), self.dialect.clone()),
        ];
        let confidence = self.confidence_label();
        if !confidence.is_empty() {
            fields.push((t("view.workbench.confidence"), confidence));
        }
        fields
    }
}

/// Placeholder written over comment characters while scanning, so a line can
/// tell "only a comment" from "code" before the markers are dropped. Never
/// reaches the output (`filter_commented_lines` removes them).
const COMMENT_MARK: char = '\u{0}';

/// Strips `-- … EOL` and `/* … */` comments, leaving string literals and
/// quoted identifiers untouched.
///
/// This mirrors the IntelliJ edition's `SqlCommentStripper.strip`, whose
/// contract the reference's Copy button promises: comments are marked while
/// scanning, comment-only lines disappear instead of leaving blank lines, and
/// each kept line loses its trailing whitespace. `''` / `""` doubled quotes and
/// backslash escapes inside a quoted span are honoured, so `'-- not a comment'`
/// survives.
pub(crate) fn strip_sql_comments(sql: &str) -> String {
    if sql.is_empty() {
        return String::new();
    }
    let mut chars: Vec<char> = sql.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if ch == '\'' || ch == '"' || ch == '`' {
            i = skip_quoted_span(&chars, i, ch);
            continue;
        }
        if ch == '-' && i + 1 < chars.len() && chars[i + 1] == '-' {
            while i < chars.len() && chars[i] != '\n' {
                chars[i] = COMMENT_MARK;
                i += 1;
            }
            continue;
        }
        if ch == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
            chars[i] = COMMENT_MARK;
            chars[i + 1] = COMMENT_MARK;
            i += 2;
            while i < chars.len() {
                if chars[i] == '*' && i + 1 < chars.len() && chars[i + 1] == '/' {
                    chars[i] = COMMENT_MARK;
                    chars[i + 1] = COMMENT_MARK;
                    i += 2;
                    break;
                }
                if chars[i] != '\n' {
                    chars[i] = COMMENT_MARK;
                }
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    filter_commented_lines(&chars)
}

/// One quoted span (string literal or quoted identifier), starting at `start`.
/// Returns the index just past the closing quote (or the end of input).
fn skip_quoted_span(chars: &[char], start: usize, quote: char) -> usize {
    let mut i = start + 1;
    while i < chars.len() {
        if chars[i] == '\\' {
            i += 2;
            continue;
        }
        if chars[i] == quote {
            // A doubled quote is an escaped one, not the end of the span.
            if i + 1 < chars.len() && chars[i + 1] == quote {
                i += 2;
                continue;
            }
            return i + 1;
        }
        i += 1;
    }
    chars.len()
}

/// Drops marked (comment) characters, then removes lines that held only a
/// comment and trims trailing whitespace from the lines that remain.
fn filter_commented_lines(chars: &[char]) -> String {
    let mut out = String::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        let mut end = i;
        while end < chars.len() && chars[end] != '\n' {
            end += 1;
        }
        let has_code = (i..end).any(|k| chars[k] != COMMENT_MARK && !chars[k].is_whitespace());
        let has_mark = (i..end).any(|k| chars[k] == COMMENT_MARK);
        if has_code || !has_mark {
            let line: String = (i..end)
                .filter(|&k| chars[k] != COMMENT_MARK)
                .map(|k| chars[k])
                .collect();
            out.push_str(line.trim_end());
            if end < chars.len() {
                out.push('\n');
            }
        }
        i = if end < chars.len() { end + 1 } else { end };
    }
    out
}

/// The fields that change when the dialect switches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DialectDisplay {
    pub name: String,
    pub quote_open: String,
    pub quote_close: String,
    pub pagination: String,
    pub keyword_case: String,
}

// ---------------------------------------------------------------------------
// The dialect the settings page decides
// ---------------------------------------------------------------------------

/// The configured dialect and whether it is only the fallback.
///
/// `true` means the user never set a value, so the page shows `MySQL（默认）`
/// and the hint pointing at the settings page.
pub fn configured_dialect() -> (String, bool) {
    for key in [DIALECT_SETTING, DIALECT_SETTING_LEGACY] {
        if let Some(value) = host::setting_string(key) {
            let value = value.trim();
            if !value.is_empty() {
                return (value.to_string(), false);
            }
        }
    }
    (FALLBACK_DIALECT.to_string(), true)
}

/// The dialect's capability summary, for the meta line's tooltip.
pub fn capabilities_text(name: &str) -> String {
    let Some(found) = dialect::by_name(name) else {
        return t("view.workbench.no_dialect");
    };
    let capabilities = &found.capabilities;
    [
        t("view.workbench.cap.quote")
            .replace("{0}", &capabilities.quote_open.to_string())
            .replace("{1}", &capabilities.quote_close.to_string()),
        t("view.workbench.cap.pagination")
            .replace("{0}", &format!("{:?}", capabilities.pagination)),
        t("view.workbench.cap.keyword_case")
            .replace("{0}", &format!("{:?}", capabilities.keyword_case)),
    ]
    .join("\n")
}

// ---------------------------------------------------------------------------
// Rendering the input
// ---------------------------------------------------------------------------

/// Guesses the input kind the way the IntelliJ panel does: a mapper fragment
/// is anything that opens a `<mapper>` root or a statement tag.
pub fn detect_kind(text: &str) -> &'static str {
    for needle in [
        "<mapper", "<select", "<insert", "<update", "<delete", "<sql ",
    ] {
        if text.contains(needle) {
            return KIND_XML;
        }
    }
    KIND_SQL
}

/// Renders `text` (of `kind`, or auto-detected when `kind` is not one of the
/// two known values) into the workbench model. Pure: no `host::` calls.
pub fn render_input(text: &str, kind: &str, dialect_name: &str) -> WorkbenchModel {
    let kind = match kind {
        KIND_SQL | KIND_XML => kind,
        _ => detect_kind(text),
    };
    if kind == KIND_XML {
        render_mapper(text, dialect_name)
    } else {
        plain_sql(text, dialect_name)
    }
}

/// Plain SQL is used verbatim: nothing is inferred, so the render is `exact`.
fn plain_sql(text: &str, dialect_name: &str) -> WorkbenchModel {
    let sql = text.trim_end().to_string();
    let mut model = WorkbenchModel::new(&sql, Vec::new(), dialect_name);
    model.raw = text.to_string();
    model.kind = KIND_SQL.to_string();
    model.confidence = if sql.trim().is_empty() {
        String::new()
    } else {
        "exact".to_string()
    };
    model.advice = advice_text(&sql, dialect_name);
    model
}

/// A mapper fragment goes through [`MapperSet`]; only its first statement is
/// previewed until the page picks another one (lane L45 asks for a different id
/// through the `preview-statement` op, which re-enters this function with it).
fn render_mapper(text: &str, dialect_name: &str) -> WorkbenchModel {
    render_mapper_statement(text, None, dialect_name)
}

/// Renders one statement of a mapper document (`None` = the first one).
///
/// The model lists every statement id the picker may offer (lane L45), so the
/// page can switch without the host shipping the whole file again: the ids and
/// the chosen one travel in the model, and the mapper text itself stays in the
/// preview store ([`set_mapper_source`]).
pub(crate) fn render_mapper_statement(
    text: &str,
    statement: Option<&str>,
    dialect_name: &str,
) -> WorkbenchModel {
    let mut model = WorkbenchModel::new("", Vec::new(), dialect_name);
    model.raw = text.to_string();
    model.kind = KIND_XML.to_string();

    let (set, issues) = MapperSet::load(&[(FRAGMENT_DOC, text)]);
    let mut reasons: Vec<String> = issues
        .iter()
        .map(|issue| format!("[{}] {}: {}", issue.message_key, issue.path, issue.detail))
        .collect();

    let ids = statement_ids(&set);
    model.statements = ids.clone();
    let chosen = match statement {
        Some(id) => Some(id.to_string()),
        None => ids.first().cloned(),
    };
    let Some(id) = chosen else {
        model.confidence = "unresolved".to_string();
        reasons.push(format!("[{}]", crate::core::xml::KEY_STATEMENT_NOT_FOUND));
        model.reason = reasons.join("\n");
        return model;
    };

    let out = set.render(&id, &json!({}));
    model.confidence = confidence_str(out.confidence).to_string();
    model.source = id.clone();
    model.statement = id;
    if out.confidence == Confidence::Unresolved {
        // `unresolved` never carries SQL: only the reason goes to the page.
        model.sql.clear();
        model.params.clear();
    } else {
        model.sql = out.sql.trim_end().to_string();
        model.params = out.params.iter().map(param_text).collect();
    }
    reasons.extend(reason_lines(&out));
    model.reason = reasons.join("\n");
    model.advice = advice_text(&model.sql, dialect_name);
    model
}

/// The statement ids a picker may offer (lane L45).
///
/// [`MapperSet::statement_ids`] lists each namespaced statement twice — bare
/// `findById` then qualified `ns.findById` — and the preview has always shown
/// the bare form. Keep that form (the id the model's `source` carries) and drop
/// the qualified twin, so the picker never offers the same statement twice.
pub(crate) fn statement_ids(set: &MapperSet) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in set.statement_ids() {
        // A qualified twin directly follows its bare id: `ns.` + an id already
        // kept. Same statement, one entry.
        let twin = out.last().is_some_and(|kept: &String| {
            id.len() > kept.len() + 1
                && id.ends_with(kept.as_str())
                && id.as_bytes()[id.len() - kept.len() - 1] == b'.'
        });
        if !twin && !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

fn confidence_str(confidence: Confidence) -> &'static str {
    match confidence {
        Confidence::Exact => "exact",
        Confidence::Partial => "partial",
        Confidence::Unresolved => "unresolved",
    }
}

fn confidence_label(confidence: &str) -> String {
    match confidence {
        "exact" => t("view.workbench.confidence.exact"),
        "partial" => t("view.workbench.confidence.partial"),
        "unresolved" => t("view.workbench.confidence.unresolved"),
        "" => String::new(),
        _ => t("view.workbench.confidence.unknown"),
    }
}

/// A parameter value as the page shows it: strings bare, everything else as
/// JSON.
fn param_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// `[key] detail` lines from a render's notes, plus any key that has no note.
fn reason_lines(out: &RenderOutput) -> Vec<String> {
    let mut lines: Vec<String> = out
        .notes
        .iter()
        .map(|note| format!("[{}] {}", note.message_key, note.detail))
        .collect();
    for key in &out.message_keys {
        if !out.notes.iter().any(|note| &note.message_key == key) {
            lines.push(format!("[{key}]"));
        }
    }
    lines
}

/// The meta line's advice: counts only, because the individual rules return
/// message keys this build has no text for. Details come from the
/// `perf-advice` command.
fn advice_text(sql: &str, dialect_name: &str) -> String {
    if sql.trim().is_empty() {
        return String::new();
    }
    let findings = perf::analyze(sql, dialect_name);
    if findings.is_empty() {
        return t("view.workbench.advice.none");
    }
    let errors = findings
        .iter()
        .filter(|finding| finding.severity == Severity::Error)
        .count();
    let warns = findings
        .iter()
        .filter(|finding| finding.severity == Severity::Warn)
        .count();
    t("view.workbench.advice")
        .replace("{0}", &findings.len().to_string())
        .replace("{1}", &errors.to_string())
        .replace("{2}", &warns.to_string())
}

// ---------------------------------------------------------------------------
// Model store
// ---------------------------------------------------------------------------

/// The preview's two display switches (lane L45). Pure presentation: the page
/// shapes the text it paints, the model's SQL is never rewritten.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DisplayPrefs {
    /// Break the shown SQL before each top-level keyword.
    pub keyword_wrap: bool,
    /// Put each top-level comma-separated item on its own line.
    pub vertical: bool,
}

/// What the preview tab keeps between state pushes.
#[derive(Default)]
struct PreviewStore {
    model: WorkbenchModel,
    /// Last status line (the panel keeps it across unrelated pushes).
    status: String,
    /// Set by an op that rewrote the input box (render / format / load-file /
    /// clear); one-shot, so a later unrelated push leaves what the user typed
    /// alone.
    replace_input: bool,
    /// Set by `open-workbench` and by the log panel's "open line": the page has
    /// to open and select the preview tab. Cleared once the page reports the
    /// tab it shows, because by then it has applied the selection.
    select_pending: bool,
    /// Set together with `replace_input` by a command that handed a statement
    /// over (lane L34). Consumed by [`format_handoff_sql`] when the page
    /// reports its tab, so only handed-over SQL — never what the user typed —
    /// goes through `render.formatOnPreview`.
    handoff: bool,
    /// The mapper document the current render came from (lane L45), kept so the
    /// page can switch statements by id without shipping the file back. `None`
    /// for inline SQL and log rows, which have nothing to switch.
    mapper_xml: Option<String>,
    /// The two display switches (lane L45): break before top-level keywords, and
    /// one comma-separated item per line. Page state that rides the `state`
    /// push, so a remount (or the standalone page reloading) restores what the
    /// reader chose. Host memory only — no setting key, and the SQL itself is
    /// never touched.
    display: DisplayPrefs,
}

static PREVIEW: OnceLock<Mutex<PreviewStore>> = OnceLock::new();

fn preview_store() -> MutexGuard<'static, PreviewStore> {
    let mutex = PREVIEW.get_or_init(|| Mutex::new(PreviewStore::default()));
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Replaces what the preview tab shows (called by the lanes that render).
///
/// The statement stash is cleared too: a model set by hand carries no document
/// for the picker to switch within. Callers that do hold the mapper text call
/// [`set_mapper_source`] right after, which is the only way the stash is set.
pub fn set_model(model: WorkbenchModel) {
    let mut store = preview_store();
    store.model = model;
    store.mapper_xml = None;
}

/// Hands a mapper document to the store as well as the model (lane L45).
///
/// The statement picker switches by id, so the host has to keep the document
/// the ids were read from: the page never holds the file, and a state push does
/// not carry it. Inline SQL and log rows clear the stash — they have no
/// statements to switch between.
pub fn set_mapper_source(xml: Option<String>) {
    preview_store().mapper_xml = xml;
}

/// The mapper document the current render came from, if any (lane L45).
pub fn mapper_source() -> Option<String> {
    preview_store().mapper_xml.clone()
}

/// The two display switches as the page last set them (lane L45).
pub fn display_prefs() -> DisplayPrefs {
    preview_store().display
}

/// The current model.
pub fn model() -> WorkbenchModel {
    preview_store().model.clone()
}

/// The one-shot flags a test has to restore alongside the model.
///
/// `replace_input` and `handoff` are consumed by the next push
/// (`mem::take`), so they are invisible to [`model`] — a test that restores
/// only the model leaves them as its body left them, and the next test's
/// assertion on `replaceInput` then fails. CI found this; a local serial run
/// did not, because the order happened to be friendly.
#[cfg(test)]
pub(crate) fn take_oneshot_flags() -> (bool, bool) {
    let mut store = preview_store();
    (
        std::mem::replace(&mut store.replace_input, false),
        std::mem::replace(&mut store.handoff, false),
    )
}

/// Puts back what [`take_oneshot_flags`] took.
#[cfg(test)]
pub(crate) fn restore_oneshot_flags(flags: (bool, bool)) {
    let mut store = preview_store();
    store.replace_input = flags.0;
    store.handoff = flags.1;
}

/// Empties the preview tab.
pub fn clear() {
    let mut store = preview_store();
    store.model = WorkbenchModel::default();
    // Nothing is left to replace or format once the model is gone.
    store.replace_input = false;
    store.handoff = false;
    // No statements to switch between once the render is gone.
    store.mapper_xml = None;
}

/// Asks the page to open and select the "SQL 预览" tab on the next push.
pub fn request_tab() {
    preview_store().select_pending = true;
}

/// The page reported the tab it now shows, so a pending request is satisfied.
pub fn note_page_tab() {
    preview_store().select_pending = false;
}

/// Asks the next push to write the model's text into the input box: a command
/// that handed a statement over has to show it there, and no op of the page
/// ran to set the flag.
pub fn request_input_replace() {
    let mut store = preview_store();
    store.replace_input = true;
    // A command handing a statement over is a hand-off, the one case
    // `render.formatOnPreview` applies to (lane L34).
    store.handoff = true;
}

/// Formats the statement a hand-off put in the preview, when
/// `render.formatOnPreview` is on (lane L34).
///
/// The reference setting is "格式化还原 SQL" — restored SQL, i.e. what a
/// command or a log row handed over. It runs when the page reports the tab it
/// switched to, the point where the hand-off has landed, and it is a no-op for
/// what the user typed: a re-render of the input box stays byte-identical.
///
/// Returns `true` when the model was rewritten; the next push then also
/// replaces the input box, so the two panes agree.
pub fn format_handoff_sql() -> bool {
    let pending = std::mem::take(&mut preview_store().handoff);
    if !pending || !settings::get_bool(FORMAT_ON_PREVIEW_SETTING, true) {
        return false;
    }
    let (dialect_name, _) = configured_dialect();
    let Some(found) = dialect::by_name(&dialect_name) else {
        return false;
    };
    let mut store = preview_store();
    if store.model.sql.trim().is_empty() {
        return false;
    }
    let formatted = format::format(&store.model.sql, found);
    if formatted == store.model.sql {
        return false;
    }
    store.model.sql = formatted;
    store.model.dialect = dialect_name;
    store.replace_input = true;
    true
}

fn set_status(status: String, replace_input: bool) {
    let mut store = preview_store();
    store.status = status;
    if replace_input {
        store.replace_input = true;
    }
}

// ---------------------------------------------------------------------------
// The payload the log panel carries
// ---------------------------------------------------------------------------

/// The `tabs.preview` object of the log panel's state: the model (with the
/// three tabs), the labels, the read-only dialect block, the status line and
/// whether this push rewrites the input box.
///
/// Taking `replace_input` here makes the flag one-shot: the log panel pushes
/// its state for other reasons too (filter, capture refresh), and those pushes
/// must not overwrite what the user typed.
pub fn preview_tab() -> Value {
    let mut store = preview_store();
    let replace_input = std::mem::take(&mut store.replace_input);
    let current = store.model.clone();
    let status = store.status.clone();
    let display = store.display;
    drop(store);

    let (dialect_name, is_default) = configured_dialect();
    // Callers that only have the statement (`commands::open_workbench`) leave
    // `raw` empty; the input pane then starts from the statement itself
    // instead of an empty box.
    let raw = if current.raw.is_empty() && !current.sql.is_empty() {
        &current.sql
    } else {
        &current.raw
    };
    json!({
        "replaceInput": replace_input,
        "model": {
            "sql": &current.sql,
            "params": &current.params,
            "dialect": &dialect_name,
            "confidence": &current.confidence,
            "source": &current.source,
            "kind": &current.kind,
            "reason": &current.reason,
            // The page wears a degradation strip whenever this is true: an
            // `unresolved` render carries no SQL, only reasons (lane L35b).
            "degraded": current.confidence == "unresolved",
            "advice": &current.advice,
            // Lane L45: the statement picker's data. `statements` is empty for
            // inline SQL and log rows (the page hides the picker), and holds one
            // entry for a mapper with a single statement (the page disables it
            // instead of pretending there is a choice).
            "statements": &current.statements,
            "statement": &current.statement,
            "tabs": {
                "sql": &current.sql,
                "formatted": current.formatted_sql(&dialect_name),
                "raw": raw,
                "columns": current.columns_text(),
            },
        },
        "dialect": {
            "name": &dialect_name,
            "isDefault": is_default,
            "capabilities": capabilities_text(&dialect_name),
        },
        "labels": labels(&current.confidence),
        "status": { "text": status },
        // Lane L45: the two display switches ride every push, so a remount or
        // the standalone page restores them without a settings key.
        "display": {
            "keywordWrap": display.keyword_wrap,
            "vertical": display.vertical,
        },
    })
}

/// The `selectTab` value for the next push, while the page still has to open
/// the preview tab (`None` once it reported its tab).
pub fn select_tab() -> Option<&'static str> {
    preview_store().select_pending.then_some("preview")
}

// ---------------------------------------------------------------------------
// Where the preview is pushed
// ---------------------------------------------------------------------------

/// How the preview workbench is carried.
///
/// The workbench is one implementation (`web/preview.js`) with two mount
/// points, so the host has to name the live one before it pushes. Until API
/// 1.18 brings `[[contributes.dialogs]]` the answer is always
/// [`PreviewTarget::Tab`]: the preview rides the log panel's own `state` push,
/// and that panel (`sqlv-logs`) is the single view its messages go to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewTarget {
    /// The fifth tab of the log panel: the preview is part of that page's state.
    Tab,
}

impl PreviewTarget {
    /// The view a preview message is addressed to.
    ///
    /// The tab target is the log panel, because the tab *is* part of that page:
    /// the panel carries `tabs.preview` in the same `state` push as everything
    /// else, so one channel serves both halves of the view.
    pub fn view_id(self) -> &'static str {
        match self {
            Self::Tab => crate::views::log_panel::VIEW_ID,
        }
    }
}

/// Which mount point carries the preview right now.
///
/// This is the one place that answers it (lane L42). Callers must not hard-code
/// a view id: they ask this function, or [`Self::view_id`] on its answer. Moving
/// the preview to a dialog is a change here and in the `match` of
/// [`send_to_preview`], nowhere else.
pub fn preview_target() -> PreviewTarget {
    // The dialog mount (`web/preview.html` as its own `[[contributes.dialogs]]`
    // entry, view id `sqlv-preview`) needs API 1.18. It becomes a second
    // variant here; `send_to_preview` then posts `{op:"preview-tab", tab}` to
    // `PreviewTarget::view_id()` instead of leaving the delivery to the panel.
    PreviewTarget::Tab
}

/// The cross-view "send this statement to the preview" op (lane L42).
///
/// The log panel's quick-preview button posts `{op:"preview-send", line,
/// selectTab}`; the host resolves the line against the log it already holds,
/// renders it, retargets the input box, and lets [`preview_target`] decide where
/// the result goes. Written as a message crossing views so the two mounts stay
/// interchangeable: today both ends resolve to the same view id.
///
/// It stores and does not push: the panel answers every `preview*` op with one
/// `state` push, which is exactly how the tab mount delivers.
pub fn send_to_preview(line: usize, select_tab: bool) {
    let statement = crate::views::log_panel::snapshot()
        .entries
        .into_iter()
        .find(|entry| entry.line_no == line)
        .map(|entry| entry.sql)
        .filter(|sql| !sql.is_empty());
    if let Some(text) = statement {
        let (dialect_name, _) = configured_dialect();
        render_input_state(&text, "");
        preview_store().model.dialect = dialect_name;
        // A hand-off rewrites the input box, and it is what
        // `render.formatOnPreview` applies to (lane L34) — the same rule a
        // command handing a statement over gets.
        request_input_replace();
    }
    if select_tab {
        request_tab();
    }
    // The target itself is asked, so the branch that names a channel lives
    // here in one place instead of at each call site.
    match preview_target() {
        // The payload rides the panel's `state` push right after this op.
        PreviewTarget::Tab => {}
    }
}

fn labels(confidence: &str) -> Value {
    json!({
        "title": t("view.workbench.title"),
        "hintInput": t("view.workbench.hint.input"),
        "empty": t("view.workbench.empty"),
        "resolve": t("view.workbench.action.resolve"),
        "format": t("view.workbench.action.format"),
        "loadFile": t("view.workbench.action.load_file"),
        "clear": t("view.workbench.action.clear"),
        "openDialog": t("view.workbench.action.open_dialog"),
        "openDialogNote": t("view.workbench.open_dialog.note"),
        "copy": t("view.workbench.copy"),
        "copySql": t("view.workbench.action.copy_sql"),
        "export": t("view.workbench.export"),
        // Lane L35b: the preview toolbar follows the Reference order — Copy
        // SQL / Format / Display mode / Explain / Refresh, with Export and
        // Rollback folded into "More".
        "explain": t("view.workbench.action.explain"),
        "explainNote": t("view.workbench.execute.note"),
        "refresh": t("view.workbench.action.refresh"),
        "more": t("view.workbench.action.more"),
        "moreNote": t("view.workbench.more.note"),
        "bannerDismiss": t("view.workbench.banner.dismiss"),
        "paramsName": t("view.workbench.params.name"),
        "execute": t("view.workbench.action.execute"),
        "executeNote": t("view.workbench.execute.note"),
        "mode": t("view.workbench.mode"),
        "modeNormal": t("view.workbench.mode.normal"),
        "modeFormatted": t("view.workbench.mode.formatted"),
        "params": t("view.workbench.params"),
        "sql": t("view.workbench.sql"),
        "tabSql": t("view.workbench.tab.sql"),
        "tabRaw": t("view.workbench.tab.raw"),
        "tabColumns": t("view.workbench.tab.columns"),
        "columnsEmpty": t("view.workbench.columns.empty"),
        "reasonTitle": t("view.workbench.reason.title"),
        "dialect": t("view.workbench.dialect"),
        "dialectDefault": t("view.workbench.dialect.default_suffix"),
        "dialectHint": t("view.workbench.dialect.hint"),
        "confidence": t("view.workbench.confidence"),
        "confidenceLabel": confidence_label(confidence),
        // Lane L27: editable parameters, clipboard import and the rollback
        // entry.
        "rebind": t("view.workbench.action.rebind"),
        "importClipboard": t("view.workbench.action.import_clipboard"),
        "rollback": t("view.workbench.action.rollback"),
        "rollbackNote": t("view.workbench.rollback.note"),
        "paramsHint": t("view.workbench.params.hint"),
        "clipboardMissing": t("view.workbench.status.clipboard_missing"),
        "imported": t("view.workbench.status.imported"),
        "importFailed": t("view.workbench.status.import_failed"),
        "paramsEmpty": t("view.workbench.status.params_empty"),
        "paramsNoPlaceholder": t("view.workbench.status.params_no_placeholder"),
        "rebound": t("view.workbench.status.rebound"),
        "rollbackRefused": t("view.workbench.status.rollback_refused"),
        // Lane L44: "更多" gained diff / share / qualify. Diff and share work;
        // qualify cannot, because the renderer, not this page, would have to
        // resolve each column's table — the entry stays disabled and its
        // tooltip says so.
        "diff": t("view.workbench.action.diff"),
        "diffNote": t("view.workbench.diff.note"),
        "share": t("view.workbench.action.share"),
        "shareNote": t("view.workbench.share.note"),
        "qualify": t("view.workbench.action.qualify"),
        "qualifyNote": t("view.workbench.qualify.note"),
        // Lane L45: the statement picker and the two display switches. The
        // switches are page state (they never reach the host), but their text
        // is localised here like every other label.
        "statementLabel": t("view.workbench.statement"),
        "statementNote": t("view.workbench.statement.note"),
        "statementNone": t("view.workbench.statement.none"),
        "noStatements": t("view.workbench.status.no_statements"),
        "statementUnknown": t("view.workbench.status.statement_unknown"),
        "keywordWrap": t("view.workbench.display.keyword_wrap"),
        "keywordWrapNote": t("view.workbench.display.keyword_wrap.note"),
        "vertical": t("view.workbench.display.vertical"),
        "verticalNote": t("view.workbench.display.vertical.note"),
    })
}

// ---------------------------------------------------------------------------
// Messages from the page
// ---------------------------------------------------------------------------

/// Handles one `preview*` message the log panel routed here.
///
/// The log panel pushes its state right after, so this only updates the store:
/// the model, the status line and the input-replacement flag.
pub fn handle_preview_message(message: &Value) -> Result<(), Error> {
    let op = message.get("op").and_then(Value::as_str).unwrap_or("");
    // The page prefixes every op (`preview-render` …) so the log panel can
    // route it without colliding with its own `clear` / `export` / `copy`.
    let op = op.strip_prefix("preview").unwrap_or(op);
    let op = op.strip_prefix('-').unwrap_or(op);
    match op {
        // The packaged page asks for the state once its script is ready. The
        // input pane starts from the model (nothing has been typed yet).
        "ready" => {
            let current = model();
            let has_content = !current.raw.trim().is_empty() || !current.sql.trim().is_empty();
            set_status(String::new(), has_content);
        }
        "render" => {
            let input = message.get("text").and_then(Value::as_str).unwrap_or("");
            let kind = message.get("kind").and_then(Value::as_str).unwrap_or("");
            render_input_state(input, kind);
        }
        "format" => {
            let input = message.get("text").and_then(Value::as_str).unwrap_or("");
            format_input_state(input);
        }
        // Refresh (lane L35b): re-run the render on what the input box holds.
        // The box is not always the source of truth (a hand-off from a command
        // leaves it untouched), so an empty box falls back to the model's own
        // text instead of blanking the preview.
        "refresh" => {
            let input = message.get("text").and_then(Value::as_str).unwrap_or("");
            let text = if input.trim().is_empty() {
                model().raw
            } else {
                input.to_string()
            };
            render_input_state(&text, "");
        }
        "load-file" => load_file_state()?,
        // Statement switching (lane L45): the picker names an id from
        // `model.statements`, and the host re-renders that one out of the
        // mapper document it kept (`set_mapper_source`). Parameters, dialect
        // and display mode are the page's own state and are untouched; only the
        // model changes, so the push carries `replaceInput` for the input box
        // to follow the newly chosen statement.
        "statement" => {
            let id = message.get("id").and_then(Value::as_str).unwrap_or("");
            select_statement_state(id);
        }
        // Display switches (lane L45): the page reports the two toggles, the
        // host only remembers them so the next `state` push (and a page that
        // remounts) carries them back. Pure presentation — the model, its SQL
        // and the input box are untouched.
        "display" => {
            let mut store = preview_store();
            store.display = DisplayPrefs {
                keyword_wrap: message
                    .get("keywordWrap")
                    .and_then(Value::as_bool)
                    .unwrap_or(store.display.keyword_wrap),
                vertical: message
                    .get("vertical")
                    .and_then(Value::as_bool)
                    .unwrap_or(store.display.vertical),
            };
        }
        "clear" => {
            clear();
            set_status(t("view.workbench.status.cleared"), true);
        }
        "copy" => copy_sql()?,
        "export" => export_sql()?,
        // Lane L44: the reference's "差异对比" — the original on the left, the
        // rendered SQL on the right, opened in the platform diff viewer (the one
        // surface that can be popped out into a window). The payload is built
        // the same way `commands::preview_sql_diff` builds it.
        "diff" => open_diff_tab()?,
        // Lane L44: "分享" degrades to a local copy — this plugin never sends
        // anything over the network, and the clipboard is the whole share path.
        "share" => share_sql()?,
        // Parameter rebinding (lane L27): the page sends the edited values; the
        // host renders each as a SQL literal and substitutes them into the
        // model's SQL through the core log placeholder scanner.
        "params" => rebind_params(values_of(message)),
        // Rollback (lane L27): the workbench has no table/primary-key
        // metadata, so no plan can be built — the refusal is the designed
        // outcome, never a guessed table.
        "rollback" => set_status(t("view.workbench.status.rollback_refused"), false),
        // The dialect is read-only: it comes from the settings page. The op is
        // still answered so an older page does not wait for a reply.
        "dialect" => set_status(t("view.workbench.status.dialect_readonly"), false),
        // Lane L44: "含表列名" (qualify every column with its table). The switch
        // belongs to the renderer: it has to know which table each column came
        // from, which is exactly what this plugin does not have. There is no
        // such flag in `core::xml` or in the engine jar (`RenderRequest` carries
        // source / dialect / params / entity_tables only), so the entry is
        // disabled in the page and this branch — reached only by an old page —
        // says why instead of rewriting SQL here.
        "qualify" => set_status(t("view.workbench.status.qualify_refused"), false),
        // Lane L42: a row handed over from the log list. Resolving it here (not
        // in the page) keeps the log's parsed rows the single source, and the
        // destination is `preview_target`'s call, not this handler's.
        "send" => {
            let line = message
                .get("line")
                .and_then(Value::as_u64)
                .map(|v| v as usize);
            let select_tab = message
                .get("selectTab")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            match line {
                // Whether the row is renderable and where the result goes are
                // `send_to_preview`'s business; the tab request it sets is
                // cleared when the page reports its tab.
                Some(line) => {
                    send_to_preview(line, select_tab);
                }
                // No row chosen: the page still asked for the preview, so the
                // tab comes forward over the same boundary.
                None => {
                    if select_tab {
                        request_tab();
                    }
                    set_status(t("view.workbench.status.params_empty"), false);
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn render_input_state(input: &str, kind: &str) {
    let (dialect_name, _) = configured_dialect();
    let mut next = if input.trim().is_empty() {
        let mut empty = WorkbenchModel::new("", Vec::new(), &dialect_name);
        empty.raw = input.to_string();
        empty
    } else {
        render_input(input, kind, &dialect_name)
    };
    next.dialect = dialect_name;
    let status = status_for(&next);
    // Lane L45: a mapper render keeps its document, so the picker can switch.
    // Inline SQL has no statements to switch between and clears the stash.
    let mapper = (next.kind == KIND_XML).then(|| input.to_string());
    set_model(next);
    set_mapper_source(mapper);
    set_status(status, true);
}

/// Re-renders the statement the picker named (lane L45).
///
/// The document comes from the store, not from the page: the page only knows
/// the ids. An id the current model does not list — or a render with no mapper
/// behind it — is refused with a status line instead of guessing a statement.
fn select_statement_state(id: &str) {
    let Some(xml) = mapper_source() else {
        return set_status(t("view.workbench.status.no_statements"), false);
    };
    let current = model();
    // Accept both spellings: the list may carry `ns.id` while a caller still
    // names the bare `id` (the two renderers label statements differently).
    let listed = current
        .statements
        .iter()
        .find(|known| known.as_str() == id || known.ends_with(&format!(".{id}")));
    let Some(id) = listed.map(|known| known.to_string()) else {
        return set_status(
            t("view.workbench.status.statement_unknown").replace("{0}", id),
            false,
        );
    };
    let id = id.as_str();
    if current.statement == id {
        // The picker re-selected the statement already shown: nothing to
        // re-render, and the parameters the reader typed stay as they are.
        return;
    }
    let (dialect_name, _) = configured_dialect();
    let mut next = render_mapper_statement(&xml, Some(id), &dialect_name);
    next.dialect = dialect_name;
    // The picker keeps its own list: `preview-sql` lists a statement by its
    // qualified id, the input box by its bare one, and re-deriving the list
    // here would change every option under the reader's cursor. `source` and
    // `statement` both follow the choice.
    next.statements = current.statements;
    next.source = id.to_string();
    next.statement = id.to_string();
    let status = status_for(&next);
    set_model(next);
    set_mapper_source(Some(xml));
    set_status(status, true);
}

fn format_input_state(input: &str) {
    let (dialect_name, _) = configured_dialect();
    if detect_kind(input) == KIND_XML {
        let mut next = render_input(input, KIND_XML, &dialect_name);
        next.dialect = dialect_name;
        set_model(next);
        // The re-render is a fresh mapper document: the picker keeps working on
        // the statement the user is looking at (lane L45).
        set_mapper_source(Some(input.to_string()));
        return set_status(t("view.workbench.status.format_xml_skipped"), true);
    }
    let Some(found) = dialect::by_name(&dialect_name) else {
        let mut next = render_input(input, KIND_SQL, &dialect_name);
        next.dialect = dialect_name;
        set_model(next);
        return set_status(t("view.workbench.status.format_unknown_dialect"), true);
    };
    let formatted = format::format(input, found);
    let unchanged = formatted == input;
    let mut next = render_input(&formatted, KIND_SQL, &dialect_name);
    next.dialect = dialect_name;
    set_model(next);
    let status = if unchanged {
        t("view.workbench.status.format_unchanged")
    } else {
        t("view.workbench.status.formatted")
    };
    set_status(status, true);
}

fn copy_sql() -> Result<(), Error> {
    let current = model();
    if !current.has_sql() {
        set_status(t("view.workbench.status.copy_empty"), false);
        return Ok(());
    }
    host::copy_to_clipboard(&current.copy_text())?;
    set_status(t("view.workbench.copied"), false);
    Ok(())
}

fn export_sql() -> Result<(), Error> {
    let current = model();
    if !current.has_sql() {
        set_status(t("view.workbench.status.export_empty"), false);
        return Ok(());
    }
    let (dialect_name, _) = configured_dialect();
    let body = current.formatted_sql(&dialect_name);
    let confidence = if current.confidence.is_empty() {
        "n/a"
    } else {
        current.confidence.as_str()
    };
    let source = if current.source.is_empty() {
        "pasted"
    } else {
        current.source.as_str()
    };
    let text = format!(
        "-- mybatis-sql workbench export (dialect: {dialect_name}, confidence: {confidence})\n-- source: {source}\n{body}\n"
    );
    crate::commands::common::emit(&text)?;
    set_status(t("view.workbench.status.exported"), false);
    Ok(())
}

/// Opens the rendered SQL against its own source in the platform diff viewer
/// (lane L44).
///
/// The left side is the model's `raw` — what the user typed, the mapper
/// fragment a command handed over, or the log row a "quick preview" resolved —
/// and the right side is the executable SQL. Names follow the model's origin so
/// the diff header says which statement it is; a statement-only model (one a
/// command handed over) has no separate source text, so the left side falls back
/// to the SQL exactly as `preview_tab` does for the "原文" tab.
fn open_diff_tab() -> Result<(), Error> {
    let current = model();
    if !current.has_sql() {
        set_status(t("view.workbench.status.diff_empty"), false);
        return Ok(());
    }
    let left = if current.raw.is_empty() {
        current.sql.clone()
    } else {
        current.raw.clone()
    };
    let source = if current.source.is_empty() {
        t("view.workbench.diff.inline")
    } else {
        current.source.clone()
    };
    let diff = ContentDiff {
        left,
        right: current.sql.clone(),
        left_label: Some(t("preview.diff.left")),
        right_label: Some(t("preview.diff.right")),
        title: Some(format!("{}: {source}", t("view.workbench.diff.title"))),
    };
    host::open_diff(&diff, Some(DIFF_FILE_NAME))
        .map_err(|error| Error::Failed(error.to_string()))?;
    set_status(t("view.workbench.status.diff_opened"), false);
    Ok(())
}

/// Copies the rendered SQL — the reference's "分享" path, reduced to what this
/// platform can do without leaving the machine: there is no share service, no
/// external clipboard target and no network capability in the manifest, so the
/// clipboard is the whole share.
fn share_sql() -> Result<(), Error> {
    let current = model();
    if !current.has_sql() {
        set_status(t("view.workbench.status.share_empty"), false);
        return Ok(());
    }
    host::copy_to_clipboard(&current.copy_text())?;
    set_status(t("view.workbench.status.shared"), false);
    Ok(())
}

/// The `values` array of a `preview-params` message, as strings.
fn values_of(message: &Value) -> Vec<String> {
    message
        .get("values")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Rebinds the user's edited parameter values into the model's SQL (lane L27).
///
/// `sql` keeps its `?` placeholders; each value becomes a SQL literal — a
/// number / boolean / `null` stays bare, everything else is single-quoted with
/// `'` doubled — and `core::log`'s placeholder scanner substitutes them in
/// order (the same scanner the log panel's restoration uses, so quoting is
/// never applied twice). The raw values stay in `params` for further edits.
fn rebind_params(values: Vec<String>) {
    let current = model();
    if values.is_empty() || current.sql.trim().is_empty() {
        set_status(t("view.workbench.status.params_empty"), false);
        return;
    }
    let had_placeholders = current.sql.contains('?');
    let literals: Vec<String> = values.iter().map(|value| param_literal(value)).collect();
    let rebound = crate::core::log::restore_rendered(&current.sql, &literals);
    let mut next = current;
    next.sql = rebound;
    next.params = values;
    set_model(next);
    let status = if had_placeholders {
        t("view.workbench.status.rebound")
    } else {
        t("view.workbench.status.params_no_placeholder")
    };
    set_status(status, false);
}

/// One user-typed parameter value as a SQL literal.
fn param_literal(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.eq_ignore_ascii_case("null") {
        return "NULL".to_string();
    }
    if is_number(trimmed)
        || trimmed.eq_ignore_ascii_case("true")
        || trimmed.eq_ignore_ascii_case("false")
    {
        return trimmed.to_string();
    }
    let mut out = String::with_capacity(trimmed.len() + 2);
    out.push('\'');
    for ch in trimmed.chars() {
        if ch == '\'' {
            out.push_str("''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// `true` for a decimal number (optional sign, at most one dot, at least one
/// digit); anything else is a string literal.
fn is_number(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut index = 0;
    if bytes
        .first()
        .is_some_and(|byte| *byte == b'+' || *byte == b'-')
    {
        index = 1;
    }
    let rest = &bytes[index..];
    if rest.is_empty() {
        return false;
    }
    let mut dots = 0;
    let mut digits = 0;
    for byte in rest {
        match byte {
            b'0'..=b'9' => digits += 1,
            b'.' => {
                dots += 1;
                if dots > 1 {
                    return false;
                }
            }
            _ => return false,
        }
    }
    digits > 0
}

/// Lists the project's SQL and mapper XML files and lets the user pick one.
///
/// The SDK has no file-open dialog (only `pick` over a list and
/// `pick_directory`), and `host::open_editors` would need `editor.read`, which
/// this plugin does not declare — so "load from file" is a project-file picker.
fn load_file_state() -> Result<(), Error> {
    let projects = match host::projects() {
        Ok(projects) => projects,
        Err(error) => {
            set_status(
                t("view.workbench.status.load_no_project").replace("{0}", &error.to_string()),
                false,
            );
            return Ok(());
        }
    };
    let Some(project) = projects.first() else {
        set_status(
            t("view.workbench.status.load_no_project").replace("{0}", ""),
            false,
        );
        return Ok(());
    };

    let mut paths: Vec<String> = Vec::new();
    for glob in ["**/*.xml", "**/*.sql"] {
        match project.find_files(glob, PICK_LIMIT) {
            Ok(found) => paths.extend(found),
            Err(error) => {
                set_status(
                    t("view.workbench.status.load_failed")
                        .replace("{0}", glob)
                        .replace("{1}", &error.to_string()),
                    false,
                );
                return Ok(());
            }
        }
    }
    paths.sort();
    paths.dedup();
    if paths.is_empty() {
        set_status(t("view.workbench.status.load_no_files"), false);
        return Ok(());
    }

    let items: Vec<PickItem> = paths.iter().map(|path| PickItem::new(path, path)).collect();
    let chosen = match host::pick(&t("view.workbench.load_file.title"), &items) {
        Ok(chosen) => chosen,
        Err(error) => {
            set_status(error.to_string(), false);
            return Ok(());
        }
    };
    // Cancelled: the previous state stays as it was.
    let Some(path) = chosen else {
        return Ok(());
    };

    let text = match project.read_file(&path) {
        Ok(text) => text,
        Err(error) => {
            set_status(
                t("view.workbench.status.load_failed")
                    .replace("{0}", &path)
                    .replace("{1}", &error.to_string()),
                false,
            );
            return Ok(());
        }
    };
    let (dialect_name, _) = configured_dialect();
    let mut next = render_input(&text, kind_from_path(&path), &dialect_name);
    next.dialect = dialect_name;
    next.source = path.clone();
    // Lane L45: a loaded mapper file keeps its document for the statement
    // picker; a loaded .sql file has nothing to switch between.
    let mapper = (next.kind == KIND_XML).then(|| text.clone());
    set_model(next);
    set_mapper_source(mapper);
    set_status(
        t("view.workbench.status.loaded").replace("{0}", &path),
        true,
    );
    Ok(())
}

fn kind_from_path(path: &str) -> &'static str {
    if path.to_ascii_lowercase().ends_with(".xml") {
        KIND_XML
    } else {
        KIND_SQL
    }
}

fn status_for(current: &WorkbenchModel) -> String {
    if current.raw.trim().is_empty() && current.sql.trim().is_empty() {
        return t("view.workbench.status.empty");
    }
    match current.confidence.as_str() {
        "unresolved" => t("view.workbench.status.unresolved"),
        "partial" => t("view.workbench.status.partial"),
        _ => {
            let kind = if current.kind == KIND_XML {
                t("view.workbench.kind.xml")
            } else {
                t("view.workbench.kind.sql")
            };
            t("view.workbench.status.resolved")
                .replace("{0}", &kind)
                .replace("{1}", &current.params.len().to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Serialises tests that read or write the process-wide model: the `commands`
/// tests load logs and open the workbench too, and one lock per panel keeps a
/// parallel test from observing the other's state.
#[cfg(test)]
pub(crate) fn test_store_lock() -> super::StateLockGuard {
    super::test_state_lock()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::common;
    use openidea_plugin::mock::MockHost;

    fn test_host() -> (MockHost, super::super::StateLockGuard) {
        let guard = test_store_lock();
        let host = MockHost::install();
        clear();
        note_page_tab();
        // A fresh store: the status and the one-shot flag from another test
        // must not leak into this one.
        let mut store = preview_store();
        store.status.clear();
        store.replace_input = false;
        store.handoff = false;
        // Lane L45: the display switches are process-wide too, so a test that
        // toggled them must not decide the next test's starting state.
        store.display = DisplayPrefs::default();
        drop(store);
        (host, guard)
    }

    fn sample() -> WorkbenchModel {
        WorkbenchModel {
            sql: "SELECT id, name FROM \"user\" WHERE id = 1".to_string(),
            params: vec!["1".to_string()],
            dialect: "MySQL".to_string(),
            confidence: "exact".to_string(),
            source: "UserMapper.xml#findById".to_string(),
            raw: "SELECT id, name FROM \"user\" WHERE id = 1".to_string(),
            ..WorkbenchModel::default()
        }
    }

    fn send(value: Value) {
        handle_preview_message(&value).expect("the op is answered");
    }

    /// The `tabs.preview` object the log panel would carry.
    fn tab() -> Value {
        preview_tab()
    }

    /// XML text nodes leave runs of whitespace in the rendered SQL.
    fn compact(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn model_round_trips_through_json() {
        let model = sample();
        let json = serde_json::to_string(&model).unwrap();
        let decoded: WorkbenchModel = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, model);
    }

    #[test]
    fn switching_dialects_changes_the_display_fields() {
        let mut model = sample();
        let mysql = model.dialect_display().expect("MySQL is a known dialect");
        assert_eq!(mysql.quote_open, "`");
        assert_eq!(mysql.keyword_case, "Upper");

        assert!(model.with_dialect("PostgreSQL"));
        let pg = model
            .dialect_display()
            .expect("PostgreSQL is a known dialect");
        assert_eq!(pg.quote_open, "\"");
        assert_eq!(pg.pagination, "Pg");
        assert_ne!(pg, mysql);

        assert!(
            !model.with_dialect("NoSuchDb"),
            "unknown dialects are rejected"
        );
        assert_eq!(model.dialect, "PostgreSQL");
    }

    #[test]
    fn copy_text_is_the_executable_sql() {
        // No comments in the sample: Copy hands the statement over verbatim.
        let model = sample();
        assert_eq!(model.copy_text(), model.sql);
    }

    #[test]
    fn copy_text_strips_comments_like_the_reference() {
        // The reference's Copy button runs `SqlCommentStripper`: line and block
        // comments go, comment-only lines disappear, trailing blanks are
        // trimmed. A copied statement must be the one that runs.
        let mut model = sample();
        model.sql = "SELECT id, -- the key\n       name /* display */\nFROM t\n-- trailing note\nWHERE id = 1".to_string();
        assert_eq!(
            model.copy_text(),
            "SELECT id,\n       name\nFROM t\nWHERE id = 1"
        );
    }

    #[test]
    fn copy_text_keeps_comment_markers_inside_quoted_spans() {
        let mut model = sample();
        model.sql =
            "SELECT '-- not a comment', \"/* nor this */\", `--x` FROM t -- real\n".to_string();
        assert_eq!(
            model.copy_text(),
            "SELECT '-- not a comment', \"/* nor this */\", `--x` FROM t\n"
        );
    }

    #[test]
    fn export_text_is_the_statement_formatted_for_the_dialect() {
        let model = sample();
        let found = dialect::by_name("MySQL").expect("MySQL");
        assert_eq!(model.export_text(), format::format(&model.sql, found));
    }

    #[test]
    fn the_preview_tab_carries_the_model_labels_and_read_only_dialect() {
        let (_host, _guard) = test_host();
        set_model(sample());
        let tab = tab();
        assert_eq!(tab["model"]["sql"], sample().sql);
        assert_eq!(tab["model"]["tabs"]["raw"], sample().raw);
        assert!(tab["model"]["tabs"]["formatted"].is_string());
        assert_eq!(tab["model"]["tabs"]["columns"], "");
        assert!(tab["labels"]["hintInput"].as_str().is_some());
        assert!(tab["labels"]["columnsEmpty"].as_str().is_some());
        assert!(tab["dialect"]["capabilities"].is_string());
        assert_eq!(tab["dialect"]["isDefault"], true);
        assert_eq!(tab["replaceInput"], false);
        assert_eq!(tab["status"]["text"], "");
    }

    #[test]
    fn a_ready_message_fills_the_input_from_a_statement_only_model() {
        let (_host, _guard) = test_host();
        // `commands::open_workbench` sets only `sql`; the input pane still has
        // to start from it instead of an empty box.
        let mut current = WorkbenchModel::new("select 1", Vec::new(), "MySQL");
        current.confidence = "exact".to_string();
        set_model(current);
        send(json!({ "op": "preview-ready" }));
        let state = tab();
        assert_eq!(state["replaceInput"], true);
        assert_eq!(state["model"]["tabs"]["raw"], "select 1");
        // The flag is one-shot: the next push (a filter, a capture refresh)
        // leaves what the user typed alone.
        assert_eq!(tab()["replaceInput"], false);
    }

    #[test]
    fn render_asks_the_page_to_replace_the_input() {
        let (_host, _guard) = test_host();
        send(json!({ "op": "preview-render", "text": "select 2" }));
        let tab = tab();
        assert_eq!(tab["replaceInput"], true);
        assert_eq!(tab["model"]["tabs"]["sql"], "select 2");
    }

    #[test]
    fn copy_and_export_leave_the_typed_input_alone() {
        let (_host, _guard) = test_host();
        set_model(sample());
        for op in ["preview-copy", "preview-export"] {
            send(json!({ "op": op }));
            assert_eq!(tab()["replaceInput"], false, "op {op}");
        }
    }

    #[test]
    fn rendering_plain_sql_is_exact_and_keeps_the_text_verbatim() {
        let (_host, _guard) = test_host();
        let text = "select id from user where id = ?\n";
        send(json!({ "op": "preview-render", "text": text, "kind": "sql" }));
        let current = model();
        assert_eq!(current.kind, KIND_SQL);
        assert_eq!(current.confidence, "exact");
        assert_eq!(current.raw, text, "the raw tab keeps the input verbatim");
        assert_eq!(current.sql, "select id from user where id = ?");
        assert!(current.reason.is_empty());
        let tab = tab();
        assert_eq!(
            tab["model"]["tabs"]["sql"],
            "select id from user where id = ?"
        );
        assert_eq!(tab["model"]["tabs"]["raw"], text);
    }

    #[test]
    fn rendering_a_mapper_fragment_goes_through_the_xml_renderer() {
        let (_host, _guard) = test_host();
        let text = r#"<mapper namespace="com.example.UserMapper">
            <select id="findById">SELECT id, name FROM users WHERE id = #{id}</select>
        </mapper>"#;
        send(json!({ "op": "preview-render", "text": text }));
        let current = model();
        assert_eq!(current.kind, KIND_XML);
        assert_eq!(current.confidence, "exact");
        assert_eq!(current.source, "findById");
        assert!(
            compact(&current.sql).contains("SELECT id, name FROM users WHERE id = ?"),
            "{}",
            current.sql
        );
    }

    #[test]
    fn an_unresolved_fragment_ships_no_sql_but_keeps_the_reason() {
        let (_host, _guard) = test_host();
        let text = r#"<mapper namespace="a">
            <sql id="one"><include refid="two"/></sql>
            <sql id="two"><include refid="one"/></sql>
            <select id="s">SELECT <include refid="one"/> FROM t</select>
        </mapper>"#;
        send(json!({ "op": "preview-render", "text": text, "kind": "xml" }));
        let current = model();
        assert_eq!(current.confidence, "unresolved");
        assert!(
            current.sql.trim().is_empty(),
            "unresolved never carries SQL"
        );
        assert!(
            current.reason.contains("xml.include.cycle"),
            "{}",
            current.reason
        );
        let tab = tab();
        assert_eq!(tab["model"]["tabs"]["sql"], "");
        assert!(
            tab["status"]["text"]
                .as_str()
                .is_some_and(|text| !text.is_empty())
        );
    }

    #[test]
    fn a_fragment_without_statements_is_unresolved() {
        let (_host, _guard) = test_host();
        send(json!({
            "op": "preview-render",
            "text": "<mapper namespace=\"a\"/>",
            "kind": "xml",
        }));
        let current = model();
        assert_eq!(current.confidence, "unresolved");
        assert!(current.sql.is_empty());
        assert!(
            current
                .reason
                .contains(crate::core::xml::KEY_STATEMENT_NOT_FOUND)
        );
    }

    #[test]
    fn the_state_marks_an_unresolved_render_degraded() {
        // Lane L35b: the page wears a degradation strip while the render is
        // `unresolved`. The state carries the flag and the reasons (every line
        // starts with its message key), so the strip needs no extra lookup.
        let (_host, _guard) = test_host();
        send(json!({
            "op": "preview-render",
            "text": "<mapper namespace=\"a\"/>",
            "kind": "xml",
        }));
        let state = tab();
        assert_eq!(state["model"]["degraded"], true);
        assert_eq!(state["model"]["confidence"], "unresolved");
        let reason = state["model"]["reason"].as_str().expect("the reasons");
        assert!(
            reason.contains(crate::core::xml::KEY_STATEMENT_NOT_FOUND),
            "{reason}"
        );
        assert!(
            reason.lines().all(|line| line.starts_with('[')),
            "each reason line carries its key: {reason}"
        );
        // A clean render clears the flag, so the strip hides itself again.
        send(json!({ "op": "preview-render", "text": "select 1" }));
        let state = tab();
        assert_eq!(state["model"]["degraded"], false);
        assert_eq!(state["model"]["confidence"], "exact");
    }

    #[test]
    fn a_refresh_message_re_renders_the_input() {
        // Lane L35b: the toolbar's Refresh re-runs the render. What the box
        // holds wins; an empty box falls back to the model's own text (a
        // hand-off from a command leaves the box untouched).
        let (_host, _guard) = test_host();
        send(json!({ "op": "preview-refresh", "text": "select 2" }));
        let state = tab();
        assert_eq!(state["model"]["sql"], "select 2");
        assert_eq!(state["replaceInput"], true);

        set_model(sample());
        send(json!({ "op": "preview-refresh", "text": "" }));
        let state = tab();
        assert_eq!(
            state["model"]["tabs"]["raw"],
            sample().raw,
            "the model text stands in for an empty box"
        );
        assert_eq!(state["replaceInput"], true);
    }

    #[test]
    fn the_preview_page_wears_the_reference_layout() {
        // Lane L35b: the preview half follows `SqlPreviewPanel` — meta row
        // (dialect chip + advice), toolbar (Copy SQL / Format / Display mode /
        // Explain / Refresh), the splitter columns (parameters left, result
        // tabs right), then a status-only footer. The duplicate bottom Copy
        // SQL and the disabled Execute are gone: Explain replaces the latter
        // in the toolbar and Export / Rollback fold behind "More".
        //
        // Lane L42: the workbench moved out of the panel page into the module
        // `web/preview.js` (with `web/preview.css`); the layout assertions
        // follow it there, and the panel page keeps only the mount host.
        let page = include_str!("../../web/log-panel.html");
        let module = include_str!("../../web/preview.js");
        let css = include_str!("../../web/preview.css");
        // The input half exists and holds the parameter list: both lookups are
        // the assertion, so the spans themselves are not kept.
        let _pane_start = module.find("id=\"pvPaneTop\"").expect("the preview markup");
        let _pane_end = module
            .find("id=\"pvParamsList\"")
            .expect("the parameter list");

        // Vertical order: meta row, toolbar, degradation strip, columns. The
        // input half above has a toolbar of its own, so the preview toolbar is
        // the first one *after* the meta row.
        let meta = module.find("class=\"pv-meta\"").expect("meta row");
        let toolbar = meta
            + module[meta..]
                .find("class=\"pv-toolbar\"")
                .expect("preview toolbar");
        let banner = module.find("id=\"pvBanner\"").expect("degradation strip");
        let cols = module.find("class=\"pv-cols\"").expect("split columns");
        assert!(meta < toolbar, "the meta row sits above the toolbar");
        assert!(toolbar < banner, "the strip sits under the toolbar");
        assert!(banner < cols, "the strip keeps clear of the columns");

        // Lane C: the performance advice left the meta row for a line of its
        // own (the reference's perf row), directly under the meta/toolbar row
        // and above the degradation strip.
        let perf = module.find("id=\"pvPerfRow\"").expect("perf row");
        assert!(meta < perf, "the perf row sits under the meta row");
        assert!(perf < toolbar, "the perf row keeps the toolbar row clear");
        assert!(perf < banner, "the perf row sits above the strip");

        // Toolbar order, the reference's entries plus "More": copy / format /
        // display mode / keyword wrap / vertical / explain / refresh / more.
        let order = [
            "id=\"pvCopy\"",
            "id=\"pvFormatPreview\"",
            "id=\"pvMode\"",
            "id=\"pvKeywordWrap\"",
            "id=\"pvVertical\"",
            "id=\"pvExplain\"",
            "id=\"pvRefresh\"",
            "id=\"pvMore\"",
        ];
        let mut previous = 0;
        for id in order {
            let at = module
                .find(id)
                .unwrap_or_else(|| panic!("missing toolbar entry {id}"));
            assert!(at >= previous, "{id} is out of the reference order");
            previous = at;
        }
        // Lane C: the display-mode control is the reference's toggle button
        // (its label names the target mode), and keyword wrap / vertical are
        // direct toolbar entries rather than entries of the "More" fold.
        assert!(
            module.contains("id=\"pvMode\" class=\"btn pv-btn-text\""),
            "the mode toggle is a text button"
        );
        assert!(
            module.contains("id=\"pvKeywordWrap\" class=\"btn pv-btn-text\""),
            "keyword wrap is in the toolbar"
        );
        assert!(
            module.contains("id=\"pvVertical\" class=\"btn pv-btn-text\""),
            "vertical is in the toolbar"
        );
        assert!(
            module.contains("on('pvMode', function () { setMode(modeTarget()); });"),
            "the mode button toggles"
        );
        // Format in the result toolbar is the reference's display-mode switch,
        // not a rewrite of the source text on the input half.
        assert!(
            module.contains("on('pvFormatPreview', function () { setMode('formatted'); });"),
            "result Format switches the display mode"
        );
        // Explain is disabled and explains itself; the fold holds Export and
        // the (disabled) Rollback.
        assert!(
            module.contains("id=\"pvExplain\" class=\"btn\" type=\"button\" disabled"),
            "Explain stays disabled"
        );
        assert!(
            module.contains("explainButton.title = labels.explainNote"),
            "the reason"
        );
        assert!(module.contains("id=\"pvExport\""), "Export lives in More");
        assert!(
            module.contains("id=\"pvRollback\""),
            "Rollback lives in More"
        );
        assert!(module.contains("role=\"menu\""), "the fold is a menu");
        // Lane L44: the fold gained Diff and Share (both live) and Qualify (the
        // one entry the renderer would have to provide, so it is disabled with
        // the reason in its tooltip).
        for id in ["id=\"pvDiff\"", "id=\"pvShare\"", "id=\"pvQualify\""] {
            assert!(module.contains(id), "{id} lives in More");
        }
        assert!(
            module.contains("id=\"pvQualify\" class=\"pv-more-item\" type=\"button\" role=\"menuitem\"\n                disabled"),
            "Qualify is disabled in the markup too"
        );

        // Two columns: parameters at 30%, result tabs beside them, in the
        // reference tab order SQL / Original / Columns.
        let params = module
            .find("class=\"pv-params\"")
            .expect("parameter column");
        let sqlside = module.find("class=\"pv-sqlside\"").expect("result column");
        assert!(params < sqlside, "parameters sit left of the result tabs");
        // Lane C: the two columns are separated by a draggable splitter (the
        // reference's horizontal OnePixelSplitter, default 0.3).
        let hsplit = module
            .find("id=\"pvHSplitter\"")
            .expect("the horizontal splitter");
        assert!(params < hsplit, "the splitter sits right of the parameters");
        assert!(
            hsplit < sqlside,
            "the splitter sits left of the result tabs"
        );
        assert!(
            module.contains("setParamsFraction((event.clientX - rect.left) / rect.width);"),
            "the horizontal splitter drags"
        );
        assert!(
            css.contains(".pv-root .pv-hsplitter"),
            "the splitter has its own rule"
        );
        // Lane C: the meta row names where the render came from, and the SQL
        // tab is syntax-coloured (the reference's editor look), while the
        // 原文 / 列信息 tabs and placeholders stay plain.
        assert!(
            module.contains("id=\"pvSource\""),
            "the meta row carries the source"
        );
        assert!(
            module.contains("source.textContent = m.source || '';"),
            "the source is painted"
        );
        // The input hint is guidance for an empty box only, as in the
        // reference's standalone workbench.
        assert!(
            module.contains("hint.hidden = editor.value.trim() !== '';"),
            "the input hint hides once there is text"
        );
        assert!(
            module.contains("export function tokenizeSql"),
            "the SQL tokenizer exists"
        );
        assert!(
            module.contains("paintSql(body, text);"),
            "the SQL tab is painted from tokens"
        );
        assert!(
            css.contains(".pv-root .pv-tok-kw"),
            "the token colours have their own rules"
        );
        assert!(
            css.contains(".pv-root .pv-params {"),
            "the parameter column has its own rule"
        );
        let mut previous = 0;
        for tab in ["sql", "raw", "columns"] {
            let at = module
                .find(&format!("data-pvtab=\"{tab}\""))
                .unwrap_or_else(|| panic!("missing result tab {tab}"));
            assert!(at >= previous, "result tab {tab} is out of order");
            previous = at;
        }

        // Editable parameters in two columns (name, value) — the reference's
        // name-beside-input layout — and the empty state.
        assert!(
            module.contains("class=\"pv-params-actions\""),
            "Rebind / Import stay"
        );
        assert!(
            module.contains("id=\"pvParamsList\""),
            "the parameter list host"
        );
        assert!(css.contains(".pv-param-name"), "the name column");
        assert!(css.contains("pv-param-input"), "the value input");
        assert!(
            css.contains("grid-template-columns: minmax(48px, 38%) minmax(0, 1fr)"),
            "name and value share the row"
        );
        assert!(css.contains("pv-params-empty"), "the no-parameter state");
        assert!(
            module.contains("labels.columnsEmpty"),
            "the columns tab keeps its empty state text"
        );

        // Status-only footer: the duplicate Copy SQL and the old Execute are
        // gone, and nothing else lives down there.
        let footer_start = module.find("<div class=\"pv-footer\">").expect("footer");
        let footer_end = footer_start + module[footer_start..].find("</div>").expect("footer end");
        let footer = &module[footer_start..footer_end];
        assert!(footer.contains("id=\"pvStatus\""), "the status line");
        assert!(
            !footer.contains("class=\"btn\""),
            "the footer holds no controls: {footer}"
        );
        for gone in ["id=\"pvCopySql\"", "id=\"pvExecute\""] {
            assert!(
                !module.contains(gone),
                "the duplicate control {gone} is gone"
            );
        }

        // The degradation strip is closable and driven by the state's flag.
        assert!(module.contains("class=\"pv-banner\""), "the strip");
        assert!(module.contains("id=\"pvBannerClose\""), "the strip closes");
        assert!(
            module.contains("if (!m.degraded) {"),
            "the strip follows the model's flag"
        );
        assert!(
            module.contains("pvBannerDismissed"),
            "a dismissed strip stays hidden until the next render"
        );
        // The panel page mounts the module on the fifth tab and carries no
        // workbench DOM of its own: one implementation, two mount points.
        assert!(
            page.contains("import { mountPreview } from './preview.js'"),
            "the panel imports the module"
        );
        assert!(
            page.contains("mountPreview($('panelPreview'), { post: post })"),
            "the module is mounted on the fifth tab"
        );
        assert!(
            !page.contains("id=\"pvDialectChip\""),
            "the workbench markup lives in the module now"
        );
        assert!(
            !page.contains("iframe"),
            "a packaged page cannot nest one: the bridge is not in this frame's children"
        );
    }

    #[test]
    fn the_preview_target_is_the_fifth_tab_of_the_log_panel() {
        // Lane L42: one function answers where the preview is pushed, so the
        // switch to the API 1.18 dialog is a return value, not a hunt. Today
        // the answer is the tab, and the tab *is* the log panel's page.
        assert_eq!(preview_target(), PreviewTarget::Tab);
        assert_eq!(preview_target().view_id(), crate::views::log_panel::VIEW_ID);
        // No view id is hard-coded at a call site: the one address used is the
        // target's own.
        let source = include_str!("workbench.rs");
        assert!(
            !source.contains("post_view_message(\"sqlv-preview\""),
            "a dialog view id may not be hard-coded before it exists"
        );
    }

    #[test]
    fn a_send_op_renders_the_row_and_asks_for_the_tab() {
        // Lane L42: the log list hands a row over by line; the host resolves it
        // and renders it exactly as a typed statement would be. The panel's op
        // loop pushes the state right after, and the tab request rides it.
        let (_host, _guard) = test_host();
        note_page_tab();
        handle_preview_message(&json!({
            "op": "preview-send",
            "selectTab": true,
        }))
        .expect("a hand-off without a line is answered");
        assert!(
            select_tab().is_some(),
            "the tab still comes forward with no row chosen"
        );

        // A row the log does not hold renders nothing but still opens the tab.
        note_page_tab();
        handle_preview_message(&json!({ "op": "preview-send", "line": 999_999 })).unwrap();
        assert!(model().sql.is_empty(), "no row, no statement");
        assert!(select_tab().is_some(), "the tab is still requested");

        // With a row present the statement lands in the model.
        crate::views::log_panel::set_log_text(
            "==>  Preparing: SELECT id FROM user WHERE id = ?\n\
             ==> Parameters: 7(Long)\n\
             <==      Total: 1",
        );
        note_page_tab();
        let line = crate::views::log_panel::snapshot()
            .entries
            .iter()
            .find(|entry| entry.has_sql())
            .expect("a parsed row")
            .line_no;
        handle_preview_message(&json!({ "op": "preview-send", "line": line })).unwrap();
        let current = model();
        assert!(
            current.sql.to_uppercase().contains("SELECT ID FROM USER"),
            "the row's statement is rendered: {}",
            current.sql
        );
        // The log row's SQL is already restored, so it arrives with no `?`
        // placeholders and the model carries no parameter list: the value is
        // inlined in the statement, exactly as the row shows it.
        assert!(
            current.params.is_empty(),
            "a restored statement has no placeholders: {:?}",
            current.params
        );
        assert!(
            current.sql.contains('7'),
            "the restored value is inlined: {}",
            current.sql
        );
        assert!(select_tab().is_some(), "the hand-off opens the preview tab");
    }

    #[test]
    fn preview_send_resolves_the_line_through_the_shared_log_snapshot() {
        // The op is the cross-view "send to preview": the panel names a line,
        // the host renders it. A line whose row carries no SQL (an error row)
        // leaves the model alone instead of blanking it.
        let (_host, _guard) = test_host();
        set_model(sample());
        crate::views::log_panel::set_log_text(
            "### Error updating database. Cause: java.sql.SQLException: boom",
        );
        let error_line = crate::views::log_panel::snapshot()
            .entries
            .first()
            .map(|entry| entry.line_no)
            .expect("the error row");
        handle_preview_message(&json!({ "op": "preview-send", "line": error_line })).unwrap();
        assert_eq!(model().sql, sample().sql, "a row without SQL never blanks");
    }

    #[test]
    fn formatting_rewrites_the_input_and_re_renders_it() {
        let (_host, _guard) = test_host();
        send(json!({ "op": "preview-format", "text": "select id from user" }));
        let current = model();
        assert_eq!(current.kind, KIND_SQL);
        assert_eq!(current.confidence, "exact");
        assert_ne!(current.raw, "select id from user", "the input is rewritten");
        assert!(
            current.raw.to_uppercase().contains("SELECT"),
            "{}",
            current.raw
        );
    }

    #[test]
    fn formatting_leaves_a_mapper_fragment_alone() {
        let (_host, _guard) = test_host();
        let text = "<mapper namespace=\"a\"><select id=\"s\">select 1</select></mapper>";
        send(json!({ "op": "preview-format", "text": text }));
        assert_eq!(model().raw, text, "XML is not SQL-formatted");
        assert_eq!(
            tab()["status"]["text"],
            t("view.workbench.status.format_xml_skipped")
        );
    }

    #[test]
    fn an_unknown_dialect_setting_falls_back_to_mysql() {
        let (_host, _guard) = test_host();
        assert_eq!(configured_dialect(), ("MySQL".to_string(), true));
    }

    #[test]
    fn the_dialect_comes_from_the_settings_page_and_is_read_only() {
        let (host, _guard) = test_host();
        host.set_setting("dialect.default", "\"PostgreSQL\"");
        set_model(sample());
        let (name, is_default) = configured_dialect();
        assert_eq!(name, "PostgreSQL");
        assert!(!is_default);

        // A page that still sends the old op cannot change it.
        send(json!({ "op": "preview-dialect", "value": "Oracle" }));
        assert_eq!(model().dialect, "MySQL", "the op changes nothing");
        let tab = tab();
        assert_eq!(tab["dialect"]["name"], "PostgreSQL");
        assert_eq!(tab["dialect"]["isDefault"], false);
        assert_eq!(
            tab["status"]["text"],
            t("view.workbench.status.dialect_readonly")
        );
        assert_eq!(tab["model"]["dialect"], "PostgreSQL");
    }

    #[test]
    fn the_legacy_dialect_key_is_honoured_too() {
        let (host, _guard) = test_host();
        host.set_setting("sql.dialect", "\"Oracle\"");
        assert_eq!(configured_dialect(), ("Oracle".to_string(), false));
    }

    #[test]
    fn the_copy_message_reaches_the_clipboard() {
        let (host, _guard) = test_host();
        set_model(sample());
        send(json!({ "op": "preview-copy" }));
        assert_eq!(host.clipboard().as_deref(), Some(sample().sql.as_str()));
        assert_eq!(tab()["status"]["text"], t("view.workbench.copied"));
    }

    #[test]
    fn copying_without_sql_reports_the_status_and_leaves_the_clipboard_alone() {
        let (host, _guard) = test_host();
        send(json!({ "op": "preview-copy" }));
        assert!(host.clipboard().is_none());
        assert_eq!(
            tab()["status"]["text"],
            t("view.workbench.status.copy_empty")
        );
    }

    #[test]
    fn exporting_writes_the_sql_to_the_output_channel() {
        let (host, _guard) = test_host();
        set_model(sample());
        send(json!({ "op": "preview-export" }));
        let out = &host.output()[common::CHANNEL];
        assert!(out.contains("mybatis-sql workbench export"), "{out}");
        assert!(
            compact(out).contains(&compact(&sample().sql)),
            "the exported body is the formatted statement: {out}"
        );
        assert_eq!(tab()["status"]["text"], t("view.workbench.status.exported"));
    }

    #[test]
    fn exporting_without_sql_reports_the_status() {
        let (host, _guard) = test_host();
        send(json!({ "op": "preview-export" }));
        assert!(!host.output().contains_key(common::CHANNEL));
        assert_eq!(
            tab()["status"]["text"],
            t("view.workbench.status.export_empty")
        );
    }

    #[test]
    fn clearing_empties_the_model_and_tells_the_page() {
        let (_host, _guard) = test_host();
        set_model(sample());
        send(json!({ "op": "preview-clear" }));
        let current = model();
        assert!(current.sql.is_empty());
        assert!(current.raw.is_empty());
        assert_eq!(current.confidence, "");
        let tab = tab();
        assert_eq!(tab["model"]["tabs"]["sql"], "");
        assert_eq!(tab["model"]["tabs"]["raw"], "");
        assert_eq!(tab["replaceInput"], true);
        assert_eq!(tab["status"]["text"], t("view.workbench.status.cleared"));
    }

    #[test]
    fn loading_a_file_picks_it_from_the_project_and_sets_the_source() {
        let (host, _guard) = test_host();
        host.set_project_root(Some("/project"));
        host.add_file(
            "/project/UserMapper.xml",
            "<mapper namespace=\"a\"><select id=\"s\">select 1</select></mapper>",
        );
        host.answer_next_prompt(Some(&["/project/UserMapper.xml"]));
        send(json!({ "op": "preview-load-file" }));
        let current = model();
        assert_eq!(current.source, "/project/UserMapper.xml");
        assert_eq!(current.kind, KIND_XML);
        assert!(!current.sql.is_empty());
        assert_eq!(
            tab()["status"]["text"],
            t("view.workbench.status.loaded").replace("{0}", "/project/UserMapper.xml")
        );
    }

    #[test]
    fn a_cancelled_file_pick_changes_nothing() {
        let (host, _guard) = test_host();
        host.set_project_root(Some("/project"));
        host.add_file("/project/a.sql", "select 1");
        set_model(sample());
        host.answer_next_prompt(None);
        send(json!({ "op": "preview-load-file" }));
        assert_eq!(model(), sample());
    }

    #[test]
    fn loading_without_sql_or_xml_files_reports_it() {
        let (host, _guard) = test_host();
        host.set_project_root(Some("/project"));
        host.add_file("/project/readme.md", "# notes");
        send(json!({ "op": "preview-load-file" }));
        assert_eq!(
            tab()["status"]["text"],
            t("view.workbench.status.load_no_files")
        );
    }

    #[test]
    fn display_fields_are_localised_and_ordered() {
        let (_host, _guard) = test_host();
        let fields = sample().display_fields();
        assert_eq!(fields[0].0, t("view.workbench.sql"));
        assert_eq!(fields[0].1, sample().sql);
        assert_eq!(fields[2].0, t("view.workbench.dialect"));
        assert_eq!(fields[3].1, t("view.workbench.confidence.exact"));
    }

    #[test]
    fn the_state_carries_the_three_tabs_and_the_advice_line() {
        let (_host, _guard) = test_host();
        send(json!({
            "op": "preview-render",
            "text": "select * from user where name like '%x%'",
            "kind": "sql",
        }));
        let tab = tab();
        let tabs = &tab["model"]["tabs"];
        for key in ["sql", "formatted", "raw", "columns"] {
            assert!(tabs.get(key).is_some(), "tabs.{key} is missing");
        }
        assert_eq!(tabs["columns"], "");
        assert!(
            tab["model"]["advice"]
                .as_str()
                .is_some_and(|advice| !advice.is_empty()),
            "the meta line carries an advice line"
        );
    }

    #[test]
    fn an_empty_input_reports_the_empty_status_and_keeps_what_was_typed() {
        let (_host, _guard) = test_host();
        send(json!({ "op": "preview-render", "text": "   " }));
        let current = model();
        assert!(current.sql.is_empty());
        assert_eq!(current.raw, "   ");
        assert_eq!(tab()["status"]["text"], t("view.workbench.status.empty"));
    }

    #[test]
    fn the_tab_request_is_carried_until_the_page_reports_its_tab() {
        let (_host, _guard) = test_host();
        assert_eq!(select_tab(), None, "nothing requested yet");
        request_tab();
        assert_eq!(select_tab(), Some("preview"));
        // Repeated pushes keep asking: a push sent before the page loaded must
        // not be the only chance the page has to see the request.
        assert_eq!(select_tab(), Some("preview"));
        note_page_tab();
        assert_eq!(select_tab(), None);
    }

    #[test]
    fn format_on_preview_formats_the_handed_off_statement() {
        // Lane L34: a command hands a statement over (`set_model` +
        // `request_input_replace`); once the page reports its tab,
        // `render.formatOnPreview` (default on) formats the model.
        let (_host, _guard) = test_host();
        let input = "select id from t where a = 1";
        set_model(WorkbenchModel::new(input, Vec::new(), "MySQL"));
        request_input_replace();
        assert!(
            format_handoff_sql(),
            "the handed-off statement is formatted"
        );
        let mysql = dialect::by_name("MySQL").expect("MySQL");
        assert_eq!(model().sql, format::format(input, mysql));
        assert_ne!(model().sql, input);
        assert_eq!(
            tab()["replaceInput"],
            true,
            "the input box follows the formatted statement"
        );
    }

    #[test]
    fn format_on_preview_off_keeps_the_handed_off_statement_byte_for_byte() {
        let (host, _guard) = test_host();
        host.set_setting("render.formatOnPreview", "false");
        let input = "  select id\nfrom t where a=1  ";
        set_model(WorkbenchModel::new(input, Vec::new(), "MySQL"));
        request_input_replace();
        assert!(!format_handoff_sql());
        assert_eq!(model().sql, input);
    }

    #[test]
    fn format_on_preview_leaves_typed_renders_and_plain_models_alone() {
        // The setting is about restored SQL, not about text the user typed: a
        // `preview-render` and a model set without a hand-off stay verbatim.
        let (_host, _guard) = test_host();
        send(json!({ "op": "preview-render", "text": "select 2" }));
        assert_eq!(model().sql, "select 2");
        assert!(!format_handoff_sql(), "nothing was handed off");
        set_model(WorkbenchModel::new("select 3", Vec::new(), "MySQL"));
        assert!(!format_handoff_sql(), "a bare set_model is not a hand-off");
        assert_eq!(model().sql, "select 3");
    }

    #[test]
    fn an_unknown_op_and_an_unprefixed_op_change_nothing() {
        let (_host, _guard) = test_host();
        set_model(sample());
        send(json!({ "op": "preview-nope" }));
        assert_eq!(model(), sample());
        // The bare `copy` op is the log panel's own and must not reach here.
        send(json!({ "op": "copy" }));
        assert_eq!(model(), sample());
    }

    // Statement switching and the display switches (lane L45) -------------

    const TWO_STATEMENTS: &str = r#"<mapper namespace="com.example.UserMapper">
        <select id="findById">SELECT id FROM users WHERE id = #{id}</select>
        <update id="touch">UPDATE users SET name = #{name} WHERE id = #{id}</update>
    </mapper>"#;

    #[test]
    fn a_mapper_render_lists_its_statements_and_marks_the_chosen_one() {
        let (_host, _guard) = test_host();
        send(json!({ "op": "preview-render", "text": TWO_STATEMENTS, "kind": "xml" }));
        let current = model();
        assert_eq!(current.statements, vec!["findById", "touch"]);
        assert_eq!(current.statement, "findById", "the first one is shown");
        let state = tab();
        assert_eq!(state["model"]["statements"], json!(["findById", "touch"]));
        assert_eq!(state["model"]["statement"], "findById");
        // The document is kept, so a later switch needs no file from the page.
        assert!(mapper_source().is_some());
    }

    #[test]
    fn inline_sql_offers_no_statements_and_keeps_no_document() {
        let (_host, _guard) = test_host();
        send(json!({ "op": "preview-render", "text": "select 1" }));
        let current = model();
        assert!(current.statements.is_empty(), "no fake options");
        assert!(current.statement.is_empty());
        assert!(mapper_source().is_none());
        let state = tab();
        assert_eq!(state["model"]["statements"], json!([]));
    }

    #[test]
    fn a_single_statement_mapper_lists_one_entry() {
        // One entry is how the page tells "a mapper with nothing to choose"
        // (disable the picker) from "not a mapper" (hide it).
        let (_host, _guard) = test_host();
        send(json!({
            "op": "preview-render",
            "text": "<mapper namespace=\"a\"><select id=\"one\">select 1</select></mapper>",
        }));
        assert_eq!(model().statements, vec!["one"]);
    }

    #[test]
    fn the_statement_op_re_renders_the_chosen_statement() {
        let (_host, _guard) = test_host();
        send(json!({ "op": "preview-render", "text": TWO_STATEMENTS, "kind": "xml" }));
        assert!(compact(&model().sql).contains("SELECT id FROM users"));
        send(json!({ "op": "preview-statement", "id": "touch" }));
        let current = model();
        assert_eq!(current.statement, "touch");
        assert_eq!(current.source, "touch");
        assert!(
            compact(&current.sql).contains("UPDATE users SET name = ?"),
            "{}",
            current.sql
        );
        // The picker's list survives the switch.
        assert_eq!(current.statements, vec!["findById", "touch"]);
        let state = tab();
        assert_eq!(
            state["replaceInput"], true,
            "the input box follows the chosen statement"
        );
        assert_eq!(state["model"]["statement"], "touch");
        // The document is still held: a second switch works too.
        send(json!({ "op": "preview-statement", "id": "findById" }));
        assert!(compact(&model().sql).contains("SELECT id FROM users"));
    }

    #[test]
    fn the_statement_op_refuses_an_unknown_id_and_a_non_mapper_render() {
        let (_host, _guard) = test_host();
        send(json!({ "op": "preview-render", "text": TWO_STATEMENTS, "kind": "xml" }));
        send(json!({ "op": "preview-statement", "id": "noSuchStatement" }));
        assert_eq!(model().statement, "findById", "the shown one stays");
        assert_eq!(
            tab()["status"]["text"],
            t("view.workbench.status.statement_unknown").replace("{0}", "noSuchStatement")
        );

        send(json!({ "op": "preview-render", "text": "select 1" }));
        set_status(String::new(), false);
        send(json!({ "op": "preview-statement", "id": "findById" }));
        assert_eq!(model().sql, "select 1", "nothing to switch to");
        assert_eq!(
            tab()["status"]["text"],
            t("view.workbench.status.no_statements")
        );
    }

    #[test]
    fn the_statement_op_keeps_the_display_state_alone() {
        // A statement switch must not touch the two display switches, and the
        // switches must survive the push that follows it.
        let (_host, _guard) = test_host();
        send(json!({ "op": "preview-display", "keywordWrap": true, "vertical": false }));
        send(json!({ "op": "preview-render", "text": TWO_STATEMENTS, "kind": "xml" }));
        let before = display_prefs();
        send(json!({ "op": "preview-statement", "id": "touch" }));
        assert_eq!(display_prefs(), before);
        let state = tab();
        assert_eq!(state["display"]["keywordWrap"], true);
        assert_eq!(state["display"]["vertical"], false);
    }

    #[test]
    fn the_display_op_remembers_both_switches_and_never_touches_the_sql() {
        let (_host, _guard) = test_host();
        set_model(WorkbenchModel::new(
            "select a, b from t",
            Vec::new(),
            "MySQL",
        ));
        let sql = model().sql;

        send(json!({ "op": "preview-display", "keywordWrap": true }));
        assert_eq!(
            display_prefs(),
            DisplayPrefs {
                keyword_wrap: true,
                vertical: false
            }
        );
        send(json!({ "op": "preview-display", "vertical": true }));
        assert_eq!(
            display_prefs(),
            DisplayPrefs {
                keyword_wrap: true,
                vertical: true
            }
        );
        // A partial message keeps the field it does not carry.
        send(json!({ "op": "preview-display", "vertical": false }));
        assert!(display_prefs().keyword_wrap);

        let state = tab();
        assert_eq!(state["display"]["keywordWrap"], true);
        assert_eq!(state["display"]["vertical"], false);
        // Presentation only: the model's SQL is byte-identical, and the push
        // does not ask for the input box to be rewritten.
        assert_eq!(model().sql, sql);
        assert_eq!(state["replaceInput"], false);
        assert_eq!(state["model"]["tabs"]["sql"], sql);
    }

    #[test]
    fn the_display_state_defaults_to_off() {
        let (_host, _guard) = test_host();
        set_model(sample());
        let state = tab();
        assert_eq!(state["display"]["keywordWrap"], false);
        assert_eq!(state["display"]["vertical"], false);
        assert_eq!(display_prefs(), DisplayPrefs::default());
    }

    #[test]
    fn the_new_model_fields_round_trip_and_default_when_absent() {
        // Backward compatibility: an older push (or a stored model) without
        // `statements` / `statement` decodes with empty values, never an error.
        let decoded: WorkbenchModel =
            serde_json::from_str(r#"{"sql":"select 1","dialect":"MySQL"}"#)
                .expect("the older shape still decodes");
        assert!(decoded.statements.is_empty());
        assert!(decoded.statement.is_empty());

        let mut model = sample();
        model.statements = vec!["one".to_string(), "two".to_string()];
        model.statement = "two".to_string();
        let json = serde_json::to_string(&model).unwrap();
        let back: WorkbenchModel = serde_json::from_str(&json).unwrap();
        assert_eq!(back, model);
    }

    #[test]
    fn the_statement_picker_control_is_in_the_module() {
        // Lane L45: the picker lives in the meta row of `web/preview.js`, and
        // the two display toggles live in the "More" menu (where L44 also
        // adds items, so this only asserts L45's own ids are present).
        let module = include_str!("../../web/preview.js");
        for id in [
            "pvStatement",
            "pvStatementWrap",
            "pvKeywordWrap",
            "pvVertical",
        ] {
            assert!(module.contains(&format!("id=\"{id}\"")), "missing {id}");
        }
        assert!(
            module.contains("preview-statement"),
            "the picker posts its own op"
        );
        assert!(
            module.contains("preview-display"),
            "the switches report their state"
        );
        // The shaping is display-side: the model's SQL is what Copy reads, not
        // the shaped text.
        assert!(
            module.contains("export function wrapKeywords"),
            "the wrap helper"
        );
        assert!(
            module.contains("export function verticalItems"),
            "the vertical helper"
        );
        assert!(
            module.contains("text = shapeForDisplay(text, preview.display);"),
            "only the painted text is shaped"
        );
    }

    // Parameters, clipboard import and rollback (lane L27) ----------------

    #[test]
    fn rebinding_params_quotes_strings_and_leaves_numbers_bare() {
        let (_host, _guard) = test_host();
        set_model(WorkbenchModel::new(
            "select * from t where a = ? and b = ? and c = ? and d = ? and e = ?",
            Vec::new(),
            "MySQL",
        ));
        send(json!({
            "op": "preview-params",
            "values": ["1", "x'y", "true", "null", "-2.5"],
        }));
        let current = model();
        assert_eq!(
            current.sql,
            "select * from t where a = 1 and b = 'x''y' and c = true and d = NULL and e = -2.5"
        );
        assert_eq!(current.params.len(), 5, "the raw values stay editable");
        let tab = tab();
        assert_eq!(
            tab["replaceInput"], false,
            "a rebind leaves the input alone"
        );
        assert_eq!(tab["status"]["text"], t("view.workbench.status.rebound"));
    }

    #[test]
    fn parameter_literals_quote_only_strings() {
        assert_eq!(param_literal("1"), "1");
        assert_eq!(param_literal("-2.5"), "-2.5");
        assert_eq!(param_literal("+7"), "+7");
        assert_eq!(param_literal("true"), "true");
        assert_eq!(param_literal("NULL"), "NULL");
        assert_eq!(param_literal("abc"), "'abc'");
        assert_eq!(param_literal("x'y"), "'x''y'");
        assert_eq!(param_literal("  padded  "), "'padded'");
        assert_eq!(param_literal("1.2.3"), "'1.2.3'");
        assert_eq!(param_literal(""), "''");
        assert!(!is_number("1.2.3"));
        assert!(!is_number("12a"));
        assert!(is_number("0"));
    }

    #[test]
    fn rebinding_without_placeholders_reports_it() {
        let (_host, _guard) = test_host();
        set_model(WorkbenchModel::new("select 1", Vec::new(), "MySQL"));
        send(json!({ "op": "preview-params", "values": ["1"] }));
        assert_eq!(model().sql, "select 1");
        assert_eq!(
            tab()["status"]["text"],
            t("view.workbench.status.params_no_placeholder")
        );
    }

    #[test]
    fn rebinding_with_no_values_reports_it() {
        let (_host, _guard) = test_host();
        set_model(WorkbenchModel::new(
            "select * from t where id = ?",
            Vec::new(),
            "MySQL",
        ));
        send(json!({ "op": "preview-params", "values": [] }));
        assert_eq!(
            tab()["status"]["text"],
            t("view.workbench.status.params_empty")
        );
    }

    #[test]
    fn exporting_after_a_rebind_carries_the_bound_values() {
        let (host, _guard) = test_host();
        set_model(WorkbenchModel::new(
            "select * from t where id = ?",
            Vec::new(),
            "MySQL",
        ));
        send(json!({ "op": "preview-params", "values": ["42"] }));
        send(json!({ "op": "preview-export" }));
        let out = &host.output()[common::CHANNEL];
        assert!(
            compact(out).to_lowercase().contains("id = 42"),
            "the export is the executable SQL: {out}"
        );
    }

    #[test]
    fn rollback_refuses_without_primary_key_metadata() {
        let (_host, _guard) = test_host();
        set_model(WorkbenchModel::new(
            "DELETE FROM user WHERE id = 1",
            Vec::new(),
            "MySQL",
        ));
        send(json!({ "op": "preview-rollback" }));
        assert_eq!(
            tab()["status"]["text"],
            t("view.workbench.status.rollback_refused")
        );
    }

    #[test]
    fn the_formatted_tab_carries_the_dialect_formatted_sql() {
        let (_host, _guard) = test_host();
        set_model(WorkbenchModel::new(
            "select id from t where a=1",
            Vec::new(),
            "MySQL",
        ));
        let tab = tab();
        let formatted = tab["model"]["tabs"]["formatted"].as_str().unwrap();
        assert_ne!(formatted, "select id from t where a=1");
        assert!(formatted.to_uppercase().contains("SELECT"), "{formatted}");
    }

    // Lane L44: the "更多" menu's diff and share paths, and the refusal of the
    // one entry the renderer would have to provide.

    #[test]
    fn the_diff_op_opens_the_source_against_the_rendered_sql() {
        let (host, _guard) = test_host();
        let mut current = WorkbenchModel::new(
            "select id from user where id = ?",
            vec!["1".to_string()],
            "MySQL",
        );
        current.raw =
            "<mapper namespace=\"com.acme\"><select id=\"find\">select id from user where id = #{id}</select></mapper>"
                .to_string();
        current.source = "UserMapper.xml#find".to_string();
        current.confidence = "exact".to_string();
        set_model(current);
        send(json!({ "op": "preview-diff" }));
        let diffs = host.opened_diffs();
        assert_eq!(diffs.len(), 1, "one diff tab");
        let (diff, file_name) = &diffs[0];
        assert_eq!(file_name.as_deref(), Some("sql"), "SQL highlighting");
        assert!(
            diff.left
                .as_deref()
                .is_some_and(|left| left.contains("<mapper")),
            "the left side is the original source: {:?}",
            diff.left
        );
        assert!(
            diff.right
                .as_deref()
                .is_some_and(|right| right.contains("select id from user")),
            "the right side is the rendered SQL: {:?}",
            diff.right
        );
        let title = diff.title.as_deref().unwrap_or_default();
        assert!(title.contains("UserMapper.xml#find"), "{title}");
        assert_eq!(
            tab()["status"]["text"],
            t("view.workbench.status.diff_opened")
        );
        assert!(
            !host.output().contains_key(common::CHANNEL),
            "the diff tab is the surface; Output stays closed"
        );
    }

    #[test]
    fn the_diff_op_without_sql_reports_it_and_opens_nothing() {
        let (host, _guard) = test_host();
        send(json!({ "op": "preview-diff" }));
        assert!(host.opened_diffs().is_empty(), "no diff for an empty model");
        assert_eq!(
            tab()["status"]["text"],
            t("view.workbench.status.diff_empty")
        );
    }

    #[test]
    fn a_statement_only_model_diffs_against_its_own_sql() {
        // A command hands the statement over with no separate source text; the
        // diff still opens, with the statement on both sides.
        let (host, _guard) = test_host();
        let mut current = WorkbenchModel::new("select 1", Vec::new(), "MySQL");
        current.confidence = "exact".to_string();
        set_model(current);
        send(json!({ "op": "preview-diff" }));
        let diffs = host.opened_diffs();
        assert_eq!(diffs.len(), 1);
        let (diff, _) = &diffs[0];
        assert_eq!(diff.left.as_deref(), Some("select 1"));
        assert_eq!(diff.right.as_deref(), Some("select 1"));
        assert!(
            diff.title
                .as_deref()
                .is_some_and(|title| title.contains(&t("view.workbench.diff.inline"))),
            "an inline statement names itself: {:?}",
            diff.title
        );
    }

    #[test]
    fn the_share_op_copies_the_sql_and_stays_offline() {
        let (host, _guard) = test_host();
        set_model(WorkbenchModel::new(
            "select 2 from dual",
            Vec::new(),
            "MySQL",
        ));
        send(json!({ "op": "preview-share" }));
        assert_eq!(host.clipboard().as_deref(), Some("select 2 from dual"));
        assert_eq!(tab()["status"]["text"], t("view.workbench.status.shared"));
        assert_eq!(
            tab()["replaceInput"],
            false,
            "sharing leaves the input box alone"
        );
    }

    #[test]
    fn sharing_without_sql_reports_it_and_leaves_the_clipboard_alone() {
        let (host, _guard) = test_host();
        send(json!({ "op": "preview-share" }));
        assert!(host.clipboard().is_none());
        assert_eq!(
            tab()["status"]["text"],
            t("view.workbench.status.share_empty")
        );
    }

    #[test]
    fn the_qualify_op_refuses_because_the_renderer_owns_the_flag() {
        let (host, _guard) = test_host();
        set_model(WorkbenchModel::new(
            "select id from user",
            Vec::new(),
            "MySQL",
        ));
        send(json!({ "op": "preview-qualify" }));
        assert_eq!(
            tab()["status"]["text"],
            t("view.workbench.status.qualify_refused")
        );
        // The refusal never rewrites the SQL: nothing here guesses table names.
        assert_eq!(model().sql, "select id from user");
        assert!(host.opened_diffs().is_empty());
    }
}
