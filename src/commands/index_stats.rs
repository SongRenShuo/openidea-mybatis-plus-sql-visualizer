//! `index-stats`: report what the process-wide Java/XML indexes hold, and what
//! building them cost.
//!
//! The indexes are fed by the document indexer on project open and on every
//! change; this command is the reading a big project needs before anyone can
//! claim the plugin is cheap there. It never triggers indexing itself.

use std::fmt::Write as _;

use openidea_plugin::{Error, Invocation};

use crate::commands::common;
use crate::indexer;

/// Matches `[[contributes.commands]] id` in `plugin.toml`.
pub const ID: &str = "index-stats";

/// Dispatch entry.
pub fn run(_ctx: &Invocation, _arg: Option<&str>) -> Result<Option<String>, Error> {
    common::emit(&report(&indexer::index_stats()))?;
    Ok(None)
}

fn report(stats: &indexer::IndexStats) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "-- index-stats: java {} files / {} methods (skipped {}), xml {} files / {} statements (skipped {})",
        stats.java_files,
        stats.java_methods,
        stats.java_skipped,
        stats.xml_files,
        stats.xml_statements,
        stats.xml_skipped
    );
    let _ = writeln!(
        out,
        "-- cost: {} documents, {:.1} MiB read, {:.0} ms total, {:.1} ms slowest document, {:.1} ms average",
        stats.documents,
        stats.bytes as f64 / (1024.0 * 1024.0),
        stats.micros as f64 / 1000.0,
        stats.slowest_micros as f64 / 1000.0,
        if stats.documents == 0 {
            0.0
        } else {
            stats.micros as f64 / 1000.0 / stats.documents as f64
        }
    );
    if stats.java_skipped + stats.xml_skipped > 0 {
        let _ = writeln!(
            out,
            "-- note: files beyond the index budget are skipped; rows from them have no jump target"
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    #[test]
    fn commands_index_stats_reports_counts_and_cost() {
        let host = MockHost::install();
        indexer::clear_indexes();
        indexer::index_source(
            "/project/src/main/java/com/acme/UserMapper.java",
            "package com.acme;\npublic interface UserMapper {\n  int findById(long id);\n}\n",
        );
        indexer::index_source(
            "/project/src/main/resources/mapper/UserMapper.xml",
            "<mapper namespace=\"com.acme.UserMapper\"><select id=\"findById\">select 1</select></mapper>",
        );

        run(&host.invocation(), None).unwrap();
        let text = &host.output()[common::CHANNEL];
        assert!(text.contains("java 1 files / 1 methods"), "{text}");
        assert!(text.contains("xml 1 files / 1 statements"), "{text}");
        assert!(text.contains("2 documents"), "{text}");
        assert!(text.contains("MiB read"), "{text}");
        assert!(text.contains("slowest document"), "{text}");
        assert!(!text.contains("beyond the index budget"), "{text}");
        indexer::clear_indexes();
    }
}
