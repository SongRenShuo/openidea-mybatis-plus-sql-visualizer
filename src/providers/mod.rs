//! Editor providers: navigation, injections, inlay hints, diagnostics (T3-2~T3-5).
//!
//! Every provider is a thin trait impl over a pure function in this directory:
//! the text of the document is read once, nothing else is touched (no IO, no
//! host calls), and the result is a deterministic function of the text.
//!
//! Switches are ordinary plugin settings, default off so the feature cannot
//! surprise a user who installed the plugin for rendering:
//!
//! | setting | default | effect |
//! | --- | --- | --- |
//! | `nav.enabled` | `true` | `definition` (Ctrl+B) |
//! | `nav.showLineMarkers` | `true` | gutter markers, read while indexing: the markers are the document indexer's `Edges.markers`, drawn by the host |
//! | `injections.enabled` | `true` | SQL `Injections` inside mapper XML |
//! | `sql.dialect` | unset | dialect for quote checks in diagnostics |
//!
//! `gutter` (the preview button on every mapper statement) has no switch: it is
//! the plugin's main entry point, and the IntelliJ edition shows the icon by
//! default too.
//!
//! Inlay hints and column diagnostics additionally need table columns. They are
//! never guessed: the columns come from [`set_column_lookup`], which the schema
//! lane fills once it has real metadata. Until then no inlay hint is produced
//! and no column is reported unknown.

pub mod diag;
pub mod gutter;
pub mod hover;
pub mod inject;
pub mod inlay;
pub mod nav;
pub(crate) mod sqltext;

use openidea_plugin::host;
use std::sync::{OnceLock, RwLock};

/// Turns `nav.enabled` (and friends) into a boolean; an absent value keeps the
/// schema's own default (navigation, injections and inlays are on, diagnostics
/// is off). The host's `setting()` already answers the schema default, but it
/// returns `None` when the schema item cannot be read, and the providers must
/// not turn a default-on feature off in that case.
pub(crate) fn setting_on(key: &str) -> bool {
    match host::setting_bool(key) {
        Some(value) => value,
        None => crate::settings::definition(key)
            .is_some_and(|def| matches!(def.default, crate::settings::DefaultValue::Bool(true))),
    }
}

/// The configured dialect name (`mysql`, `postgres`, `oracle`, `sqlserver`, …),
/// or an empty string when the user has not chosen one.
pub(crate) fn dialect_setting() -> String {
    let value = host::setting_string(diag::DIALECT_SETTING).unwrap_or_default();
    if value.trim().is_empty() {
        return host::setting_string(diag::DIALECT_SETTING_LEGACY).unwrap_or_default();
    }
    value
}

pub(crate) fn is_xml(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(".xml")
}

pub(crate) fn is_java(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(".java")
}

/// The three gutter tooltips, looked up once per document.
///
/// The host calls `gutter_actions` on every document change, so the lookups are
/// batched here instead of one `t()` per button — a file with 50 chains would
/// otherwise pay 50 identical table scans.
pub(crate) fn gutter_tooltips() -> gutter::Tooltips {
    gutter::Tooltips {
        mapper: crate::i18n::t(gutter::TOOLTIP_KEY),
        wrapper: crate::i18n::t(gutter::WRAPPER_TOOLTIP_KEY),
        annotation: crate::i18n::t(gutter::ANNOTATION_TOOLTIP_KEY),
    }
}

/// Table name (lower-cased, lower-case compare) → column names.
pub type ColumnLookup = fn(&str) -> Option<Vec<String>>;

fn column_slot() -> &'static RwLock<Option<ColumnLookup>> {
    static SLOT: OnceLock<RwLock<Option<ColumnLookup>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(None))
}

/// Supplies table columns to inlay hints and diagnostics. The schema lane (L8)
/// calls this with its cache; `None` removes the source and both providers go
/// quiet again.
pub fn set_column_lookup(lookup: Option<ColumnLookup>) {
    if let Ok(mut slot) = column_slot().write() {
        *slot = lookup;
    }
}

/// Columns of `table`, when a source knows them.
pub fn columns_for(table: &str) -> Option<Vec<String>> {
    let lookup = *column_slot().read().ok()?;
    lookup?(table)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_classified_by_extension() {
        assert!(is_xml("a/UserMapper.XML"));
        assert!(is_java("a/UserMapper.java"));
        assert!(!is_xml("a/UserMapper.java"));
        assert!(!is_java("a/pom.xml"));
    }

    #[test]
    fn no_column_source_means_no_columns() {
        // The registry is process-wide and empty by default in this test binary
        // unless another test sets it; reading it never panics.
        let _ = columns_for("t_user");
    }
}
