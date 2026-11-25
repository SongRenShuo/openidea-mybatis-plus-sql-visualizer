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
    host::reveal_view(log_panel::VIEW_ID)
        .map_err(|error| CliError::new(bridge::KEY_VIEW_UNAVAILABLE, error.to_string()))?;

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
            "-- {} entries; the panel shows one page at a time and `logs.pageSize` is {page} [{}]",
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

    #[test]
    fn commands_capture_open_log_parses_a_file_and_reveals_the_panel() {
        let host = MockHost::install();
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
    }

    #[test]
    fn commands_capture_open_log_accepts_the_at_form() {
        let host = MockHost::install();
        host.add_file("/project/app.log", LOG);
        run(&host.invocation(), Some("@/project/app.log")).unwrap();
        assert!(host.output()[common::CHANNEL].contains("entries: 2"));
    }

    #[test]
    fn commands_capture_open_log_says_when_only_the_first_page_is_shown() {
        let host = MockHost::install();
        host.set_setting(bridge::PAGE_SIZE_SETTING, "1");
        host.add_file("/project/app.log", LOG);
        run(&host.invocation(), Some("/project/app.log")).unwrap();
        let text = &host.output()[common::CHANNEL];
        assert!(text.contains(bridge::KEY_OPEN_LOG_LIMITED), "{text}");
        assert!(text.contains("2 entries"), "{text}");
        assert!(text.contains("`logs.pageSize` is 1"), "{text}");
    }

    #[test]
    fn commands_capture_open_log_reports_a_file_without_sql() {
        let host = MockHost::install();
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
        let host = MockHost::install();
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
