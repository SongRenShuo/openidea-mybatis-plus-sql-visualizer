//! Dialect table and capability matrix (T1-6).
//!
//! Ported (behaviourally) from the IntelliJ edition's `Dialect.kt` /
//! `DialectCapabilities.kt`: fifteen dialects, each described by a
//! [`Capabilities`] struct and a per-dialect reserved-word table. The Kotlin
//! interface is expressed here as data plus pure `&self` methods so the whole
//! table can live in a `static` slice (`all()`).
//!
//! Design notes:
//! - `name` matches the reference `Dialect.name` (and the Settings dropdown).
//! - Rendering methods (`render_limit`, `render_upsert`, …) switch on the
//!   capability fields rather than on the dialect identity, so the matrix is
//!   the single source of truth (a capability that is not in the struct cannot
//!   be claimed).
//! - Reserved-word tables live in [`keywords`] (lowercase), transcribed from
//!   the reference keyword data.

pub mod keywords;

/// How a dialect folds an unquoted identifier (O-51 family).
///
/// The catalog quoting rules depend on this: the same dictionary name is quoted
/// in opposite directions under `Upper` vs `Lower`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentifierFolding {
    /// Unquoted identifiers fold to upper case (Oracle / H2): a catalog name
    /// containing any lowercase letter must be quoted.
    Upper,
    /// Unquoted identifiers fold to lower case (PostgreSQL family): a catalog
    /// name containing any uppercase letter must be quoted.
    Lower,
    /// No folding (or case-insensitive match): name-based quoting is
    /// meaningless, fall back to `quote_if_keyword`.
    None,
}

/// Where an identifier came from — decides catalog-vs-entity quoting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentifierCaseSource {
    /// Introspected dictionary name (not case-normalised).
    Catalog,
    /// Lowercase derived name from annotations / camel→snake (the default in
    /// the reference implementation).
    Entity,
}

/// Pagination clause shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaginationStyle {
    /// `LIMIT [offset,] count` — MySQL / MariaDB / DM / OceanBase / GoldenDB.
    MySql,
    /// `LIMIT count OFFSET offset` — PostgreSQL family / H2 / SQLite.
    Pg,
    /// Oracle: `OFFSET n ROWS FETCH NEXT m ROWS ONLY` / `FETCH FIRST m ROWS ONLY`.
    OracleFetch,
    /// SQL Server: `OFFSET n ROWS FETCH NEXT m ROWS ONLY` (offset defaults to 0).
    SqlServerOffsetFetch,
}

/// Native upsert statement shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertStyle {
    /// No native "INSERT + suffix" upsert — renderer degrades to plain INSERT
    /// (Oracle / SQL Server / DM use complex MERGE, out of scope here).
    None,
    /// `ON DUPLICATE KEY UPDATE col = VALUES(col)` — MySQL family / openGauss /
    /// GaussDB / GoldenDB.
    OnDuplicateKey,
    /// `ON CONFLICT (k) DO UPDATE SET col = EXCLUDED.col` — PostgreSQL family /
    /// SQLite / KingBase / HighGo / Vastbase.
    OnConflict,
    /// Whole-statement `MERGE INTO t (cols) KEY (k) VALUES (...)` — H2.
    MergeKey,
}

/// Whether explicitly writing an auto-increment column needs a wrapper statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityWrap {
    /// No wrapper needed.
    None,
    /// `SET IDENTITY_INSERT <t> ON; … SET IDENTITY_INSERT <t> OFF;`
    /// — SQL Server and DM8 (verified identical shape in the reference).
    IdentityInsert,
}

/// Keyword casing produced by the formatter for this dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeywordCase {
    Upper,
    Lower,
}

/// Capability matrix entry. Every field is a fact about the dialect and is
/// asserted individually in the tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub pagination: PaginationStyle,
    /// `OFFSET … FETCH` must follow `ORDER BY` (SQL Server).
    pub limit_requires_order_by: bool,
    /// Open/close quote characters for `quote_identifier`.
    pub quote_open: char,
    pub quote_close: char,
    /// Acceptable identifier quote openers (for dialect-quote checks): backtick
    /// `` ` ``, double quote `"`, opening bracket `[`.
    pub identifier_quotes: &'static [char],
    pub identifier_folding: IdentifierFolding,
    /// `DELETE … LIMIT n` tail is supported.
    pub supports_delete_limit: bool,
    pub upsert: UpsertStyle,
    pub identity_wrap: IdentityWrap,
    /// The dialect has `TRUE`/`FALSE` literals (Oracle / DM do not).
    pub supports_boolean_literal: bool,
    /// The dialect parses ANSI `DATE '…'` / `TIMESTAMP '…'` typed literals
    /// (SQL Server / SQLite do not — they fall back to a quoted string).
    pub supports_ansi_date_literal: bool,
    /// Keyword casing for the formatter.
    pub keyword_case: KeywordCase,
}

/// One database dialect. Constructed in the `static` table below; exposed via
/// [`all`] / [`by_name`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dialect {
    pub name: &'static str,
    pub capabilities: Capabilities,
    reserved_words: &'static [&'static str],
}

macro_rules! cap {
    ($pag:ident, $qo:expr, $qc:expr, $iq:expr, $fold:ident, $del:expr, $ord:expr,
     $ups:ident, $idw:ident, $bool:expr, $ansi:expr) => {
        Capabilities {
            pagination: PaginationStyle::$pag,
            limit_requires_order_by: $ord,
            quote_open: $qo,
            quote_close: $qc,
            identifier_quotes: $iq,
            identifier_folding: IdentifierFolding::$fold,
            supports_delete_limit: $del,
            upsert: UpsertStyle::$ups,
            identity_wrap: IdentityWrap::$idw,
            supports_boolean_literal: $bool,
            supports_ansi_date_literal: $ansi,
            keyword_case: KeywordCase::Upper,
        }
    };
}

const BACKTICK_SET: &[char] = &['`'];
const DOUBLE_SET: &[char] = &['"'];
const BRACKET_DOUBLE_SET: &[char] = &['[', '"'];
const TRIPLE_SET: &[char] = &['"', '`', '['];

/// All fifteen dialects, in the reference registry order (which is also the
/// Settings dropdown order).
static ALL: [Dialect; 15] = [
    Dialect {
        name: "MySQL",
        capabilities: cap!(
            MySql,
            '`',
            '`',
            BACKTICK_SET,
            None,
            true,
            false,
            OnDuplicateKey,
            None,
            true,
            true
        ),
        reserved_words: keywords::MYSQL_RESERVED,
    },
    Dialect {
        name: "PostgreSQL",
        capabilities: cap!(
            Pg, '"', '"', DOUBLE_SET, Lower, false, false, OnConflict, None, true, true
        ),
        reserved_words: keywords::PG_RESERVED,
    },
    Dialect {
        name: "Oracle",
        capabilities: cap!(
            OracleFetch,
            '"',
            '"',
            DOUBLE_SET,
            Upper,
            false,
            false,
            None,
            None,
            false,
            true
        ),
        reserved_words: keywords::ORACLE_RESERVED,
    },
    Dialect {
        name: "SQL Server",
        capabilities: cap!(
            SqlServerOffsetFetch,
            '[',
            ']',
            BRACKET_DOUBLE_SET,
            None,
            false,
            true,
            None,
            IdentityInsert,
            true,
            false
        ),
        reserved_words: keywords::SQLSERVER_RESERVED,
    },
    Dialect {
        name: "H2",
        capabilities: cap!(
            Pg, '"', '"', TRIPLE_SET, Upper, false, false, MergeKey, None, true, true
        ),
        reserved_words: keywords::H2_RESERVED,
    },
    Dialect {
        name: "SQLite",
        capabilities: cap!(
            Pg, '"', '"', TRIPLE_SET, None, false, false, OnConflict, None, true, false
        ),
        reserved_words: keywords::SQLITE_RESERVED,
    },
    Dialect {
        name: "MariaDB",
        capabilities: cap!(
            MySql,
            '`',
            '`',
            BACKTICK_SET,
            None,
            true,
            false,
            OnDuplicateKey,
            None,
            true,
            true
        ),
        reserved_words: keywords::MARIA_RESERVED,
    },
    Dialect {
        name: "DM（达梦）",
        capabilities: cap!(
            MySql,
            '"',
            '"',
            DOUBLE_SET,
            None,
            true,
            false,
            None,
            IdentityInsert,
            false,
            true
        ),
        reserved_words: keywords::DM_RESERVED,
    },
    Dialect {
        name: "KingBase（人大金仓）",
        capabilities: cap!(
            Pg, '"', '"', DOUBLE_SET, Lower, false, false, OnConflict, None, true, true
        ),
        reserved_words: keywords::KINGBASE_RESERVED,
    },
    Dialect {
        name: "OceanBase",
        capabilities: cap!(
            MySql,
            '`',
            '`',
            BACKTICK_SET,
            None,
            true,
            false,
            OnDuplicateKey,
            None,
            true,
            true
        ),
        reserved_words: keywords::MYSQL_RESERVED,
    },
    Dialect {
        name: "openGauss",
        capabilities: cap!(
            Pg,
            '"',
            '"',
            DOUBLE_SET,
            Lower,
            false,
            false,
            OnDuplicateKey,
            None,
            true,
            true
        ),
        reserved_words: keywords::OPENGAUSS_RESERVED,
    },
    Dialect {
        name: "GaussDB",
        capabilities: cap!(
            Pg,
            '"',
            '"',
            DOUBLE_SET,
            Lower,
            false,
            false,
            OnDuplicateKey,
            None,
            true,
            true
        ),
        reserved_words: keywords::GAUSSDB_RESERVED,
    },
    Dialect {
        name: "HighGo",
        capabilities: cap!(
            Pg, '"', '"', DOUBLE_SET, Lower, false, false, OnConflict, None, true, true
        ),
        reserved_words: keywords::HIGHGO_RESERVED,
    },
    Dialect {
        name: "Vastbase",
        capabilities: cap!(
            Pg, '"', '"', DOUBLE_SET, Lower, true, false, OnConflict, None, true, true
        ),
        reserved_words: keywords::VASTBASE_RESERVED,
    },
    Dialect {
        name: "GoldenDB",
        capabilities: cap!(
            MySql,
            '`',
            '`',
            BACKTICK_SET,
            None,
            true,
            false,
            OnDuplicateKey,
            None,
            true,
            true
        ),
        reserved_words: keywords::GOLDENDB_RESERVED,
    },
];

/// All supported dialects, in Settings order.
pub fn all() -> &'static [Dialect] {
    &ALL
}

/// Look up a dialect by its exact `name`. Returns `None` for unknown names
/// (callers decide how to fall back).
pub fn by_name(name: &str) -> Option<&'static Dialect> {
    ALL.iter().find(|d| d.name == name)
}

impl Dialect {
    /// Reserved words for this dialect (lowercase).
    pub fn reserved_words(&self) -> &'static [&'static str] {
        self.reserved_words
    }

    /// Whether `ident` (case-insensitively) is a reserved word in this dialect.
    pub fn is_reserved(&self, ident: &str) -> bool {
        let lower = ident.to_lowercase();
        self.reserved_words.iter().any(|w| *w == lower)
    }

    /// Wrap `ident` with this dialect's identifier quotes.
    pub fn quote_identifier(&self, ident: &str) -> String {
        let c = &self.capabilities;
        format!("{}{}{}", c.quote_open, ident, c.quote_close)
    }

    /// Quote only if `ident` is a reserved word in this dialect.
    pub fn quote_if_keyword(&self, ident: &str) -> String {
        if self.is_reserved(ident) {
            self.quote_identifier(ident)
        } else {
            ident.to_string()
        }
    }

    /// Quote according to the case source: entity names behave like
    /// `quote_if_keyword`; catalog names additionally respect the folding
    /// family (see [`IdentifierFolding`]).
    pub fn quote_if_needed(&self, ident: &str, source: IdentifierCaseSource) -> String {
        match source {
            IdentifierCaseSource::Entity => self.quote_if_keyword(ident),
            IdentifierCaseSource::Catalog => match self.capabilities.identifier_folding {
                IdentifierFolding::Upper => {
                    if ident.chars().any(|c| c.is_lowercase()) {
                        self.quote_identifier(ident)
                    } else {
                        self.quote_if_keyword(ident)
                    }
                }
                IdentifierFolding::Lower => {
                    if ident.chars().any(|c| c.is_uppercase()) {
                        self.quote_identifier(ident)
                    } else {
                        self.quote_if_keyword(ident)
                    }
                }
                IdentifierFolding::None => self.quote_if_keyword(ident),
            },
        }
    }

    /// Render the pagination clause for `limit` rows starting at `offset`.
    pub fn render_limit(&self, limit: u64, offset: Option<u64>) -> String {
        match self.capabilities.pagination {
            PaginationStyle::MySql => match offset {
                Some(o) => format!("LIMIT {o}, {limit}"),
                None => format!("LIMIT {limit}"),
            },
            PaginationStyle::Pg => match offset {
                Some(o) => format!("LIMIT {limit} OFFSET {o}"),
                None => format!("LIMIT {limit}"),
            },
            PaginationStyle::OracleFetch => match offset {
                Some(o) => format!("OFFSET {o} ROWS FETCH NEXT {limit} ROWS ONLY"),
                None => format!("FETCH FIRST {limit} ROWS ONLY"),
            },
            PaginationStyle::SqlServerOffsetFetch => {
                let o = offset.unwrap_or(0);
                format!("OFFSET {o} ROWS FETCH NEXT {limit} ROWS ONLY")
            }
        }
    }

    /// Render the upsert conflict suffix (entity case source). `None` when the
    /// dialect has no suffix-form upsert.
    pub fn render_upsert(&self, conflict_key: &str, update_columns: &[&str]) -> Option<String> {
        self.render_upsert_with(conflict_key, update_columns, IdentifierCaseSource::Entity)
    }

    /// `render_upsert` with an explicit case source.
    pub fn render_upsert_with(
        &self,
        conflict_key: &str,
        update_columns: &[&str],
        source: IdentifierCaseSource,
    ) -> Option<String> {
        match self.capabilities.upsert {
            UpsertStyle::None | UpsertStyle::MergeKey => None,
            UpsertStyle::OnDuplicateKey => {
                let cols = update_columns
                    .iter()
                    .map(|c| {
                        let q = self.quote_if_needed(c, source);
                        format!("{q} = VALUES({q})")
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                Some(format!("ON DUPLICATE KEY UPDATE {cols}"))
            }
            UpsertStyle::OnConflict => {
                let key = self.quote_if_needed(conflict_key, source);
                let cols = update_columns
                    .iter()
                    .map(|c| {
                        let q = self.quote_if_needed(c, source);
                        format!("{q} = EXCLUDED.{q}")
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                Some(format!("ON CONFLICT ({key}) DO UPDATE SET {cols}"))
            }
        }
    }

    /// Render a whole-statement upsert (H2 `MERGE INTO … KEY …`, entity case
    /// source). `None` when the dialect has no whole-statement form.
    pub fn render_upsert_statement(
        &self,
        table: &str,
        columns: &[&str],
        conflict_key: &str,
    ) -> Option<String> {
        self.render_upsert_statement_with(
            table,
            columns,
            conflict_key,
            IdentifierCaseSource::Entity,
        )
    }

    /// `render_upsert_statement` with an explicit case source.
    pub fn render_upsert_statement_with(
        &self,
        table: &str,
        columns: &[&str],
        conflict_key: &str,
        source: IdentifierCaseSource,
    ) -> Option<String> {
        match self.capabilities.upsert {
            UpsertStyle::MergeKey => {
                let t = self.quote_if_needed(table, source);
                let cols = columns
                    .iter()
                    .map(|c| self.quote_if_needed(c, source))
                    .collect::<Vec<_>>()
                    .join(", ");
                let k = self.quote_if_needed(conflict_key, source);
                let vals = columns.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
                Some(format!("MERGE INTO {t} ({cols}) KEY ({k}) VALUES ({vals})"))
            }
            _ => None,
        }
    }

    /// Wrap an INSERT that writes explicit values into an identity column
    /// (entity case source). `None` when no wrapper is needed.
    pub fn render_identity_wrapper(&self, table: &str, sql: &str) -> Option<String> {
        self.render_identity_wrapper_with(table, sql, IdentifierCaseSource::Entity)
    }

    /// `render_identity_wrapper` with an explicit case source.
    pub fn render_identity_wrapper_with(
        &self,
        table: &str,
        sql: &str,
        source: IdentifierCaseSource,
    ) -> Option<String> {
        match self.capabilities.identity_wrap {
            IdentityWrap::IdentityInsert => {
                let t = self.quote_if_needed(table, source);
                Some(format!(
                    "SET IDENTITY_INSERT {t} ON;\n{sql}\nSET IDENTITY_INSERT {t} OFF;"
                ))
            }
            IdentityWrap::None => None,
        }
    }
}

#[cfg(test)]
mod tests;
