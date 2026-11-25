//! The engine JSON protocol: requests, responses, error carrier.
//!
//! Wire shape follows `docs/ARCHITECTURE.md` §3: a request carries
//! `kind` (`wrapper`/`xml`/`annotation`/`jpa`/`jdbcTemplate`/`logs`/`check`/
//! `perf`/`explain`/`schema`/`health`), an optional `source { path, text,
//! range? }`, and optional `dialect`/`params`/`entityTables`; a response carries
//! `ok`, optional `sql`/`params`, `confidence`, and arrays `notes`/`findings`/
//! `warnings`/`messageKeys`.
//!
//! ## Field naming
//!
//! The engine side is Kotlin/Java and its existing facades emit camelCase
//! (`sourceLine`, `messageKey`, …). We keep `camelCase` on the wire throughout so
//! both sides agree without a shim.
//!
//! ## Errors keep the engine's existing shape
//!
//! The current engine reports failures as `{"ok":false,"error":"<message>"}`
//! (see `EngineApi.kt`'s `errorJson`). [`ErrorValue`] accepts both that bare form
//! and a structured [`EngineError`]; [`Response::engine_error`] normalizes the
//! bare form to a code, so callers never branch on the wire shape.
//!
//! Typed builders ([`RenderRequest`] ⇒ [`Request`]) keep the wire flat and
//! schema-compatible while giving callers ergonomic structs.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The protocol revision. Bumped only on an incompatible wire change.
pub const PROTOCOL_VERSION: u32 = 1;

fn default_protocol() -> u32 {
    PROTOCOL_VERSION
}

/// A position range inside a source document (1-based line, 0-based column).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRange {
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

/// The snippet to render.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    pub path: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<SourceRange>,
}

/// Request discriminator; render sub-kinds are their own variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    Wrapper,
    Xml,
    Annotation,
    Jpa,
    JdbcTemplate,
    Logs,
    Check,
    Perf,
    Explain,
    Schema,
    Health,
    /// Live EXPLAIN execution (T5-5): the engine opens the connection and runs
    /// `EXPLAIN`; the wire string carries the dot because it is a different
    /// operation from the pure `explain` wrapper.
    #[serde(rename = "explain.execute")]
    ExplainExecute,
}

impl Kind {
    /// The wire string (`"jdbcTemplate"`, …).
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Wrapper => "wrapper",
            Kind::Xml => "xml",
            Kind::Annotation => "annotation",
            Kind::Jpa => "jpa",
            Kind::JdbcTemplate => "jdbcTemplate",
            Kind::Logs => "logs",
            Kind::Check => "check",
            Kind::Perf => "perf",
            Kind::Explain => "explain",
            Kind::Schema => "schema",
            Kind::Health => "health",
            Kind::ExplainExecute => "explain.execute",
        }
    }
}

/// The render sub-kinds a render request may carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RenderKind {
    Wrapper,
    Xml,
    Annotation,
    Jpa,
    JdbcTemplate,
    Logs,
    Check,
    Perf,
}

impl From<RenderKind> for Kind {
    fn from(kind: RenderKind) -> Self {
        match kind {
            RenderKind::Wrapper => Kind::Wrapper,
            RenderKind::Xml => Kind::Xml,
            RenderKind::Annotation => Kind::Annotation,
            RenderKind::Jpa => Kind::Jpa,
            RenderKind::JdbcTemplate => Kind::JdbcTemplate,
            RenderKind::Logs => Kind::Logs,
            RenderKind::Check => Kind::Check,
            RenderKind::Perf => Kind::Perf,
        }
    }
}

/// A flat protocol request. Build it through the typed request structs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    #[serde(default = "default_protocol")]
    pub protocol: u32,
    pub kind: Kind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dialect: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_tables: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sql: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
    // ---- explain.execute (T5-5): the engine opens the connection itself ----
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

impl Request {
    /// A request with the pinned protocol version and an explicit kind.
    pub fn new(kind: Kind) -> Self {
        Request {
            protocol: PROTOCOL_VERSION,
            kind,
            source: None,
            dialect: None,
            params: None,
            entity_tables: None,
            sql: None,
            table: None,
            url: None,
            user: None,
            password: None,
            timeout_ms: None,
        }
    }
}

/// A render request for one [`RenderKind`].
#[derive(Debug, Clone, PartialEq)]
pub struct RenderRequest {
    pub kind: RenderKind,
    pub source: Source,
    pub dialect: Option<String>,
    pub params: Option<Value>,
    pub entity_tables: Option<Value>,
}

impl From<RenderRequest> for Request {
    fn from(r: RenderRequest) -> Self {
        Request {
            protocol: PROTOCOL_VERSION,
            kind: r.kind.into(),
            source: Some(r.source),
            dialect: r.dialect,
            params: r.params,
            entity_tables: r.entity_tables,
            sql: None,
            table: None,
            url: None,
            user: None,
            password: None,
            timeout_ms: None,
        }
    }
}

/// An EXPLAIN request (execution happens engine-side; the plugin only sends SQL).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplainRequest {
    pub sql: String,
    pub dialect: Option<String>,
}

impl From<ExplainRequest> for Request {
    fn from(r: ExplainRequest) -> Self {
        Request {
            protocol: PROTOCOL_VERSION,
            kind: Kind::Explain,
            source: None,
            dialect: r.dialect,
            params: None,
            entity_tables: None,
            sql: Some(r.sql),
            table: None,
            url: None,
            user: None,
            password: None,
            timeout_ms: None,
        }
    }
}

/// A schema-metadata request for one table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaRequest {
    pub table: String,
    pub dialect: Option<String>,
}

impl From<SchemaRequest> for Request {
    fn from(r: SchemaRequest) -> Self {
        Request {
            protocol: PROTOCOL_VERSION,
            kind: Kind::Schema,
            source: None,
            dialect: r.dialect,
            params: None,
            entity_tables: None,
            sql: None,
            table: Some(r.table),
            url: None,
            user: None,
            password: None,
            timeout_ms: None,
        }
    }
}

/// Connection details for a live EXPLAIN (T5-5).
///
/// `Debug` is hand-written to redact the password: this struct travels through
/// the wasm boundary and may end up in a log line, and a credential must never
/// be printed. Serialization does include it — the engine needs it — but the
/// wire request is never logged by this crate.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnSpec {
    pub url: String,
    pub user: String,
    pub password: String,
    pub timeout_ms: u64,
}

/// The default statement/connect timeout, matching `StdioMain.DEFAULT_EXPLAIN_TIMEOUT_MS`.
pub const DEFAULT_EXPLAIN_TIMEOUT_MS: u64 = 5_000;

impl ConnSpec {
    pub fn new(
        url: impl Into<String>,
        user: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        ConnSpec {
            url: url.into(),
            user: user.into(),
            password: password.into(),
            timeout_ms: DEFAULT_EXPLAIN_TIMEOUT_MS,
        }
    }

    pub fn with_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = timeout_ms;
        self
    }
}

impl std::fmt::Debug for ConnSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnSpec")
            .field("url", &self.url)
            .field("user", &self.user)
            .field("password", &"<redacted>")
            .field("timeout_ms", &self.timeout_ms)
            .finish()
    }
}

/// A live EXPLAIN request: the engine connects, wraps the query in the
/// dialect's plan form, and returns the raw plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplainExecuteRequest {
    pub sql: String,
    pub conn: ConnSpec,
}

impl From<ExplainExecuteRequest> for Request {
    fn from(r: ExplainExecuteRequest) -> Self {
        Request {
            protocol: PROTOCOL_VERSION,
            kind: Kind::ExplainExecute,
            source: None,
            dialect: None,
            params: None,
            entity_tables: None,
            sql: Some(r.sql),
            table: None,
            url: Some(r.conn.url),
            user: Some(r.conn.user),
            password: Some(r.conn.password),
            timeout_ms: Some(r.conn.timeout_ms),
        }
    }
}

/// The engine's reply to `explain.execute`.
///
/// Success carries `format` (`"json"` / `"xml"` / `"text"`) and the raw `plan`;
/// failure carries a classified `kind`, a stable `messageKey` (the UI's
/// localization key) and a short `detail`. The engine never sends a stack.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplainExecuteResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Reused for the dispatcher's generic `{"ok":false,"error":…}` shape
    /// (bad JSON, missing kind), which has no `kind`/`messageKey`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorValue>,
}

/// A structured engine error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineError {
    /// Coarse code (`engine.failed`, `engine.timeout`, …).
    pub code: String,
    pub message: String,
    /// Optional finer kind (`timeout`/`permission`/`syntax`/`connection`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

impl EngineError {
    pub fn failed(message: impl Into<String>) -> Self {
        EngineError {
            code: "engine.failed".to_string(),
            message: message.into(),
            kind: None,
        }
    }

    pub fn with_kind(code: &str, kind: &str, message: impl Into<String>) -> Self {
        EngineError {
            code: code.to_string(),
            message: message.into(),
            kind: Some(kind.to_string()),
        }
    }
}

/// The engine's error field: a bare message (the current `errorJson`) or the
/// structured form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ErrorValue {
    Message(String),
    Structured(EngineError),
}

/// A protocol response.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sql: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Vec<Value>>,
    /// `"exact" | "partial" | "unresolved"` (stringly on the wire, per
    /// `docs/ARCHITECTURE.md` §3), kept schema-compatible with the engine side.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub message_keys: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorValue>,
}

impl Response {
    pub fn ok(sql: Option<String>, confidence: &str) -> Self {
        Response {
            ok: true,
            sql,
            confidence: Some(confidence.to_string()),
            ..Default::default()
        }
    }

    pub fn error(error: EngineError) -> Self {
        Response {
            ok: false,
            error: Some(ErrorValue::Structured(error)),
            ..Default::default()
        }
    }

    /// The error normalized to [`EngineError`]; a bare `error` string becomes
    /// `engine.failed`. `None` when the response is ok.
    pub fn engine_error(&self) -> Option<EngineError> {
        match &self.error {
            Some(ErrorValue::Structured(e)) => Some(e.clone()),
            Some(ErrorValue::Message(m)) => Some(EngineError::failed(m.clone())),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip_request(req: &Request) -> Request {
        let json = serde_json::to_string(req).unwrap();
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn render_wrapper_round_trips_with_camel_case_source() {
        let req: Request = RenderRequest {
            kind: RenderKind::Wrapper,
            source: Source {
                path: "/p/UserMapper.java".into(),
                text: "new LambdaQueryWrapper<User>().eq(User::getStatus, \"A\")".into(),
                range: Some(SourceRange {
                    start_line: 3,
                    start_column: 4,
                    end_line: 3,
                    end_column: 70,
                }),
            },
            dialect: Some("MySQL".into()),
            params: None,
            entity_tables: None,
        }
        .into();
        assert_eq!(req.kind, Kind::Wrapper);
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"kind\":\"wrapper\""), "{json}");
        assert!(
            json.contains("\"startLine\":3"),
            "camelCase on the wire: {json}"
        );
        assert_eq!(round_trip_request(&req), req);
    }

    #[test]
    fn jdbc_template_kind_uses_camel_case_on_the_wire() {
        let req: Request = RenderRequest {
            kind: RenderKind::JdbcTemplate,
            source: Source {
                path: "a.java".into(),
                text: "jdbcTemplate.query(sql)".into(),
                range: None,
            },
            dialect: None,
            params: None,
            entity_tables: None,
        }
        .into();
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"kind\":\"jdbcTemplate\""), "{json}");
        assert_eq!(req.kind.as_str(), "jdbcTemplate");
    }

    #[test]
    fn explain_request_round_trips() {
        let req: Request = ExplainRequest {
            sql: "SELECT * FROM t WHERE id = 1".into(),
            dialect: Some("PostgreSQL".into()),
        }
        .into();
        assert_eq!(req.kind, Kind::Explain);
        assert_eq!(req.sql.as_deref(), Some("SELECT * FROM t WHERE id = 1"));
        assert_eq!(round_trip_request(&req), req);
    }

    #[test]
    fn schema_request_round_trips() {
        let req: Request = SchemaRequest {
            table: "sys_user".into(),
            dialect: Some("MySQL".into()),
        }
        .into();
        assert_eq!(req.kind, Kind::Schema);
        assert_eq!(req.table.as_deref(), Some("sys_user"));
        assert_eq!(round_trip_request(&req), req);
    }

    #[test]
    fn absent_protocol_defaults_to_the_pinned_version() {
        let json = "{\"kind\":\"health\"}";
        let req: Request = serde_json::from_str(json).unwrap();
        assert_eq!(req.protocol, PROTOCOL_VERSION);
        assert_eq!(req.kind, Kind::Health);
    }

    #[test]
    fn every_kind_has_a_stable_wire_string() {
        let cases = [
            (Kind::Wrapper, "wrapper"),
            (Kind::Xml, "xml"),
            (Kind::Annotation, "annotation"),
            (Kind::Jpa, "jpa"),
            (Kind::JdbcTemplate, "jdbcTemplate"),
            (Kind::Logs, "logs"),
            (Kind::Check, "check"),
            (Kind::Perf, "perf"),
            (Kind::Explain, "explain"),
            (Kind::Schema, "schema"),
            (Kind::Health, "health"),
            (Kind::ExplainExecute, "explain.execute"),
        ];
        for (kind, wire) in cases {
            assert_eq!(serde_json::to_string(&kind).unwrap(), format!("\"{wire}\""));
            assert_eq!(kind.as_str(), wire);
        }
    }

    #[test]
    fn response_uses_camel_case_message_keys_and_omits_empty_fields() {
        let resp = Response {
            ok: true,
            sql: Some("SELECT 1".into()),
            confidence: Some("exact".to_string()),
            message_keys: vec!["engine.ok".into()],
            ..Default::default()
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"messageKeys\":[\"engine.ok\"]"), "{json}");
        assert!(json.contains("\"confidence\":\"exact\""), "{json}");
        assert!(
            !json.contains("params"),
            "empty optional fields are omitted: {json}"
        );
        let back: Response = serde_json::from_str(&json).unwrap();
        assert_eq!(back, resp);
    }

    #[test]
    fn bare_engine_error_message_normalizes_to_a_code() {
        let json = "{\"ok\":false,\"error\":\"logText is empty.\"}";
        let resp: Response = serde_json::from_str(json).unwrap();
        assert!(!resp.ok);
        let err = resp.engine_error().expect("error present");
        assert_eq!(err.code, "engine.failed");
        assert_eq!(err.message, "logText is empty.");
    }

    #[test]
    fn structured_engine_error_round_trips() {
        let resp = Response::error(EngineError::with_kind(
            "engine.timeout",
            "timeout",
            "EXPLAIN exceeded 5000 ms",
        ));
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"code\":\"engine.timeout\""), "{json}");
        let back: Response = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.engine_error().unwrap().kind.as_deref(),
            Some("timeout")
        );
    }

    #[test]
    fn ok_response_has_no_engine_error() {
        let resp = Response::ok(Some("SELECT 1".into()), "partial");
        assert!(resp.engine_error().is_none());
        assert_eq!(resp.confidence, Some("partial".to_string()));
    }

    #[test]
    fn explain_execute_request_carries_connection_fields_camel_case() {
        let req: Request = ExplainExecuteRequest {
            sql: "SELECT 1".into(),
            conn: ConnSpec::new("jdbc:sqlserver://db:1433", "sa", "test-secret")
                .with_timeout_ms(2_000),
        }
        .into();
        assert_eq!(req.kind, Kind::ExplainExecute);
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"kind\":\"explain.execute\""), "{json}");
        assert!(json.contains("\"timeoutMs\":2000"), "{json}");
        assert_eq!(round_trip_request(&req), req);
    }

    #[test]
    fn conn_spec_debug_never_prints_the_password() {
        let spec = ConnSpec::new("jdbc:postgresql://db/x", "u", "test-secret");
        let text = format!("{spec:?}");
        assert!(text.contains("<redacted>"), "{text}");
        assert!(!text.contains("test-secret"), "{text}");
        // The wire form still carries it — the engine needs the credential.
        let json = serde_json::to_string(&spec).unwrap();
        assert!(json.contains("\"password\":\"test-secret\""), "{json}");
        assert!(json.contains("\"timeoutMs\":5000"), "{json}");
    }

    #[test]
    fn explain_execute_response_parses_success_and_classified_failure() {
        let ok: ExplainExecuteResponse =
            serde_json::from_str(r#"{"ok":true,"format":"xml","plan":"<ShowPlanXML/>"}"#).unwrap();
        assert_eq!(ok.format.as_deref(), Some("xml"));
        assert_eq!(ok.plan.as_deref(), Some("<ShowPlanXML/>"));

        let bad: ExplainExecuteResponse = serde_json::from_str(
            r#"{"ok":false,"kind":"timeout","messageKey":"explain.error.timeout","detail":"57014"}"#,
        )
        .unwrap();
        assert!(!bad.ok);
        assert_eq!(bad.kind.as_deref(), Some("timeout"));
        assert_eq!(bad.message_key.as_deref(), Some("explain.error.timeout"));

        let dispatch: ExplainExecuteResponse =
            serde_json::from_str(r#"{"ok":false,"kind":"invalid","error":"missing kind"}"#)
                .unwrap();
        assert!(dispatch.error.is_some());
    }
}
