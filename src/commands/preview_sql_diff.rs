//! `preview-sql-diff`: the same render as `preview-sql`, opened in the diff
//! viewer.
//!
//! The platform gives plugins no way to open a custom modal window. The diff
//! viewer is the one editor whose toolbar has "open in a new window", so this
//! command renders the preview as a two-sided diff (original on the left,
//! executable SQL on the right): the user pops it out when they want the SQL
//! beside their other screen, and `preview-sql` (or `Alt+Shift+Q`) keeps the
//! tool-window tab for the everyday case.
//!
//! Input forms and the render are `preview_sql::model_for`, so both entries can
//! never disagree about what a mapper renders to.

use openidea_plugin::{ContentDiff, Error, Invocation, host};

use crate::capture::bridge;
use crate::commands::common::{self, CliError};
use crate::commands::preview_sql;
use crate::i18n::t;

/// Matches `[[contributes.commands]] id` in `plugin.toml`.
pub const ID: &str = "preview-sql-diff";

/// The render produced no SQL (an unresolved statement, or an empty input).
pub const KEY_NO_SQL: &str = "preview.diff.empty_sql";

/// The diff viewer's file name, so the right side gets SQL highlighting.
const DIFF_FILE_NAME: &str = "sql";

/// Dispatch entry.
pub fn run(ctx: &Invocation, arg: Option<&str>) -> Result<Option<String>, Error> {
    match open(ctx, arg) {
        // The diff tab is the surface; do not pop the Output tab as well.
        Ok(()) => Ok(None),
        Err(err) => common::fail(err),
    }
}

fn open(ctx: &Invocation, arg: Option<&str>) -> Result<(), CliError> {
    // The diff view only reads the render: the wrapper projection (fourth
    // value) is the preview store's business, not the diff's.
    let (model, _detail, raw, _wrapper) = preview_sql::model_for(ctx, arg)?;
    let sql = model.sql.trim_end();
    if sql.trim().is_empty() {
        return Err(CliError::new(
            KEY_NO_SQL,
            format!("{}: the render produced no SQL", model.reason),
        ));
    }
    let source = if model.source.is_empty() {
        "<inline>"
    } else {
        model.source.as_str()
    };
    let diff = ContentDiff {
        left: raw,
        right: sql.to_string(),
        left_label: Some(t("preview.diff.left")),
        right_label: Some(t("preview.diff.right")),
        title: Some(format!("{}: {source}", t("preview.diff.title"))),
    };
    host::open_diff(&diff, Some(DIFF_FILE_NAME))
        .map_err(|error| CliError::new(bridge::KEY_VIEW_UNAVAILABLE, error.to_string()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    #[test]
    fn an_inline_statement_opens_a_diff_with_the_sql_on_the_right() {
        let host = MockHost::install();
        run(
            &host.invocation(),
            Some("select id from user where id = 42"),
        )
        .unwrap();
        let diffs = host.opened_diffs();
        assert_eq!(diffs.len(), 1);
        let (diff, file_name) = &diffs[0];
        assert_eq!(file_name.as_deref(), Some("sql"));
        assert!(
            diff.right
                .as_deref()
                .is_some_and(|sql| sql.contains("select id from user")),
            "{:?}",
            diff.right
        );
        assert!(
            diff.left
                .as_deref()
                .is_some_and(|raw| raw.contains("select id from user")),
            "the left side is the original input"
        );
        assert!(
            !host.output().contains_key(common::CHANNEL),
            "the diff tab is the surface; Output stays closed"
        );
    }

    #[test]
    fn a_mapper_statement_puts_the_rendered_sql_on_the_right() {
        let host = MockHost::install();
        host.add_file(
            "/project/src/main/resources/mapper/UserMapper.xml",
            "<mapper namespace=\"com.acme.UserMapper\"><select id=\"findById\">select id from user where id = #{id}</select></mapper>",
        );
        run(
            &host.invocation(),
            Some("/project/src/main/resources/mapper/UserMapper.xml#findById"),
        )
        .unwrap();
        let diffs = host.opened_diffs();
        assert_eq!(diffs.len(), 1);
        let (diff, _) = &diffs[0];
        let sql = diff.right.as_deref().unwrap_or_default();
        assert!(sql.contains("select id from user where id = ?"), "{sql}");
        assert!(
            diff.left
                .as_deref()
                .is_some_and(|raw| raw.contains("<mapper")),
            "the left side is the mapper source"
        );
    }

    #[test]
    fn an_unresolved_statement_says_why_instead_of_opening_a_diff() {
        let host = MockHost::install();
        host.add_file(
            "/project/src/main/resources/mapper/UserMapper.xml",
            "<mapper namespace=\"com.acme.UserMapper\"><select id=\"x\">select 1</select></mapper>",
        );
        let err = run(
            &host.invocation(),
            Some("/project/src/main/resources/mapper/UserMapper.xml#missing"),
        )
        .unwrap_err();
        assert!(
            matches!(err, Error::Failed(ref text) if !text.is_empty()),
            "{err:?}"
        );
        assert!(host.opened_diffs().is_empty(), "no diff for a failure");
    }
}
