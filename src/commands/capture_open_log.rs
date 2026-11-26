//! `capture-open-log`: read an exported console log into the SQL Logs panel.
//!
//! This is the no-agent path (T4-5): the platform cannot read a run console, so
//! the user exports it and points this command at the file. The whole text goes
//! through [`crate::core::log::parse`] once for the summary and through
//! [`views::log_panel::set_log_text`] so the panel owns the rows, filtering and
//! pagination (the panel re-parses lazily, exactly as it does for live frames).
//!
//! The command does not fail when the file has no SQL: it says so with a key.

use std::fmt::Write as _;

use openidea_plugin::{Error, Invocation, host};

use crate::capture::bridge;
use crate::commands::common::{self, CliError};
use crate::core::log::parse;
use crate::views::log_panel;

/// Matches `[[contributes.commands]] id` in `plugin.toml`.
pub const ID: &str = "capture-open-log";

/// What the command accepts as `arg`.
pub const USAGE: &str = "<log path> | @<log path>";

/// Dispatch entry.
pub fn run(ctx: &Invocation, arg: Option<&str>) -> Result<Option<String>, Error> {
    if arg.map(str::trim).unwrap_or("").is_empty() {
        return common::fail(common::missing_arg(USAGE));
    }
    match open_log(ctx, arg) {
        Ok(text) => {
            common::emit(&text)?;
            Ok(None)
        }
        Err(err) => common::fail(err),
    }
}

fn open_log(ctx: &Invocation, arg: Option<&str>) -> Result<String, CliError> {
    let arg = common::required_arg(arg)?;
    let path = arg.strip_prefix('@').unwrap_or(arg).trim();
    if path.is_empty() {
        return Err(CliError::new(common::KEY_EMPTY_ARG, "no log file path"));
    }
    let text = common::read_file(ctx, path)?;
    let analysis = parse(&text, &bridge::parse_options());

    log_panel::set_log_text(&text);
    // A loaded log becomes a history file (the reference's auto-saver); the
    // file stem names it so the history list is readable.
    let stem = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let stem = stem.strip_suffix(".log").unwrap_or(stem);
    log_panel::history_capture(ctx, stem);
    host::reveal_view(log_panel::VIEW_ID)
        .map_err(|error| CliError::new(bridge::KEY_VIEW_UNAVAILABLE, error.to_string()))?;
    // The body is a packaged page: the host never re-renders it, so push the new
    // state explicitly (a freshly loaded page asks for it again with `ready`).
    log_panel::push_state()
        .map_err(|error| CliError::new(bridge::KEY_VIEW_UNAVAILABLE, error.to_string()))?;
    // The panel chrome (status line) is host-drawn; ask for one refresh.
    host::refresh_view(log_panel::VIEW_ID);

    let page = bridge::panel_page_size();
    let mut out = String::new();
    let _ = writeln!(
        out,
        "-- capture-open-log: {path} (lines: {}, entries: {}, unparsed: {})",
        text.lines().count(),
        analysis.entries.len(),
        analysis.unparsed_lines.len()
    );
    if analysis.entries.is_empty() {
        let _ = writeln!(
            out,
            "-- no SQL entries found [{}]",
            bridge::KEY_OPEN_LOG_NO_ENTRIES
        );
    } else if analysis.entries.len() > page {
        let _ = writeln!(
            out,
            "-- {} entries; the panel buffer is capped at `logs.pageSize` ({page}) [{}]",
            analysis.entries.len(),
            bridge::KEY_OPEN_LOG_LIMITED
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    const LOG: &str = "\
Preparing: SELECT id, name FROM user WHERE id = ?
Parameters: 1(Long)
Preparing: DELETE FROM user WHERE id = ?
Parameters: 2(Long)
";

    /// The panel's row store is process-wide: `views::log_panel` tests hold the
    /// same lock, so this test never sees their rows and they never see ours.
    fn host_with_store_lock() -> (MockHost, std::sync::MutexGuard<'static, ()>) {
        let guard = crate::views::test_state_lock();
        (MockHost::install(), guard)
    }

    #[test]
    fn commands_capture_open_log_parses_a_file_and_reveals_the_panel() {
        let (host, _guard) = host_with_store_lock();
        host.add_file("/project/app.log", LOG);
        run(&host.invocation(), Some("/project/app.log")).unwrap();
        let text = &host.output()[common::CHANNEL];
        assert!(
            text.contains("-- capture-open-log: /project/app.log"),
            "{text}"
        );
        assert!(text.contains("entries: 2"), "{text}");
        assert!(text.contains("unparsed: 0"), "{text}");
        assert!(!text.contains(bridge::KEY_OPEN_LOG_LIMITED), "{text}");
        assert_eq!(host.revealed_views(), [log_panel::VIEW_ID.to_string()]);
        assert!(
            host.refreshed_views()
                .iter()
                .any(|view| view == log_panel::VIEW_ID),
            "an already-open panel must be refreshed, not only revealed"
        );
        // The page only renders what it is sent: the new rows must be pushed.
        let pushed = host.view_messages();
        assert_eq!(pushed.len(), 1, "{pushed:?}");
        assert_eq!(pushed[0].0, log_panel::VIEW_ID);
        let state: serde_json::Value = serde_json::from_str(&pushed[0].1).expect("valid JSON");
        assert_eq!(state["op"], "state");
        assert_eq!(state["rows"].as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn commands_capture_open_log_accepts_the_at_form() {
        let (host, _guard) = host_with_store_lock();
        host.add_file("/project/app.log", LOG);
        run(&host.invocation(), Some("@/project/app.log")).unwrap();
        assert!(host.output()[common::CHANNEL].contains("entries: 2"));
    }

    #[test]
    fn commands_capture_open_log_says_when_only_the_first_page_is_shown() {
        let (host, _guard) = host_with_store_lock();
        host.set_setting(bridge::PAGE_SIZE_SETTING, "1");
        host.add_file("/project/app.log", LOG);
        run(&host.invocation(), Some("/project/app.log")).unwrap();
        let text = &host.output()[common::CHANNEL];
        assert!(text.contains(bridge::KEY_OPEN_LOG_LIMITED), "{text}");
        assert!(text.contains("2 entries"), "{text}");
        assert!(text.contains("`logs.pageSize` (1)"), "{text}");
    }

    #[test]
    fn commands_capture_open_log_reports_a_file_without_sql() {
        let (host, _guard) = host_with_store_lock();
        host.add_file("/project/business.log", "nothing to see here\n");
        run(&host.invocation(), Some("/project/business.log")).unwrap();
        assert!(
            host.output()[common::CHANNEL].contains(bridge::KEY_OPEN_LOG_NO_ENTRIES),
            "{}",
            &host.output()[common::CHANNEL]
        );
    }

    #[test]
    fn commands_capture_open_log_reports_a_missing_file() {
        let (host, _guard) = host_with_store_lock();
        let err = run(&host.invocation(), Some("/project/nope.log")).unwrap_err();
        assert!(
            matches!(err, Error::Failed(ref text) if text.contains(common::KEY_FILE_UNREADABLE)),
            "{err:?}"
        );
        assert!(
            host.output()[common::CHANNEL].contains(common::KEY_FILE_UNREADABLE),
            "the Output tab explains the failure"
        );
    }
}
