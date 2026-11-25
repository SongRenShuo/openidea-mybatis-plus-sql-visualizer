//! Pure logic shared by every surface: no `host::` calls, no platform types.
//!
//! Each submodule is owned by one implementation lane (see
//! `docs/ORCHESTRATION.md` §3). The module tree is declared here once so that
//! lanes never need to touch `src/lib.rs`; a lane only edits files inside its
//! own directory.

pub mod annotation;
pub mod dialect;
pub mod format;
pub mod r#gen;
pub mod ibatis;
pub mod log;
pub mod params;
pub mod perf;
pub mod xml;
