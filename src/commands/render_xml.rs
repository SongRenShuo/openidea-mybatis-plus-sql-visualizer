//! `render-xml`: the first half of the product, and the P0 end-to-end probe.
//!
//! The real command will render the mapper XML around the caret. Two things are
//! missing for that and are deliberately not faked here:
//!
//! * the engine (`engine/ENGINE_VERSION` is a placeholder until the IntelliJ
//!   repository exports `mpsv-engine-<version>.jar`);
//! * `editor.read`, which this release does not request — the capability list
//!   stays at `project.read` until the engine needs the project tree.
//!
//! So the command prints a fixed statement. That is enough to prove the chain
//! the rest of the port depends on: a declared command reaches Rust code and
//! that code owns an Output tab.

use openidea_plugin::{Error, Invocation, host};

/// Matches `[[contributes.commands]] id` in `plugin.toml`.
pub const ID: &str = "render-xml";

/// The Output tab's channel name; 1–128 bytes, no control characters.
pub const CHANNEL: &str = "MyBatis SQL Visualizer";

/// Read at compile time so the banner can never disagree with the pinned file.
const ENGINE_VERSION: &str = include_str!("../../engine/ENGINE_VERSION");

/// Rendered SQL from a mapper statement, i.e. what T1-1 will produce for a
/// snippet such as `select id, name, created_at from users where deleted = 0`.
const SAMPLE_SQL: &str = "\
SELECT id, name, created_at
FROM users
WHERE deleted = 0
  AND name = ?;
";

pub fn run(_ctx: &Invocation) -> Result<Option<String>, Error> {
    host::output_clear(CHANNEL)?;
    host::output_append(
        CHANNEL,
        &format!(
            "-- MyBatis SQL Visualizer {} (engine {})\n",
            env!("CARGO_PKG_VERSION"),
            ENGINE_VERSION.trim()
        ),
    )?;
    host::output_append(CHANNEL, SAMPLE_SQL)?;
    host::output_reveal(CHANNEL)?;
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    #[test]
    fn writes_banner_and_sql_to_its_own_channel() {
        let host = MockHost::install();
        run(&host.invocation()).unwrap();
        let buffers = host.output();
        assert_eq!(buffers.len(), 1, "only the plugin's own channel is touched");
        let text = &buffers[CHANNEL];
        assert!(text.starts_with("-- MyBatis SQL Visualizer 0.1.0 (engine 0.0.0-pending)\n"));
        assert!(text.ends_with("WHERE deleted = 0\n  AND name = ?;\n"));
    }

    #[test]
    fn running_twice_replaces_rather_than_appends() {
        let host = MockHost::install();
        run(&host.invocation()).unwrap();
        run(&host.invocation()).unwrap();
        let text = &host.output()[CHANNEL];
        assert_eq!(text.matches("SELECT id, name, created_at").count(), 1);
    }
}
