//! End-to-end checks for the command shells against the mock IDE.
//!
//! `src/lib.rs` keeps `mod commands;` private (the controller owns dispatch), so
//! an integration test cannot `use` the crate's command modules. The `#[path]`
//! declarations compile the same sources into this test binary; the shells
//! exercised here depend only on `crate::core` and `crate::commands`.
//!
//! Test names carry `commands` so the lane gate `cargo test commands` runs them.

// The same sources are compiled a second time here; items the real crate root
// reaches through `lib.rs` look unused from this crate's point of view.
#![allow(dead_code, unused_imports, unused_variables)]

// Only the shells this file exercises. `src/commands/mod.rs` additionally
// declares the capture/workbench shells (lane L12), which reach into
// `crate::capture` and `crate::views`; including the whole tree here would drag
// the view layer and a `Main` plugin instance into this binary.
mod commands {
    #![allow(dead_code, unused_imports)]

    #[path = "../../src/commands/check_sql.rs"]
    pub mod check_sql;
    #[path = "../../src/commands/common.rs"]
    pub mod common;
    #[path = "../../src/commands/generate_crud.rs"]
    pub mod generate_crud;
    #[path = "../../src/commands/perf_advice.rs"]
    pub mod perf_advice;
    #[path = "../../src/commands/render_log.rs"]
    pub mod render_log;
    #[path = "../../src/commands/render_xml.rs"]
    pub mod render_xml;
    #[path = "../../src/commands/rollback_plan.rs"]
    pub mod rollback_plan;

    pub const CHANNEL: &str = common::CHANNEL;
}
#[path = "../src/core/mod.rs"]
mod core;

use openidea_plugin::mock::MockHost;

fn compact(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn commands_render_xml_end_to_end() {
    let host = MockHost::install();
    host.add_file(
        "/project/UserMapper.xml",
        r#"<mapper namespace="com.example.UserMapper">
            <select id="findById">SELECT id, name FROM users WHERE id = #{id}</select>
        </mapper>"#,
    );
    commands::render_xml::run_with_arg(
        &host.invocation(),
        Some("/project/UserMapper.xml#com.example.UserMapper.findById"),
    )
    .unwrap();
    let text = &host.output()[commands::CHANNEL];
    assert!(text.contains("-- com.example.UserMapper.findById (exact)"));
    assert!(compact(text).contains("SELECT id, name FROM users WHERE id = ?"));
}

#[test]
fn commands_render_xml_reports_a_missing_file() {
    let host = MockHost::install();
    let err = commands::render_xml::run_with_arg(&host.invocation(), Some("/project/none.xml"))
        .unwrap_err();
    assert!(
        err.to_string()
            .contains(commands::common::KEY_FILE_UNREADABLE)
    );
}

#[test]
fn commands_check_sql_end_to_end() {
    let host = MockHost::install();
    host.add_file("/project/check.sql", "DROP TABLE user");
    commands::check_sql::run(&host.invocation(), Some("@/project/check.sql")).unwrap();
    let text = &host.output()[commands::CHANNEL];
    assert!(text.contains("source: /project/check.sql"));
    assert!(text.contains("class: DDL"));
    assert!(text.contains("destructive: YES (DROP)"));
}

#[test]
fn commands_perf_advice_end_to_end() {
    let host = MockHost::install();
    commands::perf_advice::run(
        &host.invocation(),
        Some("SELECT id FROM user ORDER BY RAND()"),
    )
    .unwrap();
    let text = &host.output()[commands::CHANNEL];
    assert!(text.contains("score: "));
    assert!(text.contains("order_by_rand perf.order_by_rand"));
}

#[test]
fn commands_generate_crud_end_to_end() {
    let host = MockHost::install();
    commands::generate_crud::run(
        &host.invocation(),
        Some(r#"{"table":"user","columns":["id","name"],"pk":["id"],"dialect":"mysql"}"#),
    )
    .unwrap();
    let text = &host.output()[commands::CHANNEL];
    assert!(text.contains("-- select_by_id"));
    assert!(text.contains("SELECT `id`, `name` FROM `user` WHERE `id` = ?"));
}

#[test]
fn commands_rollback_plan_refuses_without_a_primary_key() {
    let host = MockHost::install();
    commands::rollback_plan::run(
        &host.invocation(),
        Some(r#"{"sql":"DELETE FROM log WHERE message = 'x'","table":"log","columns":["message"],"dialect":"mysql"}"#),
    )
    .unwrap();
    let text = &host.output()[commands::CHANNEL];
    assert!(text.contains("-- refused: gen.error.no_primary_key"));
    assert!(!text.contains("INSERT INTO"));
}

#[test]
fn commands_render_log_end_to_end() {
    let host = MockHost::install();
    host.add_file(
        "/project/app.log",
        "==>  Preparing: SELECT id FROM user WHERE id = ?\n==> Parameters: 7(Long)\n",
    );
    commands::render_log::run(&host.invocation(), Some("/project/app.log")).unwrap();
    let text = &host.output()[commands::CHANNEL];
    assert!(text.contains("category=mybatis/select"));
    assert!(text.contains("SELECT id FROM user WHERE id = 7"));
}
