//! The two input tabs and the preview routing (lane W2-0).
//!
//! Split out of `views::log_panel` with no behaviour change: the state and ops
//! of the "Log → SQL" and "SQL → Java" panes, the "send to tab" hand-off, and
//! the routing of the fifth tab's `preview-*` messages to `views::workbench`.

use openidea_plugin::{Error, host};
use serde_json::{Value, json};

use crate::core::log::{LogEntry, parse};
use crate::i18n::t;

use super::push_state;
use super::state::{parse_options, store};

/// One row handed to one of the two input tabs (lane L32).
///
/// The tabs' own input panes belong to the next lane; this build only carries
/// the text and the tab to show, which is what the reference's
/// `SendToSqlToJavaAction` / `SendToLogToSqlAction` put in the input area.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SendToTab {
    /// `sqlToJava` or `logToSql`.
    pub(super) tab: String,
    /// The SQL (→ SQL→Java) or the original log line (→ 日志→SQL).
    pub(super) text: String,
    /// Whether the page should bring that tab forward.
    pub(super) select_tab: bool,
}

/// State of the "Log → SQL" tab (lane L33).
///
/// The reference panel is a self-contained input pane: the user pastes MyBatis
/// (or Hibernate / JdbcTemplate) log lines, clicks Convert, and gets every
/// statement in the paste back as SQL. The input text belongs to the page; the
/// host keeps only what a state push has to restore — the result and the
/// status line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct LogToSqlTab {
    /// Converted SQL: `-- SQL #N` headers between groups, formatted for the
    /// configured dialect. The page derives the compact form itself.
    output: String,
    /// One line under the two panes.
    status: String,
    /// `info` (muted) or `error` (red); the page colours the line.
    status_kind: String,
    /// Input text written by "从剪贴板读取"; one-shot (see `replaceInput`).
    input: String,
    /// Set when the host, not the user, filled the input pane.
    replace_input: bool,
}

/// The "SQL → Java" tab's option row.
///
/// `entity` is what the generator needs (`sqlv_reverse_sql` takes
/// `{sql, entity}`); `format` / `caller` / `return_type` are carried and
/// echoed, but this build's generator only emits the Lambda form, so they
/// never change the code — the tab says so in its status line instead of
/// pretending (see `docs/registration/L33.md`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SqlToJavaOptions {
    /// `lambda` | `stringColumns`.
    format: String,
    /// Java entity class name; empty yields `unresolved`.
    entity: String,
    /// Caller class (Service/Mapper); recorded, not applied.
    caller: String,
    /// Return type declaration; recorded, not applied.
    return_type: String,
}

impl Default for SqlToJavaOptions {
    fn default() -> Self {
        Self {
            format: "lambda".to_string(),
            entity: String::new(),
            caller: String::new(),
            return_type: String::new(),
        }
    }
}

/// State of the "SQL → Java" tab (lane L33).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct SqlToJavaTab {
    /// Generated code; empty for `unresolved` and before the first run.
    code: String,
    /// One line under the two panes (the generator's reason when no code).
    status: String,
    /// `info` / `error`.
    status_kind: String,
    /// The option row's last values, echoed so the page can restore them.
    options: SqlToJavaOptions,
    /// Input text after an op that rewrote it (formatting): the page replaces
    /// the editor with it once, then the text belongs to the user again.
    input: String,
    /// One-shot flag consumed by [`sql_to_java_state`].
    replace_input: bool,
}
// ---------------------------------------------------------------------------
// Tab payloads (lane L22, docs/registration/L22.md)
// ---------------------------------------------------------------------------

/// `tabs.logToSql`: the tab's result and status line (lane L33).
///
/// `replaceInput` is one-shot: only the clipboard op makes the host fill the
/// page's input pane, so an ordinary push never overwrites what the user typed.
pub(super) fn log_to_sql_state(tab: &mut LogToSqlTab) -> Value {
    let replace_input = std::mem::take(&mut tab.replace_input);
    json!({
        "output": tab.output,
        "status": tab.status,
        "statusKind": tab.status_kind,
        "input": tab.input,
        "replaceInput": replace_input,
    })
}

/// Outcome of one "Log → SQL" conversion (pure, testable).
#[derive(Debug, Clone, PartialEq, Eq)]
struct LogConversion {
    /// Executable statements, `\n\n`-separated with `-- SQL #N` headers when
    /// the paste held more than one.
    output: String,
    /// Statements recovered.
    groups: usize,
    /// Statements that still carry a `?` placeholder (parameters missing).
    unresolved: usize,
}

/// Converts a pasted log text with the same parser and prefix settings the
/// capture lane uses, and formats each statement for the configured dialect.
///
/// A paste may hold several `Preparing:`/`Parameters:` groups (or a mix of
/// MyBatis / Hibernate / JdbcTemplate lines): every recovered statement is
/// included, and groups are separated by `-- SQL #N` comments. The page
/// derives the compact form from this one output.
fn convert_log_text(text: &str) -> LogConversion {
    let analysis = parse(text, &parse_options());
    let entries: Vec<&LogEntry> = analysis
        .entries
        .iter()
        .filter(|entry| entry.has_sql())
        .collect();
    if entries.is_empty() {
        return LogConversion {
            output: String::new(),
            groups: 0,
            unresolved: 0,
        };
    }
    let (dialect_name, _) = crate::views::workbench::configured_dialect();
    let dialect = crate::core::dialect::by_name(&dialect_name);
    let multi = entries.len() > 1;
    let mut unresolved = 0;
    let mut parts: Vec<String> = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let base = with_terminator(&entry.sql);
        if count_placeholders(&base) > 0 {
            unresolved += 1;
        }
        let body = match dialect {
            Some(found) => crate::core::format::format(&base, found),
            None => base,
        };
        parts.push(if multi {
            format!("-- SQL #{}\n{}", index + 1, body)
        } else {
            body
        });
    }
    LogConversion {
        output: parts.join("\n\n"),
        groups: entries.len(),
        unresolved,
    }
}

/// Appends the statement terminator the reference adds to every result
/// (already-terminated and empty statements are left alone).
fn with_terminator(sql: &str) -> String {
    if sql.trim_end().ends_with(';') {
        sql.to_string()
    } else {
        format!("{sql};")
    }
}

/// Counts `?` placeholders outside quoted strings and comments.
///
/// `core::log` counts placeholders while restoring, but that scanner is
/// private to the core module (which this lane must not touch); the status
/// line only needs the count, so the same skipping rules live here.
fn count_placeholders(sql: &str) -> usize {
    let bytes = sql.as_bytes();
    let mut index = 0;
    let mut count = 0;
    while index < bytes.len() {
        match bytes[index] {
            quote @ (b'\'' | b'"' | b'`') => {
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == quote {
                        // A doubled quote is an escaped one inside the literal.
                        if index + 1 < bytes.len() && bytes[index + 1] == quote {
                            index += 2;
                            continue;
                        }
                        break;
                    }
                    index += 1;
                }
                index = (index + 1).min(bytes.len());
            }
            b'-' if index + 1 < bytes.len() && bytes[index + 1] == b'-' => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if index + 1 < bytes.len() && bytes[index + 1] == b'*' => {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
            }
            b'?' => {
                count += 1;
                index += 1;
            }
            _ => index += 1,
        }
    }
    count
}

/// Runs one "Log → SQL" conversion and keeps its result and status line.
fn run_log_to_sql(text: &str) {
    let mut state = store();
    state.log_to_sql.input = text.to_string();
    if text.trim().is_empty() {
        // Like the reference, a failed conversion states the reason and
        // leaves the previous result alone.
        state.log_to_sql.status = t("view.logs.tab.log_to_sql.empty");
        state.log_to_sql.status_kind = "error".to_string();
        return;
    }
    let conversion = convert_log_text(text);
    if conversion.groups == 0 {
        state.log_to_sql.status = t("view.logs.tab.log_to_sql.no_entry");
        state.log_to_sql.status_kind = "error".to_string();
        return;
    }
    let (status, kind) = if conversion.unresolved > 0 {
        (
            t("view.logs.tab.log_to_sql.unresolved")
                .replace("{0}", &conversion.unresolved.to_string()),
            "error",
        )
    } else {
        (
            t("view.logs.tab.log_to_sql.converted").replace("{0}", &conversion.groups.to_string()),
            "info",
        )
    };
    state.log_to_sql.output = conversion.output;
    state.log_to_sql.status = status;
    state.log_to_sql.status_kind = kind.to_string();
}

/// "从剪贴板读取": fills the input pane from the clipboard and converts at
/// once, the same "fill then convert" the reference does. An empty clipboard
/// and a refused capability are status-line hints, never error dialogs.
fn read_log_clipboard() {
    let text = match host::read_clipboard() {
        Ok(content) => content.text.unwrap_or_default(),
        // The capability (`clipboard.read`) is optional and this build does
        // not declare it yet; say so instead of pretending the clipboard is
        // empty (see `docs/registration/L33.md`).
        Err(_) => {
            let mut state = store();
            state.log_to_sql.status = t("view.logs.tab.log_to_sql.clipboard_denied");
            state.log_to_sql.status_kind = "error".to_string();
            return;
        }
    };
    if text.trim().is_empty() {
        let mut state = store();
        state.log_to_sql.status = t("view.logs.tab.log_to_sql.clipboard_empty");
        state.log_to_sql.status_kind = "error".to_string();
        return;
    }
    run_log_to_sql(&text);
    store().log_to_sql.replace_input = true;
}

/// Clears the tab's result and status line; the input pane is the page's.
fn clear_log_to_sql() {
    store().log_to_sql = LogToSqlTab::default();
}

/// `tabs.sqlToJava`: the last conversion, the status line and the option row.
///
/// `replaceInput` is one-shot: it is taken on the way out, so a later push
/// (capture refresh, filter change) never overwrites what the user typed.
pub(super) fn sql_to_java_state(tab: &mut SqlToJavaTab) -> Value {
    let replace_input = std::mem::take(&mut tab.replace_input);
    json!({
        "code": tab.code,
        "status": tab.status,
        "statusKind": tab.status_kind,
        "options": {
            "format": tab.options.format,
            "entity": tab.options.entity,
            "caller": tab.options.caller,
            "returnType": tab.options.return_type,
        },
        "input": tab.input,
        "replaceInput": replace_input,
    })
}

/// Normalises the wrapper-form value the page sends.
fn normalize_wrapper_format(value: &str) -> String {
    match value.trim() {
        "stringColumns" | "string-columns" | "string_columns" | "columns" => {
            "stringColumns".to_string()
        }
        _ => "lambda".to_string(),
    }
}

/// Runs the `sqlv_reverse_sql` handler for the page and keeps the result.
///
/// The handler takes `{sql, entity}` and refuses unsupported shapes with an
/// `unresolved` result; that verdict is passed through as a status line
/// instead of guessing.
fn run_sql_to_java(sql: &str, options: SqlToJavaOptions) {
    let mut state = store();
    state.sql_to_java.options = options.clone();
    if sql.trim().is_empty() {
        state.sql_to_java.code.clear();
        state.sql_to_java.status = t("view.logs.tab.sql_to_java.empty");
        state.sql_to_java.status_kind = "error".to_string();
        return;
    }
    let arguments = json!({ "sql": sql.trim(), "entity": options.entity }).to_string();
    let raw = crate::mcp::handlers::reverse_sql::run(&arguments);
    let value: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        state.sql_to_java.code.clear();
        state.sql_to_java.status = t(value
            .pointer("/error/message_key")
            .and_then(Value::as_str)
            .unwrap_or("mcp.error.unknown_tool"));
        state.sql_to_java.status_kind = "error".to_string();
        return;
    }
    let data = &value["data"];
    let code = data.get("code").and_then(Value::as_str).unwrap_or("");
    if code.trim().is_empty() {
        state.sql_to_java.code.clear();
        let reason = reason_lines(data);
        state.sql_to_java.status = if reason.is_empty() {
            t("view.logs.tab.sql_to_java.no_code")
        } else {
            reason
        };
        state.sql_to_java.status_kind = "error".to_string();
        return;
    }
    state.sql_to_java.code = code.to_string();
    // The option row is echoed for parity with the reference, but this build's
    // generator only emits the Lambda form; the page shows a persistent hint
    // next to the option row instead of pretending the choice changed the code
    // (`docs/registration/L33.md`).
    state.sql_to_java.status = t("view.logs.tab.sql_to_java.generated");
    state.sql_to_java.status_kind = "info".to_string();
}

/// Formats the tab's input SQL for the configured dialect; the page replaces
/// its editor with the result once (`replaceInput`).
fn format_sql_input(text: &str) {
    let (dialect_name, _) = crate::views::workbench::configured_dialect();
    let formatted = match crate::core::dialect::by_name(&dialect_name) {
        Some(found) => crate::core::format::format(text, found),
        None => text.to_string(),
    };
    let mut state = store();
    state.sql_to_java.input = formatted;
    state.sql_to_java.replace_input = true;
}

/// Clears the result and status line; the option row survives (the user's
/// choices are not a result), and so does the input pane (page-owned).
fn clear_sql_to_java() {
    let mut state = store();
    let options = state.sql_to_java.options.clone();
    state.sql_to_java = SqlToJavaTab {
        options,
        ..SqlToJavaTab::default()
    };
}
/// `unresolved[]` of a generator result, one line per entry. A key without a
/// translation stays visible as the key, the same contract the workbench uses.
fn reason_lines(data: &Value) -> String {
    data.get("unresolved")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    let key = item
                        .get("message_key")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let detail = item.get("detail").cloned().unwrap_or(Value::Null);
                    let has_detail = detail.as_object().is_some_and(|map| !map.is_empty());
                    if has_detail {
                        format!("{} {}", t(key), detail)
                    } else {
                        t(key)
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}
// ---------------------------------------------------------------------------
// Ops (the dispatch in `mod.rs` calls one of these per arm)
// ---------------------------------------------------------------------------

/// `send-to-tab`: "发送到 SQL→Java" / "发送到 日志→SQL".
///
/// The two tabs' input panes are the next lane's work; what this lane owes them
/// is the hand-off itself (which tab, what text), which stays in the state until
/// the page reports the tab it now shows — a push sent before the page finished
/// loading must not be the only chance it gets.
pub(super) fn send_to_tab(message: &Value) -> Result<(), Error> {
    let target = match message.get("tab").and_then(Value::as_str) {
        Some("sqlToJava") => "sqlToJava",
        Some("logToSql") => "logToSql",
        _ => "",
    };
    if !target.is_empty()
        && let Some(line) = super::message_line(message)
    {
        let text = if target == "sqlToJava" {
            super::logs::sql_of_line(line).unwrap_or_default()
        } else {
            store()
                .text
                .lines()
                .nth(line.saturating_sub(1))
                .unwrap_or("")
                .to_string()
        };
        if !text.is_empty() {
            let select_tab = message
                .get("selectTab")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            store().send_to_tab = Some(SendToTab {
                tab: target.to_string(),
                text,
                select_tab,
            });
            let _ = push_state();
        }
    }
    Ok(())
}

/// `log-to-sql-convert`: the tab's own input pane converts the pasted log text;
/// the list's selection plays no part.
pub(super) fn log_to_sql_convert(message: &Value) -> Result<(), Error> {
    let text = message.get("text").and_then(Value::as_str).unwrap_or("");
    run_log_to_sql(text);
    let _ = push_state();
    Ok(())
}

/// `log-to-sql-clear`: the input pane belongs to the page (it clears that field
/// itself); this clears the result and the status line.
pub(super) fn log_to_sql_clear() -> Result<(), Error> {
    clear_log_to_sql();
    let _ = push_state();
    Ok(())
}

/// `log-to-sql-read-clipboard`: fills the input pane from the clipboard and
/// converts at once.
pub(super) fn log_to_sql_read_clipboard() -> Result<(), Error> {
    read_log_clipboard();
    let _ = push_state();
    Ok(())
}

/// `sql-to-java-convert`: the input and the option row come from the page, the
/// conversion is the `sqlv_reverse_sql` handler.
pub(super) fn sql_to_java_convert(message: &Value) -> Result<(), Error> {
    // `text` is the page's field; `sql` stays accepted for direct calls.
    let sql = message
        .get("text")
        .or_else(|| message.get("sql"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let entity = message
        .get("entity")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let format = normalize_wrapper_format(
        message
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or("lambda"),
    );
    let options = SqlToJavaOptions {
        format,
        entity,
        caller: message
            .get("caller")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string(),
        return_type: message
            .get("returnType")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string(),
    };
    run_sql_to_java(sql, options);
    let _ = push_state();
    Ok(())
}

/// `sql-to-java-format`: formats the input pane's SQL for the configured
/// dialect; the page replaces its editor once (`replaceInput`).
pub(super) fn sql_to_java_format(message: &Value) -> Result<(), Error> {
    let sql = message
        .get("text")
        .or_else(|| message.get("sql"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if !sql.trim().is_empty() {
        format_sql_input(sql);
        let _ = push_state();
    }
    Ok(())
}

/// `sql-to-java-clear`: clears the result and the status line.
pub(super) fn sql_to_java_clear() -> Result<(), Error> {
    clear_sql_to_java();
    let _ = push_state();
    Ok(())
}

/// `copy-text`: copy from either tab's result pane.
///
/// The page owns the (editable) text, so it sends exactly what the user sees;
/// the host has no access to the browser clipboard. `tab` picks the status line
/// that confirms.
pub(super) fn copy_text(message: &Value) -> Result<(), Error> {
    let text = message.get("text").and_then(Value::as_str).unwrap_or("");
    if !text.is_empty() {
        host::copy_to_clipboard(text)?;
        let tab = message.get("tab").and_then(Value::as_str).unwrap_or("");
        let key = match tab {
            "sqlToJava" => "view.logs.tab.sql_to_java.copied",
            "logToSql" => "view.logs.tab.log_to_sql.copied",
            _ => "",
        };
        if !key.is_empty() {
            let mut state = store();
            if tab == "sqlToJava" {
                state.sql_to_java.status = t(key);
                state.sql_to_java.status_kind = "info".to_string();
            } else {
                state.log_to_sql.status = t(key);
                state.log_to_sql.status_kind = "info".to_string();
            }
            drop(state);
            let _ = push_state();
        }
    }
    Ok(())
}

/// `preview-*`: the fifth tab's ops.
///
/// The page prefixes its ops so they cannot collide with this panel's own
/// `clear` / `export` / `copy`; the model and the op handling stay in
/// `views::workbench`, the push is this panel's state.
pub(super) fn preview_message(message: &Value) -> Result<(), Error> {
    crate::views::workbench::handle_preview_message(message)?;
    let _ = push_state();
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    use crate::commands::common;
    use crate::views::log_panel::test_support::{
        MIXED_LOG, handle_panel_message, state_of, test_host,
    };
    use crate::views::log_panel::{set_log_text, snapshot};

    /// Two MyBatis groups; the second has no `Parameters:` row.
    const MULTI_LOG: &str = "\
Preparing: SELECT id FROM users WHERE id = ?
Parameters: 1(Long)
Preparing: DELETE FROM users WHERE id = ?
";

    fn send(host: &MockHost, message: Value) {
        handle_panel_message(host, &message.to_string()).unwrap();
    }

    fn flat(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn log_to_sql_converts_every_group_with_headers() {
        let (host, _guard) = test_host();
        send(
            &host,
            json!({
                "op": "log-to-sql-convert",
                "text": "Preparing: SELECT id FROM users WHERE id = ?\nParameters: 1(Long)\nPreparing: DELETE FROM users WHERE id = ?\nParameters: 2(Long)\n",
            }),
        );
        let tab = state_of(&host)["tabs"]["logToSql"].clone();
        assert_eq!(tab["statusKind"], "info", "{tab}");
        let output = tab["output"].as_str().unwrap_or_default();
        assert!(output.contains("-- SQL #1"), "{output}");
        assert!(output.contains("-- SQL #2"), "{output}");
        let flat_output = flat(output);
        assert!(
            flat_output.contains("SELECT id FROM users WHERE id = 1"),
            "{output}"
        );
        assert!(
            flat_output.contains("DELETE FROM users WHERE id = 2"),
            "{output}"
        );
        assert_eq!(
            tab["status"],
            json!(t("view.logs.tab.log_to_sql.converted").replace("{0}", "2"))
        );
    }

    #[test]
    fn log_to_sql_counts_statements_that_still_hold_placeholders() {
        let (host, _guard) = test_host();
        send(
            &host,
            json!({"op": "log-to-sql-convert", "text": MULTI_LOG}),
        );
        let tab = state_of(&host)["tabs"]["logToSql"].clone();
        // The second statement's `?` has no parameter: red status, count 1 —
        // and the statements that did convert are still shown.
        assert_eq!(tab["statusKind"], "error", "{tab}");
        assert_eq!(
            tab["status"],
            json!(t("view.logs.tab.log_to_sql.unresolved").replace("{0}", "1"))
        );
        let output = tab["output"].as_str().unwrap_or_default();
        assert!(output.contains("-- SQL #2"), "{output}");
        assert!(flat(output).contains("WHERE id = 1"), "{output}");
    }

    #[test]
    fn log_to_sql_empty_and_unparsed_input_are_errors() {
        let (host, _guard) = test_host();
        send(&host, json!({"op": "log-to-sql-convert", "text": "   "}));
        let tab = state_of(&host)["tabs"]["logToSql"].clone();
        assert_eq!(tab["statusKind"], "error");
        assert_eq!(tab["status"], json!(t("view.logs.tab.log_to_sql.empty")));
        assert_eq!(tab["output"], "");

        send(
            &host,
            json!({"op": "log-to-sql-convert", "text": "nothing to parse here"}),
        );
        let tab = state_of(&host)["tabs"]["logToSql"].clone();
        assert_eq!(tab["status"], json!(t("view.logs.tab.log_to_sql.no_entry")));
        assert_eq!(tab["output"], "");
    }

    #[test]
    fn log_to_sql_clear_resets_output_and_status() {
        let (host, _guard) = test_host();
        send(
            &host,
            json!({"op": "log-to-sql-convert", "text": MULTI_LOG}),
        );
        handle_panel_message(&host, "{\"op\":\"log-to-sql-clear\"}").unwrap();
        let tab = state_of(&host)["tabs"]["logToSql"].clone();
        assert_eq!(tab["output"], "");
        assert_eq!(tab["status"], "");
        assert_eq!(tab["statusKind"], "");
    }

    #[test]
    fn log_to_sql_clipboard_fills_the_input_and_converts() {
        let (host, _guard) = test_host();
        host.set_clipboard(
            Some("Preparing: SELECT id FROM users WHERE id = ?\nParameters: 7(Long)\n"),
            None,
        );
        handle_panel_message(&host, "{\"op\":\"log-to-sql-read-clipboard\"}").unwrap();
        let tab = state_of(&host)["tabs"]["logToSql"].clone();
        assert_eq!(tab["replaceInput"], true, "{tab}");
        assert!(
            tab["input"]
                .as_str()
                .is_some_and(|text| text.contains("Preparing")),
            "{tab}"
        );
        assert!(
            flat(tab["output"].as_str().unwrap_or_default()).contains("WHERE id = 7"),
            "{tab}"
        );
        // `replaceInput` is one-shot: the next push must not refill the pane.
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        assert_eq!(state_of(&host)["tabs"]["logToSql"]["replaceInput"], false);
    }

    #[test]
    fn log_to_sql_clipboard_without_text_says_so() {
        let (host, _guard) = test_host();
        host.set_clipboard(None, None);
        handle_panel_message(&host, "{\"op\":\"log-to-sql-read-clipboard\"}").unwrap();
        let tab = state_of(&host)["tabs"]["logToSql"].clone();
        assert_eq!(tab["statusKind"], "error");
        assert_eq!(
            tab["status"],
            json!(t("view.logs.tab.log_to_sql.clipboard_empty"))
        );
    }

    #[test]
    fn placeholder_counting_skips_literals_and_comments() {
        assert_eq!(
            count_placeholders("SELECT * FROM t WHERE a = ? AND b = ?"),
            2
        );
        assert_eq!(count_placeholders("SELECT '?' AS q FROM t WHERE a = ?"), 1);
        assert_eq!(count_placeholders("SELECT \"?\" FROM t"), 0);
        assert_eq!(count_placeholders("SELECT * FROM t -- ?\nWHERE a = ?"), 1);
        assert_eq!(count_placeholders("SELECT * FROM t /* ? */ WHERE a = ?"), 1);
        assert_eq!(count_placeholders("SELECT 'it''s ?' FROM t"), 0);
    }

    #[test]
    fn one_statement_gets_no_group_header() {
        let (_host, _guard) = test_host();
        let out =
            convert_log_text("Preparing: SELECT id FROM users WHERE id = ?\nParameters: 1(Long)\n");
        assert_eq!(out.groups, 1);
        assert_eq!(out.unresolved, 0);
        assert!(!out.output.contains("-- SQL #1"), "{}", out.output);
        assert!(out.output.trim_end().ends_with(';'), "{}", out.output);
    }

    #[test]
    fn sql_to_java_converts_exact_and_reports_unresolved() {
        let (host, _guard) = test_host();
        send(
            &host,
            json!({
                "op": "sql-to-java-convert",
                "text": "SELECT id, name FROM user WHERE id = ?",
                "format": "lambda",
                "entity": "User",
            }),
        );
        let tab = state_of(&host)["tabs"]["sqlToJava"].clone();
        assert_eq!(tab["statusKind"], "info", "{tab}");
        assert!(
            tab["code"]
                .as_str()
                .is_some_and(|code| code.contains("LambdaQueryWrapper<User>")),
            "{tab}"
        );
        assert_eq!(tab["options"]["entity"], "User");

        // No entity name: the generator refuses, the tab shows its reason.
        send(
            &host,
            json!({
                "op": "sql-to-java-convert",
                "text": "SELECT id, name FROM user WHERE id = ?",
                "entity": "",
            }),
        );
        let tab = state_of(&host)["tabs"]["sqlToJava"].clone();
        assert_eq!(tab["statusKind"], "error");
        assert_eq!(tab["code"], "", "unresolved ships no code");
        assert_eq!(
            tab["status"],
            json!(t("gen.java.entity_required")),
            "the status is the translated text, not the raw key: {tab}"
        );
    }

    #[test]
    fn sql_to_java_echoes_the_option_row_and_keeps_it_after_clear() {
        let (host, _guard) = test_host();
        send(
            &host,
            json!({
                "op": "sql-to-java-convert",
                "text": "SELECT id FROM user WHERE id = ?",
                "entity": "User",
                "format": "stringColumns",
                "caller": "UserMapper",
                "returnType": "List<User>",
            }),
        );
        let tab = state_of(&host)["tabs"]["sqlToJava"].clone();
        assert_eq!(tab["options"]["format"], "stringColumns");
        assert_eq!(tab["options"]["caller"], "UserMapper");
        assert_eq!(tab["options"]["returnType"], "List<User>");

        handle_panel_message(&host, "{\"op\":\"sql-to-java-clear\"}").unwrap();
        let tab = state_of(&host)["tabs"]["sqlToJava"].clone();
        assert_eq!(tab["code"], "");
        assert_eq!(tab["status"], "");
        assert_eq!(
            tab["options"]["entity"], "User",
            "the option row is not a result: a clear keeps it"
        );
    }

    #[test]
    fn sql_to_java_format_rewrites_the_input_once() {
        let (host, _guard) = test_host();
        let original = "select id,name from user where id=1";
        send(&host, json!({"op": "sql-to-java-format", "text": original}));
        let tab = state_of(&host)["tabs"]["sqlToJava"].clone();
        assert_eq!(tab["replaceInput"], true, "{tab}");
        let formatted = tab["input"].as_str().unwrap_or_default();
        assert_ne!(formatted, original, "{tab}");
        assert!(
            formatted.contains('\n'),
            "the formatter breaks clauses: {formatted}"
        );
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        assert_eq!(state_of(&host)["tabs"]["sqlToJava"]["replaceInput"], false);
    }

    #[test]
    fn copy_text_reaches_the_clipboard_and_confirms_in_that_tab() {
        let (host, _guard) = test_host();
        send(
            &host,
            json!({"op": "copy-text", "tab": "logToSql", "text": "SELECT 1;"}),
        );
        assert_eq!(host.clipboard().as_deref(), Some("SELECT 1;"));
        let tab = state_of(&host)["tabs"]["logToSql"].clone();
        assert_eq!(tab["status"], json!(t("view.logs.tab.log_to_sql.copied")));
        assert_eq!(tab["statusKind"], "info");

        send(
            &host,
            json!({"op": "copy-text", "tab": "sqlToJava", "text": "wrapper.eq(...)"}),
        );
        assert_eq!(host.clipboard().as_deref(), Some("wrapper.eq(...)"));
        let tab = state_of(&host)["tabs"]["sqlToJava"].clone();
        assert_eq!(tab["status"], json!(t("view.logs.tab.sql_to_java.copied")));
    }

    #[test]
    fn send_to_tab_hands_the_row_text_to_the_input_tab() {
        // Lane L32: "发送到 SQL→Java" hands the statement over, "发送到 日志→SQL"
        // the original log line; the tab that received it comes forward and the
        // hand-off clears once the page reports the tab it shows.
        let (host, _guard) = test_host();
        set_log_text(MIXED_LOG);
        let line = snapshot()
            .entries
            .iter()
            .find(|entry| entry.has_sql())
            .unwrap()
            .line_no;
        handle_panel_message(
            &host,
            &format!("{{\"op\":\"send-to-tab\",\"tab\":\"sqlToJava\",\"line\":{line}}}"),
        )
        .unwrap();
        let state = state_of(&host);
        assert_eq!(state["sendToTab"]["tab"], "sqlToJava");
        assert_eq!(state["sendToTab"]["selectTab"], true);
        assert!(
            state["sendToTab"]["text"]
                .as_str()
                .is_some_and(|text| text.contains("SELECT") || text.contains("DELETE")),
            "{:?}",
            state["sendToTab"]
        );

        handle_panel_message(
            &host,
            &format!("{{\"op\":\"send-to-tab\",\"tab\":\"logToSql\",\"line\":{line}}}"),
        )
        .unwrap();
        let state = state_of(&host);
        assert_eq!(state["sendToTab"]["tab"], "logToSql");
        assert!(
            state["sendToTab"]["text"]
                .as_str()
                .is_some_and(|text| text.contains("Preparing")),
            "the raw log line, not the SQL: {:?}",
            state["sendToTab"]
        );

        handle_panel_message(&host, "{\"op\":\"tab\"}").unwrap();
        assert!(
            store().send_to_tab.is_none(),
            "hand-off is one-shot: the page reporting its tab consumes it"
        );
    }

    #[test]
    fn a_preview_render_message_reaches_the_workbench_model_and_the_state() {
        let (host, _guard) = test_host();
        handle_panel_message(
            &host,
            "{\"op\":\"preview-render\",\"text\":\"select 7\",\"kind\":\"sql\"}",
        )
        .unwrap();
        let preview = &state_of(&host)["tabs"]["preview"];
        assert_eq!(preview["model"]["sql"], "select 7");
        assert_eq!(preview["model"]["confidence"], "exact");
        assert_eq!(
            preview["replaceInput"], true,
            "the input box follows the render"
        );
        assert_eq!(
            crate::views::workbench::model().sql,
            "select 7",
            "the host-side model is the one the ops use"
        );
    }

    #[test]
    fn a_preview_copy_message_reaches_the_clipboard_and_the_state() {
        let (host, _guard) = test_host();
        handle_panel_message(
            &host,
            "{\"op\":\"preview-render\",\"text\":\"select 8\",\"kind\":\"sql\"}",
        )
        .unwrap();
        handle_panel_message(&host, "{\"op\":\"preview-copy\"}").unwrap();
        assert_eq!(host.clipboard().as_deref(), Some("select 8"));
        assert_eq!(
            state_of(&host)["tabs"]["preview"]["status"]["text"],
            t("view.workbench.copied")
        );
        assert_eq!(
            state_of(&host)["tabs"]["preview"]["replaceInput"],
            false,
            "a copy leaves the input box alone"
        );
    }

    #[test]
    fn a_preview_export_message_writes_the_output_channel() {
        let (host, _guard) = test_host();
        handle_panel_message(
            &host,
            "{\"op\":\"preview-render\",\"text\":\"select 9\",\"kind\":\"sql\"}",
        )
        .unwrap();
        handle_panel_message(&host, "{\"op\":\"preview-export\"}").unwrap();
        let out = &host.output()[common::CHANNEL];
        assert!(out.contains("mybatis-sql workbench export"), "{out}");
        // The body is the statement formatted for the dialect, so the
        // whitespace differs from what was typed.
        let compact = out.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(compact.to_lowercase().contains("select 9"), "{out}");
    }
}
