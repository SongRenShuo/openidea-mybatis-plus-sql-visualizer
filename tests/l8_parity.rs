//! L8 pairing: crate-independent engine-jar end-to-end test (T5-7).
//!
//! The five fixed-fixture *projection* categories (≥10 cases each) live in
//! `src/execute/parity.rs` and run as ordinary `cargo test` unit tests. They are
//! not here because the package's `[lib] crate-type` is `["cdylib"]` only: an
//! integration test under `tests/` cannot link the crate (`E0433: unlinked
//! crate`), and L8 is not allowed to change `Cargo.toml` (only the controller
//! edits it, via registration). This file only uses `std` + `serde_json`, so it
//! compiles and runs independently of that limitation.
//!
//! ## The ignored end-to-end test
//!
//! `jar_stdio_round_trip` drives the real engine jar over its `--stdio` JSON-line
//! protocol (see `src/sidecar/lifecycle.rs`). Run it with:
//!
//! ```text
//! MPSV_ENGINE_JAR=<path/to/mpsv-engine-0.1.0.jar> cargo test --test l8_parity -- --ignored
//! ```
//!
//! It is `#[ignore]`d because the pinned jar currently exposes `EngineApi` as a
//! library but has **no `--stdio` entry point / `Main-Class`** — that dispatcher
//! is a P5 jar-side follow-up. Forcing it against such a jar fails loudly, which
//! is intentional: an engine test must never pass without an engine.

/// End-to-end handshake + `check` round-trip over stdin/stdout JSON lines.
#[test]
#[ignore = "requires MPSV_ENGINE_JAR and a jar built with a --stdio dispatcher (pending)"]
fn jar_stdio_round_trip() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};

    let jar = std::env::var("MPSV_ENGINE_JAR").expect("set MPSV_ENGINE_JAR to the engine jar");
    let java = std::env::var("JAVA_HOME")
        .map(|home| format!("{home}/bin/java"))
        .unwrap_or_else(|_| "java".to_string());

    let mut child = Command::new(java)
        .args(["-jar", &jar, "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn engine");

    {
        let stdin = child.stdin.as_mut().expect("stdin");
        writeln!(stdin, "{{\"protocol\":1,\"kind\":\"health\"}}").unwrap();
        writeln!(
            stdin,
            "{{\"protocol\":1,\"kind\":\"check\",\"source\":{{\"path\":\"x\",\"text\":\"SELECT 1\"}}}}"
        )
        .unwrap();
    }

    let stdout = child.stdout.take().expect("stdout");
    let mut lines = BufReader::new(stdout).lines();

    let health: serde_json::Value =
        serde_json::from_str(&lines.next().expect("health line").unwrap()).unwrap();
    assert_eq!(health["ok"], true, "health: {health}");
    assert_eq!(health["version"], "0.1.0");

    let check: serde_json::Value =
        serde_json::from_str(&lines.next().expect("check line").unwrap()).unwrap();
    assert_eq!(check["ok"], true, "check: {check}");
    assert_eq!(check["statementCount"], 1);

    let _ = child.kill();
}
