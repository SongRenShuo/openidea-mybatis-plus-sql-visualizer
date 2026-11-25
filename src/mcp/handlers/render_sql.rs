//! `sqlv_render_sql`: render one MyBatis mapper statement to SQL.
//!
//! Two input shapes: a single `xml` document (one file) or `documents` (several
//! files, so namespace-qualified includes resolve across them). The output is
//! the core `RenderOutput` unchanged; `unresolved` results are re-checked to
//! carry no SQL.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::core::xml::{Confidence, MapperSet, RenderOutput, render_statement};
use crate::mcp::contract::{invalid, parse_args, success, success_with};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    path: String,
    text: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    statement_id: String,
    #[serde(default)]
    xml: Option<String>,
    #[serde(default)]
    documents: Option<Vec<Document>>,
    #[serde(default)]
    params: Option<Value>,
}

pub fn run(arguments: &str) -> String {
    let args = match parse_args::<Args>(arguments) {
        Ok(args) => args,
        Err(failure) => return failure,
    };
    if args.statement_id.trim().is_empty() {
        return invalid("statement_id must not be empty");
    }
    let params = match args.params {
        None | Some(Value::Null) => json!({}),
        Some(value) => value,
    };
    match (args.xml, args.documents) {
        (Some(_), Some(_)) => invalid("provide exactly one of xml or documents"),
        (None, None) => invalid("provide one of xml or documents"),
        (Some(xml), None) => {
            let output = render_statement(&xml, &args.statement_id, &params);
            success(confidence_str(output.confidence), render_value(output))
        }
        (None, Some(documents)) => {
            if documents.iter().any(|doc| doc.path.trim().is_empty()) {
                return invalid("every document needs a non-empty path");
            }
            let docs: Vec<(&str, &str)> = documents
                .iter()
                .map(|doc| (doc.path.as_str(), doc.text.as_str()))
                .collect();
            let (set, issues) = MapperSet::load(&docs);
            let output = set.render(&args.statement_id, &params);
            success_with(
                confidence_str(output.confidence),
                render_value(output),
                &[("issues", json!(issues))],
            )
        }
    }
}

fn confidence_str(confidence: Confidence) -> &'static str {
    match confidence {
        Confidence::Exact => "exact",
        Confidence::Partial => "partial",
        Confidence::Unresolved => "unresolved",
    }
}

/// The core already keeps `unresolved` SQL empty; re-check so a future core
/// change cannot leak SQL through the MCP surface.
fn render_value(mut output: RenderOutput) -> Value {
    if output.confidence == Confidence::Unresolved {
        output.sql.clear();
        output.params.clear();
    }
    serde_json::to_value(&output).unwrap_or(Value::Null)
}
