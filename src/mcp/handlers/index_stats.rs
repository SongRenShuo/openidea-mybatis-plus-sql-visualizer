//! `sqlv_index_stats`: what the plugin's own indexes hold and what building them
//! cost.
//!
//! This is the reading a big project needs before anyone can claim the plugin is
//! cheap there: the counts are the Java/XML indexes the log panel queries, and
//! the cost block is wall time spent inside the document indexer, kept apart
//! from the IDE's own scanning and language-server work.

use serde::Deserialize;
use serde_json::json;

use crate::indexer;
use crate::mcp::contract::{parse_args, success};

/// No arguments: the indexes are process-wide state.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {}

pub fn run(arguments: &str) -> String {
    if let Err(failure) = parse_args::<Args>(arguments) {
        return failure;
    }

    let stats = indexer::index_stats();
    let average = if stats.documents == 0 {
        0.0
    } else {
        stats.micros as f64 / 1000.0 / stats.documents as f64
    };
    success(
        "exact",
        json!({
            "java": {
                "files": stats.java_files,
                "methods": stats.java_methods,
                "skipped": stats.java_skipped,
            },
            "xml": {
                "files": stats.xml_files,
                "statements": stats.xml_statements,
                "skipped": stats.xml_skipped,
            },
            "cost": {
                "documents": stats.documents,
                "bytes": stats.bytes,
                "mib": (stats.bytes as f64 / (1024.0 * 1024.0) * 10.0).round() / 10.0,
                "total_ms": (stats.micros as f64 / 100.0).round() / 10.0,
                "slowest_document_ms": (stats.slowest_micros as f64 / 100.0).round() / 10.0,
                "average_document_ms": (average * 100.0).round() / 100.0,
            },
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_counts_and_cost_for_a_fresh_index() {
        indexer::clear_indexes();
        indexer::index_source(
            "/p/src/main/java/com/acme/UserMapper.java",
            "package com.acme;\npublic interface UserMapper {\n  int findById(long id);\n}\n",
        );
        indexer::index_source(
            "/p/src/main/resources/mapper/UserMapper.xml",
            "<mapper namespace=\"com.acme.UserMapper\"><select id=\"findById\">select 1</select></mapper>",
        );
        let value: serde_json::Value = serde_json::from_str(&run("{}")).expect("JSON");
        assert_eq!(value["data"]["java"]["files"], 1);
        assert_eq!(value["data"]["java"]["methods"], 1);
        assert_eq!(value["data"]["xml"]["files"], 1);
        assert_eq!(value["data"]["xml"]["statements"], 1);
        assert_eq!(value["data"]["cost"]["documents"], 2);
        assert!(value["data"]["cost"]["bytes"].as_u64().unwrap() > 0);
        assert_eq!(value["confidence"], "exact");
        indexer::clear_indexes();
    }

    #[test]
    fn refuses_unknown_fields() {
        let value: serde_json::Value =
            serde_json::from_str(&run(r#"{"verbose":true}"#)).expect("JSON");
        assert_eq!(value["ok"], false);
    }
}
