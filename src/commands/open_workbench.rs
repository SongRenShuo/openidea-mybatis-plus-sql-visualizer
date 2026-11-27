//! `open-workbench`: send a statement to the preview and bring it to the front.
//!
//! Since lane L23 the preview workbench also lives as the fifth tab of the SQL
//! log panel (`sqlv-logs`), exactly as the IntelliJ edition's "SQL 预览" tab
//! lives inside the SQL Log tool window. Lane L48 added the modal dialog
//! (`sqlv-preview`, API 1.18), so this command sets the workbench model and
//! then opens whichever shell `preview.presentation` names: the dialog by
//! default, the fifth tab (`selectTab: "preview"` + reveal `sqlv-logs`) when
//! the user chose `tab`.
//!
//! The argument is optional SQL text, `@<path>`, or a bare project file path
//! (the form the project view context menu passes). The plugin-side model is
//! set too, because the page asks for the state again when it finishes loading
//! (`{op:"preview-ready"}` in the dialog, `{op:"ready"}` in the panel) — that
//! handshake would otherwise overwrite the load with an empty model.

use openidea_plugin::{Error, Invocation, host};

use crate::capture::bridge;
use crate::commands::common::{self, CliError};
use crate::views::{log_panel, workbench};

/// Matches `[[contributes.commands]] id` in `plugin.toml`.
pub const ID: &str = "open-workbench";

/// Dispatch entry. The optional argument is SQL text or `@<path>`; without it
/// the workbench opens empty.
pub fn run(ctx: &Invocation, arg: Option<&str>) -> Result<Option<String>, Error> {
    match open(ctx, arg) {
        // The panel is the surface; a successful hand-off must not also pop the
        // Output tab open. Failures keep going to Output.
        Ok(()) => Ok(None),
        Err(err) => common::fail(err),
    }
}

fn open(ctx: &Invocation, arg: Option<&str>) -> Result<(), CliError> {
    let (sql, source) = match arg.map(str::trim).filter(|text| !text.is_empty()) {
        Some(_) => common::sql_arg(ctx, arg)?,
        None => (String::new(), None),
    };
    let configured = common::configured_dialect();
    let dialect = if configured.trim().is_empty() {
        "MySQL".to_string()
    } else {
        configured
    };

    let mut model = workbench::WorkbenchModel::new(&sql, Vec::new(), &dialect);
    model.confidence = if sql.trim().is_empty() {
        String::new()
    } else {
        "exact".to_string()
    };
    if let Some(source) = &source {
        model.source = source.clone();
    }
    workbench::set_model(model);

    // The page renders the `state` shape of the log panel and shows the
    // statement in the preview tab's input pane. `request_tab` rides along as
    // `selectTab`, and stays set until the page reports the tab it shows (a
    // push sent before the page finished loading must not be the only chance
    // it gets).
    workbench::request_tab();
    workbench::request_input_replace();
    // Lane L48: the shell comes from `preview.presentation`. The dialog is a
    // view of its own, so it is revealed instead of the tool window; the
    // payload follows from `push_dialog` / the page's `preview-ready`
    // handshake, never from the panel's `state` push.
    match workbench::preview_target() {
        workbench::PreviewTarget::Dialog => {
            workbench::push_dialog()
                .map_err(|error| CliError::new(bridge::KEY_VIEW_UNAVAILABLE, error.to_string()))?;
            host::reveal_view(workbench::DIALOG_VIEW_ID)
                .map_err(|error| CliError::new(bridge::KEY_VIEW_UNAVAILABLE, error.to_string()))?;
        }
        workbench::PreviewTarget::Tab => {
            log_panel::push_state()
                .map_err(|error| CliError::new(bridge::KEY_VIEW_UNAVAILABLE, error.to_string()))?;
            host::reveal_view(log_panel::VIEW_ID)
                .map_err(|error| CliError::new(bridge::KEY_VIEW_UNAVAILABLE, error.to_string()))?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    use serde_json::{Value, json};

    /// Restores the process-wide workbench store, which the view tests share;
    /// the lock is the view tests' own, so the two modules never interleave.
    ///
    /// Every one-shot flag has to be restored, not just the model:
    /// `replace_input` is consumed by the next state push (`mem::take`), so a
    /// test that leaves it `false` makes the *next* test's assertion on
    /// `replaceInput` fail — which is exactly how this stayed green locally
    /// (fewer threads, friendlier order) and went red on CI.
    fn with_model_restore<T>(body: impl FnOnce() -> T) -> T {
        let _guard = workbench::test_store_lock();
        let previous = workbench::model();
        let flags = workbench::take_oneshot_flags();
        let out = body();
        workbench::set_model(previous);
        workbench::note_page_tab();
        workbench::restore_oneshot_flags(flags);
        out
    }

    /// The preview payload the command produced, whatever shell it went to.
    ///
    /// Lane L48: the dialog is the default and receives
    /// `{op:"preview-tab", tab}`; the tab shell receives the panel's whole
    /// `state`. Both carry the same `tabs.preview` object, so the tests read it
    /// through this one helper and stay shell-independent.
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

    #[test]
    fn commands_open_workbench_carries_the_statement_and_reveals_the_shell() {
        with_model_restore(|| {
            let host = MockHost::install();
            run(&host.invocation(), Some("select * from user")).unwrap();

            let value = preview_state(&host);
            assert_eq!(value["selectTab"], "preview");
            let preview = &value["tabs"]["preview"];
            assert_eq!(preview["model"]["sql"], "select * from user");
            assert_eq!(preview["model"]["params"], json!([]));
            assert!(
                preview["model"]["dialect"]
                    .as_str()
                    .is_some_and(|d| !d.is_empty())
            );
            assert_eq!(
                preview["replaceInput"], true,
                "the statement has to show up in the input box"
            );
            assert!(preview["labels"].is_object());
            assert_eq!(
                host.revealed_views(),
                [workbench::preview_target().view_id().to_string()]
            );
            assert!(!host.output().contains_key(common::CHANNEL));
        });
    }

    #[test]
    fn commands_open_workbench_reads_an_at_path() {
        with_model_restore(|| {
            let host = MockHost::install();
            host.add_file("/project/pick.sql", "SELECT id FROM user WHERE id = 1\n");
            run(&host.invocation(), Some("@/project/pick.sql")).unwrap();
            let value = preview_state(&host);
            let preview = &value["tabs"]["preview"]["model"];
            assert!(
                preview["sql"]
                    .as_str()
                    .is_some_and(|sql| sql.contains("SELECT id FROM user"))
            );
            assert!(
                preview["source"]
                    .as_str()
                    .is_some_and(|source| source == "/project/pick.sql")
            );
        });
    }

    #[test]
    fn commands_open_workbench_accepts_an_empty_argument() {
        with_model_restore(|| {
            let host = MockHost::install();
            run(&host.invocation(), None).unwrap();
            let value = preview_state(&host);
            let preview = &value["tabs"]["preview"]["model"];
            assert_eq!(preview["sql"], "");
            assert_eq!(preview["confidence"], "");
            assert_eq!(value["selectTab"], "preview", "the preview still opens");
        });
    }

    #[test]
    fn commands_open_workbench_keeps_the_tab_shell_when_the_setting_says_so() {
        // Lane L48: `preview.presentation = tab` restores the shell this build
        // shipped before API 1.18 — the panel's own `state` push and the tool
        // window brought forward.
        with_model_restore(|| {
            let host = MockHost::install();
            host.set_setting("preview.presentation", "\"tab\"");
            run(&host.invocation(), Some("select 1")).unwrap();

            let value = preview_state(&host);
            assert_eq!(value["op"], "state");
            assert_eq!(value["selectTab"], "preview");
            assert_eq!(value["tabs"]["preview"]["model"]["sql"], "select 1");
            assert_eq!(host.revealed_views(), [log_panel::VIEW_ID.to_string()]);
        });
    }

    #[test]
    fn commands_open_workbench_opens_the_dialog_by_default() {
        // Lane L48: the default shell is the modal dialog, so the statement goes
        // to `sqlv-preview` and the tool window stays where it is.
        with_model_restore(|| {
            let host = MockHost::install();
            run(&host.invocation(), Some("select 2")).unwrap();

            let messages = host.view_messages();
            assert_eq!(messages.len(), 1, "{messages:?}");
            assert_eq!(messages[0].0, workbench::DIALOG_VIEW_ID);
            let value: Value = serde_json::from_str(&messages[0].1).expect("valid JSON");
            assert_eq!(value["op"], "preview-tab");
            assert_eq!(value["tab"]["model"]["sql"], "select 2");
            assert_eq!(
                host.revealed_views(),
                [workbench::DIALOG_VIEW_ID.to_string()]
            );
        });
    }
}
