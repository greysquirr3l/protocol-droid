//! The router's own tests: the two structural claims its module docs make, and
//! the refusal table.
//!
//! The claims are worth testing rather than reading. "A refusal is never a
//! `200`" and "the auth gate runs before the store is touched" are the two
//! places a later commit breaks this file quietly, because a handler that
//! forgets one still compiles and still passes every test that drives the happy
//! path. Each therefore has a test named after it, and each is
//! mutation-checked.
//!
//! Every router is driven through `tower::ServiceExt::oneshot`, so no test
//! binds a socket. A test that binds one is testing the OS as much as the
//! router, and it is the test that hangs when the code under test hangs.

use core::num::{NonZeroU32, NonZeroUsize};
use core::time::Duration;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::{
    Inbound, McpRouter, RequestHandler, SessionHandle, Unsessioned, authenticate, resolve_session,
};
use crate::adapters::primary::transport::http::limits::RateLimiter;
use crate::adapters::primary::transport::http::{
    AuthPolicy, Authenticator, Credentials, EventTail, Refusal, SecurityHeaders, ServerConfig,
    SessionStore, Streaming,
};
use crate::adapters::primary::transport::origin::OriginPolicy;
use crate::application::session::IdSource;
use crate::domain::mcp::{ResourceMetadata, ResourceUri};
use crate::domain::registry::auth::{AuthContext, Scope, ScopeSet};
use crate::domain::session::SessionId;
use crate::domain::{EventId, McpSessionId, ProtocolVersion, RateLimit};

/// Every fallible call in this module funnels through here, so a failure prints
/// the engine's own account of what it refused.
type TestError = Box<dyn std::error::Error>;

/// Names a failed claim. Annotated rather than written as `Err(fail("…"))`
/// at each call site because `.into()` there is ambiguous — `&str` converts to
/// both `&str` and `Box<dyn Error>` — so the compiler settles the function's
/// error type on whichever it saw first and every `?` in the body then fails to
/// convert.
fn fail(message: impl Into<String>) -> TestError {
    message.into().into()
}

/// States a claim that has already been computed.
///
/// Takes the two sides by reference and renders them into the failure, so a
/// failing test says which value was wrong rather than only that something
/// was. The call site keeps the comparison visible — `require(seen == want, …)`
/// — so a test that stops comparing anything is obvious at a glance.
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

/// Draws predictable session ids so a test can name the one it expects.
struct Counting(AtomicU64);

impl IdSource for Counting {
    fn next_session_id(&self) -> SessionId {
        SessionId::new(self.0.fetch_add(1, Ordering::SeqCst) + 1)
    }
}

/// A session that is only ever a tail, so the store's `S` and the stream's
/// event source are the same type and the fixture has one moving part.
fn tail() -> Result<Arc<EventTail<Value>>, TestError> {
    let capacity = NonZeroUsize::new(8).ok_or("a capacity of zero is not a capacity")?;
    Ok(Arc::new(EventTail::new(capacity)))
}

/// Records what the router handed it, so a test can assert the boundary rather
/// than only the response the boundary produced.
#[derive(Debug, Default)]
struct Recorder {
    dispatches: AtomicU64,
    /// The session each call resolved to, as a nameable string.
    seen: std::sync::Mutex<Vec<String>>,
    /// The `MCP-Protocol-Version` each call was handed, already parsed.
    protocols: std::sync::Mutex<Vec<Option<String>>>,
}

impl Recorder {
    fn seen(&self) -> Vec<String> {
        self.seen
            .lock()
            .map(|seen| seen.clone())
            .unwrap_or_default()
    }

    /// The parsed version header of each dispatch, in order.
    fn protocols(&self) -> Vec<Option<String>> {
        self.protocols
            .lock()
            .map(|versions| versions.clone())
            .unwrap_or_default()
    }

    fn dispatches(&self) -> u64 {
        self.dispatches.load(Ordering::SeqCst)
    }
}

/// The protocol layer, standing in for whatever T18 will wire up.
struct Echo {
    recorder: Arc<Recorder>,
    establish: bool,
    /// What an unsessioned body means when the handler is not told otherwise.
    stateless: bool,
}

impl RequestHandler<EventTail<Value>> for Echo {
    fn unsessioned(&self, _body: &[u8]) -> Unsessioned {
        if self.establish {
            Unsessioned::Establishing
        } else if self.stateless {
            Unsessioned::Stateless
        } else {
            Unsessioned::NeedsSession
        }
    }

    fn dispatch(
        &self,
        call: Inbound<'_, EventTail<Value>>,
    ) -> impl core::future::Future<Output = Result<axum::response::Response, Refusal>> + Send {
        let recorder = Arc::clone(&self.recorder);
        let name = match &call.session {
            SessionHandle::Establishing => "establishing".to_owned(),
            SessionHandle::Stateless => "stateless".to_owned(),
            SessionHandle::Live { id, .. } => id.to_string(),
        };
        let protocol = call.protocol.map(|version| version.to_string());
        async move {
            recorder.dispatches.fetch_add(1, Ordering::SeqCst);
            if let Ok(mut seen) = recorder.seen.lock() {
                seen.push(name);
            }
            if let Ok(mut versions) = recorder.protocols.lock() {
                versions.push(protocol);
            }
            Ok(json_response(StatusCode::OK, &json!({ "ok": true })))
        }
    }
}

/// Grants one scope to a caller presenting the expected bearer token and
/// nothing at all to anyone else. Deliberately a different shape from
/// `Anonymous`, so a test that means to exercise the gate cannot pass against
/// the default by accident.
struct OneToken;

impl Authenticator for OneToken {
    fn authenticate<'a>(
        &'a self,
        presented: &'a Credentials<'_>,
    ) -> Pin<Box<dyn Future<Output = AuthContext> + Send + 'a>> {
        let context = if presented.authorization() == Some("Bearer good") {
            AuthContext::new("holder", ScopeSet::all())
        } else {
            AuthContext::anonymous()
        };
        Box::pin(std::future::ready(context))
    }
}

fn json_response(status: StatusCode, value: &Value) -> axum::response::Response {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    let mut response = axum::response::Response::new(Body::from(body));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    response
}

/// A mounted router plus the pieces a test needs to poke at it.
struct Harness {
    router: Router,
    sessions: Arc<SessionStore<EventTail<Value>>>,
    recorder: Arc<Recorder>,
}

impl Harness {
    /// The default for a test that does not care about auth: the
    /// `AuthPolicy::Public` policy with no authenticator wired.
    ///
    /// Note that after issue #11 this is the **only** `Public` combination
    /// the production code lets through `into_router`: `with_authenticator`
    /// paired with `Public` now fails closed. A test that wants to exercise
    /// a real authenticator passes it through
    /// [`Harness::with`] alongside a `Require` policy.
    fn build(establish: bool) -> Result<Self, TestError> {
        Self::with(establish, AuthPolicy::Public, None)
    }

    fn with(
        establish: bool,
        policy: AuthPolicy,
        authenticator: Option<Arc<dyn Authenticator + Send + Sync>>,
    ) -> Result<Self, TestError> {
        let sessions = Arc::new(SessionStore::new(Arc::new(Counting(AtomicU64::new(0)))));
        Self::from_parts(
            ServerConfig::new()?,
            sessions,
            establish,
            policy,
            authenticator,
        )
    }

    fn from_parts(
        config: ServerConfig,
        sessions: Arc<SessionStore<EventTail<Value>>>,
        establish: bool,
        policy: AuthPolicy,
        authenticator: Option<Arc<dyn Authenticator + Send + Sync>>,
    ) -> Result<Self, TestError> {
        Self::from_shape(config, sessions, establish, false, policy, authenticator)
    }

    /// [`Self::from_parts`] with the handler also answering `Unsessioned` as
    /// stateless, which is what the **2026-07-28** lifecycle looks like from
    /// the router's side.
    fn stateless(
        config: ServerConfig,
        sessions: Arc<SessionStore<EventTail<Value>>>,
    ) -> Result<Self, TestError> {
        Self::from_shape(config, sessions, false, true, AuthPolicy::default(), None)
    }

    fn from_shape(
        config: ServerConfig,
        sessions: Arc<SessionStore<EventTail<Value>>>,
        establish: bool,
        stateless: bool,
        policy: AuthPolicy,
        authenticator: Option<Arc<dyn Authenticator + Send + Sync>>,
    ) -> Result<Self, TestError> {
        let recorder = Arc::new(Recorder::default());
        let mut router = McpRouter::new(
            config,
            Arc::clone(&sessions),
            Echo {
                recorder: Arc::clone(&recorder),
                establish,
                stateless,
            },
        )
        .with_policy(policy);
        if let Some(authenticator) = authenticator {
            router = router.with_authenticator(authenticator);
        }
        let router = router.into_router()?;
        Ok(Self {
            router,
            sessions,
            recorder,
        })
    }

    /// A session the store has actually issued, so a test is not asserting
    /// against an id no server would ever mint.
    fn open(&self) -> Result<McpSessionId, TestError> {
        Ok(self.sessions.open(tail()?)?)
    }

    async fn send(&self, request: Request<Body>) -> Result<axum::response::Response, TestError> {
        Ok(self.router.clone().oneshot(request).await?)
    }
}

/// The `Host` every helper below sends.
///
/// A request built from a bare `/mcp` URI carries no `Host`, which HTTP/1.1
/// requires and HTTP/2 carries as `:authority` — so a helper that omits it is
/// not describing a request a client could make, and the rebinding check has
/// nothing to read. Loopback, so it answers every test that is not about the
/// `Host` header itself.
const HOST: &str = "localhost:3000";

fn request(method: &str, uri: &str) -> Result<Request<Body>, TestError> {
    Ok(Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, HOST)
        .body(Body::empty())?)
}

fn post(body: &str) -> Result<Request<Body>, TestError> {
    Ok(Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::HOST, HOST)
        .body(Body::from(body.to_owned()))?)
}

fn get() -> Result<Request<Body>, TestError> {
    request("GET", "/mcp")
}

fn delete() -> Result<Request<Body>, TestError> {
    request("DELETE", "/mcp")
}

fn with_session(mut request: Request<Body>, id: McpSessionId) -> Result<Request<Body>, TestError> {
    let value = header::HeaderValue::from_str(&id.to_string())?;
    request.headers_mut().insert("mcp-session-id", value);
    Ok(request)
}

fn headers_with(id: McpSessionId) -> Result<HeaderMap, TestError> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "mcp-session-id",
        header::HeaderValue::from_str(&id.to_string())?,
    );
    Ok(headers)
}

fn wrong_token() -> Result<Request<Body>, TestError> {
    Ok(Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, "Bearer wrong")
        .header(header::HOST, HOST)
        .body(Body::from("{}"))?)
}

fn right_token() -> Result<Request<Body>, TestError> {
    Ok(Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, "Bearer good")
        .header(header::HOST, HOST)
        .body(Body::from("{}"))?)
}

// -------------------------------------------------------------------------
// A refusal is never a 200 — the claim every handler depends on.
// -------------------------------------------------------------------------

/// The whole refusal table, walked. If any arm can reach a success status this
/// fails, and it is the only test covering arms the others never construct.
#[test]
fn no_refusal_is_ever_a_success_status() {
    let every = [
        Refusal::MalformedSessionId,
        Refusal::MissingSession,
        Refusal::UnknownSession,
        Refusal::MethodNotAllowed {
            method: "PUT",
            reason: "not part of the streamable surface",
        },
        Refusal::ProtocolHeader {
            reason: "the header disagreed with the session".to_owned(),
        },
        Refusal::BodyTooLarge,
        Refusal::Unauthenticated,
        Refusal::Forbidden,
        Refusal::MalformedBody {
            reason: "not json".to_owned(),
        },
        Refusal::StreamGap {
            requested: EventId::new(0),
            evicted_through: EventId::new(1),
            last: EventId::new(4),
        },
        Refusal::UnknownCursor {
            requested: EventId::new(9),
            last: EventId::new(4),
        },
        Refusal::MalformedCursor,
        Refusal::Handler("boom".to_owned()),
    ];
    for refusal in &every {
        assert!(
            !refusal.status().is_success(),
            "{refusal:?} is answered with a success status"
        );
    }
}

// -------------------------------------------------------------------------
// The auth gate runs before the store is touched.
// -------------------------------------------------------------------------

/// A required policy with no credential is refused, and the store is never
/// consulted. The assertion that matters is the dispatch count: a refusal that
/// still performed the lookup is a timing oracle for which session ids exist,
/// and the status alone would not show it.
#[tokio::test]
async fn auth_is_refused_before_the_session_store_is_touched() -> Result<(), TestError> {
    let harness = Harness::with(
        true,
        AuthPolicy::Require(ScopeSet::all()),
        Some(Arc::new(OneToken)),
    )?;
    let id = harness.open()?;
    let response = harness.send(with_session(post("{}")?, id)?).await?;

    if response.status() != StatusCode::UNAUTHORIZED {
        return Err(fail("an unauthenticated caller was not refused"));
    }
    if harness.recorder.dispatches() != 0 {
        return Err(fail("the handler ran for an unauthenticated caller"));
    }
    Ok(())
}

/// The gate is not a blanket denial: the same policy admits a caller that
/// presents the token. Without this, an `authenticate` broken to refuse
/// everyone would pass every auth assertion above.
#[tokio::test]
async fn a_credential_the_policy_accepts_reaches_the_handler() -> Result<(), TestError> {
    let harness = Harness::with(
        true,
        AuthPolicy::Require(ScopeSet::all()),
        Some(Arc::new(OneToken)),
    )?;
    let id = harness.open()?;
    let request = with_session(right_token()?, id)?;
    let response = harness.send(request).await?;

    if response.status() != StatusCode::OK {
        return Err(fail("a valid credential was refused"));
    }
    if harness.recorder.dispatches() != 1 {
        return Err(fail("the handler did not run for an accepted credential"));
    }
    Ok(())
}

/// A credential that is present but wrong is refused the same way as none at
/// all. The two are the same answer on purpose: telling them apart tells an
/// attacker which half of the guess was right.
#[tokio::test]
async fn a_wrong_credential_is_refused_exactly_as_a_missing_one() -> Result<(), TestError> {
    let harness = Harness::with(
        true,
        AuthPolicy::Require(ScopeSet::all()),
        Some(Arc::new(OneToken)),
    )?;
    let id = harness.open()?;

    let missing = harness.send(with_session(post("{}")?, id)?).await?;
    let wrong = harness.send(with_session(wrong_token()?, id)?).await?;

    if missing.status() != wrong.status() {
        return Err(fail(
            "a wrong credential is answered differently from a missing one",
        ));
    }
    if harness.recorder.dispatches() != 0 {
        return Err(fail("the handler ran for a refused credential"));
    }
    Ok(())
}

// -------------------------------------------------------------------------
// Issue #11: an authenticator paired with the default `Public` policy fails
// open silently. `into_router` is the only point at which the misconfiguration
// can be turned into a startup error, and it must be loud.
// -------------------------------------------------------------------------

/// Wiring a real authenticator but leaving the policy at the default
/// `AuthPolicy::Public` is the exact fail-open the issue #11 review
/// described: the authenticator is consulted, a valid token produces a
/// scoped context, but `refuse` short-circuits on an empty `required()` and
/// the endpoint is served to anonymous callers. The fix is to fail closed
/// at `into_router` time, not to ship an endpoint that quietly does not
/// have the permission boundary its configuration claims.
#[test]
fn an_explicit_authenticator_with_the_default_public_policy_is_refused() -> Result<(), TestError> {
    let sessions = Arc::new(SessionStore::new(Arc::new(Counting(AtomicU64::new(0)))));
    let error = McpRouter::new(ServerConfig::new()?, sessions, dummy_echo())
        .with_authenticator(Arc::new(OneToken))
        .into_router();
    match error {
        Err(super::RouterError::UnprotectedAuthenticator) => Ok(()),
        Err(other) => Err(fail(format!(
            "expected UnprotectedAuthenticator, got {other:?}"
        ))),
        Ok(_) => Err(fail("the unsafe combination was accepted")),
    }
}

/// The safe combination — authenticator wired **and** a `Require` policy —
/// passes. The previous test pins the closed-by-default behaviour; this
/// test pins that the closed gate is not a blanket denial, so a future
/// commit that "simplifies" the check by also refusing `Require` is
/// caught on the way through.
#[test]
fn an_explicit_authenticator_with_a_require_policy_mounts() -> Result<(), TestError> {
    let sessions = Arc::new(SessionStore::new(Arc::new(Counting(AtomicU64::new(0)))));
    let _router = McpRouter::new(ServerConfig::new()?, sessions, dummy_echo())
        .with_authenticator(Arc::new(OneToken))
        .with_policy(AuthPolicy::Require(ScopeSet::all()))
        .into_router()?;
    Ok(())
}

/// The default `McpRouter::new` — no `with_authenticator` call — still
/// mounts under the default `Public` policy. This is the path every
/// `HttpBuilder` user takes today, and changing it would be a silent
/// breaking change for every consumer that has not yet wired auth.
#[test]
fn the_default_router_mounts_under_the_default_public_policy() -> Result<(), TestError> {
    let sessions = Arc::new(SessionStore::new(Arc::new(Counting(AtomicU64::new(0)))));
    let _router = McpRouter::new(ServerConfig::new()?, sessions, dummy_echo()).into_router()?;
    Ok(())
}

/// A no-op `Echo` for the issue #11 tests. Reuses the existing `Echo`
/// shape — the tests are not exercising the handler, only the
/// `into_router` mount path — and shares its `Recorder` (which stays
/// empty because no request ever reaches the handler).
fn dummy_echo() -> Echo {
    Echo {
        recorder: Arc::new(Recorder::default()),
        establish: false,
        stateless: true,
    }
}

// -------------------------------------------------------------------------
// Security headers on every response, refusals included.
// -------------------------------------------------------------------------

/// A refusal that drops the security headers is a refusal a client can cache
/// and a browser will render outside the policy. This is the mutation "the
/// middleware is on the router" exists to prevent.
#[tokio::test]
async fn a_refusal_still_carries_the_security_headers() -> Result<(), TestError> {
    let harness = Harness::build(true)?;
    // A GET with no session header is refused `400` before any handler runs.
    // A refusal is exactly the response a middleware that only decorates
    // successes would leave bare, so this is the case worth pinning.
    let response = harness.send(get()?).await?;
    if response.status().is_success() {
        return Err(fail("a request that should have been refused succeeded"));
    }

    for name in [
        "content-security-policy",
        "strict-transport-security",
        "x-frame-options",
    ] {
        if !response.headers().contains_key(name) {
            return Err(fail(format!("a refusal was answered without {name}")));
        }
    }
    Ok(())
}

/// The headers reach responses **no handler wrote** too: an unmapped method
/// and an unmapped path both bypass every route, so a per-route header is not
/// enough.
#[tokio::test]
async fn the_security_headers_reach_responses_no_handler_wrote() -> Result<(), TestError> {
    let harness = Harness::build(true)?;

    let unmapped_method = harness.send(request("PUT", "/mcp")?).await?;
    if !unmapped_method
        .headers()
        .contains_key("content-security-policy")
    {
        return Err(fail("a 405 was answered without the security headers"));
    }

    let unmapped_path = harness.send(request("POST", "/not-mcp")?).await?;
    if !unmapped_path
        .headers()
        .contains_key("content-security-policy")
    {
        return Err(fail("a 404 was answered without the security headers"));
    }
    Ok(())
}

/// The header **values** are the configured ones, not merely present.
#[tokio::test]
async fn the_security_header_values_are_the_configured_ones() -> Result<(), TestError> {
    let config =
        ServerConfig::new()?.with_security(SecurityHeaders::default().with_x_frame_options("DENY"));
    let sessions = Arc::new(SessionStore::new(Arc::new(Counting(AtomicU64::new(0)))));
    let harness = Harness::from_parts(config, sessions, true, AuthPolicy::default(), None)?;

    let response = harness.send(post("{}")?).await?;
    let frame = response
        .headers()
        .get("x-frame-options")
        .and_then(|value| value.to_str().ok())
        .ok_or("x-frame-options was absent")?;
    if frame != "DENY" {
        return Err(fail(format!("x-frame-options was {frame:?}, not DENY")));
    }
    Ok(())
}

// -------------------------------------------------------------------------
// The session header is validated, never minted on the spot.
// -------------------------------------------------------------------------

/// A well-formed id the store never issued is a 404. The load-bearing half is
/// the dispatch count: the router must not quietly open a session for it,
/// because a client holding a stale id would silently get a fresh one and
/// believe the old session is still there.
#[tokio::test]
async fn a_session_id_the_store_never_issued_is_a_404_and_opens_nothing() -> Result<(), TestError> {
    let harness = Harness::build(false)?;
    let forged = McpSessionId::from(SessionId::new(9_999));
    let response = harness.send(with_session(post("{}")?, forged)?).await?;

    if response.status() != StatusCode::NOT_FOUND {
        return Err(fail("an unissued session id was not refused with 404"));
    }
    if harness.recorder.dispatches() != 0 {
        return Err(fail("a forged session id reached the handler"));
    }
    Ok(())
}

/// A header that is not a session id at all is a 400, not a 404: the two are
/// different faults and the client fixes them differently.
#[tokio::test]
async fn a_malformed_session_id_is_a_400() -> Result<(), TestError> {
    let harness = Harness::build(false)?;
    let request = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("mcp-session-id", "not a session id")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::HOST, HOST)
        .body(Body::from("{}"))?;

    if harness.send(request).await?.status() != StatusCode::BAD_REQUEST {
        return Err(fail("a malformed session id was not refused with 400"));
    }
    Ok(())
}

/// A body that needs a session and did not name one is a 400, and the handler
/// never ran.
#[tokio::test]
async fn a_missing_session_id_on_a_stateful_request_is_a_400() -> Result<(), TestError> {
    let harness = Harness::build(false)?;
    let response = harness.send(post("{}")?).await?;

    if response.status() != StatusCode::BAD_REQUEST {
        return Err(fail(
            "a stateful request with no session id was not refused",
        ));
    }
    if harness.recorder.dispatches() != 0 {
        return Err(fail("a request with no session reached the handler"));
    }
    Ok(())
}

/// A body that *does* establish a session reaches the handler as
/// `Establishing` rather than being refused for a missing header.
#[tokio::test]
async fn a_body_that_opens_a_session_needs_no_session_header() -> Result<(), TestError> {
    let harness = Harness::build(true)?;
    let response = harness.send(post("{}")?).await?;

    if response.status() != StatusCode::OK {
        return Err(fail("a session-establishing body was refused"));
    }
    if harness.recorder.seen() != vec!["establishing".to_owned()] {
        return Err(fail(format!(
            "the handler saw {:?}",
            harness.recorder.seen()
        )));
    }
    Ok(())
}

/// The id the handler is given is the id the store issued.
#[tokio::test]
async fn the_handler_is_given_the_id_the_store_issued() -> Result<(), TestError> {
    let harness = Harness::build(false)?;
    let id = harness.open()?;
    let expected = id.to_string();
    let response = harness.send(with_session(post("{}")?, id)?).await?;

    if response.status() != StatusCode::OK {
        return Err(fail("a request naming a real session was refused"));
    }
    if harness.recorder.seen() != vec![expected] {
        return Err(fail(format!(
            "the handler saw {:?}",
            harness.recorder.seen()
        )));
    }
    Ok(())
}

// -------------------------------------------------------------------------
// The three routes.
// -------------------------------------------------------------------------

/// `POST` on a live session reaches the handler.
#[tokio::test]
async fn a_post_on_a_live_session_reaches_the_handler() -> Result<(), TestError> {
    let harness = Harness::build(false)?;
    let id = harness.open()?;
    let response = harness.send(with_session(post("{\"a\":1}")?, id)?).await?;

    if response.status() != StatusCode::OK {
        return Err(fail("a POST on a live session did not succeed"));
    }
    Ok(())
}

/// `GET` opens a stream on a live session: `text/event-stream`, no-cache, and
/// the agent-buffering header. A response without `X-Accel-Buffering: no` is a
/// stream nginx will hold, which is a stream that never delivers.
#[tokio::test]
async fn a_get_opens_an_event_stream() -> Result<(), TestError> {
    let harness = Harness::build(false)?;
    let id = harness.open()?;
    let response = harness.send(with_session(get()?, id)?).await?;

    if response.status() != StatusCode::OK {
        return Err(fail("GET did not open a stream"));
    }
    let headers = response.headers();
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or("no content type")?;
    if !content_type.starts_with("text/event-stream") {
        return Err(fail(format!("content type was {content_type:?}")));
    }
    for name in ["cache-control", "x-accel-buffering"] {
        if !headers.contains_key(name) {
            return Err(fail(format!("a stream was opened without {name}")));
        }
    }
    Ok(())
}

/// `GET` on a session the store does not hold is a 404, not an empty stream.
#[tokio::test]
async fn a_get_on_an_unknown_session_is_a_404() -> Result<(), TestError> {
    let harness = Harness::build(false)?;
    let forged = McpSessionId::from(SessionId::new(9_999));
    let response = harness.send(with_session(get()?, forged)?).await?;

    if response.status() != StatusCode::NOT_FOUND {
        return Err(fail("GET on an unknown session was not refused"));
    }
    Ok(())
}

/// `GET` with no session header is a 400 — there is no body to ask whether the
/// request opens one, so the header is mandatory on this route.
#[tokio::test]
async fn a_get_with_no_session_header_is_a_400() -> Result<(), TestError> {
    let harness = Harness::build(false)?;
    if harness.send(get()?).await?.status() != StatusCode::BAD_REQUEST {
        return Err(fail("GET with no session id was not refused"));
    }
    Ok(())
}

/// `GET` is gated by auth too. A stream that opened before the policy was
/// consulted would push server messages to an unauthenticated caller for as
/// long as it stays open.
#[tokio::test]
async fn a_get_is_refused_without_a_credential() -> Result<(), TestError> {
    let harness = Harness::with(
        false,
        AuthPolicy::Require(ScopeSet::all()),
        Some(Arc::new(OneToken)),
    )?;
    let id = harness.open()?;
    let response = harness.send(with_session(get()?, id)?).await?;

    if response.status() != StatusCode::UNAUTHORIZED {
        return Err(fail("GET opened a stream for an unauthenticated caller"));
    }
    Ok(())
}

/// `DELETE` terminates: after it the id is gone, and a second `DELETE` is a
/// 404. The store is read directly rather than trusting the status, because
/// "the session survived" is the claim and the status is a proxy for it.
#[tokio::test]
async fn a_delete_terminates_the_session() -> Result<(), TestError> {
    let harness = Harness::build(false)?;
    let id = harness.open()?;

    let first = harness.send(with_session(delete()?, id)?).await?;
    if first.status().is_server_error() {
        return Err(fail("DELETE failed"));
    }
    if harness.sessions.lookup(&id)?.is_some() {
        return Err(fail("the session survived a DELETE"));
    }
    let second = harness.send(with_session(delete()?, id)?).await?;
    if second.status() != StatusCode::NOT_FOUND {
        return Err(fail("a repeated DELETE did not become a 404"));
    }
    Ok(())
}

/// A `DELETE` naming an id the store never issued is a 404, and the attempt
/// creates nothing.
#[tokio::test]
async fn a_delete_on_an_unknown_session_is_a_404() -> Result<(), TestError> {
    let harness = Harness::build(false)?;
    let forged = McpSessionId::from(SessionId::new(9_999));
    if harness
        .send(with_session(delete()?, forged)?)
        .await?
        .status()
        != StatusCode::NOT_FOUND
    {
        return Err(fail("DELETE on an unknown session was not refused"));
    }
    Ok(())
}

/// A `GET` quoting `Last-Event-ID` replays only what follows it.
///
/// This is the router's half of the resume contract. `resume.rs` proves the
/// decision is right and `stream.rs` proves the decision is delivered, and
/// both can pass with a router that never forwards the header — in which case
/// a reconnecting client is handed the entire history again on every
/// reconnection, silently, with a `200` and a well-formed stream. Nothing
/// above the transport can see it.
///
/// The frames are read out of a body that then parks forever, so the read stops
/// on a deadline rather than at end-of-stream. Every event is pushed before the
/// request, so what arrives is the replay and not a live frame.
#[tokio::test]
async fn a_get_forwards_last_event_id_into_the_replay() -> Result<(), TestError> {
    let sessions = Arc::new(SessionStore::new(Arc::new(Counting(AtomicU64::new(0)))));
    let tail = tail()?;
    for n in 1..=3 {
        tail.push(json!({ "n": n }));
    }
    let id = sessions.open(Arc::clone(&tail))?;
    let harness = Harness::from_parts(
        ServerConfig::new()?,
        sessions,
        false,
        AuthPolicy::default(),
        None,
    )?;

    let mut quoted = with_session(get()?, id)?;
    quoted
        .headers_mut()
        .insert("last-event-id", header::HeaderValue::from_static("1"));
    let body = replayed_body(harness.send(quoted).await?).await?;
    let text = String::from_utf8_lossy(&body);

    if !text.contains("\"n\":2") || !text.contains("\"n\":3") {
        return Err(fail(format!(
            "a resume from 1 did not replay the events after it: {text}"
        )));
    }
    if text.contains("\"n\":1") {
        return Err(fail(format!(
            "a resume from 1 replayed event 1, which the client already has: {text}"
        )));
    }
    Ok(())
}

/// The same request with no cursor replays everything — the other half, because
/// a router that forwarded the header as an empty string rather than as `None`
/// would pass the test above while dropping the history for a fresh client.
#[tokio::test]
async fn a_get_with_no_cursor_replays_everything() -> Result<(), TestError> {
    let sessions = Arc::new(SessionStore::new(Arc::new(Counting(AtomicU64::new(0)))));
    let tail = tail()?;
    for n in 1..=3 {
        tail.push(json!({ "n": n }));
    }
    let id = sessions.open(Arc::clone(&tail))?;
    let harness = Harness::from_parts(
        ServerConfig::new()?,
        sessions,
        false,
        AuthPolicy::default(),
        None,
    )?;

    let body = replayed_body(harness.send(with_session(get()?, id)?).await?).await?;
    let text = String::from_utf8_lossy(&body);
    for n in 1..=3 {
        if !text.contains(&format!("\"n\":{n}")) {
            return Err(fail(format!("event {n} was not replayed: {text}")));
        }
    }
    Ok(())
}

/// A cursor that is not readable as a field-value is a `400`, not a stream from
/// the top. Reading it as "absent" hands the client a stream it believes it has
/// already seen, which is the one outcome worse than refusing.
#[tokio::test]
async fn an_unreadable_last_event_id_is_refused_rather_than_treated_as_absent()
-> Result<(), TestError> {
    let harness = Harness::build(false)?;
    let id = harness.open()?;
    // `0xff` is obs-text: the header type accepts it, `to_str` refuses it. That
    // is the only way to reach the unreadable arm at all — every value the type
    // will hold as visible ASCII round-trips through `to_str` cleanly.
    let mut request = with_session(get()?, id)?;
    request
        .headers_mut()
        .insert("last-event-id", header::HeaderValue::from_bytes(&[0xff])?);

    if harness.send(request).await?.status() != StatusCode::BAD_REQUEST {
        return Err(fail("an unreadable Last-Event-ID was not refused"));
    }
    Ok(())
}

/// The gate is asked before the store is, and the status code is the proof.
///
/// Nothing can observe which ran first except the answer. An unknown session
/// id consulted before authentication answers `404`; the same request with the
/// gate first answers `401`. A `404` here hands an unauthenticated caller a
/// probe: try ids until one is not refused as missing, and you have found a
/// live session you were never authorised to know about.
#[tokio::test]
async fn an_unknown_session_is_a_401_and_not_a_404_without_a_credential() -> Result<(), TestError> {
    let harness = Harness::with(
        true,
        AuthPolicy::Require(ScopeSet::all()),
        Some(Arc::new(OneToken)),
    )?;
    let forged = McpSessionId::from(SessionId::new(9_999));
    // No credential: the gate must be the only thing that answers.
    let response = harness.send(with_session(post("{}")?, forged)?).await?;

    if response.status() != StatusCode::UNAUTHORIZED {
        return Err(fail(
            "the store was consulted before the gate: an unauthenticated caller \
             was told the difference between a live session and a dead one",
        ));
    }
    if harness.recorder.dispatches() != 0 {
        return Err(fail("the handler ran for an unauthenticated caller"));
    }
    Ok(())
}

/// Reads whatever the stream has already written, then stops.
///
/// The body of an open SSE response never ends, so reading it to completion is
/// a hang rather than a slow test. A short deadline is the only honest way to
/// say "whatever is there", and it is deterministic because every event was
/// pushed before the request — the first read returns immediately and only the
/// final one waits out the deadline.
async fn replayed_body(response: axum::response::Response) -> Result<Vec<u8>, TestError> {
    use core::time::Duration;
    use futures_util::StreamExt as _;

    let mut stream = response.into_body().into_data_stream();
    let mut collected = Vec::new();
    while let Ok(Some(Ok(chunk))) =
        tokio::time::timeout(Duration::from_millis(250), stream.next()).await
    {
        collected.extend_from_slice(&chunk);
    }
    Ok(collected)
}

///
/// Worth pinning in both directions. A future change that made `Json` mode
/// answer a `GET` with a document would leave a client with no way to receive
/// server-initiated messages at all, and it would read as a faithful
/// application of the mode rather than the removal of a capability.
#[tokio::test]
async fn the_streaming_mode_never_changes_the_get_route() -> Result<(), TestError> {
    for mode in [Streaming::Json, Streaming::EventStream] {
        let config = ServerConfig::new()?.with_streaming(mode);
        let sessions = Arc::new(SessionStore::new(Arc::new(Counting(AtomicU64::new(0)))));
        let harness = Harness::from_parts(config, sessions, false, AuthPolicy::default(), None)?;
        let id = harness.open()?;
        let response = harness.send(with_session(get()?, id)?).await?;

        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .ok_or("no content type")?;
        if !content_type.starts_with("text/event-stream") {
            return Err(fail(format!(
                "GET in {mode:?} mode answered {content_type:?} rather than a stream"
            )));
        }
    }
    Ok(())
}

// -------------------------------------------------------------------------
// The body is bounded while it is read.
// -------------------------------------------------------------------------

/// An over-limit body is `BodyTooLarge`, not a framework 413 with an empty
/// body. A client that gets a 413 with no JSON-RPC error cannot tell which of
/// its two faults it was, and cannot log a request id that does not exist.
#[tokio::test]
async fn an_over_limit_body_is_a_refusal_rather_than_a_bare_413() -> Result<(), TestError> {
    let sessions = Arc::new(SessionStore::new(Arc::new(Counting(AtomicU64::new(0)))));
    let harness = Harness::from_parts(
        ServerConfig::new()?,
        sessions,
        true,
        AuthPolicy::default(),
        None,
    )?;

    let huge = "x".repeat(8 * 1024 * 1024);
    let response = harness.send(post(&huge)?).await?;

    if response.status() != StatusCode::PAYLOAD_TOO_LARGE {
        return Err(fail("an over-limit body was not refused with 413"));
    }
    // The refusal body is JSON-RPC, not an empty framework response.
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024).await?;
    let text = String::from_utf8_lossy(&bytes);
    if !text.contains("error") {
        return Err(fail(format!("a 413 carried no JSON-RPC error: {text}")));
    }
    Ok(())
}

// -------------------------------------------------------------------------
// The two free helpers both handlers call.
// -------------------------------------------------------------------------

/// `authenticate` and `resolve_session` are where the ordering is implemented,
/// and a test that only drove them through the router would pass even if one
/// handler called them in the other order.
#[tokio::test]
async fn the_gate_and_the_lookup_are_independently_usable() -> Result<(), TestError> {
    let authenticator = OneToken;
    let policy = AuthPolicy::Require(ScopeSet::all());
    let mut headers = HeaderMap::new();

    if authenticate(&authenticator, &policy, &headers)
        .await
        .is_ok()
    {
        return Err(fail("an anonymous caller passed a required policy"));
    }
    headers.insert(
        header::AUTHORIZATION,
        header::HeaderValue::from_static("Bearer good"),
    );
    if authenticate(&authenticator, &policy, &headers)
        .await
        .is_err()
    {
        return Err(fail("a valid credential was refused by the gate"));
    }

    // The lookup is a separate question, and neither answer consults the auth
    // context — that separation is the ordering the router relies on.
    let sessions = SessionStore::new(Arc::new(Counting(AtomicU64::new(0))));
    let issued = sessions.open(tail()?)?;
    let found = resolve_session(&sessions, &headers_with(issued)?)?;
    let SessionHandle::Live { id, .. } = found else {
        return Err(fail("a live session id did not resolve to a live session"));
    };
    if id != issued {
        return Err(fail(
            "the lookup returned a different id than the one asked for",
        ));
    }
    let forged = McpSessionId::from(SessionId::new(9_999));
    if resolve_session(&sessions, &headers_with(forged)?).is_ok() {
        return Err(fail("a forged session id resolved"));
    }
    if resolve_session(&sessions, &HeaderMap::new()).is_ok() {
        return Err(fail("a request with no session header resolved"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The version header, and what a request means without a session
// ---------------------------------------------------------------------------

/// A `POST` carrying an `MCP-Protocol-Version` the build can parse.
fn post_at_version(version: ProtocolVersion) -> Result<Request<Body>, TestError> {
    let mut request = post("{}")?;
    let value = header::HeaderValue::from_str(&version.to_string())?;
    request.headers_mut().insert("mcp-protocol-version", value);
    Ok(request)
}

#[tokio::test]
async fn the_version_header_reaches_the_handler_parsed() -> Result<(), TestError> {
    // The transport owes the handler one thing about this header: that the
    // bytes were parsed exactly once, at the boundary. So the handler is
    // handed a `ProtocolVersion` and never a `HeaderValue`, and the recorded
    // value is the version rather than the text it was written as.
    let harness = Harness::build(true)?;
    for version in [
        ProtocolVersion::V2024_11_05,
        ProtocolVersion::V2025_06_18,
        ProtocolVersion::V2026_07_28,
    ] {
        let _ = harness.send(post_at_version(version)?).await?;
    }
    let seen = harness.recorder.protocols();
    let expected: Vec<Option<String>> = [
        ProtocolVersion::V2024_11_05,
        ProtocolVersion::V2025_06_18,
        ProtocolVersion::V2026_07_28,
    ]
    .iter()
    .map(|version| Some(version.to_string()))
    .collect();
    require(
        &seen,
        &expected,
        "the handler was not handed the parsed header",
    )?;
    Ok(())
}

#[tokio::test]
async fn a_version_header_this_build_cannot_read_is_a_four_hundred() -> Result<(), TestError> {
    // Present but unreadable is a broken client, not one that disagrees: there
    // is no revision for the header to have named. Refusing here means the
    // request never reaches the handler, which is the only way to be sure a
    // bad header cannot be read as an absent one.
    let harness = Harness::build(true)?;
    let mut request = post("{}")?;
    request.headers_mut().insert(
        "mcp-protocol-version",
        header::HeaderValue::from_static("not-a-date"),
    );
    let response = harness.send(request).await?;
    require(
        &response.status(),
        &StatusCode::BAD_REQUEST,
        "a malformed version header was not refused",
    )?;
    require(
        &harness.recorder.dispatches(),
        &0,
        "a request with a malformed version header was still dispatched",
    )?;
    Ok(())
}

#[tokio::test]
async fn a_version_header_this_build_does_not_know_yet_is_carried_through() -> Result<(), TestError>
{
    // A date the build has no wire types for is `Unknown`, not an error. The
    // alternative — refusing what we cannot name — makes this server unusable
    // the day a new revision ships, which is the one outcome the negotiation
    // rules exist to prevent.
    let future = "2099-01-01".parse::<ProtocolVersion>()?;
    let harness = Harness::build(true)?;
    let _ = harness.send(post_at_version(future)?).await?;
    let seen = harness.recorder.protocols();
    let expected = vec![Some(future.to_string())];
    require(
        &seen,
        &expected,
        "a future revision was refused rather than carried",
    )?;
    Ok(())
}

#[tokio::test]
async fn a_request_with_no_version_header_is_not_refused() -> Result<(), TestError> {
    // The header became mandatory in 2025-06-18 and a pre-existing client
    // legitimately omits it. Absence is `None`, and the handler applies its
    // own fallback; a router that refused it would break every 2025-03-26 peer.
    let harness = Harness::build(true)?;
    let _ = harness.send(post("{}")?).await?;
    let expected = vec![None];
    require(
        &harness.recorder.protocols(),
        &expected,
        "an absent version header was not carried as absent",
    )?;
    Ok(())
}

#[tokio::test]
async fn a_stateless_request_is_served_without_a_session() -> Result<(), TestError> {
    // 2026-07-28: no initialize, no Mcp-Session-Id, and the profile comes from
    // the body. The router's part is to not insist on a session — the handler
    // told it the body stands alone, and inventing one would have made the
    // `_meta` version the request carries the only signal the server ignores.
    let sessions = Arc::new(SessionStore::new(Arc::new(Counting(AtomicU64::new(0)))));
    let harness = Harness::stateless(ServerConfig::new()?, Arc::clone(&sessions))?;
    let _ = harness
        .send(post_at_version(ProtocolVersion::V2026_07_28)?)
        .await?;
    let expected = vec![String::from("stateless")];
    require(
        &harness.recorder.seen(),
        &expected,
        "a stateless request was not served as one",
    )?;
    let stored = sessions.len()?;
    require(
        &stored,
        &0,
        "a stateless request minted a session it does not have",
    )?;
    Ok(())
}

#[tokio::test]
async fn two_stateless_requests_are_gated_independently() -> Result<(), TestError> {
    // The scenario the capability file names. Nothing is shared between them —
    // no session, no cursor, no negotiated version — so each one's header is
    // the whole of what it is gated by, and a router that cached the first
    // answer would serve the second under the wrong revision.
    let sessions = Arc::new(SessionStore::new(Arc::new(Counting(AtomicU64::new(0)))));
    let harness = Harness::stateless(ServerConfig::new()?, Arc::clone(&sessions))?;
    for version in [
        ProtocolVersion::V2025_03_26,
        ProtocolVersion::V2026_07_28,
        ProtocolVersion::V2025_03_26,
    ] {
        let _ = harness.send(post_at_version(version)?).await?;
    }
    let seen = harness.recorder.protocols();
    let expected: Vec<Option<String>> = [
        ProtocolVersion::V2025_03_26,
        ProtocolVersion::V2026_07_28,
        ProtocolVersion::V2025_03_26,
    ]
    .iter()
    .map(|version| Some(version.to_string()))
    .collect();
    require(
        &seen,
        &expected,
        "a stateless request was gated by another's header",
    )?;
    let expected = vec![String::from("stateless"); 3];
    require(
        &harness.recorder.seen(),
        &expected,
        "a stateless request was not served as one",
    )?;
    Ok(())
}

#[tokio::test]
async fn a_request_needing_a_session_and_naming_none_is_a_four_hundred() -> Result<(), TestError> {
    // The third answer to the unsessioned question, and the one the two-valued
    // version of the question could not express without lying about one of
    // the others.
    let harness = Harness::build(false)?;
    let response = harness.send(post("{}")?).await?;
    require(
        &response.status(),
        &StatusCode::BAD_REQUEST,
        "a request needing a session was not refused",
    )?;
    require(
        &harness.recorder.dispatches(),
        &0,
        "a refused request was still dispatched",
    )?;
    Ok(())
}

// -------------------------------------------------------------------------
// The challenge a 401 carries, and the one a 403 does not.
// -------------------------------------------------------------------------

/// The resource the harness publishes metadata for.
const RESOURCE: &str = "https://mcp.example/mcp";

/// A config that publishes RFC 9728 metadata for [`RESOURCE`].
fn publishing() -> Result<ServerConfig, TestError> {
    let metadata = ResourceMetadata::new(ResourceUri::new(RESOURCE)?);
    Ok(ServerConfig::new()?.with_resource_metadata(metadata))
}

/// The `WWW-Authenticate` header on a response, if it carries one.
fn challenge_of(response: &axum::response::Response) -> Option<String> {
    response
        .headers()
        .get(header::WWW_AUTHENTICATE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// A `401` names where the client can discover the authorization server.
///
/// Without this header a client that has just been handed an expired token
/// knows it needs a credential and has no way to learn which issuer issues
/// them. The challenge is the entire mechanism by which RFC 9728 gets from
/// "this resource is protected" to "here is the document", so a `401` without
/// it is a dead end rather than a smaller omission.
#[tokio::test]
async fn a_401_carries_the_resource_metadata_challenge() -> Result<(), TestError> {
    let harness = Harness::from_parts(
        publishing()?,
        Arc::new(SessionStore::new(Arc::new(Counting(AtomicU64::new(0))))),
        true,
        AuthPolicy::Require(ScopeSet::all()),
        Some(Arc::new(OneToken)),
    )?;
    let id = harness.open()?;
    // No `Authorization` header, so the policy refuses an anonymous context.
    let response = harness.send(with_session(post("{}")?, id)?).await?;

    require(&response.status(), &StatusCode::UNAUTHORIZED, "status")?;
    let challenge = challenge_of(&response)
        .ok_or_else(|| fail("a 401 carried no WWW-Authenticate challenge"))?;
    require(
        &challenge,
        &"Bearer resource_metadata=\"https://mcp.example/.well-known/oauth-protected-resource/mcp\""
            .to_owned(),
        "challenge",
    )
}

/// A `403` carries no challenge, because authenticating again cannot help.
///
/// RFC 6750 §3.1 attaches a challenge to `401`, not `403`: a `403` says this
/// credential is valid and will not do here. Sending a challenge on it tells a
/// correct client to do the one thing that will not help — fetch a new token
/// carrying exactly the scope it already has.
#[tokio::test]
async fn a_403_carries_no_challenge() -> Result<(), TestError> {
    let harness = Harness::from_parts(
        publishing()?,
        Arc::new(SessionStore::new(Arc::new(Counting(AtomicU64::new(0))))),
        true,
        AuthPolicy::Require(ScopeSet::empty().with(Scope::Admin)),
        Some(Arc::new(ReadOnly)),
    )?;
    let id = harness.open()?;
    let mut request = with_session(post("{}")?, id)?;
    request.headers_mut().insert(
        header::AUTHORIZATION,
        header::HeaderValue::from_static("Bearer good"),
    );
    let response = harness.send(request).await?;

    require(&response.status(), &StatusCode::FORBIDDEN, "status")?;
    require(
        &challenge_of(&response).is_none(),
        &true,
        "a 403 carried a WWW-Authenticate challenge",
    )
}

/// A `401` from a server that publishes nothing is unadorned, not `Bearer`.
///
/// A challenge naming no metadata tells the client the scheme and nothing
/// else, and a deployment in that state is misconfigured. Emitting a bare
/// `Bearer` would look like a working challenge to a client and leave it
/// searching for a document that was never published.
#[tokio::test]
async fn a_401_without_published_metadata_is_unadorned() -> Result<(), TestError> {
    let harness = Harness::with(
        true,
        AuthPolicy::Require(ScopeSet::all()),
        Some(Arc::new(OneToken)),
    )?;
    let id = harness.open()?;
    let response = harness.send(with_session(post("{}")?, id)?).await?;

    require(&response.status(), &StatusCode::UNAUTHORIZED, "status")?;
    require(
        &challenge_of(&response).is_none(),
        &true,
        "a 401 from a server publishing no metadata carried a challenge",
    )
}

/// Authenticates a real token, but with fewer scopes than any policy here
/// requires. Deliberately a different shape from [`OneToken`], which holds
/// everything, so a test meaning to exercise the `403` path cannot pass against
/// a gate that admits everything.
struct ReadOnly;

impl Authenticator for ReadOnly {
    fn authenticate<'a>(
        &'a self,
        presented: &'a Credentials<'_>,
    ) -> Pin<Box<dyn Future<Output = AuthContext> + Send + 'a>> {
        let context = if presented.authorization() == Some("Bearer good") {
            AuthContext::new("holder", ScopeSet::empty().with(Scope::Read))
        } else {
            AuthContext::anonymous()
        };
        Box::pin(std::future::ready(context))
    }
}

// --- T17: the layers that run before a handler -----------------------
//
// Driven through `oneshot` like everything else above, so these assert the
// router as it is actually mounted rather than the middleware in isolation.

/// A router carrying the T17 layers, built so a test can choose the origin
/// policy and the rate limit.
fn hardened(origin: OriginPolicy, limiter: RateLimiter) -> Result<Router, TestError> {
    let sessions = Arc::new(SessionStore::new(Arc::new(Counting(AtomicU64::new(0)))));
    let recorder = Arc::new(Recorder::default());
    let config = ServerConfig::new()?;
    Ok(McpRouter::new(
        config,
        sessions,
        Echo {
            recorder,
            establish: true,
            stateless: false,
        },
    )
    .with_origin_policy(origin)
    .with_rate_limiter(limiter)
    .into_router()?)
}

/// A limiter that allows `limit` requests in the hour, so a test cannot race a
/// window boundary.
fn limiter(limit: u32) -> Result<RateLimiter, TestError> {
    Ok(RateLimiter::new(RateLimit::new(
        NonZeroU32::new(limit).ok_or("a fixture asked for a zero limit")?,
        Duration::from_secs(3600),
    )))
}

fn origin(raw: &str) -> Result<OriginPolicy, TestError> {
    Ok(OriginPolicy::strict(Some(raw.parse()?)))
}

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;

fn post_with(body: &str, headers: &[(&str, &str)]) -> Result<Request<Body>, TestError> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::HOST, HOST);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    Ok(builder.body(Body::from((*body).to_owned()))?)
}

/// Streamable HTTP had no `Origin` check of its own, so a browser could
/// rebind onto it and be answered. This is the test that says that is fixed.
#[tokio::test]
async fn a_foreign_origin_is_refused_before_the_handler() -> Result<(), TestError> {
    let router = hardened(origin("http://localhost:3000")?, limiter(10)?)?;

    let response = router
        .clone()
        .oneshot(post_with(INITIALIZE, &[("origin", "http://evil.example")])?)
        .await?;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    Ok(())
}

/// An absent `Origin` is not a foreign one: a non-browser client sends none,
/// and refusing it would break every CLI client in existence.
#[tokio::test]
async fn a_request_with_no_origin_is_answered() -> Result<(), TestError> {
    let router = hardened(origin("http://localhost:3000")?, limiter(10)?)?;

    let response = router.clone().oneshot(post(INITIALIZE)?).await?;

    assert_ne!(
        response.status(),
        StatusCode::FORBIDDEN,
        "an absent Origin is a non-browser client, not a rebinding page"
    );
    Ok(())
}

/// A rebound browser names the attacker's domain in `Host`, and sends no
/// `Origin` it could have forged either.
///
/// This is the attack the `Origin` check alone does not catch, and the one
/// the conformance suite's `dns-rebinding-protection` scenario reports as a
/// server accepting a request it must refuse. Stated as an end-to-end
/// refusal rather than a unit test on the policy, because the defect was a
/// missing middleware and a policy that was correct would not have found it.
#[tokio::test]
async fn a_foreign_host_is_refused_before_the_handler() -> Result<(), TestError> {
    let router = hardened(origin("http://localhost:3000")?, limiter(10)?)?;

    let mut request = post(INITIALIZE)?;
    request
        .headers_mut()
        .insert(header::HOST, "evil.example.com".parse()?);

    let response = router.oneshot(request).await?;

    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "a request for another name was answered"
    );
    Ok(())
}

/// The loopback spellings the specification's suite calls valid, over the
/// wire rather than through the policy, because what was wrong was the
/// default: a browser and the suite both send `Origin: http://localhost:PORT`
/// and a server that refuses it serves neither.
#[tokio::test]
async fn a_loopback_origin_is_answered_on_the_wire() -> Result<(), TestError> {
    let router = hardened(origin("http://localhost:3000")?, limiter(10)?)?;

    for origin in [
        "http://localhost:3000",
        "http://127.0.0.1:3000",
        "http://[::1]:3000",
    ] {
        let response = router
            .clone()
            .oneshot(post_with(INITIALIZE, &[("origin", origin)])?)
            .await?;
        assert_ne!(
            response.status(),
            StatusCode::FORBIDDEN,
            "{origin} was refused"
        );
    }
    Ok(())
}

/// An origin a policy names must still be answered, or the allow-list is a
/// denial list wearing a different hat.
#[tokio::test]
async fn the_permitted_origin_is_answered() -> Result<(), TestError> {
    let router = hardened(origin("http://localhost:3000")?, limiter(10)?)?;

    let response = router
        .clone()
        .oneshot(post_with(
            INITIALIZE,
            &[("origin", "http://localhost:3000")],
        )?)
        .await?;

    assert_ne!(response.status(), StatusCode::FORBIDDEN);
    Ok(())
}

/// `429` and a `Retry-After` that is not zero. The header is the part that
/// matters: a client told only "429" retries immediately and keeps the rate up.
#[tokio::test]
async fn exceeding_the_rate_limit_is_a_429_with_retry_after() -> Result<(), TestError> {
    let router = hardened(OriginPolicy::strict(None), limiter(1)?)?;

    let first = router.clone().oneshot(post(INITIALIZE)?).await?;
    assert_ne!(first.status(), StatusCode::TOO_MANY_REQUESTS);

    let second = router.oneshot(post(INITIALIZE)?).await?;
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);

    let retry_after = second
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .ok_or("a 429 without Retry-After tells the client to guess")?;
    assert!(
        retry_after.parse::<u64>().is_ok_and(|seconds| seconds > 0),
        "Retry-After was {retry_after:?}; zero means retry now"
    );
    Ok(())
}

/// The allowance is per session. Without this, one client's flood would spend
/// everybody's budget and the limit would be a denial-of-service amplifier.
#[tokio::test]
async fn one_session_does_not_spend_anothers_allowance() -> Result<(), TestError> {
    let router = hardened(OriginPolicy::strict(None), limiter(1)?)?;

    let _ = router
        .clone()
        .oneshot(post_with(INITIALIZE, &[("mcp-session-id", "session-a")])?)
        .await?;
    let blocked = router
        .clone()
        .oneshot(post_with(INITIALIZE, &[("mcp-session-id", "session-a")])?)
        .await?;
    assert_eq!(blocked.status(), StatusCode::TOO_MANY_REQUESTS);

    let other = router
        .oneshot(post_with(INITIALIZE, &[("mcp-session-id", "session-b")])?)
        .await?;
    assert_ne!(
        other.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "session-b was charged for session-a's requests"
    );
    Ok(())
}

/// The body limit predates T17 and the new layers must not have displaced it.
/// A second `DefaultBodyLimit` would make this pass while answering a
/// different refusal than the one the router documents.
#[tokio::test]
async fn an_oversized_body_is_still_a_413() -> Result<(), TestError> {
    let router = hardened(OriginPolicy::strict(None), limiter(100)?)?;
    let huge = "x".repeat(8 * 1024 * 1024);

    let response = router
        .oneshot(post(&format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"{huge}\"}}"
        ))?)
        .await?;

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    Ok(())
}
