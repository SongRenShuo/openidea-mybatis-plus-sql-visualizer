//! SQL injections inside mapper XML (T3-3).
//!
//! Each SQL text node of a `<select|insert|update|delete>` or `<sql>` element
//! becomes a `language = "sql"` range. Attribute values (`test="…"`,
//! `resultMap="…"`) are never injected, and the range is trimmed to the SQL so
//! the host colours text, not indentation.
//!
//! Off by default (`injections.enabled = false`): the host already injects SQL
//! into MyBatis XML (PR #3641). Running both paints every statement twice.

use openidea_plugin::language_features::{Document, Injection, Injections};

pub const SETTING: &str = "injections.enabled";

impl Injections for crate::Main {
    fn injections(&mut self, doc: &Document) -> Vec<Injection> {
        if !super::setting_on(SETTING) || !super::is_xml(&doc.path) {
            return Vec::new();
        }
        sql_injections(&doc.text)
    }
}

/// Every SQL text node of a mapper XML document, in document order.
pub fn sql_injections(text: &str) -> Vec<Injection> {
    let mut positions = crate::indexer::scan::Positions::new(text);
    let mut out = Vec::new();
    for region in super::sqltext::regions(text) {
        // Fragments and statements alike: both are SQL text.
        for (start, end) in region.spans {
            let range = positions.range(start, end);
            out.push(Injection {
                range,
                language: "sql".to_owned(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    use openidea_plugin::providers::Document;

    const XML: &str = r#"<mapper namespace="com.acme.UserMapper">
  <sql id="cols">id, name</sql>
  <select id="findById" test="a &gt; b">
    select <include refid="cols"/>
    from t_user
    <where>
      <if test="id != null">and id = #{id}</if>
    </where>
  </select>
</mapper>"#;

    fn doc(text: &str) -> Document {
        Document {
            path: "/p/UserMapper.xml".into(),
            language: "xml".into(),
            version: 1,
            text: text.into(),
        }
    }

    fn texts<'a>(text: &'a str, injections: &[Injection]) -> Vec<&'a str> {
        injections
            .iter()
            .map(|i| {
                let start = byte(text, i.range.start);
                let end = byte(text, i.range.end);
                &text[start..end]
            })
            .collect()
    }

    fn byte(text: &str, at: openidea_plugin::Position) -> usize {
        openidea_plugin::text::offset(text, at).unwrap()
    }

    #[test]
    fn every_sql_text_node_is_injected_and_nothing_else() {
        let injections = sql_injections(XML);
        assert!(injections.iter().all(|i| i.language == "sql"));
        let found = texts(XML, &injections);
        assert!(found.contains(&"id, name"));
        assert!(found.contains(&"select"));
        assert!(found.contains(&"from t_user"));
        assert!(found.contains(&"and id = #{id}"));
        // `test="a &gt; b"` is an attribute, not a text node.
        assert!(
            !found
                .iter()
                .any(|t| t.contains("a &gt; b") || t.contains("refid"))
        );
        // Ranges are trimmed: no leading newline/indent inside them.
        assert!(found.iter().all(|t| t.trim() == *t));
        assert_eq!(injections.len(), found.len());
    }

    #[test]
    fn cdata_is_sql_text() {
        let xml = "<mapper namespace=\"n\"><select id=\"a\"><![CDATA[select 1 from dual]]></select></mapper>";
        let found = texts(xml, &sql_injections(xml));
        assert_eq!(found, vec!["select 1 from dual"]);
    }

    #[test]
    fn ranges_are_utf16() {
        let xml = "<mapper namespace=\"n\"><select id=\"a\">select '😀'</select></mapper>";
        let injections = sql_injections(xml);
        assert_eq!(injections.len(), 1);
        let width = injections[0].range.end.character - injections[0].range.start.character;
        assert_eq!(width, "select '😀'".encode_utf16().count() as u32);
        // The row is one line, starting at column 0 for the `<mapper` text.
        assert_eq!(injections[0].range.start.line, 0);
    }

    #[test]
    fn the_setting_gates_the_trait_impl() {
        let host = MockHost::install();
        let mut plugin = crate::Main;
        assert!(Injections::injections(&mut plugin, &doc(XML)).is_empty());
        host.set_setting(SETTING, "true");
        assert_eq!(Injections::injections(&mut plugin, &doc(XML)).len(), 4);
        let java = Document {
            path: "/p/M.java".into(),
            language: "java".into(),
            version: 1,
            text: "interface M { @Select(\"select 1\") int a(); }".into(),
        };
        assert!(Injections::injections(&mut plugin, &java).is_empty());
    }

    #[test]
    fn a_non_mapper_xml_injects_nothing() {
        assert!(sql_injections("<beans><select id=\"x\">select 1</select></beans>").is_empty());
    }
}
