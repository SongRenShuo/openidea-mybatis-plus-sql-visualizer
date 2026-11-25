//! Preview workbench (T2-2).
//!
//! The workbench is a self-contained page: SQL, parameters, copy, export and a
//! dialect switch. XML rendering is wired in by the lanes that produce it
//! (L8/L9) through [`set_model`]; the page itself does no I/O, no network and
//! no local storage, and talks to the plugin only with `ViewEvent::Message`.
//!
//! Registered with `web = "web/workbench.html"`; [`push_model`] posts the model
//! to that page, and [`render_workbench`] produces the same content as an
//! inline `Body::Html` for hosts/tests that do not load the packaged page.

use std::sync::{Mutex, MutexGuard, OnceLock};

use openidea_plugin::views::{Body, Button, View, ViewEvent};
use openidea_plugin::{Error, host};
use serde::{Deserialize, Serialize};

use crate::core::{dialect, format};
use crate::i18n::t;

/// The id declared in `[[contributes.views]]`.
pub const VIEW_ID: &str = "sqlv-workbench";

/// What the workbench shows: one statement with its restored parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkbenchModel {
    pub sql: String,
    #[serde(default)]
    pub params: Vec<String>,
    pub dialect: String,
    /// `exact` / `partial` / `unresolved`; empty when the caller did not say.
    #[serde(default)]
    pub confidence: String,
    /// Where the statement came from (mapper path, log line, …), for the header.
    #[serde(default)]
    pub source: String,
}

impl Default for WorkbenchModel {
    fn default() -> Self {
        Self {
            sql: String::new(),
            params: Vec::new(),
            dialect: "MySQL".to_string(),
            confidence: String::new(),
            source: String::new(),
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

    pub fn has_sql(&self) -> bool {
        !self.sql.trim().is_empty()
    }

    /// All fifteen dialect names, in the order `core::dialect` lists them.
    pub fn dialect_options() -> Vec<String> {
        dialect::all()
            .iter()
            .map(|item| item.name.to_string())
            .collect()
    }

    /// Switches the dialect; returns false (and keeps the old one) when the
    /// name is unknown.
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
        match self.confidence.as_str() {
            "exact" => t("view.workbench.confidence.exact"),
            "partial" => t("view.workbench.confidence.partial"),
            "unresolved" => t("view.workbench.confidence.unresolved"),
            "" => String::new(),
            _ => t("view.workbench.confidence.unknown"),
        }
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
            fields.push((t("view.workbench.confidence.exact"), confidence));
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

static MODEL: OnceLock<Mutex<WorkbenchModel>> = OnceLock::new();

fn model_store() -> MutexGuard<'static, WorkbenchModel> {
    let mutex = MODEL.get_or_init(|| Mutex::new(WorkbenchModel::default()));
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Replaces what the workbench shows (called by the lanes that render).
pub fn set_model(model: WorkbenchModel) {
    *model_store() = model;
}

/// The current model.
pub fn model() -> WorkbenchModel {
    model_store().clone()
}

/// Empties the workbench.
pub fn clear() {
    *model_store() = WorkbenchModel::default();
}

/// Posts the current model to the packaged page (web view).
pub fn push_model() -> Result<(), Error> {
    let current = model();
    let confidence = current.confidence_label();
    let mut model_value =
        serde_json::to_value(&current).map_err(|error| Error::Invalid(error.to_string()))?;
    if let Some(object) = model_value.as_object_mut() {
        object.insert(
            "dialects".to_string(),
            serde_json::json!(WorkbenchModel::dialect_options()),
        );
    }
    let payload = serde_json::json!({
        "op": "load",
        "model": model_value,
        "labels": {
            "sql": t("view.workbench.sql"),
            "params": t("view.workbench.params"),
            "dialect": t("view.workbench.dialect"),
            "copy": t("view.workbench.copy"),
            "export": t("view.workbench.export"),
            "empty": t("view.workbench.empty"),
            "confidence": confidence,
        }
    });
    host::post_view_message(VIEW_ID, &payload.to_string())
}

/// Builds the inline `Body::Html` variant of the workbench.
pub fn render_workbench() -> View {
    let model = model();
    let mut view = View::new(Body::Html(build_html(&model)));
    view.toolbar
        .push(Button::new("copy", &t("view.workbench.copy")));
    view.toolbar
        .push(Button::new("export", &t("view.workbench.export")));
    view.status = Some(model.dialect.clone());
    view
}

/// Toolbar buttons and messages posted by the page.
pub fn handle_workbench_event(event: ViewEvent) -> Result<(), Error> {
    match event {
        ViewEvent::Button(id) if id == "copy" => {
            let text = model().copy_text();
            host::copy_to_clipboard(&text)?;
        }
        ViewEvent::Button(id) if id == "export" => {
            let text = model().export_text();
            host::copy_to_clipboard(&text)?;
        }
        ViewEvent::Message(text) => handle_message(&text)?,
        _ => {}
    }
    Ok(())
}

fn handle_message(text: &str) -> Result<(), Error> {
    let message: serde_json::Value =
        serde_json::from_str(text).map_err(|error| Error::Invalid(error.to_string()))?;
    match message
        .get("op")
        .and_then(|value| value.as_str())
        .unwrap_or("")
    {
        // The packaged page asks for the model once its script is ready.
        "ready" => push_model()?,
        "copy" => {
            let text = model().copy_text();
            host::copy_to_clipboard(&text)?;
        }
        "export" => {
            let text = model().export_text();
            host::copy_to_clipboard(&text)?;
        }
        "dialect" => {
            let Some(name) = message.get("value").and_then(|value| value.as_str()) else {
                return Ok(());
            };
            let mut current = model();
            if current.with_dialect(name) {
                set_model(current);
                push_model()?;
                host::refresh_view(VIEW_ID);
            }
        }
        _ => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Page
// ---------------------------------------------------------------------------

const STYLE: &str = "\
body{font:var(--openidea-font-size,13px)/1.5 var(--openidea-font-ui,sans-serif);\
color:var(--openidea-fg);margin:0;padding:8px}\
.bar{display:flex;gap:8px;align-items:center;margin-bottom:8px;flex-wrap:wrap}\
select,button{background:var(--openidea-bg-input);color:var(--openidea-fg);\
border:1px solid var(--openidea-border);border-radius:3px;padding:2px 6px}\
button{cursor:pointer}\
label{font-weight:600;display:block;margin:8px 0 4px}\
pre{margin:0;padding:8px;border:1px solid var(--openidea-border);border-radius:4px;\
background:var(--openidea-bg-elevated);white-space:pre-wrap;word-break:break-word;\
font-family:var(--openidea-font-mono,monospace)}\
.badge{padding:0 6px;border-radius:3px;background:var(--openidea-bg-selected);font-size:11px}\
.badge.partial{color:var(--openidea-warning)}\
.badge.unresolved{color:var(--openidea-danger)}\
.muted{color:var(--openidea-fg-muted)}\
.notice{color:var(--openidea-success);margin-top:8px}";

fn build_html(model: &WorkbenchModel) -> String {
    let mut html = String::with_capacity(2048);
    html.push_str("<!DOCTYPE html><html><head><meta charset=\"utf-8\"><style>");
    html.push_str(STYLE);
    html.push_str("</style></head><body>");

    html.push_str("<div class=\"bar\"><label for=\"dialect\">");
    html.push_str(&escape_html(&t("view.workbench.dialect")));
    html.push_str("</label><select id=\"dialect\">");
    for name in WorkbenchModel::dialect_options() {
        html.push_str(&format!(
            "<option value=\"{}\"{}>{}</option>",
            escape_attr(&name),
            if name == model.dialect {
                " selected"
            } else {
                ""
            },
            escape_html(&name)
        ));
    }
    html.push_str("</select>");
    html.push_str(&format!(
        "<button id=\"copy\" type=\"button\">{}</button>",
        escape_html(&t("view.workbench.copy"))
    ));
    html.push_str(&format!(
        "<button id=\"export\" type=\"button\">{}</button>",
        escape_html(&t("view.workbench.export"))
    ));
    let confidence = model.confidence_label();
    if !confidence.is_empty() {
        html.push_str(&format!(
            "<span class=\"badge {}\">{}</span>",
            escape_attr(&model.confidence),
            escape_html(&confidence)
        ));
    }
    html.push_str("</div>");

    if model.has_sql() {
        html.push_str(&format!(
            "<label>{}</label><pre id=\"sql\">{}</pre>",
            escape_html(&t("view.workbench.sql")),
            escape_html(&model.sql)
        ));
        html.push_str(&format!(
            "<label>{}</label><pre id=\"params\" class=\"muted\">{}</pre>",
            escape_html(&t("view.workbench.params")),
            escape_html(&model.params_text())
        ));
    } else {
        html.push_str(&format!(
            "<p class=\"muted\">{}</p>",
            escape_html(&t("view.workbench.empty"))
        ));
    }

    html.push_str("<p id=\"notice\" class=\"notice\" hidden></p>");
    html.push_str("<script>var MODEL=");
    html.push_str(&json_for_script(model));
    html.push_str(";");
    html.push_str(
        "(function(){\
function post(message){if(window.openidea&&window.openidea.postMessage){\
window.openidea.postMessage(JSON.stringify(message));}}\
var copy=document.getElementById('copy');\
var exportButton=document.getElementById('export');\
var select=document.getElementById('dialect');\
var notice=document.getElementById('notice');\
function flash(text){if(notice){notice.textContent=text;notice.hidden=false;\
setTimeout(function(){notice.hidden=true;},1500);}}\
if(copy){copy.addEventListener('click',function(){post({op:'copy'});});}\
if(exportButton){exportButton.addEventListener('click',function(){post({op:'export'});});}\
if(select){select.addEventListener('change',function(){\
post({op:'dialect',value:select.value});});}\
var params=document.getElementById('params');\
if(params&&MODEL.params){params.textContent=MODEL.params.join(', ');}\
function apply(model){\
if(!model){return;}\
var sqlNode=document.getElementById('sql');\
if(sqlNode){sqlNode.textContent=model.sql||'';}\
if(params){params.textContent=(model.params||[]).join(', ');}\
if(select&&model.dialect){select.value=model.dialect;}\
}\
if(window.openidea&&window.openidea.onMessage){\
window.openidea.onMessage(function(raw){\
try{var message=JSON.parse(raw);\
if(message.op==='load'){apply(message.model);}\
if(message.op==='copied'){flash('');}\
}catch(error){}\
});}\
post({op:'ready'});\
})();</script></body></html>",
    );
    html
}

fn json_for_script(model: &WorkbenchModel) -> String {
    let json = serde_json::to_string(model).unwrap_or_else(|_| "null".to_string());
    json.replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn escape_attr(text: &str) -> String {
    escape_html(text)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn test_host() -> (MockHost, MutexGuard<'static, ()>) {
        let guard = TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let host = MockHost::install();
        clear();
        (host, guard)
    }

    fn sample() -> WorkbenchModel {
        WorkbenchModel {
            sql: "SELECT id, name FROM \"user\" WHERE id = 1".to_string(),
            params: vec!["1".to_string()],
            dialect: "MySQL".to_string(),
            confidence: "exact".to_string(),
            source: "UserMapper.xml#findById".to_string(),
        }
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
    fn inline_html_escapes_content_and_has_no_external_links() {
        let (_host, _guard) = test_host();
        let mut model = sample();
        model.sql = "SELECT '<script>' FROM t".to_string();
        set_model(model);
        let view = render_workbench();
        let Body::Html(html) = &view.body else {
            panic!("expected html, got {:?}", view.body)
        };
        assert!(html.contains("&lt;script&gt;"), "SQL is html-escaped");
        for needle in ["http://", "https://", "localStorage", "fetch("] {
            assert!(
                !html.contains(needle),
                "inline page must not contain {needle}"
            );
        }
    }

    #[test]
    fn the_packaged_page_has_no_external_links_or_storage() {
        let page = include_str!("../../web/workbench.html");
        assert!(
            page.contains("openidea.postMessage"),
            "the page talks to the plugin only"
        );
        for needle in [
            "http://",
            "https://",
            "localStorage",
            "fetch(",
            "XMLHttpRequest",
        ] {
            assert!(!page.contains(needle), "web page must not contain {needle}");
        }
    }

    #[test]
    fn the_copy_message_reaches_the_clipboard() {
        let (host, _guard) = test_host();
        set_model(sample());
        handle_workbench_event(ViewEvent::Message("{\"op\":\"copy\"}".to_string())).unwrap();
        assert_eq!(host.clipboard().as_deref(), Some(sample().sql.as_str()));
    }

    #[test]
    fn a_ready_message_pushes_the_model_to_the_page() {
        let (host, _guard) = test_host();
        set_model(sample());
        handle_workbench_event(ViewEvent::Message("{\"op\":\"ready\"}".to_string())).unwrap();
        let messages = host.view_messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].0, VIEW_ID);
        assert!(messages[0].1.contains("\"op\":\"load\""));
        assert!(messages[0].1.contains("UserMapper.xml#findById"));
    }

    #[test]
    fn a_dialect_message_switches_and_refreshes() {
        let (host, _guard) = test_host();
        set_model(sample());
        handle_workbench_event(ViewEvent::Message(
            "{\"op\":\"dialect\",\"value\":\"PostgreSQL\"}".to_string(),
        ))
        .unwrap();
        assert_eq!(model().dialect, "PostgreSQL");
        assert_eq!(host.refreshed_views(), vec![VIEW_ID.to_string()]);
        assert_eq!(host.view_messages().len(), 1);
    }

    #[test]
    fn buttons_copy_and_export() {
        let (host, _guard) = test_host();
        set_model(sample());
        handle_workbench_event(ViewEvent::Button("copy".to_string())).unwrap();
        assert_eq!(host.clipboard().as_deref(), Some(sample().sql.as_str()));

        handle_workbench_event(ViewEvent::Button("export".to_string())).unwrap();
        let expected = sample().export_text();
        assert_eq!(host.clipboard().as_deref(), Some(expected.as_str()));
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
    fn main_dispatches_the_workbench_view() {
        use openidea_plugin::views::Views;
        let (host, _guard) = test_host();
        set_model(sample());
        let mut main = crate::Main;
        let view = main.render(VIEW_ID, None, &host.invocation()).unwrap();
        assert!(matches!(view.body, Body::Html(_)));
        main.handle(
            VIEW_ID,
            ViewEvent::Button("copy".to_string()),
            &host.invocation(),
        )
        .unwrap();
        assert!(host.clipboard().is_some());
        assert!(matches!(
            main.render("no-such-view", None, &host.invocation()),
            Err(Error::NotFound(_))
        ));
    }
}
