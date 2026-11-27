//! Text → WrapperChain analysis (T5-4).
//!
//! Lane L8 owns this directory; see `docs/TASKS.md` T5-4.
//!
//! Lane F (wiring) added two text-level readers next to the wrapper renderer:
//! [`entity`] turns an entity class into the field/column projection the
//! wrapper needs, and [`sources`] answers "which statement source is this text"
//! for annotations, iBatis 2 and wrapper chains.

pub mod entity;
pub mod sources;
pub mod wrapper;

pub use wrapper::{Confidence, EntityColumns, WrapperChain, WrapperKind, WrapperOutput};
