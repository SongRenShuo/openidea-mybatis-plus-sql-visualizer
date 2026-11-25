//! String-aware `?` placeholder substitution (reference `LogSqlRestorer` and
//! `PlaceholderPositions`).

use super::params::{Param, render_param};

/// Result of restoring one statement.
pub(crate) struct Restored {
    pub sql: String,
    pub placeholders: usize,
    pub used: usize,
}

/// Replace the first `params.len()` placeholders in order. Extra parameters are
/// ignored; missing ones stay `?`.
pub(crate) fn restore(sql: &str, params: &[Param]) -> Restored {
    let literals: Vec<String> = params.iter().map(render_param).collect();
    restore_literals(sql, &literals)
}

/// The same substitution for callers that already hold rendered SQL literals
/// (`LogEntry::params`): placeholder order and the string/comment scanner are
/// shared with [`restore`], so no quoting happens twice.
pub(crate) fn restore_literals(sql: &str, literals: &[String]) -> Restored {
    let positions = placeholder_positions(sql);
    if positions.is_empty() {
        return Restored {
            sql: sql.to_string(),
            placeholders: 0,
            used: 0,
        };
    }
    let mut out = String::with_capacity(sql.len() + literals.len() * 8);
    let mut prev = 0usize;
    let mut used = 0usize;
    for pos in &positions {
        if used >= literals.len() {
            break;
        }
        out.push_str(&sql[prev..*pos]);
        out.push_str(&literals[used]);
        prev = pos + 1;
        used += 1;
    }
    out.push_str(&sql[prev..]);
    Restored {
        sql: out,
        placeholders: positions.len(),
        used,
    }
}

/// Positions of `?` outside single-quoted strings, `--` comments and `/* */`
/// comments. Double-quoted identifiers are not protected (standard SQL
/// identifiers do not contain `?`), matching the reference.
pub(crate) fn placeholder_positions(sql: &str) -> Vec<usize> {
    let b = sql.as_bytes();
    let mut positions = Vec::new();
    let mut i = 0usize;
    let mut in_string = false;
    while i < b.len() {
        let c = b[i];
        if in_string {
            if c == b'\'' {
                if i + 1 < b.len() && b[i + 1] == b'\'' {
                    i += 2;
                    continue;
                }
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == b'\'' {
            in_string = true;
            i += 1;
        } else if c == b'-' && i + 1 < b.len() && b[i + 1] == b'-' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
        } else if c == b'?' {
            positions.push(i);
            i += 1;
        } else {
            i += 1;
        }
    }
    positions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::log::params::Param;

    fn p(value: &str, ty: &str) -> Param {
        Param {
            value: Some(value.to_string()),
            ty: Some(ty.to_string()),
        }
    }

    #[test]
    fn replaces_in_order_and_quotes_strings() {
        let r = restore(
            "select * from t where a = ? and b = ?",
            &[p("1", "Long"), p("x'y", "String")],
        );
        assert_eq!(r.sql, "select * from t where a = 1 and b = 'x''y'");
        assert_eq!(r.placeholders, 2);
        assert_eq!(r.used, 2);
    }

    #[test]
    fn ignores_question_marks_inside_strings_and_comments() {
        let sql = "select '?' as q, c from t -- ?\n where id = ? /* ? */";
        let expected = sql.find("where id = ?").expect("marker") + "where id = ".len();
        assert_eq!(placeholder_positions(sql), vec![expected]);
    }

    #[test]
    fn keeps_missing_placeholders() {
        let r = restore("select ? , ?", &[p("1", "Long")]);
        assert_eq!(r.sql, "select 1 , ?");
        assert_eq!(r.used, 1);
    }

    #[test]
    fn rendered_literals_are_substituted_without_requoting() {
        let literals = vec!["1".to_string(), "'a''b'".to_string()];
        let r = restore_literals("select ? as a, ? as b, ? as c", &literals);
        assert_eq!(r.sql, "select 1 as a, 'a''b' as b, ? as c");
        assert_eq!(r.placeholders, 3);
        assert_eq!(r.used, 2);
        // Already-rendered text is never quoted a second time.
        assert!(!r.sql.contains("'''"));
    }
}
