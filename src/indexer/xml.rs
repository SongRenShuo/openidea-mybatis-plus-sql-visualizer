//! Mapper XML → facts (T3-1).
//!
//! A mapper file is read as text, one tag at a time: no DOM, no IO, no entity
//! expansion. Everything the graph needs — namespace, statements, result maps,
//! `<sql>` fragments, `include`/`resultMap`/`extends`/`type` references — comes
//! out of this pass.

use super::scan::{self, Attr, Positions};
use super::{Named, StatementKind};

/// A `<select|insert|update|delete>` element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XmlStatement {
    pub kind: StatementKind,
    /// `id="…"` (value range; `None` when the attribute is missing).
    pub id: Option<Named>,
    /// `resultMap="…"` on the element.
    pub result_map: Option<Named>,
    /// `resultType="…"` on the element.
    pub result_type: Option<Named>,
    /// `<include refid>` elements nested anywhere inside the statement.
    pub includes: Vec<Named>,
    /// 0-based line of the start tag.
    pub line: u32,
}

/// A `<resultMap>` element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XmlResultMap {
    pub id: Option<Named>,
    /// `type="…"` (the mapped Java class, when declared).
    pub type_name: Option<Named>,
    /// `extends="…"` (another result map id).
    pub extends: Option<Named>,
}

/// Everything one mapper XML document states.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct XmlFacts {
    pub path: String,
    /// `<mapper namespace="…">`.
    pub namespace: Option<Named>,
    pub statements: Vec<XmlStatement>,
    pub result_maps: Vec<XmlResultMap>,
    /// `<sql id="…">` fragments.
    pub fragments: Vec<Named>,
    /// Every `<include refid="…">` value, in document order.
    pub includes: Vec<Named>,
    /// `resultMap="…"` on statements, associations, collections and cases.
    pub result_map_refs: Vec<Named>,
    /// `type` / `resultType` / `ofType` values (Java classes).
    pub type_refs: Vec<Named>,
}

impl XmlFacts {
    /// The result map id a reference names, dropping this file's namespace
    /// prefix when the reference is written `namespace.id`.
    pub fn local_result_map(&self, reference: &str) -> Option<&Named> {
        let local = match (reference.rsplit_once('.'), &self.namespace) {
            (Some((ns, id)), Some(namespace)) if ns == namespace.name => id,
            _ => reference,
        };
        self.result_maps
            .iter()
            .find(|rm| rm.id.as_ref().is_some_and(|id| id.name == local))?
            .id
            .as_ref()
    }

    /// The `<sql>` fragment a `refid` names: `id` in this file, or
    /// `namespace.id` when the namespace is this file's.
    pub fn local_fragment(&self, reference: &str) -> Option<&Named> {
        let local = match (reference.rsplit_once('.'), &self.namespace) {
            (Some((ns, id)), Some(namespace)) if ns == namespace.name => id,
            _ => reference,
        };
        self.fragments.iter().rev().find(|f| f.name == local)
    }
}

fn named<'a>(positions: &mut Positions<'_>, attr: Option<&Attr<'a>>) -> Option<Named> {
    let attr = attr?;
    if attr.value.is_empty() {
        return None;
    }
    Some(Named {
        name: attr.value.to_owned(),
        range: positions.range(attr.value_start, attr.value_end),
    })
}

/// Reads one mapper XML document. Non-mapper roots yield empty facts.
pub fn index_xml(path: &str, text: &str) -> XmlFacts {
    let mut facts = XmlFacts {
        path: path.to_owned(),
        ..XmlFacts::default()
    };
    let elements = scan::elements(text);
    let Some(root) = elements.iter().position(|e| e.parent.is_none()) else {
        return facts;
    };
    if elements[root].name != "mapper" {
        return facts;
    }
    let mut positions = Positions::new(text);
    facts.namespace = named(&mut positions, elements[root].attr("namespace"));

    // Statement elements by index, so `<include>`s can be attached to them.
    let mut statement_of_element: Vec<Option<usize>> = vec![None; elements.len()];
    for (idx, element) in elements.iter().enumerate() {
        if element.parent != Some(root) {
            continue;
        }
        if let Some(kind) = StatementKind::from_tag(element.name) {
            let statement = XmlStatement {
                kind,
                id: named(&mut positions, element.attr("id")),
                result_map: named(&mut positions, element.attr("resultMap")),
                result_type: named(&mut positions, element.attr("resultType")),
                includes: Vec::new(),
                line: positions.at(element.start).line,
            };
            if let Some(result_map) = statement.result_map.clone() {
                facts.result_map_refs.push(result_map);
            }
            if let Some(result_type) = statement.result_type.clone() {
                facts.type_refs.push(result_type);
            }
            statement_of_element[idx] = Some(facts.statements.len());
            facts.statements.push(statement);
        } else if element.name == "sql" {
            if let Some(id) = named(&mut positions, element.attr("id")) {
                facts.fragments.push(id);
            }
        } else if element.name == "resultMap" {
            let result_map = XmlResultMap {
                id: named(&mut positions, element.attr("id")),
                type_name: named(&mut positions, element.attr("type")),
                extends: named(&mut positions, element.attr("extends")),
            };
            if let Some(type_name) = result_map.type_name.clone() {
                facts.type_refs.push(type_name);
            }
            if let Some(extends) = result_map.extends.clone() {
                facts.result_map_refs.push(extends);
            }
            facts.result_maps.push(result_map);
        }
    }

    // Includes and nested result-map references, at any depth.
    for (idx, element) in elements.iter().enumerate() {
        match element.name {
            "include" => {
                let Some(refid) = named(&mut positions, element.attr("refid")) else {
                    continue;
                };
                if let Some(statement) = elements[idx].parent.and_then(|p| statement_of_element[p])
                {
                    facts.statements[statement].includes.push(refid.clone());
                }
                facts.includes.push(refid);
            }
            "association" | "collection" | "case" => {
                if let Some(reference) = named(&mut positions, element.attr("resultMap")) {
                    facts.result_map_refs.push(reference);
                }
                if element.name != "case" {
                    if let Some(of_type) = named(&mut positions, element.attr("ofType")) {
                        facts.type_refs.push(of_type);
                    }
                }
            }
            _ => {}
        }
    }
    facts
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{Position, Range};

    const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE mapper PUBLIC "-//mybatis.org//DTD Mapper 3.0//EN" "http://mybatis.org/dtd/mybatis-3-mapper.dtd">
<mapper namespace="com.acme.user.UserMapper">
  <resultMap id="BaseMap" type="com.acme.user.User">
    <id column="id" property="id"/>
    <association property="team" resultMap="TeamMap"/>
  </resultMap>
  <resultMap id="RichMap" type="com.acme.user.User" extends="BaseMap"/>
  <sql id="Base_Column_List">id, name</sql>
  <select id="findById" resultMap="BaseMap">
    select <include refid="Base_Column_List"/> from t_user where id = #{id}
  </select>
  <select id="listAll" resultType="com.acme.user.User">
    select * from t_user <include refid="com.acme.shared.Common.tempTable"/>
  </select>
  <insert id="save">insert into t_user (id) values (#{id})</insert>
</mapper>"#;

    #[test]
    fn reads_namespace_statements_maps_and_fragments() {
        let facts = index_xml("/p/src/main/resources/mapper/UserMapper.xml", XML);
        assert_eq!(
            facts.namespace.as_ref().map(|n| n.name.as_str()),
            Some("com.acme.user.UserMapper")
        );
        assert_eq!(facts.statements.len(), 3);
        assert_eq!(facts.statements[0].kind, StatementKind::Select);
        assert_eq!(facts.statements[0].id.as_ref().unwrap().name, "findById");
        assert_eq!(facts.statements[0].includes.len(), 1);
        assert_eq!(facts.statements[0].includes[0].name, "Base_Column_List");
        assert_eq!(
            facts.statements[1].includes[0].name,
            "com.acme.shared.Common.tempTable"
        );
        assert_eq!(facts.fragments.len(), 1);
        assert_eq!(facts.result_maps.len(), 2);
        assert_eq!(facts.result_maps[0].id.as_ref().unwrap().name, "BaseMap");
        assert_eq!(
            facts.result_maps[0].type_name.as_ref().unwrap().name,
            "com.acme.user.User"
        );
        assert_eq!(
            facts.result_maps[1].extends.as_ref().unwrap().name,
            "BaseMap"
        );
        assert!(facts.result_map_refs.iter().any(|r| r.name == "TeamMap"));
        assert_eq!(facts.statements[0].line, 9);
    }

    #[test]
    fn id_ranges_cover_the_name_only() {
        let facts = index_xml("a.xml", XML);
        let id = facts.statements[0].id.as_ref().unwrap();
        assert_eq!(
            id.range,
            Range::new(Position::new(9, 14), Position::new(9, 22))
        );
    }

    #[test]
    fn local_references_resolve_and_foreign_ones_do_not() {
        let facts = index_xml("a.xml", XML);
        assert_eq!(
            facts.local_fragment("Base_Column_List").unwrap().name,
            "Base_Column_List"
        );
        assert_eq!(
            facts
                .local_fragment("com.acme.user.UserMapper.Base_Column_List")
                .unwrap()
                .name,
            "Base_Column_List"
        );
        assert!(
            facts
                .local_fragment("com.acme.shared.Common.tempTable")
                .is_none()
        );
        assert_eq!(facts.local_result_map("BaseMap").unwrap().name, "BaseMap");
        assert_eq!(
            facts
                .local_result_map("com.acme.user.UserMapper.BaseMap")
                .unwrap()
                .name,
            "BaseMap"
        );
        assert!(facts.local_result_map("Nope").is_none());
    }

    #[test]
    fn a_non_mapper_root_yields_nothing() {
        let facts = index_xml(
            "spring.xml",
            r#"<beans><select id="not-a-mapper"/></beans>"#,
        );
        assert!(facts.namespace.is_none());
        assert!(facts.statements.is_empty());
    }
}
