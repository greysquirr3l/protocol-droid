//! The two routes the `2024-11-05` transport is defined as.
//!
//! # Why this is a separate router rather than a mode of `McpRouter`
//!
//! Because the two transports have different lifecycles, not different
//! configuration. `McpRouter` is one endpoint that opens a session, streams on
//! it, and terminates it, with the session named by a header on every request.
//! The legacy transport is **two** endpoints that each do exactly one thing:
//!
//! - `GET <sse>` opens a stream and sends the `endpoint` event. It never
//!   receives a message.
//! - `POST <message>?sessionId=…` receives one message. It never opens a
//!   stream, and its own response carries no answer.
//!
//! A `GET` that could also receive, or a `POST` that could also stream, is not
//! a configuration of `McpRouter` — it is a router where each route has two
//! behaviours and the tests must enumerate the cross product. Two handlers with
//! one job each is the smaller thing to be correct about.
//!
//! Where the session is *named* also differs, and not by choice. `McpRouter`
//! reads `Mcp-Session-Id`; this transport's specification has no such header,
//! and the id is in the query string of the URI the server itself advertised.
//! The two transports therefore need the same session lookup and different
//! parsing, which is why the lookup is reused and the parsing is not.
//!
//! # The `202` a `POST` answers with is a convention
//!
//! The specification does not say. It says the client `POST`s to the endpoint
//! the server advertised and the server sends messages on the SSE stream, and
//! those two sentences together imply the `POST`'s own response has nothing to
//! carry. Every implementation answers `202 Accepted`; none of them do so
//! because the text requires it.
//!
//! The convention is recorded here so it is not mistaken for a guarantee, and
//! so the part a client can actually branch on is stated instead: a `404` for
//! an unknown session, a `400` for a body that is not JSON or a missing
//! `sessionId`, a `403` for a foreign `Origin`. Those are the decisions a
//! client has to make, and unlike the success status they are choices this
//! server makes deliberately.

use std::collections::HashMap;
use std::str::FromStr as _;
use std::sync::Arc;

use axum::Router as AxumRouter;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use crate::adapters::primary::transport::http::limits::RateLimiter;
use crate::adapters::primary::transport::http::precondition::{authenticate, read_body};
use crate::adapters::primary::transport::http::{
    Anonymous, AuthPolicy, Authenticator, EventTail, InvalidHeaderValue, Refusal, ServerConfig,
    SessionStore, with_security,
};
use crate::adapters::primary::transport::legacy_sse::{
    LegacyDispatcher, LegacyFrame, LegacySession, LegacyTail, OriginPolicy,
};
use crate::domain::registry::AuthContext;
use crate::domain::{McpSessionId, MessageEndpoint, MessageEndpointError, Verdict};

/// The query parameter carrying the session id on the message endpoint.
///
/// A convention, not specification text — see the module docs. A constant
/// because a literal at the two sites that must agree (the URI the server
/// builds, the URI the server parses) is how a server advertises one parameter
/// and reads another, and a client then gets a `404` forever with nothing in
/// the logs to explain it.
pub const SESSION_PARAMETER: &str = "sessionId";

/// The `202` a `POST` answers with.
///
/// A convention, per the module docs. Chosen because it is the status that says
/// "accepted, answered elsewhere", which is exactly the relationship between a
/// `POST` and the stream that will carry the answer. A `200` would promise a
/// body the specification says does not exist.
const ACCEPTED: StatusCode = StatusCode::ACCEPTED;

/// The stream path when the operator names none.
pub const DEFAULT_STREAM_PATH: &str = "/sse";

/// The message path when the operator names none.
pub const DEFAULT_MESSAGE_PATH: &str = "/messages";

/// Why this transport could not be mounted.
///
/// Exists because axum's `Router::route` **panics** on a path that does not
/// start with `/`, and a server that aborts its own start-up on a
/// configuration mistake is a server an operator cannot diagnose from a log
/// line. Every variant here is a configuration that a human typed.
#[derive(Debug, thiserror::Error)]
pub enum RouteError {
    /// A configured security header is not a legal HTTP header value.
    #[error("a configured security header is not a legal HTTP header value")]
    Header(#[from] InvalidHeaderValue),
    /// A configured path could not be mounted or advertised.
    #[error("the {which} path `{path}` cannot be used: {reason}")]
    Path {
        /// Which of the two paths it was, so the message names the setting.
        which: &'static str,
        /// The value as configured, so it can be found in the file it was set in.
        path: String,
        /// Why it was refused.
        reason: MessageEndpointError,
    },
}

/// State every legacy handler needs.
struct LegacyState<D: LegacyDispatcher> {
    /// The message path, kept separate from [`ServerConfig::endpoint`] because
    /// this transport's configuration has two paths where `McpRouter` has one.
    ///
    /// A validated [`MessageEndpoint`] rather than the configured string,
    /// because the only thing this router does with it is append a query
    /// parameter, and a path that cannot take one has no business being here.
    message_path: MessageEndpoint,
    config: ServerConfig,
    sessions: Arc<SessionStore<LegacySession<D::Session>>>,
    dispatcher: Arc<D>,
    origins: Arc<OriginPolicy>,
    policy: AuthPolicy,
    authenticator: Arc<dyn Authenticator + Send + Sync>,
    rate_limiter: RateLimiter,
}

impl<D: LegacyDispatcher> LegacyState<D> {
    /// The `endpoint` event's payload for `id`.
    ///
    /// Infallible, because the path was validated by
    /// [`MessageEndpoint::bare_path`] when the router was assembled. That is
    /// where the check belongs: a path that could escape the origin, or one
    /// carrying its own query, is a *configuration* mistake, and finding it on
    /// the first `GET` would mean finding it in production. The server is
    /// about to tell a client "send your messages here" — a path that could
    /// point elsewhere makes that instruction a redirect — so the check is at
    /// assembly rather than left to the client that would act on it.
    fn message_endpoint(&self, id: McpSessionId) -> MessageEndpoint {
        self.message_path
            .with_query_parameter(SESSION_PARAMETER, &id.to_string())
    }
}

impl<D: LegacyDispatcher> Clone for LegacyState<D> {
    fn clone(&self) -> Self {
        Self {
            message_path: self.message_path.clone(),
            config: self.config.clone(),
            sessions: Arc::clone(&self.sessions),
            dispatcher: Arc::clone(&self.dispatcher),
            origins: Arc::clone(&self.origins),
            policy: self.policy,
            authenticator: Arc::clone(&self.authenticator),
            rate_limiter: self.rate_limiter.clone(),
        }
    }
}

/// The `2024-11-05` HTTP+SSE transport's two routes.
pub struct LegacyRouter<D: LegacyDispatcher> {
    message_path: String,
    stream_path: String,
    config: ServerConfig,
    sessions: Arc<SessionStore<LegacySession<D::Session>>>,
    dispatcher: Arc<D>,
    origins: Arc<OriginPolicy>,
    policy: AuthPolicy,
    authenticator: Arc<dyn Authenticator + Send + Sync>,
    rate_limiter: RateLimiter,
}

/// Written out rather than derived because `D` is not required to be `Debug`,
/// and a derive would add that bound to a type an implementor should be free
/// to make out of anything. The paths and the `Origin` count are in here
/// because those are the two things an operator gets wrong, and a `Debug` that
/// omits them is a `Debug` that cannot answer "what did this server serve?".
impl<D: LegacyDispatcher> core::fmt::Debug for LegacyRouter<D> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("LegacyRouter")
            .field("stream_path", &self.stream_path)
            .field("message_path", &self.message_path)
            .field("permitted_origins", &self.origins.permitted().count())
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl<D: LegacyDispatcher> LegacyRouter<D> {
    /// A router with no `Origin` permitted, so every `Origin` that is present
    /// is refused until the operator says which origins are served.
    ///
    /// See [`OriginPolicy`] for why the default denies rather than allows.
    #[must_use]
    pub fn new(
        config: ServerConfig,
        sessions: Arc<SessionStore<LegacySession<D::Session>>>,
        dispatcher: D,
    ) -> Self {
        Self {
            message_path: DEFAULT_MESSAGE_PATH.to_owned(),
            stream_path: DEFAULT_STREAM_PATH.to_owned(),
            rate_limiter: RateLimiter::new(*config.rate_limit()),
            config,
            sessions,
            dispatcher: Arc::new(dispatcher),
            origins: Arc::new(OriginPolicy::strict(None)),
            policy: AuthPolicy::Public,
            authenticator: Arc::new(Anonymous),
        }
    }

    /// Sets which `Origin`s this server answers.
    #[must_use]
    pub fn with_origins(mut self, origins: OriginPolicy) -> Self {
        self.origins = Arc::new(origins);
        self
    }

    /// Replaces the anonymous default with a real implementation, and sets the
    /// scope policy it is checked against.
    #[must_use]
    pub fn with_auth(
        mut self,
        policy: AuthPolicy,
        authenticator: Arc<dyn Authenticator + Send + Sync>,
    ) -> Self {
        self.policy = policy;
        self.authenticator = authenticator;
        self
    }

    /// Replaces the per-subject request allowance.
    ///
    /// Defaults to [`ServerConfig::rate_limit`]. Requests are counted against
    /// the session they name, and requests that name none (opening a stream)
    /// share one allowance.
    #[must_use]
    pub fn with_rate_limiter(mut self, limiter: RateLimiter) -> Self {
        self.rate_limiter = limiter;
        self
    }

    /// Overrides the path the `GET` opens a stream on.
    ///
    /// Not validated here, and that is deliberate. This path is checked by axum
    /// when the router is assembled, and a path that does not match produces a
    /// router serving nothing — a loud failure. The *message* path, the one
    /// that reaches a client as an instruction, is checked by
    /// `LegacyState::message_endpoint` where the value is built, because a
    /// message path that does not match produces a client posting into the
    /// void, which is not loud at all.
    #[must_use]
    pub fn with_stream_path(mut self, path: impl Into<String>) -> Self {
        self.stream_path = path.into();
        self
    }

    /// Overrides the path the `endpoint` event advertises for `POST`.
    #[must_use]
    pub fn with_message_path(mut self, path: impl Into<String>) -> Self {
        self.message_path = path.into();
        self
    }

    /// Assembles the axum router.
    ///
    /// # Errors
    ///
    /// [`RouteError::Header`] if a configured security header is not a legal
    /// HTTP value, for the same reason `McpRouter` fails there: a server that
    /// starts and then answers without its headers is worse than one that does
    /// not start. And [`RouteError::Path`] for a path that is not an absolute
    /// path — which axum would otherwise **panic** on, taking the process with
    /// it, rather than refusing to start.
    pub fn into_router(self) -> Result<AxumRouter, RouteError> {
        // Both paths are read out before either is moved into the state: the
        // state needs the message path to build the `endpoint` event, and the
        // router needs both to mount the routes.
        let Self {
            message_path: configured_message_path,
            stream_path,
            config,
            sessions,
            dispatcher,
            origins,
            policy,
            authenticator,
            rate_limiter,
        } = self;

        // Checked here rather than left to the two places that would otherwise
        // discover it. The stream path is handed to axum, which **panics** on
        // one that does not start with `/`; the message path is handed to a
        // client, which would post into the void. The same validator for both,
        // because the requirement is identical: an absolute path on this
        // server's own origin, carrying no query and no fragment, so a
        // `sessionId` appended to it *is* the query rather than part of one.
        //
        // Written out twice rather than looped. The two paths land in two
        // different shapes — one is mounted, one is stored in the state — and a
        // loop over a pair of `&str`s that then has to be unpacked again is
        // longer than the duplication it saves.
        MessageEndpoint::bare_path(&stream_path).map_err(|reason| RouteError::Path {
            which: "stream",
            path: stream_path.clone(),
            reason,
        })?;
        let message_path =
            MessageEndpoint::bare_path(&configured_message_path).map_err(|reason| {
                RouteError::Path {
                    which: "message",
                    path: configured_message_path.clone(),
                    reason,
                }
            })?;

        let security = config.security().clone();
        let state = Arc::new(LegacyState {
            message_path,
            config,
            sessions,
            dispatcher,
            origins,
            policy,
            authenticator,
            rate_limiter,
        });
        // Two `MethodRouter`s on two paths, and deliberately not one
        // `MethodRouter` taking both methods on both. A `POST` to the stream
        // path and a `GET` to the message path are both mistakes, and axum
        // answers each with a `405` naming the methods it does serve — a better
        // answer than one route trying to be both.
        let router = AxumRouter::new()
            .route(&stream_path, axum::routing::get(open_stream))
            .route(
                &configured_message_path,
                axum::routing::post(receive_message),
            )
            .with_state(state);
        Ok(with_security(router, &security)?)
    }
}

/// `GET` — open a stream, and send the `endpoint` event first.
///
/// The ordering is not a convention here. The `endpoint` frame is pushed onto
/// the tail before the response is built, and nothing can dispatch a `message`
/// until this handler has returned, the client has read the id out of the event
/// it was just sent, and that id has been posted back. A `message` frame cannot
/// precede the `endpoint` frame. The specification requires the `endpoint`
/// event first, and pushing it here makes that a property of the tail rather
/// than something every future handler has to remember.
async fn open_stream<D: LegacyDispatcher>(
    State(state): State<Arc<LegacyState<D>>>,
    headers: HeaderMap,
) -> Response {
    // The `GET` opens the stream and dispatches nothing, so the principal it
    // resolved has nowhere to go. It is still resolved, because the endpoint's
    // policy applies to opening a stream exactly as it does to posting a
    // message — an unauthenticated caller must not be able to open one.
    if let Err(response) = check_common(&state, &headers, None).await {
        return *response;
    }

    let tail: LegacyTail = EventTail::new(state.config.replay_capacity());
    // Taken before the session exists, because the dispatcher's state is part
    // of the value being stored and may need the id it will be stored under.
    let taken = state.sessions.next_id();
    let kept = state.dispatcher.open(taken, &tail);
    let id = match state
        .sessions
        .open_with_id(Arc::new(LegacySession::new(tail.clone(), kept)), taken)
    {
        Ok(id) => id,
        Err(error) => return refuse(Refusal::Handler(error.to_string())),
    };

    // Nothing between the session being registered and the endpoint event being
    // pushed can fail. That is deliberate, and it is why a `GET` cannot leak a
    // session: the earlier version built the endpoint URI here and had to
    // un-register the session when it could not, which is a second failure
    // path through the one handler that should not have any. Validating the
    // path at assembly moved the whole class of failure out of the request.
    tail.push(LegacyFrame::Endpoint(state.message_endpoint(id)));

    // `None` for the cursor, and no `Last-Event-ID` is read. This transport
    // defines no resumption, so a client that sends the header anyway is
    // ignored rather than obeyed: obeying it would replay from a cursor whose
    // meaning this transport never established, and the client's idea of what
    // it has already seen would silently change under it.
    match tail.plan(None).into_response(state.config.keep_alive()) {
        Ok(response) => response,
        Err(error) => refuse(Refusal::Handler(error.to_string())),
    }
}

/// `POST` — take one message, and put the answer on the stream.
async fn receive_message<D: LegacyDispatcher>(
    State(state): State<Arc<LegacyState<D>>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    body: Body,
) -> Response {
    // Kept rather than discarded: this is the request's principal, and
    // `dispatch` is the only thing downstream that can act on it.
    let auth = match check_common(
        &state,
        &headers,
        query.get(SESSION_PARAMETER).map(String::as_str),
    )
    .await
    {
        Ok(auth) => auth,
        Err(response) => return *response,
    };

    let Some(raw) = query.get(SESSION_PARAMETER) else {
        return refuse(Refusal::MissingSession);
    };
    // A malformed id is refused as its own kind rather than as "no such
    // session". The distinction is the operator's only clue when a client is
    // posting to the wrong URI: a `404` says "no such session" and sends them
    // looking at session lifetime, while this one says "that is not an id" and
    // sends them looking at the URI they built.
    let Ok(id) = McpSessionId::from_str(raw) else {
        return refuse(Refusal::MalformedSessionId);
    };
    let session = match state.sessions.lookup(&id) {
        Ok(Some(session)) => session,
        Ok(None) => return refuse(Refusal::UnknownSession),
        Err(error) => return refuse(Refusal::Handler(error.to_string())),
    };

    // The ceiling is the domain's, read once and converted once, for the reason
    // `McpRouter` does the same: a `u32` that does not fit a `usize` is a
    // target whose `usize` is narrower than 32 bits, and refusing every body is
    // the safe reading there rather than a limit that silently wraps.
    let Ok(limit) = usize::try_from(state.config.max_body().bytes()) else {
        return refuse(Refusal::Handler(
            "the configured body limit does not fit this platform".to_owned(),
        ));
    };
    let body = match read_body(body, limit).await {
        Ok(body) => body,
        Err(refusal) => return refuse(refusal),
    };

    // Parsed at the boundary, so that `Value` — the one untyped thing in this
    // file — is the shape the port declares rather than something the port has
    // to re-parse. The `jsonrpc` member is *not* checked here: whether this is a
    // valid JSON-RPC message for the negotiated revision belongs to the
    // dispatcher, which is the only thing that knows the revision.
    let frame: Value = match serde_json::from_slice(&body) {
        Ok(frame) => frame,
        Err(error) => {
            return refuse(Refusal::MalformedBody {
                reason: error.to_string(),
            });
        }
    };

    match state
        .dispatcher
        .dispatch(&auth, &id, session.state(), &frame)
        .await
    {
        Ok(replies) => {
            for reply in replies {
                session.tail().push(LegacyFrame::Message(reply));
            }
            (ACCEPTED, ()).into_response()
        }
        Err(refusal) => refuse(refusal),
    }
}

/// The checks both routes make, in the order a router must make them.
///
/// `Origin` first, before authentication. The point of the `Origin` check is to
/// refuse a *connection*, and a credential is not what makes a connection
/// acceptable. Authenticating first would let a hostile page drive the
/// authenticator, which on a deployment with a token check is a credential
/// oracle reachable from any page the operator's users visit.
async fn check_common<D: LegacyDispatcher>(
    state: &LegacyState<D>,
    headers: &HeaderMap,
    subject: Option<&str>,
) -> Checked {
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    if let Err(refusal) = state.origins.check(origin) {
        return Err(Box::new(refuse(refusal)));
    }
    // `Origin` is absent from every non-browser client, so on its own it
    // cannot catch a rebound DNS name. `Host` is set from the URL the client
    // dialled and cannot be forged by a page.
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    if let Err(refusal) = state.origins.check_host(host) {
        return Err(Box::new(refuse(refusal)));
    }
    // Before authentication, so a flood of bad credentials is counted too.
    if let Verdict::Limited { retry_after } = state
        .rate_limiter
        .check(subject.filter(|s| !s.is_empty()).unwrap_or("anonymous"))
    {
        return Err(Box::new(refuse(Refusal::RateLimited { retry_after })));
    }
    // The resolved context is the *return value*, not a discarded result.
    // This router authenticates on every route and there is no other path for
    // the principal to reach the dispatcher, so returning `Ok(())` here is what
    // made a legacy-SSE deployment authenticate every caller and then serve
    // them all as anonymous.
    match authenticate(state.authenticator.as_ref(), &state.policy, headers).await {
        Ok(auth) => Ok(auth),
        Err(refusal) => Err(Box::new(refuse(refusal))),
    }
}

/// Renders a refusal as the response it is.
fn refuse(refusal: Refusal) -> Response {
    refusal.into_response()
}

/// What the pre-checks leave behind: the caller, or the response that refuses
/// it.
///
/// [`Response`] is ~128 bytes, so a `Result<AuthContext, Response>` makes every
/// `?` on this path pay for a response wrapper it is about to unwrap. The
/// handlers each want to `return` the response, so the boxing is here rather
/// than at two call sites that would each have to remember it.
type Checked = Result<AuthContext, Box<Response>>;

#[cfg(test)]
mod tests;
