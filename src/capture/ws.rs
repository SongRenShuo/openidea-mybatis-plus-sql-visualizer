//! Loopback WebSocket consumer for agent-captured SQL events (T4-3).
//!
//! The IDE sandbox has no raw socket, so the capture chain reaches a Java agent
//! through `host::WebSocket` (API 1.6, `net.websocket-connect`). This module
//! keeps the testable policy (URL rules, frame decoding, bounded staging queue,
//! connection budget, reconnect schedule) separate from the thin host wrapper.
//!
//! Wire contract with the agent (`sub-projects/sql-visualizer-agent`, T4-1):
//!
//! - every frame is UTF-8 JSON text;
//! - an event frame is `{"cmd":1,"body":"<the exact JSON body TCP sent>"}`, so
//!   the event payload is byte-identical to the TCP transport and its semantics
//!   do not change (`{"sql":…,"ts":…,"dn":…,"p":…,…}`);
//! - a canonical flat frame `{"id":…,"timestamp_ms":…,"sql":…,"params":…,
//!   "duration_ms":…,"source":…,"error":…}` is accepted too, for producers that
//!   emit the projection directly;
//! - control frames (`cmd` 2 heartbeat, 4 hello, 5 config ack) carry no SQL and
//!   are reported as controls, never as events.
//!
//! Bounds (all explicit, never "pretend it was sent"):
//!
//! | bound | value |
//! | --- | --- |
//! | open connections | [`MAX_CONNECTIONS`] (8) |
//! | staging frames | [`MAX_QUEUED_FRAMES`] (256) |
//! | staging bytes | [`MAX_QUEUE_BYTES`] (8 MiB) |
//! | one frame | [`MAX_FRAME_BYTES`] (1 MiB) |

use std::collections::VecDeque;

use openidea_plugin::Error;
use openidea_plugin::host::{self, Project, WebSocket, WebSocketRequest};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A plugin may hold eight background WebSockets (SDK `WebSocket::connect`).
pub const MAX_CONNECTIONS: usize = 8;
/// Frames the consumer may stage before it starts rejecting.
pub const MAX_QUEUED_FRAMES: usize = 256;
/// Most frames one `host::WebSocket::drain` may ask for.
///
/// The host refuses a larger request instead of clamping it: `drain()` answers
/// `invalid drain limit` for `limit == 0 || limit > DRAIN_BATCH`, and
/// `DRAIN_BATCH` is **32** (`crates/ide-host/src/plugins/websocket.rs`). Asking
/// for our own larger queue size therefore failed **every** drain, which left
/// the panel showing a healthy connection and zero rows forever — the failure
/// only became visible once the drain error was reported to the Output channel.
pub const HOST_DRAIN_LIMIT: usize = 32;
/// Rounds of `drain` one poll may make, so a backlog catches up without
/// holding the plugin for more than a few host calls.
pub const MAX_DRAIN_ROUNDS: usize = 8;
/// Bytes the consumer may stage (frames + payloads) before it starts rejecting.
pub const MAX_QUEUE_BYTES: usize = 8 * 1024 * 1024;
/// Largest single frame accepted. A larger frame is rejected, not truncated.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// The `limit` to hand `host::WebSocket::drain`: its own batch cap, never our
/// queue size.
fn drain_request(limit: u32) -> u32 {
    limit.clamp(1, HOST_DRAIN_LIMIT as u32)
}

/// `cmd` values shared with the agent's `EventBus`.
pub const CMD_EVENT: i64 = 1;
pub const CMD_HEARTBEAT: i64 = 2;
pub const CMD_CONFIG: i64 = 3;
pub const CMD_HELLO: i64 = 4;
pub const CMD_CONFIG_ACK: i64 = 5;

/// Message keys this module returns; the locale files map them (T4-2).
pub const KEY_BAD_URL: &str = "capture.ws.bad_url";
pub const KEY_TOO_MANY: &str = "capture.ws.too_many";
pub const KEY_HOST: &str = "capture.ws.host_error";
pub const KEY_GIVE_UP: &str = "capture.ws.give_up";

/// What the agent says about itself in every `HEARTBEAT` body.
///
/// The agent counts what its weaving did (`weavable` / `woven` / `failed` /
/// `matched`), what it had to skip (`unmodifiable`, `skippedByVersion`) and why
/// it stopped on its own (`gate`: 1 = the CONFIG watchdog fired). Without these
/// numbers "connected but nothing captured" cannot be attributed: hooks that
/// never attached, hooks that attached but never fired, and a session the agent
/// stopped all look alike from the plugin side.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct AgentHealth {
    /// Classes whose bytecode the agent could rewrite.
    pub weavable: u64,
    /// Classes actually rewritten.
    pub woven: u64,
    /// Classes the rewrite failed on.
    pub failed: u64,
    /// Attach attempts that matched a hook target.
    pub matched: u64,
    /// Targets skipped because their module/class refused modification.
    pub unmodifiable: u64,
    /// Targets matched through an interface and woven that way.
    pub interface_woven: u64,
    /// Targets skipped because of the class-file version.
    pub skipped_by_version: u64,
    /// Whether the catch-all fallback is active (0/1).
    pub catch_all: u64,
    /// `0` = none, `1` = the CONFIG watchdog stopped capture.
    pub gate: u64,
    /// Events dropped after the agent's own overflow ledger gave up.
    pub dropped: u64,
}

impl AgentHealth {
    /// Reads a heartbeat body; a body without any known key is `None` so a
    /// foreign producer cannot fake health.
    pub fn from_body(body: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(body).ok()?;
        let object = value.as_object()?;
        let num = |key: &str| object.get(key).and_then(Value::as_u64).unwrap_or(0);
        let known = [
            "weavable",
            "woven",
            "failed",
            "matched",
            "unmodifiable",
            "skippedByVersion",
            "gate",
        ];
        if !known.iter().any(|key| object.contains_key(*key)) {
            return None;
        }
        Some(Self {
            weavable: num("weavable"),
            woven: num("woven"),
            failed: num("failed"),
            matched: num("matched"),
            unmodifiable: num("unmodifiable"),
            interface_woven: num("interfaceWoven"),
            skipped_by_version: num("skippedByVersion"),
            catch_all: num("catchAll"),
            gate: num("gate"),
            dropped: num("dropped"),
        })
    }

    /// One line for the Output channel: the numbers a reader needs first.
    pub fn summary(&self) -> String {
        format!(
            "woven={} matched={} failed={} unmodifiable={} skippedByVersion={} catchAll={} gate={} dropped={}",
            self.woven,
            self.matched,
            self.failed,
            self.unmodifiable,
            self.skipped_by_version,
            self.catch_all,
            self.gate,
            self.dropped
        )
    }

    /// The agent stopped capture on its own because no `CONFIG` arrived.
    pub fn stopped_by_watchdog(&self) -> bool {
        self.gate == 1
    }
}

/// One captured SQL event, as the agent sends it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CaptureEvent {
    /// Monotonic consumer-assigned id (`0` when the producer did not send one).
    #[serde(default)]
    pub id: u64,
    /// Epoch milliseconds at execute start.
    #[serde(default)]
    pub timestamp_ms: u64,
    #[serde(default)]
    pub sql: String,
    /// Parameter snapshots verbatim: `[index, value, type]` from the agent, or
    /// plain values from a canonical producer.
    #[serde(default)]
    pub params: Vec<Value>,
    #[serde(default)]
    pub duration_ms: u64,
    /// `file:line` (agent legacy keys `sf`/`sl`).
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub error: Option<String>,
}

/// What a single frame turned into.
#[derive(Clone, Debug, PartialEq)]
pub enum FrameDecode {
    Event(CaptureEvent),
    /// A non-event frame (`cmd` heartbeat/hello/config ack). Not a SQL event.
    Control {
        cmd: i64,
    },
    /// A malformed frame. The caller counts it and keeps going.
    Bad {
        reason: String,
    },
}

/// Why a frame was rejected before it entered the staging queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PushRejection {
    /// The frame alone exceeds [`MAX_FRAME_BYTES`].
    FrameTooLarge { size: usize, limit: usize },
    /// The staging queue already holds the maximum frames or bytes.
    QueueFull { frames: usize, bytes: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PushOutcome {
    Queued,
    Rejected(PushRejection),
}

/// Bounded staging queue between the host's FIFO and the decoder.
#[derive(Debug, Default)]
pub struct FrameQueue {
    frames: VecDeque<Vec<u8>>,
    bytes: usize,
    rejected: u64,
}

impl FrameQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stages `frame`, or returns an explicit rejection. A rejected frame is
    /// never counted as delivered.
    pub fn push(&mut self, frame: Vec<u8>) -> PushOutcome {
        if frame.len() > MAX_FRAME_BYTES {
            self.rejected += 1;
            return PushOutcome::Rejected(PushRejection::FrameTooLarge {
                size: frame.len(),
                limit: MAX_FRAME_BYTES,
            });
        }
        if self.frames.len() >= MAX_QUEUED_FRAMES || self.bytes + frame.len() > MAX_QUEUE_BYTES {
            self.rejected += 1;
            return PushOutcome::Rejected(PushRejection::QueueFull {
                frames: self.frames.len(),
                bytes: self.bytes,
            });
        }
        self.bytes += frame.len();
        self.frames.push_back(frame);
        PushOutcome::Queued
    }

    pub fn pop(&mut self) -> Option<Vec<u8>> {
        let frame = self.frames.pop_front()?;
        self.bytes -= frame.len();
        Some(frame)
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn rejected(&self) -> u64 {
        self.rejected
    }
}

/// Decodes one UTF-8 frame into an event, a control frame, or a bad frame.
///
/// Never panics and never fails the stream: a bad frame is a value the caller
/// counts (`bad_frames`) and discards.
pub fn decode_frame(frame: &str) -> FrameDecode {
    let value: Value = match serde_json::from_str(frame) {
        Ok(value) => value,
        Err(err) => {
            return FrameDecode::Bad {
                reason: format!("invalid json: {err}"),
            };
        }
    };
    let object = match value.as_object() {
        Some(object) => object,
        None => {
            return FrameDecode::Bad {
                reason: "frame is not a JSON object".to_owned(),
            };
        }
    };
    if let Some(cmd) = object.get("cmd").and_then(Value::as_i64) {
        if cmd != CMD_EVENT {
            return FrameDecode::Control { cmd };
        }
        return match object.get("body") {
            Some(Value::String(body)) => decode_event_body(body),
            Some(body @ Value::Object(_)) => decode_canonical(body),
            _ => FrameDecode::Bad {
                reason: "event frame has no body".to_owned(),
            },
        };
    }
    decode_canonical(&value)
}

/// Builds the agent envelope `{"cmd":…,"body":…}` used for control frames the
/// plugin sends (CONFIG). The body is embedded as a JSON string, so it stays
/// byte-identical to what the TCP transport would have carried.
///
/// **The key order is part of the protocol.** The agent parses this by hand
/// (`WsFrameCodec.decodeEnvelope`): it finds `"cmd":`, reads the number, then
/// looks for `"body":` *after* those digits. `serde_json::json!` builds a
/// `BTreeMap` here (the `preserve_order` feature is off), so the generic form
/// emitted `{"body":…,"cmd":…}` — alphabetically sorted — and the agent dropped
/// every CONFIG frame with `无法解析入站 WS 信封`. That looked like "connected
/// but capturing nothing", with only the 15s watchdog to show for it. Build the
/// string in the documented order instead of trusting a map's ordering.
pub fn encode_envelope(cmd: i64, body: &str) -> String {
    // `to_string` on a &str only fails on an allocation error; an empty string
    // is still a valid envelope, so a fallback cannot corrupt the framing.
    let quoted = serde_json::to_string(body).unwrap_or_else(|_| "\"\"".to_owned());
    format!("{{\"cmd\":{cmd},\"body\":{quoted}}}")
}

/// Extracts `(cmd, body)` from an envelope, for callers that need the body.
pub fn decode_envelope(text: &str) -> Option<(i64, String)> {
    let value: Value = serde_json::from_str(text).ok()?;
    let object = value.as_object()?;
    let cmd = object.get("cmd")?.as_i64()?;
    let body = match object.get("body")? {
        Value::String(body) => body.clone(),
        other => other.to_string(),
    };
    Some((cmd, body))
}

/// Agent envelope body: legacy `EventBus`/`Jsons.writeEvent` keys, or the
/// canonical projection keys (canonical names win when both are present).
fn decode_event_body(body: &str) -> FrameDecode {
    let value: Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(err) => {
            return FrameDecode::Bad {
                reason: format!("invalid event body: {err}"),
            };
        }
    };
    decode_canonical(&value)
}

/// Reads an event object. Legacy keys are accepted because the agent's TCP body
/// is reused verbatim: `ts` epoch ms, `dn` duration nanos, `p` parameters,
/// `sf`/`sl` source, `err` error.
fn decode_canonical(value: &Value) -> FrameDecode {
    let object = match value.as_object() {
        Some(object) => object,
        None => {
            return FrameDecode::Bad {
                reason: "event is not a JSON object".to_owned(),
            };
        }
    };
    let sql = match object.get("sql") {
        Some(Value::String(sql)) => sql.clone(),
        _ => {
            return FrameDecode::Bad {
                reason: "event has no string sql".to_owned(),
            };
        }
    };

    let id = object.get("id").and_then(Value::as_u64).unwrap_or(0);
    let timestamp_ms = object
        .get("timestamp_ms")
        .and_then(Value::as_u64)
        .or_else(|| object.get("ts").and_then(Value::as_u64))
        .unwrap_or(0);
    let duration_ms = match object.get("duration_ms").and_then(Value::as_u64) {
        Some(ms) => ms,
        // Legacy `dn` is nanoseconds; project it to whole milliseconds.
        None => object
            .get("dn")
            .and_then(Value::as_u64)
            .map_or(0, |ns| ns / 1_000_000),
    };
    let params = match object.get("params").or_else(|| object.get("p")) {
        Some(Value::Array(items)) => items.clone(),
        _ => Vec::new(),
    };
    let source = match object.get("source") {
        Some(Value::String(source)) => source.clone(),
        _ => legacy_source(object),
    };
    let error = match object.get("error").or_else(|| object.get("err")) {
        Some(Value::String(error)) => Some(error.clone()),
        _ => None,
    };

    FrameDecode::Event(CaptureEvent {
        id,
        timestamp_ms,
        sql,
        params,
        duration_ms,
        source,
        error,
    })
}

/// `sf` + optional `sl` → `file:line`, the projection the panel shows.
fn legacy_source(object: &serde_json::Map<String, Value>) -> String {
    let file = match object.get("sf").and_then(Value::as_str) {
        Some(file) => file,
        None => return String::new(),
    };
    match object.get("sl").and_then(Value::as_u64) {
        Some(line) if line > 0 => format!("{file}:{line}"),
        _ => file.to_owned(),
    }
}

/// A validated WebSocket endpoint. Credentials, query strings and fragments are
/// rejected: headers carry authentication, so the URL stays free of secrets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WsEndpoint {
    pub url: String,
    pub host: String,
    pub port: Option<u16>,
    pub secure: bool,
    pub path: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UrlError {
    NotWebSocket,
    MissingHost,
    InvalidHost,
    InsecureNonLoopback,
    Credentials,
    Query,
    Fragment,
    BadPort,
}

impl UrlError {
    pub fn message_key(self) -> &'static str {
        KEY_BAD_URL
    }
}

/// Validates a capture WebSocket URL.
///
/// Rules: `ws://` only for loopback (`127.0.0.1`, `localhost`); every other
/// host needs `wss://`. No credentials (`@`), no query (`?`), no fragment (`#`).
pub fn validate_url(raw: &str) -> Result<WsEndpoint, UrlError> {
    let (secure, rest) = if let Some(rest) = raw.strip_prefix("wss://") {
        (true, rest)
    } else if let Some(rest) = raw.strip_prefix("ws://") {
        (false, rest)
    } else {
        return Err(UrlError::NotWebSocket);
    };
    if rest.contains('@') {
        return Err(UrlError::Credentials);
    }
    if rest.contains('?') {
        return Err(UrlError::Query);
    }
    if rest.contains('#') {
        return Err(UrlError::Fragment);
    }
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err(UrlError::MissingHost);
    }
    let (host, port_text) = match authority.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };
    if host.is_empty() {
        return Err(UrlError::MissingHost);
    }
    if !host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        return Err(UrlError::InvalidHost);
    }
    let port = match port_text {
        None => None,
        Some(text) => {
            if text.is_empty() {
                return Err(UrlError::BadPort);
            }
            match text.parse::<u16>() {
                Ok(0) | Err(_) => return Err(UrlError::BadPort),
                Ok(port) => Some(port),
            }
        }
    };
    let host_lower = host.to_ascii_lowercase();
    if !secure && !is_loopback_host(&host_lower) {
        return Err(UrlError::InsecureNonLoopback);
    }
    Ok(WsEndpoint {
        url: raw.to_owned(),
        host: host_lower,
        port,
        secure,
        path: path.to_owned(),
    })
}

fn is_loopback_host(host: &str) -> bool {
    host == "127.0.0.1" || host == "localhost"
}

/// Exponential reconnect schedule: 200 ms doubling, capped at 5 s, at most 3
/// attempts, then the capture chain degrades with [`KEY_GIVE_UP`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReconnectPolicy {
    pub initial_ms: u64,
    pub max_ms: u64,
    pub max_attempts: u32,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            initial_ms: 200,
            max_ms: 5_000,
            max_attempts: 3,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReconnectDecision {
    Reconnect {
        attempt: u32,
        delay_ms: u64,
    },
    GiveUp {
        attempts: u32,
        message_key: &'static str,
    },
}

/// Pure reconnect schedule. `on_connected` resets it; `on_disconnect` advances.
#[derive(Clone, Debug, Default)]
pub struct ReconnectPlan {
    policy: ReconnectPolicy,
    attempts: u32,
    degraded: bool,
}

impl ReconnectPlan {
    pub fn new(policy: ReconnectPolicy) -> Self {
        Self {
            policy,
            attempts: 0,
            degraded: false,
        }
    }

    pub fn on_connected(&mut self) {
        self.attempts = 0;
        self.degraded = false;
    }

    pub fn on_disconnect(&mut self) -> ReconnectDecision {
        if self.degraded {
            return ReconnectDecision::GiveUp {
                attempts: self.attempts,
                message_key: KEY_GIVE_UP,
            };
        }
        self.attempts += 1;
        if self.attempts > self.policy.max_attempts {
            self.degraded = true;
            return ReconnectDecision::GiveUp {
                attempts: self.attempts,
                message_key: KEY_GIVE_UP,
            };
        }
        ReconnectDecision::Reconnect {
            attempt: self.attempts,
            delay_ms: self.delay_for(self.attempts),
        }
    }

    fn delay_for(&self, attempt: u32) -> u64 {
        let shift = attempt.saturating_sub(1).min(63);
        self.policy
            .initial_ms
            .saturating_mul(1u64 << shift)
            .min(self.policy.max_ms)
    }

    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    pub fn degraded(&self) -> bool {
        self.degraded
    }
}

/// The eight-connection budget, checked before asking the host.
#[derive(Clone, Debug, Default)]
pub struct ConnectionSlots {
    open: usize,
}

impl ConnectionSlots {
    pub fn try_open(&mut self) -> Result<(), WsError> {
        if self.open >= MAX_CONNECTIONS {
            return Err(WsError::TooManyConnections {
                limit: MAX_CONNECTIONS,
            });
        }
        self.open += 1;
        Ok(())
    }

    pub fn release(&mut self) {
        self.open = self.open.saturating_sub(1);
    }

    pub fn open_count(&self) -> usize {
        self.open
    }
}

/// Supervises the connection budget plus the reconnect schedule, so a caller
/// cannot reconnect past the limit or after giving up.
#[derive(Clone, Debug, Default)]
pub struct WsSupervisor {
    slots: ConnectionSlots,
    plan: ReconnectPlan,
}

impl WsSupervisor {
    pub fn new(policy: ReconnectPolicy) -> Self {
        Self {
            slots: ConnectionSlots::default(),
            plan: ReconnectPlan::new(policy),
        }
    }

    /// Reserves a slot unless capture already degraded.
    pub fn open(&mut self) -> Result<(), WsError> {
        if self.plan.degraded() {
            return Err(WsError::Degraded {
                message_key: KEY_GIVE_UP,
            });
        }
        self.slots.try_open()
    }

    pub fn on_connected(&mut self) {
        self.plan.on_connected();
    }

    pub fn on_disconnected(&mut self) -> ReconnectDecision {
        self.slots.release();
        self.plan.on_disconnect()
    }

    pub fn degraded(&self) -> bool {
        self.plan.degraded()
    }

    pub fn attempts(&self) -> u32 {
        self.plan.attempts()
    }

    pub fn open_count(&self) -> usize {
        self.slots.open_count()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WsError {
    InvalidUrl(UrlError),
    TooManyConnections { limit: usize },
    Degraded { message_key: &'static str },
    Host(String),
}

impl WsError {
    pub fn message_key(&self) -> &'static str {
        match self {
            Self::InvalidUrl(error) => error.message_key(),
            Self::TooManyConnections { .. } => KEY_TOO_MANY,
            Self::Degraded { message_key } => message_key,
            Self::Host(_) => KEY_HOST,
        }
    }
}

impl From<Error> for WsError {
    fn from(error: Error) -> Self {
        Self::Host(error.to_string())
    }
}

impl std::fmt::Display for WsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidUrl(error) => write!(f, "invalid capture URL: {error:?}"),
            Self::TooManyConnections { limit } => {
                write!(f, "at most {limit} capture WebSocket connections")
            }
            Self::Degraded { message_key } => write!(f, "capture degraded ({message_key})"),
            Self::Host(message) => write!(f, "WebSocket host call failed: {message}"),
        }
    }
}

/// What one `poll` produced.
#[derive(Clone, Debug, Default)]
pub struct PollOutcome {
    pub events: Vec<CaptureEvent>,
    pub controls: Vec<i64>,
    /// Cumulative malformed frames (never fatal).
    pub bad_frames: u64,
    /// Cumulative rejected frames (too large or queue full).
    pub rejected: u64,
    /// Frames still staged for the next poll.
    pub queued: usize,
    /// The peer reported its final close.
    pub closed: bool,
    /// The newest `HEARTBEAT` body in this batch, when the agent sent one.
    pub health: Option<AgentHealth>,
}

/// Thin wrapper over one `host::WebSocket` plus the pure policy above.
pub struct WsConsumer {
    socket: WebSocket,
    endpoint: WsEndpoint,
    queue: FrameQueue,
    bad_frames: u64,
    closed: bool,
    /// Frames traced to the IDE log so far. The host forwards inbound frames to
    /// plugins only as **binary** messages (`crates/ide-host/src/plugins/
    /// websocket.rs`: `Message::Text` falls through to `_ => {}`), so a peer
    /// that writes text is invisible here and the panel just stays empty. The
    /// first few frames with their `cmd` make that visible instead of silent.
    traced: usize,
}

impl WsConsumer {
    /// Validates `url`, then asks the host for a loopback WebSocket.
    pub fn connect(
        project: &Project,
        url: &str,
        headers: Vec<(String, String)>,
    ) -> Result<Self, WsError> {
        let endpoint = validate_url(url).map_err(WsError::InvalidUrl)?;
        let request = WebSocketRequest {
            url: endpoint.url.clone(),
            headers,
            subprotocol: String::new(),
        };
        let socket = WebSocket::connect(project, &request).map_err(WsError::from)?;
        Ok(Self {
            socket,
            endpoint,
            queue: FrameQueue::new(),
            bad_frames: 0,
            closed: false,
            traced: 0,
        })
    }

    /// Writes the first frames of a session to the IDE log: which `cmd`, how
    /// many bytes.
    ///
    /// The host hands a plugin only **binary** inbound messages, so a peer that
    /// writes text leaves no trace in the panel and no error anywhere. Logging
    /// the first frames makes "the socket is up and nothing arrives" read as
    /// "frames were text, the host dropped them" instead of silence. Twelve
    /// frames per session is enough to see the handshake; the rest stay quiet.
    fn trace_frame(&mut self, decoded: &FrameDecode, len: usize) {
        const TRACE_FRAMES: usize = 12;
        if self.traced > TRACE_FRAMES {
            return;
        }
        self.traced += 1;
        if self.traced > TRACE_FRAMES {
            host::log(
                openidea_plugin::LogLevel::Warn,
                "capture.ws: first 12 frames traced; later frames are not logged",
            );
            return;
        }
        let label = match decoded {
            FrameDecode::Event(_) => "event".to_owned(),
            FrameDecode::Control { cmd } => format!("cmd {cmd}"),
            FrameDecode::Bad { reason } => format!("bad: {reason}"),
        };
        host::log(
            openidea_plugin::LogLevel::Warn,
            &format!("capture.ws frame #{}: {label} ({len} bytes)", self.traced),
        );
    }

    pub fn endpoint(&self) -> &WsEndpoint {
        &self.endpoint
    }

    pub fn socket_id(&self) -> u64 {
        self.socket.id()
    }

    /// Drains the host FIFO into the bounded queue, then decodes up to
    /// `decode_limit` staged frames. Rejections are counted, never dropped
    /// silently.
    pub fn poll(&mut self, drain_limit: u32, decode_limit: usize) -> Result<PollOutcome, WsError> {
        // The host rejects anything above its own `DRAIN_BATCH` (32) instead of
        // clamping — it answered `invalid drain limit` for our old request of 256
        // on every single poll, so no frame was ever read. Ask for what it
        // accepts, and keep asking while it hands back a full batch, so a backlog
        // still catches up inside one tick.
        let per_drain = drain_request(drain_limit);
        for _ in 0..MAX_DRAIN_ROUNDS {
            let batch = match self.socket.drain(per_drain) {
                Ok(batch) => batch,
                Err(error) => {
                    // The host answers `unknown WebSocket connection` once the
                    // connection is gone. Keeping the session "open" would leave
                    // the panel's tick polling a dead socket twice a second.
                    self.closed = true;
                    return Err(WsError::from(error));
                }
            };
            let full = batch.messages.len() as u32 >= per_drain;
            for message in batch.messages {
                let _ = self.queue.push(message);
            }
            if batch.closed {
                self.closed = true;
                break;
            }
            if !full || self.queue.len() >= MAX_QUEUED_FRAMES {
                break;
            }
        }

        let mut outcome = PollOutcome::default();
        let limit = decode_limit.min(MAX_QUEUED_FRAMES);
        for _ in 0..limit {
            let Some(frame) = self.queue.pop() else {
                break;
            };
            match std::str::from_utf8(&frame) {
                Ok(text) => {
                    let decoded = decode_frame(text);
                    self.trace_frame(&decoded, frame.len());
                    match decoded {
                        FrameDecode::Event(event) => outcome.events.push(event),
                        FrameDecode::Control { cmd } => {
                            // The heartbeat carries the agent's own counters, which is
                            // the only way to tell "hooks never fired" from "the
                            // session stopped" when no SQL arrives.
                            if cmd == CMD_HEARTBEAT
                                && let Some((_, body)) = decode_envelope(text)
                                && let Some(health) = AgentHealth::from_body(&body)
                            {
                                outcome.health = Some(health);
                            }
                            outcome.controls.push(cmd);
                        }
                        FrameDecode::Bad { reason } => {
                            self.bad_frames += 1;
                            host::log(
                                openidea_plugin::LogLevel::Warn,
                                &format!("capture.ws bad frame: {reason}"),
                            );
                        }
                    }
                }
                Err(err) => {
                    self.bad_frames += 1;
                    host::log(
                        openidea_plugin::LogLevel::Warn,
                        &format!("capture.ws non-UTF-8 frame: {err}"),
                    );
                }
            }
        }
        outcome.bad_frames = self.bad_frames;
        outcome.rejected = self.queue.rejected();
        outcome.queued = self.queue.len();
        outcome.closed = self.closed;
        Ok(outcome)
    }

    /// Sends one UTF-8 payload. The host frames it as a **binary** WebSocket
    /// message (`crates/ide-host/src/plugins/websocket.rs` wraps every
    /// `websocket.send(bytes)` in `Message::Binary`, and only forwards inbound
    /// `Message::Binary` to plugins), so the agent must accept binary frames —
    /// it does, and mirrors the opcode back. The name avoids saying "text":
    /// that would be a lie the next reader could act on.
    pub fn send_payload(&self, text: &str) -> Result<(), WsError> {
        self.socket.send(text.as_bytes()).map_err(WsError::from)
    }

    /// Sends a control envelope (e.g. CONFIG: `{"disabled":true}`). The agent
    /// watchdog stops capture if no CONFIG arrives, so the ws transport must
    /// keep that channel alive exactly like TCP.
    pub fn send_control(&self, cmd: i64, body: &str) -> Result<(), WsError> {
        self.send_payload(&encode_envelope(cmd, body))
    }

    pub fn close(&mut self) -> Result<(), WsError> {
        self.closed = true;
        self.socket.close().map_err(WsError::from)
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    pub fn rejected(&self) -> u64 {
        self.queue.rejected()
    }

    pub fn bad_frames(&self) -> u64 {
        self.bad_frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn canonical() -> String {
        json!({
            "id": 7,
            "timestamp_ms": 1_700_000_000_123u64,
            "sql": "select * from t where id = ?",
            "params": [[1, "42", "Long"], [2, null, null]],
            "duration_ms": 3,
            "source": "UserMapper.java:88",
            "error": null,
        })
        .to_string()
    }

    fn agent_envelope() -> String {
        json!({
            "cmd": CMD_EVENT,
            "body": "{\"sql\":\"update t set a = ?\",\"ts\":1700000000456,\"dn\":2500000,\
                     \"err\":\"java.sql.SQLException: boom\",\"sf\":\"T.java\",\"sl\":12,\
                     \"p\":[[1,\"x\",\"String\"]]}",
        })
        .to_string()
    }

    #[test]
    fn decodes_the_canonical_event_shape() {
        match decode_frame(&canonical()) {
            FrameDecode::Event(event) => {
                assert_eq!(event.id, 7);
                assert_eq!(event.timestamp_ms, 1_700_000_000_123);
                assert_eq!(event.sql, "select * from t where id = ?");
                assert_eq!(event.params.len(), 2);
                assert_eq!(event.params[0], json!([1, "42", "Long"]));
                assert_eq!(event.duration_ms, 3);
                assert_eq!(event.source, "UserMapper.java:88");
                assert_eq!(event.error, None);
            }
            other => panic!("expected event, got {other:?}"),
        }
    }

    #[test]
    fn decodes_the_agent_envelope_and_legacy_keys() {
        match decode_frame(&agent_envelope()) {
            FrameDecode::Event(event) => {
                assert_eq!(event.sql, "update t set a = ?");
                assert_eq!(event.timestamp_ms, 1_700_000_000_456);
                assert_eq!(event.duration_ms, 2); // 2_500_000 ns
                assert_eq!(event.source, "T.java:12");
                assert_eq!(event.error.as_deref(), Some("java.sql.SQLException: boom"));
                assert_eq!(event.params, vec![json!([1, "x", "String"])]);
                assert_eq!(event.id, 0, "legacy bodies carry no id");
            }
            other => panic!("expected event, got {other:?}"),
        }
    }

    #[test]
    fn control_frames_are_not_events() {
        assert_eq!(
            decode_frame(&json!({"cmd": CMD_HEARTBEAT, "body": "{}"}).to_string()),
            FrameDecode::Control { cmd: CMD_HEARTBEAT }
        );
        assert_eq!(
            decode_frame(&json!({"cmd": CMD_HELLO, "body": "{\"pid\":1}"}).to_string()),
            FrameDecode::Control { cmd: CMD_HELLO }
        );
        assert_eq!(
            decode_frame(
                &json!({"cmd": CMD_CONFIG_ACK, "body": "{\"disabled\":false}"}).to_string()
            ),
            FrameDecode::Control {
                cmd: CMD_CONFIG_ACK
            }
        );
    }

    /// The agent's heartbeat is the only place its counters exist, so the plugin
    /// has to read them: "connected, zero events" is otherwise unattributable.
    #[test]
    fn the_heartbeat_body_becomes_agent_health() {
        // The shape `SenderLoop.heartbeatBody()` writes, verbatim.
        let body = "{\"weavable\":42,\"woven\":41,\"failed\":1,\"matched\":7,\
                    \"unmodifiable\":2,\"matchedByInterface\":3,\"interfaceWoven\":3,\
                    \"skippedByVersion\":1,\"catchAll\":0,\"gate\":1,\"dropped\":5}";
        let health = AgentHealth::from_body(body).expect("counters present");
        assert_eq!(health.weavable, 42);
        assert_eq!(health.woven, 41);
        assert_eq!(health.matched, 7);
        assert_eq!(health.interface_woven, 3);
        assert_eq!(health.skipped_by_version, 1);
        assert_eq!(health.dropped, 5);
        assert!(
            health.stopped_by_watchdog(),
            "gate=1 is the CONFIG watchdog"
        );
        assert!(health.summary().contains("woven=41"));
        assert!(health.summary().contains("gate=1"));
    }

    /// The host rejects a drain request above its own `DRAIN_BATCH` (32) instead
    /// of clamping it, so asking for our 256-frame queue made **every** poll
    /// fail with `invalid drain limit`: the panel showed a connected socket and
    /// zero rows for the whole session.
    #[test]
    fn a_drain_request_never_exceeds_the_hosts_batch_limit() {
        assert_eq!(drain_request(MAX_QUEUED_FRAMES as u32), 32);
        assert_eq!(drain_request(1), 1);
        assert_eq!(drain_request(0), 1, "0 is refused by the host too");
        assert_eq!(HOST_DRAIN_LIMIT, 32, "host DRAIN_BATCH");
        // Both relations are compile-time facts; a const block keeps clippy's
        // `assertions_on_constants` quiet without weakening them.
        const {
            assert!(HOST_DRAIN_LIMIT <= MAX_QUEUED_FRAMES);
            assert!(MAX_DRAIN_ROUNDS >= 1);
        }
    }

    #[test]
    fn a_foreign_body_is_not_health() {
        // No known counter: a different producer's heartbeat must not be read as
        // "the agent reports zero of everything".
        assert_eq!(AgentHealth::from_body("{\"pid\":7}"), None);
        assert_eq!(AgentHealth::from_body("not json"), None);
        // Partial bodies keep the keys they do carry.
        let health = AgentHealth::from_body("{\"woven\":3}").expect("one counter is enough");
        assert_eq!(health.woven, 3);
        assert_eq!(health.gate, 0);
    }

    #[test]
    fn a_heartbeat_envelope_yields_health_through_the_decode_path() {
        let body = "{\"woven\":9,\"matched\":4,\"gate\":0}";
        let frame = encode_envelope(CMD_HEARTBEAT, body);
        assert_eq!(
            decode_frame(&frame),
            FrameDecode::Control { cmd: CMD_HEARTBEAT }
        );
        let (cmd, decoded_body) = decode_envelope(&frame).expect("envelope");
        assert_eq!(cmd, CMD_HEARTBEAT);
        assert_eq!(decoded_body, body);
        assert_eq!(
            AgentHealth::from_body(&decoded_body).map(|health| health.woven),
            Some(9)
        );
    }

    #[test]
    fn bad_json_becomes_a_bad_frame() {
        match decode_frame("{not json") {
            FrameDecode::Bad { reason } => assert!(reason.starts_with("invalid json"), "{reason}"),
            other => panic!("expected bad frame, got {other:?}"),
        }
    }

    #[test]
    fn an_event_without_sql_is_a_bad_frame() {
        for frame in [
            r#"{"id":1,"timestamp_ms":2}"#,
            r#"{"cmd":1,"body":"[1,2]"}"#,
            r#"{"cmd":1}"#,
        ] {
            match decode_frame(frame) {
                FrameDecode::Bad { reason } => assert!(!reason.is_empty()),
                other => panic!("expected bad frame for {frame}, got {other:?}"),
            }
        }
    }

    #[test]
    fn frames_over_one_mib_are_rejected_explicitly() {
        let mut queue = FrameQueue::new();
        let frame = vec![b'x'; MAX_FRAME_BYTES + 1];
        assert_eq!(
            queue.push(frame),
            PushOutcome::Rejected(PushRejection::FrameTooLarge {
                size: MAX_FRAME_BYTES + 1,
                limit: MAX_FRAME_BYTES,
            })
        );
        assert_eq!(queue.len(), 0);
        assert_eq!(queue.rejected(), 1);
    }

    #[test]
    fn exactly_one_mib_is_accepted() {
        let mut queue = FrameQueue::new();
        assert_eq!(queue.push(vec![b'x'; MAX_FRAME_BYTES]), PushOutcome::Queued);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.bytes(), MAX_FRAME_BYTES);
    }

    #[test]
    fn the_frame_count_bound_is_inclusive() {
        let mut queue = FrameQueue::new();
        for _ in 0..MAX_QUEUED_FRAMES {
            assert_eq!(queue.push(b"{}".to_vec()), PushOutcome::Queued);
        }
        assert_eq!(queue.len(), MAX_QUEUED_FRAMES);
        assert!(matches!(
            queue.push(b"{}".to_vec()),
            PushOutcome::Rejected(PushRejection::QueueFull {
                frames: MAX_QUEUED_FRAMES,
                ..
            })
        ));
        assert_eq!(queue.rejected(), 1);
    }

    #[test]
    fn the_byte_bound_is_inclusive_and_fifo_order_is_kept() {
        let mut queue = FrameQueue::new();
        for index in 0..(MAX_QUEUE_BYTES / MAX_FRAME_BYTES) {
            let mut frame = vec![b'0'; MAX_FRAME_BYTES];
            frame[0] = index as u8;
            assert_eq!(queue.push(frame), PushOutcome::Queued);
        }
        assert_eq!(queue.bytes(), MAX_QUEUE_BYTES);
        assert!(matches!(
            queue.push(vec![b'x']),
            PushOutcome::Rejected(PushRejection::QueueFull { .. })
        ));
        for index in 0..(MAX_QUEUE_BYTES / MAX_FRAME_BYTES) {
            let frame = queue.pop().expect("staged frame");
            assert_eq!(frame[0], index as u8);
        }
        assert!(queue.pop().is_none());
        assert_eq!(queue.bytes(), 0);
    }

    #[test]
    fn eight_connections_are_allowed_and_the_ninth_is_refused() {
        let mut slots = ConnectionSlots::default();
        for _ in 0..MAX_CONNECTIONS {
            slots.try_open().expect("within the budget");
        }
        assert_eq!(
            slots.try_open(),
            Err(WsError::TooManyConnections {
                limit: MAX_CONNECTIONS
            })
        );
        slots.release();
        slots.try_open().expect("a released slot is reusable");
    }

    #[test]
    fn loopback_ws_urls_are_accepted() {
        for url in [
            "ws://127.0.0.1:9000/capture",
            "ws://localhost:9000",
            "ws://127.0.0.1",
        ] {
            let endpoint = validate_url(url).unwrap_or_else(|error| panic!("{url}: {error:?}"));
            assert!(!endpoint.secure);
            assert!(is_loopback_host(&endpoint.host));
        }
        let endpoint = validate_url("ws://127.0.0.1:9000/capture").unwrap();
        assert_eq!(endpoint.port, Some(9000));
        assert_eq!(endpoint.path, "/capture");
    }

    #[test]
    fn insecure_non_loopback_urls_are_rejected() {
        assert_eq!(
            validate_url("ws://capture.example:9000"),
            Err(UrlError::InsecureNonLoopback)
        );
        assert!(validate_url("wss://capture.example:9000/capture").is_ok());
    }

    #[test]
    fn urls_with_credentials_query_or_fragment_are_rejected() {
        let with_userinfo = format!("ws://user:{}@127.0.0.1:9000", "pw");
        assert_eq!(validate_url(&with_userinfo), Err(UrlError::Credentials));
        assert_eq!(
            validate_url("ws://127.0.0.1:9000/?token=1"),
            Err(UrlError::Query)
        );
        assert_eq!(
            validate_url("ws://127.0.0.1:9000/#frag"),
            Err(UrlError::Fragment)
        );
    }

    #[test]
    fn malformed_urls_are_rejected() {
        assert_eq!(
            validate_url("http://127.0.0.1:9000"),
            Err(UrlError::NotWebSocket)
        );
        assert_eq!(validate_url("ws://:9000"), Err(UrlError::MissingHost));
        assert_eq!(validate_url("ws://127.0.0.1:0"), Err(UrlError::BadPort));
        assert_eq!(validate_url("ws://127.0.0.1:70000"), Err(UrlError::BadPort));
        assert_eq!(validate_url("ws://127.0.0.1:abc"), Err(UrlError::BadPort));
        assert_eq!(
            validate_url("file://127.0.0.1:9000"),
            Err(UrlError::NotWebSocket)
        );
    }

    #[test]
    fn reconnect_backs_off_200_400_800_then_gives_up() {
        let mut plan = ReconnectPlan::new(ReconnectPolicy::default());
        assert_eq!(
            plan.on_disconnect(),
            ReconnectDecision::Reconnect {
                attempt: 1,
                delay_ms: 200
            }
        );
        assert_eq!(
            plan.on_disconnect(),
            ReconnectDecision::Reconnect {
                attempt: 2,
                delay_ms: 400
            }
        );
        assert_eq!(
            plan.on_disconnect(),
            ReconnectDecision::Reconnect {
                attempt: 3,
                delay_ms: 800
            }
        );
        assert_eq!(
            plan.on_disconnect(),
            ReconnectDecision::GiveUp {
                attempts: 4,
                message_key: KEY_GIVE_UP
            }
        );
        assert!(plan.degraded());
        assert_eq!(
            plan.on_disconnect(),
            ReconnectDecision::GiveUp {
                attempts: 4,
                message_key: KEY_GIVE_UP
            },
            "degraded is sticky until a successful connect"
        );
    }

    #[test]
    fn reconnect_delay_is_capped_at_five_seconds() {
        let policy = ReconnectPolicy {
            initial_ms: 200,
            max_ms: 5_000,
            max_attempts: 10,
        };
        let mut plan = ReconnectPlan::new(policy);
        let mut delays = Vec::new();
        for _ in 0..10 {
            match plan.on_disconnect() {
                ReconnectDecision::Reconnect { delay_ms, .. } => delays.push(delay_ms),
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(&delays[..5], &[200, 400, 800, 1_600, 3_200]);
        assert!(delays.iter().all(|delay| *delay <= 5_000), "{delays:?}");
        assert_eq!(*delays.last().unwrap(), 5_000);
    }

    #[test]
    fn a_successful_connect_resets_the_schedule() {
        let mut plan = ReconnectPlan::new(ReconnectPolicy::default());
        let _ = plan.on_disconnect();
        let _ = plan.on_disconnect();
        plan.on_connected();
        assert_eq!(plan.attempts(), 0);
        assert!(!plan.degraded());
        assert_eq!(
            plan.on_disconnect(),
            ReconnectDecision::Reconnect {
                attempt: 1,
                delay_ms: 200
            }
        );
    }

    #[test]
    fn the_supervisor_refuses_to_open_after_giving_up() {
        let mut supervisor = WsSupervisor::new(ReconnectPolicy {
            initial_ms: 200,
            max_ms: 5_000,
            max_attempts: 1,
        });
        supervisor.open().expect("first connection");
        supervisor.on_connected();
        let _ = supervisor.on_disconnected();
        assert_eq!(
            supervisor.on_disconnected(),
            ReconnectDecision::GiveUp {
                attempts: 2,
                message_key: KEY_GIVE_UP
            }
        );
        assert_eq!(
            supervisor.open(),
            Err(WsError::Degraded {
                message_key: KEY_GIVE_UP
            })
        );
        assert_eq!(supervisor.open_count(), 0);
    }

    #[test]
    fn envelopes_round_trip_escaped_control_bodies() {
        let body = "{\"disabled\":true,\"note\":\"a \\\"quoted\\\" line\\nnext\"}";
        let frame = encode_envelope(CMD_CONFIG, body);
        assert_eq!(decode_envelope(&frame), Some((CMD_CONFIG, body.to_owned())));
        assert_eq!(
            decode_frame(&frame),
            FrameDecode::Control { cmd: CMD_CONFIG }
        );
    }

    /// The agent's `WsFrameCodec.decodeEnvelope` is a hand-written parser that
    /// requires `"cmd":` first, then looks for `"body":` *after* the cmd digits.
    /// A body-first envelope makes it return null and drop the frame, which the
    /// panel sees as "connected but capturing nothing".
    ///
    /// This asserts the exact bytes, not just "it round-trips": the previous
    /// version accepted either key order (`starts_with body || starts_with cmd`)
    /// and so blessed the very bug that shipped — `serde_json`'s map is a
    /// `BTreeMap` and sorted the keys to body-first.
    #[test]
    fn the_envelope_writes_cmd_before_body_as_the_agent_expects() {
        let frame = encode_envelope(CMD_CONFIG, "{\"disabled\":false}");
        assert_eq!(frame, "{\"cmd\":3,\"body\":\"{\\\"disabled\\\":false}\"}");
        let cmd_at = frame.find("\"cmd\":").expect("cmd key");
        let body_at = frame.find("\"body\":").expect("body key");
        assert!(
            cmd_at < body_at,
            "the agent searches `\"body\":` only after the cmd digits: {frame}"
        );
        // And the digits must sit between the two keys, which is what that
        // search relies on.
        let digits_at = frame.find('3').expect("cmd value");
        assert!(cmd_at < digits_at && digits_at < body_at, "{frame}");
    }
}
