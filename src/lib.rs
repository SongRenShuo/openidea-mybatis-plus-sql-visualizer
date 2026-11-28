//! MyBatis SQL Visualizer for OpenIDEA.
//!
//! The port keeps the same split as the IntelliJ edition: platform-facing
//! surfaces (commands, views, providers, MCP tools) live in this crate, while
//! the pure rendering logic lives in [`core`] and — for capabilities that need
//! a JVM — in the engine jar exported by the IntelliJ repository (`engine/`).
//!
//! Every command is a thin shell: read a project file, call one [`core`] entry
//! point, format the result into the shared Output channel. Nothing here
//! guesses: rendered results carry `exact` / `partial` / `unresolved`, and
//! `unresolved` never produces SQL.

// `serde_json::json!` expands recursively; the panels' label maps are past the
// default 128 once a view carries ~45 strings.
#![recursion_limit = "256"]

mod commands;

// Module tree is declared once here so implementation lanes never touch this
// file: each lane owns one directory and fills the placeholders in place.
// Optional exports (`views`, `providers`, `indexing`, `mcp`, …) are added to
// `register_plugin!` by the controller once a lane's registration is merged.
pub mod analyze;
pub mod capture;
pub mod core;
pub mod execute;
pub mod i18n;
pub mod indexer;
pub mod mcp;
pub mod providers;
pub mod settings;
pub mod sidecar;
pub mod views;

use openidea_plugin::{Error, Invocation, Plugin, register_plugin};

/// Plugin state. Every command is stateless for now, so nothing is stored yet;
/// state that must survive idle collection will go through `host::storage`.
#[derive(Default)]
pub struct Main;

impl Plugin for Main {
    fn run_command(
        &mut self,
        id: &str,
        ctx: &Invocation,
        arg: Option<&str>,
    ) -> Result<Option<String>, Error> {
        match id {
            commands::render_xml::ID => commands::render_xml::run_with_arg(ctx, arg),
            commands::check_sql::ID => commands::check_sql::run(ctx, arg),
            commands::perf_advice::ID => commands::perf_advice::run(ctx, arg),
            commands::generate_crud::ID => commands::generate_crud::run(ctx, arg),
            commands::rollback_plan::ID => commands::rollback_plan::run(ctx, arg),
            commands::render_log::ID => commands::render_log::run(ctx, arg),
            commands::capture_start::ID => commands::capture_start::run(ctx, arg),
            commands::capture_stop::ID => commands::capture_stop::run(ctx, arg),
            commands::capture_status::ID => commands::capture_status::run(ctx, arg),
            commands::capture_poll::ID => commands::capture_poll::run(ctx, arg),
            commands::capture_open_log::ID => commands::capture_open_log::run(ctx, arg),
            commands::open_logs::ID => commands::open_logs::run(ctx, arg),
            commands::open_workbench::ID => commands::open_workbench::run(ctx, arg),
            commands::preview_sql::ID => commands::preview_sql::run(ctx, arg),
            commands::preview_sql_diff::ID => commands::preview_sql_diff::run(ctx, arg),
            commands::index_stats::ID => commands::index_stats::run(ctx, arg),
            commands::cache_clear::ID => commands::cache_clear::run(ctx, arg),
            commands::build_info::ID => commands::build_info::run(ctx, arg),
            _ => Err(Error::NotFound(format!("command {id}"))),
        }
    }

    /// `websocket-ready` wakes the capture consumer instead of polling: the
    /// bridge drains at most 256 frames, reports host failures as a log line
    /// and never panics, so an event batch cannot fail activation.
    fn handle_events(&mut self, events: &[openidea_plugin::Event], ctx: &Invocation) {
        for event in events {
            if matches!(event, openidea_plugin::Event::WebSocketReady(_)) {
                let _ = crate::capture::bridge::on_websocket_ready(ctx);
            }
        }
    }
}

register_plugin!(
    Main,
    views,
    indexing,
    providers,
    injections,
    inlay_hints,
    diagnostics,
    language_features,
    hover,
    mcp
);

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    #[test]
    fn routes_render_xml_to_its_command() {
        let host = MockHost::install();
        host.add_file(
            "/project/UserMapper.xml",
            r#"<mapper namespace="com.example.UserMapper">
                <select id="findById">SELECT id, name FROM users WHERE id = #{id}</select>
            </mapper>"#,
        );
        let mut plugin = Main;
        let out = plugin
            .run_command(
                commands::render_xml::ID,
                &host.invocation(),
                Some("/project/UserMapper.xml#com.example.UserMapper.findById"),
            )
            .unwrap();
        assert_eq!(out, None);
        let text = &host.output()[commands::CHANNEL];
        assert!(text.contains("-- com.example.UserMapper.findById (exact)"));
        assert!(text.contains("WHERE id = ?"));
    }

    #[test]
    fn check_sql_is_routed_too() {
        let host = MockHost::install();
        let mut plugin = Main;
        plugin
            .run_command(
                commands::check_sql::ID,
                &host.invocation(),
                Some("DELETE FROM users"),
            )
            .unwrap();
        let text = &host.output()[commands::CHANNEL];
        assert!(text.contains("DELETE"));
    }

    #[test]
    fn unknown_command_is_an_error() {
        let host = MockHost::install();
        let mut plugin = Main;
        let err = plugin
            .run_command("no-such-command", &host.invocation(), None)
            .unwrap_err();
        assert_eq!(err, Error::NotFound("command no-such-command".into()));
    }
}
