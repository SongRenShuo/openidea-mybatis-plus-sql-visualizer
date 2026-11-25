//! Bilingual text (T2-4).
//!
//! The pure-logic layer returns dotted lower-case message keys (for example
//! `log.entry.params_missing` or `params.unknown_type`); this module maps them
//! to text. The language comes from [`openidea_plugin::host::locale`]: a tag
//! starting with `zh` selects Chinese, anything else English.
//!
//! Unknown keys return the key itself, so a missing translation is visible in
//! the UI instead of disappearing. The test module enumerates every known key
//! and fails if one of them would fall through.

use openidea_plugin::host;

/// `(key, English, Chinese)` for every key this plugin produces.
pub const TABLE: &[(&str, &str, &str)] = &[
    // SQL log panel (T2-1).
    ("view.logs.title", "SQL Logs", "SQL 日志"),
    (
        "view.logs.empty",
        "No SQL statements captured yet.",
        "尚未捕获到 SQL 语句。",
    ),
    ("view.logs.paused", "Live refresh paused", "实时刷新已暂停"),
    ("view.logs.category.normal", "Normal", "普通"),
    ("view.logs.category.duplicate", "Duplicate", "重复"),
    ("view.logs.category.n_plus_one", "N+1", "N+1"),
    ("view.logs.category.slow", "Slow", "慢查询"),
    ("view.logs.category.error", "Error", "错误"),
    ("view.logs.column.time", "Time", "时间"),
    ("view.logs.column.duration", "ms", "耗时(ms)"),
    ("view.logs.column.sql", "SQL", "SQL"),
    ("view.logs.column.params", "Parameters", "参数"),
    ("view.logs.column.category", "Category", "类别"),
    ("view.logs.button.pause", "Pause", "暂停"),
    ("view.logs.button.resume", "Resume", "继续"),
    ("view.logs.button.clear", "Clear", "清空"),
    ("view.logs.button.copy", "Copy SQL", "复制 SQL"),
    ("view.logs.filter.all", "All", "全部"),
    (
        "view.logs.filter.keyword",
        "Filter statements…",
        "筛选语句…",
    ),
    ("view.logs.expand", "Expand", "展开"),
    ("view.logs.collapse", "Collapse", "折叠"),
    ("view.logs.badge.n_plus_one", "N+1", "N+1"),
];

/// Localised text for `key` in the IDE's current UI language.
pub fn t(key: &str) -> String {
    t_in(&host::locale(), key)
}

/// The same lookup for an explicit locale tag (pure; used by tests).
pub fn t_in(locale: &str, key: &str) -> String {
    let chinese = locale.to_ascii_lowercase().starts_with("zh");
    TABLE
        .iter()
        .find(|(known, _, _)| *known == key)
        .map(|(_, en, zh)| {
            if chinese {
                (*zh).to_string()
            } else {
                (*en).to_string()
            }
        })
        .unwrap_or_else(|| key.to_string())
}

/// Every key with a translation, in table order.
pub fn known_keys() -> impl Iterator<Item = &'static str> {
    TABLE.iter().map(|(key, _, _)| *key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    #[test]
    fn every_known_key_is_translated_in_both_languages() {
        for key in known_keys() {
            let en = t_in("en", key);
            let zh = t_in("zh-CN", key);
            assert_ne!(en, key, "English text missing for {key}");
            assert_ne!(zh, key, "Chinese text missing for {key}");
            assert!(!en.is_empty(), "empty English text for {key}");
            assert!(!zh.is_empty(), "empty Chinese text for {key}");
        }
    }

    #[test]
    fn unknown_keys_fall_through_as_the_key_itself() {
        assert_eq!(t_in("en", "no.such.key"), "no.such.key");
        assert_eq!(t_in("zh", "no.such.key"), "no.such.key");
    }

    #[test]
    fn the_locale_decides_the_language() {
        assert_eq!(t_in("en", "view.logs.button.pause"), "Pause");
        assert_eq!(t_in("zh-CN", "view.logs.button.pause"), "暂停");
        assert_eq!(t_in("zh-TW", "view.logs.button.pause"), "暂停");
        assert_eq!(t_in("fr", "view.logs.button.pause"), "Pause");
    }

    #[test]
    fn keys_are_unique_and_the_host_locale_is_read() {
        let mut seen = std::collections::BTreeSet::new();
        for key in known_keys() {
            assert!(seen.insert(key), "duplicate key {key}");
        }

        let host = MockHost::install();
        host.set_locale("zh-CN");
        assert_eq!(t("view.logs.title"), "SQL 日志");
        host.set_locale("en");
        assert_eq!(t("view.logs.title"), "SQL Logs");
    }
}
