//! Cross-document lookups over the facts of one project layout.
//!
//! The host keeps its own project-wide graph; this type is the same idea in
//! memory, which lets the lane's tests (and any future non-host caller) answer
//! "where does this key point" without touching the file system or the host.

use super::{Facts, JavaFacts, XmlFacts, build_edges, facts_for};
use openidea_plugin::Range;
use openidea_plugin::indexing::Edges;

/// Facts of every indexed document, grouped by language.
#[derive(Clone, Debug, Default)]
pub struct MapperGraph {
    pub xml: Vec<XmlFacts>,
    pub java: Vec<JavaFacts>,
}

/// How many alias hops a lookup follows before giving up.
const MAX_ALIAS_DEPTH: usize = 4;

impl MapperGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_xml(&mut self, facts: XmlFacts) -> &mut Self {
        self.xml.push(facts);
        self
    }

    pub fn add_java(&mut self, facts: JavaFacts) -> &mut Self {
        self.java.push(facts);
        self
    }

    /// Indexes `text` when the extension says it is mapper XML or Java.
    pub fn index(&mut self, path: &str, text: &str) -> bool {
        match facts_for(path, text) {
            Some(Facts::Xml(facts)) => {
                self.add_xml(facts);
                true
            }
            Some(Facts::Java(facts)) => {
                self.add_java(facts);
                true
            }
            None => false,
        }
    }

    /// Every document's edges, in insertion order.
    pub fn edges(&self) -> Edges {
        let mut out = Edges::default();
        for (_, edges) in self.documents() {
            merge(&mut out, edges);
        }
        out
    }

    /// One document's edges.
    pub fn edges_for(&self, path: &str) -> Edges {
        self.documents()
            .into_iter()
            .find(|(document, _)| document == path)
            .map(|(_, edges)| edges)
            .unwrap_or_default()
    }

    /// `(path, edges)` of every indexed document, in insertion order.
    fn documents(&self) -> Vec<(String, Edges)> {
        let mut out = Vec::with_capacity(self.xml.len() + self.java.len());
        for facts in &self.xml {
            out.push((facts.path.clone(), build_edges(&Facts::Xml(facts.clone()))));
        }
        for facts in &self.java {
            out.push((facts.path.clone(), build_edges(&Facts::Java(facts.clone()))));
        }
        out
    }

    /// `(path, range)` of every definition of `key`.
    pub fn definitions(&self, key: &str) -> Vec<(String, Range)> {
        let mut out = Vec::new();
        for (path, edges) in self.documents() {
            out.extend(
                edges
                    .definitions
                    .iter()
                    .filter(|a| a.key == key)
                    .map(|a| (path.clone(), a.range)),
            );
        }
        out
    }

    /// `(path, range)` of every reference of `key`.
    pub fn references(&self, key: &str) -> Vec<(String, Range)> {
        let mut out = Vec::new();
        for (path, edges) in self.documents() {
            out.extend(
                edges
                    .references
                    .iter()
                    .filter(|a| a.key == key)
                    .map(|a| (path.clone(), a.range)),
            );
        }
        out
    }

    /// Every alias as `(prefix, target)`.
    pub fn aliases(&self) -> Vec<(String, String)> {
        self.edges()
            .aliases
            .into_iter()
            .map(|a| (a.prefix, a.target))
            .collect()
    }

    /// Definitions of `key`, following aliases (`java:Child#m` →
    /// `java:Base#m`), bounded in depth.
    pub fn resolve(&self, key: &str) -> Vec<(String, Range)> {
        let edges = self.edges();
        let mut keys = vec![key.to_owned()];
        let mut frontier = vec![key.to_owned()];
        for _ in 0..MAX_ALIAS_DEPTH {
            let mut next = Vec::new();
            for candidate in &frontier {
                for alias in &edges.aliases {
                    if let Some(suffix) = candidate.strip_prefix(&alias.prefix) {
                        let target = format!("{}{suffix}", alias.target);
                        if !keys.contains(&target) {
                            keys.push(target.clone());
                            next.push(target);
                        }
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        let mut out = Vec::new();
        for (path, document_edges) in self.documents() {
            for anchor in &document_edges.definitions {
                if keys.contains(&anchor.key) {
                    out.push((path.clone(), anchor.range));
                }
            }
        }
        out
    }
}

fn merge(into: &mut Edges, from: Edges) {
    into.definitions.extend(from.definitions);
    into.references.extend(from.references);
    into.aliases.extend(from.aliases);
    into.markers.extend(from.markers);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::{index_java, index_xml};

    const USER_JAVA: &str = r#"package com.acme.user;

import com.baomidou.mybatisplus.core.mapper.BaseMapper;
import org.apache.ibatis.annotations.Mapper;
import org.apache.ibatis.annotations.Param;
import org.apache.ibatis.annotations.Select;

@Mapper
public interface UserMapper extends BaseMapper<User> {
    @Select("select id, name from t_user where id = #{id}")
    User findById(@Param("id") Long id);

    int save(User user);
}
"#;

    const USER_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE mapper PUBLIC "-//mybatis.org//DTD Mapper 3.0//EN" "http://mybatis.org/dtd/mybatis-3-mapper.dtd">
<mapper namespace="com.acme.user.UserMapper">
  <resultMap id="BaseMap" type="com.acme.user.User">
    <id column="id" property="id"/>
    <result column="name" property="name"/>
  </resultMap>
  <sql id="cols">id, name</sql>
  <select id="findById" resultMap="BaseMap">
    select <include refid="cols"/> from t_user where id = #{id}
  </select>
  <insert id="save" parameterType="com.acme.user.User">
    insert into t_user (name) values (#{name})
  </insert>
</mapper>"#;

    fn single_module() -> MapperGraph {
        let mut graph = MapperGraph::new();
        graph.add_java(index_java(
            "user-service/src/main/java/com/acme/user/UserMapper.java",
            USER_JAVA,
        ));
        graph.add_java(index_java(
            "user-service/src/main/java/com/acme/user/User.java",
            "package com.acme.user;\npublic class User { Long id; String name; }\n",
        ));
        graph.add_xml(index_xml(
            "user-service/src/main/resources/mapper/UserMapper.xml",
            USER_XML,
        ));
        graph
    }

    /// The API module holds the interface, the DAO module the XML, and a third
    /// XML file holds a fragment the first one includes across namespaces.
    fn multi_module() -> MapperGraph {
        let mut graph = MapperGraph::new();
        graph.add_java(index_java(
            "user-api/src/main/java/com/acme/user/UserMapper.java",
            USER_JAVA,
        ));
        graph.add_xml(index_xml(
            "user-dao/src/main/resources/mapper/UserMapper.xml",
            USER_XML,
        ));
        graph.add_xml(index_xml(
            "common-dao/src/main/resources/mapper/CommonMapper.xml",
            r#"<mapper namespace="com.acme.common.CommonMapper">
  <sql id="auditCols">created_at, updated_at</sql>
  <select id="audit">select <include refid="auditCols"/> from t_audit</select>
</mapper>"#,
        ));
        graph.add_xml(index_xml(
            "common-dao/src/main/resources/mapper/CrossMapper.xml",
            r#"<mapper namespace="com.acme.common.CrossMapper">
  <select id="auditJoin">select <include refid="com.acme.common.CommonMapper.auditCols"/> from t_audit</select>
</mapper>"#,
        ));
        graph
    }

    const MPJ_JAVA: &str = r#"package com.acme.mapper;

import com.baomidou.mybatisplus.core.mapper.BaseMapper;
import com.github.yulichang.base.MPJBaseMapper;
import com.acme.model.UserOrderVO;
import org.apache.ibatis.annotations.Mapper;
import org.apache.ibatis.annotations.Param;
import org.apache.ibatis.annotations.Select;
import com.github.yulichang.wrapper.MPJLambdaWrapper;

@Mapper
public interface UserOrderMapper extends MPJBaseMapper<UserOrderVO> {
    @Select("select u.id, o.total from t_user u left join t_order o on o.user_id = u.id")
    List<UserOrderVO> selectJoin(@Param("ew") MPJLambdaWrapper<UserOrderVO> ew);

    default List<UserOrderVO> page(MPJLambdaWrapper<UserOrderVO> wrapper) {
        return selectJoin(wrapper);
    }
}
"#;

    /// The MPJ layout: the mapper is annotation-only, extends the MPJ base and
    /// keeps a `default` method, so its body must not be read as declarations.
    fn mpj() -> MapperGraph {
        let mut graph = MapperGraph::new();
        graph.add_java(index_java(
            "shop/src/main/java/com/acme/mapper/UserOrderMapper.java",
            MPJ_JAVA,
        ));
        graph.add_java(index_java(
            "shop/src/main/java/com/github/yulichang/base/MPJBaseMapper.java",
            r#"package com.github.yulichang.base;
public interface MPJBaseMapper<T> {
    List<T> selectList(Object wrapper);
}
"#,
        ));
        graph
    }

    #[test]
    fn single_module_matches_every_direction() {
        let graph = single_module();
        assert_eq!(
            graph.resolve("stmt:com.acme.user.UserMapper.findById")[0].0,
            "user-service/src/main/resources/mapper/UserMapper.xml"
        );
        assert_eq!(
            graph.resolve("java:com.acme.user.UserMapper#findById")[0].0,
            "user-service/src/main/java/com/acme/user/UserMapper.java"
        );
        assert_eq!(graph.resolve("mapper:com.acme.user.UserMapper").len(), 2);
        assert_eq!(graph.resolve("frag:com.acme.user.UserMapper.cols").len(), 1);
        assert_eq!(
            graph.resolve("rm:com.acme.user.UserMapper.BaseMap").len(),
            1
        );
        assert_eq!(
            graph.resolve("type:com.acme.user.User")[0].0,
            "user-service/src/main/java/com/acme/user/User.java"
        );
    }

    #[test]
    fn multi_module_matches_by_namespace_alone() {
        let graph = multi_module();
        assert_eq!(
            graph.resolve("stmt:com.acme.user.UserMapper.save")[0].0,
            "user-dao/src/main/resources/mapper/UserMapper.xml"
        );
        assert_eq!(
            graph.resolve("java:com.acme.user.UserMapper#save")[0].0,
            "user-api/src/main/java/com/acme/user/UserMapper.java"
        );
        assert_eq!(
            graph.resolve("mapper:com.acme.user.UserMapper").len(),
            2,
            "one definition in each module"
        );
        assert_eq!(
            graph.resolve("frag:com.acme.common.CommonMapper.auditCols")[0].0,
            "common-dao/src/main/resources/mapper/CommonMapper.xml"
        );
        assert_eq!(
            graph.resolve("stmt:com.acme.common.CommonMapper.audit")[0].0,
            "common-dao/src/main/resources/mapper/CommonMapper.xml"
        );
        assert_eq!(
            graph
                .resolve("stmt:com.acme.common.CrossMapper.auditJoin")
                .len(),
            1
        );
    }

    #[test]
    fn mpj_layout_keeps_annotations_and_resolves_inherited_methods() {
        let graph = mpj();
        assert_eq!(
            graph.resolve("java:com.acme.mapper.UserOrderMapper#selectJoin")[0].0,
            "shop/src/main/java/com/acme/mapper/UserOrderMapper.java"
        );
        assert_eq!(
            graph
                .resolve("mapper:com.acme.mapper.UserOrderMapper")
                .len(),
            1
        );
        // `selectList` is not declared here; the interface alias reaches the
        // MPJ base interface.
        let inherited = graph.resolve("java:com.acme.mapper.UserOrderMapper#selectList");
        assert_eq!(
            inherited[0].0,
            "shop/src/main/java/com/github/yulichang/base/MPJBaseMapper.java"
        );
        assert!(graph.aliases().contains(&(
            "java:com.acme.mapper.UserOrderMapper#".to_owned(),
            "java:com.github.yulichang.base.MPJBaseMapper#".to_owned()
        )));
    }

    #[test]
    fn eight_cross_file_key_pairs_match_both_ways() {
        let graph = single_module();
        let pairs = [
            ("mapper:com.acme.user.UserMapper", 2),
            ("stmt:com.acme.user.UserMapper.findById", 1),
            ("stmt:com.acme.user.UserMapper.save", 1),
            ("java:com.acme.user.UserMapper#findById", 1),
            ("java:com.acme.user.UserMapper#save", 1),
            ("frag:com.acme.user.UserMapper.cols", 1),
            ("rm:com.acme.user.UserMapper.BaseMap", 1),
            ("type:com.acme.user.User", 1),
        ];
        for (key, count) in pairs {
            assert_eq!(graph.resolve(key).len(), count, "definitions of {key}");
        }
        let edges = graph.edges();
        let defined: Vec<&str> = edges.definitions.iter().map(|a| a.key.as_str()).collect();
        let referred: Vec<&str> = edges.references.iter().map(|a| a.key.as_str()).collect();
        for key in [
            "stmt:com.acme.user.UserMapper.findById",
            "java:com.acme.user.UserMapper#findById",
        ] {
            assert!(
                defined.contains(&key) && referred.contains(&key),
                "{key} is not on both sides"
            );
        }
    }

    #[test]
    fn indexing_dispatches_by_extension_and_ignores_other_files() {
        let mut graph = MapperGraph::new();
        assert!(graph.index("a/b/UserMapper.java", USER_JAVA));
        assert!(graph.index("a/b/UserMapper.xml", USER_XML));
        assert!(!graph.index("a/b/build.properties", "x=y"));
        assert_eq!(graph.java.len(), 1);
        assert!(!graph.index("notes.md", "# hi"));
    }
}
