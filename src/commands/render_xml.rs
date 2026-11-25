//! `render-xml`: render mapper statements into executable SQL.
//!
//! Takes `"<path>#<namespace>.<statement>"` to render one statement, or
//! `"<path>"` to render every statement of the file. The file is read through
//! `Project::read_file`, so an absolute path and a project-relative path both
//! work and unsaved editor contents win.
//!
//! Rendering itself is [`crate::core::xml::MapperSet`]; this module only
//! formats its [`RenderOutput`] into the Output channel.

use std::fmt::Write as _;

use openidea_plugin::{Error, Invocation};
use serde_json::json;

use crate::commands::common::{self, CliError};
use crate::core::xml::{Confidence, MapperSet, RenderOutput};

/// Matches `[[contributes.commands]] id` in `plugin.toml`.
pub const ID: &str = "render-xml";

/// The shared Output channel (re-exported for `src/lib.rs`).
// The controller's `src/lib.rs` test names `commands::render_xml::CHANNEL`; in a
// non-test build nothing reads the re-export, hence the allow.
#[allow(unused_imports)]
pub use crate::commands::CHANNEL;

/// What the command accepts as `arg`.
pub const USAGE: &str = "<mapper.xml>#<namespace>.<statement> | <mapper.xml>";

pub const KEY_NO_STATEMENTS: &str = "render_xml.no_statements";

/// The argument-less entry point kept for the existing dispatch; it reports the
/// required argument instead of printing a sample.
pub fn run(ctx: &Invocation) -> Result<Option<String>, Error> {
    run_with_arg(ctx, None)
}

/// Dispatch entry: `arg` is the path (with an optional `#statement`).
pub fn run_with_arg(ctx: &Invocation, arg: Option<&str>) -> Result<Option<String>, Error> {
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
    let (path, statement) = split_arg(arg)?;
    let xml = common::read_file(ctx, &path)?;
    let (set, issues) = MapperSet::load(&[(path.as_str(), xml.as_str())]);
    let ids = match statement {
        Some(id) => vec![id],
        None => unique_statement_ids(&set),
    };
    if ids.is_empty() {
        return Err(CliError::new(
            KEY_NO_STATEMENTS,
            format!("{path}: no <select>/<insert>/<update>/<delete> statement found"),
        ));
    }

    let mut text = String::new();
    let _ = writeln!(text, "-- render-xml: {path}");
    for issue in &issues {
        let _ = writeln!(
            text,
            "-- load [{}]: {}: {}",
            issue.message_key, issue.path, issue.detail
        );
    }
    for id in &ids {
        let out = set.render(id, &json!({}));
        text.push('\n');
        append_statement(&mut text, id, &out);
    }
    Ok(text)
}

/// `path#statement` → `(path, Some(statement))`; `path` → `(path, None)`.
fn split_arg(arg: &str) -> Result<(String, Option<String>), CliError> {
    match arg.rsplit_once('#') {
        Some((path, statement)) if !path.trim().is_empty() && !statement.trim().is_empty() => {
            Ok((path.trim().to_string(), Some(statement.trim().to_string())))
        }
        Some(_) => Err(CliError::new(
            common::KEY_MISSING_ARG,
            format!("expected `{USAGE}`, got `{arg}`"),
        )),
        None => Ok((arg.to_string(), None)),
    }
}

/// One header, then either the SQL with its parameters or the reason it could
/// not be rendered (`unresolved` never prints SQL).
fn append_statement(text: &mut String, id: &str, out: &RenderOutput) {
    let _ = writeln!(text, "-- {id} ({})", confidence_label(out.confidence));
    if out.confidence == Confidence::Unresolved {
        for note in &out.notes {
            let _ = writeln!(text, "-- reason [{}]: {}", note.message_key, note.detail);
        }
        for key in &out.message_keys {
            if !out.notes.iter().any(|n| &n.message_key == key) {
                let _ = writeln!(text, "-- reason [{key}]");
            }
        }
        return;
    }
    text.push_str(out.sql.trim_end());
    text.push('\n');
    let params = serde_json::to_string(&out.params).unwrap_or_else(|_| "[]".to_string());
    let _ = writeln!(text, "params: {params}");
    for note in &out.notes {
        let _ = writeln!(text, "notes: [{}] {}", note.message_key, note.detail);
    }
    if !out.message_keys.is_empty() {
        let _ = writeln!(text, "message_keys: {}", out.message_keys.join(", "));
    }
}

fn confidence_label(confidence: Confidence) -> &'static str {
    match confidence {
        Confidence::Exact => "exact",
        Confidence::Partial => "partial",
        Confidence::Unresolved => "unresolved",
    }
}

/// `MapperSet::statement_ids` lists each statement as bare and qualified id;
/// keep the qualified form (it can be ambiguous when namespaces share a bare
/// id) and drop the bare duplicate.
fn unique_statement_ids(set: &MapperSet) -> Vec<String> {
    let ids = set.statement_ids();
    let mut out: Vec<String> = Vec::new();
    for (index, id) in ids.iter().enumerate() {
        let shadowed = ids.iter().enumerate().any(|(other_index, other)| {
            other_index != index
                && other.len() > id.len() + 1
                && other.ends_with(id.as_str())
                && other.as_bytes()[other.len() - id.len() - 1] == b'.'
        });
        if !shadowed && !out.contains(id) {
            out.push(id.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    const MAPPER: &str = r#"<mapper namespace="com.example.UserMapper">
    <sql id="cols">id, name</sql>
    <select id="findById">SELECT <include refid="cols"/> FROM users WHERE id = #{id}</select>
    <update id="touch">UPDATE users SET name = #{name} WHERE id = #{id}</update>
</mapper>"#;

    fn host_with_mapper() -> MockHost {
        let host = MockHost::install();
        host.add_file("/project/UserMapper.xml", MAPPER);
        host
    }

    /// XML text nodes leave runs of whitespace in the rendered SQL; assertions
    /// compare the whitespace-normalised form.
    fn compact(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn renders_the_requested_statement_with_header_sql_and_params() {
        let host = host_with_mapper();
        run_with_arg(
            &host.invocation(),
            Some("/project/UserMapper.xml#com.example.UserMapper.findById"),
        )
        .unwrap();
        let text = &host.output()[CHANNEL];
        assert!(text.contains("-- com.example.UserMapper.findById (exact)"));
        assert!(compact(text).contains("SELECT id, name FROM users WHERE id = ?"));
        assert!(text.contains("params: ["));
    }

    #[test]
    fn a_bare_path_renders_every_statement_of_the_file() {
        let host = MockHost::install();
        host.add_file("UserMapper.xml", MAPPER);
        run_with_arg(&host.invocation(), Some("UserMapper.xml")).unwrap();
        let text = &host.output()[CHANNEL];
        assert!(text.contains("-- com.example.UserMapper.findById (exact)"));
        assert!(text.contains("-- com.example.UserMapper.touch (exact)"));
        assert!(compact(text).contains("UPDATE users SET name = ? WHERE id = ?"));
    }

    #[test]
    fn unresolved_prints_the_reason_and_no_sql() {
        let host = MockHost::install();
        host.add_file(
            "/project/Cycle.xml",
            r#"<mapper namespace="a">
                <sql id="one"><include refid="two"/></sql>
                <sql id="two"><include refid="one"/></sql>
                <select id="s">SELECT <include refid="one"/> FROM t</select>
            </mapper>"#,
        );
        run_with_arg(&host.invocation(), Some("/project/Cycle.xml#a.s")).unwrap();
        let text = &host.output()[CHANNEL];
        assert!(text.contains("-- a.s (unresolved)"));
        assert!(text.contains("-- reason [xml.include.cycle]"));
        assert!(!text.contains("FROM t"), "unresolved must not print SQL");
    }

    #[test]
    fn an_unreadable_file_is_an_error_with_a_message_key() {
        let host = MockHost::install();
        let err = run_with_arg(&host.invocation(), Some("/project/missing.xml")).unwrap_err();
        assert!(
            matches!(err, Error::Failed(ref text) if text.contains(common::KEY_FILE_UNREADABLE))
        );
        let text = &host.output()[CHANNEL];
        assert!(text.contains(common::KEY_FILE_UNREADABLE));
        assert!(text.contains("/project/missing.xml"));
    }

    #[test]
    fn a_missing_argument_is_an_error_with_a_message_key() {
        let host = MockHost::install();
        let err = run_with_arg(&host.invocation(), None).unwrap_err();
        assert!(matches!(err, Error::Failed(ref text) if text.contains(common::KEY_MISSING_ARG)));
        assert!(host.output()[CHANNEL].contains(common::KEY_MISSING_ARG));
    }

    #[test]
    fn a_file_without_statements_is_an_error() {
        let host = MockHost::install();
        host.add_file("/project/Empty.xml", "<mapper namespace=\"a\"/>");
        let err = run_with_arg(&host.invocation(), Some("/project/Empty.xml")).unwrap_err();
        assert!(matches!(err, Error::Failed(ref text) if text.contains(KEY_NO_STATEMENTS)));
    }

    #[test]
    fn a_long_render_is_truncated_with_a_notice() {
        let host = MockHost::install();
        let mut xml = String::from("<mapper namespace=\"a\">");
        for index in 0..3000 {
            let _ = write!(
                xml,
                "<select id=\"s{index}\">SELECT id, name, created_at FROM users WHERE id = #{{id}}</select>"
            );
        }
        xml.push_str("</mapper>");
        host.add_file("/project/Big.xml", &xml);
        run_with_arg(&host.invocation(), Some("/project/Big.xml")).unwrap();
        let text = &host.output()[CHANNEL];
        assert!(text.contains(common::KEY_OUTPUT_TRUNCATED));
        assert!(text.len() < common::MAX_OUTPUT_BYTES + 512);
        assert!(text.matches("-- a.s").count() < 3000);
    }
}
