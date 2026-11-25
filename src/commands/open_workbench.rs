//! `open-workbench`: send a statement to the preview workbench and reveal it.
//!
//! The argument is optional SQL text (or `@<path>` to read a file inside the
//! project). The page receives the `{op:"load", model, labels}` message through
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

/// What the command accepts as `arg`.
pub const USAGE: &str = "[sql text] | [@<path>]";

/// Dispatch entry.
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

    let payload = bridge::workbench_payload(&sql, &dialect, source.as_deref());
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

    host::post_view_message(workbench::VIEW_ID, &payload)
        .map_err(|error| CliError::new(bridge::KEY_VIEW_UNAVAILABLE, error.to_string()))?;
    host::reveal_view(workbench::VIEW_ID)
        .map_err(|error| CliError::new(bridge::KEY_VIEW_UNAVAILABLE, error.to_string()))?;

    let mut out = String::new();
    let _ = writeln!(
        out,
        "-- open-workbench: posted {} bytes to {} (dialect: {}, sql: {} chars)",
        payload.len(),
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

    /// Restores the process-wide workbench model, which the view tests share.
    fn with_model_restore<T>(body: impl FnOnce() -> T) -> T {
        let previous = workbench::model();
        let out = body();
        workbench::set_model(previous);
        out
    }

    #[test]
    fn commands_open_workbench_posts_a_load_message_and_reveals_the_page() {
        with_model_restore(|| {
            let host = MockHost::install();
            run(&host.invocation(), Some("select * from user")).unwrap();

            let messages = host.view_messages();
            assert_eq!(messages.len(), 1, "{messages:?}");
            assert_eq!(messages[0].0, workbench::VIEW_ID);
            let value: Value = serde_json::from_str(&messages[0].1).expect("valid JSON");
            assert_eq!(value["op"], "load");
            assert_eq!(value["model"]["sql"], "select * from user");
            assert_eq!(value["model"]["params"], json!([]));
            assert!(
                value["model"]["dialect"]
                    .as_str()
                    .is_some_and(|d| !d.is_empty())
            );
            assert!(value["labels"]["sql"].as_str().is_some());
            assert_eq!(host.revealed_views(), [workbench::VIEW_ID.to_string()]);
            assert!(
                host.output()[common::CHANNEL].contains("-- open-workbench: posted"),
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
