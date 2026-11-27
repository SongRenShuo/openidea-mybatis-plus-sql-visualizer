//! `capture-poll`: drain queued capture frames now.
//!
//! The `websocket-ready` event path calls the same [`bridge::poll`]; this
//! command exists for manual debugging and prints one statistics line.

use std::fmt::Write as _;

use openidea_plugin::{Error, Invocation};

use crate::capture::bridge::{self, BridgeError};
use crate::commands::common::{self, CliError};
use crate::views::log_panel;

/// Matches `[[contributes.commands]] id` in `plugin.toml`.
pub const ID: &str = "capture-poll";

/// Dispatch entry.
pub fn run(_ctx: &Invocation, _arg: Option<&str>) -> Result<Option<String>, Error> {
    match bridge::poll(log_panel::now_ms()) {
        Ok(report) => {
            let mut out = String::new();
            let _ = writeln!(
                out,
                "-- capture-poll: fetched={} events={} entries={} controls={} bad={} queued={}{}",
                report.fetched,
                report.events,
                report.entries,
                report.controls,
                report.bad_frames,
                report.queued,
                if report.closed { " closed" } else { "" }
            );
            let _ = writeln!(
                out,
                "-- panel: appended {} chars, refreshed: {}",
                report.appended,
                if report.refresh { "yes" } else { "throttled" }
            );
            // The agent's own counters. Without them "connected but nothing
            // captured" cannot be attributed: hooks that never attached, hooks
            // that attached but never fired, and a session the agent stopped on
            // its own all look alike from here.
            match report.health {
                Some(health) => {
                    let _ = writeln!(out, "-- agent: {}", health.summary());
                    if health.stopped_by_watchdog() {
                        let _ = writeln!(
                            out,
                            "-- agent: stopped itself, no CONFIG within 15 s [capture.agent.config_watchdog]"
                        );
                    }
                }
                None => {
                    let _ = writeln!(out, "-- agent: no heartbeat yet");
                }
            }
            common::emit(&out)?;
            Ok(None)
        }
        Err(BridgeError::NotConnected) => {
            common::emit(&format!(
                "-- capture-poll: not connected [{}]\n",
                bridge::KEY_POLL_NOT_CONNECTED
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
    fn commands_capture_poll_without_a_session_is_readable() {
        let host = MockHost::install();
        let _guard = test_guard();
        run(&host.invocation(), None).unwrap();
        let text = &host.output()[common::CHANNEL];
        assert!(text.contains(bridge::KEY_POLL_NOT_CONNECTED), "{text}");
    }

    #[test]
    fn commands_capture_poll_prints_the_frame_counts() {
        let host =
            MockHost::install().with_capabilities(&["project.read", "events", "net:127.0.0.1"]);
        let _guard = test_guard();
        let project = host.invocation().project().expect("project");
        let socket = bridge::connect(&project, "wss://127.0.0.1:17890/mpsv").expect("connect");
        let frame = serde_json::json!({
            "cmd": 1,
            "body": serde_json::json!({"sql": "select 1"}).to_string(),
        })
        .to_string();
        host.push_websocket_message(socket, frame.as_bytes())
            .unwrap();
        run(&host.invocation(), None).unwrap();
        let text = &host.output()[common::CHANNEL];
        assert!(text.contains("events=1"), "{text}");
        assert!(text.contains("entries=1"), "{text}");
        assert!(
            host.refreshed_views()
                .contains(&log_panel::VIEW_ID.to_string())
        );
    }
}
