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
    // SQL log panel — L17 filter row / list / status / menu.
    ("view.logs.filter.types_title", "SQL type", "SQL 类型"),
    ("view.logs.filter.none", "None", "无"),
    ("view.logs.filter.count_suffix", "selected", "项"),
    ("view.logs.filter.select_all", "All", "全选"),
    ("view.logs.filter.select_none", "None", "全不选"),
    (
        "view.logs.filter.slow_only",
        "Slow statements only",
        "只看慢查询",
    ),
    ("view.logs.filter.search", "Search", "搜索"),
    ("view.logs.filter.dedupe", "Deduplicate", "去重"),
    ("view.logs.filter.match_case", "Aa", "Aa"),
    (
        "view.logs.filter.match_case_title",
        "Match case",
        "区分大小写",
    ),
    ("view.logs.filter.regex", ".*", ".*"),
    ("view.logs.filter.regex_title", "Regular expression", "正则"),
    ("view.logs.filter.hits_suffix", "matches", "条命中"),
    ("view.logs.filter.regex_error", "Invalid regex", "正则无效"),
    ("view.logs.slow_prefix", "Slow", "慢"),
    (
        "view.logs.status.console_prefix",
        "Console channel: scanned",
        "控制台通道：已扫描",
    ),
    ("view.logs.status.console_suffix", "lines", "行"),
    (
        "view.logs.status.agent_off",
        "Enhanced channel: off",
        "增强通道：未开启",
    ),
    (
        "view.logs.status.agent_events_prefix",
        "Enhanced channel: received",
        "增强通道：已收事件",
    ),
    ("view.logs.status.total_prefix", "Total", "共"),
    ("view.logs.status.total_suffix", "rows", "条"),
    (
        "view.logs.status.truncated_prefix",
        "Truncated to cap",
        "已截断到上限",
    ),
    ("view.logs.badge.dedupe_prefix", "⨉", "⨉"),
    ("view.logs.badge.group_prefix", "×", "×"),
    ("view.logs.menu.copy_sql", "Copy SQL", "复制 SQL"),
    ("view.logs.menu.copy_time", "Copy timestamp", "复制时间戳"),
    (
        "view.logs.menu.export",
        "Export visible rows",
        "导出可见行集",
    ),
    ("view.logs.type.select", "SELECT", "SELECT"),
    ("view.logs.type.insert", "INSERT", "INSERT"),
    ("view.logs.type.update", "UPDATE", "UPDATE"),
    ("view.logs.type.delete", "DELETE", "DELETE"),
    ("view.logs.type.create", "CREATE", "CREATE"),
    ("view.logs.type.drop", "DROP", "DROP"),
    // Preview workbench (T2-2).
    ("view.workbench.title", "SQL Workbench", "SQL 工作台"),
    ("view.workbench.sql", "SQL", "SQL"),
    ("view.workbench.params", "Parameters", "参数"),
    ("view.workbench.dialect", "Dialect", "方言"),
    ("view.workbench.copy", "Copy", "复制"),
    ("view.workbench.export", "Export", "导出"),
    (
        "view.workbench.empty",
        "The previewed SQL shows here.",
        "预览的 SQL 显示在此处。",
    ),
    (
        "view.workbench.copied",
        "Copied to the clipboard.",
        "已复制到剪贴板。",
    ),
    ("view.workbench.confidence", "Confidence", "置信度"),
    ("view.workbench.confidence.exact", "Exact", "精确"),
    ("view.workbench.confidence.partial", "Partial", "部分"),
    (
        "view.workbench.confidence.unresolved",
        "Unresolved",
        "未解析",
    ),
    ("view.workbench.confidence.unknown", "Unknown", "未知"),
    ("view.workbench.no_dialect", "Dialect unknown", "未知方言"),
    // Workbench layout and protocol (lane L18, docs/registration/L18.md).
    (
        "view.workbench.hint.input",
        "Paste or type SQL / a mapper XML fragment, then click \"Parse & Preview\". Supports ? / #{name} placeholders and <select>/<insert>/<update>/<delete> blocks.",
        "粘贴或输入 SQL / mapper XML 片段，然后点「解析预览」。支持 ? / #{名称} 占位符与 <select>/<insert>/<update>/<delete> 语句块。",
    ),
    (
        "view.workbench.action.resolve",
        "Parse & Preview",
        "解析预览",
    ),
    ("view.workbench.action.format", "Format", "格式化"),
    (
        "view.workbench.action.load_file",
        "Load from File…",
        "从文件加载…",
    ),
    (
        "view.workbench.load_file.title",
        "Select a SQL or mapper XML file",
        "选择 SQL 或 mapper XML 文件",
    ),
    ("view.workbench.action.clear", "Clear", "清空"),
    (
        "view.workbench.action.open_dialog",
        "Open in Dialog",
        "在弹框中打开",
    ),
    (
        "view.workbench.open_dialog.note",
        "This build has no separate dialog: the tool window is the workbench.",
        "本版没有独立弹框，工具窗就是工作台。",
    ),
    ("view.workbench.action.copy_sql", "Copy SQL", "复制 SQL"),
    ("view.workbench.action.execute", "Execute SQL", "执行 SQL"),
    (
        "view.workbench.execute.note",
        "This plugin never executes user SQL; EXPLAIN runs on the engine side.",
        "本插件不执行用户 SQL；EXPLAIN 走引擎侧。",
    ),
    ("view.workbench.mode", "Display mode", "显示模式"),
    ("view.workbench.mode.normal", "Normal", "普通"),
    ("view.workbench.mode.formatted", "Formatted", "格式化"),
    ("view.workbench.tab.sql", "SQL", "SQL"),
    ("view.workbench.tab.raw", "Original", "原文"),
    ("view.workbench.tab.columns", "Columns", "列信息"),
    (
        "view.workbench.columns.empty",
        "No column information: this build does not connect to the database.",
        "无列信息：本版不连接数据库。",
    ),
    (
        "view.workbench.reason.title",
        "Why it could not be rendered",
        "未能还原的原因",
    ),
    ("view.workbench.kind.sql", "SQL", "SQL"),
    ("view.workbench.kind.xml", "mapper XML", "mapper XML 片段"),
    (
        "view.workbench.dialect.default_suffix",
        " (default)",
        "（默认）",
    ),
    (
        "view.workbench.dialect.hint",
        "Change it in Settings → MyBatis SQL Visualizer",
        "在 Settings → MyBatis SQL Visualizer 修改",
    ),
    (
        "view.workbench.cap.quote",
        "Identifier quotes: {0} … {1}",
        "标识符引号：{0} … {1}",
    ),
    (
        "view.workbench.cap.pagination",
        "Pagination: {0}",
        "分页形态：{0}",
    ),
    (
        "view.workbench.cap.keyword_case",
        "Keyword case: {0}",
        "关键字大小写：{0}",
    ),
    (
        "view.workbench.advice",
        "Performance advice: {0} findings ({1} error, {2} warning)",
        "性能建议 {0} 条（严重 {1}，警告 {2}）",
    ),
    (
        "view.workbench.advice.none",
        "No performance findings.",
        "无性能建议。",
    ),
    (
        "view.workbench.status.empty",
        "Input is empty. Paste SQL or a mapper XML fragment first.",
        "输入为空。请先粘贴 SQL 或 mapper XML 片段。",
    ),
    (
        "view.workbench.status.resolved",
        "Parsed as {0}, {1} parameter(s).",
        "已解析为{0}，参数 {1} 个。",
    ),
    (
        "view.workbench.status.partial",
        "Partially rendered — check the result against the source.",
        "部分内容未能还原，请与原文核对。",
    ),
    (
        "view.workbench.status.unresolved",
        "Could not render — the reason is in the SQL tab.",
        "未能还原，原因见「SQL」页签。",
    ),
    ("view.workbench.status.cleared", "Cleared.", "已清空。"),
    (
        "view.workbench.status.copy_empty",
        "Parse a statement first, then copy.",
        "请先解析预览 SQL 或片段，再复制。",
    ),
    (
        "view.workbench.status.exported",
        "Exported to the Output panel.",
        "已导出到 Output 面板。",
    ),
    (
        "view.workbench.status.export_empty",
        "Parse a statement first, then export.",
        "请先解析预览 SQL 或片段，再导出。",
    ),
    (
        "view.workbench.status.formatted",
        "Formatted.",
        "已格式化。",
    ),
    (
        "view.workbench.status.format_unchanged",
        "Formatting left the text unchanged.",
        "格式化未改变文本。",
    ),
    (
        "view.workbench.status.format_xml_skipped",
        "A mapper XML fragment is not SQL-formatted.",
        "mapper XML 片段不做 SQL 格式化。",
    ),
    (
        "view.workbench.status.format_unknown_dialect",
        "Unknown dialect: not formatted.",
        "方言未知，未格式化。",
    ),
    (
        "view.workbench.status.loaded",
        "Loaded from {0}.",
        "已从 {0} 加载。",
    ),
    (
        "view.workbench.status.load_no_project",
        "No open project to load a file from. {0}",
        "没有打开的项目，无法从工程加载文件。{0}",
    ),
    (
        "view.workbench.status.load_no_files",
        "No .xml or .sql file found in the project.",
        "工程内没有找到 .xml / .sql 文件。",
    ),
    (
        "view.workbench.status.load_failed",
        "Could not read {0}: {1}",
        "读取 {0} 失败：{1}",
    ),
    (
        "view.workbench.status.dialect_readonly",
        "The dialect comes from the settings page and cannot be changed here.",
        "方言来自设置页，不能在此修改。",
    ),
    // Parameter restoration keys (lane L2, docs/registration/L2.md).
    (
        "params.missing_positional",
        "Missing value for positional parameter '?'; left as placeholder.",
        "位置参数 '?' 缺少对应值，已保留占位符。",
    ),
    (
        "params.missing_named",
        "Missing value for named parameter ':name'; left as placeholder.",
        "命名参数 ':name' 缺少对应值，已保留占位符。",
    ),
    (
        "params.unknown_type",
        "Parameter type could not be determined; rendered as a string literal.",
        "无法确定参数类型，已按字符串字面量渲染。",
    ),
    (
        "params.unknown_date",
        "Value does not match a date/timestamp shape; rendered as a string literal.",
        "值不符合日期/时间戳形态，已按字符串字面量渲染。",
    ),
    (
        "params.boolean_literal_unsupported",
        "Dialect has no boolean literal; rendered 1/0.",
        "该方言无布尔字面量，已按 1/0 渲染。",
    ),
    (
        "params.date_quoted_fallback",
        "Dialect lacks ANSI typed date literals; rendered as a quoted string.",
        "该方言不支持 ANSI 类型日期字面量，已回落为引号字符串。",
    ),
    (
        "params.bytes_invalid",
        "Byte array value is not a valid byte list/hex string; rendered as a string.",
        "字节数组值不是合法字节列表/十六进制串，已按字符串渲染。",
    ),
    // Log parsing keys (lane L4, docs/registration/L4.md).
    (
        "log.entry.params_missing",
        "Fewer parameters than placeholders; not every '?' was restored.",
        "占位符多于参数，未能还原全部 '?'。",
    ),
    (
        "log.entry.params_unused",
        "More parameters than placeholders; extra values were ignored.",
        "参数多于占位符，多余参数未使用。",
    ),
    (
        "log.entry.sql_truncated",
        "SQL exceeded the parsing length limit and was truncated.",
        "SQL 超过解析长度上限被截断。",
    ),
    (
        "log.entry.multiline_unclosed",
        "A multi-line SQL block was not closed; the remaining text was kept.",
        "多行 SQL 块未闭合，已保留残句。",
    ),
    (
        "log.entry.raw_preserved",
        "The original line is kept in raw_line.",
        "原文已保留在 raw_line。",
    ),
    (
        "log.entry.sql_missing",
        "Error row: no SQL was produced.",
        "错误行，未产出 SQL。",
    ),
    (
        "log.unparsed.orphan_parameters",
        "A Parameters line has no matching Preparing line.",
        "Parameters 行没有对应的 Preparing。",
    ),
    (
        "log.unparsed.orphan_preparing",
        "A Preparing line has no parsable SQL.",
        "Preparing 行没有可解析的 SQL。",
    ),
    (
        "log.unparsed.unrecognized_sql",
        "The line looks like SQL but no log family recognised it.",
        "像 SQL 但没有家族能识别。",
    ),
    (
        "log.unparsed.sql_too_long",
        "The line is too long to parse.",
        "行文本过长，未解析。",
    ),
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

    #[test]
    fn every_core_log_message_key_has_text() {
        use crate::core::log::message_keys as keys;
        let log_keys = [
            keys::ENTRY_PARAMS_MISSING,
            keys::ENTRY_PARAMS_UNUSED,
            keys::ENTRY_SQL_TRUNCATED,
            keys::ENTRY_MULTILINE_UNCLOSED,
            keys::ENTRY_RAW_PRESERVED,
            keys::ENTRY_SQL_MISSING,
            keys::UNPARSED_ORPHAN_PARAMETERS,
            keys::UNPARSED_ORPHAN_PREPARING,
            keys::UNPARSED_UNRECOGNIZED_SQL,
            keys::UNPARSED_SQL_TOO_LONG,
        ];
        assert_eq!(log_keys.len(), 10, "lane L4 defines ten log keys");
        for key in log_keys {
            assert!(
                known_keys().any(|known| known == key),
                "{key} missing from the i18n table"
            );
            assert_ne!(t_in("en", key), key, "English text missing for {key}");
            assert_ne!(t_in("zh", key), key, "Chinese text missing for {key}");
        }
    }

    #[test]
    fn every_lane_l2_parameter_key_is_covered() {
        let param_keys = [
            "params.missing_positional",
            "params.missing_named",
            "params.unknown_type",
            "params.unknown_date",
            "params.boolean_literal_unsupported",
            "params.date_quoted_fallback",
            "params.bytes_invalid",
        ];
        assert_eq!(param_keys.len(), 7, "lane L2 defines seven params keys");
        for key in param_keys {
            assert!(
                known_keys().any(|known| known == key),
                "{key} missing from the i18n table"
            );
            assert_ne!(t_in("en", key), key, "English text missing for {key}");
            assert_ne!(t_in("zh", key), key, "Chinese text missing for {key}");
        }
    }
}
