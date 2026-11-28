//! Build stamp (MPV-Build). The IntelliJ edition injects a build-time
//! millisecond timestamp into the jar manifest so same-day rebuilds of one
//! version can be told apart; this port bakes the same kind of stamp into the
//! wasm here and the `build-info` command reads it with `env!`. No
//! `rerun-if-*` directive is printed on purpose: without one cargo reruns this
//! script whenever any file in the package changes, so every rebuild gets a
//! fresh stamp.

use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis())
        .unwrap_or(0);
    println!("cargo:rustc-env=MPV_BUILD={millis}");
}
