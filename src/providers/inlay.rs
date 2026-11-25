//! INSERT target column hints (T3-4).
//!
//! `insert into t_user values (…)` with no column list is the one place where
//! a hint can save a mistake only if the columns are actually known. Columns
//! come from [`crate::providers::set_column_lookup`] (the schema lane); with no
//! source, or a table it does not know, nothing is shown — no guessing.

use crate::indexer::StatementKind;
use openidea_plugin::Range;
use openidea_plugin::inlay_hints::{Document, InlayHint, InlayHints, InlayKind};

impl InlayHints for crate::Main {
    fn inlay_hints(&mut self, doc: &Document, _range: Range) -> Vec<InlayHint> {
        insert_hints(&doc.text, &super::columns_for)
    }
}

/// Hints for every `insert into <table>` without an explicit column list whose
/// table columns `columns` can name.
pub fn insert_hints(text: &str, columns: &dyn Fn(&str) -> Option<Vec<String>>) -> Vec<InlayHint> {
    let mut out: Vec<InlayHint> = Vec::new();
    for region in super::sqltext::regions(text) {
        if region.kind != Some(StatementKind::Insert) {
            continue;
        }
        for (start, end) in region.spans {
            let Some((table, table_end)) = insert_target(text, start, end) else {
                continue;
            };
            if has_column_list(text, table_end, end) {
                continue;
            }
            let Some(names) = columns(&table) else {
                continue;
            };
            if names.is_empty() {
                continue;
            }
            let position = openidea_plugin::text::position(text, table_end);
            if out.iter().any(|hint| hint.position == position) {
                continue;
            }
            out.push(
                InlayHint::new(position, format!("({})", preview(&names)))
                    .kind(InlayKind::Parameter)
                    .tooltip(format!("columns of {table}"))
                    .padding_right(),
            );
        }
    }
    out
}

/// The table of `insert into <table>` inside `start..end`, with the offset just
/// past its name.
fn insert_target(text: &str, start: usize, end: usize) -> Option<(String, usize)> {
    let insert = super::sqltext::find_ci(text, "insert", start)?;
    if insert >= end {
        return None;
    }
    let mut i = super::sqltext::skip_ws(text, insert + "insert".len());
    if !text
        .get(i..)
        .is_some_and(|rest| rest.len() >= 4 && rest[..4].eq_ignore_ascii_case("into"))
    {
        return None;
    }
    i = super::sqltext::skip_ws(text, i + 4);
    let (name_start, name_end) = super::sqltext::ident_span(text, i)?;
    if name_end > end {
        return None;
    }
    let table = super::sqltext::unquote(&text[name_start..name_end]).to_owned();
    if table.is_empty() {
        return None;
    }
    Some((table, name_end))
}

/// `insert into t (a, b)` already names its columns; the hint would be noise.
fn has_column_list(text: &str, table_end: usize, span_end: usize) -> bool {
    let i = super::sqltext::skip_ws(text, table_end);
    i < span_end && text.as_bytes().get(i) == Some(&b'(')
}

fn preview(names: &[String]) -> String {
    const SHOWN: usize = 6;
    let mut shown = names
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > SHOWN {
        shown.push_str(", …");
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    use openidea_plugin::providers::Document;

    fn schema(table: &str) -> Option<Vec<String>> {
        match table {
            "t_user" => Some(vec!["id".into(), "name".into(), "created_at".into()]),
            "t_order" => Some(vec!["id".into(), "user_id".into(), "total".into()]),
            _ => None,
        }
    }

    fn none(_: &str) -> Option<Vec<String>> {
        None
    }

    const XML: &str = r#"<mapper namespace="n">
  <insert id="a">insert into t_user values (#{id}, #{name})</insert>
  <insert id="b">insert into `t_order` (id, total) values (#{id}, #{total})</insert>
  <insert id="c">insert into t_unknown values (1)</insert>
</mapper>"#;

    #[test]
    fn a_known_table_without_a_column_list_gets_one_hint() {
        let hints = insert_hints(XML, &schema);
        let a = hints
            .iter()
            .find(|h| h.tooltip.as_deref() == Some("columns of t_user"));
        let a = a.expect("hint for t_user");
        assert_eq!(a.label, "(id, name, created_at)");
        assert_eq!(a.kind, InlayKind::Parameter);
        assert!(a.padding_right);
    }

    #[test]
    fn an_explicit_column_list_or_an_unknown_table_gets_none() {
        let hints = insert_hints(XML, &schema);
        assert!(
            hints
                .iter()
                .all(|h| h.tooltip.as_deref() != Some("columns of t_order"))
        );
        assert!(
            hints
                .iter()
                .all(|h| h.tooltip.as_deref() != Some("columns of t_unknown"))
        );
        assert_eq!(hints.len(), 1);
    }

    #[test]
    fn no_column_source_means_no_hints_at_all() {
        assert!(insert_hints(XML, &none).is_empty());
    }

    #[test]
    fn a_quoted_table_name_is_unquoted_for_the_lookup() {
        let xml = "<mapper namespace=\"n\"><insert id=\"a\">insert into `t_order` values (1)</insert></mapper>";
        let hints = insert_hints(xml, &schema);
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].tooltip.as_deref(), Some("columns of t_order"));
        assert_eq!(hints[0].label, "(id, user_id, total)");
    }

    #[test]
    fn annotated_java_inserts_are_covered() {
        let java = "interface M {\n  @Insert(\"insert into t_user values (#{id})\")\n  int add(Integer id);\n}\n";
        let hints = insert_hints(java, &schema);
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].label, "(id, name, created_at)");
    }

    #[test]
    fn the_trait_impl_is_silent_without_a_source() {
        let _host = MockHost::install();
        crate::providers::set_column_lookup(None);
        let mut plugin = crate::Main;
        let document = Document {
            path: "/p/M.xml".into(),
            language: "xml".into(),
            version: 1,
            text: XML.into(),
        };
        assert!(InlayHints::inlay_hints(&mut plugin, &document, Range::default()).is_empty());
    }
}
