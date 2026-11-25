//! Deterministic 0–100 complexity score.
//!
//! Weights mirror the IntelliJ `ComplexityScorer` so the same query ranks the
//! same way, but the public level vocabulary is the three-tier
//! `low` / `medium` / `high` required by this port.
//!
//! The score depends on the SQL text only (no dialect), which keeps it
//! reproducible across runs and trivial to unit test.

use serde::{Deserialize, Serialize};

use super::features::Features;
use super::sql::{Analysis, StmtKind};

/// Three-tier complexity bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ComplexityLevel {
    Low,
    Medium,
    High,
}

/// One weighted contributor to the total score.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScorePart {
    pub factor: String,
    pub points: u32,
}

/// Complexity result (`score` is 0–100).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Complexity {
    pub score: u32,
    pub level: ComplexityLevel,
    /// `false` when the text does not look like a SQL statement; the score is
    /// then `0` rather than a misleading number.
    pub recognized: bool,
    pub breakdown: Vec<ScorePart>,
}

/// Weight constants (identical to the IntelliJ scorer).
const TABLE_COUNT: u32 = 3;
const JOIN_PER_TABLE: u32 = 5;
const SUBQUERY_DEPTH: u32 = 10;
const UNION: u32 = 6;
const CTE: u32 = 8;
const OR_CONDITION: u32 = 2;
const IN_ITEMS_OVER_100: u32 = 10;
const NESTED_SUBQUERY: u32 = 12;
const SELECT_STAR: u32 = 10;
const LEADING_WILDCARD: u32 = 8;
const MISSING_WHERE: u32 = 15;
const MISSING_WHERE_DML: u32 = 50;
const NO_LIMIT: u32 = 5;
const FUNCTION_ON_COL: u32 = 7;
const ORDER_BY_RAND: u32 = 10;
const DISTINCT: u32 = 3;
const GROUP_BY: u32 = 2;
const HAVING: u32 = 4;
const MAX_SCORE: u32 = 100;

/// Compute the complexity score for `sql`.
pub fn score(sql: &str) -> Complexity {
    let sa = Analysis::parse(sql);
    let f = Features::extract(&sa);
    if sql.trim().is_empty() {
        return Complexity {
            score: 0,
            level: ComplexityLevel::Low,
            recognized: false,
            breakdown: Vec::new(),
        };
    }

    let mut parts: Vec<ScorePart> = Vec::new();
    let mut add = |factor: &str, points: u32| {
        if points > 0 {
            parts.push(ScorePart {
                factor: factor.to_string(),
                points,
            });
        }
    };

    add("tableCount", f.table_count as u32 * TABLE_COUNT);
    add("joinCount", f.join_count as u32 * JOIN_PER_TABLE);
    add("subqueryDepth", f.subquery_depth * SUBQUERY_DEPTH);
    add("union", f.union_count as u32 * UNION);
    add("cte", f.cte_count as u32 * CTE);
    add("orCondition", f.or_count as u32 * OR_CONDITION);
    add(
        "inClauseOverflow",
        f.big_in_lists as u32 * IN_ITEMS_OVER_100,
    );
    add("selectStar", if f.select_star { SELECT_STAR } else { 0 });
    add(
        "leadingWildcard",
        if f.leading_wildcard {
            LEADING_WILDCARD
        } else {
            0
        },
    );
    if !f.has_where
        && matches!(
            sa.stmt,
            StmtKind::Select | StmtKind::Update | StmtKind::Delete
        )
    {
        let points = if matches!(sa.stmt, StmtKind::Update | StmtKind::Delete) {
            MISSING_WHERE_DML
        } else {
            MISSING_WHERE
        };
        add("missingWhere", points);
    }
    add("noLimit", if f.no_limit { NO_LIMIT } else { 0 });
    add(
        "functionOnCol",
        if f.function_on_column {
            FUNCTION_ON_COL
        } else {
            0
        },
    );
    add(
        "orderByRand",
        if f.order_by_rand { ORDER_BY_RAND } else { 0 },
    );
    add("distinct", if f.distinct { DISTINCT } else { 0 });
    add("groupBy", if f.group_by { GROUP_BY } else { 0 });
    add("having", if f.having { HAVING } else { 0 });
    if f.subquery_depth >= 2 {
        add("nestedSubquery", NESTED_SUBQUERY);
    }

    let total = parts.iter().map(|p| p.points).sum::<u32>().min(MAX_SCORE);
    Complexity {
        score: total,
        level: level_of(total),
        recognized: f.recognized,
        breakdown: parts,
    }
}

fn level_of(score: u32) -> ComplexityLevel {
    match score {
        0..=39 => ComplexityLevel::Low,
        40..=69 => ComplexityLevel::Medium,
        _ => ComplexityLevel::High,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_sql_scores_zero_and_is_not_recognized() {
        let c = score("   ");
        assert_eq!(c.score, 0);
        assert_eq!(c.level, ComplexityLevel::Low);
        assert!(!c.recognized);
    }

    #[test]
    fn simple_primary_key_query_is_low() {
        let c = score("SELECT id, name FROM user WHERE id = ? LIMIT 1");
        assert_eq!(c.level, ComplexityLevel::Low);
        assert!(c.score > 0 && c.score < 40, "score={}", c.score);
        assert!(c.breakdown.iter().any(|p| p.factor == "tableCount"));
    }

    #[test]
    fn select_star_join_without_where_is_medium() {
        let c = score("SELECT * FROM user u JOIN address a ON a.user_id = u.id");
        assert_eq!(c.score, 41);
        assert_eq!(c.level, ComplexityLevel::Medium);
        assert!(c.breakdown.iter().any(|p| p.factor == "selectStar"));
        assert!(c.breakdown.iter().any(|p| p.factor == "missingWhere"));
    }

    #[test]
    fn heavy_query_reaches_high_and_caps_at_100() {
        let sql = "SELECT DISTINCT * FROM a JOIN b ON b.id = a.id JOIN c ON c.id = b.id JOIN d ON d.id = c.id \
                   WHERE x LIKE '%foo%' OR a = 1 OR b = 2 OR c = 3 OR d = 4 \
                   UNION SELECT * FROM e UNION SELECT * FROM f ORDER BY RAND()";
        let c = score(sql);
        assert_eq!(c.level, ComplexityLevel::High);
        assert!(c.score <= 100);
    }

    #[test]
    fn update_without_where_gets_the_dml_weight() {
        let c = score("UPDATE user SET name = 'x'");
        assert!(
            c.breakdown
                .iter()
                .any(|p| p.factor == "missingWhere" && p.points == 50)
        );
        assert_eq!(c.level, ComplexityLevel::Medium);
    }

    #[test]
    fn score_is_deterministic() {
        let sql = "SELECT a, b FROM t1 LEFT JOIN t2 ON t2.id = t1.id WHERE a IN (SELECT a FROM t3)";
        assert_eq!(score(sql), score(sql));
    }

    #[test]
    fn cte_and_union_add_points() {
        let plain = score("SELECT id FROM t WHERE id = ?");
        let with_cte = score("WITH x AS (SELECT id FROM t) SELECT id FROM x WHERE id = ?");
        assert!(with_cte.score > plain.score);
        assert!(with_cte.breakdown.iter().any(|p| p.factor == "cte"));
    }
}
