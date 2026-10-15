//! `domain::acp::session` — the session lifecycle request /
//! response types.
//!
//! Source of truth:
//! `crates/protocol-droid-schema/spec/acp/v1.25.0/schema.json`
//! under `$defs.NewSessionRequest`, `$defs.LoadSessionRequest`,
//! `$defs.ListSessionsRequest`, `$defs.DeleteSessionRequest`,
//! `$defs.ResumeSessionRequest`, `$defs.CloseSessionRequest`,
//! `$defs.PromptRequest`, `$defs.CancelNotification`,
//! `$defs.LogoutRequest`, and the matching `*Response`
//! types.
//!
//! # What this module ships
//!
//! The 9 agent-side methods the v1.25.0 `meta.json` lists
//! (13 agent methods total — the 4 `session/set_*` and
//! `authenticate` methods are intentionally deferred to
//! keep this commit's surface focused) plus the
//! `CancelNotification` and the auxiliary value objects
//! the request / response bodies share: `SessionId`,
//! `StopReason`, `McpServer` (and its three variants),
//! `EnvVariable`, `HttpHeader`, and `SessionInfo`.
//!
//! # Why request / response types in the same module
//!
//! The schema pairs each request with its response; a
//! module-per-type layout would scatter the round-trip
//! across files. The `application::acp` dispatcher
//! will route by method name and pair the request with
//! its response in one place, so the consumer sees a
//! single import.
//!
//! # Why a single `SessionId` newtype
//!
//! Every session-scoped request carries `sessionId`. A
//! `String` would compile, but a `SessionId` makes the
//! API self-documenting (`fn load(req: LoadSessionRequest)`
//! vs `fn load(req: LoadSessionRequest, id: &str)`) and
//! lets the future per-revision codec narrow the type
//! (a hypothetical `SessionId::Uuid` variant) without
//! rewriting every call site.

use serde::{Deserialize, Serialize};

use super::content::ContentBlock;

/// A unique identifier for a conversation session.
///
/// The schema's `SessionId` is `type: "string"` — opaque
/// to the protocol. The newtype makes the API
/// self-documenting and gives the future per-revision
/// codec a place to narrow the type without rewriting
/// every call site.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub String);

impl SessionId {
    /// A `SessionId` from any string the schema accepts.
    /// The schema does not constrain the value beyond
    /// "opaque string"; the consumer is the source of
    /// truth for what makes a valid id.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The inner string, borrowed.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(&self.0, formatter)
    }
}

/// Why an agent stopped processing a prompt turn.
///
/// The schema names five values; the typed form is
/// the same five-value space. `Copy` because the
/// shape is a `Copy` enum — the wire form is a
/// single string, the consumer's storage is a
/// single word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The turn ended successfully.
    EndTurn,
    /// The turn ended because the agent reached the
    /// maximum number of tokens.
    MaxTokens,
    /// The turn ended because the agent reached the
    /// maximum number of allowed agent requests
    /// between user turns.
    MaxTurnRequests,
    /// The turn ended because the agent refused to
    /// continue. The user prompt and everything that
    /// comes after it are not included in the next
    /// prompt.
    Refusal,
    /// The turn was cancelled by the client via
    /// `session/cancel`.
    Cancelled,
}

// -- session/new -----------------------------------------------------------

/// `session/new` request. Creates a new session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionRequest {
    /// The working directory for this session.
    /// Must be an absolute path; the schema rejects
    /// relative paths on the wire.
    pub cwd: String,
    /// Additional workspace roots the session should
    /// activate. Each path is absolute. When absent
    /// or empty, no additional roots are activated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_directories: Option<Vec<String>>,
    /// MCP servers the agent should connect to.
    /// `Vec<McpServer>` is the schema's `mcpServers`
    /// field; required on the wire.
    pub mcp_servers: Vec<McpServer>,
}

/// `session/new` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionResponse {
    /// The id of the session the agent created. Every
    /// subsequent request for this conversation uses
    /// this id.
    pub session_id: SessionId,
    /// Initial mode state the agent advertises. Absent
    /// when the agent does not support session modes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modes: Option<SessionModeState>,
    /// Initial session configuration options the
    /// agent advertises. Absent when the agent has
    /// no configuration to surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_options: Option<Vec<SessionConfigOption>>,
}

// -- session/load ----------------------------------------------------------

/// `session/load` request. Loads an existing session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadSessionRequest {
    /// The id of the session to load.
    pub session_id: SessionId,
    /// The working directory the session will run in.
    /// Must match the session's recorded `cwd`.
    pub cwd: String,
    /// Additional workspace roots to activate. When
    /// non-empty, this is the complete list — the
    /// request replaces the previous list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_directories: Option<Vec<String>>,
    /// MCP servers the agent should connect to for
    /// this session.
    pub mcp_servers: Vec<McpServer>,
}

/// `session/load` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadSessionResponse {
    /// Initial mode state the agent advertises. Absent
    /// when the agent does not support session modes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modes: Option<SessionModeState>,
    /// Initial session configuration options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_options: Option<Vec<SessionConfigOption>>,
}

// -- session/list ----------------------------------------------------------

/// `session/list` request. Enumerates sessions.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListSessionsRequest {
    /// Filter sessions by working directory. Absent
    /// means "all sessions".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Opaque cursor token from a previous response's
    /// `nextCursor`. Absent means "first page".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// `session/list` response. A page of sessions plus
/// the cursor for the next page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListSessionsResponse {
    /// The session information objects for this page.
    /// The schema's `x-deserialize-skip-invalid-items`
    /// annotation means a malformed entry is silently
    /// dropped during deserialisation.
    pub sessions: Vec<SessionInfo>,
    /// Opaque cursor for the next page. Absent means
    /// "no more pages".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// Information about a single session, returned by
/// `session/list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    /// The session's unique identifier.
    pub session_id: SessionId,
    /// The working directory the session was created
    /// with. Must be an absolute path.
    pub cwd: String,
    /// Additional workspace roots reported for this
    /// session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_directories: Option<Vec<String>>,
    /// A human-readable title for the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// An ISO-8601 timestamp of the last activity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

// -- session/delete --------------------------------------------------------

/// `session/delete` request. Removes a session from
/// the agent's store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteSessionRequest {
    /// The id of the session to delete.
    pub session_id: SessionId,
}

/// `session/delete` response. Currently empty; the
/// struct exists so a future field the schema adds
/// does not break the consumer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteSessionResponse {}

// -- session/resume --------------------------------------------------------

/// `session/resume` request. Resumes a previously
/// closed session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeSessionRequest {
    /// The id of the session to resume.
    pub session_id: SessionId,
    /// The working directory the session will run in.
    pub cwd: String,
    /// Additional workspace roots to activate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_directories: Option<Vec<String>>,
    /// MCP servers the agent should connect to.
    pub mcp_servers: Vec<McpServer>,
}

/// `session/resume` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeSessionResponse {
    /// The mode state the agent advertises after
    /// resuming. Absent when the agent does not
    /// support session modes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modes: Option<SessionModeState>,
    /// Initial session configuration options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_options: Option<Vec<SessionConfigOption>>,
}

// -- session/close ---------------------------------------------------------

/// `session/close` request. Closes an active session
/// without deleting it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseSessionRequest {
    /// The id of the session to close.
    pub session_id: SessionId,
}

/// `session/close` response. Currently empty; the
/// struct exists so a future field the schema adds
/// does not break the consumer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseSessionResponse {}

// -- session/prompt --------------------------------------------------------

/// `session/prompt` request. Sends a user turn to the
/// agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptRequest {
    /// The id of the session to send the prompt to.
    pub session_id: SessionId,
    /// The blocks of content that compose the user's
    /// message. The schema says every agent MUST
    /// support `Text` and `ResourceLink`; the other
    /// variants are gated on the agent's
    /// `PromptCapabilities`.
    pub prompt: Vec<ContentBlock>,
}

/// `session/prompt` response. Carries the reason the
/// turn ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptResponse {
    /// Why the agent stopped processing the turn.
    pub stop_reason: StopReason,
}

// -- session/cancel --------------------------------------------------------

/// `session/cancel` notification. Cancels the
/// in-flight prompt. A notification has no response;
/// the consumer does not await a reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelNotification {
    /// The id of the session whose prompt should be
    /// cancelled.
    pub session_id: SessionId,
}

// -- logout ----------------------------------------------------------------

/// `logout` request. Terminates the auth session.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogoutRequest {
    /// The id of the session the auth is associated
    /// with, when the auth is session-scoped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
}

/// `logout` response. Currently empty; the struct
/// exists so a future field the schema adds does not
/// break the consumer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogoutResponse {}

// -- aux value objects -----------------------------------------------------

/// Configuration for connecting to an MCP server.
///
/// A discriminated union over the three transports the
/// schema names: `stdio` (the baseline every agent
/// supports), `http`, and `sse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpServer {
    /// Stdio transport. All agents MUST support this.
    Stdio(McpServerStdio),
    /// HTTP transport. The agent advertises support
    /// via `McpCapabilities::http`.
    Http(McpServerHttp),
    /// SSE transport. The agent advertises support
    /// via `McpCapabilities::sse`.
    Sse(McpServerSse),
}

/// Stdio transport configuration for MCP. All agents
/// MUST support this transport.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerStdio {
    /// Human-readable name identifying this server.
    pub name: String,
    /// Absolute path to the MCP server executable.
    pub command: String,
    /// Command-line arguments to pass to the server.
    pub args: Vec<String>,
    /// Environment variables to set when launching
    /// the server.
    pub env: Vec<EnvVariable>,
}

/// HTTP transport configuration for MCP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerHttp {
    /// Human-readable name identifying this server.
    pub name: String,
    /// URL to the MCP server.
    pub url: String,
    /// HTTP headers to set on every request to the
    /// server.
    pub headers: Vec<HttpHeader>,
}

/// SSE transport configuration for MCP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerSse {
    /// Human-readable name identifying this server.
    pub name: String,
    /// URL to the MCP server's SSE endpoint.
    pub url: String,
    /// HTTP headers to set on every request to the
    /// server.
    pub headers: Vec<HttpHeader>,
}

/// An environment variable to set on a launched
/// process. The schema's `EnvVariable` is just a
/// `(name, value)` pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvVariable {
    /// The variable name.
    pub name: String,
    /// The variable value.
    pub value: String,
}

/// An HTTP header to set when making requests to a
/// remote MCP server. The schema's `HttpHeader` is
/// just a `(name, value)` pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpHeader {
    /// The header name.
    pub name: String,
    /// The header value.
    pub value: String,
}

/// The set of session modes the agent supports, plus
/// the mode that is currently active. Returned by
/// `session/new` and `session/load` when the agent
/// supports session modes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionModeState {
    /// The set of available modes.
    pub available_modes: Vec<SessionMode>,
    /// The id of the currently active mode. Must be
    /// a member of `available_modes`.
    pub current_mode_id: SessionModeId,
}

/// A single session mode. The agent advertises the
/// set of modes the session can switch between via
/// `SessionModeState::available_modes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMode {
    /// The unique id of the mode. The consumer
    /// references this id in
    /// [`SetSessionModeRequest`] and
    /// [`CurrentModeUpdate`][super::notification::CurrentModeUpdate].
    pub id: SessionModeId,
    /// Human-readable name.
    pub name: String,
    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A unique identifier for a session mode.
///
/// The schema's `SessionModeId` is `type: "string"`
/// — opaque to the protocol. The newtype gives the
/// API a self-documenting handle and a place for the
/// future per-revision codec to narrow the type.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionModeId(pub String);

impl SessionModeId {
    /// A `SessionModeId` from any string the schema
    /// accepts.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl core::fmt::Display for SessionModeId {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(&self.0, formatter)
    }
}

/// A session configuration option. The agent
/// advertises the set of options the consumer can
/// set via `SetSessionConfigOptionRequest`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigOption {
    /// The unique id of the option. The consumer
    /// references this id in
    /// `SetSessionConfigOptionRequest`.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The option's current value.
    pub current_value: String,
    /// The set of values the consumer can choose
    /// from. Absent for free-form input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    /// The `text` session/prompt request carries a
    /// `sessionId` and a `prompt` array of content
    /// blocks. The wire form round-trips through
    /// `Serialize` → `Deserialize`.
    #[test]
    fn a_prompt_request_round_trips() -> Result<(), serde_json::Error> {
        let request = PromptRequest {
            session_id: SessionId::new("session-1"),
            prompt: vec![ContentBlock::Text(super::super::content::TextContent {
                text: "hi".to_owned(),
                annotations: None,
                meta: BTreeMap::new(),
            })],
        };
        let value = serde_json::to_value(&request)?;
        assert_eq!(value.get("sessionId"), Some(&json!("session-1")));
        let prompt = value
            .get("prompt")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                serde_json::Error::io(std::io::Error::other("prompt is not an array"))
            })?;
        assert_eq!(
            prompt.first().and_then(|b| b.get("type")),
            Some(&json!("text"))
        );
        let back: PromptRequest = serde_json::from_value(value)?;
        assert_eq!(back, request);
        Ok(())
    }

    /// The `sessionId` newtype round-trips as a
    /// transparent string wrapper. The wire form is
    /// just a string; the typed form is a `SessionId`.
    #[test]
    fn a_session_id_newtype_round_trips() -> Result<(), serde_json::Error> {
        let id = SessionId::new("abc-123");
        let value = serde_json::to_value(&id)?;
        assert_eq!(value, json!("abc-123"));
        let back: SessionId = serde_json::from_value(value)?;
        assert_eq!(back, id);
        Ok(())
    }

    /// The `McpServer` union serialises the
    /// `type` discriminator on the wire. Each
    /// variant round-trips independently.
    #[test]
    fn mcp_server_stdio_round_trips() -> Result<(), serde_json::Error> {
        let server = McpServer::Stdio(McpServerStdio {
            name: "stdio-server".to_owned(),
            command: "/usr/local/bin/mcp-server".to_owned(),
            args: vec!["--quiet".to_owned()],
            env: vec![EnvVariable {
                name: "LOG_LEVEL".to_owned(),
                value: "info".to_owned(),
            }],
        });
        let value = serde_json::to_value(&server)?;
        assert_eq!(value.get("type"), Some(&json!("stdio")));
        assert_eq!(value.get("name"), Some(&json!("stdio-server")));
        let back: McpServer = serde_json::from_value(value)?;
        assert_eq!(back, server);
        Ok(())
    }

    /// The `session/new` request round-trips the
    /// required fields (`cwd`, `mcpServers`) and
    /// the optional `additionalDirectories`.
    #[test]
    fn a_new_session_request_round_trips() -> Result<(), serde_json::Error> {
        let request = NewSessionRequest {
            cwd: "/home/user/project".to_owned(),
            additional_directories: Some(vec!["/home/user/shared".to_owned()]),
            mcp_servers: vec![],
        };
        let value = serde_json::to_value(&request)?;
        assert_eq!(value.get("cwd"), Some(&json!("/home/user/project")));
        assert_eq!(
            value.get("additionalDirectories"),
            Some(&json!(["/home/user/shared"]))
        );
        assert_eq!(value.get("mcpServers"), Some(&json!([])));
        let back: NewSessionRequest = serde_json::from_value(value)?;
        assert_eq!(back, request);
        Ok(())
    }

    /// The `session/list` request is `Default`
    /// because every field is optional. The wire
    /// form is the empty object `{}`.
    #[test]
    fn a_list_sessions_request_default_round_trips() -> Result<(), serde_json::Error> {
        let request = ListSessionsRequest::default();
        let value = serde_json::to_value(&request)?;
        assert_eq!(value, json!({}));
        let back: ListSessionsRequest = serde_json::from_value(value)?;
        assert_eq!(back, request);
        Ok(())
    }

    /// The `session/cancel` notification is a JSON
    /// message with no `id` and no `result` —
    /// round-trips as a struct with the right field
    /// names.
    #[test]
    fn a_cancel_notification_round_trips() -> Result<(), serde_json::Error> {
        let notification = CancelNotification {
            session_id: SessionId::new("session-1"),
        };
        let value = serde_json::to_value(&notification)?;
        assert_eq!(value.get("sessionId"), Some(&json!("session-1")));
        let back: CancelNotification = serde_json::from_value(value)?;
        assert_eq!(back, notification);
        Ok(())
    }

    /// The `StopReason` enum deserialises the string
    /// the schema names; the wire form is a string,
    /// the typed form is a `Copy` enum.
    #[test]
    fn stop_reason_string_round_trips() -> Result<(), serde_json::Error> {
        for (variant, wire) in [
            (StopReason::EndTurn, "end_turn"),
            (StopReason::MaxTokens, "max_tokens"),
            (StopReason::MaxTurnRequests, "max_turn_requests"),
            (StopReason::Refusal, "refusal"),
            (StopReason::Cancelled, "cancelled"),
        ] {
            let value = serde_json::to_value(variant)?;
            assert_eq!(value, json!(wire));
            let back: StopReason = serde_json::from_value(value)?;
            assert_eq!(back, variant);
        }
        Ok(())
    }

    /// The `NewSessionResponse` carries a
    /// `sessionId` plus optional `modes` and
    /// `configOptions`. The wire form is
    /// `camelCase`.
    #[test]
    fn a_new_session_response_round_trips() -> Result<(), serde_json::Error> {
        let response = NewSessionResponse {
            session_id: SessionId::new("session-1"),
            modes: Some(SessionModeState {
                available_modes: vec![SessionMode {
                    id: SessionModeId::new("default"),
                    name: "Default".to_owned(),
                    description: None,
                }],
                current_mode_id: SessionModeId::new("default"),
            }),
            config_options: None,
        };
        let value = serde_json::to_value(&response)?;
        assert_eq!(value.get("sessionId"), Some(&json!("session-1")));
        let modes = value
            .get("modes")
            .ok_or_else(|| serde_json::Error::io(std::io::Error::other("missing `modes` field")))?;
        assert_eq!(modes.get("currentModeId"), Some(&json!("default")));
        let back: NewSessionResponse = serde_json::from_value(value)?;
        assert_eq!(back, response);
        Ok(())
    }

    /// The `LoadSessionRequest` requires `sessionId`,
    /// `cwd`, and `mcpServers`. The wire form uses
    /// `camelCase`.
    #[test]
    fn a_load_session_request_round_trips() -> Result<(), serde_json::Error> {
        let request = LoadSessionRequest {
            session_id: SessionId::new("session-1"),
            cwd: "/home/user/project".to_owned(),
            additional_directories: None,
            mcp_servers: vec![],
        };
        let value = serde_json::to_value(&request)?;
        assert_eq!(value.get("sessionId"), Some(&json!("session-1")));
        assert_eq!(value.get("cwd"), Some(&json!("/home/user/project")));
        let back: LoadSessionRequest = serde_json::from_value(value)?;
        assert_eq!(back, request);
        Ok(())
    }

    /// The `LoadSessionResponse` carries optional
    /// `modes` and `configOptions`. The wire form
    /// is the empty object when both are absent.
    #[test]
    fn a_load_session_response_round_trips() -> Result<(), serde_json::Error> {
        let response = LoadSessionResponse {
            modes: None,
            config_options: None,
        };
        let value = serde_json::to_value(&response)?;
        assert_eq!(value, json!({}));
        let back: LoadSessionResponse = serde_json::from_value(value)?;
        assert_eq!(back, response);
        Ok(())
    }

    /// The `ListSessionsResponse` carries a
    /// `sessions` array and an optional
    /// `nextCursor`. The cursor is omitted on
    /// the last page.
    #[test]
    fn a_list_sessions_response_round_trips() -> Result<(), serde_json::Error> {
        let response = ListSessionsResponse {
            sessions: vec![SessionInfo {
                session_id: SessionId::new("session-1"),
                cwd: "/home/user/project".to_owned(),
                additional_directories: None,
                title: Some("My project".to_owned()),
                updated_at: Some("2026-10-15T00:00:00Z".to_owned()),
            }],
            next_cursor: None,
        };
        let value = serde_json::to_value(&response)?;
        let sessions = value
            .get("sessions")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| serde_json::Error::io(std::io::Error::other("missing `sessions`")))?;
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions
                .first()
                .and_then(|s| s.get("sessionId"))
                .and_then(serde_json::Value::as_str),
            Some("session-1"),
        );
        let back: ListSessionsResponse = serde_json::from_value(value)?;
        assert_eq!(back, response);
        Ok(())
    }

    /// The `DeleteSessionRequest` is a
    /// `sessionId` only. The wire form is
    /// `{"sessionId": "..."}`.
    #[test]
    fn a_delete_session_request_round_trips() -> Result<(), serde_json::Error> {
        let request = DeleteSessionRequest {
            session_id: SessionId::new("session-1"),
        };
        let value = serde_json::to_value(&request)?;
        assert_eq!(value, json!({"sessionId": "session-1"}));
        let back: DeleteSessionRequest = serde_json::from_value(value)?;
        assert_eq!(back, request);
        Ok(())
    }

    /// The `DeleteSessionResponse` is the empty
    /// object `{}` — the schema has no fields
    /// the response carries.
    #[test]
    fn a_delete_session_response_round_trips() -> Result<(), serde_json::Error> {
        let response = DeleteSessionResponse::default();
        let value = serde_json::to_value(&response)?;
        assert_eq!(value, json!({}));
        let back: DeleteSessionResponse = serde_json::from_value(value)?;
        assert_eq!(back, response);
        Ok(())
    }

    /// The `ResumeSessionRequest` requires
    /// `sessionId` and `cwd`. The optional
    /// `additionalDirectories` and required
    /// `mcpServers` round-trip.
    #[test]
    fn a_resume_session_request_round_trips() -> Result<(), serde_json::Error> {
        let request = ResumeSessionRequest {
            session_id: SessionId::new("session-1"),
            cwd: "/home/user/project".to_owned(),
            additional_directories: None,
            mcp_servers: vec![],
        };
        let value = serde_json::to_value(&request)?;
        assert_eq!(value.get("sessionId"), Some(&json!("session-1")));
        assert_eq!(value.get("cwd"), Some(&json!("/home/user/project")));
        let back: ResumeSessionRequest = serde_json::from_value(value)?;
        assert_eq!(back, request);
        Ok(())
    }

    /// The `ResumeSessionResponse` is shaped
    /// like `NewSessionResponse` minus the
    /// `sessionId` (the consumer already has it).
    #[test]
    fn a_resume_session_response_round_trips() -> Result<(), serde_json::Error> {
        let response = ResumeSessionResponse {
            modes: None,
            config_options: None,
        };
        let value = serde_json::to_value(&response)?;
        assert_eq!(value, json!({}));
        let back: ResumeSessionResponse = serde_json::from_value(value)?;
        assert_eq!(back, response);
        Ok(())
    }

    /// The `CloseSessionRequest` is a
    /// `sessionId` only. The wire form is
    /// `{"sessionId": "..."}`.
    #[test]
    fn a_close_session_request_round_trips() -> Result<(), serde_json::Error> {
        let request = CloseSessionRequest {
            session_id: SessionId::new("session-1"),
        };
        let value = serde_json::to_value(&request)?;
        assert_eq!(value, json!({"sessionId": "session-1"}));
        let back: CloseSessionRequest = serde_json::from_value(value)?;
        assert_eq!(back, request);
        Ok(())
    }

    /// The `CloseSessionResponse` is the empty
    /// object `{}` — the schema has no fields
    /// the response carries.
    #[test]
    fn a_close_session_response_round_trips() -> Result<(), serde_json::Error> {
        let response = CloseSessionResponse::default();
        let value = serde_json::to_value(&response)?;
        assert_eq!(value, json!({}));
        let back: CloseSessionResponse = serde_json::from_value(value)?;
        assert_eq!(back, response);
        Ok(())
    }

    /// The `PromptResponse` carries a
    /// `stopReason`. The wire form is
    /// `{"stopReason": "end_turn"}`.
    #[test]
    fn a_prompt_response_round_trips() -> Result<(), serde_json::Error> {
        let response = PromptResponse {
            stop_reason: StopReason::EndTurn,
        };
        let value = serde_json::to_value(&response)?;
        assert_eq!(value, json!({"stopReason": "end_turn"}));
        let back: PromptResponse = serde_json::from_value(value)?;
        assert_eq!(back, response);
        Ok(())
    }

    /// The `LogoutRequest` is `Default` because
    /// every field is optional. The wire form
    /// is the empty object `{}`.
    #[test]
    fn a_logout_request_default_round_trips() -> Result<(), serde_json::Error> {
        let request = LogoutRequest::default();
        let value = serde_json::to_value(&request)?;
        assert_eq!(value, json!({}));
        let back: LogoutRequest = serde_json::from_value(value)?;
        assert_eq!(back, request);
        Ok(())
    }

    /// The `LogoutRequest` with a `sessionId`
    /// carries the id on the wire.
    #[test]
    fn a_logout_request_with_session_id_round_trips() -> Result<(), serde_json::Error> {
        let request = LogoutRequest {
            session_id: Some(SessionId::new("session-1")),
        };
        let value = serde_json::to_value(&request)?;
        assert_eq!(value, json!({"sessionId": "session-1"}));
        let back: LogoutRequest = serde_json::from_value(value)?;
        assert_eq!(back, request);
        Ok(())
    }

    /// The `LogoutResponse` is the empty
    /// object `{}` — the schema has no fields
    /// the response carries.
    #[test]
    fn a_logout_response_round_trips() -> Result<(), serde_json::Error> {
        let response = LogoutResponse::default();
        let value = serde_json::to_value(&response)?;
        assert_eq!(value, json!({}));
        let back: LogoutResponse = serde_json::from_value(value)?;
        assert_eq!(back, response);
        Ok(())
    }

    /// The `McpServer::Http` variant carries
    /// `name`, `url`, and `headers`. The
    /// `type` discriminator is `http`.
    #[test]
    fn mcp_server_http_round_trips() -> Result<(), serde_json::Error> {
        let server = McpServer::Http(McpServerHttp {
            name: "http-server".to_owned(),
            url: "https://mcp.example.com/sse".to_owned(),
            headers: vec![HttpHeader {
                name: "Authorization".to_owned(),
                value: "Bearer token".to_owned(),
            }],
        });
        let value = serde_json::to_value(&server)?;
        assert_eq!(value.get("type"), Some(&json!("http")));
        assert_eq!(
            value.get("url"),
            Some(&json!("https://mcp.example.com/sse"))
        );
        let back: McpServer = serde_json::from_value(value)?;
        assert_eq!(back, server);
        Ok(())
    }

    /// The `McpServer::Sse` variant carries
    /// `name`, `url`, and `headers`. The
    /// `type` discriminator is `sse`.
    #[test]
    fn mcp_server_sse_round_trips() -> Result<(), serde_json::Error> {
        let server = McpServer::Sse(McpServerSse {
            name: "sse-server".to_owned(),
            url: "https://mcp.example.com/sse".to_owned(),
            headers: vec![],
        });
        let value = serde_json::to_value(&server)?;
        assert_eq!(value.get("type"), Some(&json!("sse")));
        let back: McpServer = serde_json::from_value(value)?;
        assert_eq!(back, server);
        Ok(())
    }

    /// A `SessionConfigOption` with options
    /// carries the list of values the consumer
    /// can choose from.
    #[test]
    fn a_session_config_option_round_trips() -> Result<(), serde_json::Error> {
        let option = SessionConfigOption {
            id: "model".to_owned(),
            name: "Model".to_owned(),
            description: Some("The LLM to use".to_owned()),
            current_value: "claude-sonnet-4-5".to_owned(),
            options: Some(vec![
                "claude-sonnet-4-5".to_owned(),
                "claude-opus-4-7".to_owned(),
            ]),
        };
        let value = serde_json::to_value(&option)?;
        assert_eq!(value.get("id"), Some(&json!("model")));
        assert_eq!(value.get("currentValue"), Some(&json!("claude-sonnet-4-5")));
        let back: SessionConfigOption = serde_json::from_value(value)?;
        assert_eq!(back, option);
        Ok(())
    }
}
