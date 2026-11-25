//! Engine jar lifecycle, JSON protocol, degradation (T5-1~T5-3).
//!
//! Lane L8 owns this directory; see `docs/TASKS.md` P5.
//!
//! The engine (the IntelliJ repo's `mpsv-engine-<version>.jar`) is a separate,
//! JVM artifact. This repo pins it by **version + sha256 only** — the jar itself
//! is never copied here (`docs/ARCHITECTURE.md` §6). The runtime resolves the
//! jar, verifies its digest, launches it over stdin/stdout JSON lines, and
//! degrades to plain-text mode with a `message_key` whenever any step fails:
//! the plugin must never run a wrong or corrupt engine silently.
//!
//! ## Resolve order (shared by `scripts/verify-engine.sh` and the runtime)
//!
//! 1. `<fs.plugin_dir()>/mpsv-engine-<version>.jar` — where a previous download
//!    put it (install-time mode). Checked first so an approved copy wins.
//! 2. `<repo>/engine/mpsv-engine-<version>.jar` — developer / `link` runs.
//! 3. the `engine.downloadUrl` setting (default empty): fetched into (1) with
//!    `net:`, then verified before it is ever used.
//!
//! Every candidate must pass [`verify_sha256`] against
//! [`pinned_sha256_hex`] before [`start`] will spawn it.

pub mod lifecycle;
pub mod proto;
mod sha256;

pub use lifecycle::{
    Action, DEFAULT_TIMEOUTS, DegradeReason, EngineHandle, EnginePathSource, EngineProbe,
    EngineStatus, HealthMonitor, ResolvedEngine, Timeouts, dev_repo_candidates, jar_file_name,
    parse_health_version, resolve_engine_path, resolve_engine_path_with, retry_delays_ms, start,
    stop,
};
pub use proto::{
    EngineError, ErrorValue, ExplainRequest, Kind, PROTOCOL_VERSION, RenderKind, RenderRequest,
    Request, Response, SchemaRequest, Source, SourceRange,
};
pub use sha256::sha256_hex;

use std::sync::OnceLock;

/// The engine version pinned in `engine/ENGINE_VERSION`, baked at compile time.
///
/// Baked rather than read at runtime: the wasm sandbox has no way to reach the
/// repo's `engine/` directory, and the pin must be immutable for a given build.
pub fn pinned_version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION
        .get_or_init(|| {
            include_str!("../../engine/ENGINE_VERSION")
                .trim()
                .to_string()
        })
        .as_str()
}

/// The expected jar digest from `engine/ENGINE_VERSION.sha256` (first token),
/// baked at compile time and lowercased.
pub fn pinned_sha256_hex() -> &'static str {
    static SHA: OnceLock<String> = OnceLock::new();
    SHA.get_or_init(|| {
        include_str!("../../engine/ENGINE_VERSION.sha256")
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
    })
    .as_str()
}

/// Why a candidate jar was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sha256Error {
    /// The expected digest is not 64 hex characters (the pin file drifted).
    Malformed(String),
    /// The bytes hash to something other than the pin.
    Mismatch { expected: String, actual: String },
}

impl Sha256Error {
    /// The `message_key` the UI shows for this failure.
    pub fn message_key(&self) -> &'static str {
        match self {
            Sha256Error::Malformed(_) => "engine.sha256.malformed",
            Sha256Error::Mismatch { .. } => "engine.sha256.mismatch",
        }
    }
}

impl std::fmt::Display for Sha256Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Sha256Error::Malformed(got) => {
                write!(
                    f,
                    "engine sha256 pin is malformed: {got:?} (want 64 hex chars)"
                )
            }
            Sha256Error::Mismatch { expected, actual } => {
                write!(
                    f,
                    "engine sha256 mismatch: expected {expected}, got {actual}"
                )
            }
        }
    }
}

impl std::error::Error for Sha256Error {}

fn is_hex64(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Verify that `bytes` hashes to `expected_hex`.
///
/// The comparison is case-insensitive; a malformed pin is a hard error distinct
/// from a mismatch so the UI can tell "the pin file is broken" from "this jar is
/// the wrong one".
pub fn verify_sha256(bytes: &[u8], expected_hex: &str) -> Result<(), Sha256Error> {
    let expected = expected_hex.trim().to_ascii_lowercase();
    if !is_hex64(&expected) {
        return Err(Sha256Error::Malformed(expected_hex.trim().to_string()));
    }
    let actual = sha256_hex(bytes);
    if actual != expected {
        return Err(Sha256Error::Mismatch { expected, actual });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_accepts_the_known_digest_case_insensitively() {
        let hex = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert!(verify_sha256(b"abc", hex).is_ok());
        assert!(verify_sha256(b"abc", &hex.to_ascii_uppercase()).is_ok());
    }

    #[test]
    fn verify_rejects_a_bad_hash() {
        let wrong = "0000000000000000000000000000000000000000000000000000000000000000";
        match verify_sha256(b"abc", wrong) {
            Err(Sha256Error::Mismatch { expected, actual }) => {
                assert_eq!(expected, wrong);
                assert_eq!(
                    actual,
                    "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
                );
                assert_eq!(
                    Sha256Error::Mismatch { expected, actual }.message_key(),
                    "engine.sha256.mismatch"
                );
            }
            other => panic!("expected a mismatch, got {other:?}"),
        }
    }

    #[test]
    fn verify_rejects_a_malformed_pin() {
        match verify_sha256(b"abc", "deadbeef") {
            Err(Sha256Error::Malformed(got)) => assert_eq!(got, "deadbeef"),
            other => panic!("expected malformed, got {other:?}"),
        }
    }

    #[test]
    fn the_baked_pin_is_a_hex_digest_and_not_pending() {
        assert_eq!(pinned_version(), "0.1.0");
        assert!(
            is_hex64(pinned_sha256_hex()),
            "pin = {:?}",
            pinned_sha256_hex()
        );
    }

    /// The pinned digest and the real engine jar (recorded in the IntelliJ repo,
    /// cross-checked with `sha256sum`) agree. This guards the pin against a typo
    /// that would otherwise only show up as a `sha256.mismatch` at runtime.
    #[test]
    fn the_pin_matches_the_recorded_engine_jar_digest() {
        assert_eq!(
            pinned_sha256_hex(),
            "00af8534603994d6cf870f59cd9dfe48d8bbc16e983ff2c3e9966e4bfd58bc33"
        );
    }
}
