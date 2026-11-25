//! `open-logs`: reveal the SQL Logs panel and re-render it.
//!
//! The panel may be hidden or showing stale rows; this command is the explicit
//! "show me the logs now" entry, used after a file is loaded or a capture starts.

use std::fmt::Write as _;

use openidea_plugin::{Error, Invocation, host};

use crate::capture::bridge;
use crate::commands::common::{self, CliError};
use crate::views::log_panel;

/// Matches `[[contributes.commands]] id` in `plugin.toml`.
pub const ID: &str = "open-logs";

/// Dispatch entry.
pub fn run(_ctx: &Invocation, _arg: Option<&str>) -> Result<Option<String>, Error> {
    match open() {
        Ok(text) => {
            common::emit(&text)?;
            Ok(None)
        }
        Err(err) => common::fail(err),
    }
}

fn open() -> Result<String, CliError> {
    host::reveal_view(log_panel::VIEW_ID)
        .map_err(|error| CliError::new(bridge::KEY_VIEW_UNAVAILABLE, error.to_string()))?;
    // The panel body is a packaged page; a `refresh_view` alone does not reach
    // it, so the state goes over the view message channel.
    log_panel::push_state()
        .map_err(|error| CliError::new(bridge::KEY_VIEW_UNAVAILABLE, error.to_string()))?;
    host::refresh_view(log_panel::VIEW_ID);
    let mut out = String::new();
    let _ = writeln!(
        out,
        "-- open-logs: revealed {} (buffered: {} chars)",
        log_panel::VIEW_ID,
        log_panel::buffered_len()
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    #[test]
    fn commands_open_logs_reveals_and_refreshes_the_panel() {
        let host = MockHost::install();
        run(&host.invocation(), None).unwrap();
        assert_eq!(host.revealed_views(), [log_panel::VIEW_ID.to_string()]);
        assert_eq!(host.refreshed_views(), [log_panel::VIEW_ID.to_string()]);
        // The packaged page only renders what it is sent.
        let pushed = host.view_messages();
        assert_eq!(pushed.len(), 1, "{pushed:?}");
        let state: serde_json::Value = serde_json::from_str(&pushed[0].1).expect("valid JSON");
        assert_eq!(state["op"], "state");
        assert!(
            host.output()[common::CHANNEL].contains("-- open-logs: revealed sqlv-logs"),
            "{}",
            &host.output()[common::CHANNEL]
        );
    }
}
