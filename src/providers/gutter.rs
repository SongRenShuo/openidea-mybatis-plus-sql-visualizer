//! Gutter preview buttons (`gutter-actions`).
//!
//! The IntelliJ edition draws a preview icon next to every `<select|insert|
//! update|delete>` of a mapper and next to every wrapper chain (its
//! `SqlGutterLineMarkerProvider` + `WrapperAnchorDetector`) and opens
//! `SqlPreviewDialog` on click. OpenIDEA has no plugin modal dialog, so a button
//! runs `preview-sql` instead, which lands on the preview tab of the `sqlv-logs`
//! tool window — the same render, a different shell.
//!
//! The provider is a text scan, nothing else: the host hands over the current
//! document (with unsaved edits) and this module reads it once, in document
//! order. No file is opened, no index is built, so the call stays inside the
//! editor-provider budget even on a large file.
//!
//! Two languages, two anchor rules — the reference keeps them in two providers
//! as well:
//!
//! | document | anchor | argument |
//! | --- | --- | --- |
//! | mapper XML | every statement's start tag | `<path>#<id>` |
//! | Java | `@Select` family annotation, and every wrapper chain root | `<path>@<offset>` |
//!
//! The Java argument carries a **byte offset**, not a line: the command resolves
//! the statement whose span contains it, which is the same rule the caret path
//! uses. A line would have to be re-read as UTF-16 columns and would break the
//! moment the file changed under an open editor.
//!
//! `gutter-actions` has to be declared for both languages in `plugin.toml` (see
//! `docs/registration/L35a.md`); without the declaration the host never calls
//! this function.

use openidea_plugin::language_features::{Document, GutterAction, LanguageFeatures};

/// The command a button runs (matches `[[contributes.commands]] id`).
pub const COMMAND: &str = "preview-sql";

/// The package icon the button draws. It is declared under
/// `[[contributes.markerIcons]]` so the host validates and ships it; gutter
/// icons accept declared marker SVG paths.
pub const ICON: &str = "icons/sqlGutter.svg";

/// The i18n key of the tooltip for a mapper statement.
pub const TOOLTIP_KEY: &str = "preview.gutter.tooltip";

/// The i18n key of the tooltip for a wrapper chain (the reference's
/// `gutter.tooltip`, which says "…of this Wrapper").
pub const WRAPPER_TOOLTIP_KEY: &str = "preview.gutter.wrapper.tooltip";

/// The i18n key of the tooltip for an annotated statement.
pub const ANNOTATION_TOOLTIP_KEY: &str = "preview.gutter.annotation.tooltip";

/// The group switch each source answers to (UI and Actions page).
pub const WRAPPER_SETTING: &str = "ui.gutter.wrapperChain";
/// The annotation family's switch.
pub const ANNOTATION_SETTING: &str = "ui.gutter.annotation";
/// The mapper XML statements' switch.
pub const MAPPER_XML_SETTING: &str = "ui.gutter.mapperXml";

impl LanguageFeatures for crate::Main {
    fn gutter_actions(&mut self, doc: &Document) -> Vec<GutterAction> {
        if doc.language == "java" || super::is_java(&doc.path) {
            return java_actions(&doc.path, &doc.text, &super::gutter_tooltips());
        }
        // Cheap gates first: a non-mapper XML must not even cost the locale
        // lookup the tooltip needs.
        if !super::is_xml(&doc.path) || !doc.text.contains("<mapper") {
            return Vec::new();
        }
        xml_actions(&doc.path, &doc.text, &super::gutter_tooltips())
    }
}

/// The three tooltips a document may need, in one borrow.
pub struct Tooltips {
    pub mapper: String,
    pub wrapper: String,
    pub annotation: String,
}

/// Every preview button for a Java document, in document order.
///
/// Two sources feed it: annotated statements (`@Select` family) and wrapper
/// chain roots. A chain inside an annotated method would otherwise draw two
/// icons on adjacent lines for one statement; the reference suppresses the
/// chain icon there (its `WrapperAnchorDetector` yields to the annotation), and
/// so does this function.
pub(crate) fn java_actions(path: &str, text: &str, tooltips: &Tooltips) -> Vec<GutterAction> {
    let mut positions = crate::indexer::scan::Positions::new(text);
    let annotations_on = super::setting_on(ANNOTATION_SETTING);
    let wrappers_on = super::setting_on(WRAPPER_SETTING);

    let statements = crate::analyze::sources::annotation_statements(text);
    let annotated: Vec<(usize, usize)> = statements
        .iter()
        .map(|statement| {
            (
                statement.annotation_start,
                crate::analyze::sources::annotation_end(text, statement),
            )
        })
        .collect();

    let mut out: Vec<(usize, GutterAction)> = Vec::new();
    if annotations_on {
        for statement in &statements {
            let argument = format!(
                "{path}@{}",
                // The annotation's own `@` is the anchor: it is the line the
                // reference draws the icon on, and the statement span starts
                // exactly there.
                statement.annotation_start
            );
            let line = positions.at(statement.annotation_start).line;
            out.push((
                statement.annotation_start,
                GutterAction {
                    line,
                    icon: Some(ICON.to_string()),
                    tooltip: tooltips.annotation.clone(),
                    command: COMMAND.to_string(),
                    argument: Some(argument),
                },
            ));
        }
    }
    if wrappers_on {
        for offset in crate::analyze::sources::wrapper_chain_starts(text) {
            // A chain root inside an annotated method is that method's SQL: the
            // annotation icon already opens it.
            if annotated
                .iter()
                .any(|(start, end)| offset >= *start && offset < *end)
            {
                continue;
            }
            let line = positions.at(offset).line;
            out.push((
                offset,
                GutterAction {
                    line,
                    icon: Some(ICON.to_string()),
                    tooltip: tooltips.wrapper.clone(),
                    command: COMMAND.to_string(),
                    argument: Some(format!("{path}@{offset}")),
                },
            ));
        }
    }
    out.sort_by_key(|(offset, _)| *offset);
    out.into_iter().map(|(_, action)| action).collect()
}

/// One action per mapper statement, on the line of its start tag.
///
/// `path` becomes the command argument's prefix (`<path>#<id>`); `tooltip` is
/// already localised. A document without a `<mapper` root produces nothing, so
/// a random `.xml` file costs one substring search.
pub fn actions_for(path: &str, text: &str, tooltip: &str) -> Vec<GutterAction> {
    xml_actions(
        path,
        text,
        &Tooltips {
            mapper: tooltip.to_string(),
            wrapper: tooltip.to_string(),
            annotation: tooltip.to_string(),
        },
    )
}

/// The mapper XML statements' buttons.
pub(crate) fn xml_actions(path: &str, text: &str, tooltips: &Tooltips) -> Vec<GutterAction> {
    if !text.contains("<mapper") || !super::setting_on(MAPPER_XML_SETTING) {
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
            tooltip: tooltips.mapper.clone(),
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
        let _host = openidea_plugin::mock::MockHost::install();
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
        let _host = openidea_plugin::mock::MockHost::install();
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
        let _host = openidea_plugin::mock::MockHost::install();
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
        let _host = openidea_plugin::mock::MockHost::install();
        let tooltip = crate::i18n::t_in("zh-CN", TOOLTIP_KEY);
        assert_eq!(tooltip, "预览 SQL");
        let actions = actions_for("/project/UserMapper.xml", MAPPER, &tooltip);
        assert_eq!(actions[0].tooltip, "预览 SQL");
    }

    #[test]
    fn providers_gutter_handles_a_large_document_without_panicking() {
        let _host = openidea_plugin::mock::MockHost::install();
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

    // Java: wrapper chains and annotated statements ----------------------

    /// A wrapper built inline, a wrapper declared then continued, and a mapper
    /// interface method — the three shapes the reference's `WrapperAnchorDetector`
    /// and `AnnotationSqlGutterLineMarkerProvider` cover.
    const SERVICE: &str = r#"package a;

interface UserMapper {
    @Select("select * from sys_user where id = #{id}")
    User findById(Long id);
}

public class UserService {
    public List<User> list(String name) {
        LambdaQueryWrapper<User> wrapper = new LambdaQueryWrapper<>();
        wrapper.eq(User::getName, name);
        wrapper.orderByDesc(User::getId);
        return mapper.selectList(wrapper);
    }

    public List<User> inline(int status) {
        return mapper.selectList(new LambdaQueryWrapper<User>().eq(User::getStatus, status));
    }
}
"#;

    /// The tooltips, without a host: `gutter_actions` reads settings, and the
    /// pure functions take the strings.
    fn tips() -> Tooltips {
        Tooltips {
            mapper: "Preview SQL".to_string(),
            wrapper: "Preview the Wrapper".to_string(),
            annotation: "Preview the annotation".to_string(),
        }
    }

    fn java_doc(text: &str) -> Document {
        Document {
            path: "/project/UserService.java".to_string(),
            language: "java".to_string(),
            version: 1,
            text: text.to_string(),
        }
    }

    #[test]
    fn providers_gutter_paints_one_icon_per_wrapper_chain_root() {
        let _host = openidea_plugin::mock::MockHost::install();
        let actions = java_actions("/project/UserService.java", SERVICE, &tips());
        let wrapper_lines: Vec<u32> = actions
            .iter()
            .filter(|action| action.tooltip == "Preview the Wrapper")
            .map(|action| action.line)
            .collect();
        // The declaration line and the inline `new` line — the two chain roots.
        // `wrapper.eq(...)` and `wrapper.orderByDesc(...)` continue a chain that
        // already speaks for them, so they draw nothing of their own.
        assert_eq!(wrapper_lines.len(), 2, "{actions:?}");
        assert_eq!(wrapper_lines[0], 9, "the declaration line");
        assert_eq!(wrapper_lines[1], 16, "the inline construction's line");
        for action in &actions {
            assert_eq!(action.command, COMMAND);
            assert_eq!(action.icon.as_deref(), Some(ICON));
        }
    }

    #[test]
    fn providers_gutter_paints_the_annotation_line_and_its_own_tooltip() {
        let _host = openidea_plugin::mock::MockHost::install();
        let actions = java_actions("/project/UserService.java", SERVICE, &tips());
        let annotated = actions
            .iter()
            .find(|action| action.tooltip == "Preview the annotation")
            .expect("the @Select line gets an icon");
        assert_eq!(annotated.line, 3);
        // The argument is the annotation's own `@` offset: it starts the span the
        // command resolves, so a stale offset never silently points elsewhere.
        let offset = SERVICE.find("@Select").expect("@Select is in the text");
        assert_eq!(
            annotated.argument.as_deref(),
            Some(format!("/project/UserService.java@{offset}").as_str())
        );
    }

    #[test]
    fn providers_gutter_actions_are_in_document_order() {
        let _host = openidea_plugin::mock::MockHost::install();
        let actions = java_actions("/project/UserService.java", SERVICE, &tips());
        let lines: Vec<u32> = actions.iter().map(|action| action.line).collect();
        let mut sorted = lines.clone();
        sorted.sort_unstable();
        assert_eq!(lines, sorted, "{actions:?}");
    }

    #[test]
    fn providers_gutter_skips_a_chain_inside_an_annotated_method() {
        let _host = openidea_plugin::mock::MockHost::install();
        // The chain is the annotated method's SQL: the annotation icon opens it,
        // so a second icon on the next line would be two icons for one statement.
        let text = r#"package a;
interface M {
    @Select("select * from t")
    List<T> find(String n);
    default List<T> find2(String n) {
        return mapper.selectList(new LambdaQueryWrapper<T>().eq(T::getN, n));
    }
}
"#;
        let actions = java_actions("/project/M.java", text, &tips());
        // The default method has a body, so it is not an annotated statement;
        // the interface method has none and is. One chain root, one annotation.
        let wrappers = actions
            .iter()
            .filter(|a| a.tooltip == "Preview the Wrapper")
            .count();
        assert_eq!(wrappers, 1, "{actions:?}");
    }

    #[test]
    fn providers_gutter_reads_a_java_document_through_the_trait() {
        let _host = openidea_plugin::mock::MockHost::install();
        let mut main = crate::Main;
        let actions = main.gutter_actions(&java_doc(SERVICE));
        assert!(!actions.is_empty(), "a Java document gets icons");
        // And it still says nothing about a Java file that has neither source.
        let plain = main.gutter_actions(&java_doc("package a;\nclass A {}\n"));
        assert!(plain.is_empty());
    }

    #[test]
    fn providers_gutter_java_boolean_setting_off_removes_the_group() {
        let host = openidea_plugin::mock::MockHost::install();
        let mut main = crate::Main;
        let before = main.gutter_actions(&java_doc(SERVICE)).len();
        host.set_setting(WRAPPER_SETTING, "false");
        let after = main.gutter_actions(&java_doc(SERVICE)).len();
        assert!(after < before, "{before} -> {after}");
        // The annotation group is its own switch and stays on.
        assert!(
            main.gutter_actions(&java_doc(SERVICE))
                .iter()
                .any(|action| action.tooltip.contains("annotation")),
            "the annotation group is independent"
        );
    }
}
