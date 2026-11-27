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
pub const KEY_STOP_WAITING: &str = "capture.stop.waiting";

/// Dispatch entry.
pub fn run(_ctx: &Invocation, _arg: Option<&str>) -> Result<Option<String>, Error> {
    // `disconnect` clears the waiting state itself, so ask before calling it:
    // "nothing was open" and "you cancelled a start that was waiting for the
    // application" are different outcomes and deserve different text.
    let was_waiting = bridge::waiting().is_some();
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
        Err(BridgeError::NotConnected) if was_waiting => {
            common::emit(&format!(
                "-- capture-stop: {} [{KEY_STOP_WAITING}]\n",
                crate::i18n::t(KEY_STOP_WAITING)
            ))?;
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

    /// Stopping a capture that is still waiting for the target application is a
    /// different outcome from "nothing was ever open": the user did start it,
    /// so the text must say the wait was cancelled rather than imply a mistake.
    #[test]
    fn commands_capture_stop_cancels_a_waiting_start() {
        let host = MockHost::install();
        let _guard = test_guard();
        // A dial to a loopback port with no listener is what a start looks like
        // before the target application runs; the mock host refuses it the same
        // way the real one does.
        crate::commands::capture_start::run(&host.invocation(), Some("17890")).unwrap();
        assert!(
            bridge::waiting().is_some(),
            "the refused dial has to leave a waiting state for this to be the case under test"
        );
        run(&host.invocation(), None).unwrap();
        let text = &host.output()[common::CHANNEL];
        assert!(text.contains(KEY_STOP_WAITING), "{text}");
        assert!(
            !text.contains(KEY_STOP_NOT_CONNECTED),
            "the generic text would read like the user never started capture: {text}"
        );
        assert!(bridge::waiting().is_none(), "the wait must be cleared");
    }
}
