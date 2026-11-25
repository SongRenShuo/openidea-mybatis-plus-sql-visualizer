//! `open-workbench`: send a statement to the preview workbench and reveal it.
//!
//! The argument is optional SQL text, `@<path>`, or a bare project file path
//! (the form the project view context menu passes). The page receives the
//! `{op:"load", model, labels}` message through
//! [`host::post_view_message`](openidea_plugin::host::post_view_message); the
//! plugin-side model is set too, because the page asks for it again with
//! `{op:"ready"}` when it finishes loading (that handshake would otherwise
//! overwrite the load with an empty model).

use std::fmt::Write as _;

use openidea_plugin::{Error, Invocation, host};

use crate::capture::bridge;
use crate::commands::common::{self, CliError};
use crate::views::workbench;

/// Matches `[[contributes.commands]] id` in `plugin.toml`.
pub const ID: &str = "open-workbench";

/// Dispatch entry. The optional argument is SQL text or `@<path>`; without it
/// the workbench opens empty.
pub fn run(ctx: &Invocation, arg: Option<&str>) -> Result<Option<String>, Error> {
    match open(ctx, arg) {
        Ok(text) => {
            common::emit(&text)?;
            Ok(None)
        }
        Err(err) => common::fail(err),
    }
}

fn open(ctx: &Invocation, arg: Option<&str>) -> Result<String, CliError> {
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

    // The page renders the `state` shape and shows the statement in the input
    // pane; the legacy `{op:"load"}` is not understood by it any more.
    workbench::push_state_with("", true)
        .map_err(|error| CliError::new(bridge::KEY_VIEW_UNAVAILABLE, error.to_string()))?;
    host::reveal_view(workbench::VIEW_ID)
        .map_err(|error| CliError::new(bridge::KEY_VIEW_UNAVAILABLE, error.to_string()))?;

    let mut out = String::new();
    let _ = writeln!(
        out,
        "-- open-workbench: sent to {} (dialect: {}, sql: {} chars)",
        workbench::VIEW_ID,
        dialect,
        sql.chars().count()
    );
    if let Some(source) = source {
        let _ = writeln!(out, "-- source: {source}");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    use serde_json::{Value, json};

    /// Restores the process-wide workbench model, which the view tests share; the
    /// lock is the view tests' own, so the two modules never interleave.
    fn with_model_restore<T>(body: impl FnOnce() -> T) -> T {
        let _guard = workbench::test_store_lock();
        let previous = workbench::model();
        let out = body();
        workbench::set_model(previous);
        out
    }

    #[test]
    fn commands_open_workbench_posts_a_state_message_and_reveals_the_page() {
        with_model_restore(|| {
            let host = MockHost::install();
            run(&host.invocation(), Some("select * from user")).unwrap();

            let messages = host.view_messages();
            assert_eq!(messages.len(), 1, "{messages:?}");
            assert_eq!(messages[0].0, workbench::VIEW_ID);
            let value: Value = serde_json::from_str(&messages[0].1).expect("valid JSON");
            assert_eq!(value["op"], "state");
            assert_eq!(value["replaceInput"], true);
            assert_eq!(value["model"]["sql"], "select * from user");
            assert_eq!(value["model"]["params"], json!([]));
            assert!(
                value["model"]["dialect"]
                    .as_str()
                    .is_some_and(|d| !d.is_empty())
            );
            assert!(value["labels"].is_object());
            assert_eq!(host.revealed_views(), [workbench::VIEW_ID.to_string()]);
            assert!(
                host.output()[common::CHANNEL].contains("-- open-workbench: sent to"),
                "{}",
                &host.output()[common::CHANNEL]
            );
        });
    }

    #[test]
    fn commands_open_workbench_reads_an_at_path() {
        with_model_restore(|| {
            let host = MockHost::install();
            host.add_file("/project/pick.sql", "SELECT id FROM user WHERE id = 1\n");
            run(&host.invocation(), Some("@/project/pick.sql")).unwrap();
            let value: Value = serde_json::from_str(&host.view_messages()[0].1).unwrap();
            assert!(
                value["model"]["sql"]
                    .as_str()
                    .is_some_and(|sql| sql.contains("SELECT id FROM user"))
            );
            assert!(
                value["model"]["source"]
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
            let value: Value = serde_json::from_str(&host.view_messages()[0].1).unwrap();
            assert_eq!(value["model"]["sql"], "");
            assert_eq!(value["model"]["confidence"], "");
        });
    }
}
