//! `render-log`: parse a SQL log file and show the first page of statements.
//!
//! The argument is a log path (or `@<path>`) inside the project. Parsing is
//! [`crate::core::log::parse`]; only the first `logs.pageSize` entries (default
//! 50) are formatted, so a 100k-line log stays inside the command budget and
//! the Output cap.

use std::fmt::Write as _;
use std::time::Instant;

use openidea_plugin::{Error, Invocation, host};

use crate::commands::common::{self, CliError};
use crate::core::log::{EntryClass, parse};

/// Matches `[[contributes.commands]] id` in `plugin.toml`.
pub const ID: &str = "render-log";

/// What the command accepts as `arg`.
pub const USAGE: &str = "<log path> | @<log path>";

pub const PAGE_SIZE_SETTING: &str = "logs.pageSize";
pub const DEFAULT_PAGE_SIZE: usize = 50;
pub const MAX_PAGE_SIZE: usize = 5000;

pub const KEY_NO_ENTRIES: &str = "render_log.no_entries";
pub const KEY_PAGE_LIMITED: &str = "render_log.page_limited";

/// Dispatch entry.
pub fn run(ctx: &Invocation, arg: Option<&str>) -> Result<Option<String>, Error> {
    if arg.map(str::trim).unwrap_or("").is_empty() {
        return common::fail(common::missing_arg(USAGE));
    }
    match render(ctx, arg) {
        Ok(text) => {
            common::emit(&text)?;
            Ok(None)
        }
        Err(err) => common::fail(err),
    }
}

fn render(ctx: &Invocation, arg: Option<&str>) -> Result<String, CliError> {
    let arg = common::required_arg(arg)?;
    let path = arg.strip_prefix('@').unwrap_or(arg).trim();
    if path.is_empty() {
        return Err(CliError::new(common::KEY_EMPTY_ARG, "no log file path"));
    }
    let text = common::read_file(ctx, path)?;
    let started = Instant::now();
    let analysis = parse(&text, &crate::capture::bridge::parse_options());
    let page_size = page_size();
    let shown = analysis.entries.len().min(page_size);

    let mut out = String::new();
    let _ = writeln!(
        out,
        "-- render-log: {path} (entries: {}, unparsed: {}, showing: {shown}, {} ms)",
        analysis.entries.len(),
        analysis.unparsed_lines.len(),
        started.elapsed().as_millis()
    );
    if analysis.entries.is_empty() {
        let _ = writeln!(out, "-- no SQL entries found [{KEY_NO_ENTRIES}]");
        return Ok(out);
    }
    if analysis.entries.len() > shown {
        let _ = writeln!(
            out,
            "-- showing first {shown} of {} [{KEY_PAGE_LIMITED}]",
            analysis.entries.len()
        );
    }
    for (index, entry) in analysis.entries.iter().take(shown).enumerate() {
        let _ = write!(
            out,
            "#{} line={} time={} duration={} category={}/{} confidence={}",
            index + 1,
            entry.line_no,
            entry
                .timestamp_ms
                .map_or_else(|| "-".to_string(), |ms| ms.to_string()),
            entry
                .duration_ms
                .map_or_else(|| "-".to_string(), |ms| format!("{ms}ms")),
            entry.family.as_str(),
            class_label(entry.class),
            confidence_label(entry.confidence)
        );
        // `<== Updates:` / `<== Total:` counts, printed only when the log
        // family reported one so prefix-less fixtures keep the old header.
        if let Some(rows) = entry.affected_rows {
            let _ = write!(out, " rows_affected={rows}");
        }
        if let Some(rows) = entry.result_rows {
            let _ = write!(out, " result_rows={rows}");
        }
        out.push('\n');
        if entry.sql.is_empty() {
            out.push_str("  <no SQL>");
        } else {
            out.push_str(&entry.sql);
        }
        out.push('\n');
    }
    Ok(out)
}

fn page_size() -> usize {
    match host::setting_number(PAGE_SIZE_SETTING) {
        Some(value) if value >= 1.0 => (value as usize).clamp(1, MAX_PAGE_SIZE),
        _ => DEFAULT_PAGE_SIZE,
    }
}

fn class_label(class: EntryClass) -> &'static str {
    match class {
        EntryClass::Select => "select",
        EntryClass::Insert => "insert",
        EntryClass::Update => "update",
        EntryClass::Delete => "delete",
        EntryClass::Ddl => "ddl",
        EntryClass::Other => "other",
        EntryClass::Error => "error",
    }
}

fn confidence_label(confidence: crate::core::log::Confidence) -> &'static str {
    match confidence {
        crate::core::log::Confidence::Exact => "exact",
        crate::core::log::Confidence::Partial => "partial",
        crate::core::log::Confidence::Unresolved => "unresolved",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    fn text(host: &MockHost) -> String {
        host.output()[common::CHANNEL].clone()
    }

    const LOG: &str = "\
==>  Preparing: SELECT id, name FROM user WHERE id = ? AND status = ?
==> Parameters: 1(Long), active(String)
";

    #[test]
    fn parses_mybatis_preparing_and_parameters() {
        let host = MockHost::install();
        host.add_file("/project/app.log", LOG);
        run(&host.invocation(), Some("/project/app.log")).unwrap();
        let text = text(&host);
        assert!(text.contains("category=mybatis/select"));
        assert!(text.contains("confidence=exact"));
        assert!(text.contains("SELECT id, name FROM user WHERE id = 1 AND status = 'active'"));
        assert!(text.contains("entries: 1"));
    }

    #[test]
    fn close_rows_give_the_entry_its_duration_and_row_counts() {
        let host = MockHost::install();
        host.add_file(
            "/project/app.log",
            "2026-11-25 21:00:01.001 DEBUG 4242 --- [nio-8080-exec-1] c.e.m.Mapper.selectById : ==>  Preparing: SELECT id FROM users WHERE id = ?\n\
             2026-11-25 21:00:01.002 DEBUG 4242 --- [nio-8080-exec-1] c.e.m.Mapper.selectById : ==> Parameters: 42(Long)\n\
             2026-11-25 21:00:01.006 DEBUG 4242 --- [nio-8080-exec-1] c.e.m.Mapper.selectById : <==      Total: 1\n\
             2026-11-25 21:00:02.700 DEBUG 4242 --- [nio-8080-exec-2] c.e.m.Mapper.updateStatus : ==>  Preparing: UPDATE orders SET status = ? WHERE id = ?\n\
             2026-11-25 21:00:02.701 DEBUG 4242 --- [nio-8080-exec-2] c.e.m.Mapper.updateStatus : ==> Parameters: SHIPPED(String), 42(Long)\n\
             2026-11-25 21:00:02.980 DEBUG 4242 --- [nio-8080-exec-2] c.e.m.Mapper.updateStatus : <==    Updates: 3\n",
        );
        run(&host.invocation(), Some("/project/app.log")).unwrap();
        let text = text(&host);
        assert!(text.contains("duration=5ms"));
        assert!(text.contains("result_rows=1"));
        assert!(text.contains("duration=280ms"));
        assert!(text.contains("rows_affected=3"));
        assert!(!text.contains("duration=-"));
    }

    #[test]
    fn accepts_the_at_path_form() {
        let host = MockHost::install();
        host.add_file("/project/app.log", LOG);
        run(&host.invocation(), Some("@/project/app.log")).unwrap();
        assert!(text(&host).contains("-- render-log: /project/app.log"));
    }

    #[test]
    fn page_size_comes_from_the_setting() {
        let host = MockHost::install();
        host.set_setting(PAGE_SIZE_SETTING, "1");
        host.add_file(
            "/project/app.log",
            "Preparing: DELETE FROM a WHERE id = 1\nPreparing: DELETE FROM b WHERE id = 2\n",
        );
        run(&host.invocation(), Some("/project/app.log")).unwrap();
        let text = text(&host);
        assert!(text.contains(&format!("[{KEY_PAGE_LIMITED}]")));
        assert!(text.contains("DELETE FROM a"));
        assert!(!text.contains("DELETE FROM b"));
    }

    #[test]
    fn a_file_without_sql_entries_says_so() {
        let host = MockHost::install();
        host.add_file("/project/empty.log", "nothing to see here\n");
        run(&host.invocation(), Some("/project/empty.log")).unwrap();
        assert!(text(&host).contains(KEY_NO_ENTRIES));
    }

    #[test]
    fn a_missing_file_is_an_error_with_a_key() {
        let host = MockHost::install();
        let err = run(&host.invocation(), Some("/project/nope.log")).unwrap_err();
        assert!(
            matches!(err, Error::Failed(ref text) if text.contains(common::KEY_FILE_UNREADABLE))
        );
        assert!(text(&host).contains(common::KEY_FILE_UNREADABLE));
    }

    #[test]
    fn one_hundred_thousand_lines_parse_within_the_budget() {
        let host = MockHost::install();
        let mut log = String::with_capacity(6 * 1024 * 1024);
        // 50 000 statements × (Preparing + Parameters) = 100 000 log lines.
        for index in 0..50_000 {
            let _ = writeln!(log, "Preparing: DELETE FROM t WHERE id = ?");
            let _ = writeln!(log, "Parameters: {index}(Long)");
        }
        host.add_file("/project/big.log", &log);
        let started = std::time::Instant::now();
        run(&host.invocation(), Some("/project/big.log")).unwrap();
        assert!(
            started.elapsed().as_secs() < 5,
            "100k lines must fit the 5 s budget"
        );
        let text = text(&host);
        assert!(text.contains("entries: 50000"));
        assert_eq!(text.matches("\n#").count(), DEFAULT_PAGE_SIZE);
    }
}
