//! `sqlv_mapper_check`: structural checks across mapper XML documents.
//!
//! [`MapperSet::load`] reports unparseable files, wrong roots and duplicate
//! statement ids. On top of that this handler walks the parsed documents for
//! the two reference problems the renderer would hit later:
//! - an `<include refid>` that resolves to no `<sql>` fragment (bare refids are
//!   file-local, qualified refids are namespace-qualified);
//! - a statement `resultMap` attribute that names no `<resultMap>`.
//!
//! No issue list means `exact`; any issue downgrades the envelope to `partial`.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::core::xml::include::KEY_INCLUDE_NOT_FOUND;
use crate::core::xml::xmltree::{Element, parse_document};
use crate::core::xml::{MapperSet, STATEMENT_TAGS};
use crate::mcp::contract::{invalid, parse_args, success};

const KEY_RESULT_MAP_NOT_FOUND: &str = "mapper.result_map.not_found";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    path: String,
    text: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    documents: Vec<Document>,
}

#[derive(Debug, Serialize)]
struct Issue {
    path: String,
    message_key: String,
    detail: Value,
}

struct MapperDoc {
    path: String,
    root: Element,
}

pub fn run(arguments: &str) -> String {
    let args = match parse_args::<Args>(arguments) {
        Ok(args) => args,
        Err(failure) => return failure,
    };
    if args.documents.is_empty() {
        return invalid("documents must contain at least one mapper file");
    }
    if args.documents.iter().any(|doc| doc.path.trim().is_empty()) {
        return invalid("every document needs a non-empty path");
    }

    let docs: Vec<(&str, &str)> = args
        .documents
        .iter()
        .map(|doc| (doc.path.as_str(), doc.text.as_str()))
        .collect();
    let (_set, load_issues) = MapperSet::load(&docs);
    let mut issues: Vec<Issue> = load_issues
        .into_iter()
        .map(|issue| Issue {
            path: issue.path,
            message_key: issue.message_key,
            detail: Value::String(issue.detail),
        })
        .collect();

    let mut parsed: Vec<MapperDoc> = Vec::new();
    let mut global_fragments: HashSet<String> = HashSet::new();
    let mut global_result_maps: HashSet<String> = HashSet::new();
    for doc in &args.documents {
        let Ok(root) = parse_document(&doc.text) else {
            // The load issues already contain `xml.file.unparseable`.
            continue;
        };
        if root.name != "mapper" {
            continue;
        }
        let namespace = root.attr("namespace").map(str::to_owned);
        if let Some(ns) = &namespace {
            for child in root.child_elements() {
                let id = child.attr_or_empty("id");
                if id.is_empty() {
                    continue;
                }
                match child.name.as_str() {
                    "sql" => {
                        global_fragments.insert(format!("{ns}.{id}"));
                    }
                    "resultMap" => {
                        global_result_maps.insert(format!("{ns}.{id}"));
                    }
                    _ => {}
                }
            }
        }
        parsed.push(MapperDoc {
            path: doc.path.clone(),
            root,
        });
    }

    let mut statement_count = 0usize;
    let mut result_map_count = 0usize;
    for doc in &parsed {
        let local_fragments: HashSet<&str> = doc
            .root
            .child_elements()
            .filter(|element| element.name == "sql")
            .filter_map(|element| element.attr("id"))
            .collect();
        let local_result_maps: HashSet<&str> = doc
            .root
            .child_elements()
            .filter(|element| element.name == "resultMap")
            .filter_map(|element| element.attr("id"))
            .collect();
        result_map_count += local_result_maps.len();

        for statement in doc
            .root
            .child_elements()
            .filter(|element| STATEMENT_TAGS.contains(&element.name.as_str()))
        {
            statement_count += 1;
            let statement_id = statement.attr_or_empty("id").to_owned();
            let mut includes: Vec<String> = Vec::new();
            collect_includes(statement, &mut includes);
            for refid in includes {
                if refid.is_empty() {
                    issues.push(Issue {
                        path: doc.path.clone(),
                        message_key: KEY_INCLUDE_NOT_FOUND.to_string(),
                        detail: json!({ "statement": statement_id, "refid": "" }),
                    });
                    continue;
                }
                let found = if refid.contains('.') {
                    global_fragments.contains(refid.as_str())
                } else {
                    local_fragments.contains(refid.as_str())
                };
                if !found {
                    issues.push(Issue {
                        path: doc.path.clone(),
                        message_key: KEY_INCLUDE_NOT_FOUND.to_string(),
                        detail: json!({ "statement": statement_id, "refid": refid }),
                    });
                }
            }
            if let Some(attr) = statement.attr("resultMap") {
                for name in attr.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    let found = if name.contains('.') {
                        global_result_maps.contains(name)
                    } else {
                        local_result_maps.contains(name)
                    };
                    if !found {
                        issues.push(Issue {
                            path: doc.path.clone(),
                            message_key: KEY_RESULT_MAP_NOT_FOUND.to_string(),
                            detail: json!({ "statement": statement_id, "resultMap": name }),
                        });
                    }
                }
            }
        }
    }

    let confidence = if issues.is_empty() {
        "exact"
    } else {
        "partial"
    };
    success(
        confidence,
        json!({
            "documents": args.documents.len(),
            "statements": statement_count,
            "result_maps": result_map_count,
            "issues": issues,
        }),
    )
}

fn collect_includes(element: &Element, out: &mut Vec<String>) {
    for child in element.child_elements() {
        if child.name == "include" {
            out.push(child.attr_or_empty("refid").to_string());
        }
        collect_includes(child, out);
    }
}
