//! The plugin's commands, one module per `[[contributes.commands]]` entry.
//!
//! Keep the id constant next to its implementation: `plugin.toml` and the
//! dispatch in `lib.rs` both name it, and a typo in either is silent at build
//! time.
//!
//! Every command is a thin shell: parse the argument, read a project file, call
//! one pure entry point in [`crate::core`], format the result into the shared
//! Output channel. The channel lives here so all commands write to the same tab.

// The shells become reachable from the crate root when the controller merges
// this lane's dispatch; until then only `render-xml` is wired, so the others
// would trip `dead_code` in a non-test build.
#![allow(dead_code)]

pub mod capture_open_log;
pub mod capture_poll;
pub mod capture_start;
pub mod capture_status;
pub mod capture_stop;
pub mod check_sql;
pub mod common;
pub mod generate_crud;
pub mod open_logs;
pub mod open_workbench;
pub mod perf_advice;
pub mod render_log;
pub mod render_xml;
pub mod rollback_plan;

/// The Output tab every command writes to.
pub const CHANNEL: &str = common::CHANNEL;
