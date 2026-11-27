//! `preview-sql`: send a statement to the SQL log panel's preview tab.
//!
//! The IntelliJ edition opens `SqlPreviewDialog`, a modal dialog, from the
//! mapper gutter icon and from `Alt+Shift+Q`. OpenIDEA has no plugin-facing
//! modal dialog, so both entries land on the surface this plugin already has:
//! the fifth tab of the `sqlv-logs` tool window (`sqlv-logs#preview`). The
//! render is the same — `core::xml` for mapper statements, the workbench's own
//! input path for SQL text — only the shell differs.
//!
//! ## Argument matrix
//!
//! | argument | what is previewed |
//! | --- | --- |
//! | `<mapper.xml>#<statement>` | that statement (bare id or `namespace.id`) |
//! | `<mapper.xml>` / `@<mapper.xml>` | every statement; the first one in the tab |
//! | `@<path>` / a readable bare path | the file: mapper XML renders, anything else is SQL text |
//! | inline text | that text (XML is rendered, SQL is used verbatim) |
//! | none | the focused editor: the first non-empty selection, else the document |
//!
//! `editor.read` is optional. Without it the no-argument form reports
//! [`KEY_EDITOR_UNREADABLE`]; the argument forms keep working, which is the
//! documented degradation (a user who refuses the capability can still run the
//! command from the gutter or with a path).

use std::fmt::Write as _;

use openidea_plugin::{Error, Invocation, host};
use serde_json::json;

use crate::capture::bridge;
use crate::commands::common::{self, CliError};
use crate::core::xml::{Confidence, MapperSet, RenderOutput};
use crate::views::{log_panel, workbench};

/// Matches `[[contributes.commands]] id` in `plugin.toml`.
pub const ID: &str = "preview-sql";

/// What the command accepts as `arg` (also the text of a missing-argument error).
pub const USAGE: &str = "<mapper.xml>#<statement> | <mapper.xml> | @<path> | <SQL text> | (no argument: current editor)";

/// Same key `render-xml` reports, so the two commands describe an empty mapper
/// the same way.
pub const KEY_NO_STATEMENTS: &str = "render_xml.no_statements";

/// Nothing was handed over and no editor is focused (or the editor is empty).
pub const KEY_NO_INPUT: &str = "preview.error.no_input";

/// The focused editor exists but its text cannot be read (`editor.read` missing,
/// or the document changed between the two host calls).
pub const KEY_EDITOR_UNREADABLE: &str = "preview.error.editor_unreadable";

/// Used when the settings page has no dialect; the workbench does the same.
const FALLBACK_DIALECT: &str = "MySQL";

/// Dispatch entry.
pub fn run(ctx: &Invocation, arg: Option<&str>) -> Result<Option<String>, Error> {
    match preview(ctx, arg) {
        // The preview tab is the surface: success must not pop the Output tab
        // open on every gutter click or shortcut press. Failures still go to
        // Output (`common::fail`), because the reason is the content there.
        Ok(_) => Ok(None),
        Err(err) => common::fail(err),
    }
}

/// Resolve the input and render it into the workbench model.
///
/// Returns the model, a one-line description (kept for `preview-sql-diff`'s
/// diagnostics and tests), and the raw input the diff view shows on its left
/// side (`preview-sql-diff`).
pub fn model_for(
    ctx: &Invocation,
    arg: Option<&str>,
) -> Result<(workbench::WorkbenchModel, String, String), CliError> {
    let dialect = dialect_name();
    match resolve(ctx, arg)? {
        Input::Mapper {
            path,
            xml,
            statement,
        } => {
            let (model, rendered) = mapper_model(&path, &xml, statement.as_deref(), &dialect)?;
            let detail = format!(
                "{path}#{} (statements: {}, confidence: {}, sql: {} chars)",
                model.source,
                rendered.len(),
                model.confidence,
                model.sql.chars().count()
            );
            Ok((model, detail, xml))
        }
        Input::Text { text, source } => {
            let mut model = workbench::render_input(&text, "", &dialect);
            if let Some(source) = &source {
                model.source = source.clone();
            }
            let detail = format!(
                "{} (kind: {}, confidence: {}, sql: {} chars)",
                source.as_deref().unwrap_or("<inline>"),
                if model.kind.is_empty() {
                    "none"
                } else {
                    model.kind.as_str()
                },
                if model.confidence.is_empty() {
                    "none"
                } else {
                    model.confidence.as_str()
                },
                model.sql.chars().count()
            );
            Ok((model, detail, text))
        }
    }
}

/// Resolve the input, render it into the workbench model, push the log panel's
/// state with `selectTab = "preview"` and reveal the tool window.
fn preview(ctx: &Invocation, arg: Option<&str>) -> Result<String, CliError> {
    let (model, detail, raw) = model_for(ctx, arg)?;

    // The same handshake as `open-workbench`: the page asks for the state again
    // with `{op:"ready"}` once its script is ready, so the plugin-side model has
    // to be set as well as the push below.
    // Lane L45: the statement picker switches by id, so the host keeps the
    // mapper document when that is what was rendered. `model_for` returns the
    // raw input as the third value, which is the mapper XML for a mapper render
    // and the SQL text otherwise.
    let is_mapper = model.kind == workbench::KIND_XML;
    workbench::set_model(model);
    workbench::set_mapper_source(is_mapper.then_some(raw));
    workbench::request_tab();
    workbench::request_input_replace();
    log_panel::push_state()
        .map_err(|error| CliError::new(bridge::KEY_VIEW_UNAVAILABLE, error.to_string()))?;
    host::reveal_view(log_panel::VIEW_ID)
        .map_err(|error| CliError::new(bridge::KEY_VIEW_UNAVAILABLE, error.to_string()))?;

    let mut out = String::new();
    let _ = writeln!(
        out,
        "-- preview-sql: {detail} -> {}#preview",
        log_panel::VIEW_ID
    );
    Ok(out)
}

/// The dialect the settings page declares, or [`FALLBACK_DIALECT`].
fn dialect_name() -> String {
    let configured = common::configured_dialect();
    if configured.trim().is_empty() {
        FALLBACK_DIALECT.to_string()
    } else {
        configured
    }
}

/// What the command resolved its input to.
enum Input {
    /// A mapper document (file or editor buffer) rendered through `core::xml`.
    Mapper {
        path: String,
        xml: String,
        /// `None` renders every statement (the first one in the tab).
        statement: Option<String>,
    },
    /// Free text: the workbench decides whether it is SQL or mapper XML.
    Text {
        text: String,
        source: Option<String>,
    },
}

fn resolve(ctx: &Invocation, arg: Option<&str>) -> Result<Input, CliError> {
    match arg.map(str::trim).filter(|text| !text.is_empty()) {
        Some(raw) => resolve_argument(ctx, raw),
        None => editor_input(ctx),
    }
}

fn resolve_argument(ctx: &Invocation, raw: &str) -> Result<Input, CliError> {
    if let Some((path, statement)) = split_statement_arg(raw) {
        let xml = common::read_file(ctx, &path)?;
        return Ok(Input::Mapper {
            path,
            xml,
            statement: Some(statement),
        });
    }
    // `@<path>` / bare path / inline text, with the same probe rules as
    // `open-workbench`: only an argument free of whitespace is tried as a file,
    // and a failed read falls back to literal text.
    let (text, source) = common::sql_arg(ctx, Some(raw))?;
    if let Some(path) = &source
        && looks_like_mapper(&text)
    {
        return Ok(Input::Mapper {
            path: path.clone(),
            xml: text,
            statement: None,
        });
    }
    Ok(Input::Text { text, source })
}

/// `path#statement` for a mapper file. Inline SQL keeps its `#`: the prefix of
/// `… WHERE id = #{id}` contains whitespace, so it is never taken for a path.
fn split_statement_arg(arg: &str) -> Option<(String, String)> {
    let (path, statement) = arg.rsplit_once('#')?;
    let path = path.trim();
    let path = path.strip_prefix('@').unwrap_or(path);
    let statement = statement.trim();
    if path.is_empty() || statement.is_empty() {
        return None;
    }
    if path.chars().any(char::is_whitespace) {
        return None;
    }
    // Only an explicit mapper path is treated as `path#statement`; a bare
    // `/tmp/notes#1` stays literal text instead of becoming a read error.
    if !path.to_ascii_lowercase().ends_with(".xml") {
        return None;
    }
    Some((path.to_string(), statement.to_string()))
}

/// True when the text is a mapper document (`<mapper …>` root).
fn looks_like_mapper(text: &str) -> bool {
    text.contains("<mapper")
}

/// The no-argument form: the focused editor's selection or whole document.
fn editor_input(ctx: &Invocation) -> Result<Input, CliError> {
    let editor = ctx.editor().ok_or_else(|| {
        CliError::new(
            KEY_NO_INPUT,
            format!("no editor is focused and no argument was given; usage: {USAGE}"),
        )
    })?;
    let path = editor.path();
    let document = editor
        .document()
        .map_err(|error| editor_error(&path, &error))?;
    let selections = editor
        .selections()
        .map_err(|error| editor_error(&path, &error))?;
    let selected = selections
        .iter()
        .filter_map(|range| document.text_in(*range))
        .find(|text| !text.trim().is_empty())
        .map(str::to_string);
    if let Some(text) = selected {
        return Ok(Input::Text {
            text,
            source: Some(path),
        });
    }
    if document.text.trim().is_empty() {
        return Err(CliError::new(
            KEY_NO_INPUT,
            format!("{path}: the editor is empty; usage: {USAGE}"),
        ));
    }
    // A whole mapper document goes through `core::xml` (all statements), which
    // resolves includes against the real file path; a selection is always text,
    // because the fragment may sit outside its `<sql>` definitions.
    if looks_like_mapper(&document.text) && crate::providers::is_xml(&path) {
        return Ok(Input::Mapper {
            path,
            xml: document.text,
            statement: None,
        });
    }
    Ok(Input::Text {
        text: document.text,
        source: Some(path),
    })
}

fn editor_error(path: &str, error: &Error) -> CliError {
    CliError::new(
        KEY_EDITOR_UNREADABLE,
        format!("{path}: {error}; pass the SQL or a mapper path as the argument instead"),
    )
}

/// One rendered statement: its id and the confidence word the summary prints.
type RenderedStatement = (String, &'static str);

/// The workbench model to show plus every rendered statement and its
/// confidence.
type MapperRender = (workbench::WorkbenchModel, Vec<RenderedStatement>);

/// Render one mapper statement (or every statement) into the workbench model.
///
/// The model carries the first statement; the returned list is every statement
/// with its confidence, so the summary line can say how many were rendered.
/// Lane L45: the model also carries every statement id (`statements`) and the
/// chosen one (`statement`), so the preview page can switch between them without
/// the host shipping the file again.
fn mapper_model(
    path: &str,
    xml: &str,
    statement: Option<&str>,
    dialect: &str,
) -> Result<MapperRender, CliError> {
    let (set, issues) = MapperSet::load(&[(path, xml)]);
    let all_ids = unique_statement_ids(&set);
    let ids = match statement {
        Some(id) => vec![id.to_string()],
        None => all_ids.clone(),
    };
    if ids.is_empty() {
        return Err(CliError::new(
            KEY_NO_STATEMENTS,
            format!("{path}: no <select>/<insert>/<update>/<delete> statement found"),
        ));
    }

    let rendered: Vec<(String, RenderOutput)> = ids
        .into_iter()
        .map(|id| {
            let out = set.render(&id, &json!({}));
            (id, out)
        })
        .collect();
    let (first_id, out) = &rendered[0];

    // Reuse the workbench's own plain-SQL path for the model shell (advice and
    // the formatted tab), then put the mapper fields back on top.
    let mut model = workbench::render_input(out.sql.trim_end(), workbench::KIND_SQL, dialect);
    model.kind = workbench::KIND_XML.to_string();
    model.source = first_id.clone();
    model.statement = first_id.clone();
    // Lane L45: the picker's options, computed from the whole file even when
    // one statement was named explicitly. An explicit bare id (`path#findById`)
    // must still be the one the picker shows selected, so map it onto the
    // qualified entry the list carries when that is what the file has.
    model.statements = all_ids.clone();
    if let Some(current) = all_ids
        .iter()
        .find(|known| *known == first_id || known.ends_with(&format!(".{first_id}")))
    {
        model.statement = current.clone();
        model.source = current.clone();
    }
    model.confidence = confidence_str(out.confidence).to_string();
    model.params = out.params.iter().map(param_text).collect();
    // The whole file is the "原文" for a file-level preview; for one statement
    // the page falls back to the rendered SQL, so re-rendering the input does
    // not silently switch to a different statement.
    if statement.is_none() {
        model.raw = xml.to_string();
    }
    if out.confidence == Confidence::Unresolved {
        // `unresolved` never carries SQL: only the reason goes to the page.
        model.sql.clear();
        model.params.clear();
        model.advice.clear();
    }
    let mut reasons: Vec<String> = issues
        .iter()
        .map(|issue| format!("[{}] {}: {}", issue.message_key, issue.path, issue.detail))
        .collect();
    reasons.extend(reason_lines(out));
    model.reason = reasons.join("\n");

    let summary = rendered
        .iter()
        .map(|(id, out)| (id.clone(), confidence_str(out.confidence)))
        .collect();
    Ok((model, summary))
}

/// Same rule as `commands::render_xml::unique_statement_ids` (kept local
/// because this lane may only add files): keep the qualified form when a bare
/// id is shadowed by it, and never list one statement twice.
fn unique_statement_ids(set: &MapperSet) -> Vec<String> {
    let ids = set.statement_ids();
    let mut out: Vec<String> = Vec::new();
    for (index, id) in ids.iter().enumerate() {
        let shadowed = ids.iter().enumerate().any(|(other_index, other)| {
            other_index != index
                && other.len() > id.len() + 1
                && other.ends_with(id.as_str())
                && other.as_bytes()[other.len() - id.len() - 1] == b'.'
        });
        if !shadowed && !out.contains(id) {
            out.push(id.clone());
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

/// A parameter value as the page shows it: strings bare, everything else as JSON.
fn param_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    use serde_json::Value;

    const MAPPER: &str = r#"<mapper namespace="com.example.UserMapper">
    <sql id="cols">id, name</sql>
    <select id="findById">SELECT <include refid="cols"/> FROM users WHERE id = #{id}</select>
    <update id="touch">UPDATE users SET name = #{name} WHERE id = #{id}</update>
</mapper>"#;

    /// Restores the process-wide workbench store, which the view tests share.
    fn with_model_restore<T>(body: impl FnOnce() -> T) -> T {
        let _guard = workbench::test_store_lock();
        let previous = workbench::model();
        let out = body();
        workbench::set_model(previous);
        workbench::note_page_tab();
        out
    }

    fn preview_state(host: &MockHost) -> Value {
        let messages = host.view_messages();
        assert_eq!(messages.len(), 1, "{messages:?}");
        assert_eq!(messages[0].0, log_panel::VIEW_ID);
        serde_json::from_str(&messages[0].1).expect("valid JSON")
    }

    fn model_of(state: &Value) -> Value {
        state["tabs"]["preview"]["model"].clone()
    }

    /// XML text nodes leave runs of whitespace in the rendered SQL; assertions
    /// compare the whitespace-normalised form (same rule as `render_xml`).
    fn compact(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn commands_preview_sql_renders_the_requested_statement() {
        with_model_restore(|| {
            let host = MockHost::install();
            host.add_file("/project/UserMapper.xml", MAPPER);
            run(
                &host.invocation(),
                Some("/project/UserMapper.xml#com.example.UserMapper.touch"),
            )
            .unwrap();

            let state = preview_state(&host);
            assert_eq!(state["selectTab"], "preview");
            let model = model_of(&state);
            assert_eq!(model["source"], "com.example.UserMapper.touch");
            assert_eq!(model["confidence"], "exact");
            assert_eq!(model["kind"], "xml");
            assert!(
                model["sql"]
                    .as_str()
                    .is_some_and(|sql| sql.contains("UPDATE users SET name = ?"))
            );
            // A successful preview leaves the Output tab alone; the tab is the
            // surface. (Errors still go there, tested below.)
            assert!(
                !host.output().contains_key(common::CHANNEL),
                "{:?}",
                host.output().get(common::CHANNEL)
            );
            assert_eq!(host.revealed_views(), [log_panel::VIEW_ID.to_string()]);
        });
    }

    #[test]
    fn commands_preview_sql_bare_mapper_path_renders_every_statement() {
        with_model_restore(|| {
            let host = MockHost::install();
            host.add_file("/project/UserMapper.xml", MAPPER);
            run(&host.invocation(), Some("/project/UserMapper.xml")).unwrap();

            let state = preview_state(&host);
            let model = model_of(&state);
            // The first statement is what the tab shows; the summary counts all.
            assert_eq!(model["source"], "com.example.UserMapper.findById");
            assert!(
                model["sql"]
                    .as_str()
                    .is_some_and(|sql| compact(sql).contains("SELECT id, name FROM users")),
                "{model}"
            );
            assert!(
                model["tabs"]["raw"]
                    .as_str()
                    .is_some_and(|raw| raw.contains("<mapper")),
                "the input box holds the whole mapper file"
            );
            assert!(!host.output().contains_key(common::CHANNEL));
        });
    }

    #[test]
    fn commands_preview_sql_accepts_an_at_path_to_sql() {
        with_model_restore(|| {
            let host = MockHost::install();
            host.add_file("/project/pick.sql", "SELECT id FROM users WHERE id = 1\n");
            run(&host.invocation(), Some("@/project/pick.sql")).unwrap();

            let model = model_of(&preview_state(&host));
            assert_eq!(model["sql"], "SELECT id FROM users WHERE id = 1");
            assert_eq!(model["source"], "/project/pick.sql");
            assert_eq!(model["kind"], "sql");
            assert_eq!(model["confidence"], "exact");
        });
    }

    #[test]
    fn commands_preview_sql_takes_inline_sql_verbatim() {
        with_model_restore(|| {
            let host = MockHost::install();
            run(&host.invocation(), Some("SELECT * FROM users WHERE id = 1")).unwrap();

            let model = model_of(&preview_state(&host));
            assert_eq!(model["sql"], "SELECT * FROM users WHERE id = 1");
            assert_eq!(model["kind"], "sql");
        });
    }

    #[test]
    fn commands_preview_sql_keeps_a_hash_in_inline_sql() {
        with_model_restore(|| {
            let host = MockHost::install();
            run(
                &host.invocation(),
                Some("SELECT * FROM users WHERE id = #{id}"),
            )
            .unwrap();

            let model = model_of(&preview_state(&host));
            assert_eq!(model["sql"], "SELECT * FROM users WHERE id = #{id}");
        });
    }

    #[test]
    fn commands_preview_sql_unresolved_prints_the_reason_and_no_sql() {
        with_model_restore(|| {
            let host = MockHost::install();
            host.add_file(
                "/project/Cycle.xml",
                r#"<mapper namespace="a">
                    <sql id="one"><include refid="two"/></sql>
                    <sql id="two"><include refid="one"/></sql>
                    <select id="s">SELECT <include refid="one"/> FROM t</select>
                </mapper>"#,
            );
            run(&host.invocation(), Some("/project/Cycle.xml#a.s")).unwrap();

            let model = model_of(&preview_state(&host));
            assert_eq!(model["confidence"], "unresolved");
            assert_eq!(model["sql"], "");
            assert!(
                model["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("xml.include.cycle"))
            );
        });
    }

    #[test]
    fn commands_preview_sql_without_an_argument_uses_the_editor_selection() {
        with_model_restore(|| {
            let host = MockHost::install();
            host.open_editor(
                "/project/notes.txt",
                "text",
                "hello\nSELECT 1\nworld",
                &[(1, 0, 1, 8)],
            );
            run(&host.invocation(), None).unwrap();

            let model = model_of(&preview_state(&host));
            assert_eq!(model["sql"], "SELECT 1");
            assert_eq!(model["source"], "/project/notes.txt");
        });
    }

    #[test]
    fn commands_preview_sql_without_an_argument_uses_the_whole_document() {
        with_model_restore(|| {
            let host = MockHost::install();
            host.open_editor(
                "/project/notes.sql",
                "sql",
                "SELECT * FROM users\n",
                &[(0, 0, 0, 0)],
            );
            run(&host.invocation(), None).unwrap();

            let model = model_of(&preview_state(&host));
            assert_eq!(model["sql"], "SELECT * FROM users");
        });
    }

    #[test]
    fn commands_preview_sql_without_an_argument_or_editor_is_an_error() {
        with_model_restore(|| {
            let host = MockHost::install();
            let err = run(&host.invocation(), None).unwrap_err();
            assert!(matches!(err, Error::Failed(ref text) if text.contains(KEY_NO_INPUT)));
            assert!(host.output()[common::CHANNEL].contains(KEY_NO_INPUT));
        });
    }

    #[test]
    fn commands_preview_sql_without_editor_read_reports_the_degradation() {
        with_model_restore(|| {
            let host = MockHost::install().with_capabilities(&["project.read"]);
            host.open_editor("/project/notes.sql", "sql", "SELECT 1", &[]);
            let err = run(&host.invocation(), None).unwrap_err();
            assert!(matches!(err, Error::Failed(ref text) if text.contains(KEY_EDITOR_UNREADABLE)));
            assert!(host.output()[common::CHANNEL].contains(KEY_EDITOR_UNREADABLE));
        });
    }

    #[test]
    fn commands_preview_sql_missing_mapper_file_is_an_error() {
        with_model_restore(|| {
            let host = MockHost::install();
            let err = run(&host.invocation(), Some("/project/missing.xml#a.b")).unwrap_err();
            assert!(
                matches!(err, Error::Failed(ref text) if text.contains(common::KEY_FILE_UNREADABLE))
            );
        });
    }

    #[test]
    fn commands_preview_sql_mapper_without_statements_is_an_error() {
        with_model_restore(|| {
            let host = MockHost::install();
            host.add_file("/project/Empty.xml", "<mapper namespace=\"a\"/>");
            let err = run(&host.invocation(), Some("/project/Empty.xml")).unwrap_err();
            assert!(matches!(err, Error::Failed(ref text) if text.contains(KEY_NO_STATEMENTS)));
        });
    }
}
