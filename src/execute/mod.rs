//! EXPLAIN execution and schema cache (T5-5~T5-6).
//!
//! Lane L8 owns this directory; see `docs/TASKS.md` T5-5~T5-6.

pub mod explain;
pub mod schema;

#[cfg(test)]
mod parity;

pub use explain::{ExplainError, ExplainErrorKind, ExplainNode, ExplainPlan, ExplainRow};
pub use schema::{ColumnSchema, IndexSchema, SchemaCache, SchemaMiss, TableSchema};
