//! Preview workbench (T2-2), now the fifth tab of the SQL log panel (L23).
//!
//! The workbench mirrors the IntelliJ edition's "SQL preview" panel
//! (`SqlStandalonePreviewPanel` + `SqlPreviewPanel`): an input pane on top, a
//! preview pane below, a parameter column beside the SQL column, and the tabs
//! SQL / 原文 / 列信息. In the IntelliJ edition that panel is the fifth
//! (closable) tab of the SQL Log tool window, not a tool window of its own, so
//! this build renders it inside `web/log-panel.html` (`#panelPreview`).
//!
//! This module therefore owns the model and the op handling only; it posts no
//! view message any more. The log panel carries [`preview_tab`] in its state
//! (`tabs.preview`) and routes `preview-*` messages here, so both tabs share
//! one push channel and one page.
//!
//! Two facts of the OpenIDEA platform shape this panel:
//!
//! - the dialect comes from the settings page, so the panel shows a read-only
//!   label (the IntelliJ panel has no dialect picker either);
//! - the plugin never executes user SQL, so "Execute SQL" stays disabled with
//!   the reason in its tooltip.

use std::sync::{Mutex, MutexGuard, OnceLock};

use openidea_plugin::{Error, PickItem, host};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::core::perf::{self, Severity};
use crate::core::xml::{Confidence, MapperSet, RenderOutput};
use crate::core::{dialect, format};
use crate::i18n::t;

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

/// How many project files one "load from file" pick lists.
const PICK_LIMIT: u32 = 200;

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

    /// What the Copy button puts on the clipboard: the executable SQL.
    pub fn copy_text(&self) -> String {
        self.sql.clone()
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
/// previewed (the reference panel does the same and asks the user to split the
/// fragment to see the others).
fn render_mapper(text: &str, dialect_name: &str) -> WorkbenchModel {
    let mut model = WorkbenchModel::new("", Vec::new(), dialect_name);
    model.raw = text.to_string();
    model.kind = KIND_XML.to_string();

    let (set, issues) = MapperSet::load(&[(FRAGMENT_DOC, text)]);
    let mut reasons: Vec<String> = issues
        .iter()
        .map(|issue| format!("[{}] {}: {}", issue.message_key, issue.path, issue.detail))
        .collect();

    let ids = set.statement_ids();
    let Some(id) = ids.first() else {
        model.confidence = "unresolved".to_string();
        reasons.push(format!("[{}]", crate::core::xml::KEY_STATEMENT_NOT_FOUND));
        model.reason = reasons.join("\n");
        return model;
    };

    let out = set.render(id, &json!({}));
    model.confidence = confidence_str(out.confidence).to_string();
    model.source = id.clone();
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

/// What the preview tab keeps between state pushes.
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
}

impl Default for PreviewStore {
    fn default() -> Self {
        Self {
            model: WorkbenchModel::default(),
            status: String::new(),
            replace_input: false,
            select_pending: false,
        }
    }
}

static PREVIEW: OnceLock<Mutex<PreviewStore>> = OnceLock::new();

fn preview_store() -> MutexGuard<'static, PreviewStore> {
    let mutex = PREVIEW.get_or_init(|| Mutex::new(PreviewStore::default()));
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Replaces what the preview tab shows (called by the lanes that render).
pub fn set_model(model: WorkbenchModel) {
    preview_store().model = model;
}

/// The current model.
pub fn model() -> WorkbenchModel {
    preview_store().model.clone()
}

/// Empties the preview tab.
pub fn clear() {
    preview_store().model = WorkbenchModel::default();
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
    preview_store().replace_input = true;
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
            "advice": &current.advice,
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
    })
}

/// The `selectTab` value for the next push, while the page still has to open
/// the preview tab (`None` once it reported its tab).
pub fn select_tab() -> Option<&'static str> {
    preview_store().select_pending.then_some("preview")
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
        "load-file" => load_file_state()?,
        "clear" => {
            clear();
            set_status(t("view.workbench.status.cleared"), true);
        }
        "copy" => copy_sql()?,
        "export" => export_sql()?,
        // The dialect is read-only: it comes from the settings page. The op is
        // still answered so an older page does not wait for a reply.
        "dialect" => set_status(t("view.workbench.status.dialect_readonly"), false),
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
    set_model(next);
    set_status(status, true);
}

fn format_input_state(input: &str) {
    let (dialect_name, _) = configured_dialect();
    if detect_kind(input) == KIND_XML {
        let mut next = render_input(input, KIND_XML, &dialect_name);
        next.dialect = dialect_name;
        set_model(next);
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
    set_model(next);
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
pub(crate) fn test_store_lock() -> std::sync::MutexGuard<'static, ()> {
    super::test_state_lock()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::common;
    use openidea_plugin::mock::MockHost;

    fn test_host() -> (MockHost, MutexGuard<'static, ()>) {
        let guard = test_store_lock();
        let host = MockHost::install();
        clear();
        note_page_tab();
        // A fresh store: the status and the one-shot flag from another test
        // must not leak into this one.
        let mut store = preview_store();
        store.status.clear();
        store.replace_input = false;
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
        let model = sample();
        assert_eq!(model.copy_text(), model.sql);
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
        assert!(host.output().get(common::CHANNEL).is_none());
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
    fn an_unknown_op_and_an_unprefixed_op_change_nothing() {
        let (_host, _guard) = test_host();
        set_model(sample());
        send(json!({ "op": "preview-nope" }));
        assert_eq!(model(), sample());
        // The bare `copy` op is the log panel's own and must not reach here.
        send(json!({ "op": "copy" }));
        assert_eq!(model(), sample());
    }
}
