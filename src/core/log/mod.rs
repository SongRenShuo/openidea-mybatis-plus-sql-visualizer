//! SQL log parsing (T1-11).
//!
//! Four console families are recognised in one pass:
//!
//! | Family | Reference source | Shapes |
//! | --- | --- | --- |
//! | MyBatis | `MyBatisLogParser` | `Preparing:` / `Parameters:` / `Total:` |
//! | p6spy | `LogDurationParser` + `ExecutionTimeParser` | `ts\|ms\|statement\|sql`, `elapsed N ms \| exec` |
//! | Hibernate | `HibernateLogParser` | `Hibernate:`, `/* ... */`, `binding parameter [...]` |
//! | JdbcTemplate | `JdbcTemplateLogParser` | `Executing prepared SQL statement [...]`, `... column index N ...` |
//!
//! The reference repository has no standalone p6spy SQL parser — its p6spy
//! knowledge lives in the two duration modules above; the fourth standalone
//! parser there is ShardingSphere (`Logic SQL:` / `Actual SQL:`), which is not
//! one of the four families required by T1-11.
//!
//! Entry points:
//! - [`parse`] — single pass over a whole text, returns [`LogAnalysis`].
//! - [`Parser`] — streaming variant for the capture bridge.
//!
//! No platform types, no `host::` calls; every shape is `serde`-serialisable so
//! the panel and the MCP tools consume the same contract.

mod duration;
mod hibernate;
mod jdbc;
mod model;
mod mybatis;
mod p6spy;
mod params;
mod parser;
mod restore;
mod text;
mod timestamp;

#[cfg(test)]
mod tests;

pub use model::{
    Confidence, DuplicateGroup, EntryClass, Family, LogAnalysis, LogEntry, NPlusOneGroup,
    ParseOptions, UnparsedLine, message_keys,
};
pub use parser::{Parser, classify_sql, parse};
