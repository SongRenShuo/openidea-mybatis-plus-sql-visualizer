//! iBatis 2.x dynamic SQL (T1-5): `dynamic`/`isNotNull`/`iterate`, `#name#`.
//!
//! Lane L1 owns this directory; see `docs/TASKS.md` T1-5.
//!
//! The chain is deliberately decoupled from the MyBatis 3 code: iBatis 2 has no
//! expression language, a different tag set and single-character `#` / `$`
//! delimiters. Behaviour follows the reference implementation (itself derived
//! from `ibatis-sqlmap:2.3.4.726`):
//!
//! - `open` / `prepend` / `conjunction` / `close` are concatenated **verbatim**
//!   (no space is inserted — `ANDORG_NAME` is the engine's real output);
//! - `$` substitution happens only after the tag tree has been evaluated;
//! - `<selectKey>` never leaks into the host statement body;
//! - a declared `<parameterMap>` keeps `#` verbatim on non-dynamic statements
//!   and binds the declared properties positionally;
//! - unknown tags are degraded to their contents with a warning (the engine
//!   drops the whole subtree — this chain keeps the text visible).

mod render;
pub mod token;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use render::render_statement;
pub use token::{InlineMapping, InlineOutcome};

use super::xml::RenderOutput;
use super::xml::xmltree::{Element, XmlNode};

pub const KEY_ROOT_INVALID: &str = "ibatis.root_invalid";
pub const KEY_PARSE_FAILED: &str = "ibatis.parse.failed";
pub const KEY_STATEMENT_NOT_FOUND: &str = "ibatis.statement.not_found";
pub const KEY_INCLUDE_NOT_FOUND: &str = "ibatis.include.not_found";
pub const KEY_INCLUDE_CYCLE: &str = "ibatis.include.cycle";
pub const KEY_INCLUDE_TOO_DEEP: &str = "ibatis.include.too_deep";
pub const KEY_UNKNOWN_TAG: &str = "ibatis.unknown_tag";
pub const KEY_ITERATE_NOT_COLLECTION: &str = "ibatis.iterate.not_collection";
pub const KEY_DEGRADED: &str = "ibatis.render.degraded";
pub const KEY_PARAMETER_MAP_UNKNOWN: &str = "ibatis.parameter_map.not_found";

const MAX_INCLUDE_DEPTH: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StatementKind {
    Select,
    Insert,
    Update,
    Delete,
    Statement,
    Procedure,
}

impl StatementKind {
    fn by_tag(name: &str) -> Option<Self> {
        match name {
            "select" => Some(Self::Select),
            "insert" => Some(Self::Insert),
            "update" => Some(Self::Update),
            "delete" => Some(Self::Delete),
            "statement" => Some(Self::Statement),
            "procedure" => Some(Self::Procedure),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagKind {
    Dynamic,
    Iterate,
    IsEmpty,
    IsEqual,
    IsGreaterEqual,
    IsGreaterThan,
    IsLessEqual,
    IsLessThan,
    IsNotEmpty,
    IsNotEqual,
    IsNotNull,
    IsNotParameterPresent,
    IsNotPropertyAvailable,
    IsNull,
    IsParameterPresent,
    IsPropertyAvailable,
}

impl TagKind {
    pub fn tag_name(self) -> &'static str {
        match self {
            Self::Dynamic => "dynamic",
            Self::Iterate => "iterate",
            Self::IsEmpty => "isEmpty",
            Self::IsEqual => "isEqual",
            Self::IsGreaterEqual => "isGreaterEqual",
            Self::IsGreaterThan => "isGreaterThan",
            Self::IsLessEqual => "isLessEqual",
            Self::IsLessThan => "isLessThan",
            Self::IsNotEmpty => "isNotEmpty",
            Self::IsNotEqual => "isNotEqual",
            Self::IsNotNull => "isNotNull",
            Self::IsNotParameterPresent => "isNotParameterPresent",
            Self::IsNotPropertyAvailable => "isNotPropertyAvailable",
            Self::IsNull => "isNull",
            Self::IsParameterPresent => "isParameterPresent",
            Self::IsPropertyAvailable => "isPropertyAvailable",
        }
    }

    fn by_tag(name: &str) -> Option<Self> {
        Some(match name {
            "dynamic" => Self::Dynamic,
            "iterate" => Self::Iterate,
            "isEmpty" => Self::IsEmpty,
            "isEqual" => Self::IsEqual,
            "isGreaterEqual" => Self::IsGreaterEqual,
            "isGreaterThan" => Self::IsGreaterThan,
            "isLessEqual" => Self::IsLessEqual,
            "isLessThan" => Self::IsLessThan,
            "isNotEmpty" => Self::IsNotEmpty,
            "isNotEqual" => Self::IsNotEqual,
            "isNotNull" => Self::IsNotNull,
            "isNotParameterPresent" => Self::IsNotParameterPresent,
            "isNotPropertyAvailable" => Self::IsNotPropertyAvailable,
            "isNull" => Self::IsNull,
            "isParameterPresent" => Self::IsParameterPresent,
            "isPropertyAvailable" => Self::IsPropertyAvailable,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct IbatisText {
    pub raw: String,
    pub post_parse: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IbatisTag {
    pub kind: TagKind,
    pub prepend: Option<String>,
    pub property: Option<String>,
    pub remove_first_prepend: Option<String>,
    pub open: Option<String>,
    pub close: Option<String>,
    pub conjunction: Option<String>,
    pub compare_property: Option<String>,
    pub compare_value: Option<String>,
    /// Marker state computed at build time (see [`compute_remove_first_prepend`]).
    pub rfp_initial: bool,
    pub children: Vec<IbatisNode>,
}

impl IbatisTag {
    pub fn has_prepend(&self) -> bool {
        self.prepend
            .as_deref()
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    }

    pub fn has_open(&self) -> bool {
        self.open.as_deref().map(|s| !s.is_empty()).unwrap_or(false)
    }

    pub fn has_close(&self) -> bool {
        self.close
            .as_deref()
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    }

    pub fn has_conjunction(&self) -> bool {
        self.conjunction
            .as_deref()
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum IbatisNode {
    Text(IbatisText),
    Tag(IbatisTag),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParameterDef {
    pub property: String,
    pub java_type: Option<String>,
    pub jdbc_type: Option<String>,
    pub null_value: Option<String>,
    pub mode: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParameterMap {
    pub id: String,
    pub class: Option<String>,
    pub parameters: Vec<ParameterDef>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IbatisStatement {
    pub id: String,
    pub kind: StatementKind,
    pub body: Vec<IbatisNode>,
    pub select_key: Option<Vec<IbatisNode>>,
    pub parameter_map: Option<String>,
    /// Statement-level degradation facts collected while building.
    pub issues: Vec<IbatisIssue>,
}

impl IbatisStatement {
    pub fn is_dynamic(&self) -> bool {
        self.body.iter().any(|n| matches!(n, IbatisNode::Tag(_)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IbatisIssue {
    pub message_key: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IbatisModel {
    pub namespace: Option<String>,
    pub statements: BTreeMap<String, IbatisStatement>,
    pub parameter_maps: BTreeMap<String, ParameterMap>,
    pub issues: Vec<IbatisIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message_key: &'static str,
    pub detail: String,
}

impl ParseError {
    fn new(message_key: &'static str, detail: impl Into<String>) -> Self {
        Self {
            message_key,
            detail: detail.into(),
        }
    }
}

/// Parses a `<sqlMap>` document. Structure errors are returned; statement-level
/// problems are collected in [`IbatisModel::issues`] and never abort loading.
pub fn parse(xml: &str) -> Result<IbatisModel, ParseError> {
    let root = super::xml::xmltree::parse_document(xml)
        .map_err(|e| ParseError::new(KEY_PARSE_FAILED, e.message))?;
    if root.name != "sqlMap" {
        return Err(ParseError::new(
            KEY_ROOT_INVALID,
            format!("root element is <{}>, expected <sqlMap>", root.name),
        ));
    }
    let namespace = root.attr("namespace").map(|s| s.to_string());
    let mut issues = Vec::new();
    let mut fragments: BTreeMap<String, Element> = BTreeMap::new();
    for element in root.child_elements() {
        if element.name == "sql" {
            let id = element.attr_or_empty("id");
            if !id.is_empty() && !fragments.contains_key(id) {
                fragments.insert(id.to_string(), element.clone());
            }
        }
    }
    let mut parameter_maps = BTreeMap::new();
    for element in root.child_elements() {
        if element.name != "parameterMap" {
            continue;
        }
        let Some(id) = element.attr("id") else {
            issues.push(IbatisIssue {
                message_key: "ibatis.parameter_map.missing_id".to_string(),
                detail: "<parameterMap> without id".to_string(),
            });
            continue;
        };
        let parameters = element
            .child_elements()
            .filter(|e| e.name == "parameter")
            .map(|e| ParameterDef {
                property: e.attr_or_empty("property").to_string(),
                java_type: e.attr("javaType").map(|s| s.to_string()),
                jdbc_type: e.attr("jdbcType").map(|s| s.to_string()),
                null_value: e.attr("nullValue").map(|s| s.to_string()),
                mode: e.attr("mode").map(|s| s.to_string()),
            })
            .collect();
        parameter_maps.insert(
            id.to_string(),
            ParameterMap {
                id: id.to_string(),
                class: element.attr("class").map(|s| s.to_string()),
                parameters,
            },
        );
    }

    let mut statements = BTreeMap::new();
    for element in root.child_elements() {
        let Some(kind) = StatementKind::by_tag(&element.name) else {
            continue;
        };
        let id = element.attr_or_empty("id");
        if id.is_empty() {
            continue;
        }
        let mut statement_issues: Vec<IbatisIssue> = Vec::new();
        let mut builder = Builder {
            fragments: &fragments,
            namespace: namespace.as_deref(),
            issues: &mut statement_issues,
            chain: Vec::new(),
            fatal: None,
        };
        let body = builder.build_children(element, false, None);
        let select_key = element
            .child_elements()
            .find(|e| e.name == "selectKey")
            .map(|e| builder.build_children(e, false, None));
        if let Some((key, detail)) = builder.fatal.take() {
            statement_issues.push(IbatisIssue {
                message_key: key,
                detail,
            });
        }
        if statements.contains_key(id) {
            issues.push(IbatisIssue {
                message_key: "ibatis.statement.duplicate".to_string(),
                detail: id.to_string(),
            });
            continue;
        }
        statements.insert(
            id.to_string(),
            IbatisStatement {
                id: id.to_string(),
                kind,
                body,
                select_key,
                parameter_map: element.attr("parameterMap").map(|s| s.to_string()),
                issues: statement_issues,
            },
        );
    }
    Ok(IbatisModel {
        namespace,
        statements,
        parameter_maps,
        issues,
    })
}

struct Builder<'a> {
    fragments: &'a BTreeMap<String, Element>,
    namespace: Option<&'a str>,
    issues: &'a mut Vec<IbatisIssue>,
    /// Include chain for cycle detection.
    chain: Vec<String>,
    /// Set when the document cannot be rendered at all (cycle / depth).
    fatal: Option<(String, String)>,
}

impl Builder<'_> {
    fn build_children(
        &mut self,
        element: &Element,
        post_parse: bool,
        parent_rfp: Option<&str>,
    ) -> Vec<IbatisNode> {
        let mut out = Vec::new();
        for child in &element.children {
            match child {
                XmlNode::Comment(_) => {}
                XmlNode::Text(text) => out.push(IbatisNode::Text(IbatisText {
                    raw: text.clone(),
                    post_parse,
                })),
                XmlNode::Element(child) => {
                    self.build_element(child, post_parse, parent_rfp, &mut out)
                }
            }
        }
        out
    }

    fn build_element(
        &mut self,
        element: &Element,
        post_parse: bool,
        parent_rfp: Option<&str>,
        out: &mut Vec<IbatisNode>,
    ) {
        match element.name.as_str() {
            // `selectKey` is assembled as its own statement by the engine.
            "selectKey" => {}
            "include" => {
                let refid = element.attr_or_empty("refid");
                if refid.is_empty() {
                    self.issues.push(IbatisIssue {
                        message_key: KEY_INCLUDE_NOT_FOUND.to_string(),
                        detail: "<include> without refid".to_string(),
                    });
                    return;
                }
                if self.chain.iter().any(|id| id == refid) {
                    self.fatal = Some((
                        KEY_INCLUDE_CYCLE.to_string(),
                        format!("include cycle detected: {refid}"),
                    ));
                    return;
                }
                if self.chain.len() >= MAX_INCLUDE_DEPTH {
                    self.fatal = Some((
                        KEY_INCLUDE_TOO_DEEP.to_string(),
                        format!("include nesting deeper than {MAX_INCLUDE_DEPTH}"),
                    ));
                    return;
                }
                let direct = self.fragments.get(refid);
                let namespaced_id = apply_namespace(refid, self.namespace);
                let target = match direct {
                    Some(t) => Some(t),
                    None => self.fragments.get(&namespaced_id),
                };
                let Some(target) = target else {
                    self.issues.push(IbatisIssue {
                        message_key: KEY_INCLUDE_NOT_FOUND.to_string(),
                        detail: refid.to_string(),
                    });
                    out.push(IbatisNode::Text(IbatisText {
                        raw: format!("-- [mpsv] include not found: {refid}"),
                        post_parse: false,
                    }));
                    return;
                };
                self.chain.push(refid.to_string());
                // The include boundary resets `postParseRequired` to false.
                let children = self.build_children(target, false, parent_rfp);
                self.chain.pop();
                out.extend(children);
                let _ = post_parse;
            }
            name => {
                let Some(kind) = TagKind::by_tag(name) else {
                    self.issues.push(IbatisIssue {
                        message_key: KEY_UNKNOWN_TAG.to_string(),
                        detail: format!("<{name}>"),
                    });
                    out.extend(self.build_children(element, post_parse, parent_rfp));
                    return;
                };
                let prepend = element.attr("prepend").map(|s| s.to_string());
                let remove_first_prepend =
                    element.attr("removeFirstPrepend").map(|s| s.to_string());
                let has_prepend = prepend.as_deref().map(|s| !s.is_empty()).unwrap_or(false);
                let rfp_initial = compute_remove_first_prepend(
                    kind,
                    has_prepend,
                    remove_first_prepend.as_deref(),
                    parent_rfp,
                );
                let child_post_parse = post_parse || kind == TagKind::Iterate;
                let children =
                    self.build_children(element, child_post_parse, remove_first_prepend.as_deref());
                out.push(IbatisNode::Tag(IbatisTag {
                    kind,
                    prepend,
                    property: element.attr("property").map(|s| s.to_string()),
                    remove_first_prepend,
                    open: element.attr("open").map(|s| s.to_string()),
                    close: element.attr("close").map(|s| s.to_string()),
                    conjunction: element.attr("conjunction").map(|s| s.to_string()),
                    compare_property: element.attr("compareProperty").map(|s| s.to_string()),
                    compare_value: element.attr("compareValue").map(|s| s.to_string()),
                    rfp_initial,
                    children,
                }));
            }
        }
    }
}

fn compute_remove_first_prepend(
    kind: TagKind,
    has_prepend: bool,
    remove_first_prepend: Option<&str>,
    parent_rfp: Option<&str>,
) -> bool {
    if kind == TagKind::Dynamic {
        return has_prepend;
    }
    if matches!(remove_first_prepend, Some("true") | Some("iterate")) {
        return true;
    }
    if !has_prepend {
        return matches!(parent_rfp, Some("true") | Some("iterate"));
    }
    false
}

fn apply_namespace(id: &str, namespace: Option<&str>) -> String {
    match namespace {
        Some(ns) if !ns.is_empty() && !id.contains('.') => format!("{ns}.{id}"),
        _ => id.to_string(),
    }
}

/// Renders one statement of a `<sqlMap>` document.
pub fn render(xml: &str, statement_id: &str, params: &Value) -> RenderOutput {
    let model = match parse(xml) {
        Ok(model) => model,
        Err(e) => return RenderOutput::unresolved(e.message_key, e.detail),
    };
    let Some(statement) = model.statements.get(statement_id) else {
        return RenderOutput::unresolved(KEY_STATEMENT_NOT_FOUND, statement_id.to_string());
    };
    render_statement(statement, &model, params)
}

#[cfg(test)]
mod tests {
    use super::super::xml::Confidence;
    use super::*;

    const MAP: &str = r#"<?xml version="1.0"?>
<!DOCTYPE sqlMap PUBLIC "-//iBATIS.com//DTD SQL Map 2.0//EN" "http://www.ibatis.com/dtd/sql-map-2.dtd">
<sqlMap namespace="User">
    <parameterMap id="userMap" class="com.example.User">
        <parameter property="id" jdbcType="INTEGER"/>
        <parameter property="name" jdbcType="VARCHAR"/>
    </parameterMap>
    <sql id="cols">id, name</sql>

    <select id="selectAll" resultClass="User">
        SELECT <include refid="cols"/> FROM users
    </select>

    <select id="selectByName">
        SELECT * FROM users
        <dynamic prepend="WHERE">
            <isNotNull property="name" prepend="AND">name = #name#</isNotNull>
        </dynamic>
    </select>

    <select id="selectIn">
        SELECT * FROM users
        <dynamic prepend="WHERE">
            <iterate property="ids" open="id IN (" close=")" conjunction=",">
                #ids[]#
            </iterate>
        </dynamic>
    </select>

    <insert id="insertUser" parameterMap="userMap">
        INSERT INTO users (<include refid="cols"/>) VALUES (#id#, #name#)
        <selectKey keyProperty="id" resultClass="long">
            SELECT MAX(id) + 1 FROM users
        </selectKey>
    </insert>
</sqlMap>"#;

    #[test]
    fn parse_collects_statements_fragments_and_parameter_maps() {
        let model = parse(MAP).unwrap();
        assert_eq!(model.namespace.as_deref(), Some("User"));
        assert!(model.statements.contains_key("selectAll"));
        assert!(model.statements.contains_key("insertUser"));
        assert_eq!(model.parameter_maps["userMap"].parameters.len(), 2);
        assert!(model.issues.is_empty(), "{:?}", model.issues);
    }

    #[test]
    fn select_key_is_kept_out_of_the_host_body() {
        let model = parse(MAP).unwrap();
        let insert = &model.statements["insertUser"];
        assert!(insert.select_key.is_some());
        let body = format!("{:?}", insert.body);
        assert!(!body.contains("MAX(id)"), "body: {body}");
    }

    #[test]
    fn root_must_be_sql_map() {
        let err = parse("<mapper/>").unwrap_err();
        assert_eq!(err.message_key, KEY_ROOT_INVALID);
    }

    #[test]
    fn malformed_xml_is_a_parse_error() {
        let err = parse("<sqlMap>").unwrap_err();
        assert_eq!(err.message_key, KEY_PARSE_FAILED);
    }

    #[test]
    fn missing_include_is_reported_and_placeholder_inserted() {
        let xml =
            r#"<sqlMap><select id="s">SELECT <include refid="nope"/> FROM t</select></sqlMap>"#;
        let model = parse(xml).unwrap();
        let issues = &model.statements["s"].issues;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].message_key, KEY_INCLUDE_NOT_FOUND);
        let body = format!("{:?}", model.statements["s"].body);
        assert!(body.contains("-- [mpsv] include not found: nope"));
    }

    #[test]
    fn include_cycle_does_not_hang() {
        let xml = r#"<sqlMap><sql id="a"><include refid="b"/></sql><sql id="b"><include refid="a"/></sql><select id="s">SELECT <include refid="a"/></select></sqlMap>"#;
        let model = parse(xml).unwrap();
        assert_eq!(
            model.statements["s"].issues[0].message_key,
            KEY_INCLUDE_CYCLE
        );
    }

    #[test]
    fn unknown_tag_is_degraded() {
        let xml = r#"<sqlMap><select id="s">A<weird>B</weird>C</select></sqlMap>"#;
        let model = parse(xml).unwrap();
        assert_eq!(model.statements["s"].issues[0].message_key, KEY_UNKNOWN_TAG);
    }

    #[test]
    fn tag_kind_table_is_complete() {
        for name in [
            "dynamic",
            "iterate",
            "isEmpty",
            "isEqual",
            "isGreaterEqual",
            "isGreaterThan",
            "isLessEqual",
            "isLessThan",
            "isNotEmpty",
            "isNotEqual",
            "isNotNull",
            "isNotParameterPresent",
            "isNotPropertyAvailable",
            "isNull",
            "isParameterPresent",
            "isPropertyAvailable",
        ] {
            assert!(TagKind::by_tag(name).is_some(), "{name}");
        }
        assert!(TagKind::by_tag("nope").is_none());
    }

    #[test]
    fn namespace_is_applied_only_to_undotted_refids() {
        assert_eq!(apply_namespace("cols", Some("User")), "User.cols");
        assert_eq!(apply_namespace("Other.cols", Some("User")), "Other.cols");
        assert_eq!(apply_namespace("cols", None), "cols");
    }

    #[test]
    fn remove_first_prepend_initial_state() {
        assert!(compute_remove_first_prepend(
            TagKind::Dynamic,
            true,
            None,
            None
        ));
        assert!(!compute_remove_first_prepend(
            TagKind::Dynamic,
            false,
            None,
            None
        ));
        assert!(compute_remove_first_prepend(
            TagKind::IsNotNull,
            false,
            Some("true"),
            None
        ));
        assert!(compute_remove_first_prepend(
            TagKind::IsNotNull,
            false,
            None,
            Some("true")
        ));
        assert!(!compute_remove_first_prepend(
            TagKind::IsNotNull,
            true,
            None,
            Some("true")
        ));
    }

    #[test]
    fn confidence_type_is_shared_with_the_xml_lane() {
        assert_eq!(
            serde_json::to_string(&Confidence::Partial).unwrap(),
            "\"partial\""
        );
    }
}
