//! `sqlv_history_search`: literal search over caller-supplied log text.
//!
//! The platform exposes no IDE history API and the plugin has no persisted
//! history, so the tool only searches the `text` argument. Matching is a
//! case-insensitive substring test per line; `limit` caps the returned lines and
//! `truncated` reports whether more matches existed.

use serde::Deserialize;
use serde_json::json;

use crate::mcp::contract::{invalid, parse_args, success};

const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 200;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    text: String,
    query: String,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    DEFAULT_LIMIT
}

pub fn run(arguments: &str) -> String {
    let args = match parse_args::<Args>(arguments) {
        Ok(args) => args,
        Err(failure) => return failure,
    };
    if args.text.trim().is_empty() {
        return invalid("text must not be empty");
    }
    if args.query.trim().is_empty() {
        return invalid("query must not be empty");
    }
    if args.limit == 0 || args.limit > MAX_LIMIT {
        return invalid(format!("limit must be between 1 and {MAX_LIMIT}"));
    }

    let needle = args.query.to_lowercase();
    let mut matches = Vec::new();
    let mut total_matches = 0usize;
    for (index, line) in args.text.lines().enumerate() {
        if line.to_lowercase().contains(&needle) {
            total_matches += 1;
            if matches.len() < args.limit {
                matches.push(json!({ "line_no": index + 1, "text": line }));
            }
        }
    }
    let returned = matches.len();
    success(
        "exact",
        json!({
            "query": args.query,
            "total_matches": total_matches,
            "returned": returned,
            "truncated": total_matches > returned,
            "matches": matches,
        }),
    )
}
