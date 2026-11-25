//! End-to-end checks for the project-view argument form (L16).
//!
//! The context-menu entry hands the command a plain file path — no `@` prefix
//! and no inline text. These tests exercise that exact shape through the real
//! command shells against the mock IDE.
//!
//! `src/lib.rs` keeps `mod commands;` private, so the sources are compiled into
//! this test binary through `#[path]`, the same trick `tests/l11_commands.rs`
//! uses. Test names carry `commands` so the lane gate `cargo test commands`
//! runs them.

// The sources are compiled a second time here; items the real crate root
// reaches through `lib.rs` look unused from this crate's point of view.
#![allow(dead_code, unused_imports, unused_variables)]

#[path = "../src/commands/check_sql.rs"]
pub mod check_sql;
#[path = "../src/commands/common.rs"]
pub mod common;
#[path = "../src/commands/perf_advice.rs"]
pub mod perf_advice;

mod commands {
    #![allow(dead_code, unused_imports)]

    pub use super::{check_sql, common, perf_advice};

    pub const CHANNEL: &str = common::CHANNEL;
}

#[path = "../src/core/mod.rs"]
mod core;

use openidea_plugin::mock::MockHost;

/// The path shape a project-view context menu passes: absolute, no `@`.
const PICKED: &str = "/project/sql/pick.sql";

const SQL: &str = "SELECT * FROM user WHERE name LIKE '%abc' ORDER BY RAND()";

#[test]
fn commands_accept_a_context_menu_path_for_check_sql() {
    let host = MockHost::install();
    host.add_file(PICKED, SQL);
    commands::check_sql::run(&host.invocation(), Some(PICKED)).unwrap();
    let text = &host.output()[commands::CHANNEL];
    assert!(text.contains("source: /project/sql/pick.sql"), "{text}");
    assert!(text.contains("class: QUERY"), "{text}");
    assert!(text.contains("select_star perf.select_star"), "{text}");
}

#[test]
fn commands_accept_a_context_menu_path_for_perf_advice() {
    let host = MockHost::install();
    host.add_file(PICKED, SQL);
    commands::perf_advice::run(&host.invocation(), Some(PICKED)).unwrap();
    let text = &host.output()[commands::CHANNEL];
    assert!(text.contains("source: /project/sql/pick.sql"), "{text}");
    assert!(text.contains("order_by_rand perf.order_by_rand"), "{text}");
    assert!(text.contains("leading_wildcard_like"), "{text}");
}

#[test]
fn commands_keep_inline_sql_with_spaces_inline() {
    let host = MockHost::install();
    // A file with this literal name exists; the space must still win as text.
    host.add_file("SELECT 1", "DROP TABLE user");
    commands::check_sql::run(&host.invocation(), Some("SELECT 1")).unwrap();
    let text = &host.output()[commands::CHANNEL];
    assert!(!text.contains("source:"), "{text}");
    assert!(text.contains("class: QUERY"), "{text}");
    assert!(!text.contains("DDL"), "{text}");
}
