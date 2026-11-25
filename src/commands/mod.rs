//! The plugin's commands, one module per `[[contributes.commands]]` entry.
//!
//! Keep the id constant next to its implementation: `plugin.toml` and the
//! dispatch in `lib.rs` both name it, and a typo in either is silent at build
//! time.

pub mod render_xml;
