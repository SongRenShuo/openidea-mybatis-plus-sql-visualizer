//! Settings schema and accessors (T2-3).
//!
//! The manifest declares `[contributes.settings] title` plus four pages
//! (`settings/ui-actions.json`, `settings/sql-preview.json`,
//! `settings/sql-log.json`, `settings/engine.json`); the IDE generates one page
//! per file under the plugin's Settings node. [`DEFINITIONS`] mirrors them in
//! code so callers can enumerate keys, defaults and which changes need reopening
//! the IDE. The tests keep the two in sync.
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
pub const GROUPS: &[&str] = &["Dialect", "Log", "Rendering", "Navigation", "Engine"];

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

/// The 45 settings of the first release plus the L34 parity keys, in page order.
pub static DEFINITIONS: [SettingDef; 45] = [
    // Dialect -------------------------------------------------------------
    SettingDef {
        key: "dialect.mode",
        title: "Dialect mode",
        group: "Dialect",
        kind: Kind::Enum(&["auto", "manual"]),
        default: DefaultValue::Str("auto"),
        needs_restart: false,
    },
    SettingDef {
        key: "dialect.default",
        title: "Default dialect",
        group: "Dialect",
        kind: Kind::Enum(DIALECTS),
        default: DefaultValue::Str("MySQL"),
        needs_restart: false,
    },
    SettingDef {
        key: "dialect.detectFromUrl",
        title: "Detect dialect from the JDBC URL",
        group: "Dialect",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "dialect.autoDetectFromMappers",
        title: "Detect dialect from mapper statements",
        group: "Dialect",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "dialect.quoteIdentifiers",
        title: "Quote identifiers",
        group: "Dialect",
        kind: Kind::Enum(&["auto", "always", "never"]),
        default: DefaultValue::Str("auto"),
        needs_restart: false,
    },
    SettingDef {
        key: "dialect.keywordCase",
        title: "Keyword case",
        group: "Dialect",
        kind: Kind::Enum(&["auto", "upper", "lower"]),
        default: DefaultValue::Str("auto"),
        needs_restart: false,
    },
    // Log -----------------------------------------------------------------
    SettingDef {
        key: "log.prefix",
        title: "Log prefix",
        group: "Log",
        kind: Kind::String,
        default: DefaultValue::Str(""),
        needs_restart: false,
    },
    SettingDef {
        key: "log.slowThresholdMs",
        title: "Slow statement threshold (ms)",
        group: "Log",
        kind: Kind::Integer,
        default: DefaultValue::Int(1000),
        needs_restart: false,
    },
    SettingDef {
        key: "log.nPlusOneThreshold",
        title: "N+1 threshold",
        group: "Log",
        kind: Kind::Integer,
        default: DefaultValue::Int(5),
        needs_restart: false,
    },
    SettingDef {
        key: "log.dedupe",
        title: "Fold duplicate statements",
        group: "Log",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "log.dedupeWindowMs",
        title: "Duplicate window (ms)",
        group: "Log",
        kind: Kind::Integer,
        default: DefaultValue::Int(60_000),
        needs_restart: false,
    },
    SettingDef {
        key: "log.ignoreParamsInDedupe",
        title: "Ignore parameter values when deduplicating",
        group: "Log",
        kind: Kind::Bool,
        default: DefaultValue::Bool(false),
        needs_restart: false,
    },
    SettingDef {
        key: "log.maxSqlLength",
        title: "Maximum SQL length",
        group: "Log",
        kind: Kind::Integer,
        default: DefaultValue::Int(20000),
        needs_restart: false,
    },
    SettingDef {
        key: "log.refreshIntervalMs",
        title: "Refresh interval (ms)",
        group: "Log",
        kind: Kind::Integer,
        default: DefaultValue::Int(250),
        needs_restart: false,
    },
    SettingDef {
        key: "log.timestampOffsetMinutes",
        title: "Timestamp offset (minutes east of UTC)",
        group: "Log",
        kind: Kind::Integer,
        default: DefaultValue::Int(0),
        needs_restart: false,
    },
    SettingDef {
        key: "log.showRawLine",
        title: "Show raw line for partial statements",
        group: "Log",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "log.showDuration",
        title: "Show execution duration",
        group: "Log",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "logs.displayMode",
        title: "SQL display mode",
        group: "Log",
        kind: Kind::Enum(&["single", "multi", "keyword_wrap"]),
        default: DefaultValue::Str("keyword_wrap"),
        needs_restart: false,
    },
    SettingDef {
        key: "logs.pageSize",
        title: "Maximum SQL log entries",
        group: "Log",
        kind: Kind::Integer,
        default: DefaultValue::Int(200),
        needs_restart: false,
    },
    SettingDef {
        key: "capture.port",
        title: "Java agent capture WebSocket port",
        group: "Log",
        kind: Kind::Integer,
        default: DefaultValue::Int(17890),
        needs_restart: false,
    },
    // Rendering -----------------------------------------------------------
    SettingDef {
        key: "render.paramsSeparator",
        title: "Parameter separator",
        group: "Rendering",
        kind: Kind::String,
        default: DefaultValue::Str(", "),
        needs_restart: false,
    },
    SettingDef {
        key: "render.sqlCase",
        title: "Rendered SQL case",
        group: "Rendering",
        kind: Kind::Enum(&["preserve", "upper", "lower"]),
        default: DefaultValue::Str("preserve"),
        needs_restart: false,
    },
    SettingDef {
        key: "render.dateFormat",
        title: "Date format",
        group: "Rendering",
        kind: Kind::String,
        default: DefaultValue::Str("yyyy-MM-dd HH:mm:ss"),
        needs_restart: false,
    },
    SettingDef {
        key: "render.showConfidence",
        title: "Show confidence",
        group: "Rendering",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "render.formatSql",
        title: "Format SQL",
        group: "Rendering",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "render.formatOnPreview",
        title: "Format restored SQL on preview",
        group: "Rendering",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "render.indentWidth",
        title: "Indent width",
        group: "Rendering",
        kind: Kind::Integer,
        default: DefaultValue::Int(2),
        needs_restart: false,
    },
    SettingDef {
        key: "render.truncateSql",
        title: "Preview truncation (characters)",
        group: "Rendering",
        kind: Kind::Integer,
        default: DefaultValue::Int(180),
        needs_restart: false,
    },
    SettingDef {
        key: "render.exportFormat",
        title: "Export format",
        group: "Rendering",
        kind: Kind::Enum(&["sql", "json"]),
        default: DefaultValue::Str("sql"),
        needs_restart: false,
    },
    // Navigation and injection -------------------------------------------
    SettingDef {
        key: "nav.enabled",
        title: "Mapper and XML navigation",
        group: "Navigation",
        kind: Kind::Bool,
        default: DefaultValue::Bool(false),
        needs_restart: true,
    },
    SettingDef {
        key: "nav.showLineMarkers",
        title: "Gutter line markers",
        group: "Navigation",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: true,
    },
    SettingDef {
        key: "injections.enabled",
        title: "SQL injection in mapper XML",
        group: "Navigation",
        kind: Kind::Bool,
        default: DefaultValue::Bool(false),
        needs_restart: true,
    },
    SettingDef {
        key: "injections.language",
        title: "Injection language id",
        group: "Navigation",
        kind: Kind::String,
        default: DefaultValue::Str("sql"),
        needs_restart: true,
    },
    SettingDef {
        key: "inlay.enabled",
        title: "Inlay hints",
        group: "Navigation",
        kind: Kind::Bool,
        default: DefaultValue::Bool(false),
        needs_restart: true,
    },
    SettingDef {
        key: "inlay.insertColumns",
        title: "Show INSERT target columns",
        group: "Navigation",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "diagnostics.enabled",
        title: "Diagnostics",
        group: "Navigation",
        kind: Kind::Bool,
        default: DefaultValue::Bool(false),
        needs_restart: true,
    },
    SettingDef {
        key: "diagnostics.unknownColumn",
        title: "Report unknown columns",
        group: "Navigation",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "diagnostics.dialectQuotes",
        title: "Report dialect quote mismatches",
        group: "Navigation",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    // Engine --------------------------------------------------------------
    SettingDef {
        key: "engine.downloadUrl",
        title: "Engine download URL",
        group: "Engine",
        kind: Kind::String,
        default: DefaultValue::Str(""),
        needs_restart: false,
    },
    SettingDef {
        key: "engine.autoDownload",
        title: "Download the engine on first use",
        group: "Engine",
        kind: Kind::Bool,
        default: DefaultValue::Bool(true),
        needs_restart: false,
    },
    SettingDef {
        key: "engine.degradePolicy",
        title: "When the engine is unavailable",
        group: "Engine",
        kind: Kind::Enum(&["text-only", "prompt"]),
        default: DefaultValue::Str("text-only"),
        needs_restart: false,
    },
    SettingDef {
        key: "engine.startupTimeoutMs",
        title: "Engine startup timeout (ms)",
        group: "Engine",
        kind: Kind::Integer,
        default: DefaultValue::Int(5000),
        needs_restart: false,
    },
    SettingDef {
        key: "engine.requestTimeoutMs",
        title: "Engine request timeout (ms)",
        group: "Engine",
        kind: Kind::Integer,
        default: DefaultValue::Int(30000),
        needs_restart: false,
    },
    SettingDef {
        key: "engine.javaPath",
        title: "Java executable",
        group: "Engine",
        kind: Kind::String,
        default: DefaultValue::Str(""),
        needs_restart: false,
    },
    SettingDef {
        key: "engine.maxRestarts",
        title: "Engine restarts before degrading",
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
            include_str!("../settings/sql-preview.json"),
            include_str!("../settings/sql-log.json"),
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
    fn definitions_cover_forty_five_fields_with_unique_keys() {
        assert_eq!(definitions().len(), 45);
        let mut keys = std::collections::BTreeSet::new();
        for def in definitions() {
            assert!(keys.insert(def.key), "duplicate key {}", def.key);
        }
        assert_eq!(
            keys.len(),
            45,
            "the first release plus the L34 parity keys ships 45 settings"
        );
    }

    #[test]
    fn definitions_use_the_five_groups() {
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
    fn navigation_providers_default_to_off_and_need_a_restart() {
        for key in [
            "nav.enabled",
            "injections.enabled",
            "inlay.enabled",
            "diagnostics.enabled",
        ] {
            let def = definition(key).expect(key);
            assert_eq!(
                def.default,
                DefaultValue::Bool(false),
                "{key} is off by default"
            );
            assert!(
                def.needs_restart,
                "{key} is read once when providers are built"
            );
        }
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
            Kind::Enum(&["single", "multi", "keyword_wrap"])
        );
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
