//! Shared scaffolding for the command shells.
//!
//! Commands are thin: they parse their argument, read a file through the
//! project, call one pure entry point in [`crate::core`] and format the result.
//! This module owns the parts every command repeats — the Output channel, the
//! byte cap, uniform error reporting and the argument/JSON helpers.
//!
//! No SQL is parsed here; statement classification and rendering stay in
//! `crate::core`.

use std::fmt::Write as _;

use openidea_plugin::{Error, Invocation, host};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::core::r#gen::{ColumnSpec, GenError, TableSpec};
use crate::core::log::EntryClass;
use crate::core::perf::{Finding, FindingSummary, Severity};

/// The Output tab's channel name; 1–128 bytes, no control characters.
pub const CHANNEL: &str = "MyBatis SQL Visualizer";

/// Commands write at most this many bytes; anything longer is truncated with a
/// visible notice so a 100k-line log cannot stall the Output view.
pub const MAX_OUTPUT_BYTES: usize = 200 * 1024;

pub const KEY_MISSING_ARG: &str = "cmd.arg.missing";
pub const KEY_EMPTY_ARG: &str = "cmd.arg.empty";
pub const KEY_FILE_UNREADABLE: &str = "cmd.file.unreadable";
pub const KEY_NO_PROJECT: &str = "cmd.project.missing";
pub const KEY_JSON_INVALID: &str = "cmd.json.invalid";
pub const KEY_OUTPUT_TRUNCATED: &str = "cmd.output.truncated";

/// The setting L6 registers for the SQL dialect used by quote checks.
pub const DIALECT_SETTING: &str = "sql.dialect";

/// A failure the command turns into a readable Output block plus an `Err`.
///
/// `key` is a stable message key, `detail` the machine-readable context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliError {
    pub key: String,
    pub detail: String,
}

impl CliError {
    pub fn new(key: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            detail: detail.into(),
        }
    }
}

/// The error for a missing argument, carrying the command's usage line so the
/// user sees what to type instead of only "this command needs an argument".
pub fn missing_arg(usage: &str) -> CliError {
    CliError::new(KEY_MISSING_ARG, format!("usage: {usage}"))
}

/// Write `text` to the command channel, replacing whatever was there.
pub fn emit(text: &str) -> Result<(), Error> {
    host::output_clear(CHANNEL)?;
    host::output_append(CHANNEL, &capped(text))?;
    host::output_reveal(CHANNEL)?;
    Ok(())
}

/// Truncate `text` to [`MAX_OUTPUT_BYTES`] on a character boundary and append a
/// notice naming the message key.
pub fn capped(text: &str) -> String {
    if text.len() <= MAX_OUTPUT_BYTES {
        return text.to_string();
    }
    let mut end = MAX_OUTPUT_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::with_capacity(end + 160);
    out.push_str(&text[..end]);
    let _ = write!(
        out,
        "\n-- output truncated [{KEY_OUTPUT_TRUNCATED}]: showing {end} of {} bytes\n",
        text.len()
    );
    out
}

/// `N (delete 1, select 1)` — statement count plus per-class counts, in
/// first-appearance order. Shared by `check-sql` and `perf-advice` so both
/// surfaces describe a multi-statement text the same way.
pub fn class_mix(classes: impl Iterator<Item = EntryClass>) -> String {
    let mut counts: Vec<(EntryClass, usize)> = Vec::new();
    let mut total = 0usize;
    for class in classes {
        total += 1;
        match counts.iter_mut().find(|(held, _)| *held == class) {
            Some((_, count)) => *count += 1,
            None => counts.push((class, 1)),
        }
    }
    let parts = counts
        .iter()
        .map(|(class, count)| format!("{} {count}", class.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{total} ({parts})")
}

/// Severity label used by every finding line.
pub fn severity_label(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warn => "warn",
        Severity::Info => "info",
    }
}

/// One line per rule hit (`- [warn] rule_id message_key detail`).
pub fn finding_line(text: &mut String, finding: &Finding) {
    let _ = writeln!(
        text,
        "- [{}] {} {} {}",
        severity_label(finding.severity),
        finding.rule_id,
        finding.message_key,
        finding.detail
    );
}

/// One line per rule, with `count` and statement evidence (multi-statement
/// output; the same rule firing in several statements is not repeated).
pub fn finding_summary_line(text: &mut String, summary: &FindingSummary) {
    let _ = writeln!(
        text,
        "- [{}] {} {} {} count={} statements={}",
        severity_label(summary.severity),
        summary.rule_id,
        summary.message_key,
        summary.detail,
        summary.count,
        summary
            .statements
            .iter()
            .map(|index| format!("#{index}"))
            .collect::<Vec<_>>()
            .join(",")
    );
}

/// Emit a readable error block and return it as the command's error.
pub fn report(key: &str, detail: &str) -> Result<Option<String>, Error> {
    let text = format!("-- error [{key}]: {detail}\n");
    emit(&text)?;
    Err(Error::Failed(format!("{key}: {detail}")))
}

/// [`report`] for a [`CliError`] produced by an argument helper.
pub fn fail(err: CliError) -> Result<Option<String>, Error> {
    report(&err.key, &err.detail)
}

/// The invoking window's project.
pub fn project(ctx: &Invocation) -> Result<openidea_plugin::Project, CliError> {
    ctx.project()
        .ok_or_else(|| CliError::new(KEY_NO_PROJECT, "no project is open in this window"))
}

/// Read `path` (absolute or project-relative) through `Project::read_file`.
pub fn read_file(ctx: &Invocation, path: &str) -> Result<String, CliError> {
    let project = project(ctx)?;
    project
        .read_file(path)
        .map_err(|e| CliError::new(KEY_FILE_UNREADABLE, format!("{path}: {e}")))
}

/// The command's argument, trimmed; an absent or empty argument is an error.
pub fn required_arg(arg: Option<&str>) -> Result<&str, CliError> {
    let arg =
        arg.ok_or_else(|| CliError::new(KEY_MISSING_ARG, "this command needs an argument"))?;
    let arg = arg.trim();
    if arg.is_empty() {
        return Err(CliError::new(KEY_EMPTY_ARG, "the argument is empty"));
    }
    Ok(arg)
}

/// True when `arg` can only be a path candidate: non-empty and free of
/// whitespace.
///
/// Inline SQL and inline JSON almost always contain a space, so a whitespace
/// argument is never probed as a file. That keeps `UPDATE user SET …` from
/// being mistaken for a (unreadable) path.
fn path_candidate(arg: &str) -> bool {
    !arg.is_empty() && !arg.chars().any(char::is_whitespace)
}

/// The path behind `@<path>`, or the error for a bare `@`.
fn at_path(raw_path: &str) -> Result<&str, CliError> {
    let path = raw_path.trim();
    if path.is_empty() {
        return Err(CliError::new(
            KEY_EMPTY_ARG,
            "`@` needs a file path after it",
        ));
    }
    Ok(path)
}

/// SQL argument: literal SQL, `@<path>`, or a bare project file path.
///
/// The bare-path form exists for the project-view context menu, which hands the
/// command a path and nothing else. Only an argument free of whitespace is
/// probed as a file; when the read fails the argument is taken as literal SQL.
/// Returns the SQL and the file path when it came from a file.
pub fn sql_arg(ctx: &Invocation, arg: Option<&str>) -> Result<(String, Option<String>), CliError> {
    let arg = required_arg(arg)?;
    if let Some(raw_path) = arg.strip_prefix('@') {
        let path = at_path(raw_path)?;
        let text = read_file(ctx, path)?;
        return Ok((text, Some(path.to_string())));
    }
    if path_candidate(arg)
        && let Ok(text) = read_file(ctx, arg)
    {
        return Ok((text, Some(arg.to_string())));
    }
    Ok((arg.to_string(), None))
}

/// Decode a JSON command argument: inline JSON, `@<path>`, or a bare project
/// file path holding JSON.
///
/// Only an argument that neither looks like inline JSON (`{` / `[`) nor
/// contains whitespace is probed as a file; every other form is parsed as
/// literal JSON so the existing `cmd.json.invalid` report still applies.
pub fn json_arg<T: DeserializeOwned>(ctx: &Invocation, arg: Option<&str>) -> Result<T, CliError> {
    let arg = required_arg(arg)?;
    let text = if let Some(raw_path) = arg.strip_prefix('@') {
        read_file(ctx, at_path(raw_path)?)?
    } else if !arg.starts_with('{') && !arg.starts_with('[') && path_candidate(arg) {
        // A bare path that is not readable is not an error here: fall back to
        // parsing the argument itself, which reports `cmd.json.invalid`.
        read_file(ctx, arg).unwrap_or_else(|_| arg.to_string())
    } else {
        arg.to_string()
    };
    serde_json::from_str(&text).map_err(|e| CliError::new(KEY_JSON_INVALID, e.to_string()))
}

/// The configured dialect name (`sql.dialect`), or an empty string when unset.
pub fn configured_dialect() -> String {
    host::setting_string(DIALECT_SETTING).unwrap_or_default()
}

/// Split a [`GenError`] into its message key and readable detail.
pub fn gen_error_parts(err: &GenError) -> (String, String) {
    match err {
        GenError::NoPrimaryKey => ("gen.error.no_primary_key".to_string(), String::new()),
        GenError::NoColumns => ("gen.error.no_columns".to_string(), String::new()),
        GenError::EmptySql => ("gen.error.empty_sql".to_string(), String::new()),
        GenError::UnknownDialect(name) => (
            "gen.error.unknown_dialect".to_string(),
            format!("dialect `{name}` is not supported"),
        ),
        GenError::Unresolved {
            message_key,
            detail,
        } => (message_key.clone(), detail.to_string()),
    }
}

/// A [`GenError`] as a [`CliError`].
pub fn gen_error(err: &GenError) -> CliError {
    let (key, detail) = gen_error_parts(err);
    CliError::new(key, detail)
}

/// One column in a `generate-crud` / `rollback-plan` JSON argument. Either a
/// bare name or an object matching `ColumnSpec` (snake_case or camelCase).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ColumnInput {
    Name(String),
    Spec(ColumnSpecInput),
}

#[derive(Debug, Clone, Deserialize)]
pub struct ColumnSpecInput {
    pub name: String,
    #[serde(default, alias = "primaryKey")]
    pub primary_key: bool,
    #[serde(default, alias = "autoIncrement")]
    pub auto_increment: bool,
}

/// Build the core table description from the command's JSON columns and the
/// optional `pk` name list.
pub fn build_table_spec(
    table: &str,
    schema: Option<&str>,
    columns: &[ColumnInput],
    pk: &[String],
) -> TableSpec {
    let columns = columns
        .iter()
        .map(|input| match input {
            ColumnInput::Name(name) => {
                let mut column = ColumnSpec::new(name);
                if pk.iter().any(|k| k.eq_ignore_ascii_case(name)) {
                    column = column.primary_key();
                }
                column
            }
            ColumnInput::Spec(spec) => {
                let mut column = ColumnSpec::new(&spec.name);
                if spec.primary_key {
                    column = column.primary_key();
                }
                if spec.auto_increment {
                    column = column.auto_increment();
                }
                column
            }
        })
        .collect::<Vec<_>>();
    let mut spec = TableSpec::new(table, columns);
    if let Some(schema) = schema {
        spec = spec.with_schema(schema);
    }
    spec
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    #[test]
    fn sql_arg_at_path_reads_the_file() {
        let host = MockHost::install();
        host.add_file("/project/a.sql", "SELECT 1");
        let (sql, source) = sql_arg(&host.invocation(), Some("@/project/a.sql")).unwrap();
        assert_eq!(sql, "SELECT 1");
        assert_eq!(source.as_deref(), Some("/project/a.sql"));
    }

    #[test]
    fn sql_arg_bare_path_reads_the_file() {
        let host = MockHost::install();
        host.add_file("/project/b.sql", "DELETE FROM user WHERE id = 1");
        let (sql, source) = sql_arg(&host.invocation(), Some("/project/b.sql")).unwrap();
        assert_eq!(sql, "DELETE FROM user WHERE id = 1");
        assert_eq!(source.as_deref(), Some("/project/b.sql"));
    }

    #[test]
    fn sql_arg_text_with_spaces_is_never_probed_as_a_path() {
        let host = MockHost::install();
        // A file exists under the literal text, yet the space keeps it inline.
        host.add_file("SELECT 1", "should not be read");
        let (sql, source) = sql_arg(&host.invocation(), Some("SELECT 1")).unwrap();
        assert_eq!(sql, "SELECT 1");
        assert_eq!(source, None);
    }

    #[test]
    fn sql_arg_missing_bare_path_stays_a_literal() {
        let host = MockHost::install();
        let (sql, source) = sql_arg(&host.invocation(), Some("/project/missing.sql")).unwrap();
        assert_eq!(sql, "/project/missing.sql");
        assert_eq!(source, None);
    }

    #[test]
    fn json_arg_inline_literal_is_parsed() {
        let host = MockHost::install();
        let value: serde_json::Value = json_arg(&host.invocation(), Some(r#"{"a":1}"#)).unwrap();
        assert_eq!(value["a"], 1);
    }

    #[test]
    fn json_arg_at_path_reads_json_from_the_project() {
        let host = MockHost::install();
        host.add_file("/project/c.json", r#"{"a":2}"#);
        let value: serde_json::Value =
            json_arg(&host.invocation(), Some("@/project/c.json")).unwrap();
        assert_eq!(value["a"], 2);
    }

    #[test]
    fn json_arg_bare_path_reads_json_from_the_project() {
        let host = MockHost::install();
        host.add_file("/project/d.json", r#"{"a":3}"#);
        let value: serde_json::Value =
            json_arg(&host.invocation(), Some("/project/d.json")).unwrap();
        assert_eq!(value["a"], 3);
    }

    #[test]
    fn json_arg_invalid_json_keeps_the_key() {
        let host = MockHost::install();
        let err = json_arg::<serde_json::Value>(&host.invocation(), Some("{not json")).unwrap_err();
        assert_eq!(err.key, KEY_JSON_INVALID);
    }

    #[test]
    fn capped_keeps_short_text_untouched() {
        assert_eq!(capped("SELECT 1\n"), "SELECT 1\n");
    }

    #[test]
    fn capped_truncates_on_a_character_boundary_and_names_the_key() {
        let text = "中".repeat(MAX_OUTPUT_BYTES);
        let out = capped(&text);
        let prefix = out
            .split("\n-- output truncated")
            .next()
            .expect("a truncated body");
        assert!(prefix.len() <= MAX_OUTPUT_BYTES);
        assert!(prefix.len() > MAX_OUTPUT_BYTES - 4);
        assert!(
            prefix.chars().all(|c| c == '中'),
            "cut on a 3-byte boundary"
        );
        assert!(out.contains(KEY_OUTPUT_TRUNCATED));
        assert!(out.len() < MAX_OUTPUT_BYTES + 300);
    }

    #[test]
    fn build_table_spec_marks_pk_names_and_flags() {
        let columns = vec![
            ColumnInput::Spec(ColumnSpecInput {
                name: "id".into(),
                primary_key: true,
                auto_increment: true,
            }),
            ColumnInput::Name("name".into()),
            ColumnInput::Name("code".into()),
        ];
        let spec = build_table_spec("user", None, &columns, &["code".to_string()]);
        assert_eq!(spec.primary_keys().len(), 2);
        assert_eq!(spec.columns[0].name, "id");
        assert!(spec.columns[0].auto_increment);
        assert!(spec.columns[2].primary_key);
        assert_eq!(spec.schema, None);
    }

    #[test]
    fn gen_errors_carry_stable_keys() {
        assert_eq!(
            gen_error_parts(&GenError::NoPrimaryKey).0,
            "gen.error.no_primary_key"
        );
        let unresolved = GenError::Unresolved {
            message_key: "gen.test".into(),
            detail: serde_json::json!({"x": 1}),
        };
        let (key, detail) = gen_error_parts(&unresolved);
        assert_eq!(key, "gen.test");
        assert!(detail.contains("\"x\":1"));
    }
}
