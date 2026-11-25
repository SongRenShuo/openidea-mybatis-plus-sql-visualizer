//! Generation (T1-10): CRUD, SQL→MyBatis-Plus Java, rollback plans.
//!
//! All generators are pure and return either a deterministic artifact or a
//! `GenError`. Nothing is guessed: an unsupported shape yields an explicit
//! `unresolved` result carrying a `message_key`, never half-correct SQL or
//! code. Quote style follows the dialect name supplied by the caller; no extra
//! dialect type is required.

mod crud;
mod java;
mod rollback;

pub use crud::{CrudKind, CrudStatement, crud, crud_batch};
pub use java::{JavaCode, to_mybatis_plus_java};
pub use rollback::{RollbackKind, RollbackPlan, rollback_plan};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::perf::dialect::{DialectProfile, profile};

/// Confidence of a generated artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Exact,
    Partial,
    Unresolved,
}

/// A reason a condition or statement could not be mapped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Unresolved {
    pub message_key: String,
    pub detail: Value,
}

impl Unresolved {
    pub(crate) fn new(message_key: &str, detail: Value) -> Self {
        Self {
            message_key: message_key.to_string(),
            detail,
        }
    }
}

/// One table column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnSpec {
    pub name: String,
    #[serde(default)]
    pub primary_key: bool,
    #[serde(default)]
    pub auto_increment: bool,
}

impl ColumnSpec {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            primary_key: false,
            auto_increment: false,
        }
    }

    pub fn primary_key(mut self) -> Self {
        self.primary_key = true;
        self
    }

    pub fn auto_increment(mut self) -> Self {
        self.auto_increment = true;
        self
    }
}

/// The minimal table description generators need.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableSpec {
    pub name: String,
    #[serde(default)]
    pub schema: Option<String>,
    pub columns: Vec<ColumnSpec>,
}

impl TableSpec {
    pub fn new(name: impl Into<String>, columns: Vec<ColumnSpec>) -> Self {
        Self {
            name: name.into(),
            schema: None,
            columns,
        }
    }

    pub fn with_schema(mut self, schema: impl Into<String>) -> Self {
        self.schema = Some(schema.into());
        self
    }

    pub(crate) fn primary_keys(&self) -> Vec<&ColumnSpec> {
        self.columns.iter().filter(|c| c.primary_key).collect()
    }

    pub(crate) fn writable_columns(&self) -> Vec<&ColumnSpec> {
        self.columns.iter().filter(|c| !c.auto_increment).collect()
    }
}

/// Errors that prevent generation outright.
#[derive(Debug, Clone, PartialEq)]
pub enum GenError {
    /// The statement or table has no primary key; locating a row is impossible.
    NoPrimaryKey,
    /// No usable columns (or no writable columns for an UPDATE).
    NoColumns,
    /// Empty SQL input.
    EmptySql,
    /// The dialect name is unknown, so quoting would be a guess.
    UnknownDialect(String),
    /// The input is recognised but cannot be generated faithfully.
    Unresolved { message_key: String, detail: Value },
}

impl GenError {
    pub(crate) fn unresolved(message_key: &str, detail: Value) -> Self {
        GenError::Unresolved {
            message_key: message_key.to_string(),
            detail,
        }
    }

    /// Stable message key for localisation; `None` for structural errors that
    /// have their own enum variants.
    pub fn message_key(&self) -> Option<&str> {
        match self {
            GenError::Unresolved { message_key, .. } => Some(message_key),
            _ => None,
        }
    }
}

impl std::fmt::Display for GenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GenError::NoPrimaryKey => write!(f, "gen.error.no_primary_key"),
            GenError::NoColumns => write!(f, "gen.error.no_columns"),
            GenError::EmptySql => write!(f, "gen.error.empty_sql"),
            GenError::UnknownDialect(name) => write!(f, "gen.error.unknown_dialect: {name}"),
            GenError::Unresolved { message_key, .. } => write!(f, "{message_key}"),
        }
    }
}

impl std::error::Error for GenError {}

/// Validate the table description and resolve the dialect.
pub(crate) fn prepare(spec: &TableSpec, dialect: &str) -> Result<DialectProfile, GenError> {
    if spec.columns.is_empty() {
        return Err(GenError::NoColumns);
    }
    if spec.columns.iter().any(|c| c.name.trim().is_empty()) {
        return Err(GenError::unresolved(
            "gen.error.empty_column_name",
            serde_json::json!({ "table": spec.name }),
        ));
    }
    profile(dialect).ok_or_else(|| GenError::UnknownDialect(dialect.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_and_table_builders_are_ergonomic() {
        let table = TableSpec::new(
            "user",
            vec![
                ColumnSpec::new("id").primary_key().auto_increment(),
                ColumnSpec::new("name"),
            ],
        )
        .with_schema("app");
        assert_eq!(table.primary_keys().len(), 1);
        assert_eq!(table.writable_columns().len(), 1);
        assert_eq!(table.schema.as_deref(), Some("app"));
    }

    #[test]
    fn gen_error_has_stable_message_keys() {
        assert_eq!(
            GenError::NoPrimaryKey.to_string(),
            "gen.error.no_primary_key"
        );
        assert_eq!(GenError::NoColumns.to_string(), "gen.error.no_columns");
        assert_eq!(GenError::EmptySql.to_string(), "gen.error.empty_sql");
        let unresolved = GenError::unresolved("gen.test", serde_json::json!({}));
        assert_eq!(unresolved.message_key(), Some("gen.test"));
    }

    #[test]
    fn confidence_is_serde_lowercase() {
        let json = serde_json::to_string(&Confidence::Unresolved).unwrap();
        assert_eq!(json, "\"unresolved\"");
    }
}
