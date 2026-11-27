//! `capture-start`: open the agent's loopback WebSocket capture stream.
//!
//! The argument is the port (optional): it wins over the `capture.port` setting,
//! which wins over [`bridge::DEFAULT_PORT`]. Discovery is a known gap — the
//! agent abandons capture when `port <= 0` — so the output always carries the
//! line telling the user the agent needs `port=`.
//!
//! A dial that is refused because the target application is not running yet is
//! **not** an error: the sandbox gives the plugin a WebSocket client only (the
//! reference owns a `ServerSocket` and the agent dials in), so the plugin must
//! dial out and the agent has to be up first. The click therefore records a
//! waiting state ([`bridge::Waiting`]) and the panel retries every 2 s
//! (`capture-retry`), which restores "click Capture, then start the app".
//!
//! Thin shell: the session, the frame decoding and the refresh policy live in
//! [`crate::capture`]; this module only parses the port and formats the result.

use std::fmt::Write as _;

use openidea_plugin::{Error, Invocation, host};

use crate::capture::bridge::{self, BridgeError};
use crate::capture::ws::MAX_QUEUED_FRAMES;
use crate::commands::common::{self, CliError};
use crate::i18n::t;

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
            bridge::clear_waiting();
            let _ = writeln!(
                out,
                "-- capture-start: {} [{}] (socket {socket}, websocket-ready: {subscription})",
                connected_text(port),
                bridge::KEY_CONNECTED,
            );
            let _ = writeln!(
                out,
                "-- limits: {} frames per drain, {MAX_QUEUED_FRAMES} staged, 8 connections per plugin",
                crate::capture::ws::HOST_DRAIN_LIMIT
            );
            let _ = writeln!(
                out,
                "-- agent: start the JVM with `port={port}`; the agent does not report on port 0 [{}]",
                bridge::KEY_GUIDANCE
            );
            Ok(out)
        }
        Err(BridgeError::AlreadyConnected) => {
            bridge::clear_waiting();
            let stats = bridge::Stats::current();
            let _ = writeln!(
                out,
                "-- capture-start: already connected on port {} [{}]",
                stats.port.unwrap_or(port),
                bridge::KEY_ALREADY_CONNECTED
            );
            Ok(out)
        }
        Err(error) => {
            // The target application is (probably) not running yet. Keep the
            // refusal as the waiting state's reason and let the panel retry.
            let waiting = bridge::mark_waiting(port, &url, &error);
            let _ = writeln!(
                out,
                "-- capture-start: {} [{}] ({})",
                waiting_text(port),
                bridge::KEY_WAITING,
                waiting.last_error,
            );
            let _ = writeln!(
                out,
                "-- agent: start the JVM with `port={port}`; the panel retries every 2 s [{}]",
                bridge::KEY_GUIDANCE
            );
            Ok(out)
        }
    }
}

/// The localized "waiting for the target application" sentence.
fn waiting_text(port: u16) -> String {
    t(bridge::KEY_WAITING).replace("{port}", &port.to_string())
}

/// The localized "connected" sentence.
fn connected_text(port: u16) -> String {
    t(bridge::KEY_CONNECTED).replace("{port}", &port.to_string())
}

/// One quiet reconnect for the panel's 2 s tick (`capture-retry`).
///
/// While the dial keeps being refused nothing is printed — the panel would
/// otherwise rewrite the Output tab twice a second; the attempt count and the
/// reason ride the panel state instead. A dial that succeeds prints the
/// "connected" line once, and a refusal retrying cannot fix (an invalid port)
/// clears the wait so the loop stops and the real reason is shown.
pub fn retry(ctx: &Invocation) -> Result<(), Error> {
    if bridge::waiting().is_none() {
        return Ok(());
    }
    let project = common::project(ctx)
        .map_err(|error| Error::Failed(format!("{}: {}", error.key, error.detail)))?;
    match bridge::retry(&project) {
        Ok(None) => {
            let port = bridge::Stats::current()
                .port
                .unwrap_or(bridge::DEFAULT_PORT);
            common::emit(&format!(
                "-- capture-start: {} [{}]\n",
                connected_text(port),
                bridge::KEY_CONNECTED,
            ))?;
        }
        // Still refused: the state carries the new attempt count and reason.
        Ok(Some(_)) => {}
        Err(error) => {
            common::emit(&format!(
                "-- capture-start: {} [{}]\n",
                error.detail(),
                error.message_key()
            ))?;
        }
    }
    Ok(())
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
        // The mock refuses the `ws://` dial, so this is the waiting path: the
        // command still succeeds and the port comes from the setting.
        let out = run(&host.invocation(), None).unwrap();
        assert!(out.is_none(), "the text goes to the Output channel");
        let text = &host.output()[common::CHANNEL];
        assert!(!text.contains(bridge::KEY_BAD_PORT), "{text}");
        let waiting = bridge::waiting().expect("a refused dial waits");
        assert_eq!(waiting.port, 17891, "the setting picked the port");
    }

    #[test]
    fn a_refused_dial_waits_instead_of_failing() {
        // The sandbox has no listening socket and the agent has to be up first:
        // a refused dial must not turn the click into an error.
        let host = MockHost::install();
        let _guard = crate::capture::bridge::test_guard();
        assert!(bridge::waiting().is_none(), "a clean baseline");

        let out = run(&host.invocation(), Some("17890")).unwrap();
        assert!(out.is_none());
        let text = &host.output()[common::CHANNEL];
        assert!(text.contains(bridge::KEY_WAITING), "{text}");
        assert!(
            text.contains(&t(bridge::KEY_WAITING).replace("{port}", "17890")),
            "the line names the port and the wait: {text}"
        );
        assert!(
            text.contains("17890)"),
            "the refusal reason sits in parentheses: {text}"
        );

        let waiting = bridge::waiting().expect("the wait is recorded");
        assert_eq!(waiting.port, 17890);
        assert_eq!(waiting.attempts, 1);
        assert!(!waiting.last_error.is_empty(), "the reason is kept");
        assert!(!waiting.last_key.is_empty(), "the key is kept");
    }

    #[test]
    fn retry_bumps_the_attempt_count_and_stays_quiet() {
        let host = MockHost::install();
        let _guard = crate::capture::bridge::test_guard();
        run(&host.invocation(), Some("17890")).unwrap();
        let before = host.output()[common::CHANNEL].clone();

        retry(&host.invocation()).unwrap();
        let waiting = bridge::waiting().expect("still waiting");
        assert_eq!(waiting.attempts, 2, "the tick counted a dial");
        assert_eq!(
            host.output()[common::CHANNEL],
            before,
            "a still-refused retry does not rewrite the Output tab"
        );
    }

    #[test]
    fn retry_without_a_pending_wait_is_a_no_op() {
        let host = MockHost::install();
        let _guard = crate::capture::bridge::test_guard();
        assert!(bridge::waiting().is_none());
        retry(&host.invocation()).unwrap();
        assert!(bridge::waiting().is_none());
    }

    #[test]
    fn disconnect_clears_the_wait() {
        let host = MockHost::install();
        let _guard = crate::capture::bridge::test_guard();
        run(&host.invocation(), Some("17890")).unwrap();
        assert!(bridge::waiting().is_some());
        let _ = bridge::disconnect();
        assert!(
            bridge::waiting().is_none(),
            "capture-stop stops the retry loop"
        );
    }
}
