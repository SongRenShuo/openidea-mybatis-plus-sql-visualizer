//! Dialect facts needed by this lane without depending on the dialect module
//! owned by lane L2: accepted identifier quote characters (for the quote
//! mismatch rule) and the canonical quoting style used by the generators.
//!
//! Dialect names arrive as plain strings from the caller. Unknown names are
//! rejected by the generators and make the quote rule stay silent, matching the
//! reference behaviour ("no context, do not judge").

/// Identifier quoting style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuoteStyle {
    /// `` `name` `` — MySQL family.
    Backtick,
    /// `"name"` — ANSI-style dialects.
    Double,
    /// `[name]` — SQL Server.
    Bracket,
}

/// Facts derived from a dialect name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DialectProfile {
    /// Canonical display name used in finding details.
    pub name: &'static str,
    /// Quote characters this dialect accepts for identifiers.
    pub accepted_quotes: &'static [char],
    /// Canonical quoting style for generated SQL.
    pub style: QuoteStyle,
    /// MySQL family: a double-quoted identifier is only valid when the session
    /// has `ANSI_QUOTES`, so the rule may only warn there.
    pub mysql_family: bool,
}

impl DialectProfile {
    /// Quote `name` using the dialect's canonical style.
    pub fn quote(&self, name: &str) -> String {
        match self.style {
            QuoteStyle::Backtick => format!("`{}`", name.replace('`', "``")),
            QuoteStyle::Double => format!("\"{}\"", name.replace('"', "\"\"")),
            QuoteStyle::Bracket => format!("[{}]", name.replace(']', "]]")),
        }
    }

    /// Quote a table name, prefixing an explicit schema when present.
    pub fn quote_table(&self, schema: Option<&str>, table: &str) -> String {
        match schema.filter(|s| !s.is_empty()) {
            Some(s) => format!("{}.{}", self.quote(s), self.quote(table)),
            None => self.quote(table),
        }
    }

    pub fn accepts(&self, quote: char) -> bool {
        self.accepted_quotes.contains(&quote)
    }
}

const MYSQL: &[char] = &['`'];
const PG: &[char] = &['"'];
const ANSI: &[char] = &['"'];
const SQLSERVER: &[char] = &['[', '"'];
const LOOSE: &[char] = &['"', '`', '['];

/// Resolve a dialect name (case-insensitive, common aliases included).
pub(crate) fn profile(name: &str) -> Option<DialectProfile> {
    let key = name.trim().to_ascii_lowercase();
    let key = key.replace([' ', '-', '_'], "");
    Some(match key.as_str() {
        "mysql" => DialectProfile {
            name: "MySQL",
            accepted_quotes: MYSQL,
            style: QuoteStyle::Backtick,
            mysql_family: true,
        },
        "mariadb" => DialectProfile {
            name: "MariaDB",
            accepted_quotes: MYSQL,
            style: QuoteStyle::Backtick,
            mysql_family: true,
        },
        "oceanbase" => DialectProfile {
            name: "OceanBase",
            accepted_quotes: MYSQL,
            style: QuoteStyle::Backtick,
            mysql_family: true,
        },
        "goldendb" => DialectProfile {
            name: "GoldenDB",
            accepted_quotes: MYSQL,
            style: QuoteStyle::Backtick,
            mysql_family: false,
        },
        "postgresql" | "postgres" | "pg" => DialectProfile {
            name: "PostgreSQL",
            accepted_quotes: PG,
            style: QuoteStyle::Double,
            mysql_family: false,
        },
        "oracle" => DialectProfile {
            name: "Oracle",
            accepted_quotes: ANSI,
            style: QuoteStyle::Double,
            mysql_family: false,
        },
        "sqlserver" | "mssql" | "sql" => DialectProfile {
            name: "SQL Server",
            accepted_quotes: SQLSERVER,
            style: QuoteStyle::Bracket,
            mysql_family: false,
        },
        "h2" => DialectProfile {
            name: "H2",
            accepted_quotes: LOOSE,
            style: QuoteStyle::Double,
            mysql_family: false,
        },
        "sqlite" | "sqlite3" => DialectProfile {
            name: "SQLite",
            accepted_quotes: LOOSE,
            style: QuoteStyle::Double,
            mysql_family: false,
        },
        "dm" | "dameng" => DialectProfile {
            name: "DM",
            accepted_quotes: ANSI,
            style: QuoteStyle::Double,
            mysql_family: false,
        },
        "kingbase" | "kingbasees" => DialectProfile {
            name: "KingBase",
            accepted_quotes: ANSI,
            style: QuoteStyle::Double,
            mysql_family: false,
        },
        "opengauss" => DialectProfile {
            name: "openGauss",
            accepted_quotes: ANSI,
            style: QuoteStyle::Double,
            mysql_family: false,
        },
        "gaussdb" => DialectProfile {
            name: "GaussDB",
            accepted_quotes: ANSI,
            style: QuoteStyle::Double,
            mysql_family: false,
        },
        "highgo" => DialectProfile {
            name: "HighGo",
            accepted_quotes: ANSI,
            style: QuoteStyle::Double,
            mysql_family: false,
        },
        "vastbase" => DialectProfile {
            name: "Vastbase",
            accepted_quotes: ANSI,
            style: QuoteStyle::Double,
            mysql_family: false,
        },
        _ => return None,
    })
}
