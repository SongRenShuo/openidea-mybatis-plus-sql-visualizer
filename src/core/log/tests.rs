//! Parity tests for T1-11.
//!
//! Node 1 ships the four family parsers together with the per-module tests
//! (`mybatis` / `hibernate` / `jdbc` / `p6spy` / `timestamp` / `duration` /
//! `params` / `restore`). The cross-family parity suite lands in node 2 and the
//! performance/boundary suite in node 3.

use super::*;

#[test]
fn parse_returns_entries_for_each_family() {
    let analysis = parse(
        "==>  Preparing: SELECT 1\nHibernate: select 2\nExecuting prepared SQL statement [SELECT 3]\n1723456789012|1|statement|SELECT 4",
        &ParseOptions::default(),
    );
    assert_eq!(analysis.entries.len(), 4);
}
