//! `preview-sql`: send a statement to the SQL preview.
//!
//! The IntelliJ edition opens `SqlPreviewDialog`, a modal dialog, from the
//! mapper gutter icon and from `Alt+Shift+Q`. Since API 1.18 the platform has
//! the same shell (`[[contributes.dialogs]]`), so those entries now open
//! `sqlv-preview` — the dialog `preview.presentation` names by default. A user
//! who prefers the pre-dialog shell sets that key to `tab` and gets the fifth
//! tab of the `sqlv-logs` tool window. The render is the same either way —
//! `core::xml` for mapper statements, the workbench's own input path for SQL
//! text — only the shell differs.
//!
//! ## Argument matrix
//!
//! | argument | what is previewed |
//! | --- | --- |
//! | `<mapper.xml>#<statement>` | that statement (bare id or `namespace.id`) |
//! | `<sqlMap.xml>#<statement>` | the same shape for an iBatis 2 document (root `<sqlMap>`) |
//! | `<mapper.xml>` / `@<mapper.xml>` / `<sqlMap.xml>` | every statement; the first one in the tab (iBatis: only when the file holds one) |
//! | `<Mapper.java>#<method>` | the statement annotation on that method (`@Select` / `@Insert` / `@Update` / `@Delete`) |
//! | `<Mapper>#<method>` | the same, with the class looked up in the project index |
//! | `@<path>` / a readable bare path | the file, dispatched by type: mapper XML, iBatis `<sqlMap>`, Java annotations, otherwise SQL text |
//! | wrapper chain text | `render`ed through `analyze::wrapper` (the entity class is read from the project) |
//! | inline text | that text (XML is rendered, SQL is used verbatim) |
//! | none | the focused editor: the first non-empty selection, else the document |
//!
//! Lane F wired the three sources that had a renderer but no entry:
//! `core::annotation`, `core::ibatis` and `analyze::wrapper`. The "which source
//! is this" decision lives in `analyze::sources`, shared with the
//! `sqlv_render_sql` MCP tool; this module only adds the project lookups it
//! needs (an entity class, the file behind `Class#method`) and the preview
//! model shell.
//!
//! Every route ends in a model that carries `exact` / `partial` /
//! `unresolved`: an `unresolved` render fills `reason` and leaves `sql` empty.
//! Nothing here guesses — several annotated statements with no method and no
//! caret is an `unresolved` render listing what the file offers, not "the first
//! one".
//!
//! `editor.read` is optional. Without it the no-argument form reports
//! [`KEY_EDITOR_UNREADABLE`]; the argument forms keep working, which is the
//! documented degradation (a user who refuses the capability can still run the
//! command from the gutter or with a path).

use std::fmt::Write as _;

use openidea_plugin::{Document, Error, Invocation, Position, Range, host};
use serde_json::json;

use crate::analyze::sources::{self, KEY_IBATIS_STATEMENT_REQUIRED};
use crate::analyze::{EntityColumns, entity};
use crate::capture::bridge;
use crate::commands::common::{self, CliError};
use crate::core::xml::{Confidence, MapperSet, RenderOutput};
use crate::views::{log_panel, workbench};

/// Matches `[[contributes.commands]] id` in `plugin.toml`.
pub const ID: &str = "preview-sql";

/// What the command accepts as `arg` (also the text of a missing-argument error).
pub const USAGE: &str = "<mapper.xml|sqlMap.xml>#<statement> | <path> | <Class>#<method> | @<path> | <wrapper chain> | <SQL text> | (no argument: current editor)";

/// Same key `render-xml` reports, so the two commands describe an empty mapper
/// the same way.
pub const KEY_NO_STATEMENTS: &str = "render_xml.no_statements";

/// Nothing was handed over and no editor is focused (or the editor is empty).
pub const KEY_NO_INPUT: &str = "preview.error.no_input";

/// The focused editor exists but its text cannot be read (`editor.read` missing,
/// or the document changed between the two host calls).
pub const KEY_EDITOR_UNREADABLE: &str = "preview.error.editor_unreadable";

/// `Class#method` was given but no Java class of that name is in the project.
pub const KEY_ANNOTATION_CLASS_UNKNOWN: &str = "preview.error.annotation_class_unknown";

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
/// diagnostics and tests), the raw input the diff view shows on its left side
/// (`preview-sql-diff`), and — for a wrapper chain — the projection the preview
/// keeps so the page can re-render without the host re-reading the entity.
pub fn model_for(ctx: &Invocation, arg: Option<&str>) -> Result<ModelFor, CliError> {
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
            Ok((model, detail, xml, None))
        }
        Input::Ibatis {
            path,
            xml,
            statement,
        } => {
            let (model, source) = ibatis_model(&path, &xml, statement.as_deref(), &dialect);
            let detail = format!(
                "{source} (kind: {}, confidence: {}, sql: {} chars)",
                non_empty(&model.kind),
                non_empty(&model.confidence),
                model.sql.chars().count()
            );
            Ok((model, detail, xml, None))
        }
        Input::Annotation {
            path,
            java,
            method,
            caret,
        } => {
            let (model, source) =
                annotation_model(&path, &java, method.as_deref(), caret, &dialect);
            let detail = format!(
                "{source} (kind: {}, confidence: {}, sql: {} chars)",
                non_empty(&model.kind),
                non_empty(&model.confidence),
                model.sql.chars().count()
            );
            Ok((model, detail, java, None))
        }
        Input::Wrapper {
            text,
            source,
            class,
            projection,
        } => {
            let (model, label, context) = wrapper_model(
                &text,
                source.as_deref(),
                class.as_deref(),
                &projection,
                &dialect,
            );
            let detail = format!(
                "{label} (kind: {}, confidence: {}, sql: {} chars)",
                non_empty(&model.kind),
                non_empty(&model.confidence),
                model.sql.chars().count()
            );
            Ok((model, detail, text, context))
        }
        Input::Text { text, source } => {
            let mut model = workbench::render_input(&text, "", &dialect);
            if let Some(source) = &source {
                model.source = source.clone();
            }
            let detail = format!(
                "{} (kind: {}, confidence: {}, sql: {} chars)",
                source.as_deref().unwrap_or("<inline>"),
                non_empty(&model.kind),
                non_empty(&model.confidence),
                model.sql.chars().count()
            );
            Ok((model, detail, text, None))
        }
    }
}

/// One resolved input: the model, its one-line summary, the raw text the diff
/// view shows, and the wrapper projection a re-render needs (if any).
pub type ModelFor = (
    workbench::WorkbenchModel,
    String,
    String,
    Option<workbench::WrapperContext>,
);

/// A model field that may legitimately be empty, for a one-line summary.
fn non_empty(value: &str) -> &str {
    if value.is_empty() { "none" } else { value }
}

/// Resolve the input, render it into the workbench model, push the log panel's
/// state with `selectTab = "preview"` and reveal the tool window.
fn preview(ctx: &Invocation, arg: Option<&str>) -> Result<String, CliError> {
    let (model, detail, raw, wrapper) = model_for(ctx, arg)?;

    // The same handshake as `open-workbench`: the page asks for the state again
    // once its script is ready (`preview-ready` in the dialog, `ready` in the
    // panel), so the plugin-side model has to be set as well as the push below.
    // Lane L45: the statement picker switches by id, so the host keeps the
    // mapper document when that is what was rendered. `model_for` returns the
    // raw input as the third value, which is the mapper XML for a mapper render
    // and the SQL text otherwise.
    let is_mapper = model.kind == workbench::KIND_XML;
    workbench::set_model(model);
    workbench::set_mapper_source(is_mapper.then_some(raw));
    // A wrapper render keeps its projection so the condition checkboxes can
    // re-render; `set_model` cleared it, so it goes in after.
    workbench::set_wrapper_context(wrapper);
    workbench::request_tab();
    workbench::request_input_replace();
    // Lane L48: the shell comes from `preview.presentation`; the dialog is a
    // view of its own, so it is revealed and posted to instead of the panel.
    let target = workbench::preview_target();
    match target {
        workbench::PreviewTarget::Dialog => workbench::push_dialog(),
        workbench::PreviewTarget::Tab => log_panel::push_state(),
    }
    .map_err(|error| CliError::new(bridge::KEY_VIEW_UNAVAILABLE, error.to_string()))?;
    host::reveal_view(target.view_id())
        .map_err(|error| CliError::new(bridge::KEY_VIEW_UNAVAILABLE, error.to_string()))?;

    let mut out = String::new();
    let _ = writeln!(
        out,
        "-- preview-sql: {detail} -> {}#preview",
        target.view_id()
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
    /// An iBatis 2 `<sqlMap>` document rendered through `core::ibatis`.
    Ibatis {
        path: String,
        xml: String,
        statement: Option<String>,
    },
    /// Java source whose `@Select` / `@Insert` / `@Update` / `@Delete` bodies are
    /// rendered through `core::annotation`. `method` and `caret` are the two
    /// ways to choose one; both `None` only works for a one-statement file.
    Annotation {
        path: String,
        java: String,
        method: Option<String>,
        caret: Option<usize>,
    },
    /// A `LambdaQueryWrapper` / `QueryWrapper` chain rendered through
    /// `analyze::wrapper`. The renderer needs a field→column projection;
    /// `projection` says whether the project could supply one.
    Wrapper {
        text: String,
        source: Option<String>,
        class: Option<String>,
        projection: Projection,
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
    // `<path>@<offset>`: what a Java gutter button passes. The offset is the
    // anchor's byte position, resolved here by the same span rule the caret
    // path uses, so an icon on an annotation and one on a wrapper chain share
    // one route.
    if let Some((path, offset)) = split_offset_arg(raw) {
        let java = common::read_file(ctx, &path)?;
        return Ok(java_at(ctx, &path, java, offset));
    }
    if let Some((path, statement)) = split_statement_arg(raw) {
        let xml = common::read_file(ctx, &path)?;
        // `<sqlMap>` and `<mapper>` both take `path#id`; the root element says
        // which renderer, not the file name.
        if sources::looks_like_ibatis(&xml) {
            return Ok(Input::Ibatis {
                path,
                xml,
                statement: Some(statement),
            });
        }
        return Ok(Input::Mapper {
            path,
            xml,
            statement: Some(statement),
        });
    }
    // `<Mapper.java>#<method>`: the path names the file, so no lookup is needed.
    if let Some((path, method)) = split_java_arg(raw) {
        let java = common::read_file(ctx, &path)?;
        return Ok(Input::Annotation {
            path,
            java,
            method: Some(method),
            caret: None,
        });
    }
    // `<Class>#<method>`: the class is looked up in the project index.
    if let Some((class, method)) = annotation_arg(raw) {
        return annotation_by_class(ctx, &class, Some(&method), None);
    }
    // A wrapper chain pasted inline or selected in an editor.
    if sources::looks_like_wrapper(raw) {
        return Ok(wrapper_input(ctx, raw, None));
    }
    // `@<path>` / bare path / inline text, with the same probe rules as
    // `open-workbench`: only an argument free of whitespace is tried as a file,
    // and a failed read falls back to literal text.
    let (text, source) = common::sql_arg(ctx, Some(raw))?;
    Ok(match source {
        // A whole file: its type decides before its content does.
        Some(path) => document_input(ctx, &path, text),
        None => text_input(ctx, text, None),
    })
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

/// `path.java#method` → `(path, method)`.
fn split_java_arg(arg: &str) -> Option<(String, String)> {
    let (path, method) = arg.rsplit_once('#')?;
    let path = path.trim();
    let path = path.strip_prefix('@').unwrap_or(path);
    let method = method.trim();
    if path.is_empty() || path.chars().any(char::is_whitespace) {
        return None;
    }
    if !path.to_ascii_lowercase().ends_with(".java") || !is_identifier(method) {
        return None;
    }
    Some((path.to_string(), method.to_string()))
}

/// `path.java@<offset>` → `(path, offset)`: the argument a Java gutter button
/// passes.
///
/// Distinguishing it from `@<path>` (the "this is a path" prefix) is the `@`
/// position: a prefix form has no path before it, and the offset must be all
/// digits. A bare Java path with an `@` in its directory therefore still reads
/// as a path unless the tail after the last `@` is numeric.
fn split_offset_arg(arg: &str) -> Option<(String, usize)> {
    let (path, offset) = arg.rsplit_once('@')?;
    let path = path.trim();
    let offset = offset.trim();
    if path.is_empty() || path.chars().any(char::is_whitespace) {
        return None;
    }
    if !path.to_ascii_lowercase().ends_with(".java") {
        return None;
    }
    if offset.is_empty() || !offset.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some((path.to_string(), offset.parse().ok()?))
}

/// The input a Java document at a byte offset resolves to.
///
/// An offset inside an annotated statement's method span is that statement
/// (whatever cursor position the icon was drawn from); otherwise it is the
/// wrapper chain containing it. A offset that names neither is reported as "no
/// input at this position" instead of falling back to the whole file — the
/// gutter only ever points at something it found.
fn java_at(ctx: &Invocation, path: &str, java: String, offset: usize) -> Input {
    if let Some(statement) = sources::annotation_at(&java, offset) {
        return Input::Annotation {
            path: path.to_string(),
            java,
            method: statement.method_name.clone(),
            caret: Some(offset),
        };
    }
    match sources::wrapper_chain_at(&java, offset) {
        Some(chain) => wrapper_input(ctx, &chain, Some(path.to_string())),
        // The icon was drawn from a scan of the same text, so this only happens
        // when the file changed between the draw and the click. Rendering the
        // whole file as a chain would be the wrong statement; the annotation
        // model reports "no annotated statement" for it, which is the truth.
        None => Input::Annotation {
            path: path.to_string(),
            java,
            method: None,
            caret: Some(offset),
        },
    }
}

/// `Class#method` (no path): the shape a user types to reach an annotated
/// mapper without opening the file.
///
/// The class part must contain a lower-case letter, so `SELECT#x` and other
/// upper-case SQL fragments keep being read as literal text.
fn annotation_arg(arg: &str) -> Option<(String, String)> {
    if arg.chars().any(char::is_whitespace) {
        return None;
    }
    let (class, method) = arg.rsplit_once('#')?;
    let class = class.trim();
    let method = method.trim();
    if class.is_empty() || !is_identifier(method) || !is_class_name(class) {
        return None;
    }
    Some((class.to_string(), method.to_string()))
}

fn is_class_name(text: &str) -> bool {
    text.chars().any(|ch| ch.is_lowercase()) && text.split('.').all(is_identifier)
}

fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) if first.is_alphabetic() || first == '_' || first == '$' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_alphanumeric() || ch == '_' || ch == '$')
}

/// The `Class#method` route: find the class file, then treat it like any other
/// annotated Java document.
fn annotation_by_class(
    ctx: &Invocation,
    class: &str,
    method: Option<&str>,
    caret: Option<usize>,
) -> Result<Input, CliError> {
    let path = resolve_class_path(ctx, class).ok_or_else(|| {
        CliError::new(
            KEY_ANNOTATION_CLASS_UNKNOWN,
            format!("{class}: no Java class of that name is in the project index"),
        )
    })?;
    let java = common::read_file(ctx, &path)?;
    Ok(Input::Annotation {
        path,
        java,
        method: method.map(str::to_string),
        caret,
    })
}

/// The first indexed class of that name, as a readable Java path.
fn resolve_class_path(ctx: &Invocation, class: &str) -> Option<String> {
    let project = common::project(ctx).ok()?;
    let simple = entity::simple_name(class);
    let qualified = format!(".{simple}");
    let symbols = project.symbols(&simple, 50).ok()?;
    symbols
        .iter()
        .filter(|symbol| symbol.kind.eq_ignore_ascii_case("class"))
        .filter(|symbol| {
            symbol.name == simple || symbol.name == class || symbol.name.ends_with(&qualified)
        })
        .map(|symbol| symbol.location.path.clone())
        .find(|path| path.to_ascii_lowercase().ends_with(".java"))
}

/// Text from a selection or inline argument: content decides.
fn text_input(ctx: &Invocation, text: String, source: Option<String>) -> Input {
    if looks_like_mapper(&text) {
        return Input::Mapper {
            path: source.unwrap_or_else(|| "<inline>".to_string()),
            xml: text,
            statement: None,
        };
    }
    if sources::looks_like_ibatis(&text) {
        return Input::Ibatis {
            path: source.unwrap_or_else(|| "<inline>".to_string()),
            xml: text,
            statement: None,
        };
    }
    if sources::looks_like_wrapper(&text) {
        return wrapper_input(ctx, &text, source);
    }
    Input::Text { text, source }
}

/// A whole file: its type decides first, its content second.
fn document_input(ctx: &Invocation, path: &str, text: String) -> Input {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".java") {
        // Even a Java file with no annotation goes to the annotation model: it
        // renders `unresolved` with "no @Select … found", which is the truth,
        // instead of pasting the class into the preview as if it were SQL.
        // A wrapper chain is the one other source a Java file can be; the
        // chain — never the whole file — is what the renderer reads.
        if sources::annotation_statements(&text).is_empty()
            && let Some(chain) = sources::wrapper_chain_near(&text, None)
        {
            return wrapper_input(ctx, &chain, Some(path.to_string()));
        }
        return Input::Annotation {
            path: path.to_string(),
            java: text,
            method: None,
            caret: None,
        };
    }
    if lower.ends_with(".xml") && !looks_like_mapper(&text) && !sources::looks_like_ibatis(&text) {
        // A non-mapper XML file is not SQL; keep it as text so the workbench
        // reports an empty render instead of pasting markup as a statement.
        return Input::Text {
            text,
            source: Some(path.to_string()),
        };
    }
    text_input(ctx, text, Some(path.to_string()))
}

/// What the project said about a wrapper's entity class.
///
/// The wrapper renderer cannot run without a field→column projection, and the
/// reason it has none is what the preview explains, so the four outcomes stay
/// apart instead of collapsing into "not found".
#[derive(Debug, Clone)]
enum Projection {
    /// The class was read; `fully_declared` is false when the table or a column
    /// came from MyBatis-Plus defaults, which makes the render `partial`.
    Read {
        columns: EntityColumns,
        fully_declared: bool,
    },
    /// No indexed class of that name.
    Unknown,
    /// The class is indexed but its file could not be read.
    Unreadable,
    /// The class was read and declares no field this reader treats as a column.
    NoColumns,
}

/// The wrapper route: read the entity class, then let the renderer decide.
fn wrapper_input(ctx: &Invocation, text: &str, source: Option<String>) -> Input {
    let class = sources::wrapper_entity_class(text);
    let projection = match class.as_deref() {
        Some(class) => match entity_projection(ctx, class) {
            Projection::Read {
                columns,
                fully_declared,
            } => {
                // MPJ: every entity the chain joins needs its own projection —
                // the renderer maps `Car::getDeviceId` against it and takes its
                // table name for the join clause. A class the project cannot
                // resolve is simply absent from the list; the renderer then
                // refuses with the class named, rather than guessing a table.
                let mut columns = columns;
                for joined in sources::wrapper_join_classes(text) {
                    if joined == class || columns.find(&joined).is_some() {
                        continue;
                    }
                    if let Projection::Read {
                        columns: joined_columns,
                        ..
                    } = entity_projection_single(ctx, &joined)
                    {
                        columns = columns.with_related(joined_columns);
                    }
                }
                Projection::Read {
                    fully_declared,
                    columns,
                }
            }
            other => other,
        },
        // No class in the source: the renderer reports that itself.
        None => Projection::Unknown,
    };
    Input::Wrapper {
        text: text.to_string(),
        source,
        class,
        projection,
    }
}

/// The field/column projection of `class`, read from its source in the project.
fn entity_projection(ctx: &Invocation, class: &str) -> Projection {
    let Some(path) = resolve_class_path(ctx, class) else {
        return Projection::Unknown;
    };
    let Ok(java) = common::read_file(ctx, &path) else {
        return Projection::Unreadable;
    };
    let model = entity::parse_entity(class, &java);
    if model.columns.is_empty() {
        return Projection::NoColumns;
    }
    Projection::Read {
        fully_declared: model.fully_declared(),
        columns: model.columns(),
    }
}

/// One entity's projection, without the join fan-out (which would recurse).
fn entity_projection_single(ctx: &Invocation, class: &str) -> Projection {
    let Some(path) = resolve_class_path(ctx, class) else {
        return Projection::Unknown;
    };
    let Ok(java) = common::read_file(ctx, &path) else {
        return Projection::Unreadable;
    };
    let model = entity::parse_entity(class, &java);
    if model.columns.is_empty() {
        return Projection::NoColumns;
    }
    Projection::Read {
        fully_declared: model.fully_declared(),
        columns: model.columns(),
    }
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
        // A selection is a fragment: its content decides, never the file name
        // (a SQL snippet selected inside a Java file is still SQL).
        return Ok(text_input(ctx, text, Some(path)));
    }
    if document.text.trim().is_empty() {
        return Err(CliError::new(
            KEY_NO_INPUT,
            format!("{path}: the editor is empty; usage: {USAGE}"),
        ));
    }
    // A whole Java document: render the annotation under the caret. The
    // annotation model reports "no @Select … found" when the class has none.
    if path.to_ascii_lowercase().ends_with(".java") {
        let caret = caret_offset(&document, &selections);
        // A wrapper chain is read at the caret (or as the document's only
        // chain), never as the whole file: every `.method(…)` in the file is
        // not one statement.
        if sources::annotation_statements(&document.text).is_empty()
            && let Some(chain) = sources::wrapper_chain_near(&document.text, caret)
        {
            return Ok(wrapper_input(ctx, &chain, Some(path)));
        }
        return Ok(Input::Annotation {
            path,
            java: document.text,
            method: None,
            caret,
        });
    }
    Ok(document_input(ctx, &path, document.text))
}

/// The caret as a byte offset into `document`, when the selection is empty.
///
/// A cursor is an empty range (SDK `Range` docs); a real selection was already
/// handled as text, so its end is the closest thing to "where the user is".
fn caret_offset(document: &Document, selections: &[Range]) -> Option<usize> {
    let range = selections.first()?;
    let position = if range.is_empty() {
        range.start
    } else {
        range.end
    };
    document
        .text_in(Range::new(Position::new(0, 0), position))
        .map(str::len)
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

    let rendered: Vec<(String, crate::core::xml::DetailedRender)> = ids
        .into_iter()
        .map(|id| {
            // Lane G: the gutter / command entry carries the same parameter names
            // and condition switches the workbench page shows. `Defaults` is the
            // preview's own profile — commands and MCP keep `Evaluate`, so a
            // shared render never changes meaning under them.
            let detailed = set.render_detailed(
                &id,
                &json!({}),
                crate::core::xml::ConditionSwitches::Defaults,
            );
            (id, detailed)
        })
        .collect();
    let (first_id, detailed) = &rendered[0];
    let out = &detailed.output;

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
    // Parameter names + the condition switches (lane G): `apply_parameters`
    // fills `params` (values) and `parameters` (name/type/placeholder) together
    // so the two lists stay aligned.
    workbench::apply_parameters(&mut model, detailed);
    model.conditions = detailed
        .conditions
        .iter()
        .map(workbench::condition_view)
        .collect();
    // The whole file is the "原文" for a file-level preview; for one statement
    // the page falls back to the rendered SQL, so re-rendering the input does
    // not silently switch to a different statement.
    if statement.is_none() {
        model.raw = xml.to_string();
    }
    if out.confidence == Confidence::Unresolved {
        // `unresolved` never carries SQL: only the reason goes to the page, and
        // no parameter rows or condition switches either — there is nothing for
        // them to bind to.
        model.sql.clear();
        model.params.clear();
        model.parameters.clear();
        model.conditions.clear();
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
        .map(|(id, detailed)| (id.clone(), confidence_str(detailed.output.confidence)))
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

// ---------------------------------------------------------------------------
// The three wired sources: one model builder each
// ---------------------------------------------------------------------------
//
// All three end in `output_model`, which keeps the same contract as the mapper
// path: `confidence` is always set, `reason` explains a `partial`/`unresolved`
// render, and `unresolved` never carries SQL.

/// Render one iBatis `<sqlMap>` statement into the workbench model.
///
/// A document with several statements needs `#id`: the preview page's statement
/// picker is a mapper-XML feature (`KIND_XML`), and offering "the first" of a
/// list the user cannot switch would be a silent pick.
fn ibatis_model(
    path: &str,
    xml: &str,
    statement: Option<&str>,
    dialect: &str,
) -> (workbench::WorkbenchModel, String) {
    let ids = match sources::ibatis_statement_ids(xml) {
        Ok(ids) => ids,
        Err(out) => return (output_model(&out, xml, path, dialect), path.to_string()),
    };
    if ids.is_empty() {
        let out = RenderOutput::unresolved(
            KEY_NO_STATEMENTS,
            format!(
                "{path}: no <select>/<insert>/<update>/<delete>/<statement>/<procedure> in the <sqlMap>"
            ),
        );
        return (output_model(&out, xml, path, dialect), path.to_string());
    }
    let chosen = match statement {
        Some(requested) => match sources::ibatis_resolve_id(&ids, requested) {
            Some(id) => Some(id),
            None => {
                let out = RenderOutput::unresolved(
                    crate::core::ibatis::KEY_STATEMENT_NOT_FOUND,
                    format!(
                        "{path}: no statement `{requested}`; the file has: {}",
                        ids.join(", ")
                    ),
                );
                return (output_model(&out, xml, path, dialect), path.to_string());
            }
        },
        None if ids.len() == 1 => ids.first().cloned(),
        None => {
            let out = RenderOutput::unresolved(
                KEY_IBATIS_STATEMENT_REQUIRED,
                format!(
                    "{path}: {} statements; pass `{path}#<id>`, one of: {}",
                    ids.len(),
                    ids.join(", ")
                ),
            );
            return (output_model(&out, xml, path, dialect), path.to_string());
        }
    };
    let id = chosen.expect("a single-statement document has an id");
    let out = crate::core::ibatis::render(xml, &id, &json!({}));
    let label = format!("{path}#{id}");
    (output_model(&out, &id, &label, dialect), label)
}

/// Render the statement annotation on one Java method into the model.
fn annotation_model(
    path: &str,
    java: &str,
    method: Option<&str>,
    caret: Option<usize>,
    dialect: &str,
) -> (workbench::WorkbenchModel, String) {
    let statements = sources::annotation_statements(java);
    match sources::annotation_pick(java, &statements, method, caret, path) {
        Ok(statement) => {
            let label = match &statement.method_name {
                Some(name) => format!("{path}#{name}"),
                None => format!("{path}#L{}", statement.line),
            };
            let out = sources::render_annotation(statement, &json!({}));
            (output_model(&out, java, &label, dialect), label)
        }
        Err(out) => (output_model(&out, java, path, dialect), path.to_string()),
    }
}

/// Render a wrapper chain into the model.
///
/// Without a field/column projection there is no SQL to emit: the render is
/// `unresolved` and says which class could not be used, and why. A projection
/// that was read also yields the [`workbench::WrapperContext`] the preview
/// store keeps, so the page's condition switches re-render the chain without the
/// host reading the entity again.
fn wrapper_model(
    text: &str,
    source: Option<&str>,
    class: Option<&str>,
    projection: &Projection,
    dialect: &str,
) -> (
    workbench::WorkbenchModel,
    String,
    Option<workbench::WrapperContext>,
) {
    let label = source.unwrap_or("<wrapper>");
    match projection {
        Projection::Read {
            columns,
            fully_declared,
        } => {
            let derived = !fully_declared;
            let mut model = workbench::wrapper_model(
                text,
                columns,
                derived,
                dialect,
                &std::collections::BTreeSet::new(),
            );
            model.source = label.to_string();
            let context = workbench::WrapperContext {
                text: text.to_string(),
                columns: columns.clone(),
                derived,
            };
            (model, label.to_string(), Some(context))
        }
        Projection::Unknown => (
            wrapper_unresolved(
                text,
                label,
                class,
                dialect,
                "is not in the project index, so there is no field/column mapping",
            ),
            label.to_string(),
            None,
        ),
        Projection::Unreadable => (
            wrapper_unresolved(
                text,
                label,
                class,
                dialect,
                "is indexed but its file could not be read",
            ),
            label.to_string(),
            None,
        ),
        Projection::NoColumns => (
            wrapper_unresolved(
                text,
                label,
                class,
                dialect,
                "declares no column this reader understands (static and collection fields are skipped)",
            ),
            label.to_string(),
            None,
        ),
    }
}

/// A wrapper render that could not run for lack of a projection, still labelled
/// as a wrapper so the page does not present it as plain SQL.
fn wrapper_unresolved(
    text: &str,
    label: &str,
    class: Option<&str>,
    dialect: &str,
    detail: &str,
) -> workbench::WorkbenchModel {
    let out = sources::entity_unknown(label, class, detail);
    let mut model = output_model(&out, text, label, dialect);
    model.kind = workbench::KIND_WRAPPER.to_string();
    model
}

/// The workbench model for a render that is not a mapper document.
fn output_model(
    out: &RenderOutput,
    raw: &str,
    source: &str,
    dialect: &str,
) -> workbench::WorkbenchModel {
    let unresolved = out.confidence == Confidence::Unresolved;
    let sql = if unresolved { "" } else { out.sql.trim_end() };
    let mut model = workbench::render_input(sql, workbench::KIND_SQL, dialect);
    model.kind = workbench::KIND_SQL.to_string();
    model.source = source.to_string();
    model.raw = raw.to_string();
    model.confidence = confidence_str(out.confidence).to_string();
    if unresolved {
        // `unresolved` never carries SQL: only the reason goes to the page.
        model.sql.clear();
        model.params.clear();
        model.advice.clear();
    } else {
        model.params = out.params.iter().map(param_text).collect();
    }
    model.reason = reason_lines(out).join("\n");
    model
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
        .map(|note| {
            format!(
                "[{}] {}",
                note.message_key,
                crate::i18n::reason_body(&note.message_key, &note.detail)
            )
        })
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
    use serde_json::{Value, json};

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

    /// The preview payload the command produced, whatever shell it went to.
    ///
    /// Lane L48: the dialog is the default shell and receives
    /// `{op:"preview-tab", tab}`, while the tab shell receives the panel's whole
    /// `state`. Both carry the same `tabs.preview` object, so the tests read it
    /// through this one helper and stay shell-independent; `selectTab` comes
    /// from the store, which is where the dialog shell keeps the request.
    fn preview_state(host: &MockHost) -> Value {
        let messages = host.view_messages();
        assert_eq!(messages.len(), 1, "{messages:?}");
        let value: Value = serde_json::from_str(&messages[0].1).expect("valid JSON");
        match workbench::preview_target() {
            workbench::PreviewTarget::Dialog => {
                assert_eq!(messages[0].0, workbench::DIALOG_VIEW_ID);
                assert_eq!(value["op"], "preview-tab");
                json!({
                    "selectTab": workbench::select_tab(),
                    "tabs": { "preview": value["tab"].clone() },
                })
            }
            workbench::PreviewTarget::Tab => {
                assert_eq!(messages[0].0, log_panel::VIEW_ID);
                value
            }
        }
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
            assert!(model["sql"].as_str().is_some_and(|sql| {
                sql.split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .contains("UPDATE users SET name = ?")
            }));
            // A successful preview leaves the Output tab alone; the shell is the
            // surface. (Errors still go there, tested below.)
            assert!(
                !host.output().contains_key(common::CHANNEL),
                "{:?}",
                host.output().get(common::CHANNEL)
            );
            assert_eq!(
                host.revealed_views(),
                [workbench::preview_target().view_id().to_string()]
            );
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

    #[test]
    fn commands_preview_sql_keeps_the_tab_shell_when_the_setting_says_so() {
        // Lane L48: `preview.presentation = tab` is the pre-dialog shell, and it
        // must keep working exactly as before — the panel's `state` push with
        // `selectTab`, and the tool window brought forward.
        with_model_restore(|| {
            let host = MockHost::install();
            host.set_setting("preview.presentation", "\"tab\"");
            host.add_file("/project/UserMapper.xml", MAPPER);
            run(
                &host.invocation(),
                Some("/project/UserMapper.xml#com.example.UserMapper.findById"),
            )
            .unwrap();

            let state = preview_state(&host);
            assert_eq!(state["selectTab"], "preview");
            assert_eq!(state["tabs"]["preview"]["model"]["kind"], "xml");
            assert_eq!(host.revealed_views(), [log_panel::VIEW_ID.to_string()]);
            assert!(
                !host
                    .revealed_views()
                    .contains(&workbench::DIALOG_VIEW_ID.to_string()),
                "no dialog in the tab shell"
            );
        });
    }

    // -----------------------------------------------------------------------
    // Lane F: the three wired sources
    // -----------------------------------------------------------------------

    const ANNOTATED: &str = r#"package com.example;

public interface UserMapper {
    @Select("SELECT id, name FROM users WHERE id = #{id}")
    User findById(@Param("id") Long id);

    @Update("UPDATE users SET name = #{name} WHERE id = #{id}")
    int rename(@Param("name") String name, @Param("id") Long id);
}
"#;

    const ONE_ANNOTATION: &str = r#"public interface SimpleMapper {
    @Delete("DELETE FROM users WHERE id = #{id}")
    int deleteById(Long id);
}
"#;

    const SQLMAP: &str = r#"<?xml version="1.0"?>
<sqlMap namespace="User">
    <select id="selectAll">SELECT * FROM users</select>
    <select id="selectById">SELECT * FROM users WHERE id = #id#</select>
</sqlMap>"#;

    fn symbol_of(name: &str, path: &str) -> openidea_plugin::SymbolInfo {
        openidea_plugin::SymbolInfo {
            name: name.to_string(),
            kind: "class".to_string(),
            container: None,
            location: openidea_plugin::Location {
                path: path.to_string(),
                range: Range::default(),
            },
        }
    }

    #[test]
    fn commands_preview_sql_renders_an_annotation_from_a_java_path() {
        with_model_restore(|| {
            let host = MockHost::install();
            host.add_file("/project/UserMapper.java", ANNOTATED);
            run(&host.invocation(), Some("/project/UserMapper.java#rename")).unwrap();

            let state = preview_state(&host);
            assert_eq!(state["selectTab"], "preview");
            let model = model_of(&state);
            assert_eq!(model["source"], "/project/UserMapper.java#rename");
            assert_eq!(model["kind"], "sql");
            assert_eq!(model["confidence"], "exact");
            assert!(
                model["sql"].as_str().is_some_and(
                    |sql| compact(sql).contains("UPDATE users SET name = ? WHERE id = ?")
                ),
                "{model}"
            );
            // No mapper document, so the statement picker stays hidden.
            assert_eq!(model["statements"], json!([]));
            assert!(!host.output().contains_key(common::CHANNEL));
        });
    }

    #[test]
    fn commands_preview_sql_finds_the_class_behind_class_hash_method() {
        with_model_restore(|| {
            let host = MockHost::install();
            host.add_file("/project/UserMapper.java", ANNOTATED);
            host.set_symbols(
                "/project",
                vec![symbol_of("UserMapper", "/project/UserMapper.java")],
            );
            run(&host.invocation(), Some("UserMapper#findById")).unwrap();

            let model = model_of(&preview_state(&host));
            assert_eq!(model["confidence"], "exact");
            assert_eq!(model["source"], "/project/UserMapper.java#findById");
            assert!(
                model["sql"].as_str().is_some_and(
                    |sql| compact(sql).contains("SELECT id, name FROM users WHERE id = ?")
                ),
                "{model}"
            );
        });
    }

    #[test]
    fn commands_preview_sql_unknown_class_is_an_error_with_its_own_key() {
        with_model_restore(|| {
            let host = MockHost::install();
            let err = run(&host.invocation(), Some("GhostMapper#findById")).unwrap_err();
            assert!(
                matches!(err, Error::Failed(ref text) if text.contains(KEY_ANNOTATION_CLASS_UNKNOWN)),
                "{err:?}"
            );
            assert!(host.output()[common::CHANNEL].contains(KEY_ANNOTATION_CLASS_UNKNOWN));
        });
    }

    #[test]
    fn commands_preview_sql_renders_the_annotation_under_the_caret() {
        with_model_restore(|| {
            let host = MockHost::install();
            // Caret inside `rename` (0-based line 6, column 4).
            host.open_editor(
                "/project/UserMapper.java",
                "java",
                ANNOTATED,
                &[(6, 4, 6, 4)],
            );
            run(&host.invocation(), None).unwrap();

            let model = model_of(&preview_state(&host));
            assert_eq!(model["confidence"], "exact");
            assert_eq!(model["source"], "/project/UserMapper.java#rename");
            assert!(
                model["sql"].as_str().is_some_and(|sql| sql
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .contains("UPDATE users")),
                "{model}"
            );
        });
    }

    #[test]
    fn commands_preview_sql_annotation_without_a_pick_is_unresolved_and_lists_methods() {
        with_model_restore(|| {
            let host = MockHost::install();
            // Caret on the interface declaration, inside no method.
            host.open_editor(
                "/project/UserMapper.java",
                "java",
                ANNOTATED,
                &[(1, 2, 1, 2)],
            );
            run(&host.invocation(), None).unwrap();

            let model = model_of(&preview_state(&host));
            assert_eq!(model["confidence"], "unresolved");
            assert_eq!(model["sql"], "");
            let reason = model["reason"].as_str().unwrap_or_default();
            assert!(reason.contains("annotation.method.required"), "{reason}");
            assert!(reason.contains("findById, rename"), "{reason}");
        });
    }

    #[test]
    fn commands_preview_sql_single_annotation_needs_no_method() {
        with_model_restore(|| {
            let host = MockHost::install();
            host.add_file("/project/SimpleMapper.java", ONE_ANNOTATION);
            run(&host.invocation(), Some("/project/SimpleMapper.java")).unwrap();

            let model = model_of(&preview_state(&host));
            assert_eq!(model["confidence"], "exact");
            assert_eq!(model["source"], "/project/SimpleMapper.java#deleteById");
            assert!(
                model["sql"].as_str().is_some_and(|sql| sql
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .contains("DELETE FROM users WHERE id = ?")),
                "{model}"
            );
        });
    }

    #[test]
    fn commands_preview_sql_renders_an_ibatis_statement_by_id() {
        with_model_restore(|| {
            let host = MockHost::install();
            host.add_file("/project/UserSqlMap.xml", SQLMAP);
            run(
                &host.invocation(),
                Some("/project/UserSqlMap.xml#selectById"),
            )
            .unwrap();

            let state = preview_state(&host);
            let model = model_of(&state);
            assert_eq!(model["confidence"], "exact");
            assert_eq!(model["kind"], "sql");
            assert_eq!(model["source"], "/project/UserSqlMap.xml#selectById");
            assert!(
                model["sql"].as_str().is_some_and(|sql| sql
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .contains("SELECT * FROM users WHERE id = ?")),
                "{model}"
            );
        });
    }

    #[test]
    fn commands_preview_sql_ibatis_with_several_statements_needs_an_id() {
        with_model_restore(|| {
            let host = MockHost::install();
            host.add_file("/project/UserSqlMap.xml", SQLMAP);
            run(&host.invocation(), Some("/project/UserSqlMap.xml")).unwrap();

            let model = model_of(&preview_state(&host));
            assert_eq!(model["confidence"], "unresolved");
            assert_eq!(model["sql"], "");
            let reason = model["reason"].as_str().unwrap_or_default();
            assert!(reason.contains(KEY_IBATIS_STATEMENT_REQUIRED), "{reason}");
            assert!(reason.contains("selectAll, selectById"), "{reason}");
        });
    }

    #[test]
    fn commands_preview_sql_ibatis_unknown_statement_is_unresolved() {
        with_model_restore(|| {
            let host = MockHost::install();
            host.add_file("/project/UserSqlMap.xml", SQLMAP);
            run(&host.invocation(), Some("/project/UserSqlMap.xml#nope")).unwrap();

            let model = model_of(&preview_state(&host));
            assert_eq!(model["confidence"], "unresolved");
            assert_eq!(model["sql"], "");
            assert!(
                model["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("ibatis.statement.not_found")),
                "{model}"
            );
        });
    }

    const DECLARED_ENTITY: &str = r#"
@TableName("sys_user")
public class User {
    @TableId("id")
    private Long id;

    @TableField("status")
    private String status;
}
"#;

    const DERIVED_ENTITY: &str = r#"
@TableName("sys_user")
public class User {
    @TableField("status")
    private String status;

    private Integer age;
}
"#;

    #[test]
    fn commands_preview_sql_renders_a_wrapper_chain_exact_when_every_column_is_declared() {
        with_model_restore(|| {
            let host = MockHost::install();
            host.add_file("/project/entity/User.java", DECLARED_ENTITY);
            host.set_symbols(
                "/project",
                vec![symbol_of("User", "/project/entity/User.java")],
            );
            run(
                &host.invocation(),
                Some(r#"new LambdaQueryWrapper<User>().eq(User::getStatus, "A")"#),
            )
            .unwrap();

            let model = model_of(&preview_state(&host));
            assert_eq!(model["confidence"], "exact", "{model}");
            assert_eq!(
                model["sql"],
                "SELECT * FROM sys_user
WHERE status = 'A'"
            );
        });
    }

    #[test]
    fn commands_preview_sql_wrapper_with_a_derived_column_is_partial() {
        with_model_restore(|| {
            let host = MockHost::install();
            host.add_file("/project/entity/User.java", DERIVED_ENTITY);
            host.set_symbols(
                "/project",
                vec![symbol_of("User", "/project/entity/User.java")],
            );
            run(
                &host.invocation(),
                Some("new LambdaQueryWrapper<User>().eq(User::getAge, 18)"),
            )
            .unwrap();

            let model = model_of(&preview_state(&host));
            assert_eq!(model["confidence"], "partial", "{model}");
            assert_eq!(
                model["sql"],
                "SELECT * FROM sys_user
WHERE age = 18"
            );
            assert!(
                model["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("wrapper.column.derived")),
                "{model}"
            );
        });
    }

    #[test]
    fn commands_preview_sql_wrapper_without_its_entity_is_unresolved() {
        with_model_restore(|| {
            let host = MockHost::install();
            run(
                &host.invocation(),
                Some(r#"new LambdaQueryWrapper<Ghost>().eq(Ghost::getStatus, "A")"#),
            )
            .unwrap();

            let model = model_of(&preview_state(&host));
            assert_eq!(model["confidence"], "unresolved");
            assert_eq!(model["sql"], "");
            let reason = model["reason"].as_str().unwrap_or_default();
            assert!(reason.contains("wrapper.entity.not_found"), "{reason}");
            assert!(reason.contains("Ghost"), "{reason}");
        });
    }

    // The gutter's Java argument (`<path>@<offset>`) ---------------------

    #[test]
    fn commands_preview_sql_accepts_the_gutter_offset_for_a_wrapper_chain() {
        with_model_restore(|| {
            let host = MockHost::install();
            host.add_file("/project/entity/User.java", DECLARED_ENTITY);
            host.set_symbols(
                "/project",
                vec![symbol_of("User", "/project/entity/User.java")],
            );
            let service = r#"package a;

public class UserService {
    java.util.List<User> list(String status) {
        LambdaQueryWrapper<User> wrapper = new LambdaQueryWrapper<>();
        wrapper.eq(User::getStatus, status);
        return mapper.selectList(wrapper);
    }
}
"#;
            host.add_file("/project/UserService.java", service);
            let offset = service
                .find("LambdaQueryWrapper<User>")
                .expect("declaration");
            run(
                &host.invocation(),
                Some(&format!("/project/UserService.java@{offset}")),
            )
            .unwrap();

            let model = model_of(&preview_state(&host));
            assert_eq!(model["kind"], "wrapper");
            assert_eq!(model["table"], "sys_user");
            assert!(
                model["sql"].as_str().is_some_and(|sql| sql
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .contains("FROM sys_user WHERE status = ?")),
                "{model}"
            );
            // The chain, not the whole file: the declaration and the one call
            // that continues it before the caret.
            assert!(
                model["params"]
                    .as_array()
                    .is_some_and(|rows| rows.len() == 1),
                "{model}"
            );
        });
    }

    #[test]
    fn commands_preview_sql_resolves_the_gutter_offset_inside_a_continued_chain() {
        with_model_restore(|| {
            let host = MockHost::install();
            host.add_file("/project/entity/User.java", DECLARED_ENTITY);
            host.set_symbols(
                "/project",
                vec![symbol_of("User", "/project/entity/User.java")],
            );
            let service = r#"package a;

public class UserService {
    java.util.List<User> list(String status) {
        LambdaQueryWrapper<User> wrapper = new LambdaQueryWrapper<>();
        wrapper.eq(User::getStatus, status);
        return mapper.selectList(wrapper);
    }
}
"#;
            host.add_file("/project/UserService.java", service);
            // The icon on a chaining statement resolves the chain it belongs to,
            // so both icons of one chain reach the same render.
            let at_call = service.find("wrapper.eq").expect("the chaining call");
            run(
                &host.invocation(),
                Some(&format!("/project/UserService.java@{at_call}")),
            )
            .unwrap();

            let model = model_of(&preview_state(&host));
            assert_eq!(model["kind"], "wrapper");
            assert!(
                model["sql"].as_str().is_some_and(|sql| sql
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .contains("status = ?")),
                "{model}"
            );
        });
    }

    #[test]
    fn commands_preview_sql_accepts_the_gutter_offset_for_an_annotation() {
        with_model_restore(|| {
            let host = MockHost::install();
            host.add_file("/project/ByIds.java", ONE_ANNOTATION);
            let offset = ONE_ANNOTATION.find("@Delete").expect("the annotation");
            run(
                &host.invocation(),
                Some(&format!("/project/ByIds.java@{offset}")),
            )
            .unwrap();

            let model = model_of(&preview_state(&host));
            assert_eq!(model["confidence"], "exact");
            assert!(
                model["sql"].as_str().is_some_and(|sql| sql
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .contains("DELETE FROM users")),
                "{model}"
            );
        });
    }

    #[test]
    fn commands_preview_sql_offset_form_is_not_mistaken_for_a_path_prefix() {
        // `@<path>` is the "this is a path" prefix; `path@123` is the gutter
        // form. The two must not be confused, and a non-numeric tail stays a path.
        assert_eq!(
            split_offset_arg("/project/A.java@42"),
            Some(("/project/A.java".to_string(), 42))
        );
        assert_eq!(split_offset_arg("@/project/A.java"), None);
        assert_eq!(split_offset_arg("/project/A.java#find"), None);
        assert_eq!(split_offset_arg("/project/A.xml@42"), None);
        assert_eq!(
            split_offset_arg("/a@b/UserService.java@7"),
            Some(("/a@b/UserService.java".to_string(), 7)),
            "a directory named with an @ still works: the tail must be numeric"
        );
    }

    #[test]
    fn commands_preview_sql_annotation_with_a_degraded_script_is_partial() {
        with_model_restore(|| {
            let host = MockHost::install();
            host.add_file(
                "/project/ByIds.java",
                r#"public interface ByIds {
    @Select("<script>SELECT * FROM users WHERE id IN <foreach collection='ids' item='i' open='(' separator=',' close=')'>#{i}</foreach></script>")
    java.util.List<User> byIds(@Param("ids") java.util.List<Long> ids);
}
"#,
            );
            run(&host.invocation(), Some("/project/ByIds.java#byIds")).unwrap();

            let model = model_of(&preview_state(&host));
            assert_eq!(model["confidence"], "partial", "{model}");
            assert!(
                model["sql"]
                    .as_str()
                    .is_some_and(|sql| sql.starts_with("SELECT * FROM users WHERE id IN")),
                "{model}"
            );
            assert!(
                model["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("xml.foreach.null_collection")),
                "{model}"
            );
        });
    }

    #[test]
    fn commands_preview_sql_ibatis_degradation_is_partial_with_its_reason() {
        with_model_restore(|| {
            let host = MockHost::install();
            host.add_file(
                "/project/Broken.xml",
                r#"<sqlMap namespace="User">
    <select id="selectGone">SELECT <include refid="gone"/> FROM users</select>
</sqlMap>"#,
            );
            run(&host.invocation(), Some("/project/Broken.xml#selectGone")).unwrap();

            let model = model_of(&preview_state(&host));
            assert_eq!(model["confidence"], "partial", "{model}");
            assert!(
                model["sql"].as_str().is_some_and(|sql| sql
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .contains("FROM users")),
                "{model}"
            );
            assert!(
                model["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("ibatis.include.not_found")),
                "{model}"
            );
        });
    }
}
