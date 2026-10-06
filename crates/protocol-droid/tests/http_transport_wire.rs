//! The two halves of the Streamable HTTP transport, on a socket.
//!
//! Everything else this transport has is tested one side at a time. The
//! client's parser and the router's writer agree on the SSE grammar in their
//! own suites, which is exactly the arrangement where two components can each
//! be right and still disagree with each other — a header name spelled
//! differently, an id the client resumes from the server never issued, a media
//! type one side writes and the other refuses.
//!
//! So this file binds a real listener, serves the real router, and drives it
//! with the real client. Nothing is stubbed except the protocol layer, which is
//! the one piece T18 owns and is the smallest thing that can produce a session
//! and answer a method.
//!
//! Nothing here asserts on an intermediate value. Every claim is read off the
//! bytes that crossed the socket, because an assertion on a value both sides
//! already agreed on is not evidence that they agree on the wire.

#![cfg(feature = "http")]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use protocol_droid::adapters::primary::transport::http::{
    EventTail, Inbound, McpRouter, Refusal, RequestHandler, SecurityHeaders, ServerConfig,
    SessionHandle, SessionStore, Unsessioned,
};
use protocol_droid::adapters::secondary::transport::http::{
    HttpClient, HttpClientConfig, HttpClientError,
};
use protocol_droid::application::session::IdSource;
use protocol_droid::domain::EventId;
use protocol_droid::domain::session::SessionId;
use serde_json::{Value, json};

type TestError = Box<dyn std::error::Error>;

fn fail(message: impl Into<String>) -> TestError {
    message.into().into()
}

/// A session that is only ever a tail, so the store's `S` and the stream's
/// event source are the same type and this file has one moving part.
fn tail() -> Result<Arc<EventTail<Value>>, TestError> {
    let capacity = std::num::NonZeroUsize::new(8).ok_or("a capacity of zero is not a capacity")?;
    Ok(Arc::new(EventTail::new(capacity)))
}

// ---------------------------------------------------------------------------
// The protocol layer: the smallest thing that can hold a session
// ---------------------------------------------------------------------------

/// Answers `initialize` and `tools/list`, and nothing else.
///
/// Deliberately hand-written rather than built on the T12 server API: that API
/// would be the *thing under test* one task from now, and a test that routes
/// through the thing it is checking proves only that the thing agrees with
/// itself.
struct MiniServer {
    sessions: Arc<SessionStore<EventTail<Value>>>,
    calls: Arc<AtomicU64>,
}

impl MiniServer {
    /// The one tool this server publishes.
    ///
    /// A `fn` rather than a `const` because `json!` builds a `Map`, and that
    /// is not a `const fn`. A literal here would be the same value with the
    /// cost of being a second thing to keep in step.
    fn tools() -> Value {
        json!({
            "tools": [{
                "name": "echo",
                "description": "Returns its argument.",
                "inputSchema": { "type": "object", "properties": { "text": { "type": "string" } } },
            }]
        })
    }

    /// The method a body asks for, or `None` when it is not a JSON-RPC frame
    /// this server understands.
    fn method(body: &[u8]) -> Option<String> {
        let value: Value = serde_json::from_slice(body).ok()?;
        value.get("method")?.as_str().map(ToOwned::to_owned)
    }
}

impl RequestHandler<EventTail<Value>> for MiniServer {
    fn unsessioned(&self, body: &[u8]) -> Unsessioned {
        match Self::method(body).as_deref() {
            Some("initialize") => Unsessioned::Establishing,
            // Everything else in this test is a stateful call, so a body that
            // names no session is genuinely missing one.
            _ => Unsessioned::NeedsSession,
        }
    }

    fn dispatch(
        &self,
        call: Inbound<'_, EventTail<Value>>,
    ) -> impl core::future::Future<Output = Result<axum::response::Response, Refusal>> + Send {
        let body = call.body.to_vec();
        let sessions = Arc::clone(&self.sessions);
        self.calls.fetch_add(1, Ordering::SeqCst);
        // A request that opens a session has no id to answer with until the
        // protocol layer mints one, and the router cannot do it: the store is
        // the protocol layer's, and what a session *is* is a protocol-layer
        // decision. So the handler owns this, which is why it holds the store.
        let opened = matches!(call.session, SessionHandle::Establishing);
        let session = match &call.session {
            // Both of these answer without an id, for different reasons. A
            // request that opens a session has none yet; a stateless one has
            // none ever, because this server mints nothing for it and the
            // router's contract is that the handler decides.
            SessionHandle::Establishing | SessionHandle::Stateless => None,
            SessionHandle::Live { id, session } => Some((*id, Arc::clone(session))),
        };

        async move {
            let method = Self::method(&body).ok_or_else(|| Refusal::MalformedBody {
                reason: "not a JSON-RPC frame".to_owned(),
            })?;
            let result = match method.as_str() {
                "initialize" => json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": { "protocolVersion": "2025-06-18", "capabilities": {} },
                }),
                "tools/list" => json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "result": Self::tools(),
                }),
                other => json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "error": { "code": -32601, "message": format!("no such method: {other}") },
                }),
            };

            // Listing tools is also when this server publishes something, so a
            // `GET` that follows has a stream to deliver. Publishing on a read
            // is odd, but it is the only ordering a single-threaded test can
            // rely on: the push has provably happened before the GET arrives.
            if let Some((_, tail)) = &session
                && method == "tools/list"
            {
                for n in 1..=2 {
                    let _ = tail.push(json!({ "n": n }));
                }
            }

            // `initialize` is the one method that mints, and it mints once.
            let id = match (opened, session) {
                (true, _) => {
                    // A store that cannot mint is a server that cannot serve,
                    // and the one channel this handler has is a refusal.
                    let opened = sessions
                        .open(tail().map_err(|error| Refusal::Handler(error.to_string()))?)
                        .map_err(|error| Refusal::Handler(format!("no session: {error}")))?;
                    Some(opened)
                }
                (false, Some((id, _))) => Some(id),
                (false, None) => None,
            };

            // The body is a `Value` this server just built, so a serialise
            // failure is a bug here rather than anything a peer did. It still
            // is not a `panic!`: the adapter maps it into the one channel a
            // handler has, which is a refusal.
            let encoded = serde_json::to_vec(&result).map_err(|error| {
                Refusal::Handler(format!("the reply did not serialise: {error}"))
            })?;
            let mut response = axum::response::Response::new(axum::body::Body::from(encoded));
            if let Some(id) = id {
                let value =
                    axum::http::HeaderValue::from_str(&id.to_string()).map_err(|error| {
                        Refusal::Handler(format!("the id was not a header: {error}"))
                    })?;
                response.headers_mut().insert("mcp-session-id", value);
            }
            Ok(response)
        }
    }
}

/// Draws ids a test can name.
struct Counting(AtomicU64);

impl IdSource for Counting {
    fn next_session_id(&self) -> SessionId {
        SessionId::new(self.0.fetch_add(1, Ordering::SeqCst) + 1)
    }
}

// ---------------------------------------------------------------------------
// A server on a real port
// ---------------------------------------------------------------------------

/// A router bound to a loopback port, with the task serving it.
///
/// The port is `0`, so the OS picks one and two tests can run at once. A test
/// that hard-codes 8080 is a test that fails the second time anything else
/// takes the port, and the failure looks like a transport bug.
struct Live {
    url: String,
    sessions: Arc<SessionStore<EventTail<Value>>>,
    calls: Arc<AtomicU64>,
}

impl Live {
    async fn start() -> Result<Self, TestError> {
        let sessions = Arc::new(SessionStore::new(Arc::new(Counting(AtomicU64::new(0)))));
        let calls = Arc::new(AtomicU64::new(0));
        let handler = MiniServer {
            sessions: Arc::clone(&sessions),
            calls: Arc::clone(&calls),
        };

        let config = ServerConfig::new()?.with_security(SecurityHeaders::default());
        let router = McpRouter::new(config, Arc::clone(&sessions), handler).into_router()?;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        tokio::spawn(async move {
            // The listener is bound before this is spawned, so a connection
            // refused here would be a test-harness bug rather than a race
            // between bind and serve.
            let _ = axum::serve(listener, router).await;
        });

        Ok(Self {
            url: format!("http://127.0.0.1:{port}/mcp"),
            sessions,
            calls,
        })
    }

    fn client(&self) -> Result<HttpClient, TestError> {
        Ok(HttpClient::new(HttpClientConfig::new(&self.url)?)?)
    }

    /// How many requests reached the protocol layer.
    ///
    /// A count rather than a bool so a test can say "twice" and not only
    /// "once or more", and so a second request that arrives twice is visible.
    fn dispatches(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }
}

/// The `initialize` body, and the other two calls a test reaches for.
const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#;
const LIST_TOOLS: &str = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
const NO_SUCH_METHOD: &str = r#"{"jsonrpc":"2.0","id":3,"method":"nope/nope"}"#;

// ---------------------------------------------------------------------------
// The claims
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_client_establishes_a_session_over_the_wire() -> Result<(), TestError> {
    // The session id is minted by the server and read by the client out of a
    // response header. Every hop in between is real: a socket, an HTTP parse,
    // a header the two sides had to spell the same way, and a client that
    // remembered it. A stubbed `HeaderMap` proves none of that.
    let live = Live::start().await?;
    let client = live.client()?;

    let document = client.post(INITIALIZE.as_bytes(), false).await?;
    if !document.status().is_success() {
        return Err(fail(format!("initialize answered {}", document.status())));
    }
    let issued = document
        .session_id()
        .ok_or("the server issued no session for initialize")?;

    // And the client kept it, which is the half that is easy to get wrong: a
    // client that reads the header and then forgets it is a client whose
    // second request is refused for a reason the caller cannot see.
    if client.session_id() != Some(issued) {
        return Err(fail("the client did not keep the session it was issued"));
    }
    Ok(())
}

#[tokio::test]
async fn a_second_call_carries_the_session_the_first_was_issued() -> Result<(), TestError> {
    // The same fact read from the other side: the id on the wire is one the
    // store can actually resolve, not one the server would have rejected.
    let live = Live::start().await?;
    let client = live.client()?;
    let _ = client.post(INITIALIZE.as_bytes(), false).await?;

    let document = client.post(LIST_TOOLS.as_bytes(), false).await?;
    if document.status() != axum::http::StatusCode::OK {
        return Err(fail(format!("tools/list answered {}", document.status())));
    }
    // One initialize, one tools/list. A client that silently re-initialized
    // would pass the status check above and leave the caller holding a
    // session it never asked for.
    if live.dispatches() != 2 {
        return Err(fail(format!(
            "the protocol layer saw {} requests, expected two",
            live.dispatches()
        )));
    }
    // A `400` here would mean the id did not survive the round trip through
    // the store, so the status is the assertion.
    let tools: Value = serde_json::from_slice(document.body())?;
    if tools.pointer("/result/tools").is_none() {
        return Err(fail(format!("tools/list answered {tools}")));
    }
    if live.sessions.len()? != 1 {
        return Err(fail(format!(
            "the server holds {} sessions, expected one",
            live.sessions.len()?
        )));
    }
    Ok(())
}

#[tokio::test]
async fn a_call_without_a_session_is_refused_over_the_wire() -> Result<(), TestError> {
    // A client that has not initialized cannot list tools, and the answer is
    // a `400` rather than a `200` with an empty list. The distinction is the
    // whole point: an empty list is a successful query, and a client cannot
    // tell a server with no tools from one that was not asked.
    let live = Live::start().await?;
    let client = live.client()?;

    // A refusal is an `Err`, not a `2xx`-shaped response carrying a bad status.
    // That is the client's design — a peer that answered `4xx` did not answer
    // the call — and it is what stops a caller reading a refusal as a result.
    // The status is therefore inside the error, and this asserts on it there.
    let refused = match client.post(LIST_TOOLS.as_bytes(), false).await {
        Err(HttpClientError::Refused { status, .. }) => status,
        Err(other) => return Err(fail(format!("expected a refusal, got {other}"))),
        Ok(document) => {
            return Err(fail(format!(
                "an unsessioned tools/list answered {}",
                document.status()
            )));
        }
    };
    if refused != axum::http::StatusCode::BAD_REQUEST {
        return Err(fail(format!(
            "an unsessioned tools/list was refused {refused}"
        )));
    }
    // The status alone would be satisfied by a server that ran the handler and
    // then decided to refuse the answer. The handler never saw the body, which
    // is the part that keeps an unauthenticated request from reaching a tool.
    if live.dispatches() != 0 {
        return Err(fail(format!(
            "{} request(s) reached the protocol layer unauthenticated",
            live.dispatches()
        )));
    }
    Ok(())
}

#[tokio::test]
async fn a_json_rpc_error_survives_the_round_trip() -> Result<(), TestError> {
    // A JSON-RPC error is a `200` with an error body, not an HTTP status. A
    // client that confuses the two answers a peer's "no such method" with a
    // transport failure and retries it forever, so this asserts both halves:
    // the status is success and the body carries the code.
    let live = Live::start().await?;
    let client = live.client()?;
    let _ = client.post(INITIALIZE.as_bytes(), false).await?;

    let document = client.post(NO_SUCH_METHOD.as_bytes(), false).await?;
    if !document.status().is_success() {
        return Err(fail(format!(
            "a JSON-RPC error carried the HTTP status {}",
            document.status()
        )));
    }
    let body: Value = serde_json::from_slice(document.body())?;
    let code = body
        .pointer("/error/code")
        .and_then(Value::as_i64)
        .ok_or("the error body carried no code")?;
    if code != -32601 {
        return Err(fail(format!("the error code was {code}, not -32601")));
    }
    Ok(())
}

#[tokio::test]
async fn the_streamed_answer_is_parsed_by_the_same_grammar() -> Result<(), TestError> {
    // The half that a socket is the only way to test. The client's parser and
    // the server's encoder are the two ends of one grammar, and each half
    // passing its own suite says nothing about whether the halves agree — a
    // `data: ` line written with one space and read with two, a blank line
    // that one side counts and the other does not. Here the server streams
    // and the client reads events, so the grammar is only satisfied if both
    // ends are right.
    let live = Live::start().await?;
    let client = live.client()?;
    let _ = client.post(INITIALIZE.as_bytes(), false).await?;
    // Publishing happens on `tools/list`, so the GET below has something to
    // deliver rather than parking on a tail nothing will ever push to.
    let _ = client.post(LIST_TOOLS.as_bytes(), false).await?;

    // No cursor: the client is asking for everything the server still holds,
    // which is what a first `GET` means. The tail's own ids are assigned on
    // push, so they are 1 and 2 without this test choosing them.
    let mut stream = client.open_stream(None, true).await?;
    let mut delivered: Vec<EventId> = Vec::new();
    let mut payloads: Vec<String> = Vec::new();
    // Bounded, so a server that never dispatches fails the test rather than
    // parking it until the keep-alive — the failure a reader with the wrong
    // grammar produces is silence, and silence needs a deadline to be a
    // failure at all.
    let mut rounds = 32_u32;
    while delivered.len() < 2 {
        rounds = rounds.saturating_sub(1);
        if rounds == 0 {
            return Err(fail(format!(
                "the stream delivered {delivered:?} and then stopped answering"
            )));
        }
        let Some(event) = stream.next_event().await? else {
            return Err(fail(format!(
                "the stream ended after {delivered:?}, wanted two events"
            )));
        };
        if let Some(id) = event.id() {
            delivered.push(id);
        }
        payloads.push(event.data().to_owned());
    }

    if delivered != vec![EventId::new(1), EventId::new(2)] {
        return Err(fail(format!(
            "the ids crossed the wire as {delivered:?}, not 1 and 2 in order"
        )));
    }
    if payloads != vec![String::from(r#"{"n":1}"#), String::from(r#"{"n":2}"#)] {
        return Err(fail(format!(
            "the payloads crossed the wire as {payloads:?}"
        )));
    }
    if stream.last_delivered() != Some(EventId::new(2)) {
        return Err(fail(format!(
            "the stream's cursor was {:?}, expected 2",
            stream.last_delivered()
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The router's right to name the media type
// ---------------------------------------------------------------------------

/// Answers with a media type of its own choosing, and a status of its own
/// choosing, without raising a [`Refusal`].
struct Framing {
    content_type: &'static str,
    status: u16,
}

impl RequestHandler<EventTail<Value>> for Framing {
    fn unsessioned(&self, _body: &[u8]) -> Unsessioned {
        // Establishing, so a POST reaches this handler at all. A fixture
        // answering `NeedsSession` would be refused by the router for want of
        // a session before it could choose a framing, and both tests would
        // pass against the router's own refusal rather than against this.
        Unsessioned::Establishing
    }

    fn dispatch(
        &self,
        _call: Inbound<'_, EventTail<Value>>,
    ) -> impl core::future::Future<Output = Result<axum::response::Response, Refusal>> + Send {
        let content_type = self.content_type;
        let status = self.status;
        async move {
            let mut response = axum::response::Response::new(axum::body::Body::from("{}"));
            *response.status_mut() = axum::http::StatusCode::from_u16(status)
                .map_err(|error| Refusal::Handler(format!("{status} is not a status: {error}")))?;
            let value = axum::http::HeaderValue::from_static(content_type);
            response
                .headers_mut()
                .insert(axum::http::header::CONTENT_TYPE, value);
            Ok(response)
        }
    }
}

/// A server whose only job is to answer with a chosen framing.
async fn framed(
    content_type: &'static str,
    status: u16,
) -> Result<(HttpClient, axum::http::StatusCode), TestError> {
    let sessions = Arc::new(SessionStore::new(Arc::new(Counting(AtomicU64::new(0)))));
    let config = ServerConfig::new()?.with_security(SecurityHeaders::default());
    let router = McpRouter::new(
        config,
        Arc::clone(&sessions),
        Framing {
            content_type,
            status,
        },
    )
    .into_router()?;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let url = format!("http://127.0.0.1:{port}/mcp");
    let client = HttpClient::new(HttpClientConfig::new(&url)?)?;
    let _ = sessions;
    Ok((client, axum::http::StatusCode::from_u16(status)?))
}

#[tokio::test]
async fn a_streamed_answer_keeps_its_own_media_type() -> Result<(), TestError> {
    // A POST may legitimately be answered with a stream — a `tools/call` that
    // reports progress is the 2025-03-26 shape. The router names the media type
    // of a JSON answer, so a handler that framed its own must keep it:
    // rewriting `text/event-stream` to `application/json` turns a working
    // streaming response into a parse error on the client, and the client is
    // right to refuse it.
    let (client, _) = framed("text/event-stream", 200).await?;
    let reply = client.post_streaming(INITIALIZE.as_bytes(), false).await?;
    match reply {
        protocol_droid::adapters::secondary::transport::http::HttpReply::Stream(_) => Ok(()),
        protocol_droid::adapters::secondary::transport::http::HttpReply::Document(document) => {
            Err(fail(format!(
                "a streamed answer was re-framed as {}",
                document.content_type().unwrap_or("nothing")
            )))
        }
    }
}

#[tokio::test]
async fn a_handlers_own_refusal_status_keeps_its_own_media_type() -> Result<(), TestError> {
    // The other half of the same guard. A handler that answers `4xx` as `Ok`
    // — rather than raising a [`Refusal`] — has already said "this is an
    // error, and here is how it is framed". The router stamping that would
    // overwrite a deliberate answer, and a proxy between the two would read a
    // body the sender never wrote.
    let (client, expected) = framed("application/problem+json", 429).await?;
    let answered = client.post(INITIALIZE.as_bytes(), false).await;
    match answered {
        Err(HttpClientError::Refused { content_type, .. }) => {
            if content_type.as_deref() != Some("application/problem+json") {
                return Err(fail(format!(
                    "the handler's media type arrived as {content_type:?}"
                )));
            }
            if expected != axum::http::StatusCode::TOO_MANY_REQUESTS {
                return Err(fail("the fixture did not answer 429"));
            }
            Ok(())
        }
        Err(other) => Err(fail(format!("expected a refusal, got {other}"))),
        Ok(document) => Err(fail(format!(
            "a 429 arrived as a success carrying {}",
            document.status()
        ))),
    }
}
