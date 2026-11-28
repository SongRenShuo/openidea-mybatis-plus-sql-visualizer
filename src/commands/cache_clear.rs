//! `cache-clear`: the About page's "Clear caches" action.
//!
//! The reference's `PluginCacheManager.clearAll` clears runtime caches only —
//! entity resolution, reverse lookup, preview parsing — and never the captured
//! SQL files, the preview SQL history or the parameter history. This port's
//! runtime caches are the process-wide Java/XML method and statement indexes
//! (the host refills them from the open documents) and the pending
//! capture-start slot. The log panel's contents, its SQL history snapshots and
//! the suppression rules are user data and stay.

use std::fmt::Write as _;

use openidea_plugin::{Error, Invocation};

use crate::capture::bridge;
use crate::commands::common;
use crate::indexer;

/// Matches `[[contributes.commands]] id` in `plugin.toml`.
pub const ID: &str = "cache-clear";

/// Dispatch entry.
pub fn run(_ctx: &Invocation, _arg: Option<&str>) -> Result<Option<String>, Error> {
    // Snapshot before clearing, so the Output line says what was thrown away.
    let stats = indexer::index_stats();
    indexer::clear_indexes();
    bridge::clear_waiting();

    let mut out = String::new();
    let _ = writeln!(
        out,
        "-- cache-clear: cleared the runtime indexes (java {} files / {} methods, xml {} files / {} statements)",
        stats.java_files, stats.java_methods, stats.xml_files, stats.xml_statements
    );
    let _ = writeln!(
        out,
        "-- cache-clear: cleared the pending capture-start slot"
    );
    let _ = writeln!(
        out,
        "-- cache-clear: captured SQL files, the log panel contents and SQL history are user data and were not touched"
    );
    common::emit_showing(&out)?;
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    #[test]
    fn commands_cache_clear_reports_and_empties_the_indexes() {
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
        assert!(indexer::index_stats().java_methods > 0);

        run(&host.invocation(), None).unwrap();
        let text = &host.output()[common::CHANNEL];
        assert!(text.contains("java 1 files / 1 methods"), "{text}");
        assert!(text.contains("xml 1 files / 1 statements"), "{text}");
        assert!(text.contains("capture-start slot"), "{text}");
        assert!(text.contains("were not touched"), "{text}");
        assert_eq!(indexer::index_stats().java_methods, 0);
        assert_eq!(indexer::index_stats().xml_statements, 0);
        indexer::clear_indexes();
    }
}
