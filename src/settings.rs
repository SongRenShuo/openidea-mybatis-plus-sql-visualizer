//! Settings schema and accessors (T2-3).
//!
//! The manifest declares `[contributes.settings] title` plus eight pages
//! (`settings/ui-actions.json`, `shortcuts.json`, `sql-preview.json`,
//! `table-mapping.json`, `sql-log.json`, `mcp.json`, `about.json` and
//! `engine.json`); the IDE generates one page per file under the plugin's
//! Settings node. [`DEFINITIONS`] mirrors them in code so callers can enumerate
//! keys, defaults and which changes need reopening the IDE. The tests keep the
//! two in sync.
//!
//! Code reads values through the `get_*` helpers, which fall back to the default
//! when a stored value has the wrong type or the user never touched the setting.

use openidea_plugin::host;

/// The dialect names matching `core::dialect::all()`, for the enum choice.
pub const DIALECTS: &[&str] = &[
    "MySQL",
    "PostgreSQL",
    "Oracle",
    "SQL Server",
    "H2",
    "SQLite",
    "MariaDB",
    "DM（达梦）",
    "KingBase（人大金仓）",
    "OceanBase",
    "openGauss",
    "GaussDB",
    "HighGo",
    "Vastbase",
    "GoldenDB",
];

/// Groups share a heading on the generated page, in this order.
pub const GROUPS: &[&str] = &[
    "Gutter",
    "XML support",
    "Editor",
    "Onboarding",
    "Language",
    "Preview",
    "Dialect",
    "Rendering",
    "MyBatis Plus",
    "Advanced",
    "Code generation",
    "Table Mapping",
    "MyBatis core",
    "List display",
    "CRUD colors",
    "ORM capture",
    "Auto save",
    "Source capture",
    "Shortcuts",
    "MCP",
    "About",
    "Engine",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bool,
    Integer,
    String,
    /// A string restricted to the listed values.
    Enum(&'static [&'static str]),
}

impl Kind {
    /// The JSON Schema `type` of the property.
    pub fn json_type(self) -> &'static str {
        match self {
            Kind::Bool => "boolean",
            Kind::Integer => "integer",
            Kind::String | Kind::Enum(_) => "string",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DefaultValue {
    Bool(bool),
    Int(i64),
    Str(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SettingDef {
    pub key: &'static str,
    pub title: &'static str,
    pub group: &'static str,
    pub kind: Kind,
    pub default: DefaultValue,
    /// The value is read when providers/injections are created, so a change
    /// only takes effect after reopening the affected files (or the IDE).
    pub needs_restart: bool,
}

/// The first release's settings, the L34 parity keys, the L48 preview shell,
/// the C14 MyBatis-Plus interceptor simulation and the L-SettingsParity
/// expansion to the IntelliJ edition's 81 application-level fields.
/// Not every key has a consumer in this port yet. The list below is kept next
/// to the table so a reader (and the lane report) can tell a working switch
/// from a placeholder; each such row repeats the boundary in its `description`
/// on the settings page.
///
/// stored, not wired:
/// - UI and Actions: `ui.gutter.mapperXml/annotation/jpaHql/jdbc/
///   inlineFallback/wrapperChain` (only the master switch
///   `nav.showLineMarkers` is read; this build produces mapper XML icons
///   only), `ui.mapperNavYield`, `ui.xmlInlineSql`, `ui.ibatisXml`,
///   `ui.xmlSyntaxHighlight`, `ui.consolePrepMode`, `ui.pagesXmlSourceMode`,
///   `ui.joinDiagramNative`, `ui.showWhitespaces`, `ui.onboardingAnchorTip`,
///   `ui.dialectTip`, `ui.firstCaptureGuide`, `ui.language` (this port always
///   follows the host locale).
/// - Shortcuts: every row is read-only text; the platform has no key recorder,
///   so the reference's default keys are shown and changed in IDE Keymap.
/// - SQL Preview: `preview.replaceParams`, `preview.verticalPairAnnotation`,
///   `preview.autoFoldEnabled/Threshold`, `preview.emptyParamFill`,
///   `preview.literalParamRecovery`, `preview.injectTenantOnInsert`,
///   `preview.dataPermissionHint/Condition`, `preview.paginationCount`,
///   `preview.jdbcPreview`, `preview.explainStructured`,
///   `preview.explainRetention`, `preview.executionTimeoutSeconds`,
///   `preview.embeddedColumnNaming`, and the code-generation trio (there is no
///   SQL → Java command in this port).
/// - Dialect / Rendering (pre-existing placeholders): `dialect.mode` (only
///   `dialect.default` is read), `dialect.detectFromUrl`,
///   `dialect.autoDetectFromMappers`, `dialect.quoteIdentifiers`,
///   `dialect.keywordCase`, `render.paramsSeparator`, `render.sqlCase`,
///   `render.dateFormat`, `render.showConfidence`, `render.formatSql`,
///   `render.indentWidth`, `render.truncateSql`, `render.exportFormat`.
/// - Table Name Mapping: all except `preview.tablePrefix` (preview page);
///   entity inference stays fixed to camel → snake.
/// - SQL Log: the panel / auto-save half — `log.autoCaptureOnStart`,
///   `log.autoOpenOnCapture`, `log.autoOpenYield`, `log.stopOnWindowClose`,
///   `log.timestampEnabled`, `log.timestampFormat`, `log.timestampSource`,
///   `log.formatRestored`, `log.showMethodName`, `log.dataSourceBadgeDetail`,
/// `logs.crud.style`, `logs.crud.select/insert/update/delete`,
///   `log.shardingSphereEnabled`, `log.autoSaveEnabled/Path/FileNamePattern/
///   MaxFiles`, `log.stackMaxFrames` (no call-stack collection),
///   `log.sourceFrameSkipPrefixes`, `log.sourceFrameBizJarPrefixes`,
///   `log.desensitize`.
/// - MCP Tools: `mcp.enabled`, `mcp.disabledTools`, `mcp.mask`.
/// - About: `about.updateChannel`, `about.buildNumber` (read-only) and
///   `about.cacheAction` (explanatory text only).
///
/// Wired by this lane through `core::log::ParseOptions` (read by the capture
/// bridge and the log commands): `log.prefix`, `log.paramPrefix`,
/// `log.mixedPrefix`, `log.filterKeywords`, `log.timePrefix`,
/// `log.hibernateEnabled`, `log.hibernateSqlPrefix`, `log.hibernateParamPrefix`
/// and `log.jdbcEnabled`.
pub static DEFINITIONS: [SettingDef; 142] = [
    // Preview -------------------------------------------------------------
    // Lane L48: where the preview opens. The dialog is the IntelliJ edition's
    // `SqlPreviewDialog`; the tab is the fifth tab of the log panel, which the
    // build shipped before API 1.18 existed. Read on every open, so a change
    // applies to the next preview without a restart.
    SettingDef {
        key: "preview.presentation",
        title: "预览打开位置",
        group: "Preview",
        kind: Kind::Enum(&["dialog", "tab"]),
        default: DefaultValue::Str("dialog"),
        needs_restart: false,
    },
    // MyBatis-Plus interceptors (C14) ---------------------------------------
    // The reference derives "is there a logic-delete column" from the entity
    // (`@TableLogic`). This build has no entity at preview time, so the column
    // is a setting and every switch starts off: a simulation that cannot know
    // whether the project uses the interceptor must not rewrite SQL by
    // default. Read on every render, so a change applies to the next preview.
    SettingDef {
        key: "preview.injectLogicDelete",
        title: "模拟逻辑删除拦截器",
        group: "MyBatis Plus",
        kind: Kind::Bool,
        default: DefaultValue::Bool(false),
        needs_restart: false,
    },
    SettingDef {
        key: "preview.logicDeleteColumn",
        title: "逻辑删除列名",
        group: "MyBatis Plus",
        kind: Kind::String,
        default: DefaultValue::Str("deleted"),
        needs_restart: false,
    },
    SettingDef {
        key: "preview.logicDeleteValue",
        title: "逻辑删除值",
        group: "MyBatis Plus",
        kind: Kind::String,
        default: DefaultValue::Str("0"),
        needs_restart: false,
    },
    SettingDef {
        key: "preview.injectTenantLine",
        title: "模拟多租户拦截器",
        group: "MyBatis Plus",
        kind: Kind::Bool,
        default: DefaultValue::Bool(false),
        needs_restart: false,
    },
    SettingDef {
        key: "preview.tenantColumnName",
        title: "租户列名",
        group: "MyBatis Plus",
        kind: Kind::String,
        default: DefaultValue::Str("tenant_id"),
        needs_restart: false,
    },
    SettingDef {
        key: "preview.tenantColumnDefault",
        title: "租户列默认值",
        group: "MyBatis Plus",
        kind: Kind::String,
        default: DefaultValue::Str("1"),
        needs_restart: false,
    },
    SettingDef {
        key: "preview.paginationSim",
        title: "模拟分页拦截器",
        group: "MyBatis Plus",
        kind: Kind::Bool,
        default: DefaultValue::Bool(false),
        needs_restart: false,
    },
    SettingDef {
        key: "preview.paginationCurrent",
        title: "页码",
        group: "MyBatis Plus",
        kind: Kind::Integer,
        default: DefaultValue::Int(1),
        needs_restart: false,
    },
    SettingDef {
        key: "preview.paginationSize",
        title: "每页条数",
        group: "MyBatis Plus",
        kind: Kind::Integer,
        default: DefaultValue::Int(10),
        needs_restart: false,
    },
    SettingDef {
        key: "preview.tablePrefix",
        title: "预览时的表名前缀",
        group: "MyBatis Plus",
        kind: Kind::String,
        default: DefaultValue::Str(""),
        needs_restart: false,
    },
    // Dialect -------------------------------------------------------------
    SettingDef {
        key: "dialect.mode",
        title: "方言模式",
        group: "Dialect",
        kind: Kind::Enum(&["auto", "manual"]),
        default: DefaultValue::Str("auto"),
        needs_restart: false,
    },
    SettingDef {
        key: "dialect.default",
        title: "默认方言",
        group: "Dialect",
        kind: Kind::Enum(DIALECTS),
        default: DefaultValue::Str("MySQL"),
        needs_restart: false,
    },
    SettingDef {
        key: "dialect.detectFromUrl",
        title: "从 JDBC URL 推断方言",
        group: "Dialect",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "dialect.autoDetectFromMappers",
        title: "从 mapper 语句推断方言",
        group: "Dialect",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "dialect.quoteIdentifiers",
        title: "标识符加引号",
        group: "Dialect",
        kind: Kind::Enum(&["auto", "always", "never"]),
        default: DefaultValue::Str("auto"),
        needs_restart: false,
    },
    SettingDef {
        key: "dialect.keywordCase",
        title: "关键字大小写",
        group: "Dialect",
        kind: Kind::Enum(&["auto", "upper", "lower"]),
        default: DefaultValue::Str("auto"),
        needs_restart: false,
    },
    // SQL Preview page, L-SettingsParity ----------------------------------
    // Reference parity for the preview panel. `preview.injectTenantOnInsert`,
    // `preview.dataPermissionHint/Condition`, `preview.paginationCount`,
    // `preview.jdbcPreview`, the EXPLAIN switches, the execution timeout and the
    // embedded-column naming are stored, not wired: this build's Rust preview
    // has no consumer for them yet, and the panel description says so.
    SettingDef {
        key: "preview.replaceParams",
        title: "替换 ? 占位符",
        group: "Preview",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "preview.verticalPairAnnotation",
        title: "竖向排列值行列名标注",
        group: "Preview",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "preview.autoFoldEnabled",
        title: "SQL 预览字段较多时自动折叠",
        group: "Preview",
        kind: Kind::Bool,
        default: DefaultValue::Bool(false),
        needs_restart: false,
    },
    SettingDef {
        key: "preview.autoFoldThreshold",
        title: "自动折叠字段数阈值",
        group: "Preview",
        kind: Kind::Integer,
        default: DefaultValue::Int(10),
        needs_restart: false,
    },
    SettingDef {
        key: "preview.emptyParamFill",
        title: "空参数填充策略",
        group: "Preview",
        kind: Kind::Enum(&["placeholder", "null", "empty_string"]),
        default: DefaultValue::Str("placeholder"),
        needs_restart: false,
    },
    SettingDef {
        key: "preview.literalParamRecovery",
        title: "零占位符字面量参数恢复",
        group: "Preview",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "preview.injectTenantOnInsert",
        title: "INSERT 时追加租户列",
        group: "MyBatis Plus",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "preview.dataPermissionHint",
        title: "显示 DataPermissionInterceptor 运行时差异提示",
        group: "MyBatis Plus",
        kind: Kind::Bool,
        default: DefaultValue::Bool(false),
        needs_restart: false,
    },
    SettingDef {
        key: "preview.dataPermissionCondition",
        title: "数据权限模拟条件",
        group: "MyBatis Plus",
        kind: Kind::String,
        default: DefaultValue::Str(""),
        needs_restart: false,
    },
    SettingDef {
        key: "preview.paginationCount",
        title: "附分页 COUNT 查询预览",
        group: "MyBatis Plus",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "preview.jdbcPreview",
        title: "启用 JdbcTemplate SQL 字符串预览",
        group: "Advanced",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "preview.explainStructured",
        title: "预览面板内结构化渲染 EXPLAIN",
        group: "Advanced",
        kind: Kind::Bool,
        default: DefaultValue::Bool(false),
        needs_restart: false,
    },
    SettingDef {
        key: "preview.explainRetention",
        title: "跨会话保留原始 EXPLAIN 计划",
        group: "Advanced",
        kind: Kind::Bool,
        default: DefaultValue::Bool(false),
        needs_restart: false,
    },
    SettingDef {
        key: "preview.executionTimeoutSeconds",
        title: "执行超时（秒，0 = 不限）",
        group: "Advanced",
        kind: Kind::Integer,
        default: DefaultValue::Int(30),
        needs_restart: false,
    },
    SettingDef {
        key: "preview.embeddedColumnNaming",
        title: "嵌入列命名",
        group: "Advanced",
        kind: Kind::Enum(&["auto", "none", "component_path"]),
        default: DefaultValue::Str("auto"),
        needs_restart: false,
    },
    // Code generation group of the SQL Preview page. The port has no SQL → Java
    // command, so these three are stored, not wired.
    SettingDef {
        key: "codegen.reverseWrapperFormat",
        title: "SQL → Java 默认 Wrapper 形态",
        group: "Code generation",
        kind: Kind::Enum(&["auto", "query", "lambda", "wrappers", "mpj_lambda"]),
        default: DefaultValue::Str("auto"),
        needs_restart: false,
    },
    SettingDef {
        key: "codegen.manualEntitySelection",
        title: "自动推断失败时允许手动选择实体类",
        group: "Code generation",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "codegen.libraryEntitySweep",
        title: "后台扫描依赖库以反查实体",
        group: "Code generation",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    // Table Name Mapping page, L-SettingsParity ---------------------------
    // Entity table-name inference in this port is fixed to camel → snake and the
    // preview only reads `preview.tablePrefix`; every key below is stored, not
    // wired (the panel description says so).
    SettingDef {
        key: "table.suffix",
        title: "表名后缀",
        group: "Table Mapping",
        kind: Kind::String,
        default: DefaultValue::Str(""),
        needs_restart: false,
    },
    SettingDef {
        key: "table.removePrefix",
        title: "移除表名前缀",
        group: "Table Mapping",
        kind: Kind::String,
        default: DefaultValue::Str(""),
        needs_restart: false,
    },
    SettingDef {
        key: "table.removeSuffix",
        title: "移除表名后缀",
        group: "Table Mapping",
        kind: Kind::String,
        default: DefaultValue::Str(""),
        needs_restart: false,
    },
    SettingDef {
        key: "table.ignoredFields",
        title: "忽略字段名",
        group: "Table Mapping",
        kind: Kind::String,
        default: DefaultValue::Str(""),
        needs_restart: false,
    },
    SettingDef {
        key: "table.inferenceStrategy",
        title: "表名推断策略",
        group: "Table Mapping",
        kind: Kind::Enum(&["snake", "camel"]),
        default: DefaultValue::Str("snake"),
        needs_restart: false,
    },
    SettingDef {
        key: "table.columnCalibration",
        title: "按数据源校正列名",
        group: "Table Mapping",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    // SQL Log page, L-SettingsParity ---------------------------------------
    // Six groups in the reference's order: MyBatis core, List display, CRUD
    // colors, ORM capture, Auto save, Source capture. `log.prefix`,
    // `log.paramPrefix`, `log.mixedPrefix`, `log.filterKeywords`,
    // `log.timePrefix`, the Hibernate prefixes and the Hibernate / JDBC
    // switches are read by `core::log` through `ParseOptions`; everything the
    // log panel or the auto-save chain owns is stored, not wired here and says
    // so on the page.
    SettingDef {
        key: "log.prefix",
        title: "日志 SQL 前缀",
        group: "MyBatis core",
        kind: Kind::String,
        default: DefaultValue::Str("Preparing:"),
        needs_restart: false,
    },
    SettingDef {
        key: "log.paramPrefix",
        title: "日志参数前缀",
        group: "MyBatis core",
        kind: Kind::String,
        default: DefaultValue::Str("Parameters:"),
        needs_restart: false,
    },
    SettingDef {
        key: "log.mixedPrefix",
        title: "混合前缀匹配",
        group: "MyBatis core",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "log.autoCaptureOnStart",
        title: "项目启动时自动开始监听",
        group: "MyBatis core",
        kind: Kind::Bool,
        default: DefaultValue::Bool(false),
        needs_restart: false,
    },
    SettingDef {
        key: "log.autoOpenOnCapture",
        title: "发现 SQL 自动打开窗口",
        group: "MyBatis core",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "log.autoOpenYield",
        title: "自动展开时让位给正在使用的面板",
        group: "MyBatis core",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "log.stopOnWindowClose",
        title: "关闭工具窗口时停止监听",
        group: "MyBatis core",
        kind: Kind::Bool,
        default: DefaultValue::Bool(false),
        needs_restart: false,
    },
    SettingDef {
        key: "log.timestampEnabled",
        title: "时间戳显示",
        group: "MyBatis core",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "log.timestampFormat",
        title: "时间戳格式",
        group: "MyBatis core",
        kind: Kind::String,
        default: DefaultValue::Str("yyyy-MM-dd HH:mm:ss"),
        needs_restart: false,
    },
    SettingDef {
        key: "log.timestampSource",
        title: "时间来源",
        group: "MyBatis core",
        kind: Kind::Enum(&["log", "capture"]),
        default: DefaultValue::Str("log"),
        needs_restart: false,
    },
    SettingDef {
        key: "log.formatRestored",
        title: "日志还原 SQL 时格式化",
        group: "MyBatis core",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "log.filterKeywords",
        title: "过滤包含关键字的 SQL",
        group: "MyBatis core",
        kind: Kind::String,
        default: DefaultValue::Str(""),
        needs_restart: false,
    },
    SettingDef {
        key: "log.dedupeWindowMs",
        title: "去重窗口（毫秒）",
        group: "MyBatis core",
        kind: Kind::Integer,
        default: DefaultValue::Int(60_000),
        needs_restart: false,
    },
    SettingDef {
        key: "log.maxSqlLength",
        title: "最大解析长度",
        group: "MyBatis core",
        kind: Kind::Integer,
        default: DefaultValue::Int(20000),
        needs_restart: false,
    },
    SettingDef {
        key: "logs.pageSize",
        title: "SQL 日志最大条数",
        group: "MyBatis core",
        kind: Kind::Integer,
        default: DefaultValue::Int(200),
        needs_restart: false,
    },
    SettingDef {
        key: "log.timePrefix",
        title: "执行耗时前缀",
        group: "MyBatis core",
        kind: Kind::String,
        default: DefaultValue::Str("Time:"),
        needs_restart: false,
    },
    SettingDef {
        key: "log.slowThresholdMs",
        title: "慢查询阈值（毫秒）",
        group: "MyBatis core",
        kind: Kind::Integer,
        default: DefaultValue::Int(1000),
        needs_restart: false,
    },
    SettingDef {
        key: "log.showDuration",
        title: "显示执行耗时",
        group: "MyBatis core",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "log.showMethodName",
        title: "来源位置显示类名#方法名",
        group: "MyBatis core",
        kind: Kind::Bool,
        default: DefaultValue::Bool(false),
        needs_restart: false,
    },
    SettingDef {
        key: "log.dataSourceBadgeDetail",
        title: "数据源角标显示完整上下文（库名@主机:端口）",
        group: "MyBatis core",
        kind: Kind::Bool,
        default: DefaultValue::Bool(false),
        needs_restart: false,
    },
    SettingDef {
        key: "log.dedupe",
        title: "移除重复的 SQL",
        group: "MyBatis core",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "log.nPlusOneThreshold",
        title: "N+1 阈值",
        group: "MyBatis core",
        kind: Kind::Integer,
        default: DefaultValue::Int(5),
        needs_restart: false,
    },
    SettingDef {
        key: "log.ignoreParamsInDedupe",
        title: "去重时忽略参数值",
        group: "MyBatis core",
        kind: Kind::Bool,
        default: DefaultValue::Bool(false),
        needs_restart: false,
    },
    SettingDef {
        key: "log.refreshIntervalMs",
        title: "刷新间隔（毫秒）",
        group: "MyBatis core",
        kind: Kind::Integer,
        default: DefaultValue::Int(250),
        needs_restart: false,
    },
    SettingDef {
        key: "log.timestampOffsetMinutes",
        title: "时间戳时区偏移（分钟，东为正）",
        group: "MyBatis core",
        kind: Kind::Integer,
        default: DefaultValue::Int(0),
        needs_restart: false,
    },
    SettingDef {
        key: "log.showRawLine",
        title: "无法完整解析时显示原始行",
        group: "MyBatis core",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "capture.port",
        title: "Java agent 捕获端口",
        group: "MyBatis core",
        kind: Kind::Integer,
        default: DefaultValue::Int(17890),
        needs_restart: false,
    },
    SettingDef {
        key: "logs.displayMode",
        title: "SQL 显示模式",
        group: "List display",
        kind: Kind::Enum(&["single", "keyword_wrap", "formatted"]),
        default: DefaultValue::Str("keyword_wrap"),
        needs_restart: false,
    },
    SettingDef {
        key: "logs.crud.style",
        title: "CRUD 颜色标记形态",
        group: "CRUD colors",
        kind: Kind::Enum(&["bar", "background", "off"]),
        default: DefaultValue::Str("bar"),
        needs_restart: false,
    },
    SettingDef {
        key: "logs.crud.select",
        title: "SELECT 颜色（#RRGGBB）",
        group: "CRUD colors",
        kind: Kind::String,
        default: DefaultValue::Str("#4A86E8"),
        needs_restart: false,
    },
    SettingDef {
        key: "logs.crud.insert",
        title: "INSERT 颜色（#RRGGBB）",
        group: "CRUD colors",
        kind: Kind::String,
        default: DefaultValue::Str("#4CAF50"),
        needs_restart: false,
    },
    SettingDef {
        key: "logs.crud.update",
        title: "UPDATE 颜色（#RRGGBB）",
        group: "CRUD colors",
        kind: Kind::String,
        default: DefaultValue::Str("#E67E22"),
        needs_restart: false,
    },
    SettingDef {
        key: "logs.crud.delete",
        title: "DELETE 颜色（#RRGGBB）",
        group: "CRUD colors",
        kind: Kind::String,
        default: DefaultValue::Str("#D32F2F"),
        needs_restart: false,
    },
    SettingDef {
        key: "log.hibernateEnabled",
        title: "捕获 Hibernate / JPA 日志",
        group: "ORM capture",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "log.hibernateSqlPrefix",
        title: "Hibernate SQL 行前缀",
        group: "ORM capture",
        kind: Kind::String,
        default: DefaultValue::Str("Hibernate:"),
        needs_restart: false,
    },
    SettingDef {
        key: "log.hibernateParamPrefix",
        title: "Hibernate 参数行前缀",
        group: "ORM capture",
        kind: Kind::String,
        default: DefaultValue::Str("binding parameter"),
        needs_restart: false,
    },
    SettingDef {
        key: "log.jdbcEnabled",
        title: "捕获 Spring JdbcTemplate 日志",
        group: "ORM capture",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "log.shardingSphereEnabled",
        title: "捕获 ShardingSphere 双段日志",
        group: "ORM capture",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "log.autoSaveEnabled",
        title: "自动保存历史 SQL",
        group: "Auto save",
        kind: Kind::Bool,
        default: DefaultValue::Bool(false),
        needs_restart: false,
    },
    SettingDef {
        key: "log.autoSavePath",
        title: "保存目录",
        group: "Auto save",
        kind: Kind::String,
        default: DefaultValue::Str(""),
        needs_restart: false,
    },
    SettingDef {
        key: "log.autoSaveFileNamePattern",
        title: "文件名模板",
        group: "Auto save",
        kind: Kind::String,
        default: DefaultValue::Str("sql-history-$yyyy-MM-dd$.sql"),
        needs_restart: false,
    },
    SettingDef {
        key: "log.autoSaveMaxFiles",
        title: "历史文件保留上限",
        group: "Auto save",
        kind: Kind::Integer,
        default: DefaultValue::Int(30),
        needs_restart: false,
    },
    SettingDef {
        key: "log.stackMaxFrames",
        title: "调用栈快照帧数",
        group: "Source capture",
        kind: Kind::Integer,
        default: DefaultValue::Int(24),
        needs_restart: false,
    },
    SettingDef {
        key: "log.sourceFrameSkipPrefixes",
        title: "源码捕获帧过滤前缀",
        group: "Source capture",
        kind: Kind::String,
        default: DefaultValue::Str(""),
        needs_restart: false,
    },
    SettingDef {
        key: "log.sourceFrameBizJarPrefixes",
        title: "JAR 业务帧包名前缀",
        group: "Source capture",
        kind: Kind::String,
        default: DefaultValue::Str(""),
        needs_restart: false,
    },
    SettingDef {
        key: "log.desensitize",
        title: "采集时脱敏",
        group: "Source capture",
        kind: Kind::Bool,
        default: DefaultValue::Bool(false),
        needs_restart: false,
    },
    // Rendering -----------------------------------------------------------
    SettingDef {
        key: "render.paramsSeparator",
        title: "参数分隔符",
        group: "Rendering",
        kind: Kind::String,
        default: DefaultValue::Str(", "),
        needs_restart: false,
    },
    SettingDef {
        key: "render.sqlCase",
        title: "渲染 SQL 大小写",
        group: "Rendering",
        kind: Kind::Enum(&["preserve", "upper", "lower"]),
        default: DefaultValue::Str("preserve"),
        needs_restart: false,
    },
    SettingDef {
        key: "render.dateFormat",
        title: "日期格式",
        group: "Rendering",
        kind: Kind::String,
        default: DefaultValue::Str("yyyy-MM-dd HH:mm:ss"),
        needs_restart: false,
    },
    SettingDef {
        key: "render.showConfidence",
        title: "显示置信度",
        group: "Rendering",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "render.formatSql",
        title: "格式化输出",
        group: "Preview",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "render.formatOnPreview",
        title: "预览时格式化还原 SQL",
        group: "Rendering",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "render.indentWidth",
        title: "缩进宽度",
        group: "Rendering",
        kind: Kind::Integer,
        default: DefaultValue::Int(2),
        needs_restart: false,
    },
    SettingDef {
        key: "render.truncateSql",
        title: "预览截断（字符）",
        group: "Rendering",
        kind: Kind::Integer,
        default: DefaultValue::Int(180),
        needs_restart: false,
    },
    SettingDef {
        key: "render.exportFormat",
        title: "导出格式",
        group: "Rendering",
        kind: Kind::Enum(&["sql", "json"]),
        default: DefaultValue::Str("sql"),
        needs_restart: false,
    },
    // Navigation and injection -------------------------------------------
    // `nav.showLineMarkers` is the gutter master switch (the host reads it while
    // indexing the document's markers); the six source-group switches below sit
    // under it in the reference and are stored here for parity. This build only
    // produces mapper XML icons, so every group but `ui.gutter.mapperXml` is
    // stored, not wired.
    SettingDef {
        key: "nav.enabled",
        title: "Mapper 与 XML 双向跳转",
        group: "XML support",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: true,
    },
    SettingDef {
        key: "nav.showLineMarkers",
        title: "在代码左侧显示预览 SQL 图标",
        group: "Gutter",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: true,
    },
    SettingDef {
        key: "injections.enabled",
        title: "mapper XML 内嵌 SQL 注入",
        group: "XML support",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: true,
    },
    SettingDef {
        key: "injections.language",
        title: "注入语言 ID",
        group: "XML support",
        kind: Kind::String,
        default: DefaultValue::Str("sql"),
        needs_restart: true,
    },
    SettingDef {
        key: "inlay.enabled",
        title: "内联提示",
        group: "Editor",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: true,
    },
    SettingDef {
        key: "inlay.insertColumns",
        title: "显示 INSERT 目标列",
        group: "Editor",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "diagnostics.enabled",
        title: "诊断",
        group: "Editor",
        kind: Kind::Bool,
        default: DefaultValue::Bool(false),
        needs_restart: true,
    },
    SettingDef {
        key: "diagnostics.unknownColumn",
        title: "报告未知列",
        group: "Editor",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "diagnostics.dialectQuotes",
        title: "报告方言引号不匹配",
        group: "Editor",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    // Gutter source groups, navigation yield, XML support, editor switches,
    // onboarding tips and the interface language (L-SettingsParity, UI and
    // Actions page). The capability behind most of these does not exist in this
    // port yet, so the schema declares them (UI parity with the IntelliJ
    // edition) and the panel description says so; the final report lists them.
    SettingDef {
        key: "ui.gutter.mapperXml",
        title: "mapper XML 语句",
        group: "Gutter",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: true,
    },
    SettingDef {
        key: "ui.gutter.annotation",
        title: "注解 SQL（@Select 族）",
        group: "Gutter",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: true,
    },
    SettingDef {
        key: "ui.gutter.jpaHql",
        title: "JPA / HQL 族",
        group: "Gutter",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: true,
    },
    SettingDef {
        key: "ui.gutter.jdbc",
        title: "JdbcTemplate 字符串 SQL",
        group: "Gutter",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: true,
    },
    SettingDef {
        key: "ui.gutter.inlineFallback",
        title: "通用 XML 内嵌兜底",
        group: "Gutter",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: true,
    },
    SettingDef {
        key: "ui.gutter.wrapperChain",
        title: "Wrapper 链 / 方法名字符串 SQL",
        group: "Gutter",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: true,
    },
    SettingDef {
        key: "ui.mapperNavYield",
        title: "与同类 MyBatis 导航插件共存时让出跳转图标",
        group: "Gutter",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: true,
    },
    SettingDef {
        key: "ui.xmlInlineSql",
        title: "识别通用 XML 内嵌 SQL（叶子标签文本 / CDATA）",
        group: "XML support",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: true,
    },
    SettingDef {
        key: "ui.ibatisXml",
        title: "iBatis 2.x <sqlMap> 语句预览",
        group: "XML support",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: true,
    },
    SettingDef {
        key: "ui.xmlSyntaxHighlight",
        title: "mapper XML 语法高亮（关键字 + 参数占位着色）",
        group: "XML support",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: true,
    },
    SettingDef {
        key: "ui.consolePrepMode",
        title: "预览打开时的控制台准备方式",
        group: "Editor",
        kind: Kind::Enum(&["lazy", "lazy_silent", "eager"]),
        default: DefaultValue::Str("lazy"),
        needs_restart: false,
    },
    SettingDef {
        key: "ui.pagesXmlSourceMode",
        title: "自研框架数据源语言",
        group: "Editor",
        kind: Kind::Enum(&["auto", "sql", "hql"]),
        default: DefaultValue::Str("auto"),
        needs_restart: false,
    },
    SettingDef {
        key: "ui.joinDiagramNative",
        title: "使用 IDE 原生图框架渲染 JOIN 图",
        group: "Editor",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "ui.showWhitespaces",
        title: "在插件自建编辑器中显示空白符",
        group: "Editor",
        kind: Kind::Bool,
        default: DefaultValue::Bool(false),
        needs_restart: false,
    },
    SettingDef {
        key: "ui.onboardingAnchorTip",
        title: "首次检测到可预览 SQL 时显示一次性引导",
        group: "Onboarding",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "ui.dialectTip",
        title: "SQL 方言无法确定时显示一次性提示",
        group: "Onboarding",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "ui.firstCaptureGuide",
        title: "首次捕获 SQL 时显示一次性提示",
        group: "Onboarding",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "ui.language",
        title: "界面语言",
        group: "Language",
        kind: Kind::Enum(&["auto", "zh", "en"]),
        default: DefaultValue::Str("auto"),
        needs_restart: true,
    },
    // Shortcuts / Table Mapping / MCP / About ------------------------------
    // The reference keeps these as four settings pages of its own. This build
    // declares the pages (manifest order 50/45/40/30/20/15/10) and carries the
    // keys the IDE can render; keymaps stay in IDE Keymap, which is why the
    // shortcut row is a read-only reminder rather than a recorder.
    // Shortcuts stay in IDE Keymap: the platform has no key-recording API, so
    // these rows carry the reference's default keys as read-only text.
    SettingDef {
        key: "shortcuts.preview",
        title: "预览 SQL",
        group: "Shortcuts",
        kind: Kind::String,
        default: DefaultValue::Str("Alt+Shift+Q"),
        needs_restart: false,
    },
    SettingDef {
        key: "shortcuts.sqlToJava",
        title: "SQL 转 Java",
        group: "Shortcuts",
        kind: Kind::String,
        default: DefaultValue::Str("Alt+Shift+J"),
        needs_restart: false,
    },
    SettingDef {
        key: "shortcuts.explain",
        title: "EXPLAIN 选中表",
        group: "Shortcuts",
        kind: Kind::String,
        default: DefaultValue::Str(""),
        needs_restart: false,
    },
    SettingDef {
        key: "shortcuts.count",
        title: "COUNT 选中表",
        group: "Shortcuts",
        kind: Kind::String,
        default: DefaultValue::Str(""),
        needs_restart: false,
    },
    SettingDef {
        key: "shortcuts.sample",
        title: "抽样选中表",
        group: "Shortcuts",
        kind: Kind::String,
        default: DefaultValue::Str(""),
        needs_restart: false,
    },
    SettingDef {
        key: "table.prefix",
        title: "表名前缀",
        group: "Table Mapping",
        kind: Kind::String,
        default: DefaultValue::Str(""),
        needs_restart: false,
    },
    // MCP Tools page, L-SettingsParity -------------------------------------
    // The reference pages each of its tools with a checkbox; the platform
    // schema has no checkbox list here, so the disabled set is one comma
    // separated string. Nothing in this port reads the three keys yet.
    SettingDef {
        key: "mcp.enabled",
        title: "启用 MCP 工具",
        group: "MCP",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "mcp.disabledTools",
        title: "禁用的 MCP 工具",
        group: "MCP",
        kind: Kind::String,
        default: DefaultValue::Str(""),
        needs_restart: false,
    },
    SettingDef {
        key: "mcp.mask",
        title: "MCP 输出脱敏",
        group: "MCP",
        kind: Kind::Bool,
        default: DefaultValue::Bool(false),
        needs_restart: false,
    },
    // About page, L-SettingsParity ----------------------------------------
    // The build number and the cache cleanup row are read-only: the platform
    // has no settings-page action, so the second is explanatory text only.
    SettingDef {
        key: "about.updateChannel",
        title: "更新通道",
        group: "About",
        kind: Kind::Enum(&["marketplace", "gitee"]),
        default: DefaultValue::Str("marketplace"),
        needs_restart: false,
    },
    SettingDef {
        key: "about.buildNumber",
        title: "内部构建号",
        group: "About",
        kind: Kind::String,
        default: DefaultValue::Str(""),
        needs_restart: false,
    },
    SettingDef {
        key: "about.cacheAction",
        title: "插件缓存清理",
        group: "About",
        kind: Kind::String,
        default: DefaultValue::Str(""),
        needs_restart: false,
    },
    // Engine --------------------------------------------------------------
    SettingDef {
        key: "engine.downloadUrl",
        title: "引擎下载地址",
        group: "Engine",
        kind: Kind::String,
        default: DefaultValue::Str(""),
        needs_restart: false,
    },
    SettingDef {
        key: "engine.autoDownload",
        title: "首次使用时下载引擎",
        group: "Engine",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "engine.degradePolicy",
        title: "引擎不可用时的处理",
        group: "Engine",
        kind: Kind::Enum(&["text-only", "prompt"]),
        default: DefaultValue::Str("text-only"),
        needs_restart: false,
    },
    SettingDef {
        key: "engine.startupTimeoutMs",
        title: "引擎启动超时（毫秒）",
        group: "Engine",
        kind: Kind::Integer,
        default: DefaultValue::Int(5000),
        needs_restart: false,
    },
    SettingDef {
        key: "engine.requestTimeoutMs",
        title: "引擎请求超时（毫秒）",
        group: "Engine",
        kind: Kind::Integer,
        default: DefaultValue::Int(30000),
        needs_restart: false,
    },
    SettingDef {
        key: "engine.javaPath",
        title: "Java 可执行文件",
        group: "Engine",
        kind: Kind::String,
        default: DefaultValue::Str(""),
        needs_restart: false,
    },
    SettingDef {
        key: "engine.maxRestarts",
        title: "降级前的引擎重启次数",
        group: "Engine",
        kind: Kind::Integer,
        default: DefaultValue::Int(3),
        needs_restart: false,
    },
];

/// All settings in page order.
pub fn definitions() -> &'static [SettingDef] {
    &DEFINITIONS
}

/// The definition of `key`, if it exists.
pub fn definition(key: &str) -> Option<&'static SettingDef> {
    DEFINITIONS.iter().find(|def| def.key == key)
}

/// A boolean setting, or `default` when unset / not a boolean.
pub fn get_bool(key: &str, default: bool) -> bool {
    host::setting_bool(key).unwrap_or(default)
}

/// A string setting, or `default` when unset / not a string.
pub fn get_string(key: &str, default: &str) -> String {
    host::setting_string(key).unwrap_or_else(|| default.to_string())
}

/// An integer setting, or `default` when unset / not a number.
pub fn get_i64(key: &str, default: i64) -> i64 {
    host::setting_number(key)
        .map(|value| value as i64)
        .unwrap_or(default)
}

/// A boolean setting with the value [`DEFINITIONS`] declares as its default.
///
/// The accessors above need the caller to repeat the default, which is how a
/// stored value and the generated settings page can drift apart. These three
/// read the default off the definition, so there is one source of truth.
pub fn bool_setting(key: &str) -> bool {
    let default = match definition(key).map(|def| def.default) {
        Some(DefaultValue::Bool(value)) => value,
        _ => false,
    };
    get_bool(key, default)
}

/// A string setting with the declared default (empty when the key is unknown).
pub fn string_setting(key: &str) -> String {
    let default = match definition(key).map(|def| def.default) {
        Some(DefaultValue::Str(value)) => value,
        _ => "",
    };
    get_string(key, default)
}

/// An integer setting with the declared default (0 when the key is unknown).
pub fn integer_setting(key: &str) -> i64 {
    let default = match definition(key).map(|def| def.default) {
        Some(DefaultValue::Int(value)) => value,
        _ => 0,
    };
    get_i64(key, default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    use serde_json::Value;

    fn schema() -> Value {
        // The settings node is split into pages, one schema file each; the tests
        // assert every code-defined key is declared exactly once across them.
        let mut merged = serde_json::Map::new();
        for text in [
            include_str!("../settings/ui-actions.json"),
            include_str!("../settings/shortcuts.json"),
            include_str!("../settings/sql-preview.json"),
            include_str!("../settings/table-mapping.json"),
            include_str!("../settings/sql-log.json"),
            include_str!("../settings/mcp.json"),
            include_str!("../settings/about.json"),
            include_str!("../settings/engine.json"),
        ] {
            let page: Value = serde_json::from_str(text).expect("a page schema is valid JSON");
            for (key, value) in page["properties"]
                .as_object()
                .expect("a page schema has properties")
            {
                assert!(
                    merged.insert(key.clone(), value.clone()).is_none(),
                    "{key} is declared on two settings pages"
                );
            }
        }
        Value::Object(serde_json::Map::from_iter([(
            "properties".to_string(),
            Value::Object(merged),
        )]))
    }

    #[test]
    fn every_title_is_chinese_and_non_empty() {
        for def in definitions() {
            assert!(!def.title.trim().is_empty(), "empty title on {}", def.key);
            assert!(
                def.title
                    .chars()
                    .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                "title of {} is not Chinese: {}",
                def.key,
                def.title
            );
        }
        for group in GROUPS {
            assert!(!group.trim().is_empty(), "an empty group name");
        }
    }

    #[test]
    fn every_enum_default_is_one_of_its_options() {
        for def in definitions() {
            if let (Kind::Enum(options), DefaultValue::Str(value)) = (def.kind, def.default) {
                assert!(
                    options.contains(&value),
                    "default {value} missing from the enum of {}",
                    def.key
                );
            }
        }
    }

    #[test]
    fn locales_cover_every_title_and_group_with_the_same_key_set() {
        // The host reads `locales/en.json` and `locales/zh-CN.json` as one
        // catalog per language; a key present in one and missing from the other
        // shows the raw key in that language. The settings page renders titles
        // verbatim, so the Chinese title is the catalog key.
        let en: std::collections::BTreeMap<String, String> =
            serde_json::from_str(include_str!("../locales/en.json")).expect("en catalog");
        let zh: std::collections::BTreeMap<String, String> =
            serde_json::from_str(include_str!("../locales/zh-CN.json")).expect("zh catalog");
        assert_eq!(
            en.keys().collect::<Vec<_>>(),
            zh.keys().collect::<Vec<_>>(),
            "the two catalogs must share one key set"
        );
        for def in definitions() {
            for (name, catalog) in [("en", &en), ("zh-CN", &zh)] {
                let text = catalog.get(def.title).unwrap_or_else(|| {
                    panic!("{name} misses the title of {}: {}", def.key, def.title)
                });
                assert!(
                    !text.trim().is_empty(),
                    "{name} value is empty for {}",
                    def.key
                );
            }
        }
        for group in GROUPS {
            for (name, catalog) in [("en", &en), ("zh-CN", &zh)] {
                assert!(catalog.contains_key(*group), "{name} misses group {group}");
            }
        }
    }

    #[test]
    fn accessors_read_the_user_value() {
        let host = MockHost::install();
        host.set_setting("nav.enabled", "true");
        host.set_setting("dialect.default", "\"PostgreSQL\"");
        host.set_setting("log.slowThresholdMs", "2500");
        assert!(get_bool("nav.enabled", false));
        assert_eq!(get_string("dialect.default", "MySQL"), "PostgreSQL");
        assert_eq!(get_i64("log.slowThresholdMs", 1000), 2500);
    }

    #[test]
    fn accessors_fall_back_to_the_default() {
        let _host = MockHost::install();
        assert!(get_bool("nav.enabled", true), "unset keys keep the default");
        assert_eq!(get_string("dialect.default", "MySQL"), "MySQL");
        assert_eq!(get_i64("log.slowThresholdMs", 1000), 1000);
    }

    #[test]
    fn wrong_typed_values_fall_back_instead_of_panicking() {
        let host = MockHost::install();
        host.set_setting("nav.enabled", "\"yes\"");
        host.set_setting("log.slowThresholdMs", "true");
        assert!(get_bool("nav.enabled", true));
        assert_eq!(get_i64("log.slowThresholdMs", 1000), 1000);
    }

    #[test]
    fn definitions_cover_every_field_with_unique_keys() {
        assert_eq!(definitions().len(), DEFINITIONS.len());
        let mut keys = std::collections::BTreeSet::new();
        for def in definitions() {
            assert!(keys.insert(def.key), "duplicate key {}", def.key);
        }
        assert_eq!(
            keys.len(),
            DEFINITIONS.len(),
            "the first release, the L34 parity keys, the L48 preview shell and the C14 interceptor keys share one table"
        );
    }

    #[test]
    fn definitions_use_every_group() {
        let mut groups = std::collections::BTreeSet::new();
        for def in definitions() {
            assert!(
                GROUPS.contains(&def.group),
                "unknown group {} on {}",
                def.group,
                def.key
            );
            groups.insert(def.group);
        }
        assert_eq!(groups.len(), GROUPS.len());
    }

    #[test]
    fn defaults_match_their_declared_kind() {
        for def in definitions() {
            match (def.kind, def.default) {
                (Kind::Bool, DefaultValue::Bool(_)) => {}
                (Kind::Integer, DefaultValue::Int(_)) => {}
                (Kind::String, DefaultValue::Str(_)) => {}
                (Kind::Enum(options), DefaultValue::Str(value)) => {
                    assert!(
                        options.contains(&value),
                        "default {value} missing from the enum of {}",
                        def.key
                    );
                }
                other => panic!("default/kind mismatch on {}: {other:?}", def.key),
            }
        }
    }

    #[test]
    fn navigation_providers_default_on_and_need_a_restart() {
        // Navigation, XML SQL injection and inlay hints ship enabled: the port's
        // whole point is seeing SQL where it is written. Diagnostics stay opt-in
        // (a project without a column source must not get noise).
        for key in ["nav.enabled", "injections.enabled", "inlay.enabled"] {
            let def = definition(key).expect(key);
            assert_eq!(
                def.default,
                DefaultValue::Bool(true),
                "{key} is on by default"
            );
            assert!(
                def.needs_restart,
                "{key} is read once when providers are built"
            );
        }
        let diag = definition("diagnostics.enabled").expect("diagnostics.enabled");
        assert_eq!(diag.default, DefaultValue::Bool(false));
        assert!(diag.needs_restart);
        assert!(
            !definition("dialect.mode")
                .expect("dialect.mode")
                .needs_restart
        );
    }

    #[test]
    fn the_l34_parity_keys_keep_the_reference_defaults() {
        // Lane L34: the three keys the reference exposes and this build can
        // actually consume. Their defaults come from the reference panel, and
        // every one is read on the next push, so no restart is needed.
        let expected = [
            ("logs.displayMode", DefaultValue::Str("keyword_wrap")),
            ("log.showDuration", DefaultValue::Bool(true)),
            ("render.formatOnPreview", DefaultValue::Bool(true)),
        ];
        for (key, default) in expected {
            let def = definition(key).unwrap_or_else(|| panic!("{key} is missing"));
            assert_eq!(def.default, default, "default of {key}");
            assert!(!def.needs_restart, "{key} is read on the next push");
        }
        assert_eq!(
            definition("logs.displayMode")
                .expect("logs.displayMode")
                .kind,
            Kind::Enum(&["single", "keyword_wrap", "formatted"])
        );
    }

    #[test]
    fn the_c14_interceptor_keys_start_off_and_keep_the_reference_defaults() {
        // C14: a simulation the project may not use must never rewrite SQL
        // before the user asks for it, so every switch starts off. The column
        // and value defaults follow the reference (`deleted` / `0`,
        // `tenant_id` / `1`, `Page(1, 10)`).
        let switches = [
            "preview.injectLogicDelete",
            "preview.injectTenantLine",
            "preview.paginationSim",
        ];
        for key in switches {
            let def = definition(key).unwrap_or_else(|| panic!("{key} is missing"));
            assert_eq!(def.default, DefaultValue::Bool(false), "default of {key}");
            assert_eq!(def.group, "MyBatis Plus");
            assert!(!def.needs_restart, "{key} is read on the next render");
        }
        let strings = [
            ("preview.logicDeleteColumn", "deleted"),
            ("preview.logicDeleteValue", "0"),
            ("preview.tenantColumnName", "tenant_id"),
            ("preview.tenantColumnDefault", "1"),
            ("preview.tablePrefix", ""),
        ];
        for (key, default) in strings {
            let def = definition(key).unwrap_or_else(|| panic!("{key} is missing"));
            assert_eq!(def.default, DefaultValue::Str(default), "default of {key}");
        }
        assert_eq!(
            definition("preview.paginationCurrent").map(|def| def.default),
            Some(DefaultValue::Int(1))
        );
        assert_eq!(
            definition("preview.paginationSize").map(|def| def.default),
            Some(DefaultValue::Int(10))
        );
    }

    #[test]
    fn the_setting_helpers_read_the_declared_default() {
        let _host = MockHost::install();
        assert!(!bool_setting("preview.injectLogicDelete"));
        assert_eq!(string_setting("preview.logicDeleteColumn"), "deleted");
        assert_eq!(integer_setting("preview.paginationSize"), 10);
        assert_eq!(string_setting("no.such.key"), "");
        assert_eq!(integer_setting("no.such.key"), 0);
    }

    #[test]
    fn schema_json_matches_the_code_definitions() {
        let schema = schema();
        let properties = schema
            .get("properties")
            .and_then(Value::as_object)
            .expect("the schema has a properties object");
        assert_eq!(properties.len(), definitions().len());

        for def in definitions() {
            let property = properties
                .get(def.key)
                .unwrap_or_else(|| panic!("{} missing from settings.schema.json", def.key));
            assert_eq!(
                property.get("type").and_then(Value::as_str),
                Some(def.kind.json_type()),
                "type of {}",
                def.key
            );
            assert_eq!(
                property.get("title").and_then(Value::as_str),
                Some(def.title),
                "title of {}",
                def.key
            );
            assert_eq!(
                property.get("group").and_then(Value::as_str),
                Some(def.group),
                "group of {}",
                def.key
            );
            match (def.kind, def.default) {
                (_, DefaultValue::Bool(value)) => {
                    assert_eq!(property.get("default"), Some(&Value::Bool(value)))
                }
                (_, DefaultValue::Int(value)) => {
                    assert_eq!(property.get("default"), Some(&Value::from(value)))
                }
                (Kind::Enum(options), DefaultValue::Str(value)) => {
                    assert_eq!(property.get("default"), Some(&Value::from(value)));
                    let listed: Vec<&str> = property
                        .get("enum")
                        .and_then(Value::as_array)
                        .expect("enum properties list their options")
                        .iter()
                        .filter_map(Value::as_str)
                        .collect();
                    assert_eq!(listed, options, "enum options of {}", def.key);
                }
                (_, DefaultValue::Str(value)) => {
                    assert_eq!(property.get("default"), Some(&Value::from(value)))
                }
            }
        }
    }

    #[test]
    fn schema_has_no_extra_properties() {
        let schema = schema();
        let properties = schema
            .get("properties")
            .and_then(Value::as_object)
            .expect("properties");
        for key in properties.keys() {
            assert!(definition(key).is_some(), "{key} is not in DEFINITIONS");
        }
    }
}
