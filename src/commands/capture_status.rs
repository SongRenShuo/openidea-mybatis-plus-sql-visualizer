//! `capture-status`: report the live capture state without touching the socket.
//!
//! When no agent is attached the command names the file path alternative
//! (`capture-open-log`) instead of pretending a connection exists.

use std::fmt::Write as _;

use openidea_plugin::{Error, Invocation};

use crate::capture::bridge;
use crate::commands::common;

/// Matches `[[contributes.commands]] id` in `plugin.toml`.
pub const ID: &str = "capture-status";

/// Dispatch entry.
pub fn run(_ctx: &Invocation, _arg: Option<&str>) -> Result<Option<String>, Error> {
    common::emit(&status_text(&bridge::Stats::current()))?;
    Ok(None)
}

fn status_text(stats: &bridge::Stats) -> String {
    let mut out = String::new();
    if stats.connected {
        let _ = writeln!(
            out,
            "-- capture-status: connected (port {}, socket {})",
            stats
                .port
                .map_or_else(|| "-".to_string(), |p| p.to_string()),
            stats
                .socket_id
                .map_or_else(|| "-".to_string(), |id| id.to_string())
        );
        let _ = writeln!(
            out,
            "-- url: {}",
            stats.url.as_deref().unwrap_or("(unknown)")
        );
        let _ = writeln!(
            out,
            "-- frames: {} (events {}), bad: {}, dropped: {}, queued: {}",
            stats.frames, stats.events, stats.bad_frames, stats.dropped, stats.queued
        );
        let _ = writeln!(
            out,
            "-- last error: {}",
            stats.last_error.as_deref().unwrap_or("none")
        );
        return out;
    }

    let _ = writeln!(
        out,
        "-- capture-status: not connected [{}]",
        bridge::KEY_NOT_CONNECTED
    );
    let _ = writeln!(
        out,
        "-- alternative: `capture-open-log <log path>` reads an exported console log [{}]",
        bridge::KEY_FILE_HINT
    );
    if stats.frames > 0 || stats.bad_frames > 0 {
        let _ = writeln!(
            out,
            "-- last session: frames={} events={} bad={} dropped={}",
            stats.frames, stats.events, stats.bad_frames, stats.dropped
        );
    }
    if let Some(error) = &stats.last_error {
        let _ = writeln!(out, "-- last error: {error}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::bridge::test_guard;
    use openidea_plugin::mock::MockHost;

    #[test]
    fn commands_capture_status_without_a_session_points_at_the_log_path() {
        let host = MockHost::install();
        let _guard = test_guard();
        run(&host.invocation(), None).unwrap();
        let text = &host.output()[common::CHANNEL];
        assert!(text.contains(bridge::KEY_NOT_CONNECTED), "{text}");
        assert!(text.contains(bridge::KEY_FILE_HINT), "{text}");
        assert!(text.contains("capture-open-log"), "{text}");
    }

    #[test]
    fn commands_capture_status_reports_a_live_session() {
        let host =
            MockHost::install().with_capabilities(&["project.read", "events", "net:127.0.0.1"]);
        let _guard = test_guard();
        let project = host.invocation().project().expect("project");
        let socket = bridge::connect(&project, "wss://127.0.0.1:17890/mpsv").expect("connect");
        run(&host.invocation(), None).unwrap();
        let text = &host.output()[common::CHANNEL];
        assert!(text.contains("connected (port 17890"), "{text}");
        assert!(text.contains(&format!("socket {socket}")), "{text}");
        assert!(text.contains("frames: 0"), "{text}");
        assert!(text.contains("last error: none"), "{text}");
    }
}
