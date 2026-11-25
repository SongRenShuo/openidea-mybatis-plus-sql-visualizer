//! `capture-stop`: ask the agent to stop, close the socket and print statistics.
//!
//! Stopping when nothing is open is a readable no-op, not an error: the command
//! is also used from a toolbar button.

use std::fmt::Write as _;

use openidea_plugin::{Error, Invocation};

use crate::capture::bridge::{self, BridgeError};
use crate::commands::common::{self, CliError};

/// Matches `[[contributes.commands]] id` in `plugin.toml`.
pub const ID: &str = "capture-stop";

pub const KEY_STOP_NOT_CONNECTED: &str = "capture.stop.not_connected";

/// Dispatch entry.
pub fn run(_ctx: &Invocation, _arg: Option<&str>) -> Result<Option<String>, Error> {
    match bridge::disconnect() {
        Ok(stats) => {
            let mut out = String::new();
            let _ = writeln!(
                out,
                "-- capture-stop: closed; frames={} events={} bad={} dropped={}",
                stats.frames, stats.events, stats.bad_frames, stats.dropped
            );
            if let Some(error) = stats.last_error {
                let _ = writeln!(out, "-- last error: {error}");
            }
            common::emit(&out)?;
            Ok(None)
        }
        Err(BridgeError::NotConnected) => {
            common::emit(&format!(
                "-- capture-stop: no capture WebSocket is open [{KEY_STOP_NOT_CONNECTED}]\n"
            ))?;
            Ok(None)
        }
        Err(error) => common::fail(CliError::new(error.message_key(), error.detail())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::bridge::test_guard;
    use openidea_plugin::mock::MockHost;

    #[test]
    fn commands_capture_stop_without_a_session_is_readable() {
        let host = MockHost::install();
        let _guard = test_guard();
        run(&host.invocation(), None).unwrap();
        let text = &host.output()[common::CHANNEL];
        assert!(text.contains(KEY_STOP_NOT_CONNECTED), "{text}");
        assert!(text.contains("-- capture-stop:"), "{text}");
    }
}
