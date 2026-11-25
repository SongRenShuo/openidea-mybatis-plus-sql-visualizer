//! Schema metadata cache with DDL invalidation (T5-6).
//!
//! A pure data structure: no `host::`, no driver. The caller fills it from
//! whatever connection it holds; this module only decides when an entry is
//! **stale** and how to keep "offline" from turning into a false "table has no
//! columns".
//!
//! ## Why the key includes the dialect
//!
//! The same table name means different types/columns across engines (MySQL
//! `int` vs Oracle `NUMBER`). A cache keyed only by table would leak one
//! dialect's schema into another's lookup, so [`SchemaCache::lookup`] is
//! dialect-scoped and there is a cross-dialect isolation test below.
//!
//! ## Offline is not "absent"
//!
//! When the source cannot be reached, the caller marks the cache offline; a
//! lookup then reports [`SchemaMiss::Offline`], never an empty table. That is the
//! difference between "we do not know" and "there are no columns" — the plugin
//! must not mis-report on a disconnected database.

use std::collections::BTreeMap;

/// One column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnSchema {
    pub name: String,
    pub sql_type: String,
    pub nullable: bool,
}

/// One index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexSchema {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
}

/// A table's cached metadata.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TableSchema {
    pub table: String,
    pub columns: Vec<ColumnSchema>,
    pub indexes: Vec<IndexSchema>,
}

impl TableSchema {
    pub fn new(table: &str) -> Self {
        TableSchema {
            table: table.to_string(),
            ..Default::default()
        }
    }

    pub fn with_column(mut self, name: &str, sql_type: &str, nullable: bool) -> Self {
        self.columns.push(ColumnSchema {
            name: name.to_string(),
            sql_type: sql_type.to_string(),
            nullable,
        });
        self
    }

    pub fn with_index(mut self, name: &str, columns: &[&str], unique: bool) -> Self {
        self.indexes.push(IndexSchema {
            name: name.to_string(),
            columns: columns.iter().map(|c| c.to_string()).collect(),
            unique,
        });
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SchemaKey {
    dialect: String,
    table: String,
}

/// Why a lookup found nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaMiss {
    /// The source is unreachable; the table may well exist.
    Offline(String),
    /// No cached entry and the source has not been queried yet.
    Absent,
}

/// Dialect-scoped metadata cache.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SchemaCache {
    entries: BTreeMap<SchemaKey, TableSchema>,
    offline: Option<String>,
}

impl SchemaCache {
    pub fn new() -> Self {
        SchemaCache::default()
    }

    /// Mark the source unreachable; lookups report [`SchemaMiss::Offline`].
    pub fn mark_offline(&mut self, reason: impl Into<String>) {
        self.offline = Some(reason.into());
    }

    /// Mark the source reachable again (keeps cached entries).
    pub fn mark_online(&mut self) {
        self.offline = None;
    }

    pub fn is_offline(&self) -> Option<&str> {
        self.offline.as_deref()
    }

    /// Insert (or replace) one table's metadata for a dialect.
    pub fn put(&mut self, dialect: &str, schema: TableSchema) {
        let key = SchemaKey {
            dialect: dialect.to_string(),
            table: schema.table.clone(),
        };
        self.entries.insert(key, schema);
    }

    /// Look up one table for one dialect.
    pub fn lookup(&self, dialect: &str, table: &str) -> Result<&TableSchema, SchemaMiss> {
        if let Some(reason) = &self.offline {
            return Err(SchemaMiss::Offline(reason.clone()));
        }
        self.entries
            .get(&SchemaKey {
                dialect: dialect.to_string(),
                table: table.to_string(),
            })
            .ok_or(SchemaMiss::Absent)
    }

    /// Remove every dialect's entry for `table`; returns how many were removed.
    pub fn invalidate_table(&mut self, table: &str) -> usize {
        let before = self.entries.len();
        self.entries.retain(|key, _| key.table != table);
        before - self.entries.len()
    }

    /// Inspect `sql`; if it is a DDL that changes a table's shape, drop that
    /// table's entries and return the affected table names (deduped).
    ///
    /// A non-DDL statement is a no-op returning an empty vec — a plain `SELECT`
    /// must never evict the cache.
    pub fn invalidate_on_ddl(&mut self, sql: &str) -> Vec<String> {
        let targets = ddl_targets(sql);
        for table in &targets {
            self.invalidate_table(table);
        }
        targets
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

// ---------------------------------------------------------------------------
// DDL target extraction (pure)
// ---------------------------------------------------------------------------

fn strip_comments(sql: &str) -> String {
    // Remove -- … and /* … */ so a leading comment does not hide the keyword.
    let bytes = sql.as_bytes();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'-' && bytes.get(i + 1) == Some(&b'-') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

fn strip_ident(raw: &str) -> String {
    raw.trim().trim_matches(['`', '"', '[', ']']).to_string()
}

/// The first identifier token of `s` (stops at whitespace, `(`, `,`, `;`).
fn first_ident(s: &str) -> Option<String> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    let end = s
        .find(|c: char| c.is_whitespace() || c == '(' || c == ',' || c == ';')
        .unwrap_or(s.len());
    let token = &s[..end];
    if token.is_empty() {
        None
    } else {
        Some(strip_ident(token))
    }
}

/// Case-insensitive prefix strip that returns the **original-case** remainder
/// (so a table written `sys_user` stays lowercase; we must not uppercase the
/// identifier when invalidating a cache keyed by the real name).
fn strip_prefix_ci<'a>(s: &'a str, keyword: &str) -> Option<&'a str> {
    if s.len() >= keyword.len() && s[..keyword.len()].eq_ignore_ascii_case(keyword) {
        Some(&s[keyword.len()..])
    } else {
        None
    }
}

fn starts_with_ci(s: &str, keyword: &str) -> bool {
    strip_prefix_ci(s, keyword).is_some()
}

fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() || h.len() < n.len() {
        return None;
    }
    (0..=h.len() - n.len()).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

/// The tables a DDL statement changes; empty for anything else.
fn ddl_targets(sql: &str) -> Vec<String> {
    let cleaned = strip_comments(sql);
    let trimmed = cleaned.trim().trim_end_matches(';').trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let mut out: Vec<String> = Vec::new();

    // CREATE [TEMPORARY] TABLE [IF NOT EXISTS] name
    if let Some(rest) = strip_prefix_ci(trimmed, "CREATE TEMPORARY TABLE")
        .or_else(|| strip_prefix_ci(trimmed, "CREATE TABLE"))
    {
        out.extend(collect_declared_tables(rest));
        return dedup(out);
    }
    // ALTER TABLE name
    if let Some(rest) = strip_prefix_ci(trimmed, "ALTER TABLE") {
        if let Some(name) = first_ident(skip_if_exists(rest)) {
            out.push(name);
        }
        return dedup(out);
    }
    // DROP TABLE [IF EXISTS] a, b
    if let Some(rest) = strip_prefix_ci(trimmed, "DROP TABLE") {
        for part in skip_if_exists(rest).split(',') {
            if let Some(name) = first_ident(part) {
                out.push(name);
            }
        }
        return dedup(out);
    }
    // TRUNCATE [TABLE] name
    if let Some(rest) =
        strip_prefix_ci(trimmed, "TRUNCATE TABLE").or_else(|| strip_prefix_ci(trimmed, "TRUNCATE"))
    {
        if let Some(name) = first_ident(rest) {
            out.push(name);
        }
        return dedup(out);
    }
    // RENAME TABLE a TO b, c TO d
    if let Some(rest) = strip_prefix_ci(trimmed, "RENAME TABLE") {
        for part in rest.split(',') {
            for side in part.split_whitespace() {
                if side.eq_ignore_ascii_case("TO") {
                    continue;
                }
                let name = strip_ident(side.trim_end_matches(','));
                if !name.is_empty() {
                    out.push(name);
                }
            }
        }
        return dedup(out);
    }
    // CREATE [UNIQUE] INDEX idx ON table / DROP INDEX idx ON table
    if starts_with_ci(trimmed, "CREATE INDEX")
        || starts_with_ci(trimmed, "CREATE UNIQUE INDEX")
        || starts_with_ci(trimmed, "DROP INDEX")
    {
        if let Some(on) = find_on_table(trimmed) {
            out.push(on);
        }
        return dedup(out);
    }

    Vec::new()
}

/// After `CREATE TABLE`, the name may be preceded by `IF NOT EXISTS`.
fn collect_declared_tables(rest: &str) -> Vec<String> {
    // A single name here; a comma would be a column list, not a second table.
    first_ident(skip_if_exists(rest)).into_iter().collect()
}

fn skip_if_exists(rest: &str) -> &str {
    let t = rest.trim_start();
    strip_prefix_ci(t, "IF NOT EXISTS")
        .or_else(|| strip_prefix_ci(t, "IF EXISTS"))
        .map(str::trim_start)
        .unwrap_or(t)
}

/// For `... INDEX … ON <table>`, return the table.
fn find_on_table(sql: &str) -> Option<String> {
    let idx = find_ci(sql, " ON ")?;
    first_ident(&sql[idx + 4..])
}

fn dedup(mut names: Vec<String>) -> Vec<String> {
    names.retain(|n| !n.is_empty());
    let mut seen = std::collections::BTreeSet::new();
    names.retain(|n| seen.insert(n.clone()));
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(table: &str) -> TableSchema {
        TableSchema::new(table)
            .with_column("id", "bigint", false)
            .with_column("name", "varchar", true)
            .with_index("pk", &["id"], true)
    }

    fn cache_with_two_dialects(table: &str) -> SchemaCache {
        let mut cache = SchemaCache::new();
        cache.put("MySQL", sample(table));
        let mut pg = sample(table);
        pg.columns[0].sql_type = "bigint".into();
        cache.put("PostgreSQL", pg);
        cache
    }

    // ---- invalidation (≥3) ----

    #[test]
    fn alter_table_invalidates_only_that_table() {
        let mut cache = cache_with_two_dialects("sys_user");
        cache.put("MySQL", sample("sys_order"));
        let hit = cache.invalidate_on_ddl("ALTER TABLE sys_user ADD COLUMN email VARCHAR(64)");
        assert_eq!(hit, vec!["sys_user".to_string()]);
        assert_eq!(
            cache.lookup("MySQL", "sys_user"),
            Err(SchemaMiss::Absent),
            "altered table evicted"
        );
        assert!(
            cache.lookup("MySQL", "sys_order").is_ok(),
            "other tables survive"
        );
    }

    #[test]
    fn drop_and_create_table_are_detected() {
        let mut cache = cache_with_two_dialects("sys_user");
        assert_eq!(
            cache.invalidate_on_ddl("DROP TABLE IF EXISTS `sys_user`;"),
            vec!["sys_user".to_string()]
        );
        assert!(cache.is_empty());
        assert_eq!(
            cache.invalidate_on_ddl("CREATE TABLE `sys_user` (id bigint)"),
            vec!["sys_user".to_string()]
        );
    }

    #[test]
    fn rename_table_invalidates_both_sides() {
        let mut cache = SchemaCache::new();
        cache.put("MySQL", sample("old_name"));
        cache.put("MySQL", sample("new_name"));
        let hit = cache.invalidate_on_ddl("RENAME TABLE old_name TO new_name");
        assert_eq!(hit, vec!["old_name".to_string(), "new_name".to_string()]);
        assert!(cache.is_empty());
    }

    #[test]
    fn create_index_invalidates_its_table_and_drop_index_does_too() {
        let mut cache = cache_with_two_dialects("sys_user");
        assert_eq!(
            cache.invalidate_on_ddl("CREATE INDEX idx_name ON sys_user (name)"),
            vec!["sys_user".to_string()]
        );
        cache.put("MySQL", sample("sys_user"));
        assert_eq!(
            cache.invalidate_on_ddl("DROP INDEX idx_name ON sys_user"),
            vec!["sys_user".to_string()]
        );
    }

    #[test]
    fn a_select_never_invalidates_the_cache() {
        let mut cache = cache_with_two_dialects("sys_user");
        assert!(
            cache
                .invalidate_on_ddl("SELECT * FROM sys_user WHERE id = 1")
                .is_empty()
        );
        assert!(
            !cache.is_empty(),
            "a select must leave the cached entries untouched"
        );
        assert!(cache.lookup("MySQL", "sys_user").is_ok());
    }

    // ---- offline does not look like "no columns" (≥1) ----

    #[test]
    fn offline_reports_offline_not_absent() {
        let mut cache = cache_with_two_dialects("sys_user");
        cache.mark_offline("connection refused");
        assert_eq!(
            cache.lookup("MySQL", "sys_user"),
            Err(SchemaMiss::Offline("connection refused".to_string())),
            "a disconnected source must not read as a missing table"
        );
        cache.mark_online();
        assert!(cache.lookup("MySQL", "sys_user").is_ok());
    }

    // ---- dialect isolation ----

    #[test]
    fn a_table_does_not_leak_across_dialects() {
        let mut cache = SchemaCache::new();
        cache.put(
            "MySQL",
            TableSchema::new("t").with_column("id", "int", false),
        );
        assert!(cache.lookup("MySQL", "t").is_ok());
        assert_eq!(
            cache.lookup("PostgreSQL", "t"),
            Err(SchemaMiss::Absent),
            "the same table name under another dialect is a different entry"
        );
    }

    #[test]
    fn invalidate_table_removes_every_dialect_of_that_table() {
        let mut cache = cache_with_two_dialects("sys_user");
        assert_eq!(cache.invalidate_table("sys_user"), 2);
        assert!(cache.is_empty());
    }
}
