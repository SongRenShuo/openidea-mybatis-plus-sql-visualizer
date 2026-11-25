//! Command-side bridge for the live capture chain (T4-2/T4-4).
//!
//! Lane L7 owns the transport ([`crate::capture::ws`]), the tail and the refresh
//! policy; lane L5 owns the panels. This module is the wiring between them and
//! the commands: it keeps the one live [`WsConsumer`] for the process, turns
//! decoded [`CaptureEvent`]s into the log text the panel already parses, and
//! throttles `refresh_view` through [`RefreshState`].
//!
//! Rules it enforces:
//!
//! - **no panic**: every host failure is returned as [`BridgeError`] with a
//!   stable message key (`capture.ws.host_error` for transport errors);
//! - **bounded work**: one poll drains at most [`MAX_QUEUED_FRAMES`] frames and
//!   never sleeps;
//! - **no invented port**: the port is the command argument, then the
//!   `capture.port` setting, then [`DEFAULT_PORT`]; the agent abandonnne quand
//!   `port <= 0`, so the output always says the agent needs `port=`.

use std::fmt::Write as _;
use std::sync::{Mutex, MutexGuard, OnceLock};

use openidea_plugin::host::{self, Project};
use openidea_plugin::{Error, EventKind, Invocation, LogLevel};
use serde_json::Value;

use crate::capture::refresh::{RefreshAction, RefreshPolicy, RefreshState};
use crate::capture::ws::{CMD_CONFIG, CaptureEvent, MAX_QUEUED_FRAMES, WsConsumer, WsError};
use crate::core::log::{LogEntry, ParseOptions, parse};
use crate::settings;
use crate::views::log_panel;

/// Port the plugin tells the agent to bind when nothing says otherwise.
pub const DEFAULT_PORT: u16 = 17890;
/// Setting the user can pin the port in (read-only for this lane).
pub const PORT_SETTING: &str = "capture.port";
/// Path the agent's WebSocket transport listens on.
pub const WS_PATH: &str = "/mpsv";

/// Entries the `capture-open-log` output admits it is not showing.
pub const PAGE_SIZE_SETTING: &str = "logs.pageSize";
pub const DEFAULT_PAGE_SIZE: usize = 50;
pub const MAX_PAGE_SIZE: usize = 5000;

pub const KEY_BAD_PORT: &str = "capture.port.invalid";
pub const KEY_GUIDANCE: &str = "capture.port.guidance";
pub const KEY_ALREADY_CONNECTED: &str = "capture.start.already_connected";
pub const KEY_NOT_CONNECTED: &str = "capture.status.not_connected";
pub const KEY_FILE_HINT: &str = "capture.status.file_hint";
pub const KEY_OPEN_LOG_LIMITED: &str = "capture.open_log.page_limited";
pub const KEY_OPEN_LOG_NO_ENTRIES: &str = "capture.open_log.no_entries";
pub const KEY_POLL_NOT_CONNECTED: &str = "capture.poll.not_connected";
pub const KEY_VIEW_UNAVAILABLE: &str = "capture.view.unavailable";

// ---------------------------------------------------------------------------
// Port resolution
// ---------------------------------------------------------------------------

/// A rejected port. `detail` is machine-readable, the text is localised by the
/// registration's key table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortError {
    pub key: &'static str,
    pub detail: String,
}

impl PortError {
    fn invalid(raw: &str, why: &str) -> Self {
        Self {
            key: KEY_BAD_PORT,
            detail: format!("{raw}: {why}"),
        }
    }
}

/// `arg`, then the `capture.port` setting, then [`DEFAULT_PORT`].
///
/// An empty or blank argument counts as absent. A value outside `1..=65535`
/// (including the agent's "give up" value `0`) is an error, never a silent
/// fallback: the agent would not start and the user would blame the plugin.
pub fn resolve_port(arg: Option<&str>, setting: Option<i64>) -> Result<u16, PortError> {
    if let Some(raw) = arg.map(str::trim).filter(|text| !text.is_empty()) {
        let value: i64 = raw
            .parse()
            .map_err(|_| PortError::invalid(raw, "not a number"))?;
        return checked_port(value);
    }
    match setting {
        Some(value) => checked_port(value),
        None => Ok(DEFAULT_PORT),
    }
}

fn checked_port(value: i64) -> Result<u16, PortError> {
    if (1..=65535).contains(&value) {
        Ok(value as u16)
    } else {
        Err(PortError::invalid(
            &value.to_string(),
            "port must be 1-65535; the agent does not report on port 0",
        ))
    }
}

/// The URL the plugin connects to; loopback `ws://` is the only transport the
/// agent exposes (see [`crate::capture::ws::validate_url`]).
pub fn capture_url(port: u16) -> String {
    format!("ws://127.0.0.1:{port}{WS_PATH}")
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Everything the bridge can refuse, with the message key the locale table maps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeError {
    Ws(WsError),
    Port(PortError),
    AlreadyConnected,
    NotConnected,
}

impl BridgeError {
    pub fn message_key(&self) -> &'static str {
        match self {
            Self::Ws(error) => error.message_key(),
            Self::Port(error) => error.key,
            Self::AlreadyConnected => KEY_ALREADY_CONNECTED,
            Self::NotConnected => KEY_NOT_CONNECTED,
        }
    }

    pub fn detail(&self) -> String {
        match self {
            Self::Ws(error) => error.to_string(),
            Self::Port(error) => error.detail.clone(),
            Self::AlreadyConnected => "one capture WebSocket is already open".to_string(),
            Self::NotConnected => "no capture WebSocket is open".to_string(),
        }
    }

    pub fn into_error(self) -> Error {
        Error::Failed(format!("{}: {}", self.message_key(), self.detail()))
    }
}

impl From<WsError> for BridgeError {
    fn from(error: WsError) -> Self {
        Self::Ws(error)
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

struct Session {
    consumer: WsConsumer,
    port: u16,
    url: String,
    /// Decoded frames (events + controls + malformed), cumulative.
    frames: u64,
    events: u64,
    bad_frames: u64,
    last_error: Option<String>,
    refresh: RefreshState,
}

static SESSION: OnceLock<Mutex<Option<Session>>> = OnceLock::new();
static SUBSCRIBED: OnceLock<Mutex<bool>> = OnceLock::new();

fn slot() -> MutexGuard<'static, Option<Session>> {
    let mutex = SESSION.get_or_init(|| Mutex::new(None));
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn subscribed() -> MutexGuard<'static, bool> {
    let mutex = SUBSCRIBED.get_or_init(|| Mutex::new(false));
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A snapshot of the capture chain for `capture-status` and `capture-stop`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    pub connected: bool,
    pub port: Option<u16>,
    pub url: Option<String>,
    pub socket_id: Option<u64>,
    pub frames: u64,
    pub events: u64,
    pub bad_frames: u64,
    pub dropped: u64,
    pub queued: usize,
    pub last_error: Option<String>,
}

impl Stats {
    fn of(session: &Session) -> Self {
        Self {
            connected: !session.consumer.is_closed(),
            port: Some(session.port),
            url: Some(session.url.clone()),
            socket_id: Some(session.consumer.socket_id()),
            frames: session.frames,
            events: session.events,
            bad_frames: session.bad_frames,
            dropped: session.consumer.rejected(),
            queued: session.consumer.queued(),
            last_error: session.last_error.clone(),
        }
    }
}

/// One `capture-poll` result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PollReport {
    /// Frames drained from the host FIFO and decoded in this call.
    pub fetched: usize,
    pub events: usize,
    /// Log entries the panel text yielded for those events.
    pub entries: usize,
    pub controls: usize,
    /// Cumulative malformed frames.
    pub bad_frames: u64,
    /// Cumulative frames rejected by the bounded queue.
    pub dropped: u64,
    pub queued: usize,
    pub closed: bool,
    /// Characters appended to the panel text.
    pub appended: usize,
    /// Whether the throttle window let this batch trigger `refresh_view`.
    pub refresh: bool,
}

impl Stats {
    /// Read the state without touching the socket.
    pub fn current() -> Self {
        match slot().as_ref() {
            Some(session) => Self::of(session),
            None => Self::default(),
        }
    }
}

/// Opens the one capture WebSocket and sends the initial `CONFIG` frame the
/// agent watchdog expects. Returns the host socket id.
pub fn connect(project: &Project, url: &str) -> Result<u64, BridgeError> {
    let mut guard = slot();
    if let Some(existing) = guard.as_ref()
        && !existing.consumer.is_closed()
    {
        return Err(BridgeError::AlreadyConnected);
    }
    let consumer = WsConsumer::connect(project, url, Vec::new())?;
    let endpoint = consumer.endpoint().clone();
    let session = Session {
        port: endpoint.port.unwrap_or(DEFAULT_PORT),
        url: endpoint.url.clone(),
        frames: 0,
        events: 0,
        bad_frames: 0,
        last_error: None,
        refresh: RefreshState::new(refresh_policy()),
        consumer,
    };
    // The agent stops capture when no CONFIG arrives, so a ws connection that
    // never sends one would look connected while capturing nothing.
    session
        .consumer
        .send_control(CMD_CONFIG, "{\"disabled\":false}")?;
    let id = session.consumer.socket_id();
    *guard = Some(session);
    Ok(id)
}

fn refresh_policy() -> RefreshPolicy {
    let interval = settings::get_i64(
        "log.refreshIntervalMs",
        crate::capture::refresh::DEFAULT_THROTTLE_MS as i64,
    )
    .max(1) as u64;
    RefreshPolicy::new(interval)
}

/// Subscribes to `websocket-ready` once per process. Returns whether this call
/// did the subscription (so the command can say so).
pub fn ensure_websocket_subscription() -> bool {
    let mut done = subscribed();
    if *done {
        return false;
    }
    match host::subscribe(EventKind::WebsocketReady) {
        Ok(()) => {
            *done = true;
            true
        }
        Err(error) => {
            host::log(
                LogLevel::Warn,
                &format!("capture: websocket-ready subscription failed: {error}"),
            );
            false
        }
    }
}

/// Asks the agent to stop, closes the socket and returns the final statistics.
pub fn disconnect() -> Result<Stats, BridgeError> {
    let mut guard = slot();
    let Some(mut session) = guard.take() else {
        return Err(BridgeError::NotConnected);
    };
    let mut stats = Stats::of(&session);
    if let Err(error) = session
        .consumer
        .send_control(CMD_CONFIG, "{\"disabled\":true}")
    {
        session.last_error = Some(error.to_string());
        stats.last_error = session.last_error.clone();
    }
    if let Err(error) = session.consumer.close() {
        session.last_error = Some(error.to_string());
        stats.last_error = session.last_error.clone();
    }
    stats.connected = false;
    Ok(stats)
}

/// Drops every staged panel row (the panel's own Clear button does this too);
/// used by tests before a fresh session.
#[cfg(test)]
fn reset_for_tests() {
    let _ = disconnect();
}

/// Serialises every test that touches the process-wide session, across modules
/// (`capture::*` and `commands::capture_*` tests both take it).
#[cfg(test)]
pub(crate) fn test_guard() -> MutexGuard<'static, ()> {
    static TEST_LOCK: Mutex<()> = Mutex::new(());
    let guard = TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    reset_for_tests();
    guard
}

// ---------------------------------------------------------------------------
// Polling
// ---------------------------------------------------------------------------

/// Drains up to [`MAX_QUEUED_FRAMES`] queued frames, appends their log text to
/// the panel cache and refreshes the panel when the throttle allows.
///
/// `sink` receives the text instead of the panel; tests use it to keep the
/// process-wide panel store untouched.
fn poll_inner(now_ms: u64, sink: &mut dyn FnMut(&str)) -> Result<PollReport, BridgeError> {
    let mut guard = slot();
    let Some(session) = guard.as_mut() else {
        return Err(BridgeError::NotConnected);
    };
    let outcome = match session
        .consumer
        .poll(MAX_QUEUED_FRAMES as u32, MAX_QUEUED_FRAMES)
    {
        Ok(outcome) => outcome,
        Err(error) => {
            session.last_error = Some(error.to_string());
            return Err(BridgeError::Ws(error));
        }
    };

    let bad_delta = outcome.bad_frames.saturating_sub(session.bad_frames);
    session.bad_frames = outcome.bad_frames;
    session.frames += outcome.events.len() as u64 + outcome.controls.len() as u64 + bad_delta;
    session.events += outcome.events.len() as u64;

    let options = parse_options();
    let mut text = String::new();
    let mut entries = 0usize;
    for event in &outcome.events {
        let chunk = event_text(event);
        entries += parse(&chunk, &options).entries.len();
        text.push_str(&chunk);
    }
    if !text.is_empty() {
        sink(&text);
    }

    let refresh = matches!(
        session.refresh.on_items(outcome.events.len(), now_ms),
        RefreshAction::Refresh { .. }
    );
    if outcome.closed {
        session.last_error = Some("the peer closed the capture WebSocket".to_string());
    }

    Ok(PollReport {
        fetched: outcome.events.len() + outcome.controls.len() + bad_delta as usize,
        events: outcome.events.len(),
        entries,
        controls: outcome.controls.len(),
        bad_frames: outcome.bad_frames,
        dropped: outcome.rejected,
        queued: outcome.queued,
        closed: outcome.closed,
        appended: text.len(),
        refresh,
    })
}

/// The command/event entry point: append to the panel and refresh it.
pub fn poll(now_ms: u64) -> Result<PollReport, BridgeError> {
    let report = poll_inner(now_ms, &mut |text| log_panel::append_text(text))?;
    if report.refresh {
        host::refresh_view(log_panel::VIEW_ID);
    }
    Ok(report)
}

/// `websocket-ready` handler; never panics and never blocks. A stale event
/// (no session, or a session that raced `capture-stop`) is silently ignored.
pub fn on_websocket_ready(_ctx: &Invocation) -> Result<(), Error> {
    match poll(log_panel::now_ms()) {
        Ok(_) => Ok(()),
        Err(BridgeError::NotConnected) => Ok(()),
        Err(error) => {
            host::log(
                LogLevel::Warn,
                &format!("capture: {} {}", error.message_key(), error.detail()),
            );
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Frame → panel text → LogEntry
// ---------------------------------------------------------------------------

/// Parses one event's panel text the same way the panel does. This is the
/// contract the acceptance tests pin; `poll` uses it per frame to count entries.
pub fn event_log_entry(event: &CaptureEvent) -> Option<LogEntry> {
    parse(&event_text(event), &parse_options())
        .entries
        .into_iter()
        .next()
}

/// The log-family text one frame becomes.
///
/// - events with parameters: a MyBatis `Preparing:` + `Parameters:` pair, so
///   the panel restores the SQL and shows the rendered literals;
/// - events without parameters that still carry `?`: a p6spy `statement` row,
///   which keeps the timestamp and duration and marks the statement `partial`
///   (`capture` never invents values);
/// - events with no parameters and no placeholder: a MyBatis `Executing:` row;
/// - `error` is appended as a SQL-error row so the panel classifies it.
pub fn event_text(event: &CaptureEvent) -> String {
    let mut out = String::new();
    let stamp = iso_stamp(event.timestamp_ms);
    if !event.params.is_empty() {
        let _ = writeln!(out, "{stamp}Preparing: {}", event.sql);
        let _ = writeln!(out, "Parameters: {}", params_text(&event.params));
        if event.duration_ms > 0 {
            let _ = writeln!(out, "elapsed {} ms", event.duration_ms);
        }
    } else if event.sql.contains('?') {
        // `elapsed N ms` in the duration field, not a bare number: p6spy's
        // extraction would otherwise take a small timestamp for the duration.
        let _ = writeln!(
            out,
            "{}|elapsed {} ms|statement|{}|",
            event.timestamp_ms,
            event.duration_ms,
            one_line(&event.sql)
        );
    } else if !event.sql.trim().is_empty() {
        let _ = writeln!(out, "{stamp}Executing: {}", one_line(&event.sql));
        if event.duration_ms > 0 {
            let _ = writeln!(out, "elapsed {} ms", event.duration_ms);
        }
    }
    if let Some(error) = &event.error {
        let _ = writeln!(out, "SQLException: {}", one_line(error));
    }
    out
}

fn one_line(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

fn iso_stamp(ms: u64) -> String {
    if ms == 0 {
        return String::new();
    }
    let seconds = (ms / 1000) as i64;
    let millis = ms % 1000;
    let days = seconds.div_euclid(86_400);
    let seconds_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3600;
    let minute = (seconds_of_day % 3600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}.{millis:03}")
}

/// Howard Hinnant's civil-from-days (UTC), the same arithmetic the panel uses.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = (z - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// `1(Long), x(String), null` — the MyBatis `Parameters:` payload for the
/// structured agent parameters (`[[index, value, type], …]`).
pub fn params_text(params: &[Value]) -> String {
    params.iter().map(param_text).collect::<Vec<_>>().join(", ")
}

fn param_text(value: &Value) -> String {
    let (val, ty) = param_parts(value);
    match val {
        None => "null".to_string(),
        Some(text) => {
            let text = one_line(&text);
            match ty {
                Some(ty) if !ty.is_empty() => format!("{text}({ty})"),
                _ => format!("{text}({})", inferred_type(value)),
            }
        }
    }
}

/// Splits one parameter into `(value text, type annotation)`.
///
/// Accepted shapes: the agent tuple `[index, value, type]`, an object
/// `{value, type}`, or the bare value.
fn param_parts(value: &Value) -> (Option<String>, Option<String>) {
    match value {
        Value::Array(items) => {
            let inner = items.get(1).cloned().unwrap_or(Value::Null);
            let ty = items.get(2).and_then(Value::as_str).map(str::to_string);
            (value_text(&inner), ty)
        }
        Value::Object(map) => {
            let inner = map
                .get("value")
                .or_else(|| map.get("val"))
                .cloned()
                .unwrap_or(Value::Null);
            let ty = map
                .get("type")
                .or_else(|| map.get("ty"))
                .and_then(Value::as_str)
                .map(str::to_string);
            (value_text(&inner), ty)
        }
        other => (value_text(other), None),
    }
}

fn value_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Number(number) => Some(number.to_string()),
        other => Some(other.to_string()),
    }
}

/// Type used when the agent sent no annotation: the JSON kind is authoritative.
fn inferred_type(value: &Value) -> &'static str {
    match value {
        Value::Number(_) => "Long",
        Value::Bool(_) => "Boolean",
        _ => "String",
    }
}

/// Parse options matching the panel's (`log.*` settings), so `capture-open-log`
/// analyses a file exactly as the panel would.
pub fn parse_options() -> ParseOptions {
    let window = settings::get_i64("log.dedupeWindowMs", 0);
    ParseOptions {
        slow_threshold_ms: settings::get_i64("log.slowThresholdMs", 1000).max(0) as f64,
        n_plus_one_threshold: settings::get_i64("log.nPlusOneThreshold", 5).max(2) as usize,
        ignore_params_in_dedupe: settings::get_bool("log.ignoreParamsInDedupe", false),
        dedupe_window_ms: (window > 0).then_some(window),
        max_sql_len: settings::get_i64("log.maxSqlLength", 20_000).max(256) as usize,
        timestamp_offset_minutes: settings::get_i64("log.timestampOffsetMinutes", 0) as i32,
        ..ParseOptions::default()
    }
}

/// `logs.pageSize`, clamped to the same range `render-log` uses.
pub fn panel_page_size() -> usize {
    match host::setting_number(PAGE_SIZE_SETTING) {
        Some(value) if value >= 1.0 => (value as usize).clamp(1, MAX_PAGE_SIZE),
        _ => DEFAULT_PAGE_SIZE,
    }
}

/// The `{op:"load"}` message for the workbench page (T2-2 contract).
pub fn workbench_payload(sql: &str, dialect: &str, source: Option<&str>) -> String {
    let confidence = if sql.trim().is_empty() { "" } else { "exact" };
    let mut model = serde_json::json!({
        "sql": sql,
        "params": [],
        "dialect": dialect,
        "confidence": confidence,
    });
    if let Some(source) = source
        && let Some(object) = model.as_object_mut()
    {
        object.insert("source".to_string(), Value::String(source.to_string()));
    }
    serde_json::json!({
        "op": "load",
        "model": model,
        "labels": {
            "sql": crate::i18n::t("view.workbench.sql"),
            "params": crate::i18n::t("view.workbench.params"),
            "dialect": crate::i18n::t("view.workbench.dialect"),
            "copy": crate::i18n::t("view.workbench.copy"),
            "export": crate::i18n::t("view.workbench.export"),
            "empty": crate::i18n::t("view.workbench.empty"),
            "confidence": crate::i18n::t("view.workbench.confidence"),
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::log::Confidence;
    use openidea_plugin::mock::MockHost;
    use serde_json::json;

    fn guard() -> std::sync::MutexGuard<'static, ()> {
        test_guard()
    }

    /// The mock host only accepts `wss://`; the production URL stays `ws://`
    /// (loopback), which the real host allows.
    fn test_url(port: u16) -> String {
        format!("wss://127.0.0.1:{port}{WS_PATH}")
    }

    fn install() -> MockHost {
        MockHost::install().with_capabilities(&["project.read", "events", "net:127.0.0.1"])
    }

    fn project(host: &MockHost) -> Project {
        host.invocation().project().expect("the mock project")
    }

    fn event(
        sql: &str,
        params: Value,
        timestamp_ms: u64,
        duration_ms: u64,
        error: Option<&str>,
    ) -> CaptureEvent {
        CaptureEvent {
            id: 1,
            timestamp_ms,
            sql: sql.to_string(),
            params: params.as_array().cloned().unwrap_or_default(),
            duration_ms,
            source: "T.java:1".to_string(),
            error: error.map(str::to_string),
        }
    }

    // -- port resolution ---------------------------------------------------

    #[test]
    fn port_defaults_to_17890_when_nothing_is_set() {
        assert_eq!(resolve_port(None, None), Ok(17890));
        assert_eq!(resolve_port(Some("   "), None), Ok(17890));
    }

    #[test]
    fn port_setting_fills_in_when_the_argument_is_absent() {
        assert_eq!(resolve_port(None, Some(18000)), Ok(18000));
        assert_eq!(resolve_port(Some(""), Some(18001)), Ok(18001));
    }

    #[test]
    fn port_argument_wins_over_the_setting() {
        assert_eq!(resolve_port(Some("19000"), Some(18000)), Ok(19000));
        assert_eq!(resolve_port(Some(" 19001 "), None), Ok(19001));
    }

    #[test]
    fn an_invalid_port_is_refused_with_the_key() {
        for raw in ["abc", "0", "-1", "65536", "70000"] {
            let error = resolve_port(Some(raw), None).unwrap_err();
            assert_eq!(error.key, KEY_BAD_PORT, "{raw}");
            assert!(error.detail.starts_with(raw.trim()), "{raw}");
        }
        let error = resolve_port(None, Some(0)).unwrap_err();
        assert_eq!(error.key, KEY_BAD_PORT);
    }

    #[test]
    fn the_url_is_loopback_ws_on_the_mpsv_path() {
        assert_eq!(capture_url(17890), "ws://127.0.0.1:17890/mpsv");
    }

    // -- frame → LogEntry --------------------------------------------------

    #[test]
    fn a_frame_with_a_numeric_parameter_restores_bare_sql() {
        let _host = MockHost::install();
        let event = event(
            "select * from user where id = ?",
            json!([[1, "42", "Long"]]),
            1_700_000_000_123,
            3,
            None,
        );
        let entry = event_log_entry(&event).expect("one entry");
        assert_eq!(entry.sql, "select * from user where id = 42");
        assert_eq!(entry.raw_sql, "select * from user where id = ?");
        assert_eq!(entry.params, vec!["42".to_string()]);
        assert_eq!(entry.class, crate::core::log::EntryClass::Select);
        assert_eq!(entry.confidence, Confidence::Exact);
        assert_eq!(entry.timestamp_ms, Some(1_700_000_000_123));
        assert_eq!(entry.duration_ms, Some(3.0));
        assert_eq!(entry.family, crate::core::log::Family::MyBatis);
    }

    #[test]
    fn a_string_parameter_is_quoted() {
        let _host = MockHost::install();
        let event = event(
            "update user set status = ? where id = ?",
            json!([[1, "active", "String"], [2, 7, "Long"]]),
            0,
            0,
            None,
        );
        let entry = event_log_entry(&event).expect("one entry");
        assert!(
            entry.sql.contains("status = 'active'"),
            "sql: {}",
            entry.sql
        );
        assert_eq!(entry.params, vec!["'active'".to_string(), "7".to_string()]);
        assert_eq!(entry.class, crate::core::log::EntryClass::Update);
    }

    #[test]
    fn a_null_parameter_renders_as_sql_null() {
        let _host = MockHost::install();
        let event = event(
            "delete from user where id = ?",
            json!([[1, null, null]]),
            0,
            0,
            None,
        );
        let entry = event_log_entry(&event).expect("one entry");
        assert!(entry.sql.contains("id = NULL"), "sql: {}", entry.sql);
        assert_eq!(entry.params, vec!["NULL".to_string()]);
        assert_eq!(entry.class, crate::core::log::EntryClass::Delete);
    }

    #[test]
    fn a_parameterless_frame_is_exact() {
        let _host = MockHost::install();
        let event = event("select count(*) from user", json!([]), 0, 9, None);
        let entry = event_log_entry(&event).expect("one entry");
        assert_eq!(entry.sql, "select count(*) from user");
        assert!(entry.params.is_empty());
        assert_eq!(entry.confidence, Confidence::Exact);
        assert_eq!(entry.duration_ms, Some(9.0));
    }

    #[test]
    fn a_parameterless_frame_with_placeholders_is_partial() {
        let _host = MockHost::install();
        let event = event("select * from user where id = ?", json!([]), 5, 2, None);
        let entry = event_log_entry(&event).expect("one entry");
        assert_eq!(entry.confidence, Confidence::Partial);
        assert!(
            entry
                .message_keys
                .iter()
                .any(|key| key == "log.entry.params_missing")
        );
        assert_eq!(entry.duration_ms, Some(2.0));
    }

    #[test]
    fn an_error_frame_becomes_an_unresolved_error_row() {
        let _host = MockHost::install();
        let event = event("", json!([]), 0, 0, Some("java.sql.SQLException: boom"));
        let entry = event_log_entry(&event).expect("one entry");
        assert_eq!(entry.class, crate::core::log::EntryClass::Error);
        assert_eq!(entry.confidence, Confidence::Unresolved);
        assert!(!entry.has_sql());
        assert!(
            entry
                .raw_line
                .as_deref()
                .is_some_and(|line| line.contains("boom"))
        );
    }

    #[test]
    fn the_iso_stamp_matches_the_panel_format() {
        assert_eq!(iso_stamp(1_700_000_000_123), "2023-11-14 22:13:20.123");
        assert_eq!(iso_stamp(0), "");
    }

    // -- session / poll / bad frames --------------------------------------

    #[test]
    fn connect_and_disconnect_round_trip_and_refuse_a_second_socket() {
        let host = install();
        let _guard = guard();
        let url = test_url(17890);
        let socket = connect(&project(&host), &url).expect("connect");
        assert!(socket >= 1);

        let stats = Stats::current();
        assert!(stats.connected);
        assert_eq!(stats.port, Some(17890));
        assert_eq!(stats.url.as_deref(), Some(url.as_str()));
        assert_eq!(stats.socket_id, Some(socket));

        assert_eq!(
            connect(&project(&host), &url).unwrap_err(),
            BridgeError::AlreadyConnected
        );

        let closed = disconnect().expect("disconnect");
        assert!(!closed.connected);
        assert!(!Stats::current().connected);
        assert_eq!(disconnect().unwrap_err(), BridgeError::NotConnected);
    }

    #[test]
    fn a_good_frame_is_appended_and_a_bad_one_is_counted() {
        let host = install();
        let _guard = guard();
        let socket = connect(&project(&host), &test_url(17891)).expect("connect");
        let good = json!({
            "cmd": 1,
            "body": "{\"sql\":\"select 1\",\"ts\":1700000000000}",
        })
        .to_string();
        host.push_websocket_message(socket, good.as_bytes())
            .unwrap();
        host.push_websocket_message(socket, b"{not json").unwrap();

        let mut sink = String::new();
        let report = poll_inner(1_000, &mut |text| sink.push_str(text)).expect("poll");
        assert_eq!(report.events, 1);
        assert_eq!(report.entries, 1);
        assert_eq!(report.bad_frames, 1);
        assert_eq!(report.fetched, 2);
        assert!(sink.contains("Executing: select 1"), "sink: {sink}");

        host.push_websocket_message(socket, b"[1,2]").unwrap();
        let report = poll_inner(1_100, &mut |_| {}).expect("poll");
        assert_eq!(report.events, 0);
        assert_eq!(report.bad_frames, 2);
        assert_eq!(Stats::current().frames, 3);
        assert_eq!(Stats::current().events, 1);
    }

    #[test]
    fn a_silent_close_is_reported() {
        let host = install();
        let _guard = guard();
        let socket = connect(&project(&host), &test_url(17892)).expect("connect");
        host.close_websocket(socket).unwrap();
        let report = poll_inner(1_000, &mut |_| {}).expect("poll");
        assert!(report.closed);
        assert!(!Stats::current().connected);
        assert!(Stats::current().last_error.is_some());
    }

    // -- throttle state machine -------------------------------------------

    #[test]
    fn the_first_batch_refreshes_immediately() {
        let host = install();
        let _guard = guard();
        let socket = connect(&project(&host), &test_url(17893)).expect("connect");
        host.push_websocket_message(socket, &frame("select 1"))
            .unwrap();
        let report = poll_inner(10_000, &mut |_| {}).expect("poll");
        assert!(report.refresh);
    }

    #[test]
    fn a_burst_inside_the_window_does_not_refresh_again() {
        let host = install();
        let _guard = guard();
        let socket = connect(&project(&host), &test_url(17894)).expect("connect");
        host.push_websocket_message(socket, &frame("select 1"))
            .unwrap();
        assert!(poll_inner(10_000, &mut |_| {}).unwrap().refresh);
        host.push_websocket_message(socket, &frame("select 2"))
            .unwrap();
        assert!(!poll_inner(10_100, &mut |_| {}).unwrap().refresh);
        host.push_websocket_message(socket, &frame("select 3"))
            .unwrap();
        assert!(!poll_inner(10_249, &mut |_| {}).unwrap().refresh);
    }

    #[test]
    fn the_next_window_refreshes_with_the_merged_batch() {
        let host = install();
        let _guard = guard();
        let socket = connect(&project(&host), &test_url(17895)).expect("connect");
        host.push_websocket_message(socket, &frame("select 1"))
            .unwrap();
        assert!(poll_inner(10_000, &mut |_| {}).unwrap().refresh);
        host.push_websocket_message(socket, &frame("select 2"))
            .unwrap();
        assert!(!poll_inner(10_100, &mut |_| {}).unwrap().refresh);
        host.push_websocket_message(socket, &frame("select 3"))
            .unwrap();
        assert!(poll_inner(10_250, &mut |_| {}).unwrap().refresh);
    }

    #[test]
    fn polling_without_a_session_is_a_readable_error() {
        let _guard = guard();
        assert_eq!(
            poll_inner(0, &mut |_| {}).unwrap_err(),
            BridgeError::NotConnected
        );
        assert_eq!(BridgeError::NotConnected.message_key(), KEY_NOT_CONNECTED);
    }

    fn frame(sql: &str) -> Vec<u8> {
        json!({
            "cmd": 1,
            "body": json!({"sql": sql, "ts": 1_700_000_000_000u64}).to_string(),
        })
        .to_string()
        .into_bytes()
    }

    // -- workbench payload -------------------------------------------------

    #[test]
    fn the_workbench_payload_carries_the_model_and_labels() {
        let _host = MockHost::install();
        let payload = workbench_payload(
            "select * from user",
            "MySQL",
            Some("UserMapper.xml#findAll"),
        );
        let value: Value = serde_json::from_str(&payload).expect("valid JSON");
        assert_eq!(value["op"], "load");
        assert_eq!(value["model"]["sql"], "select * from user");
        assert_eq!(value["model"]["params"], json!([]));
        assert_eq!(value["model"]["dialect"], "MySQL");
        assert_eq!(value["model"]["confidence"], "exact");
        assert_eq!(value["model"]["source"], "UserMapper.xml#findAll");
        assert!(value["labels"]["sql"].as_str().is_some());
        assert!(value["labels"]["confidence"].as_str().is_some());
    }

    #[test]
    fn an_empty_workbench_payload_claims_no_confidence() {
        let _host = MockHost::install();
        let value: Value =
            serde_json::from_str(&workbench_payload("", "PostgreSQL", None)).expect("JSON");
        assert_eq!(value["model"]["sql"], "");
        assert_eq!(value["model"]["confidence"], "");
        assert!(value["model"].get("source").is_none());
    }
}
