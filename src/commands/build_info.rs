//! `build-info`: the About page's "Build number" action.
//!
//! The reference reads an `MPV-Build` millisecond timestamp out of its jar
//! manifest — build-time injected, unique per rebuild, so same-day rebuilds of
//! one version can be told apart in bug reports. This port has no jar; `build.rs`
//! bakes the same kind of stamp into the wasm at compile time and this command
//! prints it. The settings row's static identity (plugin version + pinned
//! engine digest) complements it: that one survives, this one identifies the
//! exact rebuild.

use std::fmt::Write as _;

use openidea_plugin::{Error, Invocation};

use crate::commands::common;
use crate::views::log_panel::format_epoch_ms;

/// Matches `[[contributes.commands]] id` in `plugin.toml`.
pub const ID: &str = "build-info";

/// Dispatch entry.
pub fn run(_ctx: &Invocation, _arg: Option<&str>) -> Result<Option<String>, Error> {
    let stamp: i64 = env!("MPV_BUILD").parse().unwrap_or(0);
    let mut out = String::new();
    let _ = writeln!(out, "-- MPV-Build: {} ({})", stamp, format_epoch_ms(stamp));
    let _ = writeln!(out, "-- version: {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(
        out,
        "-- report this number when filing an issue so same-day rebuilds can be told apart"
    );
    common::emit_showing(&out)?;
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    #[test]
    fn commands_build_info_prints_the_stamp_and_version() {
        let host = MockHost::install();
        run(&host.invocation(), None).unwrap();
        let text = &host.output()[common::CHANNEL];
        assert!(text.contains("MPV-Build:"), "{text}");
        assert!(
            text.contains(&format!("version: {}", env!("CARGO_PKG_VERSION"))),
            "{text}"
        );
        // The stamp the build script baked in must be a real timestamp, and the
        // rendered line shows its UTC wall clock.
        let stamp: i64 = env!("MPV_BUILD").parse().unwrap_or(0);
        assert!(stamp > 0, "build.rs must bake a positive stamp");
        assert!(text.contains(&format!("MPV-Build: {stamp}")), "{text}");
    }
}
