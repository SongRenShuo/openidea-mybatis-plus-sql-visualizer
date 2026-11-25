//! Gutter preview buttons for mapper XML (`gutter-actions`).
//!
//! The IntelliJ edition draws a preview icon next to every `<select|insert|
//! update|delete>` of a mapper and opens `SqlPreviewDialog` on click. OpenIDEA
//! has no plugin modal dialog, so the button runs `preview-sql` instead, which
//! lands on the preview tab of the `sqlv-logs` tool window — the same render,
//! a different shell.
//!
//! The provider is a text scan, nothing else: the host hands over the current
//! document (with unsaved edits) and this module reads it once, in document
//! order. No file is opened, no index is built, no setting is consulted, so the
//! call stays inside the editor-provider budget even on a large mapper.
//!
//! `gutter-actions` has to be declared for `xml` in `plugin.toml` (see
//! `docs/registration/L35a.md`); without the declaration the host never calls
//! this function.

use openidea_plugin::language_features::{Document, GutterAction, LanguageFeatures};

/// The command a button runs (matches `[[contributes.commands]] id`).
pub const COMMAND: &str = "preview-sql";

/// The package icon the button draws. It is declared under
/// `[[contributes.markerIcons]]` so the host validates and ships it; gutter
/// icons accept declared marker SVG paths.
pub const ICON: &str = "icons/sqlGutter.svg";

/// The i18n key of the tooltip.
pub const TOOLTIP_KEY: &str = "preview.gutter.tooltip";

impl LanguageFeatures for crate::Main {
    fn gutter_actions(&mut self, doc: &Document) -> Vec<GutterAction> {
        // Cheap gates first: a non-mapper XML must not even cost the locale
        // lookup the tooltip needs.
        if !super::is_xml(&doc.path) || !doc.text.contains("<mapper") {
            return Vec::new();
        }
        actions_for(&doc.path, &doc.text, &crate::i18n::t(TOOLTIP_KEY))
    }
}

/// One action per mapper statement, on the line of its start tag.
///
/// `path` becomes the command argument's prefix (`<path>#<id>`); `tooltip` is
/// already localised. A document without a `<mapper` root produces nothing, so
/// a random `.xml` file costs one substring search.
pub fn actions_for(path: &str, text: &str, tooltip: &str) -> Vec<GutterAction> {
    if !text.contains("<mapper") {
        return Vec::new();
    }
    let elements = crate::indexer::scan::elements(text);
    let mut positions = crate::indexer::scan::Positions::new(text);
    let mut out = Vec::new();
    for element in &elements {
        if !crate::core::xml::STATEMENT_TAGS.contains(&element.name) {
            continue;
        }
        let Some(attr) = element.attr("id") else {
            continue;
        };
        let id = attr.value.trim();
        if id.is_empty() {
            continue;
        }
        // `element.start` is the `<` of the start tag; `Positions` walks the
        // text forward, and elements arrive in document order.
        let line = positions.at(element.start).line;
        out.push(GutterAction {
            line,
            icon: Some(ICON.to_string()),
            tooltip: tooltip.to_string(),
            command: COMMAND.to_string(),
            argument: Some(format!("{path}#{id}")),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAPPER: &str = r#"<mapper namespace="com.example.UserMapper">
    <select id="findById">SELECT * FROM users WHERE id = #{id}</select>
    <update id="touch">UPDATE users SET name = #{name}</update>
</mapper>"#;

    fn doc(path: &str, text: &str) -> Document {
        Document {
            path: path.to_string(),
            language: "xml".to_string(),
            version: 1,
            text: text.to_string(),
        }
    }

    #[test]
    fn providers_gutter_mapper_statements_get_one_action_each() {
        let actions = actions_for("/project/UserMapper.xml", MAPPER, "Preview SQL");
        assert_eq!(actions.len(), 2, "{actions:?}");
        assert_eq!(actions[0].line, 1, "the <select> start tag's line");
        assert_eq!(actions[0].command, COMMAND);
        assert_eq!(
            actions[0].argument.as_deref(),
            Some("/project/UserMapper.xml#findById")
        );
        assert_eq!(actions[0].icon.as_deref(), Some(ICON));
        assert_eq!(actions[0].tooltip, "Preview SQL");
        assert_eq!(actions[1].line, 2);
        assert_eq!(
            actions[1].argument.as_deref(),
            Some("/project/UserMapper.xml#touch")
        );
    }

    #[test]
    fn providers_gutter_ignores_non_mapper_xml() {
        let actions = actions_for(
            "/project/pom.xml",
            "<project><build><plugins/></build></project>",
            "Preview SQL",
        );
        assert!(actions.is_empty());
    }

    #[test]
    fn providers_gutter_ignores_non_xml_documents() {
        // `t()` reads the host locale, which needs an installed mock host.
        let _host = openidea_plugin::mock::MockHost::install();
        let mut main = crate::Main;
        assert!(
            main.gutter_actions(&doc("/project/UserMapper.java", MAPPER))
                .is_empty()
        );
        assert!(
            main.gutter_actions(&doc("/project/UserMapper.xml", MAPPER))
                .len()
                == 2
        );
    }

    #[test]
    fn providers_gutter_skips_statements_without_an_id() {
        let actions = actions_for(
            "/project/a.xml",
            r#"<mapper namespace="a"><select>SELECT 1</select><select id="ok">SELECT 2</select></mapper>"#,
            "Preview SQL",
        );
        assert_eq!(actions.len(), 1, "{actions:?}");
        assert_eq!(actions[0].argument.as_deref(), Some("/project/a.xml#ok"));
    }

    #[test]
    fn providers_gutter_tooltip_is_localised() {
        let tooltip = crate::i18n::t_in("zh-CN", TOOLTIP_KEY);
        assert_eq!(tooltip, "预览 SQL");
        let actions = actions_for("/project/UserMapper.xml", MAPPER, &tooltip);
        assert_eq!(actions[0].tooltip, "预览 SQL");
    }

    #[test]
    fn providers_gutter_handles_a_large_document_without_panicking() {
        let mut xml = String::from("<mapper namespace=\"a\">\n");
        for index in 0..2000 {
            xml.push_str(&format!(
                "<select id=\"s{index}\">SELECT {index}</select>\n"
            ));
        }
        xml.push_str("</mapper>");
        let actions = actions_for("/project/Big.xml", &xml, "Preview SQL");
        assert_eq!(actions.len(), 2000);
        assert_eq!(actions[0].line, 1);
        assert_eq!(actions[1999].line, 2000);
        assert_eq!(
            actions[1999].argument.as_deref(),
            Some("/project/Big.xml#s1999")
        );
    }
}
