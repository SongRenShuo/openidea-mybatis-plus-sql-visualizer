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
    // The reference's empty state is a second guidance line under
    // `log.panel.empty` (lane A); the panel shows both in the muted status ink.
    (
        "view.logs.empty.hint",
        "SQL is captured automatically once a console with SQL logging runs (Capture). You can also paste logs manually in the \"Log to SQL\" tab; for real parameters and exact timings, enable Enhanced Capture in Settings.",
        "点「捕捉」并运行带 SQL 日志的控制台后自动捕获；也可从「日志转 SQL」页签手动粘贴还原；想要参数实参与精确耗时，可在设置页开启增强捕获。",
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
        "Console: scanned",
        "控制台：已扫描",
    ),
    ("view.logs.status.console_suffix", "lines", "行"),
    (
        "view.logs.status.agent_off",
        "Enhanced: off",
        "增强：未开启",
    ),
    (
        "view.logs.status.agent_events_prefix",
        "Enhanced: received",
        "增强：已收事件",
    ),
    // Lane A: a capture start that keeps being refused while the plugin dials
    // (the target application is not up yet). `{port}` is filled in by the page.
    (
        "view.logs.status.capture_waiting",
        "Waiting for the target application (port {port})",
        "等待目标应用启动（端口 {port}）",
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
    // Row actions (lane L28): jump, focus, delete, mute.
    ("view.logs.menu.jump_java", "Jump to Java", "跳转到 Java"),
    (
        "view.logs.menu.jump_xml",
        "Jump to Mapper XML",
        "跳转到 Mapper XML",
    ),
    (
        "view.logs.menu.focus_method",
        "Focus by method",
        "按方法聚焦",
    ),
    (
        "view.logs.menu.focus_request",
        "Focus by request",
        "按请求聚焦",
    ),
    ("view.logs.menu.delete", "Delete this row", "删除此行"),
    (
        "view.logs.menu.block_method",
        "Mute this method",
        "屏蔽此方法",
    ),
    ("view.logs.focus.method_prefix", "Method", "方法"),
    ("view.logs.focus.request_prefix", "Request", "请求"),
    ("view.logs.focus.clear", "Clear focus", "清除聚焦"),
    ("view.logs.blocked.prefix", "Muted", "已屏蔽"),
    (
        "view.logs.blocked.clear",
        "Show muted methods again",
        "恢复被屏蔽的方法",
    ),
    (
        "view.logs.jump.unavailable",
        "The file cannot be opened from here; the position is on the clipboard and in the Output channel.",
        "此处无法打开文件；位置已复制到剪贴板并输出到 Output 通道。",
    ),
    // Toolbar actions and interactions (lane L26).
    (
        "view.logs.button.capture_start",
        "Start capture",
        "开始捕捉",
    ),
    ("view.logs.button.capture_stop", "Stop capture", "停止捕捉"),
    (
        "view.logs.button.scroll_end",
        "Scroll to bottom",
        "滚动到底",
    ),
    (
        "view.logs.button.sort_duration",
        "Sort by duration",
        "按耗时排序",
    ),
    (
        "view.logs.sort.time_title",
        "Sorted by time (ascending)",
        "按时间正序",
    ),
    (
        "view.logs.sort.duration_title",
        "Sorted by duration (descending)",
        "按耗时降序",
    ),
    // Sort popup (lane A): the reference's seven `log.sort.*` menu entries, in
    // its `sortOptions()` order, plus the `log.sort.label` tooltip prefix.
    ("view.logs.sort.label", "Sort:", "排序："),
    ("view.logs.sort.time_asc", "Time ↑", "时间升序"),
    ("view.logs.sort.time_desc", "Time ↓", "时间降序"),
    ("view.logs.sort.duration_asc", "Duration ↑", "耗时升序"),
    ("view.logs.sort.duration_desc", "Duration ↓", "耗时降序"),
    ("view.logs.sort.sequence", "Row #", "行号"),
    (
        "view.logs.sort.structure",
        "Structure clusters",
        "同结构聚类",
    ),
    (
        "view.logs.sort.database",
        "Group by database",
        "按目标库分组",
    ),
    ("view.logs.button.blocked", "Blocked methods", "屏蔽的方法"),
    ("view.logs.blocked.title", "Blocked methods", "已屏蔽的方法"),
    (
        "view.logs.blocked.empty",
        "No methods are blocked.",
        "暂无屏蔽的方法。",
    ),
    ("view.logs.button.settings", "Settings", "设置"),
    ("view.logs.settings.title", "Settings", "设置"),
    (
        "view.logs.settings.guide",
        "Open Settings → Plugins → MyBatis SQL Visualizer to change the dialect and log options.",
        "请在 Settings → Plugins → MyBatis SQL Visualizer 中修改方言与日志选项。",
    ),
    ("view.logs.button.help", "Help", "帮助"),
    ("view.logs.status.follow_on", "Following latest", "跟随最新"),
    ("view.logs.status.follow_off", "Follow paused", "跟随已暂停"),
    (
        "view.logs.group.expand",
        "Expand this group of statements",
        "展开这组同结构语句",
    ),
    (
        "view.logs.group.collapse",
        "Collapse this group",
        "折叠这组语句",
    ),
    (
        "view.logs.help.title",
        "Keyboard shortcuts and interactions",
        "快捷键与交互速查",
    ),
    (
        "view.logs.help.search",
        "Ctrl+F: open the search row (case / regex toggles)",
        "Ctrl+F：打开搜索行（可切换大小写/正则）",
    ),
    (
        "view.logs.help.tab",
        "Ctrl+Tab / Ctrl+Shift+Tab: switch tabs",
        "Ctrl+Tab / Ctrl+Shift+Tab：切换页签",
    ),
    (
        "view.logs.help.dblclick",
        "Double-click a row: send its SQL to the SQL Preview workbench",
        "双击行：把该行 SQL 送入「SQL 预览」工作台",
    ),
    (
        "view.logs.help.menu",
        "Right-click a row: copy SQL / copy timestamp / delete / mute / send to SQL→Java / send to Log→SQL / jump to source / jump to Mapper XML / focus by method / focus by request / focus by template / export visible rows",
        "右键行：复制 SQL / 复制时间戳 / 删除 / 屏蔽此方法 / 发送到 SQL→Java / 发送到 日志→SQL / 跳转源码 / 跳转 Mapper XML / 按方法聚焦 / 按请求聚焦 / 按模板聚焦 / 导出可见行集",
    ),
    (
        "view.logs.help.scroll",
        "Scroll the list: leaving the bottom pauses follow; scroll back or click Scroll to bottom to resume",
        "滚动列表：滚离底部即暂停跟随，滚回底部或点「滚动到底」恢复",
    ),
    (
        "view.logs.help.sort",
        "Sort by duration: toggles between time order and duration order",
        "「按耗时排序」：在时间正序与耗时降序间切换",
    ),
    (
        "view.logs.help.blocked",
        "Blocked methods: view the muted methods and restore them all",
        "「屏蔽的方法」：查看已屏蔽的方法并全部恢复",
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
    ("view.workbench.action.explain", "Explain", "执行计划"),
    ("view.workbench.action.refresh", "Refresh", "刷新"),
    ("view.workbench.action.more", "More", "更多"),
    (
        "view.workbench.more.note",
        "Export, diff, share, qualify columns and rollback",
        "导出、差异对比、分享、含表列名与回滚",
    ),
    ("view.workbench.banner.dismiss", "Dismiss", "关闭"),
    ("view.workbench.params.name", "Param {0}", "参数 {0}"),
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
    // Workbench parameters, clipboard import and rollback (lane L27).
    ("view.workbench.action.rebind", "Rebind", "重绑定"),
    (
        "view.workbench.action.import_clipboard",
        "Import from Clipboard",
        "从剪贴板导入",
    ),
    (
        "view.workbench.action.rollback",
        "Generate Rollback",
        "生成回滚",
    ),
    (
        "view.workbench.rollback.note",
        "The workbench has no table / primary-key metadata, so a rollback plan cannot be generated here; the rollback-plan command takes a table description (columns + primary key).",
        "工作台没有表/主键元数据，无法在此生成回滚；回滚计划需要表结构（列 + 主键），请用 rollback-plan 命令并给出表描述。",
    ),
    // Lane L44: the "更多" menu's two usable entries plus the one the renderer
    // would have to provide.
    ("view.workbench.action.diff", "Diff with source", "差异对比"),
    (
        "view.workbench.diff.title",
        "SQL preview diff",
        "SQL 预览差异",
    ),
    ("view.workbench.diff.inline", "inline statement", "行内语句"),
    (
        "view.workbench.diff.note",
        "Original on the left, rendered SQL on the right; open it in the platform diff viewer.",
        "左侧原文、右侧渲染出的 SQL，在平台差异查看器中打开。",
    ),
    (
        "view.workbench.action.share",
        "Share (copy)",
        "分享（复制）",
    ),
    (
        "view.workbench.share.note",
        "No share service or network channel: the entry copies the SQL to the clipboard.",
        "没有分享服务与联网通道：此入口把 SQL 复制到剪贴板。",
    ),
    (
        "view.workbench.action.qualify",
        "Qualify columns",
        "含表列名",
    ),
    (
        "view.workbench.qualify.note",
        "Rendering each column as table.column is the renderer's job; neither the core renderer nor the engine jar has such a switch yet, so the entry stays disabled.",
        "把列渲染成 表名.列名 是渲染器的职责；core 渲染器与引擎 jar 都还没有这个开关，因此此项保持禁用。",
    ),
    (
        "view.workbench.status.diff_opened",
        "Opened the diff tab.",
        "已打开差异页签。",
    ),
    (
        "view.workbench.status.diff_empty",
        "Nothing to compare: render a statement first.",
        "没有可对比的内容：请先解析预览。",
    ),
    (
        "view.workbench.status.shared",
        "SQL copied to the clipboard.",
        "SQL 已复制到剪贴板。",
    ),
    (
        "view.workbench.status.share_empty",
        "Nothing to share: render a statement first.",
        "没有可分享的内容：请先解析预览。",
    ),
    (
        "view.workbench.status.qualify_refused",
        "Qualifying columns needs the renderer: neither the core renderer nor the engine has a table-qualifier switch yet.",
        "含表列名需要渲染器支持：core 渲染器与引擎都还没有表名限定开关。",
    ),
    (
        "view.workbench.params.hint",
        "Edit a value, then Rebind to write it into the SQL; numbers and booleans stay bare, everything else is quoted.",
        "修改参数后点「重绑定」写入 SQL；数字与布尔值裸写，其余按字符串加引号。",
    ),
    (
        "view.workbench.status.clipboard_missing",
        "The clipboard capability is not granted; importing from the clipboard is unavailable.",
        "未授予剪贴板能力，无法从剪贴板导入。",
    ),
    (
        "view.workbench.status.imported",
        "Imported {0} value(s) from the clipboard (not rebound).",
        "已从剪贴板导入 {0} 个值（未自动重绑定）。",
    ),
    (
        "view.workbench.status.import_failed",
        "Could not read the clipboard: {0}",
        "读取剪贴板失败：{0}",
    ),
    (
        "view.workbench.status.params_empty",
        "There are no parameter values to bind.",
        "没有可回填的参数值。",
    ),
    (
        "view.workbench.status.params_no_placeholder",
        "The SQL has no ? placeholder to rebind.",
        "SQL 中没有可回填的 ? 占位符。",
    ),
    (
        "view.workbench.status.rebound",
        "Parameters rebound into the SQL.",
        "参数已回填到 SQL。",
    ),
    (
        "view.workbench.status.rollback_refused",
        "A rollback plan needs the table's primary key; the workbench has no table metadata, so no plan is generated (use the rollback-plan command with a table description).",
        "回滚计划需要表的主键；工作台没有表元数据，因此不生成回滚（请用 rollback-plan 命令并给出表描述）。",
    ),
    // Statement picker and display switches (lane L45).
    ("view.workbench.statement", "Statement", "目标语句"),
    (
        "view.workbench.statement.note",
        "Switch which statement of this mapper the preview shows; parameters, dialect and display mode stay as they are.",
        "切换预览该 mapper 里的哪条语句；参数、方言与显示模式保持不变。",
    ),
    (
        "view.workbench.statement.none",
        "This mapper has a single statement.",
        "该 mapper 只有一条语句。",
    ),
    (
        "view.workbench.status.no_statements",
        "There is no mapper behind this render, so there is no statement to switch to.",
        "当前渲染不来自 mapper，没有可切换的语句。",
    ),
    (
        "view.workbench.status.statement_unknown",
        "No statement with id {0} in this mapper.",
        "该 mapper 里没有 id 为 {0} 的语句。",
    ),
    (
        "view.workbench.display.keyword_wrap",
        "Wrap at keywords",
        "关键字换行",
    ),
    (
        "view.workbench.display.keyword_wrap.note",
        "Display only: break the SQL before top-level keywords (SELECT/FROM/WHERE/…). The SQL itself is unchanged.",
        "仅影响显示：在每个顶层关键字（SELECT/FROM/WHERE/…）前断行，SQL 本身不变。",
    ),
    (
        "view.workbench.display.vertical",
        "One item per line",
        "竖向排列",
    ),
    (
        "view.workbench.display.vertical.note",
        "Display only: put each comma-separated item on its own line. The SQL itself is unchanged.",
        "仅影响显示：逗号分隔的每一项各占一行，SQL 本身不变。",
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
    // SQL log panel tabs (lane L22, docs/registration/L22.md).
    ("view.logs.tab.logs", "SQL Logs", "SQL 日志"),
    ("view.logs.tab.log_to_sql", "Log → SQL", "日志 → SQL"),
    ("view.logs.tab.sql_to_java", "SQL → Java", "SQL → Java"),
    ("view.logs.tab.history", "SQL History", "SQL 历史"),
    // Fifth tab: the preview workbench, as the IntelliJ edition's "SQL 预览"
    // tab of the SQL Log tool window (lane L23).
    ("view.logs.tab.preview", "SQL Preview", "SQL 预览"),
    // Two input tabs (lane L33, docs/registration/L33.md).
    ("view.logs.tab.log_to_sql.convert", "Convert", "转换"),
    (
        "view.logs.tab.log_to_sql.read_clipboard",
        "Read from Clipboard",
        "从剪贴板读取",
    ),
    ("view.logs.tab.log_to_sql.clear_input", "Clear", "清空"),
    (
        "view.logs.tab.log_to_sql.hint_input",
        "Paste MyBatis log lines here (Preparing: / Parameters:), then click Convert.",
        "在此粘贴 MyBatis 日志（Preparing: / Parameters:），点「转换」。",
    ),
    (
        "view.logs.tab.log_to_sql.hint_result",
        "Restored SQL appears here; edit it freely before copying.",
        "还原后的 SQL 显示在此处，可直接改写后复制。",
    ),
    ("view.logs.tab.log_to_sql.copy", "Copy SQL", "复制 SQL"),
    (
        "view.logs.tab.log_to_sql.toggle_format",
        "Toggle Single/Multi-line",
        "压缩/多行切换",
    ),
    (
        "view.logs.tab.log_to_sql.clear_result",
        "Clear Result",
        "清空结果",
    ),
    (
        "view.logs.tab.log_to_sql.empty",
        "Please paste MyBatis log first",
        "请先粘贴 MyBatis 日志",
    ),
    (
        "view.logs.tab.log_to_sql.no_entry",
        "No SQL log lines found. Paste MyBatis (Preparing: / Parameters:), Hibernate (Hibernate:) or JdbcTemplate (Executing prepared SQL statement [...]) format logs.",
        "未解析到 SQL 日志行。请粘贴 MyBatis（Preparing: / Parameters:）、Hibernate（Hibernate:）或 JdbcTemplate（Executing prepared SQL statement [...]）格式的日志。",
    ),
    (
        "view.logs.tab.log_to_sql.clipboard_empty",
        "The clipboard holds no text",
        "剪贴板中没有文本",
    ),
    (
        "view.logs.tab.log_to_sql.clipboard_denied",
        "This build has no clipboard-reading capability (clipboard.read), so the clipboard cannot be read here.",
        "本构建未获得剪贴板读取能力（clipboard.read），无法读取剪贴板。",
    ),
    (
        "view.logs.tab.log_to_sql.converted",
        "Converted {0} statement(s)",
        "已转换 {0} 条 SQL",
    ),
    (
        "view.logs.tab.log_to_sql.unresolved",
        "{0} statement(s) still contain ? placeholders: parameter count does not match the log.",
        "{0} 条 SQL 仍含 ? 占位符：日志参数数量不匹配。",
    ),
    (
        "view.logs.tab.log_to_sql.copied",
        "SQL copied to clipboard",
        "SQL 已复制到剪贴板",
    ),
    ("view.logs.tab.sql_to_java.convert", "Convert", "转换"),
    (
        "view.logs.tab.sql_to_java.format",
        "Format SQL",
        "格式化 SQL",
    ),
    ("view.logs.tab.sql_to_java.clear_input", "Clear", "清空"),
    (
        "view.logs.tab.sql_to_java.hint_input",
        "Paste a SELECT statement here, then click Convert.",
        "在此粘贴 SELECT 语句，点「转换」生成 MyBatis-Plus 代码。",
    ),
    (
        "view.logs.tab.sql_to_java.hint_result",
        "Generated Java code appears here; edit it freely before copying.",
        "生成的 Java 代码显示在此处，可直接改写后复制。",
    ),
    ("view.logs.tab.sql_to_java.copy", "Copy Code", "复制代码"),
    (
        "view.logs.tab.sql_to_java.clear_result",
        "Clear Result",
        "清空结果",
    ),
    (
        "view.logs.tab.sql_to_java.format_label",
        "Wrapper form",
        "生成形态",
    ),
    (
        "view.logs.tab.sql_to_java.format_lambda",
        "Lambda (column refs)",
        "Lambda（列引用）",
    ),
    (
        "view.logs.tab.sql_to_java.format_string",
        "String column names",
        "字符串列名",
    ),
    (
        "view.logs.tab.sql_to_java.entity_label",
        "Entity class",
        "实体类名",
    ),
    (
        "view.logs.tab.sql_to_java.entity_hint",
        "Entity class (required, e.g. User)",
        "实体类名（必填，如 User）",
    ),
    (
        "view.logs.tab.sql_to_java.caller_label",
        "Caller class",
        "调用类",
    ),
    (
        "view.logs.tab.sql_to_java.caller_hint",
        "Caller class (e.g. UserService)",
        "调用类（如 UserService）",
    ),
    (
        "view.logs.tab.sql_to_java.return_label",
        "Result type",
        "返回类型",
    ),
    (
        "view.logs.tab.sql_to_java.return_hint",
        "Result type (e.g. List<User>)",
        "返回类型（如 List<User>）",
    ),
    (
        "view.logs.tab.sql_to_java.empty",
        "Please enter SQL first",
        "请先输入 SQL",
    ),
    (
        "view.logs.tab.sql_to_java.generated",
        "Generated MyBatis-Plus Java code",
        "已生成 MyBatis-Plus Java 代码",
    ),
    (
        "view.logs.tab.sql_to_java.fixed_lambda",
        "(this build always emits a Lambda chain; form / caller / result type only appear here)",
        "（本版固定生成 Lambda 链；形态/调用类/返回类型仅作提示）",
    ),
    (
        "view.logs.tab.sql_to_java.no_code",
        "No code generated",
        "未生成代码",
    ),
    (
        "view.logs.tab.sql_to_java.copied",
        "Java code copied to clipboard",
        "Java 代码已复制到剪贴板",
    ),
    (
        "view.logs.tab.history.keyword",
        "Search the captured log buffer…",
        "搜索已捕获的日志缓冲区…",
    ),
    ("view.logs.tab.history.search", "Search", "搜索"),
    (
        "view.logs.tab.history.empty",
        "Type a keyword to search the captured log buffer.",
        "输入关键字搜索已捕获的日志缓冲区。",
    ),
    (
        "view.logs.tab.history.no_match",
        "No matching lines.",
        "没有匹配的行。",
    ),
    ("view.logs.tab.history.count_suffix", "matches", "条命中"),
    ("view.logs.tab.history.source", "Source", "来源"),
    (
        "view.logs.tab.history.truncated",
        "Only the first matches are listed.",
        "只列出前若干条命中。",
    ),
    // Two-level history (parity with HistorySqlLogPanel): files first, then the
    // statements of the opened file.
    (
        "view.logs.tab.history.files_empty",
        "No history files yet. Logs are saved when you capture or open one.",
        "尚无历史文件；捕获或打开日志后会自动保存。",
    ),
    (
        "view.logs.tab.history.entries_empty",
        "This file has no statements to show.",
        "该文件没有可显示的 SQL。",
    ),
    (
        "view.logs.tab.history.search_files",
        "Search history files…",
        "搜索历史文件…",
    ),
    (
        "view.logs.tab.history.search_entries",
        "Search SQL / source…",
        "搜索 SQL / 源文件…",
    ),
    (
        "view.logs.tab.history.file_count_suffix",
        "statements",
        "条 SQL",
    ),
    (
        "view.logs.tab.history.files_count_suffix",
        "files",
        "个文件",
    ),
    ("view.logs.tab.history.back", "Back", "返回"),
    ("view.logs.tab.history.refresh", "Refresh", "刷新"),
    (
        "view.logs.tab.history.scroll_end",
        "Scroll to end",
        "滚动到底",
    ),
    (
        "view.logs.tab.history.preview_toggle",
        "Quick preview",
        "快速预览",
    ),
    (
        "view.logs.tab.history.preview_empty",
        "Select a statement to preview it here.",
        "选中一条 SQL 在此快速预览。",
    ),
    ("view.logs.tab.history.copy", "Copy SQL", "复制 SQL"),
    ("view.logs.tab.history.jump_java", "Go to Java", "跳转 Java"),
    (
        "view.logs.tab.history.jump_xml",
        "Go to Mapper XML",
        "跳转 Mapper XML",
    ),
    (
        "view.logs.tab.history.open_preview",
        "Open in SQL preview",
        "在 SQL 预览中打开",
    ),
    // Lane L32: row fragments, toolbar entries and row actions the reference
    // panel has (duration unit, affected-row suffix, quick preview, community,
    // per-template focus and the two "send to an input tab" hand-offs).
    ("view.logs.row.duration_suffix", "ms", "ms"),
    ("view.logs.row.rows_suffix", "rows", "行"),
    ("view.logs.button.preview", "Quick preview", "快速预览"),
    ("view.logs.menu.sort_time", "Sort by time", "按时间排序"),
    ("view.logs.menu.settings_page", "Settings page…", "设置页…"),
    (
        "view.logs.menu.focus_template",
        "Focus by template",
        "按模板聚焦",
    ),
    (
        "view.logs.menu.send_sql_to_java",
        "Send to SQL\u{2192}Java",
        "发送到 SQL\u{2192}Java",
    ),
    (
        "view.logs.menu.send_log_to_sql",
        "Send to Log\u{2192}SQL",
        "发送到 日志\u{2192}SQL",
    ),
    ("view.logs.focus.template_prefix", "Template", "模板"),
    ("view.logs.button.community", "Community", "交流群"),
    ("view.logs.community.title", "Community", "交流群"),
    (
        "view.logs.community.guide",
        "The community group and the author's contact are listed on the project page.",
        "交流群与作者联系方式见项目主页。",
    ),
    // SQL preview entry (L35a): the gutter button and the command's editor path.
    ("preview.gutter.tooltip", "Preview SQL", "预览 SQL"),
    (
        "preview.error.no_input",
        "Nothing to preview: pass a mapper path, `path#statement` or SQL text, or open a file in the editor.",
        "没有可预览的内容：请传入 Mapper 路径、`路径#语句` 或 SQL 文本，或在编辑器中打开文件。",
    ),
    (
        "preview.error.editor_unreadable",
        "The current editor cannot be read (editor.read is missing or the document is stale); pass the SQL or a mapper path as the command argument instead.",
        "无法读取当前编辑器（缺少 editor.read 或文档已过期）；请改用命令参数传入 SQL 或 Mapper 路径。",
    ),
    // Diff-tab preview (the reachable "pop out into a window" path).
    ("preview.diff.left", "Original", "原文"),
    ("preview.diff.right", "SQL", "SQL"),
    ("preview.diff.title", "SQL preview", "SQL 预览"),
    (
        "preview.diff.empty_sql",
        "Nothing to preview: the statement did not render to SQL",
        "没有可预览的 SQL：该语句没有渲染出 SQL",
    ),
    // Hover SQL preview (L37): card titles, counts and notices. Core render
    // reasons stay as `[message_key] detail`, the workbench convention.
    ("hover.title.sql", "SQL", "SQL"),
    ("hover.title.fragment", "SQL fragment", "SQL 片段"),
    ("hover.title.resultmap", "Result map", "结果映射"),
    ("hover.title.method", "mapper method", "Mapper 方法"),
    (
        "hover.fragment.used_prefix",
        "Included in this file:",
        "本文件内被 include：",
    ),
    ("hover.fragment.used_suffix", "time(s)", "次"),
    ("hover.resultmap.type_prefix", "Maps to", "映射到"),
    ("hover.resultmap.extends_prefix", "Extends", "继承自"),
    (
        "hover.resultmap.used_prefix",
        "Used by statements in this file:",
        "本文件内被语句引用：",
    ),
    (
        "hover.sql.truncated_prefix",
        "SQL truncated; showing the first",
        "SQL 已截断，仅显示前",
    ),
    ("hover.sql.truncated_suffix", "characters", "个字符"),
    ("hover.java.ambiguous_prefix", "Found", "找到"),
    (
        "hover.java.ambiguous_suffix",
        "candidate mapper statements; cannot choose one.",
        "处候选 Mapper 语句，无法确定唯一一条。",
    ),
    // Capture start (lane A): the sandbox gives the plugin a WebSocket client
    // only, so the agent has to be running before the plugin can dial it. A
    // refused dial is a waiting state, and `{port}` is filled in by the caller.
    (
        "capture.start.waiting",
        "Capture started; waiting for the target application to start (port {port})",
        "已开始捕获，等待目标应用启动（端口 {port}）",
    ),
    (
        "capture.start.connected",
        "Connected to the target application (port {port})",
        "已接入目标应用（端口 {port}）",
    ),
    // Stopping while nothing is attached but the plugin is still waiting: the
    // generic "no WebSocket is open" reads like a mistake, because the user did
    // start capture and is now stopping it.
    (
        "capture.stop.waiting",
        "Stopped waiting for the target application",
        "已停止等待目标应用",
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
