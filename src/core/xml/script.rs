//! SqlNode tree and the script builder (T1-1).
//!
//! Mirrors MyBatis 3.5.x `XMLScriptBuilder.parseDynamicTags`:
//! - text / CDATA → `Text` when it contains a live `${}`, `StaticText` otherwise;
//! - elements dispatch by tag name; `when` / `otherwise` outside a `choose` fall
//!   back to `if` / plain contents (the reference implementation keeps that
//!   tolerant path);
//! - unknown elements are a build error (`xml.unknown_element`) — MyBatis
//!   raises "Unknown element" for `<include>`/`<sql>` inside a `<script>`;
//! - comments are ignored.
//!
//! `where` and `set` are trims with MyBatis' exact built-in overrides,
//! including the newline / carriage-return / tab variants for `where`.

use super::token::contains_token;
use super::xmltree::{Element, XmlNode};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildError {
    pub message_key: &'static str,
    pub detail: String,
}

impl BuildError {
    fn unknown(tag: &str) -> Self {
        Self {
            message_key: "xml.unknown_element",
            detail: format!("Unknown element <{tag}> in SQL statement."),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SqlNode {
    Mixed(Vec<SqlNode>),
    StaticText(String),
    Text(String),
    If {
        test: String,
        contents: Box<SqlNode>,
    },
    Choose {
        whens: Vec<(String, SqlNode)>,
        otherwise: Option<Box<SqlNode>>,
    },
    Foreach(ForeachNode),
    Trim(TrimNode),
    Bind {
        name: String,
        value: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ForeachNode {
    pub contents: Box<SqlNode>,
    pub collection_expression: String,
    pub separator: Option<String>,
    pub open: Option<String>,
    pub close: Option<String>,
    pub item: Option<String>,
    pub index: Option<String>,
    pub nullable: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrimNode {
    pub contents: Box<SqlNode>,
    pub prefix: Option<String>,
    pub prefix_overrides: Vec<String>,
    pub suffix: Option<String>,
    pub suffix_overrides: Vec<String>,
}

/// Parses a statement element (or a `<script>` root) into a node tree.
pub fn build(statement: &Element) -> Result<SqlNode, BuildError> {
    Ok(SqlNode::Mixed(parse_children(statement)?))
}

fn parse_children(element: &Element) -> Result<Vec<SqlNode>, BuildError> {
    let mut contents = Vec::new();
    for child in &element.children {
        match child {
            XmlNode::Comment(_) => {}
            XmlNode::Text(text) => {
                if contains_token(text, "${", "}") {
                    contents.push(SqlNode::Text(text.clone()));
                } else {
                    contents.push(SqlNode::StaticText(text.clone()));
                }
            }
            XmlNode::Element(child) => parse_element(child, &mut contents)?,
        }
    }
    Ok(contents)
}

fn parse_element(child: &Element, contents: &mut Vec<SqlNode>) -> Result<(), BuildError> {
    match child.name.as_str() {
        "trim" => {
            contents.push(SqlNode::Trim(TrimNode {
                contents: Box::new(SqlNode::Mixed(parse_children(child)?)),
                prefix: non_empty(child.attr_or_empty("prefix")),
                prefix_overrides: split_overrides(child.attr_or_empty("prefixOverrides")),
                suffix: non_empty(child.attr_or_empty("suffix")),
                suffix_overrides: split_overrides(child.attr_or_empty("suffixOverrides")),
            }));
        }
        "where" => {
            contents.push(SqlNode::Trim(TrimNode {
                contents: Box::new(SqlNode::Mixed(parse_children(child)?)),
                prefix: Some("WHERE".to_string()),
                prefix_overrides: [
                    "AND ", "OR ", "AND\n", "OR\n", "AND\r", "OR\r", "AND\t", "OR\t",
                ]
                .iter()
                .map(|s| s.to_string())
                .collect(),
                suffix: None,
                suffix_overrides: Vec::new(),
            }));
        }
        "set" => {
            contents.push(SqlNode::Trim(TrimNode {
                contents: Box::new(SqlNode::Mixed(parse_children(child)?)),
                prefix: Some("SET".to_string()),
                prefix_overrides: vec![",".to_string()],
                suffix: None,
                suffix_overrides: vec![",".to_string()],
            }));
        }
        "foreach" => {
            contents.push(SqlNode::Foreach(ForeachNode {
                contents: Box::new(SqlNode::Mixed(parse_children(child)?)),
                collection_expression: child.attr_or_empty("collection").to_string(),
                separator: non_empty(child.attr_or_empty("separator")),
                open: non_empty(child.attr_or_empty("open")),
                close: non_empty(child.attr_or_empty("close")),
                item: non_empty(child.attr_or_empty("item")),
                index: non_empty(child.attr_or_empty("index")),
                nullable: child.attr_or_empty("nullable") == "true",
            }));
        }
        "if" => {
            contents.push(SqlNode::If {
                test: child.attr_or_empty("test").to_string(),
                contents: Box::new(SqlNode::Mixed(parse_children(child)?)),
            });
        }
        "choose" => {
            contents.push(parse_choose(child)?);
        }
        "when" => {
            contents.push(SqlNode::If {
                test: child.attr_or_empty("test").to_string(),
                contents: Box::new(SqlNode::Mixed(parse_children(child)?)),
            });
        }
        "otherwise" => {
            contents.push(SqlNode::Mixed(parse_children(child)?));
        }
        "bind" => {
            contents.push(SqlNode::Bind {
                name: child.attr_or_empty("name").to_string(),
                value: child.attr_or_empty("value").to_string(),
            });
        }
        other => return Err(BuildError::unknown(other)),
    }
    Ok(())
}

fn parse_choose(child: &Element) -> Result<SqlNode, BuildError> {
    let mut whens = Vec::new();
    let mut otherwise = None;
    for grandchild in child.child_elements() {
        match grandchild.name.as_str() {
            "when" => whens.push((
                grandchild.attr_or_empty("test").to_string(),
                SqlNode::Mixed(parse_children(grandchild)?),
            )),
            "otherwise" => {
                let node = SqlNode::Mixed(parse_children(grandchild)?);
                if otherwise.is_some() {
                    return Err(BuildError {
                        message_key: "xml.choose.multiple_otherwise",
                        detail: "Too many default (otherwise) elements in choose statement."
                            .to_string(),
                    });
                }
                otherwise = Some(Box::new(node));
            }
            _ => {}
        }
    }
    Ok(SqlNode::Choose { whens, otherwise })
}

fn non_empty(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn split_overrides(raw: &str) -> Vec<String> {
    if raw.trim().is_empty() {
        Vec::new()
    } else {
        raw.split('|').map(|s| s.to_string()).collect()
    }
}

/// Builds a `<script>` body used by annotations. `<include>` / `<sql>` are
/// rejected exactly like MyBatis 3.5.19 does inside a script.
pub fn build_script(script_text: &str) -> Result<SqlNode, BuildError> {
    let root = super::xmltree::parse_document(script_text).map_err(|e| BuildError {
        message_key: "xml.parse.failed",
        detail: e.message,
    })?;
    if root.name != "script" {
        return Err(BuildError {
            message_key: "xml.script.root_invalid",
            detail: format!(
                "annotation SQL starts with <script> but root is <{}>",
                root.name
            ),
        });
    }
    build(&root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::xml::xmltree::parse_document;

    fn statement(body: &str) -> SqlNode {
        let text = format!("<select id=\"s\">{body}</select>");
        build(&parse_document(&text).unwrap()).unwrap()
    }

    #[test]
    fn text_becomes_static_or_dynamic() {
        let node = statement("SELECT 1");
        assert_eq!(
            node,
            SqlNode::Mixed(vec![SqlNode::StaticText("SELECT 1".to_string())])
        );
        let node = statement("SELECT ${col}");
        assert_eq!(
            node,
            SqlNode::Mixed(vec![SqlNode::Text("SELECT ${col}".to_string())])
        );
    }

    #[test]
    fn escaped_dollar_is_not_dynamic() {
        let node = statement("SELECT \\${col}");
        assert_eq!(
            node,
            SqlNode::Mixed(vec![SqlNode::StaticText("SELECT \\${col}".to_string())])
        );
    }

    #[test]
    fn comments_are_dropped() {
        let node = statement("SELECT 1 <!-- note --> FROM t");
        match node {
            SqlNode::Mixed(children) => assert_eq!(children.len(), 2),
            _ => panic!("unexpected node"),
        }
    }

    #[test]
    fn where_and_set_carry_builtin_overrides() {
        let node = statement("<where> AND a = 1 </where><set> a = 1, </set>");
        let SqlNode::Mixed(children) = node else {
            panic!("mixed expected")
        };
        let SqlNode::Trim(where_node) = &children[0] else {
            panic!("trim expected")
        };
        assert_eq!(where_node.prefix.as_deref(), Some("WHERE"));
        assert_eq!(where_node.prefix_overrides.len(), 8);
        let SqlNode::Trim(set_node) = &children[1] else {
            panic!("trim expected")
        };
        assert_eq!(set_node.prefix.as_deref(), Some("SET"));
        assert_eq!(set_node.suffix_overrides, vec![",".to_string()]);
    }

    #[test]
    fn choose_accepts_one_otherwise_and_rejects_two() {
        assert!(
            build(
                &parse_document(
                    "<s><choose><when test=\"a\">1</when><otherwise>2</otherwise></choose></s>"
                )
                .unwrap()
            )
            .is_ok()
        );
        let err = build(
            &parse_document(
                "<s><choose><otherwise>1</otherwise><otherwise>2</otherwise></choose></s>",
            )
            .unwrap(),
        )
        .unwrap_err();
        assert_eq!(err.message_key, "xml.choose.multiple_otherwise");
    }

    #[test]
    fn unknown_elements_are_rejected() {
        let err = build(&parse_document("<s><include refid=\"x\"/></s>").unwrap()).unwrap_err();
        assert_eq!(err.message_key, "xml.unknown_element");
    }
}
