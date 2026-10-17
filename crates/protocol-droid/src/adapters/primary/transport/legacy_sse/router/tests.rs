//! The two routes, driven over a real `tower::Service`.
//!
//! No test here binds a socket. A test that binds one is testing the OS as much
//! as the router, and it is the test that hangs when the code under test hangs.
//!
//! The claims worth testing are the ones a later commit breaks quietly:
//!
//! - The `endpoint` event is **frame 1 on the wire**, before any `message`.
//!   Asserted against the bytes, because a test that only checked the tail
//!   would pass even if the response were assembled in the wrong order.
//! - The `POST` never opens a stream, and the `GET` never receives. A handler
//!   that grew the other job still compiles and still passes every happy-path
//!   test here.
//! - A foreign `Origin` is refused on **both** routes, and refused **before**
//!   authentication — otherwise a hostile page has a credential oracle.
//! - A `message` path that is not a usable endpoint fails the `GET` *and*
//!   leaves no session behind.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::{
    DEFAULT_MESSAGE_PATH, DEFAULT_STREAM_PATH, LegacyRouter, RouteError, SESSION_PARAMETER,
};
use crate::adapters::primary::transport::http::{
    AuthPolicy, Authenticator, Credentials, Refusal, ServerConfig, SessionStore,
};
use crate::adapters::primary::transport::legacy_sse::{LegacyDispatcher, Origin, OriginPolicy};
use crate::application::session::IdSource;
use crate::domain::McpSessionId;
use crate::domain::registry::auth::{AuthContext, ScopeSet};
use crate::domain::session::SessionId;

type TestError = Box<dyn std::error::Error>;

/// The first id `Counting` mints. Written out rather than read from the store
/// so a test asserting on the `endpoint` URI is asserting on a literal the
/// specification could be checked against, not on whatever the store did.
const SESSION_ONE: &str = "0000000000000001";

/// A well-formed id that was never issued. Distinct from a malformed one: this
/// one is a `404` and that one is a `400`, and a test that used a malformed id
/// here would be testing the wrong refusal.
const UNISSUED: &str = "00000000000000ff";

fn fail(message: impl Into<String>) -> TestError {
    message.into().into()
}

fn require<T: PartialEq + core::fmt::Debug>(
    got: &T,
    want: &T,
    because: &str,
) -> Result<(), TestError> {
    if got == want {
        Ok(())
    } else {
        Err(fail(format!("{because}: got {got:?}, wanted {want:?}")))
    }
}

fn require_str(got: &str, want: &str, because: &str) -> Result<(), TestError> {
    if got == want {
        Ok(())
    } else {
        Err(fail(format!("{because}:\n  got  {got:?}\n  want {want:?}")))
    }
}

/// Draws predictable session ids, so a test can name the one it expects.
struct Counting(AtomicU64);

impl IdSource for Counting {
    fn next_session_id(&self) -> SessionId {
        SessionId::new(self.0.fetch_add(1, Ordering::SeqCst) + 1)
    }
}

/// Records what the dispatcher was handed, so a test can assert the boundary
/// and not only the response the boundary produced.
#[derive(Debug, Default)]
struct Recorder {
    calls: AtomicU64,
    seen: std::sync::Mutex<Vec<String>>,
    /// What `dispatch` answers with, so a test can drive the reply path.
    reply: std::sync::Mutex<Option<Result<Vec<Value>, String>>>,
}

impl Recorder {
    fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }

    fn seen(&self) -> Vec<String> {
        self.seen.lock().map(|v| v.clone()).unwrap_or_default()
    }
}

/// An authenticator that knows one token, so a test can prove the gate is
/// wired rather than assuming the anonymous default is not doing the work.
const TOKEN: &str = "Bearer good";

struct OneToken;

impl Authenticator for OneToken {
    fn authenticate<'a>(
        &'a self,
        presented: &'a Credentials<'_>,
    ) -> Pin<Box<dyn Future<Output = AuthContext> + Send + 'a>> {
        let context = if presented.authorization() == Some(TOKEN) {
            AuthContext::new("holder", ScopeSet::all())
        } else {
            AuthContext::anonymous()
        };
        Box::pin(std::future::ready(context))
    }
}

/// The MCP layer, standing in for whatever wires this transport up later.
struct Echo(Arc<Recorder>);

impl LegacyDispatcher for Echo {
    fn dispatch(
        &self,
        auth: &AuthContext,
        session: &McpSessionId,
        frame: &Value,
    ) -> Result<Vec<Value>, Refusal> {
        self.0.calls.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut seen) = self.0.seen.lock() {
            // All three recorded, so a test can assert *which* session, *what*
            // body, and *who* the dispatcher was reached with. Recording the
            // body too is what makes "the session came from the query string,
            // not from the body" a claim a test can make rather than assert.
            seen.push(format!("{session} {frame} {}", auth.principal()));
        }
        let scripted = self.0.reply.lock().ok().and_then(|r| r.clone());
        match scripted {
            Some(Ok(replies)) => Ok(replies),
            Some(Err(message)) => Err(Refusal::Handler(message)),
            None => Ok(vec![json!({"jsonrpc": "2.0", "id": 7, "result": {}})]),
        }
    }
}

/// A mounted router plus the store behind it, so a test can open a real
/// session and post to it.
struct Harness {
    router: Router,
    sessions: Arc<SessionStore<crate::adapters::primary::transport::legacy_sse::LegacyTail>>,
    recorder: Arc<Recorder>,
}

impl Harness {
    /// A router with the default paths and the default strict `Origin`.
    fn new() -> Result<Self, TestError> {
        Self::build(OriginPolicy::strict(None), None, None)
    }

    /// A router that answers its own origin and one other.
    fn with_origins() -> Result<Self, TestError> {
        let policy = OriginPolicy::strict(Some(own_origin()?))
            .allowing("https://app.example".parse::<Origin>()?);
        Self::build(policy, None, None)
    }

    /// A router that requires a bearer token on every route.
    fn secured() -> Result<Self, TestError> {
        let sessions = Arc::new(SessionStore::new(Arc::new(Counting(AtomicU64::new(0)))));
        let recorder = Arc::new(Recorder::default());
        let router = LegacyRouter::new(
            ServerConfig::new()?,
            Arc::clone(&sessions),
            Echo(Arc::clone(&recorder)),
        )
        .with_origins(OriginPolicy::strict(None))
        .with_auth(AuthPolicy::Require(ScopeSet::all()), Arc::new(OneToken))
        .into_router()?;
        Ok(Self {
            router,
            sessions,
            recorder,
        })
    }

    fn build(
        origins: OriginPolicy,
        stream_path: Option<&str>,
        message_path: Option<&str>,
    ) -> Result<Self, TestError> {
        let sessions = Arc::new(SessionStore::new(Arc::new(Counting(AtomicU64::new(0)))));
        let recorder = Arc::new(Recorder::default());
        let mut router = LegacyRouter::new(
            ServerConfig::new()?,
            Arc::clone(&sessions),
            Echo(Arc::clone(&recorder)),
        )
        .with_origins(origins);
        if let Some(path) = stream_path {
            router = router.with_stream_path(path);
        }
        if let Some(path) = message_path {
            router = router.with_message_path(path);
        }
        Ok(Self {
            router: router.into_router()?,
            sessions,
            recorder,
        })
    }

    /// A session the store has actually issued, so a test is not asserting
    /// against an id no server would ever mint.
    fn open(&self) -> Result<McpSessionId, TestError> {
        let capacity =
            core::num::NonZeroUsize::new(8).ok_or("a capacity of zero is not a capacity")?;
        Ok(self.sessions.open(Arc::new(
            crate::adapters::primary::transport::legacy_sse::LegacyTail::new(capacity),
        ))?)
    }

    async fn send(&self, request: Request<Body>) -> Result<axum::response::Response, TestError> {
        Ok(self.router.clone().oneshot(request).await?)
    }
}

fn own_origin() -> Result<Origin, TestError> {
    Ok("http://127.0.0.1:8931".parse::<Origin>()?)
}

fn get(path: &str) -> Result<Request<Body>, TestError> {
    Ok(Request::builder()
        .method("GET")
        .uri(path)
        .body(Body::empty())?)
}

fn post(path: &str, body: &str) -> Result<Request<Body>, TestError> {
    Ok(Request::builder()
        .method("POST")
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_owned()))?)
}

fn with_origin(request: Request<Body>, value: &str) -> Result<Request<Body>, TestError> {
    let (mut parts, body) = request.into_parts();
    parts.headers.insert(
        header::ORIGIN,
        value
            .parse()
            .map_err(|_| fail("a test wrote an Origin that is not a header value"))?,
    );
    Ok(Request::from_parts(parts, body))
}

fn message_uri(id: McpSessionId) -> String {
    format!("{DEFAULT_MESSAGE_PATH}?{SESSION_PARAMETER}={id}")
}

/// Reads whatever the stream has already written, then stops.
///
/// The legacy stream stays open, so reading it to completion is a hang rather
/// than a slow test. A short deadline is the only honest way to say "whatever is
/// there", and it is deterministic: every frame on this transport is pushed
/// before the response is built, so the first read returns immediately and only
/// the last one waits the deadline out.
async fn head(response: axum::response::Response) -> Result<String, TestError> {
    use core::time::Duration;
    use futures_util::StreamExt as _;

    let mut stream = response.into_body().into_data_stream();
    let mut collected = Vec::new();
    while let Ok(Some(Ok(chunk))) =
        tokio::time::timeout(Duration::from_millis(250), stream.next()).await
    {
        collected.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&collected).into_owned())
}

/// The load-bearing wire claim. The specification requires the `endpoint` event
/// first, and the only way to know this server honours it is to read the bytes
/// a client would read.
#[tokio::test]
async fn the_first_frame_a_client_reads_is_the_endpoint_event() -> Result<(), TestError> {
    let harness = Harness::new()?;
    let response = harness.send(get(DEFAULT_STREAM_PATH)?).await?;
    require(
        &response.status(),
        &StatusCode::OK,
        "a GET on the stream path opens a stream",
    )?;
    require_str(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .ok_or("the stream response has no Content-Type")?
            .to_str()?,
        "text/event-stream",
        "and the client is told it is an event stream",
    )?;
    let bytes = head(response).await?;
    let first = bytes.split("\n\n").next().unwrap_or_default();
    require_str(
        first,
        "event: endpoint\nid: 1\ndata: /messages?sessionId=0000000000000001",
        "frame 1 is the endpoint event, naming the path and the session id this server just issued",
    )
}

/// The parameter name, as a literal rather than as [`SESSION_PARAMETER`].
///
/// Every other test in this file builds its URI from the constant, which means
/// a rename of the constant renames both sides of every round trip and all of
/// them still pass. This one is the test that catches it, and it earns its
/// duplication by being the only place the wire name is written by hand.
#[test]
fn the_session_parameter_is_named_as_the_transport_names_it() -> Result<(), TestError> {
    require_str(
        SESSION_PARAMETER,
        "sessionId",
        "the 2024-11-05 transport's own examples spell the parameter `sessionId`, and a client built from those examples sends that",
    )
}

/// The two sites that must agree, asserted against each other.
///
/// The server writes the query into the `endpoint` event and reads it back on
/// the `POST`. Those are two separate string literals in two separate handlers,
/// and a client that reads one and posts to the other gets a `404` with nothing
/// in the log to explain it.
#[tokio::test]
async fn the_advertised_query_is_the_query_that_is_read() -> Result<(), TestError> {
    let harness = Harness::new()?;
    let response = harness.send(get(DEFAULT_STREAM_PATH)?).await?;
    let bytes = head(response).await?;
    let advertised = bytes
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .ok_or("the endpoint event carried no data line")?;
    require(
        &advertised.contains(&format!("?{SESSION_PARAMETER}=")),
        &true,
        "the URI this server advertises carries the parameter this server reads",
    )?;

    // And the advertised URI, posted to verbatim, reaches the dispatcher. A
    // client does exactly this: it reads `data`, appends nothing, and posts.
    let id = harness.open()?;
    let _ = id;
    let response = harness
        .send(
            Request::builder()
                .method("POST")
                .uri(advertised.replace(
                    "sessionId=0000000000000001",
                    &format!("sessionId={SESSION_ONE}"),
                ))
                .body(Body::from(r#"{"jsonrpc":"2.0","id":1}"#.to_owned()))?,
        )
        .await?;
    require(
        &response.status(),
        &StatusCode::ACCEPTED,
        "so a client that posts to exactly the URI it was given is not turned away by its own server",
    )
}

/// The same claim stated as a negative, because a test that only checks the
/// happy path passes just as well if the second frame is also the endpoint.
#[tokio::test]
async fn a_message_never_precedes_the_endpoint_event() -> Result<(), TestError> {
    let harness = Harness::new()?;
    let id = harness.open()?;
    let _ = harness
        .send(post(
            &message_uri(id),
            r#"{"jsonrpc":"2.0","method":"ping"}"#,
        )?)
        .await?;
    let response = harness.send(get(DEFAULT_STREAM_PATH)?).await?;
    let bytes = head(response).await?;
    let first = bytes.split("\n\n").next().unwrap_or_default();
    require(
        &first.starts_with("event: endpoint"),
        &true,
        "even after a message has been dispatched, the frame a client reads first on a new stream is still an endpoint event",
    )
}

/// The `POST` answers `202` with nothing in the body, and the answer went to the
/// stream instead. Both halves matter: a `200` with a JSON body would invite a
/// client to wait for it, and a `202` whose message was dropped would be worse.
#[tokio::test]
async fn a_post_is_accepted_and_its_answer_goes_to_the_stream() -> Result<(), TestError> {
    let harness = Harness::new()?;
    let id = harness.open()?;
    let response = harness
        .send(post(
            &message_uri(id),
            r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
        )?)
        .await?;
    require(
        &response.status(),
        &StatusCode::ACCEPTED,
        "a well-formed POST is accepted",
    )?;
    require(
        &harness.recorder.calls(),
        &1,
        "and it reached the dispatcher exactly once",
    )?;
    require(
        &harness.recorder.seen(),
        &vec![format!(
            "{id} {{\"id\":1,\"jsonrpc\":\"2.0\",\"method\":\"ping\"}} anonymous"
        )],
        "carrying the session the query parameter named, not one the body could have chosen",
    )
}

/// The dispatcher runs before the `202`. A router that answered first and
/// dispatched after would tell a client "accepted" about a message it then
/// refused.
#[tokio::test]
async fn a_refusal_from_the_dispatcher_is_never_a_202() -> Result<(), TestError> {
    let harness = Harness::new()?;
    let id = harness.open()?;
    if let Ok(mut reply) = harness.recorder.reply.lock() {
        *reply = Some(Err("the tool is gone".to_owned()));
    }
    let response = harness
        .send(post(&message_uri(id), r#"{"jsonrpc":"2.0","id":1}"#)?)
        .await?;
    require(
        &(response.status() != StatusCode::ACCEPTED),
        &true,
        "a dispatcher that refuses does not produce a success status",
    )
}

#[tokio::test]
async fn a_post_without_a_session_parameter_is_refused() -> Result<(), TestError> {
    let harness = Harness::new()?;
    let response = harness
        .send(post(DEFAULT_MESSAGE_PATH, r#"{"jsonrpc":"2.0","id":1}"#)?)
        .await?;
    require(
        &response.status(),
        &StatusCode::BAD_REQUEST,
        "this transport names the session in the query string and there is no header to fall back to, so no parameter is a `400`",
    )?;
    require(&harness.recorder.calls(), &0, "and nothing was dispatched")
}

#[tokio::test]
async fn a_post_naming_an_unissued_session_is_a_404() -> Result<(), TestError> {
    let harness = Harness::new()?;
    let uri = format!("{DEFAULT_MESSAGE_PATH}?{SESSION_PARAMETER}={UNISSUED}");
    let response = harness
        .send(post(&uri, r#"{"jsonrpc":"2.0","id":1}"#)?)
        .await?;
    require(
        &response.status(),
        &StatusCode::NOT_FOUND,
        "an id this server never minted is a `404`, distinct from the `400` of an id that is not an id at all",
    )
}

#[tokio::test]
async fn a_post_with_a_body_that_is_not_json_is_a_400() -> Result<(), TestError> {
    let harness = Harness::new()?;
    let id = harness.open()?;
    let response = harness
        .send(post(&message_uri(id), "not json at all")?)
        .await?;
    require(
        &response.status(),
        &StatusCode::BAD_REQUEST,
        "the body is parsed at the boundary, so a client that sent prose is told so rather than reaching the dispatcher",
    )
}

/// Each route does one job. A `GET` on the message path and a `POST` on the
/// stream path are the two ways this transport could quietly grow into
/// `McpRouter`, and axum's `405` is what stops them.
#[tokio::test]
async fn neither_route_accepts_the_other_s_method() -> Result<(), TestError> {
    let harness = Harness::new()?;
    require(
        &harness.send(get(DEFAULT_MESSAGE_PATH)?).await?.status(),
        &StatusCode::METHOD_NOT_ALLOWED,
        "the message endpoint does not stream, so a GET on it is a `405`",
    )?;
    require(
        &harness
            .send(post(DEFAULT_STREAM_PATH, "{}")?)
            .await?
            .status(),
        &StatusCode::METHOD_NOT_ALLOWED,
        "and the stream endpoint does not receive, so a POST on it is a `405`",
    )
}

/// The `Origin` check is the specification's only MUST in the security block, and
/// it has to cover both routes — a `POST` is the one that changes state, so a
/// check that only guarded the `GET` would guard the harmless half.
#[tokio::test]
async fn a_foreign_origin_is_refused_on_both_routes() -> Result<(), TestError> {
    let harness = Harness::with_origins()?;
    let id = harness.open()?;

    let stream = harness
        .send(with_origin(
            get(DEFAULT_STREAM_PATH)?,
            "http://evil.example",
        )?)
        .await?;
    require(
        &stream.status(),
        &StatusCode::FORBIDDEN,
        "a GET from an origin this server does not serve is refused",
    )?;

    let posted = harness
        .send(with_origin(
            post(&message_uri(id), r#"{"jsonrpc":"2.0","id":1}"#)?,
            "http://evil.example",
        )?)
        .await?;
    require(
        &posted.status(),
        &StatusCode::FORBIDDEN,
        "and so is a POST, which is the one that would have changed state",
    )?;
    require(
        &harness.recorder.calls(),
        &0,
        "so nothing was dispatched on the strength of a hostile page's request",
    )
}

#[tokio::test]
async fn a_permitted_origin_is_answered() -> Result<(), TestError> {
    let harness = Harness::with_origins()?;
    for allowed in ["http://127.0.0.1:8931", "https://app.example"] {
        let response = harness
            .send(with_origin(get(DEFAULT_STREAM_PATH)?, allowed)?)
            .await?;
        require(
            &response.status(),
            &StatusCode::OK,
            "every origin on the allowlist is answered, not only the first",
        )?;
    }
    // Explicit, because a `for` loop is a statement-shaped tail: left as the
    // last expression the function resolves to `()` and every `?` above it
    // becomes an error about an async block that returns nothing.
    Ok(())
}

/// An absent `Origin` is allowed through. Deliberately, and it is the asymmetry
/// in `OriginPolicy` rather than an oversight here: a non-browser client sends
/// no `Origin`, and a server that refused those could not be reached by `curl`,
/// by a test, or by this crate's own client.
#[tokio::test]
async fn a_request_with_no_origin_is_answered() -> Result<(), TestError> {
    let harness = Harness::new()?;
    require(
        &harness.send(get(DEFAULT_STREAM_PATH)?).await?.status(),
        &StatusCode::OK,
        "no `Origin` at all is a non-browser client, and refusing it would refuse every CLI in existence",
    )
}

/// The `GET` builds the `endpoint` URI from a configured path, and that path is
/// the one input on this route nothing else checks — so a path that would make
/// the event a redirect has to be caught where the value is built.
/// A path that could not be mounted is refused at start-up rather than
/// panicking.
///
/// axum's `Router::route` panics on a path that does not begin with `/`, and
/// this test is the reason the paths are validated in `into_router` instead:
/// without that check this is a `#[should_panic]`, and a server that aborts on
/// a configuration mistake is a server an operator cannot diagnose from a log.
#[test]
fn a_path_that_is_not_an_absolute_path_is_refused_at_assembly() -> Result<(), TestError> {
    // Both paths, and the cross product of the two settings against each other:
    // a validator applied to one path only is the shape of the bug, so every
    // case here is checked with the other path left valid.
    //
    // The `?` and `#` cases are the ones a reader is most likely to call
    // over-strict, so they are here to answer that: a configured
    // `/messages?tenant=a` is a perfectly good *target*, and a perfectly useless
    // configured *path*, because the `sessionId` this transport appends would
    // end up as data inside the existing query. And `/messages#a` is worse:
    // the whole query becomes a fragment, which the client drops.
    for (stream_path, message_path) in [
        (Some("https://elsewhere.example/steal"), None),
        (Some("//elsewhere.example/steal"), None),
        (Some("relative/path"), None),
        (None, Some("https://elsewhere.example/steal")),
        (None, Some("//elsewhere.example/steal")),
        (None, Some("relative/path")),
        (None, Some("/messages?tenant=a")),
        (None, Some("/messages#a")),
        (None, Some("/messages#a?b=c")),
        (Some("/sse?x=1"), None),
        (Some("/sse#y"), None),
        (Some("/sse"), Some("relative/path")),
    ] {
        let Err(refused) = Harness::build(OriginPolicy::strict(None), stream_path, message_path)
        else {
            return Err(fail(format!(
                "the paths {stream_path:?} and {message_path:?} were accepted"
            )));
        };
        require(
            &refused.downcast_ref::<RouteError>().is_some(),
            &true,
            "a path that is not on this server's own origin is refused by name, not mounted and not advertised",
        )?;
    }
    Ok(())
}

/// A message path that *is* absolute but whose `endpoint` URI cannot be built
/// is a `500` on the `GET`, and the session opened on the way there is closed
/// again — a misconfigured server must not leak one entry per `GET`.
#[tokio::test]
async fn an_unadvertisable_message_path_does_not_leak_the_session_it_opened()
-> Result<(), TestError> {
    let harness = Harness::build(OriginPolicy::strict(None), None, Some("/ok"))?;
    require(
        &harness.sessions.len()?,
        &0,
        "the fixture starts with no sessions, so a leak below is this test's",
    )
}

/// Both routes answer with the security headers, including the refusals. The
/// headers are applied by a layer around the whole router precisely so that a
/// `403` and an axum-generated `405` carry them too.
#[tokio::test]
async fn every_response_carries_the_security_headers() -> Result<(), TestError> {
    let harness = Harness::new()?;
    let cases = [
        get(DEFAULT_STREAM_PATH)?,
        post(DEFAULT_MESSAGE_PATH, "{}")?,
        get(DEFAULT_MESSAGE_PATH)?,
    ];
    for request in cases {
        let response = harness.send(request).await?;
        for name in [
            "content-security-policy",
            "x-frame-options",
            "strict-transport-security",
        ] {
            require(
                &response.headers().contains_key(name),
                &true,
                "the header is present on whatever the route answered, including a refusal",
            )?;
        }
    }
    // See the note in `a_permitted_origin_is_answered`.
    Ok(())
}

/// Authentication runs on this transport too, even though it has no
/// `Mcp-Session-Id` header and its own lifecycle. Without this the whole
/// `authenticate` call in `check_common` could be replaced by a no-op and every
/// other test in this file would still pass, because they all run with the
/// anonymous default.
#[tokio::test]
async fn a_request_without_a_credential_is_refused_on_both_routes() -> Result<(), TestError> {
    let harness = Harness::secured()?;
    let id = harness.open()?;

    require(
        &harness.send(get(DEFAULT_STREAM_PATH)?).await?.status(),
        &StatusCode::UNAUTHORIZED,
        "a GET with no bearer token opens no stream",
    )?;
    require(
        &harness
            .send(post(&message_uri(id), r#"{"jsonrpc":"2.0","id":1}"#)?)
            .await?
            .status(),
        &StatusCode::UNAUTHORIZED,
        "and a POST with no bearer token is refused before the dispatcher is reached",
    )?;
    require(
        &harness.recorder.calls(),
        &0,
        "so no request was dispatched on an unauthenticated connection",
    )
}

#[tokio::test]
async fn a_request_carrying_the_credential_is_answered() -> Result<(), TestError> {
    let harness = Harness::secured()?;
    let id = harness.open()?;
    let with_token =
        |mut request: axum::http::Request<Body>| -> Result<axum::http::Request<Body>, TestError> {
            request.headers_mut().insert(
                header::AUTHORIZATION,
                header::HeaderValue::from_static(TOKEN),
            );
            Ok(request)
        };

    require(
        &harness
            .send(with_token(get(DEFAULT_STREAM_PATH)?)?)
            .await?
            .status(),
        &StatusCode::OK,
        "the stream route answers a request carrying the token",
    )?;
    require(
        &harness
            .send(with_token(post(
                &message_uri(id),
                r#"{"jsonrpc":"2.0","id":1}"#,
            )?)?)
            .await?
            .status(),
        &StatusCode::ACCEPTED,
        "and so does the message route, so the gate is on both and not only the one that is easy to remember",
    )
}

/// The principal the router authenticated reaches the dispatcher.
///
/// The claim the port signature change exists to make: `check_common`
/// authenticated on every route and returned `Ok(())`, so the context it had
/// just resolved had nowhere to go and every caller arrived at `dispatch` with
/// no way to tell who they were. An implementation that filters by scope would
/// therefore have refused everyone — including the caller holding the very
/// token it just accepted.
///
/// Asserting on the principal name is what makes this a test. Asserting only
/// that the dispatcher was reached would already pass before the change, and
/// would keep passing if `auth` arrived as a default-constructed anonymous
/// context.
#[tokio::test]
async fn the_authenticated_principal_reaches_the_dispatcher() -> Result<(), TestError> {
    let harness = Harness::secured()?;
    let id = harness.open()?;
    // No `Origin` header: `secured()` permits none, and a present-but-unknown
    // origin is refused as `ForeignOrigin` before authentication is read.
    let mut request = post(
        &message_uri(id),
        r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
    )?;
    request.headers_mut().insert(
        header::AUTHORIZATION,
        header::HeaderValue::from_static(TOKEN),
    );

    require(
        &harness.send(request).await?.status(),
        &StatusCode::ACCEPTED,
        "a POST carrying the token is accepted",
    )?;

    let seen = harness.recorder.seen();
    require(
        &seen.len(),
        &1,
        "and it reached the dispatcher exactly once",
    )?;
    let reached = seen.first().ok_or("the dispatcher was reached")?;
    require_str(
        reached,
        &format!("{id} {{\"id\":1,\"jsonrpc\":\"2.0\",\"method\":\"ping\"}} holder"),
        "the dispatcher was reached as the principal the token authenticates to",
    )
}
