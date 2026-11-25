//! `capture-start`: open the agent's loopback WebSocket capture stream.
//!
//! The argument is the port (optional): it wins over the `capture.port` setting,
//! which wins over [`bridge::DEFAULT_PORT`]. Discovery is a known gap — the
//! agent abandons capture when `port <= 0` — so the output always carries the
//! line telling the user the agent needs `port=`.
//!
//! Thin shell: the session, the frame decoding and the refresh policy live in
//! [`crate::capture`]; this module only parses the port and formats the result.

use std::fmt::Write as _;

use openidea_plugin::{Error, Invocation, host};

use crate::capture::bridge::{self, BridgeError};
use crate::capture::ws::MAX_QUEUED_FRAMES;
use crate::commands::common::{self, CliError};

/// Matches `[[contributes.commands]] id` in `plugin.toml`.
pub const ID: &str = "capture-start";

/// Dispatch entry. The optional argument is the capture port; without it the
/// `capture.port` setting (default 17890) is used.
pub fn run(ctx: &Invocation, arg: Option<&str>) -> Result<Option<String>, Error> {
    match start(ctx, arg) {
        Ok(text) => {
            common::emit(&text)?;
            Ok(None)
        }
        Err(err) => common::fail(err),
    }
}

fn start(ctx: &Invocation, arg: Option<&str>) -> Result<String, CliError> {
    let setting = host::setting_number(bridge::PORT_SETTING).map(|value| value as i64);
    let port = bridge::resolve_port(arg, setting)
        .map_err(|error| CliError::new(error.key, error.detail))?;
    let project = common::project(ctx)?;
    let url = bridge::capture_url(port);
    // Subscribe before connecting: the first `websocket-ready` can arrive with
    // the very first frame, and an unsubscribed event is lost.
    let subscribed = bridge::ensure_websocket_subscription();
    let subscription = if subscribed {
        "subscribed"
    } else if bridge::websocket_subscription_active() {
        "already subscribed"
    } else {
        "unavailable (events denied?)"
    };

    let mut out = String::new();
    match bridge::connect(&project, &url) {
        Ok(socket) => {
            let _ = writeln!(
                out,
                "-- capture-start: connected {url} (socket {socket}, websocket-ready: {subscription})"
            );
            let _ = writeln!(
                out,
                "-- limits: {MAX_QUEUED_FRAMES} frames queued per poll, 8 connections per plugin"
            );
            let _ = writeln!(
                out,
                "-- agent: start the JVM with `port={port}`; the agent does not report on port 0 [{}]",
                bridge::KEY_GUIDANCE
            );
            Ok(out)
        }
        Err(BridgeError::AlreadyConnected) => {
            let stats = bridge::Stats::current();
            let _ = writeln!(
                out,
                "-- capture-start: already connected on port {} [{}]",
                stats.port.unwrap_or(port),
                bridge::KEY_ALREADY_CONNECTED
            );
            Ok(out)
        }
        Err(error) => Err(CliError::new(error.message_key(), error.detail())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    #[test]
    fn commands_capture_start_rejects_an_invalid_port() {
        let host = MockHost::install();
        let _guard = crate::capture::bridge::test_guard();
        let err = run(&host.invocation(), Some("not-a-port")).unwrap_err();
        assert!(
            matches!(err, Error::Failed(ref text) if text.contains(bridge::KEY_BAD_PORT)),
            "{err:?}"
        );
        assert!(
            host.output()[common::CHANNEL].contains(bridge::KEY_BAD_PORT),
            "the Output tab explains the rejected port"
        );
    }

    #[test]
    fn commands_capture_start_uses_the_setting_when_no_argument_is_given() {
        let host = MockHost::install();
        let _guard = crate::capture::bridge::test_guard();
        host.set_setting(bridge::PORT_SETTING, "17891");
        let err = run(&host.invocation(), None).unwrap_err();
        let text = err.to_string();
        assert!(!text.contains(bridge::KEY_BAD_PORT), "{text}");
    }
}
