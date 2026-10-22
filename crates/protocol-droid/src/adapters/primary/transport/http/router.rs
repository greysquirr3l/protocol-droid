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
//! handler ends in `refuse`. There is no error arm a handler can turn into a
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
//! `RequestHandler::establishes_session` is asked: only when the header is
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
//! - **A `DELETE` tombstone.** See `terminate_session`.

use std::sync::Arc;

use axum::Router as AxumRouter;
use axum::body::Body;
use axum::extract::State;

use axum::http::{HeaderMap, HeaderName, HeaderValue, Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::MethodRouter;
use serde_json::Value;

use super::config::Streaming;
use super::hardening::{self, enforce_deadline, enforce_host, enforce_origin, enforce_rate_limit};
use super::limits::RateLimiter;
use super::precondition::{authenticate, read_body};
use super::{
    Anonymous, AuthPolicy, Authenticator, EventTail, JSON_MEDIA_TYPE, Refusal, SecurityHeaders,
    ServerConfig, SessionStore,
};
use crate::adapters::primary::transport::origin::OriginPolicy;
use crate::domain::ProtocolVersion;
use crate::domain::jsonrpc::{Frame, WireId, decode};
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
pub(crate) struct RouterState<S, H> {
    config: ServerConfig,
    sessions: Arc<SessionStore<S>>,
    handler: H,
    policy: AuthPolicy,
    authenticator: Arc<dyn Authenticator + Send + Sync>,
    origin: OriginPolicy,
    rate_limiter: RateLimiter,
}

impl<S, H> RouterState<S, H> {
    /// The `Origin` allow-list this server answers for.
    pub(crate) const fn origin(&self) -> &OriginPolicy {
        &self.origin
    }

    /// The per-subject request accounting.
    /// The configuration this router was built with.
    pub(crate) const fn config(&self) -> &ServerConfig {
        &self.config
    }

    pub(crate) const fn rate_limiter(&self) -> &RateLimiter {
        &self.rate_limiter
    }
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
    /// modelling it as `Establishing` would have the router hand the
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
    /// Every request header, for the checks only the protocol layer can make.
    ///
    /// A `2026-07-28` request is validated against its own headers —
    /// `MCP-Protocol-Version`, `Mcp-Method` and `Mcp-Name` must agree with the
    /// body — and what "agree" means is a protocol question this router does
    /// not answer.
    pub headers: &'a HeaderMap,
    /// How this server was configured to frame a reply.
    ///
    /// Carried rather than read from config inside the handler, for the same
    /// reason `protocol` is: the value is decided here, where the config
    /// lives, and the handler applies it. It was previously not carried at
    /// all, which is how `Streaming::Json` came to mean a bare document for
    /// `initialize` and an event stream for everything after it — the default
    /// was honoured on one path and ignored on the other.
    pub streaming: Streaming,
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
///     fn unsessioned(&self, _body: &[u8], _headers: &axum::http::HeaderMap) -> Unsessioned {
///         Unsessioned::Establishing
///     }
///
///     async fn dispatch(
///         &self,
///         call: Inbound<'_, S>,
///     ) -> Result<Response<Body>, Refusal> {
///         let _ = (call.context, call.session, call.body, call.protocol, call.headers);
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
    ///
    /// The headers are offered because a body alone cannot always say: a
    /// `2026-07-28` request whose `_meta` is missing altogether is still a
    /// stateless request — its `MCP-Protocol-Version` names a revision with no
    /// sessions — and it is owed that revision's `-32602`, not a
    /// missing-session refusal from a lifecycle it is not using.
    fn unsessioned(&self, body: &[u8], headers: &HeaderMap) -> Unsessioned;

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
/// The error returned by [`McpRouter::into_router`].
///
/// Two failure modes, both at mount time rather than at the first request:
/// a configured security header that is not a legal HTTP value
/// ([`Header`](Self::Header)), and the silent-fail-open combination
/// described in issue #11
/// ([`UnprotectedAuthenticator`](Self::UnprotectedAuthenticator)). The
/// router was previously typed `Result<_, InvalidHeaderValue>` and the
/// new variant widens it; callers that used `?` continue to work because
/// the existing `ServeError::Routes` wraps `InvalidHeaderValue` and
/// `BridgeServeError::Router` does the same.
#[derive(Debug, thiserror::Error)]
pub enum RouterError {
    /// A configured security header is not a legal HTTP header value.
    ///
    /// Surfaces a malformed `SecurityHeaders` at mount time rather than
    /// at the first response. The alternative is a server that starts and
    /// then answers every request with a header set it cannot write.
    #[error("a configured security header is not a legal HTTP header value: {0}")]
    Header(#[source] InvalidHeaderValue),

    /// A real authenticator was wired but the policy is still the default
    /// [`AuthPolicy::Public`].
    ///
    /// The combination is the exact fail-open the issue #11 review
    /// described: `with_authenticator` is consulted, a valid token
    /// produces a scoped `AuthContext`, but `AuthPolicy::refuse` answers
    /// `None` on an empty `required()` and the endpoint is served to
    /// anonymous callers. Failing closed here means a deployment that
    /// wired a token authenticator but forgot the `with_policy` call
    /// gets a startup error, not a permission boundary that does not
    /// exist.
    ///
    /// The fix is to follow `with_authenticator` with
    /// `with_policy(AuthPolicy::Require(scopes))` — or, for a deployment
    /// that genuinely wants anonymous callers, drop the
    /// `with_authenticator` call entirely.
    #[error(
        "with_authenticator was wired but the policy is still AuthPolicy::Public; \
         a real authenticator paired with the default public policy fails open \
         silently. Add .with_policy(AuthPolicy::Require(scopes)) after \
         .with_authenticator(...), or drop the .with_authenticator(...) call if \
         the endpoint is meant to be anonymous."
    )]
    UnprotectedAuthenticator,
}

impl From<InvalidHeaderValue> for RouterError {
    fn from(value: InvalidHeaderValue) -> Self {
        Self::Header(value)
    }
}

/// Generic over the stored session `S` and the protocol layer `H`, so swapping
/// either does not change the router's own type.
pub struct McpRouter<S, H> {
    config: ServerConfig,
    sessions: Arc<SessionStore<S>>,
    handler: H,
    policy: AuthPolicy,
    authenticator: Arc<dyn Authenticator + Send + Sync>,
    origin: OriginPolicy,
    rate_limiter: RateLimiter,
    /// True iff [`with_authenticator`](Self::with_authenticator) was called.
    ///
    /// The struct's default `authenticator` is the unit [`Anonymous`], and
    /// a caller that wires a real authenticator but leaves the policy at
    /// the default [`AuthPolicy::Public`] is the exact configuration
    /// described in GitHub issue #11: the server starts, the token check
    /// is consulted, but `refuse` short-circuits on an empty `required()`
    /// and the endpoint is served to anonymous callers. The field exists
    /// so [`into_router`](Self::into_router) can fail closed on the
    /// combination.
    authenticator_was_explicit: bool,
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
            rate_limiter: RateLimiter::new(*config.rate_limit()),
            origin: OriginPolicy::strict(None),
            config,
            sessions,
            handler,
            policy: AuthPolicy::Public,
            authenticator: Arc::new(Anonymous),
            authenticator_was_explicit: false,
        }
    }

    /// Sets the `Origin` allow-list.
    ///
    /// T17. Streamable HTTP had no origin check of its own — the policy was
    /// written for the legacy HTTP+SSE transport and left there — which meant
    /// a browser could rebind onto a Streamable HTTP server and have the
    /// request answered. The default is [`OriginPolicy::strict`] with no
    /// origins, which refuses every `Origin` that is sent and allows a request
    /// that sent none — see `OriginPolicy::check` for why absent is not the
    /// same as foreign.
    #[must_use]
    pub fn with_origin_policy(mut self, origin: OriginPolicy) -> Self {
        self.origin = origin;
        self
    }

    /// Replaces the per-subject request allowance.
    ///
    /// Defaults to [`ServerConfig::rate_limit`], so a configuration that set
    /// one does not have to set it twice.
    #[must_use]
    pub fn with_rate_limiter(mut self, limiter: RateLimiter) -> Self {
        self.rate_limiter = limiter;
        self
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
        self.authenticator_was_explicit = true;
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
    /// [`RouterError::Header`] if a configured security header is not a
    /// legal HTTP header value. Failing here is the point: the alternative
    /// is a server that starts and then answers every request with a header
    /// set it cannot write.
    ///
    /// [`RouterError::UnprotectedAuthenticator`] if a real authenticator was
    /// wired with [`with_authenticator`](Self::with_authenticator) but the
    /// policy is still the default [`AuthPolicy::Public`]. The combination
    /// is the silent fail-open the issue #11 review described: the
    /// authenticator is consulted, a valid token produces a scoped context,
    /// but `refuse` short-circuits on an empty `required()` and the
    /// endpoint is served to anonymous callers. Failing closed at mount
    /// time turns the misconfiguration into a startup error the operator
    /// sees, rather than a permission boundary that quietly does not exist.
    pub fn into_router(self) -> Result<AxumRouter, RouterError> {
        if self.authenticator_was_explicit && matches!(self.policy, AuthPolicy::Public) {
            return Err(RouterError::UnprotectedAuthenticator);
        }
        let endpoint = self.config.endpoint().as_str().to_owned();
        let security = self.config.security().clone();
        let max_concurrency = self.config.max_concurrency();
        let state = Arc::new(RouterState {
            config: self.config,
            sessions: Arc::clone(&self.sessions),
            handler: self.handler,
            policy: self.policy,
            authenticator: Arc::clone(&self.authenticator),
            origin: self.origin,
            rate_limiter: self.rate_limiter,
        });
        let methods: MethodRouter<Arc<RouterState<S, H>>> = axum::routing::post(post_request)
            .get(open_stream)
            .delete(terminate_session);
        let mut router = AxumRouter::new().route(&endpoint, methods);

        // RFC 9728 §3.1's discovery document, mounted from the same config that
        // made the `401` challenge name it. `with_resource_metadata` says this
        // server publishes a document; leaving the caller to also remember to
        // merge a router for it means the two halves can drift, and the drift
        // is a `WWW-Authenticate` pointing at a `404`. The config is the single
        // source of truth for both.
        //
        // Safe inside this router, which was the open question when this was
        // written: `OriginPolicy::check` returns `Ok` for an absent `Origin`
        // (a non-browser client, by design), and `check_host` only demands the
        // `Host` a production deployment already had to configure for its MCP
        // requests to be answered at all. So the discovery `GET` faces exactly
        // the policy every other route on this server faces — no more.
        if let Some(metadata) = state.config.resource_metadata() {
            router = router.route(
                &metadata.resource().metadata_path(),
                axum::routing::get(serve_document),
            );
        }

        let router = router
            .layer(axum::middleware::from_fn_with_state(
                Arc::clone(&state),
                enforce_deadline,
            ))
            .layer(axum::middleware::from_fn_with_state(
                Arc::clone(&state),
                enforce_origin,
            ))
            .layer(axum::middleware::from_fn_with_state(
                Arc::clone(&state),
                enforce_host,
            ))
            .layer(axum::middleware::from_fn_with_state(
                Arc::clone(&state),
                enforce_rate_limit,
            ))
            .with_state(state);
        // Security headers outermost, so they reach the responses no handler
        // wrote; load shedding inside them, so a shed request still carries
        // them. `apply_layers` is last because it is the only layer that
        // decides not to run the inner ones at all.
        Ok(with_security(
            hardening::apply_layers(router, max_concurrency),
            &security,
        )?)
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
    let context = match authenticate(state.authenticator.as_ref(), &state.policy, &headers).await {
        Ok(context) => context,
        Err(refusal) => return refuse_with_challenge(state.config.challenge().as_deref(), refusal),
    };

    // The ceiling is the domain's, read once and converted once. A `u32` that
    // does not fit a `usize` is a target whose `usize` is narrower than 32
    // bits; refusing every body is the safe reading there, because the
    // alternative is a limit that silently wraps.
    let Ok(limit) = usize::try_from(state.config.max_body().bytes()) else {
        // A `u32` that does not fit a `usize` is a target whose `usize` is
        // narrower than 32 bits. Refusing every body is the safe reading
        // there, because the alternative is a limit that silently wraps.
        return refuse_with_challenge(
            state.config.challenge().as_deref(),
            Refusal::Handler("the configured body limit does not fit this platform".to_owned()),
        );
    };
    let body = match read_body(body, limit).await {
        Ok(body) => body,
        Err(refusal) => return refuse_with_challenge(state.config.challenge().as_deref(), refusal),
    };

    // Read once, here, so every refusal *after* this point can answer the id
    // the client used. A body that will not decode has no id to offer and
    // falls back to `null`, which is what JSON-RPC asks for when the id was
    // never detected.
    let request_id = request_id_of(&body);

    // The body is read before the header is resolved, because "what does this
    // body mean without a session?" is a question about the body. It is asked
    // only when the header is absent, and asking it is the same as not
    // consulting the store — an absent header never reaches a lookup.
    let session = match Header::read(&headers, &SESSION_HEADER) {
        Header::Absent => match state.handler.unsessioned(&body, &headers) {
            Unsessioned::Establishing => SessionHandle::Establishing,
            Unsessioned::Stateless => SessionHandle::Stateless,
            Unsessioned::NeedsSession => {
                return refuse_with_id_and_challenge(
                    state.config.challenge().as_deref(),
                    Refusal::MissingSession,
                    request_id.as_ref(),
                );
            }
        },
        Header::Value(_) | Header::Unreadable => match resolve_session(&state.sessions, &headers) {
            Ok(session) => session,
            Err(refusal) => {
                return refuse_with_id_and_challenge(
                    state.config.challenge().as_deref(),
                    refusal,
                    request_id.as_ref(),
                );
            }
        },
    };

    // The version header is parsed before anything is dispatched, so a client
    // that wrote a broken one is refused without the request running. What it
    // *disagrees* with is not decided here: that is a comparison against the
    // session's negotiated revision, and only the protocol layer knows it.
    //
    // A stateless request is the exception. Its header is compared with the
    // version its own `_meta` names, and a version this build cannot parse is
    // still a version the client asked for — answered with the revision's
    // `UnsupportedProtocolVersionError` echoing it, which the protocol layer
    // can only do if the request reaches it.
    let protocol = match read_protocol(&headers) {
        Ok(protocol) => protocol,
        Err(_) if matches!(session, SessionHandle::Stateless) => None,
        Err(refusal) => {
            return refuse_with_id_and_challenge(
                state.config.challenge().as_deref(),
                refusal,
                request_id.as_ref(),
            );
        }
    };

    let call = Inbound {
        context: &context,
        session,
        body: &body,
        protocol,
        headers: &headers,
        streaming: state.config.streaming(),
    };
    match state.handler.dispatch(call).await {
        Ok(response) => stamp_media_type(response),
        Err(refusal) => refuse_with_id_and_challenge(
            state.config.challenge().as_deref(),
            refusal,
            request_id.as_ref(),
        ),
    }
}

/// Names the media type of a handler's answer.
///
/// The handler produced a *body*; saying how that body is framed is the
/// transport's job, not the protocol layer's. Leaving it to the handler means
/// every handler has to remember, and a handler that forgets answers `200`
/// with no media type at all — which is a response any conformant client is
/// entitled to refuse, so the bug shows up as a peer that "does not work"
/// against this server and nowhere else.
///
/// Only a `2xx` is stamped. A refusal already carries its own
/// `application/json` and its own status, and overwriting a refusal's media
/// type would be a second, quieter version of the same mistake.
///
/// A body the handler framed itself is left alone: a handler that deliberately
/// answers with a stream has set `text/event-stream`, and overwriting that
/// with JSON would turn a working streaming response into a parse error on the
/// client.
fn stamp_media_type(response: Response) -> Response {
    if !response.status().is_success() {
        return response;
    }
    let already_framed = response
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|value| {
            let raw = value.as_bytes();
            raw.starts_with(b"text/event-stream") || raw.starts_with(b"application/json")
        });
    if already_framed {
        return response;
    }
    let mut response = response;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static(JSON_MEDIA_TYPE),
    );
    response
}

/// The `GET` route: open the session's stream, resuming from `Last-Event-ID`.
/// Serves the RFC 9728 document this router was configured to publish.
///
/// Deliberately does **not** consult the authenticator or the policy. §3.1
/// puts this document at a URL a client fetches *because* it could not
/// authenticate, so running the same gate that produced the `401` would make
/// the escape route require the credential it explains how to obtain. That is
/// not a weakening of the policy: the document names authorization servers and
/// scopes, and is exactly the content RFC 9728 §2 intends to be public.
async fn serve_document<S, H>(State(state): State<Arc<RouterState<S, H>>>) -> Response
where
    S: EventSource,
    H: RequestHandler<S>,
{
    let Some(metadata) = state.config.resource_metadata() else {
        // The route is only mounted when there is a document, so this arm is
        // unreachable through this router. Answering `404` rather than `500`
        // keeps it that way: if the route and the config ever disagree, a
        // client sees "no document here", which is what that is.
        return StatusCode::NOT_FOUND.into_response();
    };
    // Every field is a string, an `Arc<str>` or a `Vec` of those, so a failure
    // here is a bug in the derive rather than anything a caller configured. The
    // client gets a bodiless `500` because the wire contract is the document.
    let Ok(body) = serde_json::to_vec(metadata) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

async fn open_stream<S, H>(
    State(state): State<Arc<RouterState<S, H>>>,
    headers: HeaderMap,
) -> Response
where
    S: EventSource,
    H: RequestHandler<S>,
{
    if let Err(refusal) = authenticate(state.authenticator.as_ref(), &state.policy, &headers).await
    {
        return refuse_with_challenge(state.config.challenge().as_deref(), refusal);
    }

    // A `GET` is a stream for a session that already exists. There is no body
    // to inspect, so the header is not optional here.
    let session = match resolve_session(&state.sessions, &headers) {
        Ok(session) => session,
        Err(refusal) => return refuse_with_challenge(state.config.challenge().as_deref(), refusal),
    };
    let SessionHandle::Live { session, .. } = session else {
        // Unreachable: `resolve_session` only ever yields `Live`. Written as a
        // `let ... else` rather than a panic because this file's rules forbid
        // both, and a refusal is the honest answer if it is ever reached.
        return refuse_with_challenge(
            state.config.challenge().as_deref(),
            Refusal::Handler("the session lookup produced no session".to_owned()),
        );
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
        Header::Unreadable => {
            return refuse_with_challenge(
                state.config.challenge().as_deref(),
                Refusal::MalformedCursor,
            );
        }
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
        Err(error) => refuse_with_challenge(
            state.config.challenge().as_deref(),
            Refusal::Handler(error.to_string()),
        ),
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
    if let Err(refusal) = authenticate(state.authenticator.as_ref(), &state.policy, &headers).await
    {
        return refuse_with_challenge(state.config.challenge().as_deref(), refusal);
    }

    let id = match session_id(&headers) {
        Ok(Some(id)) => id,
        Ok(None) => {
            return refuse_with_challenge(
                state.config.challenge().as_deref(),
                Refusal::MissingSession,
            );
        }
        Err(refusal) => return refuse_with_challenge(state.config.challenge().as_deref(), refusal),
    };

    // The returned `Arc` is dropped at the end of this match arm, which is what
    // releases the session's tail and every waker parked on it. Nothing else in
    // the adapter drops a session, so this is the only path that frees one.
    match state.sessions.terminate(&id) {
        Ok(Some(_terminated)) => StatusCode::OK.into_response(),
        Ok(None) => {
            refuse_with_challenge(state.config.challenge().as_deref(), Refusal::UnknownSession)
        }
        Err(error) => refuse_with_challenge(
            state.config.challenge().as_deref(),
            Refusal::Handler(error.to_string()),
        ),
    }
}

/// Resolves the caller, and refuses it if the endpoint's policy says so.
///
/// # Errors
///
/// Whatever [`AuthPolicy::refuse`] returns: `401` for a caller that presented
/// nothing usable, `403` for one whose credential proved an identity without
/// the scope this endpoint names.
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

/// The one way a refusal becomes a response.
///
/// Every failure path in every handler ends here. It is a function rather than a
/// habit repeated at each call site because the alternative is the shape of bug
/// this adapter is built not to have: a handler that answers a refusal with
/// `StatusCode::OK` because it was in a hurry, on a path no test walks.
/// Renders a refusal as the response it is.
///
/// A `401` also carries the `WWW-Authenticate` challenge, because a
/// `401` without one is a dead end for the client: it knows it needs a
/// credential but not where the authorization server is, which is the one thing
/// a client that has just been handed an expired token cannot work out for
/// itself. RFC 9728 §5.1.
///
/// The `403` arm gets nothing. `insufficient_scope` and `ForeignOrigin` are
/// both 403s, and RFC 6750 §3.1 attaches a challenge to `401`, not `403` — a
/// `403` says this credential is fine and will not do, which re-authenticating
/// cannot change. Sending a challenge there would tell a correct client to do
/// the one thing that will not help.
/// The JSON-RPC id a body carries, if it carries one at all.
///
/// Total where the rest of the pipeline is fallible: a body that is a batch, a
/// reply shape, or not JSON at all simply has no id, and every caller already
/// has to handle "no id" because the earliest refusals run before any body
/// exists. Returning `None` for those keeps one answer for both cases instead
/// of two.
fn request_id_of(body: &[u8]) -> Option<WireId<'static>> {
    let decoded = decode(body).ok()?;
    let Frame::Single(envelope) = decoded else {
        return None;
    };
    envelope.id().map(WireId::to_owned)
}

fn refuse_with_challenge(challenge: Option<&str>, refusal: Refusal) -> Response {
    refuse_with_id_and_challenge(challenge, refusal, None)
}

/// [`refuse_with_challenge`], echoing the JSON-RPC id of the refused request.
///
/// `id` is the id read off a body this handler has already read. Passing it is
/// what stops a refusal that happened *after* the body arrived from answering
/// `id: null` — see [`Refusal::body_with_id`]. Everything that runs before the
/// body exists has nothing to pass and keeps answering `null`, which is what
/// JSON-RPC asks for in that case.
fn refuse_with_id_and_challenge(
    challenge: Option<&str>,
    refusal: Refusal,
    id: Option<&WireId<'_>>,
) -> Response {
    let status = refusal.status();
    let mut response = refusal.into_response_with_id(id);
    if status == StatusCode::UNAUTHORIZED
        && let Some(challenge) = challenge
        && let Ok(value) = HeaderValue::from_str(challenge)
    {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, value);
    }
    response
}

#[cfg(test)]
mod tests;
