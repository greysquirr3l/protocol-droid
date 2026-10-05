//! The Streamable HTTP route table: `POST`, `GET` and `DELETE` on one endpoint,
//! and the middleware every one of them answers through.
//!
//! # What this module decides, and what it refuses to decide
//!
//! This is the transport's boundary and nothing past it. It decides four
//! things, in this order, and delegates everything else:
//!
//! 1. **Who is calling.** [`RequestHandler`] never sees a request that has not
//!    been through [`AuthPolicy::refuse`], and the gate runs before the session
//!    store is touched at all.
//! 2. **How large the body is.** Bounded by [`ServerConfig::max_body`] while
//!    it is read, with the over-limit case answered as
//!    [`Refusal::BodyTooLarge`] rather than as a framework `413` with an empty
//!    body.
//! 3. **Which session this is.** The `Mcp-Session-Id` header is *validated*,
//!    never *parsed into existence*: a well-formed id the store never issued is
//!    [`Refusal::UnknownSession`], a `404`, and never a session minted on the
//!    spot.
//! 4. **Nothing else.** Which method a body carries, what it means and what the
//!    answer is belong to the protocol layer. This file names no method, no
//!    revision and no capability; [`RequestHandler`] is the seam, and A2A
//!    (T27) or ACP (T29) drop in behind it as a different `H`.
//!
//! # The two rules that are structural rather than reviewed
//!
//! Both are easy to state and easy to break in a later commit, so they are
//! enforced by where the code lives rather than by a reviewer remembering.
//!
//! **A refusal is never a `200`.** [`Refusal::status`] and [`Refusal::body`]
//! are the only mapping in the adapter, and every failure path in every
//! handler ends in [`refuse`]. There is no error arm a handler can turn into a
//! successful-looking response.
//!
//! **Every response carries the security headers, including the ones no handler
//! wrote.** They are applied by [`with_security`] — a layer around the *whole*
//! router, not a call inside a handler — so a refusal, an error from the
//! protocol layer, and axum's own `405` for an unmapped method and `404` for an
//! unmapped path all carry them. A refusal that leaks no body but drops
//! `Cache-Control` is still a response an intermediary is entitled to store.
//!
//! # Why authentication precedes the session lookup
//!
//! Because the store answers two different questions — "does this id exist?"
//! and "no such id" — and the second takes measurably longer than the first
//! only for a *hit*. Lookup before authentication turns the store into an
//! oracle: an unauthenticated caller walks a plausible id space and reads the
//! latency, learns which ids are live, and can then aim a `DELETE` at one. The
//! gate is first on every route, so that difference is never observable by a
//! caller that has proved nothing.
//!
//! The same reasoning fixes where
//! [`RequestHandler::establishes_session`] is asked: only when the header is
//! *absent*, and therefore before the store is consulted at all. A handler that
//! answered from session state would reintroduce the oracle one layer in.
//!
//! # What is not here, and who owns it
//!
//! - **Session expiry.** A session leaves the store only when `DELETE`
//!   terminates it. A client whose connection drops without a `DELETE` leaves
//!   one behind forever, and this router does not reap it: a half-reaper that
//!   evicts by count turns a load spike into silent destruction of live state,
//!   which is the failure `SessionStore` documents at length. What is missing
//!   is an idle-or-absolute deadline owned by the store and driven by an
//!   injected `Clock`.
//! - **The `MCP-Protocol-Version` check.** A codec concern, and therefore the
//!   protocol layer's; this router does not know a version exists.
//! - **The `405` the specification allows for `GET`.** [`Refusal`] carries it
//!   for a server with no server-initiated stream. This server always has a
//!   tail to serve, so the arm is never raised here.
//! - **A `DELETE` tombstone.** See [`terminate_session`].

use std::sync::Arc;

use axum::Router as AxumRouter;
use axum::body::{Body, Bytes};
use axum::extract::State;
use core::pin::Pin;

use axum::http::{HeaderMap, HeaderName, HeaderValue, Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::MethodRouter;
use futures_core::Stream;
use serde_json::Value;

use super::{
    Anonymous, AuthPolicy, Authenticator, Credentials, EventTail, Refusal, SecurityHeaders,
    ServerConfig, SessionStore,
};
use crate::domain::ProtocolVersion;
use crate::domain::mcp::McpSessionId;
use crate::domain::registry::AuthContext;

/// The header naming a session, introduced with the Streamable HTTP transport
/// in revision **2025-03-26** — the revision that replaced the 2024-11-05
/// HTTP+SSE pair. Earlier revisions have no equivalent, because they had no
/// server-issued session at all.
///
/// This is the one MCP-specific header the adapter names, and it is a constant
/// rather than a string at each call site so that a later revision with a
/// different name is a one-line change here instead of a search.
const SESSION_HEADER: HeaderName = HeaderName::from_static("mcp-session-id");

/// The standard SSE resume header. Not MCP-specific: it comes from the HTML
/// `EventSource` contract every SSE implementation inherits.
const LAST_EVENT_ID: HeaderName = HeaderName::from_static("last-event-id");

/// The header a client asserts its session's revision with. Mandatory from
/// **2025-06-18** (PR #548) and absent before it, which is why its absence is
/// not an error here and its presence is a parsed value.
///
/// The adapter reads it but does not judge it. `check_protocol_header` compares
/// a header against the revision a session *negotiated*, and only the protocol
/// layer knows that; what the transport owes is to have parsed the bytes once,
/// so nothing downstream ever sees a raw `HeaderValue`.
const PROTOCOL_HEADER: HeaderName = HeaderName::from_static("mcp-protocol-version");

/// The three security headers, parsed once at construction.
///
/// [`SecurityHeaders`] is the configuration; this is the parsed form the
/// middleware needs on *every* response. Re-validating three strings per
/// response would put an allocation on the path that is guaranteed to be taken,
/// refusals included.
#[derive(Debug, Clone)]
struct SecurityLayer {
    content_security_policy: HeaderValue,
    strict_transport_security: HeaderValue,
    x_frame_options: HeaderValue,
}

impl SecurityLayer {
    /// Parses the configured values, failing construction rather than a request.
    ///
    /// These values come from a deployment, not from a peer, so one this server
    /// cannot write is a configuration error and belongs at the call site that
    /// supplied it — not as a `500` on every request afterwards.
    fn parse(headers: &SecurityHeaders) -> Result<Self, InvalidHeaderValue> {
        Ok(Self {
            content_security_policy: HeaderValue::from_str(headers.content_security_policy())?,
            strict_transport_security: HeaderValue::from_str(headers.strict_transport_security())?,
            x_frame_options: HeaderValue::from_str(headers.x_frame_options())?,
        })
    }
}

/// The configuration supplied a security header this server cannot send.
#[derive(Debug, thiserror::Error)]
#[error("a configured security header is not a valid HTTP header value: {0}")]
pub struct InvalidHeaderValue(axum::http::header::InvalidHeaderValue);

impl From<axum::http::header::InvalidHeaderValue> for InvalidHeaderValue {
    fn from(source: axum::http::header::InvalidHeaderValue) -> Self {
        Self(source)
    }
}

/// Everything a request needs, assembled once and shared by every route.
struct RouterState<S, H> {
    config: ServerConfig,
    sessions: Arc<SessionStore<S>>,
    handler: H,
    policy: AuthPolicy,
    authenticator: Arc<dyn Authenticator + Send + Sync>,
}

/// What a header said, distinguishing "not there" from "there and not text".
///
/// The three-way split is the point. A header whose bytes are not a legal
/// field-value has not been *absent*: treating it as absent would hand a peer
/// that cannot spell the header correctly a fresh session instead of a `400`.
#[derive(Debug, Clone, Copy)]
enum Header<'h> {
    /// The header was not sent.
    Absent,
    /// The header was sent and is visible ASCII.
    Value(&'h str),
    /// The header was sent and is not readable as a field-value.
    Unreadable,
}

impl<'h> Header<'h> {
    /// Reads `name` out of `headers`.
    fn read(headers: &'h HeaderMap, name: &HeaderName) -> Self {
        let Some(raw) = headers.get(name) else {
            return Self::Absent;
        };
        raw.to_str().map_or(Self::Unreadable, Self::Value)
    }
}

/// What a session has to be able to do for the `GET` route to open a stream on
/// it.
///
/// Declared here, beside the only consumer, because a port belongs to the
/// consumer. One method, because that is all the stream route asks: *what* to
/// replay is [`EventTail::plan`]'s decision, and *whether* to open a stream at
/// all is the protocol layer's.
pub trait EventSource: Send + Sync + 'static {
    /// The tail this session's server-initiated messages are published on.
    fn tail(&self) -> &EventTail<Value>;
}

/// A tail is its own event source, which is the shape T14's server uses: one
/// session, one tail. A server keeping more state per session holds the tail as
/// a field and implements this for the session itself.
impl EventSource for EventTail<Value> {
    fn tail(&self) -> &EventTail<Value> {
        self
    }
}

/// The session a request resolved to, as the protocol layer sees it.
#[derive(Debug)]
pub enum SessionHandle<S> {
    /// The body opens a session, so there was none to name.
    ///
    /// Distinct from a request that named nothing and needed to: the router
    /// answers the second with a `400` and never reaches the handler, so this
    /// arm can only mean the first.
    Establishing,
    /// The body is a complete stateless request, and the profile for it comes
    /// from the body rather than from a stored session.
    ///
    /// The **2026-07-28** lifecycle, and the reason this enum has three arms
    /// rather than two. A stateless request never becomes a session, so
    /// modelling it as [`Establishing`] would have the router hand the
    /// protocol layer an id to issue, and modelling it as
    /// [`SessionHandle::Live`] would mean inventing a session that does not
    /// exist. Either way the `_meta` version the request carries — the only
    /// signal a stateless request has — gets ignored.
    Stateless,
    /// The header named a session this server holds.
    Live {
        /// The id, as the server issued it.
        id: McpSessionId,
        /// The stored session.
        session: Arc<S>,
    },
}

/// What a body means when the request named no session.
///
/// The protocol layer answers this because the router is protocol-neutral: it
/// cannot know that `initialize` opens a session, nor that a **2026-07-28**
/// request carrying a version in `_meta` stands on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsessioned {
    /// The body opens a session.
    Establishing,
    /// The body is self-describing and is served against its own profile.
    Stateless,
    /// The body belongs to a session and the header named none. The router
    /// answers this with a `400` and the handler is never asked.
    NeedsSession,
}

/// A request the router has resolved and hands to the protocol layer.
#[derive(Debug)]
pub struct Inbound<'a, S> {
    /// What the caller proved it is, checked against the endpoint's policy.
    pub context: &'a AuthContext,
    /// The session this request belongs to, or that it opens.
    pub session: SessionHandle<S>,
    /// The body, already bounded by [`ServerConfig::max_body`].
    pub body: &'a [u8],
    /// The `MCP-Protocol-Version` header, parsed once here or refused as
    /// malformed, and `None` when the client sent none.
    ///
    /// Absence is not a fault: the header became mandatory in **2025-06-18**
    /// and a pre-existing client legitimately omits it. The handler compares
    /// it against the revision the session negotiated, through
    /// [`check_protocol_header`](crate::domain::check_protocol_header), which
    /// is the only place that knows the answer.
    pub protocol: Option<ProtocolVersion>,
}

/// The `POST` route's port: everything this module deliberately does not know.
///
/// Two methods. The first answers a question only the protocol layer can — does
/// this body open a session? — and the second does the work. The trait is
/// `async` and the router is generic over `H`, so there is no `dyn` here and no
/// boxed future per request: this is the hot path, and it is monomorphised.
///
/// # Examples
///
/// A handler that opens a session for any body and answers nothing yet — the
/// shape a real server starts as, with `dispatch` filled in by the protocol
/// layer:
///
/// ```
/// use std::sync::Arc;
///
/// use axum::body::Body;
/// use axum::http::Response;
/// use protocol_droid::adapters::primary::transport::http::{
///     EventSource, EventTail, Inbound, McpRouter, Refusal, RequestHandler, ServerConfig,
///     SessionStore, Unsessioned,
/// };
/// use protocol_droid::application::session::IdSource;
/// use protocol_droid::domain::session::state::SessionId;
/// use serde_json::Value;
///
/// /// Mints the same id every time, which is fine for a single example server.
/// struct OneId;
/// impl IdSource for OneId {
///     fn next_session_id(&self) -> SessionId {
///         SessionId::new(1)
///     }
/// }
///
/// struct Server;
///
/// impl<S: EventSource> RequestHandler<S> for Server {
///     fn unsessioned(&self, _body: &[u8]) -> Unsessioned {
///         Unsessioned::Establishing
///     }
///
///     async fn dispatch(
///         &self,
///         call: Inbound<'_, S>,
///     ) -> Result<Response<Body>, Refusal> {
///         let _ = (call.context, call.session, call.body, call.protocol);
///         Err(Refusal::Handler("this example serves no methods".to_owned()))
///     }
/// }
///
/// let config = ServerConfig::new()?;
/// let sessions = Arc::new(SessionStore::<EventTail<Value>>::new(Arc::new(OneId)));
/// // The router is stateful, so it keeps the handler alive; hand it to a
/// // server when one exists, or test it through `tower` in the meantime.
/// let router = McpRouter::new(config, sessions, Server).into_router()?;
/// let _ = router;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub trait RequestHandler<S>: Send + Sync + 'static {
    /// What a request body means when it named no session.
    ///
    /// Asked for every `POST` whose header is absent, and never for one whose
    /// header is present — so a body that establishes a session but arrives
    /// with a bad id is still a `400`.
    ///
    /// Three answers rather than a bool, because a boolean cannot express the
    /// case the **2026-07-28** stateless lifecycle adds: a request that is
    /// complete on its own, carries its version in `_meta`, and is served
    /// against a profile resolved per request. That is not "opens a session"
    /// and it is not "needs one", and a two-valued question has to lie about
    /// one of them.
    fn unsessioned(&self, body: &[u8]) -> Unsessioned;

    /// Runs the request.
    ///
    /// The body has arrived read and bounded; the context has passed the
    /// endpoint's policy; and the session, when the header named one, is
    /// [`SessionHandle::Live`], because a malformed or unknown id was refused
    /// before this was called.
    ///
    /// # Errors
    ///
    /// Any [`Refusal`]. There is no other failure channel: an adapter that
    /// cannot answer a request has exactly one honest way to say so, and it is
    /// a status code with a JSON-RPC body rather than a transport exception.
    ///
    /// # Why this is `-> impl Future + Send` and not `async fn`
    ///
    /// Because a trait method written `async fn` returns an RPITIT future whose
    /// auto-trait bounds are *not* visible to a generic caller, and this
    /// adapter's caller is generic: `post_request` is generic over `H` and
    /// axum requires a handler's future to be `Send`. Written this way the
    /// bound is part of the contract, so a caller holding only `H:
    /// RequestHandler<S>` can rely on it without knowing anything about `H`'s
    /// body. An implementor writes a plain `async fn dispatch`, which satisfies
    /// this declaration and keeps the future unboxed — the alternative,
    /// `Pin<Box<dyn Future>>`, would put a heap allocation on the request path
    /// to buy a bound the declaration already gives for free.
    fn dispatch(
        &self,
        call: Inbound<'_, S>,
    ) -> impl Future<Output = Result<Response, Refusal>> + Send;
}

/// The Streamable HTTP route table.
///
/// Generic over the stored session `S` and the protocol layer `H`, so swapping
/// either does not change the router's own type.
pub struct McpRouter<S, H> {
    config: ServerConfig,
    sessions: Arc<SessionStore<S>>,
    handler: H,
    policy: AuthPolicy,
    authenticator: Arc<dyn Authenticator + Send + Sync>,
}

/// Hand-written rather than derived, and for two reasons.
///
/// The derive would demand `S: Debug + H: Debug`, which is a bound a
/// deployment's own protocol layer would have to satisfy for a `{:?}` nobody
/// writes. And this is the one type in the adapter where a `{:?}` is a
/// plausible accident: it holds the session store. [`SessionStore`]'s own
/// `Debug` deliberately prints its length and not its keys, and a derived
/// format would bypass that, so the field is left out entirely rather than
/// summarised.
impl<S, H> core::fmt::Debug for McpRouter<S, H> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("McpRouter")
            .field("endpoint", &self.config.endpoint())
            .field("streaming", &self.config.streaming())
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl<S, H> McpRouter<S, H>
where
    S: EventSource,
    H: RequestHandler<S>,
{
    /// A router over `sessions`, dispatching through `handler`, authenticating
    /// anonymously and permitting everything.
    ///
    /// [`with_authenticator`](Self::with_authenticator) and
    /// [`with_policy`](Self::with_policy) are the two knobs, and both are
    /// explicit: a deployment that needs a credential says so, rather than
    /// inheriting a default that fails open or closed by accident.
    #[must_use]
    pub fn new(config: ServerConfig, sessions: Arc<SessionStore<S>>, handler: H) -> Self {
        Self {
            config,
            sessions,
            handler,
            policy: AuthPolicy::Public,
            authenticator: Arc::new(Anonymous),
        }
    }

    /// Replaces the anonymous default with a real implementation.
    ///
    /// T16 supplies the OAuth one. It is a `dyn` because a deployment picks it
    /// at start-up and the choice is not this router's to make; the trait is
    /// object-safe precisely because its one method is synchronous.
    #[must_use]
    pub fn with_authenticator(
        mut self,
        authenticator: Arc<dyn Authenticator + Send + Sync>,
    ) -> Self {
        self.authenticator = authenticator;
        self
    }

    /// States, out loud, whether this endpoint needs a credential.
    #[must_use]
    pub const fn with_policy(mut self, policy: AuthPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// The mounted [`axum::Router`].
    ///
    /// # Errors
    ///
    /// [`InvalidHeaderValue`] if a configured security header is not a legal
    /// HTTP header value. Failing here is the point: the alternative is a
    /// server that starts and then answers every request with a header set it
    /// cannot write.
    pub fn into_router(self) -> Result<AxumRouter, InvalidHeaderValue> {
        let endpoint = self.config.endpoint().as_str().to_owned();
        let security = self.config.security().clone();
        let state = Arc::new(RouterState {
            config: self.config,
            sessions: Arc::clone(&self.sessions),
            handler: self.handler,
            policy: self.policy,
            authenticator: Arc::clone(&self.authenticator),
        });
        let methods: MethodRouter<Arc<RouterState<S, H>>> = axum::routing::post(post_request)
            .get(open_stream)
            .delete(terminate_session);
        let router = AxumRouter::new()
            .route(&endpoint, methods)
            .with_state(state);
        with_security(router, &security)
    }
}

/// Wraps `router` so that **every** response carries `headers`.
///
/// Public and free-standing because T15's legacy HTTP+SSE transport mounts a
/// different set of routes and needs the identical guarantee. A second
/// implementation of "apply the three headers" is exactly how a transport ends
/// up answering errors without them.
///
/// The layer wraps the router as a whole rather than each route, which is what
/// puts the headers on responses no handler wrote: axum's `405` for an unmapped
/// method, its `404` for an unmapped path, and an extractor's rejection.
///
/// # Errors
///
/// [`InvalidHeaderValue`] if a configured header is not a legal HTTP value.
pub fn with_security(
    router: AxumRouter,
    headers: &SecurityHeaders,
) -> Result<AxumRouter, InvalidHeaderValue> {
    let layer = Arc::new(SecurityLayer::parse(headers)?);
    Ok(router.layer(axum::middleware::from_fn_with_state(
        layer,
        apply_security_headers,
    )))
}

/// Adds the three security headers to whatever the inner service answered.
async fn apply_security_headers(
    State(policy): State<Arc<SecurityLayer>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let mut response = next.run(request).await;
    let out = response.headers_mut();
    // `insert` rather than `append`: these are policy headers about the
    // response as a whole, and a second copy of one is a header an
    // intermediary has to guess between.
    out.insert(
        header::CONTENT_SECURITY_POLICY,
        policy.content_security_policy.clone(),
    );
    out.insert(
        header::STRICT_TRANSPORT_SECURITY,
        policy.strict_transport_security.clone(),
    );
    out.insert(header::X_FRAME_OPTIONS, policy.x_frame_options.clone());
    response
}

/// The `POST` route: one request, one answer, or one stream.
async fn post_request<S, H>(
    State(state): State<Arc<RouterState<S, H>>>,
    headers: HeaderMap,
    body: Body,
) -> Response
where
    S: EventSource,
    H: RequestHandler<S>,
{
    // Authentication first, before anything else. See the module docs for the
    // oracle the ordering prevents.
    let context = match authenticate(state.authenticator.as_ref(), &state.policy, &headers) {
        Ok(context) => context,
        Err(refusal) => return refuse(refusal),
    };

    // The ceiling is the domain's, read once and converted once. A `u32` that
    // does not fit a `usize` is a target whose `usize` is narrower than 32
    // bits; refusing every body is the safe reading there, because the
    // alternative is a limit that silently wraps.
    let Ok(limit) = usize::try_from(state.config.max_body().bytes()) else {
        // A `u32` that does not fit a `usize` is a target whose `usize` is
        // narrower than 32 bits. Refusing every body is the safe reading
        // there, because the alternative is a limit that silently wraps.
        return refuse(Refusal::Handler(
            "the configured body limit does not fit this platform".to_owned(),
        ));
    };
    let body = match read_body(body, limit).await {
        Ok(body) => body,
        Err(refusal) => return refuse(refusal),
    };

    // The version header is parsed before anything is dispatched, so a client
    // that wrote a broken one is refused without the request running. What it
    // *disagrees* with is not decided here: that is a comparison against the
    // session's negotiated revision, and only the protocol layer knows it.
    let protocol = match read_protocol(&headers) {
        Ok(protocol) => protocol,
        Err(refusal) => return refuse(refusal),
    };

    // The body is read before the header is resolved, because "what does this
    // body mean without a session?" is a question about the body. It is asked
    // only when the header is absent, and asking it is the same as not
    // consulting the store — an absent header never reaches a lookup.
    let session = match Header::read(&headers, &SESSION_HEADER) {
        Header::Absent => match state.handler.unsessioned(&body) {
            Unsessioned::Establishing => SessionHandle::Establishing,
            Unsessioned::Stateless => SessionHandle::Stateless,
            Unsessioned::NeedsSession => return refuse(Refusal::MissingSession),
        },
        Header::Value(_) | Header::Unreadable => match resolve_session(&state.sessions, &headers) {
            Ok(session) => session,
            Err(refusal) => return refuse(refusal),
        },
    };

    let call = Inbound {
        context: &context,
        session,
        body: &body,
        protocol,
    };
    match state.handler.dispatch(call).await {
        Ok(response) => response,
        Err(refusal) => refuse(refusal),
    }
}

/// The `GET` route: open the session's stream, resuming from `Last-Event-ID`.
async fn open_stream<S, H>(
    State(state): State<Arc<RouterState<S, H>>>,
    headers: HeaderMap,
) -> Response
where
    S: EventSource,
    H: RequestHandler<S>,
{
    if let Err(refusal) = authenticate(state.authenticator.as_ref(), &state.policy, &headers) {
        return refuse(refusal);
    }

    // A `GET` is a stream for a session that already exists. There is no body
    // to inspect, so the header is not optional here.
    let session = match resolve_session(&state.sessions, &headers) {
        Ok(session) => session,
        Err(refusal) => return refuse(refusal),
    };
    let SessionHandle::Live { session, .. } = session else {
        // Unreachable: `resolve_session` only ever yields `Live`. Written as a
        // `let ... else` rather than a panic because this file's rules forbid
        // both, and a refusal is the honest answer if it is ever reached.
        return refuse(Refusal::Handler(
            "the session lookup produced no session".to_owned(),
        ));
    };

    // The cursor is the header and nothing else. The replay decision — full
    // replay, a gap to announce, a cursor ahead of the stream, or a cursor that
    // is not a cursor at all — belongs to `EventTail::plan`, and re-deriving
    // any of it here is how a resume starts agreeing with itself on the happy
    // path and disagreeing on the two paths nobody exercises. A `Last-Event-ID`
    // that is not readable as a field-value cannot be handed to `plan` at all,
    // and reading it as "absent" would hand the client a stream it believes it
    // has already seen.
    let cursor = match Header::read(&headers, &LAST_EVENT_ID) {
        Header::Absent => None,
        Header::Value(text) => Some(text),
        Header::Unreadable => return refuse(Refusal::MalformedCursor),
    };

    // `into_response` is the one conversion from a decision to a response: a
    // refusal becomes its own status and JSON-RPC body through the same
    // `Refusal::into_response` every other path here uses, and anything else
    // becomes a `200 text/event-stream` carrying the replayed frames and then
    // staying open. Its `Err` arm is response assembly rather than protocol,
    // so it is a 500 and not a stream.
    match session
        .tail()
        .plan(cursor)
        .into_response(state.config.keep_alive())
    {
        Ok(response) => response,
        Err(error) => refuse(Refusal::Handler(error.to_string())),
    }
}

/// The `DELETE` route: terminate the named session.
///
/// # Idempotence
///
/// Terminating a live session is `200`, and a repeated `DELETE` is a `404`
/// because the store has no tombstone and cannot tell a repeat from an id it
/// never issued. That is the specification's own answer to "this server has no
/// such session", and it tells the client to start a new session — which is
/// exactly what a client that just terminated one wants. Making the repeat a
/// `200` would need the store to remember terminated ids, which is a retention
/// policy and has to be a bounded one; until that exists a `404` is the honest
/// answer, and this paragraph is the note that says so.
async fn terminate_session<S, H>(
    State(state): State<Arc<RouterState<S, H>>>,
    headers: HeaderMap,
) -> Response
where
    S: EventSource,
    H: RequestHandler<S>,
{
    if let Err(refusal) = authenticate(state.authenticator.as_ref(), &state.policy, &headers) {
        return refuse(refusal);
    }

    let id = match session_id(&headers) {
        Ok(Some(id)) => id,
        Ok(None) => return refuse(Refusal::MissingSession),
        Err(refusal) => return refuse(refusal),
    };

    // The returned `Arc` is dropped at the end of this match arm, which is what
    // releases the session's tail and every waker parked on it. Nothing else in
    // the adapter drops a session, so this is the only path that frees one.
    match state.sessions.terminate(&id) {
        Ok(Some(_terminated)) => StatusCode::OK.into_response(),
        Ok(None) => refuse(Refusal::UnknownSession),
        Err(error) => refuse(Refusal::Handler(error.to_string())),
    }
}

/// Resolves the caller, and refuses it if the endpoint's policy says so.
///
/// # Errors
///
/// Whatever [`AuthPolicy::refuse`] returns: `401` for a caller that presented
/// nothing usable, `403` for one whose credential proved an identity without
/// the scope this endpoint names.
fn authenticate(
    authenticator: &dyn Authenticator,
    policy: &AuthPolicy,
    headers: &HeaderMap,
) -> Result<AuthContext, Refusal> {
    let context = authenticator.authenticate(&Credentials::from_headers(headers));
    policy.refuse(&context).map_or(Ok(context), Err)
}

/// The `Mcp-Session-Id` the request named, if it named one.
///
/// # Errors
///
/// [`Refusal::MalformedSessionId`] — a `400` — when the header is present but
/// is not a well-formed id. That is deliberately distinct from the `404` of
/// [`resolve_session`]: one means the client's header is wrong, the other
/// means its session is gone, and the fixes are different.
fn session_id(headers: &HeaderMap) -> Result<Option<McpSessionId>, Refusal> {
    match Header::read(headers, &SESSION_HEADER) {
        Header::Absent => Ok(None),
        Header::Unreadable => Err(Refusal::MalformedSessionId),
        Header::Value(text) => text
            .parse::<McpSessionId>()
            .map(Some)
            .map_err(|_| Refusal::MalformedSessionId),
    }
}

/// Resolves the header into a live session.
///
/// # Errors
///
/// [`Refusal::MissingSession`] for an absent header,
/// [`Refusal::MalformedSessionId`] for one that is not an id,
/// [`Refusal::UnknownSession`] for an id this server never issued, and
/// [`Refusal::Handler`] if the store's lock is poisoned.
fn resolve_session<S: EventSource>(
    sessions: &SessionStore<S>,
    headers: &HeaderMap,
) -> Result<SessionHandle<S>, Refusal> {
    let Some(id) = session_id(headers)? else {
        return Err(Refusal::MissingSession);
    };
    let found = sessions
        .lookup(&id)
        .map_err(|error| Refusal::Handler(error.to_string()))?;
    // The `Arc` is cloned out of the store and the clone is what the protocol
    // layer holds for the rest of the request. A session another request
    // terminates mid-flight therefore stays alive until this one finishes,
    // which is the lifetime a handler can safely assume.
    found.map_or_else(
        || Err(Refusal::UnknownSession),
        |session| Ok(SessionHandle::Live { id, session }),
    )
}

/// Collects the request body, refusing it the moment it exceeds `limit`.
///
/// Bounded *while* reading rather than after: a `Content-Length` is a claim, and
/// a body that arrives over a limit must not first be buffered in full in order
/// to find out. The two failure cases are kept apart because a client can act on
/// one and not the other — over the ceiling is the caller's mistake and is a
/// `413`; an unreadable body is ours and is a `500`.
///
/// # Errors
///
/// [`Refusal::BodyTooLarge`] or [`Refusal::Handler`].
/// Reads `MCP-Protocol-Version`, or `None` when the client sent none.
///
/// A header that is present but is not a date this build can parse is a
/// `400` here rather than something the handler has to notice. A date it
/// cannot *know* — a future revision — parses to
/// [`ProtocolVersion::Unknown`] and is carried through, because refusing it
/// here would make this server unusable the day a new revision ships, which is
/// the one outcome the negotiation rules exist to prevent.
fn read_protocol(headers: &HeaderMap) -> Result<Option<ProtocolVersion>, Refusal> {
    let raw = match Header::read(headers, &PROTOCOL_HEADER) {
        Header::Absent => return Ok(None),
        Header::Unreadable => {
            return Err(Refusal::ProtocolHeader {
                reason: String::from("the value is not visible ASCII"),
            });
        }
        Header::Value(value) => value,
    };
    raw.parse::<ProtocolVersion>()
        .map(Some)
        .map_err(|_| Refusal::ProtocolHeader {
            reason: raw.to_owned(),
        })
}

/// The `POST` route's body, bounded by [`ServerConfig::max_body`].
async fn read_body(body: Body, limit: usize) -> Result<Bytes, Refusal> {
    let mut chunks = body.into_data_stream();
    let mut collected: Vec<u8> = Vec::new();
    while let Some(chunk) = next_item(&mut chunks).await {
        let chunk = chunk.map_err(|error| {
            Refusal::Handler(format!("the request body could not be read: {error}"))
        })?;
        if collected.len().saturating_add(chunk.len()) > limit {
            return Err(Refusal::BodyTooLarge);
        }
        collected.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(collected))
}

/// The next item from `stream`, waiting for as long as it takes.
///
/// `Stream::next` lives on `futures_util::StreamExt`, and this crate depends
/// on `futures-core` rather than the whole of `futures`. Polling through
/// `poll_fn` is the same operation written out, and it is the only place in the
/// adapter that has to spell it.
async fn next_item<S>(stream: &mut S) -> Option<S::Item>
where
    S: Stream + Unpin,
{
    std::future::poll_fn(|context| Pin::new(&mut *stream).poll_next(context)).await
}

/// The one way a refusal becomes a response.
///
/// Every failure path in every handler ends here. It is a function rather than a
/// habit repeated at each call site because the alternative is the shape of bug
/// this adapter is built not to have: a handler that answers a refusal with
/// `StatusCode::OK` because it was in a hurry, on a path no test walks.
fn refuse(refusal: Refusal) -> Response {
    refusal.into_response()
}

#[cfg(test)]
mod tests;
