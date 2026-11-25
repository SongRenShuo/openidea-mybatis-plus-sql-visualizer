//! Index advice derived from the predicates actually present in the SQL.
//!
//! The advisory never invents tables or columns: table names come from
//! `FROM`/`JOIN`, column names from `WHERE`/`ORDER BY`, and an existing index
//! covers a suggestion when the candidate column list is a prefix of it.
//! Column order matters (equality first, then range/order), mirroring how a
//! B-tree index is used.

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::sql::{Analysis, PredKind, predicates};

/// A known index on a table (metadata supplied by the caller).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexDef {
    /// Table name the index belongs to.
    pub table: String,
    pub name: String,
    pub columns: Vec<String>,
    #[serde(default)]
    pub unique: bool,
}

/// One index suggestion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexAdvice {
    pub table: String,
    /// Suggested leading column combination, in index order.
    pub columns: Vec<String>,
    /// Localization key for the reason.
    pub reason_key: String,
    /// Advisory output is always `partial`: it is derived from the SQL text
    /// alone, without statistics or cardinality.
    pub confidence: String,
    pub detail: serde_json::Value,
}

/// Suggest indexes for the tables referenced by `sql`.
pub fn index_advice(sql: &str, indexes: &[IndexDef]) -> Vec<IndexAdvice> {
    let sa = Analysis::parse(sql);
    if sa.table_refs().is_empty() {
        return Vec::new();
    }
    let preds = predicates(&sa);
    let mut out: Vec<IndexAdvice> = Vec::new();

    for tr in sa.table_refs() {
        let table = &tr.name;
        let mut eq: Vec<&str> = Vec::new();
        let mut range: Vec<&str> = Vec::new();
        let mut order: Vec<&str> = Vec::new();
        let mut unusable: Vec<&str> = Vec::new();

        for p in preds.iter().filter(|p| {
            p.table
                .as_deref()
                .is_some_and(|t| t.eq_ignore_ascii_case(table))
        }) {
            match p.kind {
                PredKind::Eq => push_unique(&mut eq, &p.column),
                PredKind::Range | PredKind::Between | PredKind::In | PredKind::PrefixLike => {
                    push_unique(&mut range, &p.column)
                }
                PredKind::LeadingLike => push_unique(&mut unusable, &p.column),
                PredKind::OrderBy => push_unique(&mut order, &p.column),
            }
        }

        // A leading wildcard makes a B-tree index unusable; only keep the column
        // when another predicate shape can still use it.
        unusable.retain(|c| {
            !eq.iter().any(|e| e.eq_ignore_ascii_case(c))
                && !range.iter().any(|r| r.eq_ignore_ascii_case(c))
        });

        let mut candidate: Vec<String> = Vec::new();
        for c in eq.iter() {
            push_unique_string(&mut candidate, c);
        }
        for c in range.iter() {
            push_unique_string(&mut candidate, c);
        }
        let order_only = candidate.is_empty() && !order.is_empty();
        if candidate.is_empty() {
            for c in order.iter() {
                push_unique_string(&mut candidate, c);
            }
        }
        if candidate.is_empty() {
            continue;
        }

        let known: Vec<&IndexDef> = indexes
            .iter()
            .filter(|i| table_matches(&i.table, table))
            .collect();
        if known.iter().any(|idx| covers(&idx.columns, &candidate)) {
            continue;
        }

        let reason = if order_only {
            "perf.index.suggest_order_by"
        } else if candidate.len() >= 2 {
            "perf.index.suggest_composite"
        } else {
            "perf.index.suggest_single"
        };
        let advice = IndexAdvice {
            table: table.clone(),
            columns: candidate.clone(),
            reason_key: reason.to_string(),
            confidence: "partial".to_string(),
            detail: json!({
                "table": table,
                "equality_columns": eq,
                "range_columns": range,
                "order_by_columns": order,
                "unusable_columns": unusable,
                "existing_indexes": known.iter().map(|i| i.name.clone()).collect::<Vec<_>>(),
                "heuristic": true,
            }),
        };
        if !out
            .iter()
            .any(|a| a.table.eq_ignore_ascii_case(&advice.table) && a.columns == advice.columns)
        {
            out.push(advice);
        }
    }
    out
}

fn push_unique<'a>(list: &mut Vec<&'a str>, value: &'a str) {
    if !list.iter().any(|v| v.eq_ignore_ascii_case(value)) {
        list.push(value);
    }
}

fn push_unique_string(list: &mut Vec<String>, value: &str) {
    if !list.iter().any(|v| v.eq_ignore_ascii_case(value)) {
        list.push(value.to_string());
    }
}

fn table_matches(known: &str, table: &str) -> bool {
    known.eq_ignore_ascii_case(table)
        || known
            .rsplit('.')
            .next()
            .is_some_and(|n| n.eq_ignore_ascii_case(table.rsplit('.').next().unwrap_or(table)))
}

/// Does `existing` cover `candidate` as a leading prefix?
fn covers(existing: &[String], candidate: &[String]) -> bool {
    if existing.len() < candidate.len() {
        return false;
    }
    existing
        .iter()
        .zip(candidate.iter())
        .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idx(table: &str, columns: &[&str]) -> IndexDef {
        IndexDef {
            table: table.to_string(),
            name: format!("idx_{}_{}", table, columns.join("_")),
            columns: columns.iter().map(|c| c.to_string()).collect(),
            unique: false,
        }
    }

    #[test]
    fn single_equality_predicate_gets_a_single_column_suggestion() {
        let advice = index_advice("SELECT * FROM user WHERE status = 1", &[]);
        assert_eq!(advice.len(), 1);
        assert_eq!(advice[0].table, "user");
        assert_eq!(advice[0].columns, vec!["status"]);
        assert_eq!(advice[0].reason_key, "perf.index.suggest_single");
        assert_eq!(advice[0].confidence, "partial");
    }

    #[test]
    fn two_equalities_suggest_composite_equality_first() {
        let advice = index_advice(
            "SELECT id FROM orders WHERE status = ? AND user_id = ?",
            &[],
        );
        assert_eq!(advice[0].columns, vec!["status", "user_id"]);
        assert_eq!(advice[0].reason_key, "perf.index.suggest_composite");
    }

    #[test]
    fn existing_index_with_same_prefix_suppresses_the_advice() {
        let indexes = vec![idx("orders", &["user_id", "status"])];
        // Existing index columns do not start with the candidate order
        // (status, user_id), so the advice is still produced.
        let advice = index_advice(
            "SELECT id FROM orders WHERE status = ? AND user_id = ?",
            &indexes,
        );
        assert_eq!(advice.len(), 1);
        // With equality columns in the other order it is covered.
        let advice = index_advice(
            "SELECT id FROM orders WHERE user_id = ? AND status = ?",
            &indexes,
        );
        assert!(advice.is_empty());
    }

    #[test]
    fn order_by_without_where_suggests_an_order_by_index() {
        let advice = index_advice("SELECT id FROM user ORDER BY created_at DESC", &[]);
        assert_eq!(advice.len(), 1);
        assert_eq!(advice[0].columns, vec!["created_at"]);
        assert_eq!(advice[0].reason_key, "perf.index.suggest_order_by");
    }

    #[test]
    fn leading_wildcard_alone_produces_no_advice() {
        let advice = index_advice("SELECT id FROM user WHERE name LIKE '%foo'", &[]);
        assert!(advice.is_empty());
    }

    #[test]
    fn multi_table_query_only_advises_tables_that_appear_in_the_sql() {
        let advice = index_advice(
            "SELECT u.id FROM user u JOIN address a ON a.user_id = u.id WHERE u.status = ?",
            &[idx("user", &["id"])],
        );
        assert_eq!(advice.len(), 1);
        assert_eq!(advice[0].table, "user");
        assert_eq!(advice[0].columns, vec!["status"]);
    }

    #[test]
    fn no_table_reference_means_no_advice() {
        assert!(index_advice("SELECT 1", &[]).is_empty());
    }
}
