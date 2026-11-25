//! `process:java` engine lifecycle: path resolution, spawn, handshake, health,
//! retry/degradation, shutdown.
//!
//! ## Transport: stdin/stdout JSON lines, not a TCP port
//!
//! `process:java` runs the engine on the host machine, but the plugin itself is
//! a wasm sandbox: it has no socket API (`docs/ARCHITECTURE.md` §1 — "沙箱：无裸
//! socket"). A jar listening on `--port 0` would be unreachable. The only
//! channel the plugin has to a child process is its stdin/stdout, so the engine
//! is started as `java -jar <jar> --stdio` and speaks one JSON object per line:
//!
//! ```text
//! -> {"protocol":1,"kind":"health"}
//! <- {"ok":true,"version":"0.1.0"}
//! ```
//!
//! ## What is pure vs host-facing
//!
//! Everything that can be reasoned about is a pure function with unit tests:
//! the health state machine ([`HealthMonitor`]), the retry schedule
//! ([`retry_delays_ms`]), candidate ordering
//! ([`dev_repo_candidates`]), handshake parsing
//! ([`parse_health_version`]) and path resolution against a probe
//! ([`resolve_engine_path_with`]). Only [`start`] / [`stop`] touch `host::`.
//!
//! ## The jar-side entry point is not present yet
//!
//! The pinned jar (see `engine/`) currently exposes `EngineApi` as a library and
//! a `SmokeMain` demo, but no `--stdio` dispatcher and no `Main-Class`
//! manifest attribute. Until the IntelliJ repo adds one (T5-3's jar extension,
//! blocked here by the main-repo gate — see `docs/registration/L8.md`), [`start`]
//! resolves and verifies the jar, then degrades with
//! [`DegradeReason::HandshakeFailed`]. That is deliberate: the plugin must never
//! pretend an engine answered when it did not.

use openidea_plugin::HttpRequest;
use openidea_plugin::host::{self, Child, Command, ProcessOptions};

use super::{Sha256Error, pinned_sha256_hex, verify_sha256};

// ---------------------------------------------------------------------------
// Public model
// ---------------------------------------------------------------------------

/// Where a resolved jar came from (drives diagnostics, not behaviour).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnginePathSource {
    /// `<fs.plugin_dir()>/mpsv-engine-<version>.jar` — installed or downloaded.
    PluginDir,
    /// A developer `link` run that found `engine/mpsv-engine-<version>.jar`.
    DevRepo,
    /// Fetched from `engine.downloadUrl` on this run.
    Downloaded,
}

/// A jar selected for launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEngine {
    pub path: String,
    pub source: EnginePathSource,
    pub version: String,
}

/// Lifecycle state of the engine for one plugin instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineStatus {
    Stopped,
    Starting,
    Ready { version: String },
    Degraded { reason: DegradeReason },
}

impl EngineStatus {
    pub fn is_ready(&self) -> bool {
        matches!(self, EngineStatus::Ready { .. })
    }

    /// The `message_key` to show, or `None` when everything is fine.
    pub fn message_key(&self) -> Option<&'static str> {
        match self {
            EngineStatus::Degraded { reason } => Some(reason.message_key()),
            _ => None,
        }
    }
}

/// Why the engine is unavailable. Each variant maps to a localization key the
/// UI shows (the plugin owns the text; the engine side never carries prose).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DegradeReason {
    /// No local jar and no download URL — plain-text mode only.
    BinaryMissing,
    /// `engine.downloadUrl` is empty, so fetching was not attempted.
    DownloadDisabled,
    /// The fetch or the write into `plugin_dir` failed.
    DownloadFailed(String),
    /// The pin file itself is malformed (64 hex expected).
    Sha256Malformed(String),
    /// The bytes are not the pinned engine.
    Sha256Mismatch { expected: String, actual: String },
    /// `java` could not be started at all (approved `process:java`, PATH…).
    SpawnFailed(String),
    /// The process ran but never produced the health reply.
    HandshakeFailed(String),
    /// The process reported a version other than the pin.
    VersionMismatch { expected: String, got: String },
    /// The process died the maximum number of times.
    Crashed,
}

impl DegradeReason {
    /// Localization key; see `docs/registration/L8.md` for the text.
    pub fn message_key(&self) -> &'static str {
        match self {
            DegradeReason::BinaryMissing => "engine.download.disabled",
            DegradeReason::DownloadDisabled => "engine.download.disabled",
            DegradeReason::DownloadFailed(_) => "engine.download.failed",
            DegradeReason::Sha256Malformed(_) => "engine.sha256.malformed",
            DegradeReason::Sha256Mismatch { .. } => "engine.sha256.mismatch",
            DegradeReason::SpawnFailed(_) => "engine.spawn.failed",
            DegradeReason::HandshakeFailed(_) => "engine.handshake.failed",
            DegradeReason::VersionMismatch { .. } => "engine.version.mismatch",
            DegradeReason::Crashed => "engine.crashed",
        }
    }

    fn from_sha(err: Sha256Error) -> Self {
        match err {
            Sha256Error::Malformed(got) => DegradeReason::Sha256Malformed(got),
            Sha256Error::Mismatch { expected, actual } => {
                DegradeReason::Sha256Mismatch { expected, actual }
            }
        }
    }
}

/// Health state machine (pure).
///
/// A crash or a failed health check is retried up to `max_restarts` times; after
/// that the engine stays degraded. Fatal startup errors (missing jar, bad hash,
/// wrong version, spawn failure) are **not** retried — retrying cannot fix them
/// and would only delay the text-mode fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthMonitor {
    max_restarts: u32,
    restarts: u32,
    status: EngineStatus,
}

/// What the caller should do next after a transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Spawn (or respawn) the process.
    Restart,
    /// Stop trying; show the degraded UI.
    Degrade,
}

impl HealthMonitor {
    pub fn new(max_restarts: u32) -> Self {
        HealthMonitor {
            max_restarts,
            restarts: 0,
            status: EngineStatus::Stopped,
        }
    }

    /// The plugin instance starts using the engine.
    pub fn start(&mut self) -> Action {
        self.status = EngineStatus::Starting;
        Action::Restart
    }

    /// The handshake succeeded and reported `version`.
    pub fn started(&mut self, version: &str) {
        self.status = EngineStatus::Ready {
            version: version.to_string(),
        };
    }

    /// The process crashed (or the health reply failed). Retries until the cap.
    pub fn crash(&mut self) -> Action {
        if self.restarts < self.max_restarts {
            self.restarts += 1;
            self.status = EngineStatus::Starting;
            Action::Restart
        } else {
            self.status = EngineStatus::Degraded {
                reason: DegradeReason::Crashed,
            };
            Action::Degrade
        }
    }

    /// A fatal error: degrade immediately, no retry.
    pub fn fatal(&mut self, reason: DegradeReason) -> Action {
        self.status = EngineStatus::Degraded { reason };
        Action::Degrade
    }

    pub fn status(&self) -> &EngineStatus {
        &self.status
    }

    pub fn restarts(&self) -> u32 {
        self.restarts
    }
}

/// Timeouts in milliseconds. `cold_start_ms` is the handshake budget (target
/// ≤2000 ms); `request_ms` is the per-request budget used by the protocol layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    pub cold_start_ms: u32,
    pub request_ms: u32,
}

pub const DEFAULT_TIMEOUTS: Timeouts = Timeouts {
    cold_start_ms: 2_000,
    request_ms: 5_000,
};

/// Exponential backoff (ms) for up to `max` restarts: 250, 500, 1000, …
pub fn retry_delays_ms(max: u32) -> Vec<u32> {
    (0..max)
        .map(|i| 250u32.saturating_mul(1 << i.min(6)))
        .collect()
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// The jar's file name for a version.
pub fn jar_file_name(version: &str) -> String {
    format!("mpsv-engine-{version}.jar")
}

fn join(dir: &str, name: &str) -> String {
    format!("{}/{}", dir.trim_end_matches(['/', '\\']), name)
}

/// Candidate dev-repo paths, walking up from `start_dir`.
///
/// `link` runs put the plugin's data somewhere under the repo (e.g.
/// `<repo>/build/plugin-data`), so an ancestor's `engine/` directory holds the
/// developer jar. Returns `<anc>/engine/<jar_name>` from the deepest ancestor
/// outward. Pure, so the ordering is unit-tested.
pub fn dev_repo_candidates(start_dir: &str, jar_name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut dir = Some(start_dir.trim_end_matches(['/', '\\']).to_string());
    while let Some(d) = dir {
        if d.is_empty() {
            break;
        }
        // Join without doubling the separator at the filesystem root ("/" -> "/engine/..").
        let base = d.trim_end_matches(['/', '\\']);
        out.push(if base.is_empty() {
            format!("/engine/{jar_name}")
        } else {
            format!("{base}/engine/{jar_name}")
        });
        dir = parent_of(&d);
    }
    out
}

fn parent_of(path: &str) -> Option<String> {
    let p = path.trim_end_matches(['/', '\\']);
    if p.is_empty() {
        return None; // filesystem root: stop the walk instead of looping on "/"
    }
    match p.rfind(['/', '\\']) {
        Some(0) => Some("/".to_string()), // "/x" -> "/"
        Some(i) => Some(p[..i].to_string()),
        None => None, // "C:" or a bare name
    }
}

/// Parse the engine's health reply (one JSON object line) for its version.
///
/// Tolerant by design: the engine is a separate artifact with its own
/// serializer, so we only require `ok: true` and a `version` string.
pub fn parse_health_version(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if value.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            continue;
        }
        if let Some(version) = value.get("version").and_then(|v| v.as_str()) {
            return Some(version.to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Resolution (probe-driven so tiers are testable without real files)
// ---------------------------------------------------------------------------

/// The minimal filesystem/network surface resolution needs.
pub trait EngineProbe {
    fn plugin_dir(&self) -> Option<String>;
    fn exists(&self, path: &str) -> bool;
    fn download(&self, url: &str) -> Option<Vec<u8>>;
    fn store(&self, path: &str, bytes: &[u8]) -> bool;
}

/// Resolve which jar to use, in the documented order, against any probe.
///
/// `expected_sha` is verified **before** a downloaded jar is enabled; local
/// tiers are verified by [`start`] after this returns (so a path can be shown
/// even when the hash is wrong, with the mismatch reported by the caller).
pub fn resolve_engine_path_with<P: EngineProbe>(
    probe: &P,
    version: &str,
    expected_sha: &str,
    download_url: Option<&str>,
) -> Result<ResolvedEngine, DegradeReason> {
    let name = jar_file_name(version);
    let plugin_dir = probe.plugin_dir().ok_or(DegradeReason::BinaryMissing)?;

    // 1) previously installed / downloaded copy
    let in_plugin_dir = join(&plugin_dir, &name);
    if probe.exists(&in_plugin_dir) {
        return Ok(ResolvedEngine {
            path: in_plugin_dir,
            source: EnginePathSource::PluginDir,
            version: version.to_string(),
        });
    }

    // 2) developer repo (`link`)
    for candidate in dev_repo_candidates(&plugin_dir, &name) {
        if probe.exists(&candidate) {
            return Ok(ResolvedEngine {
                path: candidate,
                source: EnginePathSource::DevRepo,
                version: version.to_string(),
            });
        }
    }
    for candidate in dev_repo_candidates("engine", &name) {
        // A relative walk from the working directory ("engine/mpsv-engine-*.jar").
        if probe.exists(&candidate) {
            return Ok(ResolvedEngine {
                path: candidate,
                source: EnginePathSource::DevRepo,
                version: version.to_string(),
            });
        }
    }

    // 3) download into plugin_dir, verified before use
    match download_url.map(str::trim).filter(|u| !u.is_empty()) {
        Some(url) => {
            let bytes = probe
                .download(url)
                .ok_or_else(|| DegradeReason::DownloadFailed(url.to_string()))?;
            verify_sha256(&bytes, expected_sha).map_err(DegradeReason::from_sha)?;
            if !probe.store(&in_plugin_dir, &bytes) {
                return Err(DegradeReason::DownloadFailed(format!(
                    "could not write {in_plugin_dir}"
                )));
            }
            Ok(ResolvedEngine {
                path: in_plugin_dir,
                source: EnginePathSource::Downloaded,
                version: version.to_string(),
            })
        }
        None => Err(DegradeReason::DownloadDisabled),
    }
}

/// Host-backed probe.
struct HostProbe;

impl EngineProbe for HostProbe {
    fn plugin_dir(&self) -> Option<String> {
        host::fs::plugin_dir().ok()
    }

    fn exists(&self, path: &str) -> bool {
        host::fs::stat(path).is_ok()
    }

    fn download(&self, url: &str) -> Option<Vec<u8>> {
        let response = host::fetch(&HttpRequest::get(url)).ok()?;
        if (200..300).contains(&response.status) {
            Some(response.body)
        } else {
            None
        }
    }

    fn store(&self, path: &str, bytes: &[u8]) -> bool {
        host::fs::write_bytes(path, bytes).is_ok()
    }
}

/// Read the `engine.downloadUrl` setting (default empty ⇒ no download).
fn download_url() -> Option<String> {
    host::setting_string("engine.downloadUrl")
        .or_else(|| host::setting("engine.downloadUrl"))
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
}

/// Resolve the engine jar for the running plugin (host-backed).
pub fn resolve_engine_path(version: &str) -> Result<ResolvedEngine, DegradeReason> {
    let url = download_url();
    resolve_engine_path_with(&HostProbe, version, pinned_sha256_hex(), url.as_deref())
}

// ---------------------------------------------------------------------------
// Start / stop (host-facing)
// ---------------------------------------------------------------------------

/// A running engine process. Dropping it releases the child (the host ends the
/// process when the plugin instance goes away, per the SDK's `Child` contract).
pub struct EngineHandle {
    pub path: String,
    pub source: EnginePathSource,
    pub version: String,
    pub status: EngineStatus,
    pub restarts: u32,
    child: Option<Child>,
}

impl EngineHandle {
    /// Send a request line and read one reply line, bounded by `request_ms`.
    ///
    /// Returns the raw reply text; protocol decoding lives in [`crate::sidecar::proto`].
    pub fn request_line(&self, line: &str) -> Result<String, DegradeReason> {
        let child = self
            .child
            .as_ref()
            .ok_or_else(|| DegradeReason::HandshakeFailed("engine is not running".into()))?;
        child
            .write(&format!("{}\n", line.trim_end()))
            .map_err(|e| DegradeReason::HandshakeFailed(e.to_string()))?;
        let chunk = child
            .wait(DEFAULT_TIMEOUTS.request_ms)
            .map_err(|e| DegradeReason::HandshakeFailed(e.to_string()))?;
        if chunk.stdout.trim().is_empty()
            && let Some(code) = chunk.status
        {
            return Err(DegradeReason::HandshakeFailed(format!(
                "engine exited {code}: {}",
                chunk.stderr.trim()
            )));
        }
        Ok(chunk.stdout)
    }

    /// Stop the process and mark the engine stopped.
    pub fn stop(&mut self) -> Result<(), DegradeReason> {
        if let Some(child) = self.child.as_ref() {
            // Best effort: ask it to leave, then make sure it does.
            let _ = child.write("{\"protocol\":1,\"kind\":\"shutdown\"}\n");
            child
                .kill()
                .map_err(|e| DegradeReason::SpawnFailed(e.to_string()))?;
        }
        self.child = None;
        self.status = EngineStatus::Stopped;
        Ok(())
    }

    fn kill_quietly(&mut self) {
        if let Some(child) = self.child.as_ref() {
            let _ = child.kill();
        }
        self.child = None;
    }
}

/// The argv used to launch the engine's stdio dispatcher.
///
/// Pure and shared by [`start`] and the real-process test, so the plugin and the
/// test can never drift on the launch contract. The jar carries
/// `Main-Class: mpsv.engine.StdioMain` (see `sub-projects/engine-jar/build.gradle.kts`),
/// so `java -jar` is enough — no separate stdio jar, no classpath assembly.
pub fn engine_command(jar: &str) -> (String, Vec<String>) {
    (
        "java".to_string(),
        vec!["-jar".to_string(), jar.to_string(), "--stdio".to_string()],
    )
}

/// Start the pinned engine over stdin/stdout JSON lines.
///
/// Steps: resolve → verify sha256 → spawn `java -jar <jar> --stdio` →
/// handshake (a `ping` line, version check) within [`DEFAULT_TIMEOUTS`].
pub fn start(version: &str) -> Result<EngineHandle, DegradeReason> {
    let resolved = resolve_engine_path(version)?;

    // Verify local tiers too (the download tier already verified).
    let bytes = host::fs::read_bytes(&resolved.path)
        .map_err(|e| DegradeReason::SpawnFailed(format!("read {}: {e}", resolved.path)))?;
    verify_sha256(&bytes, pinned_sha256_hex()).map_err(DegradeReason::from_sha)?;

    let (program, args) = engine_command(&resolved.path);
    let mut command = Command::new(&program);
    for arg in &args {
        command = command.arg(arg);
    }
    let options = ProcessOptions {
        max_output_bytes: Some(4 * 1024 * 1024),
        buffer_bytes: None,
    };
    let child = host::start_stream(&command, None, Some(options))
        .map_err(|e| DegradeReason::SpawnFailed(e.to_string()))?;

    let mut handle = EngineHandle {
        path: resolved.path,
        source: resolved.source,
        version: version.to_string(),
        status: EngineStatus::Starting,
        restarts: 0,
        child: Some(child),
    };

    match handle.handshake(version) {
        Ok(reported) => {
            handle.status = EngineStatus::Ready { version: reported };
            Ok(handle)
        }
        Err(reason) => {
            handle.kill_quietly();
            Err(reason)
        }
    }
}

impl EngineHandle {
    fn handshake(&self, expected_version: &str) -> Result<String, DegradeReason> {
        let child = self
            .child
            .as_ref()
            .ok_or_else(|| DegradeReason::HandshakeFailed("no child process".into()))?;
        child
            .write("{\"protocol\":1,\"kind\":\"ping\"}\n")
            .map_err(|e| DegradeReason::HandshakeFailed(e.to_string()))?;
        let chunk = child
            .wait(DEFAULT_TIMEOUTS.cold_start_ms)
            .map_err(|e| DegradeReason::HandshakeFailed(e.to_string()))?;

        if let Some(reported) = parse_health_version(&chunk.stdout) {
            if reported == expected_version {
                return Ok(reported);
            }
            return Err(DegradeReason::VersionMismatch {
                expected: expected_version.to_string(),
                got: reported,
            });
        }
        if let Some(code) = chunk.status {
            return Err(DegradeReason::HandshakeFailed(format!(
                "engine exited {code}: {}",
                chunk.stderr.trim()
            )));
        }
        Err(DegradeReason::HandshakeFailed(format!(
            "no health reply: {}",
            chunk.stdout.trim()
        )))
    }
}

/// Stop an engine handle (free-function form of [`EngineHandle::stop`]).
pub fn stop(handle: &mut EngineHandle) -> Result<(), DegradeReason> {
    handle.stop()
}

// ---------------------------------------------------------------------------
// Tests — pure logic only; start/stop are exercised in wasm/mock integration.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // ---- health state machine (≥4 cases) ----

    #[test]
    fn state_machine_reaches_ready_after_start() {
        let mut m = HealthMonitor::new(3);
        assert_eq!(m.status(), &EngineStatus::Stopped);
        assert_eq!(m.start(), Action::Restart);
        assert_eq!(m.status(), &EngineStatus::Starting);
        m.started("0.1.0");
        assert_eq!(
            m.status(),
            &EngineStatus::Ready {
                version: "0.1.0".into()
            }
        );
        assert!(m.status().is_ready());
        assert_eq!(m.status().message_key(), None);
    }

    #[test]
    fn state_machine_retries_crashes_up_to_the_cap_then_degrades() {
        let mut m = HealthMonitor::new(3);
        m.start();
        assert_eq!(m.crash(), Action::Restart);
        assert_eq!(m.crash(), Action::Restart);
        assert_eq!(m.crash(), Action::Restart);
        assert_eq!(m.restarts(), 3);
        assert_eq!(m.crash(), Action::Degrade);
        assert_eq!(
            m.status(),
            &EngineStatus::Degraded {
                reason: DegradeReason::Crashed
            }
        );
        assert_eq!(m.status().message_key(), Some("engine.crashed"));
        // Further crashes stay degraded and do not grow the counter.
        assert_eq!(m.crash(), Action::Degrade);
        assert_eq!(m.restarts(), 3);
    }

    #[test]
    fn state_machine_fatal_error_degrades_without_retry() {
        let mut m = HealthMonitor::new(3);
        m.start();
        let action = m.fatal(DegradeReason::Sha256Mismatch {
            expected: "aa".into(),
            actual: "bb".into(),
        });
        assert_eq!(action, Action::Degrade);
        assert_eq!(m.restarts(), 0, "fatal errors are not retried");
        assert_eq!(
            m.status().message_key(),
            Some("engine.sha256.mismatch"),
            "degraded status surfaces the reason's key"
        );
    }

    #[test]
    fn state_machine_recovers_from_a_crash_below_the_cap() {
        let mut m = HealthMonitor::new(3);
        m.start();
        m.started("0.1.0");
        assert_eq!(m.crash(), Action::Restart); // one transient crash
        m.started("0.1.0");
        assert!(m.status().is_ready(), "a respawn can become ready again");
        assert_eq!(m.restarts(), 1);
    }

    // ---- retry schedule (≥3 cases) ----

    #[test]
    fn retry_schedule_is_exponential_and_bounded_by_max() {
        assert_eq!(retry_delays_ms(0), Vec::<u32>::new());
        assert_eq!(retry_delays_ms(1), vec![250]);
        assert_eq!(retry_delays_ms(3), vec![250, 500, 1000]);
        assert_eq!(retry_delays_ms(3).len(), 3);
    }

    #[test]
    fn retry_schedule_does_not_overflow_for_a_large_cap() {
        let v = retry_delays_ms(10);
        assert_eq!(v.len(), 10);
        assert!(v.windows(2).all(|w| w[0] <= w[1]), "non-decreasing");
    }

    #[test]
    #[allow(clippy::assertions_on_constants)] // the target is intentionally pinned
    fn default_cold_start_budget_meets_the_two_second_target() {
        assert_eq!(DEFAULT_TIMEOUTS.cold_start_ms, 2_000);
        assert!(DEFAULT_TIMEOUTS.request_ms >= DEFAULT_TIMEOUTS.cold_start_ms);
    }

    // ---- path candidates ----

    #[test]
    fn dev_repo_candidates_walk_up_to_the_repo_root() {
        let got = dev_repo_candidates("/repo/build/plugin-data", "mpsv-engine-0.1.0.jar");
        assert_eq!(
            got,
            vec![
                "/repo/build/plugin-data/engine/mpsv-engine-0.1.0.jar".to_string(),
                "/repo/build/engine/mpsv-engine-0.1.0.jar".to_string(),
                "/repo/engine/mpsv-engine-0.1.0.jar".to_string(),
                "/engine/mpsv-engine-0.1.0.jar".to_string(),
            ]
        );
    }

    #[test]
    fn jar_file_name_is_exact() {
        assert_eq!(jar_file_name("0.1.0"), "mpsv-engine-0.1.0.jar");
    }

    #[test]
    fn engine_command_is_java_jar_stdio() {
        let (program, args) = engine_command("/x/mpsv-engine-0.1.0.jar");
        assert_eq!(program, "java");
        assert_eq!(
            args,
            vec![
                "-jar".to_string(),
                "/x/mpsv-engine-0.1.0.jar".to_string(),
                "--stdio".to_string()
            ]
        );
    }

    // ---- handshake parsing ----

    #[test]
    fn parses_the_health_reply() {
        assert_eq!(
            parse_health_version("{\"ok\":true,\"version\":\"0.1.0\"}\n"),
            Some("0.1.0".into())
        );
        assert_eq!(
            parse_health_version("noise\n{\"ok\":true,\"version\":\"9.9.9\"}\n"),
            Some("9.9.9".into())
        );
        assert_eq!(parse_health_version("{\"ok\":false,\"error\":\"x\"}"), None);
        assert_eq!(parse_health_version("garbage"), None);
    }

    // ---- resolution against a fake probe (3 tiers + fallbacks) ----

    struct MapProbe {
        dir: Option<String>,
        files: HashSet<String>,
        download: Option<Vec<u8>>,
    }

    impl EngineProbe for MapProbe {
        fn plugin_dir(&self) -> Option<String> {
            self.dir.clone()
        }
        fn exists(&self, path: &str) -> bool {
            self.files.contains(path)
        }
        fn download(&self, _url: &str) -> Option<Vec<u8>> {
            self.download.clone()
        }
        fn store(&self, _path: &str, _bytes: &[u8]) -> bool {
            true
        }
    }

    #[test]
    fn resolves_tier1_the_plugin_dir_copy() {
        let mut files = HashSet::new();
        files.insert("/p/mpsv-engine-0.1.0.jar".to_string());
        let probe = MapProbe {
            dir: Some("/p".into()),
            files,
            download: None,
        };
        let r = resolve_engine_path_with(&probe, "0.1.0", "aa", None).unwrap();
        assert_eq!(r.path, "/p/mpsv-engine-0.1.0.jar");
        assert_eq!(r.source, EnginePathSource::PluginDir);
    }

    #[test]
    fn resolves_tier2_the_dev_repo_jar() {
        let mut files = HashSet::new();
        files.insert("/repo/engine/mpsv-engine-0.1.0.jar".to_string());
        let probe = MapProbe {
            dir: Some("/repo/build/plugin-data".into()),
            files,
            download: None,
        };
        let r = resolve_engine_path_with(&probe, "0.1.0", "aa", None).unwrap();
        assert_eq!(r.path, "/repo/engine/mpsv-engine-0.1.0.jar");
        assert_eq!(r.source, EnginePathSource::DevRepo);
    }

    #[test]
    fn resolves_tier3_by_downloading_then_verifying() {
        let bytes = b"engine-bytes".to_vec();
        let expected = crate::sidecar::sha256_hex(&bytes);
        let probe = MapProbe {
            dir: Some("/p".into()),
            files: HashSet::new(),
            download: Some(bytes),
        };
        let r =
            resolve_engine_path_with(&probe, "0.1.0", &expected, Some("https://x/e.jar")).unwrap();
        assert_eq!(r.path, "/p/mpsv-engine-0.1.0.jar");
        assert_eq!(r.source, EnginePathSource::Downloaded);
    }

    #[test]
    fn a_download_with_a_bad_hash_is_refused() {
        let probe = MapProbe {
            dir: Some("/p".into()),
            files: HashSet::new(),
            download: Some(b"wrong".to_vec()),
        };
        let expected = crate::sidecar::sha256_hex(b"right");
        match resolve_engine_path_with(&probe, "0.1.0", &expected, Some("https://x/e.jar")) {
            Err(DegradeReason::Sha256Mismatch {
                expected: e,
                actual: a,
            }) => {
                assert_eq!(e, expected);
                assert_eq!(a, crate::sidecar::sha256_hex(b"wrong"));
            }
            other => panic!("expected Sha256Mismatch, got {other:?}"),
        }
    }

    #[test]
    fn no_jar_and_no_url_degrades_as_download_disabled() {
        let probe = MapProbe {
            dir: Some("/p".into()),
            files: HashSet::new(),
            download: None,
        };
        match resolve_engine_path_with(&probe, "0.1.0", "aa", None) {
            Err(reason) => assert_eq!(reason.message_key(), "engine.download.disabled"),
            Ok(r) => panic!("expected a degrade, resolved {r:?}"),
        }
    }

    #[test]
    fn no_plugin_dir_is_binary_missing() {
        let probe = MapProbe {
            dir: None,
            files: HashSet::new(),
            download: None,
        };
        assert_eq!(
            resolve_engine_path_with(&probe, "0.1.0", "aa", None),
            Err(DegradeReason::BinaryMissing)
        );
    }

    // ---- host-backed start degrades cleanly in the mock (no jar, no url) ----

    #[test]
    fn start_degrades_in_a_bare_mock_instead_of_pretending() {
        let _host = openidea_plugin::mock::MockHost::install();
        match start("0.1.0") {
            Err(reason) => assert!(matches!(
                reason,
                DegradeReason::BinaryMissing | DegradeReason::DownloadDisabled
            )),
            Ok(_) => panic!("a bare mock has no engine jar; start must degrade"),
        }
    }

    // ---- real-process end-to-end (ignored; needs the built engine jar) ----

    /// Drives the **real** engine jar's `--stdio` dispatcher over std::process:
    /// ping handshake (measuring cold start), a `check` round-trip with `id`
    /// echo, an `explain`, and a bad-JSON request that must not kill the process.
    ///
    /// Run:
    /// `MPSV_ENGINE_JAR=<path/to/mpsv-engine-0.1.0.jar> cargo test sidecar -- --ignored`
    #[test]
    #[ignore = "spawns the real engine jar; needs MPSV_ENGINE_JAR"]
    fn real_jar_stdio_round_trip() {
        use std::io::{BufRead, BufReader, Write};
        use std::process::{Command, Stdio};
        use std::time::Instant;

        let jar =
            std::env::var("MPSV_ENGINE_JAR").expect("set MPSV_ENGINE_JAR to the engine jar path");
        let (program, args) = engine_command(&jar);
        let java = std::env::var("JAVA_HOME")
            .map(|home| format!("{home}/bin/java"))
            .unwrap_or(program);

        let started = Instant::now();
        let mut child = Command::new(java)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn engine");
        // A free helper (not a closure) keeps the child borrows simple: it takes
        // `&mut` only for the write, then stdout/kill are used afterwards.
        fn send(child: &mut std::process::Child, line: &str) {
            let stdin = child.stdin.as_mut().expect("stdin");
            writeln!(stdin, "{line}").expect("write request");
        }

        // 1) ping handshake + cold start
        send(&mut child, r#"{"protocol":1,"kind":"ping"}"#);
        let mut lines = BufReader::new(child.stdout.take().expect("stdout")).lines();
        let ping: serde_json::Value =
            serde_json::from_str(&lines.next().expect("ping line").expect("ping io")).unwrap();
        let cold_start_ms = started.elapsed().as_millis();
        assert_eq!(ping["ok"], true, "ping: {ping}");
        assert_eq!(ping["version"], "0.1.0");

        // 2) check with an id echo
        send(
            &mut child,
            r#"{"protocol":1,"kind":"check","id":7,"source":{"path":"x","text":"SELECT 1"}}"#,
        );
        let check: serde_json::Value =
            serde_json::from_str(&lines.next().expect("check line").expect("check io")).unwrap();
        assert_eq!(check["ok"], true, "check: {check}");
        assert_eq!(check["statementCount"], 1);
        assert_eq!(check["id"], 7, "id is echoed");

        // 3) explain shaping
        send(
            &mut child,
            r#"{"protocol":1,"kind":"explain","sql":"SELECT * FROM t WHERE id = 1","dialect":"MySQL"}"#,
        );
        let explain: serde_json::Value =
            serde_json::from_str(&lines.next().expect("explain line").expect("explain io"))
                .unwrap();
        assert_eq!(explain["ok"], true, "explain: {explain}");
        assert!(
            explain["explainSql"]
                .as_str()
                .unwrap_or_default()
                .starts_with("EXPLAIN")
        );

        // 4) bad JSON: structured error, process stays alive
        send(&mut child, "not json");
        let bad: serde_json::Value =
            serde_json::from_str(&lines.next().expect("bad line").expect("bad io")).unwrap();
        assert_eq!(bad["ok"], false, "bad: {bad}");
        assert_eq!(bad["kind"], "invalid");

        let _ = child.kill();
        let _ = child.wait(); // reap the child so the test leaves no zombie
        eprintln!("L8 real jar: cold_start_ms={cold_start_ms} (target <= 2000)");
        assert!(cold_start_ms < 10_000, "engine hung: {cold_start_ms} ms");
    }
}
