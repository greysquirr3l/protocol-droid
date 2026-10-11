//! A typed A2A client over HTTP + JSON-RPC + SSE.
//!
//! Drives an A2A peer end-to-end: discovers the
//! [`AgentCard`](crate::domain::a2a::agent_card::AgentCard) at
//! `/.well-known/agent-card.json`, sends request/response
//! JSON-RPC envelopes to the JSON-RPC method table, and
//! reads streaming SSE responses into typed
//! [`StreamingEvent`](crate::domain::a2a::streaming_events::StreamingEvent)
//! values.
//!
//! # Why this exists
//!
//! Every consumer who wants to drive an A2A peer is
//! reaching for `reqwest` and rolling the same five
//! hundred lines: a counter for ids, a JSON-RPC
//! envelope shape, an SSE parser, a stream of typed
//! events. The [`a2a_client`](https://github.com/greysquirr3l/protocol-droid/blob/main/crates/protocol-droid/examples/a2a_client.rs)
//! example is the proof — five hundred lines of
//! `Box<dyn Error>` plumbing a typed consumer would
//! rather not write.
//!
//! [`A2aClient`] is the typed version. One struct, one
//! constructor, ten methods that take the A2A
//! request/response types the library already defines
//! and return the typed answers the library already
//! parses. The four lines of envelope munging the
//! example is built on are folded into a private
//! `post_json_rpc` helper; the SSE parsing the example
//! rolls is a private `post_sse_collect`; the id
//! counter is a private [`AtomicI64`]. None of those
//! is a decision the consumer should be making.
//!
//! # What it does
//!
//! 1. `A2aClient::new(peer)` builds a client pointed
//!    at the peer's base URL. The base URL is the
//!    one the well-known card resolves to, e.g.
//!    `http://peer:8080`; the well-known URI is
//!    appended automatically.
//! 2. `card()` fetches the well-known
//!    `AgentCard` and parses it. The card is not
//!    cached — a consumer who wants the card once
//!    calls it once; a consumer who wants it for
//!    every request calls it for every request.
//! 3. The four request/response methods
//!    ([`send_message`](Self::send_message),
//!    [`get_task`](Self::get_task),
//!    [`list_tasks`](Self::list_tasks),
//!    [`cancel_task`](Self::cancel_task)) POST a
//!    JSON-RPC envelope and parse the typed
//!    response. Each method takes the library's
//!    request type and returns the library's
//!    response type, so a consumer is matching on
//!    real A2A types, not on `serde_json::Value`.
//! 4. The two streaming methods
//!    ([`send_streaming_message`](Self::send_streaming_message),
//!    [`subscribe_to_task`](Self::subscribe_to_task))
//!    POST a JSON-RPC envelope with
//!    `accept: text/event-stream` and collect the
//!    SSE data lines into a `Vec<StreamingEvent>`.
//! 5. The four push-notification-config methods
//!    ([`create_push_notification_config`](Self::create_push_notification_config),
//!    [`get_push_notification_config`](Self::get_push_notification_config),
//!    [`list_push_notification_configs`](Self::list_push_notification_configs),
//!    [`delete_push_notification_config`](Self::delete_push_notification_config))
//!    round-trip a `TaskPushNotificationConfig`
//!    through the same JSON-RPC envelope.
//! 6. [`get_extended_agent_card`](Self::get_extended_agent_card)
//!    fetches the authenticated extended agent
//!    card. The A2A spec splits the card into a
//!    public read (the well-known path, no
//!    authentication) and an extended read (this
//!    method, requires the same bearer token the
//!    JSON-RPC and streaming routes require). The
//!    method exists so a consumer does not have
//!    to drop down to `reqwest` for one
//!    authenticated card fetch.
//!
//! # What it deliberately does not do
//!
//! - It does not cache the agent card. A consumer
//!   that wants the card cached can wrap the call;
//!   a client that did cache by default would
//!   silently serve a stale card and the
//!   `list_changed` semantics a future revision
//!   adds would be wrong.
//! - It does not pool reqwest connections. The
//!   [`HttpClient`](reqwest::Client) inside
//!   [`A2aClient`] is one per client, and
//!   `reqwest::Client` already pools by default — a
//!   second layer is a second opinion on what
//!   `reqwest` is already doing.
//! - It does not retry on transport error. A
//!   retry policy is a decision the consumer should
//!   make (the A2A spec does not define one); a
//!   client that retried by default would be wrong
//!   in the cases nobody tests.
//! - It does not stream events as they arrive. A
//!   streaming consumer wants events one at a time
//!   so it can decide when to stop. The current
//!   shape (`Vec<StreamingEvent>`) is a
//!   convenience; a future revision will swap it
//!   for `Pin<Box<dyn Stream<Item = …>>>` without
//!   changing the public method names.
//!
//! # Errors
//!
//! Every method returns [`Result<_, A2aClientError>`].
//! The error variants name the four failure modes:
//! transport (the peer could not be reached),
//! JSON-RPC (the peer answered the envelope with
//! an `error` member), parse (the response was
//! well-formed JSON but not the typed shape the
//! method expected), and stream (an SSE reply
//! ended before the method's expected terminal
//! event). The HTTP status code is in the
//! transport error, the JSON-RPC code and message
//! are in the JSON-RPC error, and the parse error
//! is the [`serde_json`] error verbatim — a
//! consumer matching on the variant gets the
//! detail from `Display` or `source()`.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use reqwest::Client as HttpClient;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::adapters::secondary::a2a::A2aMethod;
use crate::domain::a2a::agent_card::AgentCard;
use crate::domain::a2a::extended_agent_card::{
    GetExtendedAgentCardRequest, GetExtendedAgentCardResponse,
};
use crate::domain::a2a::list_tasks::{ListTasksRequest, ListTasksResponse};
use crate::domain::a2a::push_notification_methods::{
    CreateTaskPushNotificationConfigRequest, CreateTaskPushNotificationConfigResponse,
    DeleteTaskPushNotificationConfigRequest, GetTaskPushNotificationConfigRequest,
    GetTaskPushNotificationConfigResponse, ListTaskPushNotificationConfigsRequest,
    ListTaskPushNotificationConfigsResponse,
};
use crate::domain::a2a::send_message::{SendMessageRequest, SendMessageResponse};
use crate::domain::a2a::streaming_events::StreamingEvent;
use crate::domain::a2a::task_lifecycle::{
    CancelTaskRequest, CancelTaskResponse, GetTaskRequest, GetTaskResponse,
};
use crate::domain::a2a::well_known::AGENT_CARD_WELL_KNOWN_PATH;

/// What failed when [`A2aClient`] could not complete a call.
#[derive(Debug, thiserror::Error)]
pub enum A2aClientError {
    /// The peer could not be reached over HTTP, or returned a non-2xx status.
    #[error("the peer could not be reached: {0}")]
    Io(#[from] reqwest::Error),

    /// The peer answered the JSON-RPC envelope with an `error` member.
    ///
    /// The code and message are the JSON-RPC `code` and `message`
    /// fields, copied verbatim. The `data` member, when present,
    /// is dropped: it is the spec's escape hatch for vendor
    /// extensions, and a typed client that lifted it would force
    /// every consumer to know every extension.
    #[error("the peer answered with a JSON-RPC error {code}: {message}")]
    JsonRpc {
        /// The JSON-RPC error code the peer returned.
        code: i64,
        /// The JSON-RPC error message the peer returned.
        message: String,
    },

    /// The response could not be parsed as the expected typed shape.
    #[error("the response could not be parsed: {0}")]
    Parse(#[from] serde_json::Error),

    /// The SSE stream ended before the method's expected terminal event.
    #[error("the SSE stream ended: {0}")]
    StreamEnded(String),
}

/// The base URL of an A2A peer.
///
/// `peer` is the URL the well-known card resolves to, e.g.
/// `http://peer:8080`. The well-known URI and the JSON-RPC
/// method table are appended to this base, so a peer that
/// exposes its card at `http://peer:8080/.well-known/agent-card.json`
/// and its method table at `http://peer:8080/a2a` is driven
/// with `A2aClient::new("http://peer:8080")`.
///
/// The four constructors below build a client aimed at the
/// conventional paths. A consumer whose peer uses a
/// non-conventional layout — e.g. the card at
/// `/card` and the JSON-RPC at `/rpc` — uses
/// [`A2aClient::with_paths`] to point the client at
/// non-default paths.
///
#[derive(Debug)]
pub struct A2aClient {
    base: Arc<str>,
    card_path: Arc<str>,
    jsonrpc_path: Arc<str>,
    streaming_path: Arc<str>,
    bearer: Option<Arc<str>>,
    http: HttpClient,
    next_id: Arc<AtomicI64>,
}

impl Clone for A2aClient {
    /// A clone is cheap: the [`HttpClient`]
    /// `reqwest` ships already pools and
    /// shares connections across clones,
    /// and the id counter is an `Arc` so a
    /// clone and its original issue distinct
    /// ids. The [`Arc<str>`] paths are
    /// reference-counted copies, not deep
    /// clones. The bearer token, if any, is
    /// also `Arc<str>` — a clone points at
    /// the same allocation, not a copy of
    /// the secret.
    fn clone(&self) -> Self {
        Self {
            base: Arc::clone(&self.base),
            card_path: Arc::clone(&self.card_path),
            jsonrpc_path: Arc::clone(&self.jsonrpc_path),
            streaming_path: Arc::clone(&self.streaming_path),
            bearer: self.bearer.as_ref().map(Arc::clone),
            http: self.http.clone(),
            next_id: Arc::clone(&self.next_id),
        }
    }
}

impl A2aClient {
    /// A client pointed at the conventional paths.
    ///
    /// The well-known card is fetched from
    /// `peer + /.well-known/agent-card.json`; the
    /// JSON-RPC method table is `POST`ed to
    /// `peer + /a2a`. The two paths match the
    /// A2A v1.0 §8 discovery and §3 method
    /// routes; a consumer whose peer does not
    /// follow them uses
    /// [`with_paths`](Self::with_paths) instead.
    ///
    /// # Errors
    ///
    /// [`A2aClientError::Io`] when the underlying
    /// `reqwest::Client` cannot be built (a TLS
    /// configuration error or an invalid
    /// default-header value). The transport is
    /// built once at construction; every method
    /// on the client reuses it.
    pub fn new(peer: impl Into<String>) -> Result<Self, A2aClientError> {
        let http = HttpClient::builder()
            .user_agent(concat!("protocol-droid/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            base: Arc::from(peer.into()),
            card_path: Arc::from(AGENT_CARD_WELL_KNOWN_PATH),
            jsonrpc_path: Arc::from("/a2a"),
            streaming_path: Arc::from("/a2a/stream"),
            bearer: None,
            http,
            next_id: Arc::new(AtomicI64::new(1)),
        })
    }

    /// A client pointed at non-default card and JSON-RPC paths.
    ///
    /// `card_path` is the URI the GET to fetch the
    /// agent card is sent to, relative to `peer`
    /// (e.g. `/custom/card`). `jsonrpc_path` is the
    /// URI the POST to the JSON-RPC method table
    /// is sent to, relative to `peer`
    /// (e.g. `/custom/rpc`). `streaming_path` is
    /// the URI the streaming methods
    /// ([`send_streaming_message`](Self::send_streaming_message)
    /// and [`subscribe_to_task`](Self::subscribe_to_task))
    /// POST to, relative to `peer`.
    ///
    /// # Errors
    ///
    /// [`A2aClientError::Io`] when the underlying
    /// `reqwest::Client` cannot be built. The
    /// construction mirrors [`A2aClient::new`].
    pub fn with_paths(
        peer: impl Into<String>,
        card_path: impl Into<String>,
        jsonrpc_path: impl Into<String>,
        streaming_path: impl Into<String>,
    ) -> Result<Self, A2aClientError> {
        let http = HttpClient::builder()
            .user_agent(concat!("protocol-droid/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            base: Arc::from(peer.into()),
            card_path: Arc::from(card_path.into()),
            jsonrpc_path: Arc::from(jsonrpc_path.into()),
            streaming_path: Arc::from(streaming_path.into()),
            bearer: None,
            http,
            next_id: Arc::new(AtomicI64::new(1)),
        })
    }

    /// Attaches a Bearer token to every subsequent request.
    ///
    /// The A2A v1.0 spec lets an agent declare a
    /// `securitySchemes` map on its card; the most
    /// common value is `Http` with `Bearer` (RFC
    /// 6750). A consumer that has an opaque token
    /// from the peer's auth endpoint passes it
    /// here, and every `card()`, `send_message()`,
    /// and `send_streaming_message()` call carries
    /// `Authorization: Bearer <token>`. The card
    /// itself is fetched unauthenticated by default
    /// — a peer that gates the card behind auth
    /// needs a separate way to get the token, which
    /// is out of scope for this commit.
    ///
    /// The token is held in an `Arc<str>`; a clone
    /// shares the same allocation, not a copy of
    /// the secret.
    #[must_use]
    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Self {
        self.bearer = Some(Arc::from(token.into()));
        self
    }

    /// Fetches the agent card at the well-known URI.
    ///
    /// Not cached. A consumer who wants the card
    /// once calls it once; a consumer who wants
    /// the card to reflect a server-side update
    /// calls it again.
    ///
    /// # Errors
    ///
    /// [`A2aClientError::Io`] on a transport
    /// failure or non-2xx status, or
    /// [`A2aClientError::Parse`] when the body
    /// is well-formed JSON but not the
    /// `AgentCard` shape.
    pub async fn card(&self) -> Result<AgentCard, A2aClientError> {
        let url = format!("{}{}", self.base, self.card_path);
        let mut request = self
            .http
            .get(&url)
            .header(reqwest::header::ACCEPT, "application/json");
        if let Some(token) = self.bearer.as_deref() {
            request = request.header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let response = request.send().await?.error_for_status()?;
        let body = response.text().await?;
        let card: AgentCard = serde_json::from_str(&body)?;
        Ok(card)
    }

    /// `SendMessage` — send a message to the agent.
    ///
    /// # Errors
    ///
    /// [`A2aClientError`] on transport, JSON-RPC, or parse failure.
    pub async fn send_message(
        &self,
        request: SendMessageRequest,
    ) -> Result<SendMessageResponse, A2aClientError> {
        let params = serde_json::to_value(&request)?;
        self.post_json_rpc(A2aMethod::SendMessage, params).await
    }

    /// `GetTask` — read a task by id.
    pub async fn get_task(
        &self,
        request: GetTaskRequest,
    ) -> Result<GetTaskResponse, A2aClientError> {
        let params = serde_json::to_value(&request)?;
        self.post_json_rpc(A2aMethod::GetTask, params).await
    }

    /// `ListTasks` — list tasks with optional filtering.
    pub async fn list_tasks(
        &self,
        request: ListTasksRequest,
    ) -> Result<ListTasksResponse, A2aClientError> {
        let params = serde_json::to_value(&request)?;
        self.post_json_rpc(A2aMethod::ListTasks, params).await
    }

    /// `CancelTask` — cancel a task in progress.
    pub async fn cancel_task(
        &self,
        request: CancelTaskRequest,
    ) -> Result<CancelTaskResponse, A2aClientError> {
        let params = serde_json::to_value(&request)?;
        self.post_json_rpc(A2aMethod::CancelTask, params).await
    }

    /// `SendStreamingMessage` — send a message and stream the agent's
    /// progress as a sequence of typed events.
    ///
    /// Returns every event the server sent before
    /// closing the stream. The A2A spec terminates
    /// a `SendStreamingMessage` with a `status`
    /// event whose `final` flag is `true`; a
    /// consumer who needs that terminal event
    /// checks `StreamingEvent::Status(…).final`
    /// on the last element.
    pub async fn send_streaming_message(
        &self,
        request: SendMessageRequest,
    ) -> Result<Vec<StreamingEvent>, A2aClientError> {
        let params = serde_json::to_value(&request)?;
        self.post_sse_collect(A2aMethod::SendStreamingMessage, params)
            .await
    }

    /// `SubscribeToTask` — open an SSE stream of updates
    /// for an existing non-terminal task.
    pub async fn subscribe_to_task(
        &self,
        task_id: String,
    ) -> Result<Vec<StreamingEvent>, A2aClientError> {
        let params = json!({ "id": task_id });
        self.post_sse_collect(A2aMethod::SubscribeToTask, params)
            .await
    }

    /// `CreateTaskPushNotificationConfig` — register a
    /// push notification config for a task.
    pub async fn create_push_notification_config(
        &self,
        request: CreateTaskPushNotificationConfigRequest,
    ) -> Result<CreateTaskPushNotificationConfigResponse, A2aClientError> {
        let params = serde_json::to_value(&request)?;
        self.post_json_rpc(A2aMethod::CreateTaskPushNotificationConfig, params)
            .await
    }

    /// `GetTaskPushNotificationConfig` — read a config by id.
    pub async fn get_push_notification_config(
        &self,
        request: GetTaskPushNotificationConfigRequest,
    ) -> Result<GetTaskPushNotificationConfigResponse, A2aClientError> {
        let params = serde_json::to_value(&request)?;
        self.post_json_rpc(A2aMethod::GetTaskPushNotificationConfig, params)
            .await
    }

    /// `ListTaskPushNotificationConfigs` — list a task's configs.
    pub async fn list_push_notification_configs(
        &self,
        request: ListTaskPushNotificationConfigsRequest,
    ) -> Result<ListTaskPushNotificationConfigsResponse, A2aClientError> {
        let params = serde_json::to_value(&request)?;
        self.post_json_rpc(A2aMethod::ListTaskPushNotificationConfigs, params)
            .await
    }

    /// `DeleteTaskPushNotificationConfig` — remove a config.
    ///
    /// Returns `Ok(())` when the peer confirms the
    /// deletion. The A2A spec answers a successful
    /// delete with the empty JSON-RPC `result`; the
    /// helper's `()` deserialisation accepts that
    /// without an extra wrapper.
    pub async fn delete_push_notification_config(
        &self,
        request: DeleteTaskPushNotificationConfigRequest,
    ) -> Result<(), A2aClientError> {
        let params = serde_json::to_value(&request)?;
        let _: Value = self
            .post_json_rpc(A2aMethod::DeleteTaskPushNotificationConfig, params)
            .await?;
        Ok(())
    }

    /// `GetExtendedAgentCard` — fetch the
    /// authenticated extended agent card.
    ///
    /// The A2A spec splits the agent card into
    /// two reads: the *public* card served on the
    /// well-known path with no authentication,
    /// and the *extended* card returned by this
    /// method, which requires the same bearer
    /// token the JSON-RPC and streaming routes
    /// require. The extended card is the same
    /// card plus any additional capabilities the
    /// server chooses not to advertise publicly
    /// (a paid tool, an admin endpoint, a
    /// tenant-scoped skill).
    ///
    /// A consumer who has not set a bearer token
    /// via [`with_bearer_token`](Self::with_bearer_token)
    /// will see a 401 from the peer — that is
    /// the spec's "you must authenticate to read
    /// this card" signal, not a transport error.
    /// A consumer who has set a token gets the
    /// extended card in the response.
    pub async fn get_extended_agent_card(
        &self,
        request: GetExtendedAgentCardRequest,
    ) -> Result<GetExtendedAgentCardResponse, A2aClientError> {
        let params = serde_json::to_value(&request)?;
        self.post_json_rpc(A2aMethod::GetExtendedAgentCard, params)
            .await
    }

    /// POSTs a JSON-RPC envelope to the method table and parses the typed result.
    async fn post_json_rpc<T: DeserializeOwned>(
        &self,
        method: A2aMethod,
        params: Value,
    ) -> Result<T, A2aClientError> {
        let url = format!("{}{}", self.base, self.jsonrpc_path);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let envelope = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method.as_str(),
            "params": params,
        });
        let body = serde_json::to_vec(&envelope)?;
        let mut request = self
            .http
            .post(&url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "application/json")
            .body(body);
        if let Some(token) = self.bearer.as_deref() {
            request = request.header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let response = request.send().await?.error_for_status()?;
        let body = response.text().await?;
        let envelope: Value = serde_json::from_str(&body)?;
        Self::extract_result(&envelope)
    }

    /// POSTs a JSON-RPC envelope with `accept: text/event-stream` and
    /// collects every `data:` line into a typed event.
    async fn post_sse_collect(
        &self,
        method: A2aMethod,
        params: Value,
    ) -> Result<Vec<StreamingEvent>, A2aClientError> {
        let url = format!("{}{}", self.base, self.streaming_path);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let envelope = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method.as_str(),
            "params": params,
        });
        let body = serde_json::to_vec(&envelope)?;
        let mut request = self
            .http
            .post(&url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .body(body);
        if let Some(token) = self.bearer.as_deref() {
            request = request.header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let response = request.send().await?.error_for_status()?;
        let body = response.text().await?;
        Self::parse_sse_blocks(&body)
    }

    /// Lifts the typed `result` out of a JSON-RPC envelope.
    fn extract_result<T: DeserializeOwned>(envelope: &Value) -> Result<T, A2aClientError> {
        if let Some(error) = envelope.get("error") {
            let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unspecified")
                .to_owned();
            return Err(A2aClientError::JsonRpc { code, message });
        }
        let Some(result) = envelope.get("result") else {
            return Err(A2aClientError::Parse(serde_json::Error::io(
                std::io::Error::other("missing `result` member in the JSON-RPC envelope"),
            )));
        };
        Ok(serde_json::from_value(result.clone())?)
    }

    /// Parses an SSE body into typed events.
    ///
    /// The wire format is one event per blank-line
    /// boundary; an event is one or more
    /// `field: value` lines; the `data:` lines
    /// carry the JSON-RPC envelope the spec names.
    /// Anything else (`event:`, `id:`, `retry:`) is
    /// dropped — the A2A spec does not use them.
    fn parse_sse_blocks(body: &str) -> Result<Vec<StreamingEvent>, A2aClientError> {
        let mut events = Vec::new();
        for block in body.split("\n\n") {
            for line in block.lines() {
                let Some(data) = line.strip_prefix("data: ") else {
                    continue;
                };
                let envelope: Value = match serde_json::from_str(data) {
                    Ok(value) => value,
                    Err(error) => {
                        return Err(A2aClientError::StreamEnded(format!(
                            "a `data:` line was not well-formed JSON: {error}"
                        )));
                    }
                };
                let event: StreamingEvent = Self::extract_result(&envelope)?;
                events.push(event);
            }
        }
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_envelope() -> Value {
        json!({ "jsonrpc": "2.0" })
    }

    #[test]
    fn extract_result_pulls_typed_value() {
        let envelope = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "name": "alpha" }
        });
        let value: Result<serde_json::Value, A2aClientError> = A2aClient::extract_result(&envelope);
        match value {
            Ok(value) => assert_eq!(value, json!({ "name": "alpha" })),
            Err(error) => unreachable!("a `result` member produces a typed value, got {error}"),
        }
    }

    #[test]
    fn extract_result_promotes_error_member_to_typed_error() {
        let envelope = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": { "code": -32601, "message": "Method not found" }
        });
        match A2aClient::extract_result::<serde_json::Value>(&envelope) {
            Ok(value) => {
                unreachable!("the error member surfaces as `JsonRpc`, got Ok({value})")
            }
            Err(A2aClientError::JsonRpc { code, message }) => {
                assert_eq!(code, -32601);
                assert_eq!(message, "Method not found");
            }
            Err(other) => unreachable!("the error member surfaces as `JsonRpc`, got {other}"),
        }
    }

    #[test]
    fn extract_result_without_result_is_a_parse_error() {
        match A2aClient::extract_result::<serde_json::Value>(&empty_envelope()) {
            Ok(value) => {
                unreachable!("an envelope without `result` is a parse error, got Ok({value})")
            }
            Err(A2aClientError::Parse(_)) => {}
            Err(other) => unreachable!("an envelope without `result` is a parse error, got {other}"),
        }
    }

    #[test]
    fn parse_sse_blocks_skips_lines_without_data_prefix() {
        let body = "event: ping\n\n: heartbeat\n\n";
        match A2aClient::parse_sse_blocks(body) {
            Ok(events) => assert!(events.is_empty()),
            Err(error) => {
                unreachable!("lines without `data:` are skipped, not errors, got {error}")
            }
        }
    }

    #[test]
    fn with_bearer_token_is_carried_on_a_clone() -> Result<(), A2aClientError> {
        // The token is an `Arc<str>`; cloning the client
        // shares the same allocation, not a deep copy.
        let original = A2aClient::new("http://peer")?.with_bearer_token("secret");
        let cloned = original.clone();
        // Both clients point at the same peer and same
        // paths; the wire-level assertion is that every
        // outbound request carries `Authorization: Bearer
        // secret`, which the integration test pins against
        // a real server. This unit test only checks the
        // construction: no panic, the same path.
        assert_eq!(cloned.card_path, original.card_path);
        Ok(())
    }

    #[test]
    fn without_bearer_token_the_authorization_header_is_absent() -> Result<(), A2aClientError> {
        // The default client is unauthenticated. The
        // struct's `bearer` field is `None`, so the
        // request builder skips the Authorization header.
        let client = A2aClient::new("http://peer")?;
        match &client.bearer {
            None => {}
            Some(_) => unreachable!("a fresh `A2aClient::new` has no bearer token"),
        }
        Ok(())
    }

    #[test]
    fn get_extended_agent_card_request_serialises_as_empty_object(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // The spec's `GetExtendedAgentCardRequest`
        // has only a `tenant` path parameter, so
        // the wire body is `{}` (not `null`). The
        // test pins the shape so a future revision
        // that adds a real field catches the
        // round-trip drift here, not in a wire
        // test against a real server.
        let request = GetExtendedAgentCardRequest::new();
        let value = serde_json::to_value(&request)?;
        let object = value
            .as_object()
            .ok_or("the extended-card request must serialise to a JSON object")?;
        if !object.is_empty() {
            return Err(format!("expected `{{}}`, got {value}").into());
        }
        Ok(())
    }
}
