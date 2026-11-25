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

    /// The error the command returns to the host.
    pub fn into_error(self) -> Error {
        Error::Failed(format!("{}: {}", self.key, self.detail))
    }
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

/// SQL argument: literal SQL, or `@<path>` to read a file inside the project.
/// Returns the SQL and the file path when it came from a file.
pub fn sql_arg(ctx: &Invocation, arg: Option<&str>) -> Result<(String, Option<String>), CliError> {
    let arg = required_arg(arg)?;
    let Some(raw_path) = arg.strip_prefix('@') else {
        return Ok((arg.to_string(), None));
    };
    let path = raw_path.trim();
    if path.is_empty() {
        return Err(CliError::new(
            KEY_EMPTY_ARG,
            "`@` needs a file path after it",
        ));
    }
    let text = read_file(ctx, path)?;
    Ok((text, Some(path.to_string())))
}

/// Decode a JSON command argument.
pub fn json_arg<T: DeserializeOwned>(arg: Option<&str>) -> Result<T, CliError> {
    let arg = required_arg(arg)?;
    serde_json::from_str(arg).map_err(|e| CliError::new(KEY_JSON_INVALID, e.to_string()))
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
