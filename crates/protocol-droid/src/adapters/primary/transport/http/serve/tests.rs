//! This module's tests.
//!
//! Out of `serve` so that reading the module means reading the handler and the
//! wiring rather than the tests that sit underneath both. The precedent is
//! `dispatch/tests.rs`.

use core::pin::Pin;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

use super::builder::{default_ceiling, routes};
use super::{HttpBuilder, ServeError};
use crate::adapters::primary::transport::http::router::RouterError;
use crate::adapters::primary::transport::http::{
    Anonymous, AuthPolicy, Authenticator, Credentials, ServerConfig, Streaming,
};
use crate::application::server::{Server, ToolHandler};
use crate::domain::ir::JsonObject;
use crate::domain::registry::auth::ScopeSet;
use crate::domain::registry::{AuthContext, Primitive, Registered, Scope, Tool};

/// Anything a test here can fail at, gathered so a test can `?` through
/// unrelated error types without unwrapping.
#[derive(Debug)]
enum Failure {
    /// A test assumption about the crate's own defaults did not hold.
    Expectation(&'static str),
    /// Two values that should have matched and did not.
    ///
    /// Separate from [`Self::Expectation`] because "the media type was
    /// this, the media type should have been that" is the assertion this
    /// module could not previously make at all — the two values are the
    /// whole point of the test, and folding them into one message would
    /// mean printing only half of it.
    Mismatch(String, String),
    /// The transport refused something a test did not expect it to.
    Serve(ServeError),
    /// A request never produced a response.
    NoResponse,
}

impl core::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Expectation(what) => write!(formatter, "expected {what}"),
            Self::Mismatch(found, wanted) => {
                write!(formatter, "expected {wanted}, got {found}")
            }
            Self::Serve(error) => write!(formatter, "the transport refused: {error}"),
            Self::NoResponse => formatter.write_str("the request produced no response"),
        }
    }
}

impl std::error::Error for Failure {}

impl From<ServeError> for Failure {
    fn from(error: ServeError) -> Self {
        Self::Serve(error)
    }
}

/// A server with nothing on it, which is the smallest thing a consumer
/// would actually serve.
fn server() -> Server {
    Server::builder("test").build()
}

/// The JSON-RPC document a response body carries.
///
/// A `POST` to a sessioned route is answered with an event stream, so the
/// document a test wants is one `data:` frame inside it — and for a route
/// with no session there is no stream at all and the body is the document.
/// Both are the same answer on the wire; reading only one of them is how a
/// test that meant "the server replied correctly" ends up reporting "the
/// server replied as `application/json`" instead.
fn document_in(body: &[u8]) -> Result<Value, Failure> {
    let text = std::str::from_utf8(body)
        .map_err(|_| Failure::Expectation("the reply body to be UTF-8"))?;
    if let Ok(document) = serde_json::from_str::<Value>(text) {
        return Ok(document);
    }
    // The *last* frame, not the first. A sessioned `POST` is answered with
    // an event stream, and the frames in it are ordered as they happened —
    // so a tool that logs or reports progress puts its notification ahead
    // of its result, and taking the first frame answers the question with
    // the notification. The result is what closes the exchange, and it is
    // matched on its id rather than taken positionally: a `POST` that also
    // carries the answer to a server-initiated request has two documents
    // in it, and only one of them is this request's answer.
    let wanted = request_id_hint(text);
    let mut found = None;
    for frame in text.split("\n\n") {
        let Some(payload) = frame.lines().find_map(|line| line.strip_prefix("data: ")) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(payload) else {
            continue;
        };
        let answers = value
            .get("id")
            .is_some_and(|id| wanted.as_ref().is_none_or(|wanted| id == wanted));
        if answers {
            found = Some(value);
        }
    }
    if found.is_none() {
        // Nothing matched the id, so fall back to the last document rather
        // than reporting no reply at all — a test that cared about the id
        // would have said so.
        for frame in text.split("\n\n") {
            let Some(payload) = frame.lines().find_map(|line| line.strip_prefix("data: "))
            else {
                continue;
            };
            if let Ok(value) = serde_json::from_str::<Value>(payload) {
                found = Some(value);
            }
        }
    }
    found.ok_or(Failure::Expectation(
        "the reply to carry a JSON-RPC document",
    ))
}

/// The request id a reply answers, read off the frames themselves.
///
/// There is no other way to tell a `POST`'s own answer from the answer to a
/// server-initiated request travelling in the same stream, and taking the
/// wrong one turns a test about listing tools into a test about whatever
/// the server last asked.
fn request_id_hint(text: &str) -> Option<serde_json::Value> {
    let mut last = None;
    for frame in text.split("\n\n") {
        let Some(payload) = frame.lines().find_map(|line| line.strip_prefix("data: ")) else {
            continue;
        };
        if let Ok(value) = serde_json::from_str::<Value>(payload)
            && (value.get("result").is_some() || value.get("error").is_some())
        {
            last = value.get("id").cloned();
        }
    }
    last
}

/// Ids a test can predict, so an assertion about a minted session can name
/// the value it expects.
fn test_ids() -> std::sync::Arc<dyn crate::application::session::IdSource + Send + Sync> {
    #[derive(Debug)]
    struct Counting(core::sync::atomic::AtomicU64);

    impl crate::application::session::IdSource for Counting {
        fn next_session_id(&self) -> crate::domain::session::SessionId {
            crate::domain::session::SessionId::new(
                self.0.fetch_add(1, core::sync::atomic::Ordering::SeqCst),
            )
        }
    }

    std::sync::Arc::new(Counting(core::sync::atomic::AtomicU64::new(1)))
}

/// The default ceiling is a revision a handshake can complete on.
///
/// `latest_known()` is `2026-07-28`, the revision SEP-2575 removed
/// `initialize` from. A server gated on it could never finish talking to
/// anybody, and every request it refused would name a feature rather than
/// the real cause.
#[test]
fn the_default_ceiling_has_a_handshake() -> Result<(), Failure> {
    let ceiling = default_ceiling();
    if !crate::domain::handshake_defined_in(ceiling) {
        return Err(Failure::Expectation(
            "the default ceiling to be a revision with an initialize",
        ));
    }
    Ok(())
}

/// Binding on port `0` reports the port the OS chose, not `0`.
///
/// The whole reason [`HttpBuilder::bind`] is separate from serving: a
/// caller that asked for a free port needs the real one back to tell anyone
/// where to connect.
#[tokio::test]
async fn binding_reports_the_address_the_os_chose() -> Result<(), Failure> {
    let (_, report) = HttpBuilder::new(server())
        .addr(SocketAddr::from(([127, 0, 0, 1], 0)))
        .bind()
        .await?;
    if report.addr.port() == 0 {
        return Err(Failure::Expectation("a real port back"));
    }
    if !report.addr.ip().is_loopback() {
        return Err(Failure::Expectation("a loopback address by default"));
    }
    Ok(())
}

/// Binding an address already in use fails rather than quietly moving.
///
/// Silently moving is the failure that hurts: a caller prints the address,
/// a client connects to whatever is actually there, and the two do not
/// match.
#[tokio::test]
async fn binding_a_taken_address_fails() -> Result<(), Failure> {
    let (listener, report) = HttpBuilder::new(server())
        .addr(SocketAddr::from(([127, 0, 0, 1], 0)))
        .bind()
        .await?;
    let second = HttpBuilder::new(server()).addr(report.addr).bind().await;
    // The held `listener` is what makes the address taken; drop it only
    // after the attempt so the port is still busy.
    drop(listener);
    match second {
        Err(ServeError::Bind { addr, .. }) => {
            if addr != report.addr {
                return Err(Failure::Expectation("the refused address reported back"));
            }
            Ok(())
        }
        Err(other) => Err(Failure::Serve(other)),
        Ok(_) => Err(Failure::Expectation("a refusal rather than a second bind")),
    }
}

/// `Streaming::Json` — the default — has to mean `application/json` on a
/// *sessioned* POST, and it did not.
///
/// Issue #20: `initialize` answered a bare document while every request
/// after it answered an event stream, because the framing was decided by
/// whether a session existed rather than by how the server was configured.
/// `streaming()` was read in exactly one place, a log field. A consumer
/// that never touched `Streaming` got `text/event-stream` and an
/// `id: N\ndata: {...}` body where it expected a document.
///
/// Both modes are asserted here rather than only the default, because the
/// previous test (`the_streaming_mode_never_changes_the_get_route`) covers
/// the GET route and would not have caught this: `Streaming` has never
/// changed what the GET route does.
#[tokio::test]
async fn the_streaming_mode_decides_how_a_sessioned_post_is_framed() -> Result<(), Failure> {
    for (streaming, expected) in [
        (Streaming::Json, "application/json"),
        (Streaming::EventStream, "text/event-stream"),
    ] {
        let config = ServerConfig::new()
            .map_err(|_| Failure::Expectation("the config did not parse"))?
            .with_streaming(streaming);
        let router = crate::adapters::primary::transport::http::serve::test_routes(
            server(),
            default_ceiling(),
            Some(config),
            Some(test_ids()),
        )?;

        let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#;
        let (status, minted, _) = post(&router, initialize, None, None).await?;
        if status != StatusCode::OK {
            return Err(Failure::Expectation("initialize was refused"));
        }
        let Some(session) = minted else {
            return Err(Failure::Expectation("no session was minted"));
        };

        let (status, _, _) = post(
            &router,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
            None,
            Some(&session),
        )
        .await?;
        if status != StatusCode::OK {
            return Err(Failure::Expectation("a sessioned tools/list was refused"));
        }

        let response = raw_post(
            &router,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
            Some(&session),
        )
        .await?;
        let media = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        if !media.starts_with(expected) {
            return Err(Failure::Mismatch(media, expected.to_owned()));
        }
    }
    Ok(())
}

/// A tool that asks the peer a question cannot do it on a server that
/// answers with one document, and it has to find out rather than hang.
///
/// The property under test is that the call *returns*. Before this change
/// a `Streaming::Json` server pushed the tool's question into a ring no one
/// drains and waited on a channel that only the end of the session could
/// close: a hang with no error and no timeout. Whatever the refusal turns
/// out to be, the assertion is that there is one.
///
/// The refusal's wording is pinned by `an_unreadable_exchange_names_the_setting`
/// in the `exchange` module, which can reach the exchange directly. Here
/// the capability gate answers first — this server declares no elicitation
/// capability — so what arrives here is that gate's refusal, not the
/// exchange's.
#[tokio::test]
async fn a_tool_that_asks_the_peer_is_answered_rather_than_left_waiting() -> Result<(), Failure>
{
    let config = ServerConfig::new()
        .map_err(|_| Failure::Expectation("the config did not parse"))?
        .with_streaming(Streaming::Json);
    let router = crate::adapters::primary::transport::http::serve::test_routes(
        eliciting_server()?,
        default_ceiling(),
        Some(config),
        Some(test_ids()),
    )?;

    let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#;
    let (status, minted, _) = post(&router, initialize, None, None).await?;
    if status != StatusCode::OK {
        return Err(Failure::Expectation("initialize was refused"));
    }
    let Some(session) = minted else {
        return Err(Failure::Expectation("no session was minted"));
    };

    let (status, _, document) = post(
        &router,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"elicit","arguments":{}}}"#,
        None,
        Some(&session),
    )
    .await?;
    if status != StatusCode::OK {
        return Err(Failure::Expectation("the call was refused outright"));
    }
    if document.to_string().contains("reason") {
        return Ok(());
    }
    Err(Failure::Mismatch(
        document.to_string(),
        "an answer naming why the question could not be asked".to_owned(),
    ))
}

/// `initialize` is answered, and mints a session the client can name.
///
/// The assertion that matters most is the header. Without it a client that
/// completed a handshake still cannot make a second request, and the
/// failure surfaces as a `400` on a call the server would have served
/// happily — which reads as a broken server rather than a missing header.
#[tokio::test]
async fn initialize_answers_and_mints_a_session() -> Result<(), Failure> {
    let router = crate::adapters::primary::transport::http::serve::test_routes(
        server(),
        default_ceiling(),
        None,
        Some(test_ids()),
    )?;
    let body = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#;
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("host", "localhost")
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream")
                .body(Body::from(body.to_vec()))
                .map_err(|_| Failure::Expectation("the request built"))?,
        )
        .await
        .map_err(|_| Failure::NoResponse)?;

    if response.status() != StatusCode::OK {
        return Err(Failure::Expectation("a 200 for a well-formed initialize"));
    }
    if response.headers().get("mcp-session-id").is_none() {
        return Err(Failure::Expectation(
            "an mcp-session-id header naming the new session",
        ));
    }
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .map_err(|_| Failure::Expectation("the reply body to be readable"))?;
    let document = document_in(&bytes)?;
    if document.get("result").is_none() {
        return Err(Failure::Expectation("a result, not an error"));
    }
    Ok(())
}

/// A stateful call naming no session is refused with a `400`.
///
/// The `400` is the router's, and it is the right one: the peer asked a
/// question belonging to a session and did not say which. Answering it as
/// a protocol error would answer a different question, and a client would
/// retry it the same way forever.
#[tokio::test]
async fn a_stateful_call_with_no_session_is_refused() -> Result<(), Failure> {
    let router = crate::adapters::primary::transport::http::serve::test_routes(
        server(),
        default_ceiling(),
        None,
        Some(test_ids()),
    )?;
    let body = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("host", "localhost")
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream")
                .body(Body::from(body.as_bytes().to_vec()))
                .map_err(|_| Failure::Expectation("the request built"))?,
        )
        .await
        .map_err(|_| Failure::NoResponse)?;
    if response.status() != StatusCode::BAD_REQUEST {
        return Err(Failure::Expectation(
            "a 400 for a stateful call with no session header",
        ));
    }
    Ok(())
}

/// A body that is not a JSON-RPC message is refused, not dispatched.
///
/// The distinction that matters: a peer sending nonsense is a `400`, while
/// a peer sending a well-formed request this server cannot serve is a
/// `200` carrying a JSON-RPC error. Collapsing the two makes a client
/// retry a request that will never succeed.
#[tokio::test]
async fn a_body_that_is_not_jsonrpc_is_refused() -> Result<(), Failure> {
    let router = crate::adapters::primary::transport::http::serve::test_routes(
        server(),
        default_ceiling(),
        None,
        Some(test_ids()),
    )?;
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("host", "localhost")
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream")
                .body(Body::from("{not json"))
                .map_err(|_| Failure::Expectation("the request built"))?,
        )
        .await
        .map_err(|_| Failure::NoResponse)?;
    if response.status() != StatusCode::BAD_REQUEST {
        return Err(Failure::Expectation(
            "a 400 for a body that is not a message",
        ));
    }
    Ok(())
}

// -----------------------------------------------------------------
// Issue #10: the builder can express the documented builder path —
// a deployment that wires an authenticator and a `Require` policy
// does not have to drop `HttpBuilder` and hand-assemble the
// router. The pass-throughs land in `routes` so the assembly path
// is single, not two.
// -----------------------------------------------------------------

/// The new builder methods do not have to be called — the default
/// builder still builds. This is the path every consumer that
/// has not wired auth takes, and changing the default would be a
/// silent breaking change.
#[tokio::test]
async fn the_default_builder_still_mounts() -> Result<(), Failure> {
    let parts = routes(HttpBuilder::new(server()))?;
    // Mounted. The test passes if `routes` returns `Ok`.
    let _ = parts.router;
    Ok(())
}

/// A builder with `with_authenticator` + `with_policy(Require)` is
/// the safe combination (issue #11): a deployment that wants a
/// credential can now say so on the builder, without dropping to
/// the hand-assembled router.
#[tokio::test]
async fn builder_authenticator_with_require_policy_mounts() -> Result<(), Failure> {
    let parts = routes(
        HttpBuilder::new(server())
            .with_authenticator(Arc::new(Anonymous))
            .with_policy(AuthPolicy::Require(ScopeSet::all())),
    )?;
    let _ = parts.router;
    Ok(())
}

/// The unsafe combination (issue #11) is now caught at the builder
/// too: `with_authenticator` without `with_policy` returns
/// `Routes` at build time rather than serving a publicly-readable
/// endpoint.
#[tokio::test]
async fn builder_authenticator_without_policy_fails_closed() -> Result<(), Failure> {
    match routes(HttpBuilder::new(server()).with_authenticator(Arc::new(Anonymous))) {
        Err(ServeError::Routes(RouterError::UnprotectedAuthenticator)) => Ok(()),
        Err(other) => Err(Failure::Serve(other)),
        Ok(_) => Err(Failure::Expectation("the unsafe combination was accepted")),
    }
}

// -----------------------------------------------------------------
// Issue #19: the principal the router authenticated now reaches the
// handler. Every visibility gate reads `ctx.peer().auth()`, and the
// HTTP transport was building that peer from the session rather than
// from the request — so an authenticated caller was answered as
// anonymous, and every scope-gated primitive was invisible to the very
// caller entitled to it.
// -----------------------------------------------------------------

/// The one tool the servers in this section publish.
const GATED: &str = "gated";

/// A server whose only tool tries to elicit from the peer.
///
/// The tool is the point: without a handler that asks a question and
/// waits, there is nothing for the unreadable-exchange refusal to refuse,
/// and the test could not tell "the exchange was refused" from "no tool
/// was called".
fn eliciting_server() -> Result<Server, Failure> {
    const ELICIT: &str = "elicit";
    let tool = Tool::new(ELICIT, r#"{"type":"object"}"#)
        .map_err(|_| Failure::Expectation("a tool schema the crate accepts"))?;
    // The refusal is carried back as the tool's text rather than raised as
    // a JSON-RPC error, because a tool that could not ask its question is
    // still a tool that ran and answered — and the point of the test is
    // that the caller finds out, with a reason, rather than hanging.
    // `"{}"` is the one string `JsonObject::new` cannot refuse, so the empty
    // object is built once here and cloned per call rather than being an
    // `unwrap` on a fact about the type.
    let empty =
        JsonObject::new("{}").map_err(|_| Failure::Expectation("an empty object parses"))?;
    let handler: ToolHandler = Arc::new(move |context, _arguments| {
        let empty = empty.clone();
        Box::pin(async move {
            match context.elicit(&empty).await {
                Ok(answer) => answer,
                // Carried back as the tool's own result rather than raised
                // as a JSON-RPC error: a tool that could not ask its
                // question still ran and still answered, and what is under
                // test is that the caller finds out with a reason instead
                // of hanging.
                Err(error) => JsonObject::new(format!("{{\"reason\":\"{error:?}\"}}"))
                    .unwrap_or_else(|_| empty.clone()),
            }
        })
    });
    Ok(Server::builder("elicitation-on-json")
        .restricted_tool(Registered::from(Primitive::from(tool)), handler)
        .build())
}

/// A server whose only tool is invisible without the `Read` scope.
///
/// The scope is the assertion's teeth. An ungated tool is visible to
/// everyone, so a test built on one cannot tell an authenticated caller
/// from an anonymous one and would pass either way.
fn gated_server() -> Result<Server, Failure> {
    let tool = Tool::new(GATED, r#"{"type":"object"}"#)
        .map_err(|_| Failure::Expectation("a tool schema the crate accepts"))?;
    let handler: ToolHandler =
        Arc::new(|_context, arguments| Box::pin(async move { arguments }));
    Ok(Server::builder("auth-propagation")
        .restricted_tool(
            Registered::from(Primitive::from(tool)).requiring_scope(Scope::Read),
            handler,
        )
        .build())
}

/// `Bearer alice` authenticates as a principal holding `Read`.
///
/// Every other credential — including none at all — authenticates as a
/// *named* principal holding nothing. The naming is load-bearing: an
/// anonymous context is indistinguishable from a peer the transport never
/// authenticated, which is the state this section exists to rule out.
/// The token `Tokens` recognises, named once so the tests and the
/// authenticator cannot drift apart.
const TOKEN_READ: &str = "tok-read";

#[derive(Debug, Clone, Copy)]
struct Tokens;

impl Authenticator for Tokens {
    fn authenticate<'a>(
        &'a self,
        presented: &'a Credentials<'_>,
    ) -> Pin<Box<dyn Future<Output = AuthContext> + Send + 'a>> {
        let holds_read = presented
            .authorization()
            .is_some_and(|presented| presented == TOKEN_READ);
        Box::pin(std::future::ready(if holds_read {
            AuthContext::new("alice", ScopeSet::empty().with(Scope::Read))
        } else {
            AuthContext::new("bob", ScopeSet::empty())
        }))
    }
}

/// The router these tests drive.
///
/// `Require(ScopeSet::empty())` rather than `Public`, for two reasons. An
/// endpoint with an authenticator wired under a `Public` policy is refused
/// at mount time, and an empty requirement is the only policy that admits
/// a *named* caller holding nothing. So both callers get past the endpoint
/// policy and differ only at the visibility gate, which is the claim under
/// test — a `403` here would prove nothing about the gate.
fn auth_routes() -> Result<axum::Router, Failure> {
    Ok(routes(
        HttpBuilder::new(gated_server()?)
            .ids(test_ids())
            .with_authenticator(Arc::new(Tokens))
            .with_policy(AuthPolicy::Require(ScopeSet::empty())),
    )?
    .router)
}

/// One POST to `/mcp`, with the credential and session the test chose.
///
/// Reports the status, the minted session id if one was minted, and the
/// decoded reply — all read off the response rather than off a value both
/// sides already agreed on.
/// One POST, returned undecoded.
///
/// [`post`] reads the body into a document, which is the right shape for
/// every assertion except the one this file could not make: a
/// `text/event-stream` body is not a document, so a helper that decoded
/// it would fail before the media type was ever read, and the test that
/// needs to see the header needs the response untouched.
async fn raw_post(
    router: &axum::Router,
    body: &str,
    session: Option<&str>,
) -> Result<axum::response::Response, Failure> {
    let mut request = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "localhost")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream");
    if let Some(session) = session {
        request = request.header("mcp-session-id", session);
    }
    let built = request
        .body(Body::from(body.as_bytes().to_vec()))
        .map_err(|_| Failure::Expectation("the request built"))?;
    router
        .clone()
        .oneshot(built)
        .await
        .map_err(|_| Failure::NoResponse)
}

async fn post(
    router: &axum::Router,
    body: &str,
    credential: Option<&str>,
    session: Option<&str>,
) -> Result<(StatusCode, Option<String>, Value), Failure> {
    let mut request = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "localhost")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream");
    if let Some(credential) = credential {
        request = request.header("authorization", credential);
    }
    if let Some(session) = session {
        request = request.header("mcp-session-id", session);
    }
    let built = request
        .body(Body::from(body.as_bytes().to_vec()))
        .map_err(|_| Failure::Expectation("the request built"))?;
    let response = router
        .clone()
        .oneshot(built)
        .await
        .map_err(|_| Failure::NoResponse)?;
    let status = response.status();
    let minted = response
        .headers()
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .map_err(|_| Failure::Expectation("the reply body to be readable"))?;
    let document = document_in(&bytes)?;
    Ok((status, minted, document))
}

/// The tool names a `tools/list` reply offers.
fn listed(document: &Value) -> Result<Vec<String>, Failure> {
    let tools = document
        .get("result")
        .and_then(|result| result.get("tools"))
        .and_then(Value::as_array)
        .ok_or(Failure::Expectation("a tools array in the result"))?;
    Ok(tools
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .collect())
}

/// The handshake, as the caller holding `Read`, naming the new session.
async fn open_session(router: &axum::Router) -> Result<String, Failure> {
    let (status, minted, _) = post(
        router,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
        Some("tok-read"),
        None,
    )
    .await?;
    if status != StatusCode::OK {
        return Err(Failure::Expectation("a 200 for a well-formed initialize"));
    }
    minted.ok_or(Failure::Expectation(
        "an mcp-session-id naming the new session",
    ))
}

/// An authenticated caller sees what its scopes earn.
///
/// The claim issue #19 was filed on: the peer the listing is filtered
/// against is built from the request, so the principal that authenticated
/// this request is the one the gate reads.
#[tokio::test]
async fn an_authenticated_caller_sees_what_its_scopes_earn() -> Result<(), Failure> {
    let router = auth_routes()?;
    let session = open_session(&router).await?;

    let (status, _, reply) = post(
        &router,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
        Some("tok-read"),
        Some(&session),
    )
    .await?;
    if status != StatusCode::OK {
        return Err(Failure::Expectation("a 200 for tools/list"));
    }
    if !listed(&reply)?.iter().any(|name| name == GATED) {
        return Err(Failure::Expectation(
            "the Read-gated tool listed for a caller holding Read",
        ));
    }
    Ok(())
}

/// The principal is read from the request, so two requests on one session
/// are filtered independently.
///
/// Both calls here are the same method on the same session, opened by the
/// caller holding `Read`, and they are answered differently: the second is
/// filtered against the credential it presented rather than the one that
/// opened the session. That is the whole of the lifetime claim as a caller
/// experiences it, and it is what fails if the peer's principal comes from
/// session state instead of from the request.
///
/// The pairing is what makes it a test rather than a demonstration — one
/// caller holding the scope and one holding nothing, against one session —
/// because a gate that answered both the same way passes a test that ever
/// only used one caller.
/// A streamed `tools/list` is filtered against the caller's own credential.
///
/// A sessioned `POST` is answered with an event stream, and its handler
/// runs *beside* that stream rather than inside it — so the handler is
/// given a peer rebuilt from the session. The principal is the
/// *request's*, resolved by the router from the credentials this one
/// presented, so a rebuild that takes the session's peer instead answers an
/// authenticated call as though nobody had authenticated, and the gated
/// primitive becomes invisible to the caller who earned it.
///
/// Asserted on the listing rather than the call on purpose. A hidden tool
/// answers a `tools/call` with the same `no such tool` as a tool that was
/// never registered, deliberately — the difference would let a client probe
/// for what it may not see. The listing is the only place the gate is
/// visible, which makes it the only place this can be caught.
#[tokio::test]
async fn a_streamed_listing_keeps_the_callers_principal() -> Result<(), Failure> {
    let router = auth_routes()?;
    let session = open_session(&router).await?;

    let (status, _, held) = post(
        &router,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
        Some("tok-read"),
        Some(&session),
    )
    .await?;
    if status != StatusCode::OK {
        return Err(Failure::Expectation("a 200 for tools/list"));
    }
    if !listed(&held)?.iter().any(|name| name == GATED) {
        return Err(Failure::Expectation(
            "the streamed listing to carry the gate's principal",
        ));
    }

    // The same request with no credential must not see it. If both answers
    // were the same, the principal was never consulted and the check above
    // would have passed for the wrong reason.
    let (_, _, anonymous) = post(
        &router,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/list","params":{}}"#,
        None,
        Some(&session),
    )
    .await?;
    if listed(&anonymous)?.iter().any(|name| name == GATED) {
        return Err(Failure::Expectation(
            "the streamed listing to hide the gate from an anonymous caller",
        ));
    }
    Ok(())
}

#[tokio::test]
async fn a_later_request_is_filtered_against_its_own_credential() -> Result<(), Failure> {
    let router = auth_routes()?;
    let session = open_session(&router).await?;

    let (status, _, held) = post(
        &router,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
        Some("tok-read"),
        Some(&session),
    )
    .await?;
    if status != StatusCode::OK {
        return Err(Failure::Expectation("a 200 for tools/list"));
    }
    let (status, _, unheld) = post(
        &router,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/list","params":{}}"#,
        Some("tok-anon"),
        Some(&session),
    )
    .await?;
    if status != StatusCode::OK {
        return Err(Failure::Expectation("a 200 for tools/list"));
    }

    if !listed(&held)?.iter().any(|name| name == GATED) {
        return Err(Failure::Expectation(
            "the Read-gated tool listed for a caller holding Read",
        ));
    }
    if listed(&unheld)?.iter().any(|name| name == GATED) {
        return Err(Failure::Expectation(
            "the Read-gated tool hidden from a caller holding nothing, on a session a caller who did hold it opened",
        ));
    }
    Ok(())
}
