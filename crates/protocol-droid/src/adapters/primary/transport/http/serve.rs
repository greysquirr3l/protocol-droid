//! Serving MCP over Streamable HTTP, on a socket this crate bound.
//!
//! [`McpRouter`] builds an axum `Router`, and a `Router` is not a server. The
//! gap between the two is where a consumer's HTTP support actually lives:
//! somebody has to own a [`SessionStore`], mint ids for it, implement
//! [`RequestHandler`] against the real [`Dispatcher`], bind a listener, and
//! decide the bind address. Five decisions, of which one — the bind address —
//! is genuinely the caller's.
//!
//! This module makes the other four. It is the HTTP counterpart of
//! [`StdioBuilder`](crate::application::server::serve::StdioBuilder), and it
//! mirrors it deliberately: both resolve a **ceiling** before the peer's first
//! frame, both leave the ceiling overridable where the choice is real, and
//! neither asks the caller to name a protocol version.
//!
//! The dispatch sequence is the stdio loop's, not a second one. Decoding,
//! refusing a malformed frame, distinguishing `initialize` from everything
//! else, and choosing a JSON-RPC code for a refusal are all decisions the
//! crate makes elsewhere; a second copy here would be a second opinion on each
//! of them, and a second opinion is wrong in exactly the cases nobody tests.
//!
//! # What it deliberately does not do
//!
//! It does not terminate TLS. The bind address defaults to loopback, and a
//! loopback listener serving plain HTTP is a development shape rather than a
//! deployment one. Binding `0.0.0.0` is permitted — the right answer depends on
//! what terminates TLS in front of it, and that is not a fact this crate can
//! see — but a non-loopback bind is this server's responsibility to get right,
//! not something [`HttpBuilder::addr`] pretends to make safe.

use core::num::NonZeroUsize;
use std::net::SocketAddr;
use std::sync::Arc;

use serde_json::Value;

use crate::adapters::primary::transport::http::config::ServerConfig;
use crate::adapters::primary::transport::http::config::Streaming;
use crate::adapters::primary::transport::http::documents::{
    answer_of, id_as_i64, method_of, params_of, refusal_document, refusal_document_from_refusal,
    result_document,
};
use crate::adapters::primary::transport::http::exchange::{ExchangeTable, HttpExchange};
use crate::adapters::primary::transport::http::limits::RateLimiter;
use crate::adapters::primary::transport::http::refusal::Refusal;
use crate::adapters::primary::transport::http::router::{
    EventSource, Inbound, McpRouter, RequestHandler, SessionHandle, Unsessioned,
};
use crate::adapters::primary::transport::http::session_store::SessionStore;
use crate::adapters::primary::transport::http::stateless::{StatelessHeaders, StatelessRequest};
use crate::adapters::primary::transport::http::stream::{EventTail, Finishing};
use crate::adapters::primary::transport::http::{AuthPolicy, Authenticator};
use crate::adapters::primary::transport::origin::OriginPolicy;
use crate::application::mcp::request_meta::declares_version;
use crate::application::server::Server;
use crate::application::server::context::{PeerSession, RequestContext};
use crate::application::server::dispatch::Dispatcher;
use crate::application::server::{ListKind, ListSubscription};
use crate::application::session::IdSource;
use crate::application::session::frame_sink::{FrameSink, SinkError};
use crate::domain::feature::resolve;
use crate::domain::ir::JsonObject;
use crate::domain::jsonrpc::{Frame, WireId, decode};
use crate::domain::mcp::McpSessionId;
use crate::domain::session::{CancellationToken, ProgressToken, SessionId};
use crate::domain::{
    CapabilitySet, EventId, Negotiation, ProtocolVersion, RequestId, SupportedVersions,
    TransportKind,
};

/// The tail this session's server-initiated messages are published on.
pub(super) type Tail = EventTail<Value>;

/// Where an [`IdSource`] mints session ids from.
///
/// A named alias rather than a bare trait object repeated in two signatures, so
/// that a reader sees the choice being made once.
type Ids = Arc<dyn IdSource + Send + Sync>;

/// How many undelivered frames one session's tail retains.
///
/// A `GET` that follows a `POST` resumes from this ring, so a client that
/// disconnects and reconnects within the window gets the frames it missed.
/// Large enough to cover a reconnect, small enough that an idle session is not
/// holding a megabyte of `Value`s nobody will read.
/// The keep-alive interval a draining stream uses.
///
/// Zero, because a `POST`'s stream ends as soon as the tail is drained and a
/// keep-alive only matters to a stream that is staying open. A non-zero value
/// here would emit `:ping` frames into the middle of a response that is about
/// to close.
pub(super) const KEEP_ALIVE: core::time::Duration = core::time::Duration::ZERO;

pub(super) const TAIL_CAPACITY: NonZeroUsize = match NonZeroUsize::new(64) {
    Some(capacity) => capacity,
    // A const context cannot call `ok_or`, and `None` here would mean a zero,
    // which is not a capacity. Unreachable for any value above zero.
    None => NonZeroUsize::MIN,
};

/// What [`HttpBuilder::bind`] reports.
///
/// The address the OS chose rather than the one that was asked for, because
/// `127.0.0.1:0` is the ordinary way to ask for a free port and the address a
/// caller needs in order to connect is the one that was bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeReport {
    /// The address actually bound.
    pub addr: SocketAddr,
    /// The revision ceiling pre-handshake sessions were gated with.
    ///
    /// The *ceiling*, not a negotiated revision: no peer has connected yet.
    pub ceiling: ProtocolVersion,
}

/// Why an HTTP server could not be brought up.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// The address could not be listened on.
    #[error("could not bind {addr}: {source}")]
    Bind {
        /// What was asked for.
        addr: SocketAddr,
        /// What the OS said.
        #[source]
        source: std::io::Error,
    },

    /// The endpoint policy is not usable.
    #[error("the endpoint policy is not usable: {0}")]
    Endpoint(#[from] crate::adapters::primary::transport::http::config::EndpointError),

    /// The accept loop failed.
    #[error("the accept loop failed: {0}")]
    Accept(#[source] std::io::Error),

    /// The route table could not be built.
    ///
    /// In practice a configured security header that is not a legal HTTP
    /// header value, or the issue #11 fail-open combination of a wired
    /// authenticator and the default `AuthPolicy::Public` policy. Failing
    /// here rather than at the first request is the point: the alternative
    /// is a server that starts and then answers every request with a
    /// permission boundary that does not exist.
    #[error("the route table could not be built: {0}")]
    Routes(#[from] crate::adapters::primary::transport::http::router::RouterError),

    /// The legacy HTTP+SSE routes could not be mounted.
    ///
    /// A path that is not an absolute path on this origin, or a configured
    /// security header that is not a legal value.
    #[cfg(feature = "legacy-sse")]
    #[error("the legacy HTTP+SSE routes could not be mounted: {0}")]
    LegacyRoutes(#[from] crate::adapters::primary::transport::legacy_sse::RouteError),

    /// No session-id source is available in this build.
    #[error(
        "this build has no id source: build with `random-ids`, or pass one to \
         HttpBuilder::ids"
    )]
    NoIdSource,

    /// The system entropy pool could not be read.
    #[error("session ids could not be drawn: {reason}")]
    IdSource {
        /// What the entropy source said.
        reason: String,
    },
}

/// One session's state on an HTTP connection.
///
/// Holds the negotiated [`PeerSession`] beside the tail its server-initiated
/// messages go on, because the router is generic over the stored type and a
/// [`PeerSession`] alone cannot answer [`EventSource::tail`].
///
/// The peer starts at the ceiling profile and is replaced by
/// [`Dispatcher::handshake`]'s return once `initialize` arrives. That ordering
/// is not incidental: a [`RequestContext`] needs a `PeerSession` to exist before
/// dispatch, and the handshake is the call that produces one.
#[derive(Debug)]
pub struct HttpSession {
    /// The session's protocol state — the ceiling profile until `initialize`
    /// replaces it, and the negotiated one forever after.
    peer: PeerSession,
    /// Where this session's server-initiated messages are published.
    tail: Tail,
    /// The server-initiated requests this session is waiting on.
    ///
    /// Held per session rather than globally because the id a server-initiated
    /// request carries is minted per session: two sessions both asking the peer
    /// to sample both mint id `1`, and a shared table would hand the second
    /// answer to the first session's tool.
    exchange: ExchangeTable,
    /// This session's standing interest in list changes.
    ///
    /// Held here so the session and the subscription end together: the store
    /// dropping an expired or deleted session drops this guard, and the
    /// fan-out stops writing to a tail nobody reads. `None` on a session that
    /// has not finished its handshake, because nothing has been negotiated yet
    /// that could gate a notification.
    _lists: Option<ListSubscription>,
}

impl EventSource for HttpSession {
    fn tail(&self) -> &Tail {
        &self.tail
    }
}

impl HttpSession {
    /// This session's peer, for a handler that reads it off a stored session.
    fn peer_of(session: &Arc<Self>) -> PeerSession {
        session.peer.clone()
    }

    /// This session's peer.
    ///
    /// The same state as [`Self::peer_of`] and for the same reason: a task
    /// that outlives the request it was spawned for has to keep reading the
    /// *session's* profile rather than a borrow of a body that is gone.
    pub fn peer(&self) -> PeerSession {
        self.peer.clone()
    }
}

/// The handler that bridges [`McpRouter`] to the real [`Dispatcher`].
///
/// The port trait belongs to the router, so this type implements it and
/// declares nothing. There is no second "how to answer a request" here — only
/// [`Dispatcher`] with an HTTP envelope around it.
pub struct HttpHandler {
    /// The protocol layer. Shared, because the router is cheap to clone and the
    /// dispatcher is not.
    pub(super) dispatcher: Arc<Dispatcher>,
    /// This server's sessions. The handler owns the store rather than being
    /// handed one per request because *minting* is a protocol-layer decision:
    /// the router can look a session up but cannot decide what a session is.
    pub(super) sessions: Arc<SessionStore<HttpSession>>,
    /// The revision to gate a session with before it has been negotiated.
    ceiling: ProtocolVersion,
}

impl core::fmt::Debug for HttpHandler {
    /// Prints the ceiling and nothing else.
    ///
    /// Handwritten because the store it holds is a table of session ids, and
    /// this project does not put a full session id in a log line. A derived
    /// `Debug` would make a `{:?}` in a log statement a way around that.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("HttpHandler")
            .field("ceiling", &self.ceiling)
            .finish_non_exhaustive()
    }
}

impl HttpHandler {
    /// A handler over `dispatcher` and `sessions`, gated at `ceiling`.
    #[must_use]
    pub const fn new(
        dispatcher: Arc<Dispatcher>,
        sessions: Arc<SessionStore<HttpSession>>,
        ceiling: ProtocolVersion,
    ) -> Self {
        Self {
            dispatcher,
            sessions,
            ceiling,
        }
    }

    /// The revision a session is gated with before it is negotiated.
    #[must_use]
    pub const fn ceiling(&self) -> ProtocolVersion {
        self.ceiling
    }

    /// The session a request arrives on, or a fresh one to handshake on.
    ///
    /// A fresh session is seeded at the ceiling profile rather than at the
    /// peer's answer, because `initialize` is the request that *produces* the
    /// negotiated peer: there is nothing to read until it has run. The same
    /// ordering the stdio loop uses, and for the same reason.
    ///
    /// The principal is deliberately *not* applied here. It is the request's
    /// rather than the session's, because the router resolved it from the
    /// credentials this request presented and a bearer token can be refreshed,
    /// replaced, or withdrawn between two requests on one session. Every
    /// visibility gate reads `ctx.peer().auth()` — `Dispatcher` for the
    /// listings, `RegistryServer::call` for the call path — so a peer that
    /// never received the request's context is anonymous to all of them, and a
    /// scope-gated primitive is invisible to the very caller who earned it.
    fn peer_for(&self, stored: Option<&Arc<HttpSession>>) -> PeerSession {
        stored.map_or_else(|| self.seed().peer, HttpSession::peer_of)
    }

    /// A session at the ceiling, for a connection that has not handshaken.
    fn seed(&self) -> HttpSession {
        Self::session_for(self.ceiling, CapabilitySet::empty())
    }

    /// A peer resolved for `version` and what the client said it supports.
    ///
    /// Shared by the two callers that need a session with no handshake behind
    /// it: the ceiling seed for a request that has not negotiated, and the
    /// stateless peer, which differs only in that it knows its revision and its
    /// capabilities. Building both from one function is what keeps the
    /// stateless path from quietly acquiring a difference of its own.
    pub(super) fn session_for(version: ProtocolVersion, client_caps: CapabilitySet) -> HttpSession {
        let profile = resolve(Negotiation {
            version,
            client_caps,
            server_caps: CapabilitySet::all_for(version),
            transport: TransportKind::StreamableHttp,
        });
        let tail = Tail::new(TAIL_CAPACITY);
        let exchange = ExchangeTable::new();
        let sink = Arc::new(TailSink { tail: tail.clone() });
        HttpSession {
            peer: PeerSession::new(
                version,
                profile,
                SessionId::new(1),
                TransportKind::StreamableHttp,
                Arc::new(HttpExchange::new(exchange.clone(), tail.clone())),
                sink,
            ),
            tail,
            exchange,
            _lists: None,
        }
    }
    /// Answers a request beside a stream that is already open.
    ///
    /// A handler that publishes *and waits* — a tool that asks the peer to
    /// sample or to elicit — cannot be answered the way a handler that only
    /// publishes is. Awaiting it inline means the response is built after the
    /// tool returns, and the tool returns after the peer answers, and the peer
    /// answers after it reads a request sitting in a stream this method has
    /// not built yet. A deadlock whose symptom is a client that waits forever
    /// for a request it will never be sent.
    ///
    /// So the stream is built *first* and the handler runs beside it. The
    /// tool's request is on the wire within milliseconds, the peer's answer
    /// arrives on its own `POST`, and the handler finishes and signals, at
    /// which point the stream ends.
    ///
    /// Separate from the caller's own dispatch because the two shapes do not
    /// share a tail: this one needs the stream before the handler runs, and
    /// the other needs the handler before the stream.
    ///
    /// # Errors
    ///
    /// [`Refusal::Handler`] if the stream cannot be assembled.
    fn respond_beside_stream(
        &self,
        request_peer: &PeerSession,
        session: &Arc<HttpSession>,
        id: Option<&WireId<'static>>,
        progress: Option<ProgressToken>,
        method: &str,
        params: &JsonObject,
    ) -> Result<axum::response::Response, Refusal> {
        let tail = session.tail.clone();
        // Where the stream stood before this request ran, so it carries this
        // request's frames and not the exchange that opened the session.
        let since = session.tail.last().to_string();
        let finished = Finishing::new();
        let response = tail
            .plan(Some(since.as_str()))
            .into_finishable_response(finished.clone(), KEEP_ALIVE)
            .map_err(|error| Refusal::Handler(error.to_string()))?;

        let dispatcher = Arc::clone(&self.dispatcher);
        // The whole peer is carried, not just the session's. The caller's
        // principal lives on the *request's* peer — the router resolved it from
        // the credentials this request presented — and rebuilding from the
        // session would answer as though nobody had authenticated. Every
        // visibility gate reads `ctx.peer().auth()`, so the cost of that is a
        // scope-gated primitive invisible to the caller who earned it.
        let owned_peer = request_peer.clone();
        let owned_id = id.map(WireId::to_owned);
        let owned_method = method.to_owned();
        let owned_params = params.clone();
        let tail_for_result = tail;
        let finished_for_task = finished;
        let task = tokio::spawn(async move {
            let peer = owned_peer;
            let context = RequestContext::for_request_with_state(
                &peer,
                RequestId::from_wire(owned_id.as_ref()),
                CancellationToken::new(),
                progress,
                dispatcher.state().clone(),
            );
            let document = match dispatcher
                .dispatch(&context, &owned_method, &owned_params)
                .await
            {
                Ok(result) => match result_document(owned_id.as_ref(), &result) {
                    Ok(document) => document,
                    // A refusal here is this server's own data failing to
                    // serialise, reported as a JSON-RPC error rather than
                    // dropped: a `POST` whose handler has already run cannot
                    // take an HTTP status for it any more.
                    Err(refusal) => refusal_document_from_refusal(owned_id.as_ref(), &refusal),
                },
                Err(error) => refusal_document(owned_id.as_ref(), &error),
            };
            tail_for_result.push(document);
            finished_for_task.fire();
        });

        // The stream owns the handler's lifetime from here: dropping the
        // response drops the body, which drops the handle the task holds. The
        // task is already running, so returning does not wait for it.
        let _task = task;
        Ok(response)
    }

    /// Routes a peer's answer to the tool waiting for it, and acknowledges.
    ///
    /// An answer is a JSON-RPC message with an id and no method. It is
    /// recognised rather than dispatched because there is nothing to dispatch
    /// it to: the tool that asked the question is parked on the answer, and
    /// routing it down the request path fails to find a method, answers the
    /// peer with an error, and leaves the tool waiting for an answer the server
    /// has already decided does not exist.
    ///
    /// The acknowledgement is `202 Accepted` with no body, which is what the
    /// specification asks of a response to a response — and the same answer is
    /// given whether the answer found its waiter or not, because a reply to an
    /// unknown request is discarded and inventing a request to attach the
    /// discard to would be worse.
    fn acknowledge_answer(
        body: &[u8],
        session: Option<&Arc<HttpSession>>,
        id: Option<&WireId<'static>>,
    ) -> Result<axum::response::Response, Refusal> {
        if let (Some(session), Some(id)) = (session, id)
            && let Some(numeric) = id_as_i64(id)
        {
            session.exchange.resolve(numeric, answer_of(body));
        }
        axum::response::Response::builder()
            .status(axum::http::StatusCode::ACCEPTED)
            .body(axum::body::Body::empty())
            .map_err(|_| Refusal::Handler("the acknowledgement did not build".to_owned()))
    }
}

impl RequestHandler<HttpSession> for HttpHandler {
    /// What a body means when the request named no session.
    ///
    /// Three arms, and which one a body falls in is a wire-format question
    /// this method can answer on its own:
    ///
    /// - `initialize` opens a session. It is the only method that does.
    /// - A body whose `params._meta` names
    ///   [`PROTOCOL_VERSION_KEY`](crate::application::mcp::request_meta::PROTOCOL_VERSION_KEY)
    ///   is a SEP-2575 stateless request. It carries its own revision and its
    ///   own capabilities, so it needs no session and is served against its
    ///   own profile.
    /// - Anything else is a stateful call missing its header, which the router
    ///   answers with a `400` and never hands here — so returning
    ///   [`Unsessioned::NeedsSession`] for it is not a guess, it is the arm
    ///   that produces that `400`.
    ///
    /// The version key is what makes a body self-describing, and it is the
    /// only member that can: capabilities without a revision would not say
    /// which gates the answer is read against.
    ///
    /// This runs on every sessionless POST, so the check is a lookup and not a
    /// parse — a full [`RequestMeta::read`] would decode the whole body only
    /// for a caller with a session header to throw it away.
    ///
    /// [`RequestMeta::read`]: crate::application::mcp::request_meta::RequestMeta::read
    fn unsessioned(&self, body: &[u8], headers: &axum::http::HeaderMap) -> Unsessioned {
        // Stateless first. A body naming its own revision, or a request whose
        // `MCP-Protocol-Version` is a revision with no sessions, is served by
        // the stateless lifecycle — including an `initialize`, which that
        // lifecycle answers with `-32601` rather than by opening a session the
        // client's own revision says do not exist.
        if declares_own_version(body) || StatelessHeaders::names_stateless_revision(headers) {
            return Unsessioned::Stateless;
        }
        match method_of(body) {
            Some(method) if method == "initialize" => Unsessioned::Establishing,
            _ => Unsessioned::NeedsSession,
        }
    }

    /// Runs one request, and answers it.
    ///
    /// The refusal channel is a [`Refusal`]: there is no other, and an adapter
    /// that cannot answer a request has exactly one honest way to say so. Note
    /// that a *protocol* refusal — an unknown method, a tool that does not
    /// exist — is not one. Those are answered in the body with a JSON-RPC error
    /// object, because the peer asked a well-formed question and the answer is
    /// that no such thing exists.
    fn dispatch(
        &self,
        call: Inbound<'_, HttpSession>,
    ) -> impl core::future::Future<Output = Result<axum::response::Response, Refusal>> + Send {
        // Everything the handler needs is copied out of `call` before the
        // future, because `Inbound` borrows the body and the router's `Arc`,
        // neither of which outlives this function.
        let body = call.body.to_vec();
        let protocol = call.protocol;
        // Captured here because the framing decision below happens after the
        // body is owned, and this is the only record of how the server was
        // configured to answer. See [`Streaming`].
        let streaming = call.streaming;
        let opening = matches!(call.session, SessionHandle::Establishing);
        let stateless = matches!(call.session, SessionHandle::Stateless);
        let stored = match &call.session {
            SessionHandle::Establishing | SessionHandle::Stateless => None,
            SessionHandle::Live { session, .. } => Some(Arc::clone(session)),
        };
        // The principal is this request's, not the session's — see
        // `peer_for`, where the reasoning lives.
        //
        // A stateless request's peer is *not* built here, because there is
        // nothing yet to build it from: the revision and the capabilities both
        // live in this request's `_meta`, and the body has not been decoded
        // until the future below runs. The seed peer stands in for that case
        // and is replaced before anything reads it.
        let auth = call.context.clone();
        let peer = self.peer_for(stored.as_ref()).with_auth(auth.clone());
        // Copied now for the same reason as the body: the header map is the
        // router's, and a stateless request is checked against it after the
        // body has been decoded.
        let stateless_headers = stateless.then(|| StatelessHeaders::read(call.headers));

        async move {
            // Decoded once. A body that is a batch, or a reply shape, or
            // unreadable, is the router's business and is refused here rather
            // than answered with a second opinion about what it meant.
            let decoded = decode(&body).map_err(|error| Refusal::MalformedBody {
                reason: error.to_string(),
            })?;
            let Frame::Single(envelope) = decoded else {
                return Err(Refusal::MalformedBody {
                    reason: "an HTTP request body carries one message".to_owned(),
                });
            };
            // Where the session's event stream stands *before* this request
            // runs. Everything the handler publishes from here on is this
            // request's answer; everything before it is a previous exchange's,
            // and replaying that here is how a `tools/list` answers with the
            // `initialize` frame that opened the session.
            let cursor: Option<EventId> = stored.as_ref().map(|session| session.tail.last());
            // Owned before anything else, because a handler may outlive this
            // frame: the request body is dropped when this function returns,
            // and a task still running needs the id it was answering.
            let id: Option<WireId<'static>> = envelope.id().map(WireId::to_owned);

            // An answer to a request *this* server made: a JSON-RPC message
            // with an id and no method. It is recognised here rather than
            // dispatched, because there is nothing to dispatch it to.
            if method_of(&body).is_none() {
                return Self::acknowledge_answer(&body, stored.as_ref(), id.as_ref());
            }

            let params = params_of(&envelope)?;
            let method = envelope.method_str().unwrap_or_default();

            // SEP-2575: a stateless request is served from its own `_meta`, by
            // a path that shares nothing with a session — see `stateless`.
            if let Some(headers) = stateless_headers {
                return self
                    .serve_stateless(StatelessRequest {
                        id,
                        method: method.to_owned(),
                        params,
                        headers,
                        auth,
                        streaming,
                    })
                    .await;
            }

            // The progress token lives in `params._meta.progressToken`, which
            // is where the specification puts it and not beside the request id.
            // Not reading it is a quiet failure: `report_progress` answers
            // `NoProgressToken` and the client that asked to be told waits for
            // updates from a server that believed nobody had asked.
            let progress: Option<ProgressToken> =
                crate::application::mcp::request_meta::progress_token(&params);
            let peer_for_now = peer;
            // Held only as long as this body. The task below builds its own
            // from the stored session, because it outlives both of them.
            let context = RequestContext::for_request_with_state(
                &peer_for_now,
                RequestId::from_wire(id.as_ref()),
                CancellationToken::new(),
                progress.clone(),
                Arc::clone(self.dispatcher.state()),
            );

            // `initialize` is the one method that changes the session rather
            // than being answered from it: the handshake resolves a profile for
            // the revision the client offered, and every gate after this point
            // has to read *that* one. A peer that negotiated `2024-11-05` must
            // not be served on a profile resolved for whatever the process was
            // built with.
            let (document, negotiated) = if method == "initialize" {
                match self.dispatcher.handshake(&context, &params) {
                    Ok((result, peer)) => (result_document(id.as_ref(), &result)?, Some(peer)),
                    Err(error) => (refusal_document(id.as_ref(), &error), None),
                }
            } else if let Some(session) = stored.as_ref() {
                // A sessioned request is answered beside an already-open
                // stream, which is the only shape that survives a handler that
                // publishes *and waits* — see [`Self::respond_beside_stream`].
                //
                // Only when this server was configured to frame a stream at
                // all. Under `Streaming::Json` the documented default, the
                // reply is one JSON document: the stream is built and drained
                // into it before the response exists, which is why a tool that
                // elicits cannot work here and says so rather than hanging —
                // see [`ExchangeTable::is_readable`].
                if streaming == Streaming::EventStream {
                    return self.respond_beside_stream(
                        &peer_for_now,
                        session,
                        id.as_ref(),
                        progress,
                        method,
                        &params,
                    );
                }
                match self.dispatcher.dispatch(&context, method, &params).await {
                    Ok(result) => (result_document(id.as_ref(), &result)?, None),
                    Err(error) => (refusal_document(id.as_ref(), &error), None),
                }
            } else {
                match self.dispatcher.dispatch(&context, method, &params).await {
                    Ok(result) => (result_document(id.as_ref(), &result)?, None),
                    Err(error) => (refusal_document(id.as_ref(), &error), None),
                }
            };

            // A notification wants no answer, and a notification cannot open a
            // session either — there is no id to put in the response header.
            if id.is_none() {
                return Self::response(&Value::Null, None, None);
            }

            // The header is checked against the session's own revision, which
            // is why this is asked of the peer rather than of the raw header.
            if let Some(agreed) = negotiated.as_ref().map(PeerSession::negotiated) {
                check_header(protocol, agreed)?;
            }

            if opening {
                return self.open_session(negotiated, stored.as_deref(), streaming, &document);
            }

            // The tail is handed over only when this server was configured to
            // frame a stream. Under `Streaming::Json` the reply is one
            // document with nothing to drain into, and passing the tail would
            // answer with an event stream regardless — which is exactly how the
            // default came to mean a bare document on `initialize` and a stream
            // on everything after it.
            Self::response(
                &document,
                None,
                if streaming == Streaming::EventStream {
                    stored.map(|session| (session.tail.clone(), cursor))
                } else {
                    None
                },
            )
        }
    }
}

impl HttpHandler {
    /// Mints the session an `initialize` establishes, and answers with its id.
    ///
    /// Split out of [`RequestHandler::dispatch`] because opening a session is
    /// one concern with one rule in it — what this session may be asked, and
    /// what may be published to it — and inlining that into the request loop
    /// left the rule sitting behind forty lines of session bookkeeping.
    ///
    /// # Errors
    ///
    /// [`Refusal::Handler`] if `initialize` succeeded without producing a peer
    /// to store, if the store refuses the session, or if the answer will not
    /// serialise.
    fn open_session(
        &self,
        negotiated: Option<PeerSession>,
        stored: Option<&HttpSession>,
        streaming: Streaming,
        document: &Value,
    ) -> Result<axum::response::Response, Refusal> {
        // `handshake` narrows the context it was handed, so the peer it
        // returns carries the *establishing request's* principal. Every
        // request resolves its own principal above, so nothing downstream
        // reads this one — which is exactly why it is worth dropping rather
        // than storing: session state outlives the credential that produced
        // it, and a principal recorded there has the wrong lifetime for a
        // token that can be refreshed, replaced, or withdrawn mid-session.
        let peer = negotiated
            .ok_or_else(|| Refusal::Handler("a session was opened without a handshake".to_owned()))?
            .without_auth();
        // The tail the router streams to the client, and the sink the handler
        // writes through, are the same ring. Binding the peer to a *different*
        // one is the quiet failure this shape invites: the session's own tail
        // stays empty forever while the peer dutifully publishes into a ring
        // no reader ever drains, so a tool that logs looks like a tool that
        // logs nothing.
        //
        // Whether anything drains that ring is the framing decision, so it is
        // recorded on the table here rather than left for a tool to discover by
        // hanging. A session opened under `Streaming::Json` is never drained,
        // because every reply it gives is one document.
        let readable = streaming == Streaming::EventStream;
        let (tail, exchange) = stored.map_or_else(
            || {
                let exchange = if readable {
                    ExchangeTable::readable()
                } else {
                    ExchangeTable::new()
                };
                (Tail::new(TAIL_CAPACITY), exchange)
            },
            |session| (session.tail.clone(), session.exchange.clone()),
        );
        // The store allocates the id below, so it is read back from the minted
        // handle rather than allocated twice — and the peer is bound to it
        // before anything can key on it.
        let sink: Arc<dyn crate::application::session::DynFrameSink> =
            Arc::new(TailSink { tail: tail.clone() });
        // Every list notification the negotiated profile allows. A session
        // that did not negotiate `listChanged` for a kind is filtered by the
        // fan-out's own gate, so subscribing to all three is not a promise.
        let lists = self.dispatcher.server().handle().subscribe(
            &sink,
            peer.profile(),
            &ListKind::ALL,
            None,
        );
        let peer = peer
            .with_sink(sink)
            // Bound to *this* session's table, for the same reason the sink is
            // bound to this session's tail: the peer outlives the seed it was
            // built from, and an exchange on the wrong table publishes a
            // request nobody on this session is waiting to answer.
            .with_exchange(Arc::new(HttpExchange::new(exchange.clone(), tail.clone())));
        let id_source = self.sessions.next_id();
        let peer = peer.with_session(id_source);
        let minted = self
            .sessions
            .open_with_id(
                Arc::new(HttpSession {
                    peer,
                    tail,
                    exchange,
                    _lists: Some(lists),
                }),
                id_source,
            )
            .map_err(|error| Refusal::Handler(format!("no session: {error}")))?;
        Self::response(document, Some(&minted), None)
    }

    /// The answer document, with the session header when one was minted.
    ///
    /// # Errors
    ///
    /// [`Refusal::Handler`] if the document will not serialise or the minted id
    /// will not become a header value. Both are this server's own data, which
    /// is why they are not reported as the peer's fault.
    fn response(
        document: &Value,
        id: Option<&McpSessionId>,
        tail: Option<(Tail, Option<EventId>)>,
    ) -> Result<axum::response::Response, Refusal> {
        // A session that may have published is answered with its event stream,
        // and the reply goes *into* that stream rather than beside it.
        //
        // The alternative — streaming the handler's notifications and putting
        // the JSON-RPC result in the body — is a response with two answers in
        // two places, and which one a client reads as the result depends on the
        // content type. The same tool call would resolve or hang purely on how
        // the peer asked. One stream, frames in the order they happened: the
        // notifications the call emitted, then its result.
        if let Some((tail, cursor)) = tail {
            tail.push(document.clone());
            // The cursor, as the `Last-Event-ID` spelling `plan` reads. Held in
            // a binding rather than built inline because the reference has to
            // outlive the call.
            let since = cursor.map(|id| id.to_string());
            return match tail
                .plan(since.as_deref())
                .into_draining_response(KEEP_ALIVE)
            {
                Ok(response) => {
                    let mut response = response;
                    if let Some(id) = id {
                        let value = axum::http::HeaderValue::from_str(&id.to_string()).map_err(
                            |error| Refusal::Handler(format!("the id was not a header: {error}")),
                        )?;
                        response.headers_mut().insert("mcp-session-id", value);
                    }
                    Ok(response)
                }
                Err(error) => Err(Refusal::Handler(error.to_string())),
            };
        }

        let encoded = serde_json::to_vec(document)
            .map_err(|error| Refusal::Handler(format!("the reply did not serialise: {error}")))?;
        let mut response = axum::response::Response::new(axum::body::Body::from(encoded));
        if let Some(id) = id {
            let value = axum::http::HeaderValue::from_str(&id.to_string())
                .map_err(|error| Refusal::Handler(format!("the id was not a header: {error}")))?;
            response.headers_mut().insert("mcp-session-id", value);
        }
        Ok(response)
    }
}

/// Checks the `MCP-Protocol-Version` header against what the session agreed.
///
/// Only after a handshake, because before one the session has agreed to nothing
/// and there is no answer to compare against.
fn check_header(
    header: Option<ProtocolVersion>,
    negotiated: ProtocolVersion,
) -> Result<(), Refusal> {
    let sent = header.map(ProtocolVersion::as_wire);
    crate::domain::check_protocol_header(negotiated, sent.as_deref()).map_err(|error| {
        Refusal::ProtocolHeader {
            reason: error.to_string(),
        }
    })
}

/// Whether a body is a SEP-2575 stateless request.
///
/// The test is the presence of the version key in `params._meta`, which is
/// what makes a request carry its own revision rather than borrow a session's.
/// A batch, a reply, or a body that will not decode has no params to look in
/// and is not a stateless request; the caller's own decode will say why.
fn declares_own_version(body: &[u8]) -> bool {
    let Ok(decoded) = decode(body) else {
        return false;
    };
    let Frame::Single(envelope) = decoded else {
        return false;
    };
    let Some(params) = envelope.params() else {
        return false;
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(params.get()) else {
        return false;
    };
    declares_version(&parsed)
}

/// The sink a session's handler-initiated messages go out on.
///
/// Publishing to the session's tail is what makes a notification — a log
/// record, a progress increment — reach the client: the router streams the
/// tail as the `POST`'s response body, and anything a handler writes here
/// arrives as an SSE frame on that same request.
///
/// A sink that discarded every frame would fail quietly and loudly elsewhere: a
/// tool that logs correctly looks like a tool that logs nothing, and a client
/// waiting for three log records sees zero and reports the server as broken
/// rather than as unwired.
#[derive(Debug, Clone)]
pub(super) struct TailSink {
    pub(super) tail: Tail,
}

impl TailSink {
    /// Publishes one encoded frame, synchronously.
    ///
    /// # Errors
    ///
    /// [`SinkError`] when the frame is not JSON.
    pub(super) fn push(&self, frame: &str) -> Result<(), SinkError> {
        serde_json::from_str::<Value>(frame)
            .map(|value| {
                self.tail.push(value);
            })
            .map_err(|_| SinkError)
    }
}

impl FrameSink for TailSink {
    /// Publishes one frame to the session's tail.
    ///
    /// Parsed on the way in rather than trusted, because the tail holds
    /// `Value`: a frame that is not JSON has nothing to stream, and refusing it
    /// here leaves a caller that can still be told why.
    ///
    /// # Errors
    ///
    /// [`SinkError`] when the frame is not JSON.
    fn send(
        &self,
        frame: &str,
    ) -> impl core::future::Future<Output = Result<(), SinkError>> + Send {
        core::future::ready(self.push(frame))
    }
}

/// The builder behind [`serve_http`].
///
/// Every method is optional and every default is safe. The one decision this
/// builder does not make for the caller is which address to bind, and it
/// defaults that to loopback rather than to every interface.
pub struct HttpBuilder {
    /// The server whose tools and resources are served.
    server: Server,
    /// The address to listen on.
    addr: SocketAddr,
    /// The revision pre-handshake sessions are gated with.
    ceiling: ProtocolVersion,
    /// The endpoint policy and framing limits.
    ///
    /// Not defaulted here: [`ServerConfig::new`] parses the specification's
    /// endpoint and can fail, and a builder that cannot fail is worth more than
    /// one that resolves a constant early. An absent value is the crate's own
    /// defaults, resolved in [`serve`](Self::serve).
    config: Option<ServerConfig>,
    /// Where session ids come from.
    ids: Option<Ids>,
    /// The authenticator the router is built with.
    ///
    /// Absent means the router's default `Anonymous` authenticator; the
    /// `with_authenticator` setter records the choice so the route builder
    /// can pair it with the policy in the right order. See
    /// `McpRouter::with_authenticator` for the issue #11 fail-closed
    /// contract this is the consumer entry point for.
    authenticator: Option<Arc<dyn Authenticator + Send + Sync>>,
    /// The auth policy the router is built with.
    policy: Option<AuthPolicy>,
    /// The `Origin` allow-list the router enforces.
    origin_policy: Option<OriginPolicy>,
    /// The per-subject request allowance.
    rate_limiter: Option<RateLimiter>,
    /// Where the `2024-11-05` HTTP+SSE routes are mounted, when they are.
    #[cfg(feature = "legacy-sse")]
    legacy_sse: Option<LegacySsePaths>,
}

/// The two paths the legacy transport is mounted on.
#[cfg(feature = "legacy-sse")]
#[derive(Debug, Clone)]
struct LegacySsePaths {
    stream: String,
    message: String,
}

impl core::fmt::Debug for HttpBuilder {
    /// Prints what was configured and nothing else.
    ///
    /// Handwritten for the same reason as [`HttpHandler`]'s: a derived `Debug`
    /// would walk the [`Server`] and the id source, and neither belongs in a
    /// log line. The address and the ceiling are the two facts worth having
    /// when a serve has gone wrong.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("HttpBuilder")
            .field("addr", &self.addr)
            .field("ceiling", &self.ceiling)
            .field("config", &self.config)
            .field("ids_supplied", &self.ids.is_some())
            .finish_non_exhaustive()
    }
}

impl HttpBuilder {
    /// Serves `server`, on loopback unless told otherwise.
    ///
    /// The defaults are the crate's own strict ones rather than the permissive
    /// ones, so a builder that sets nothing is already safe: loopback, the
    /// newest handshake-capable revision, and a session id from the system
    /// entropy pool.
    #[must_use]
    pub fn new(server: Server) -> Self {
        Self {
            server,
            addr: SocketAddr::from(([127, 0, 0, 1], 3000)),
            ceiling: default_ceiling(),
            config: None,
            ids: None,
            authenticator: None,
            policy: None,
            origin_policy: None,
            rate_limiter: None,
            #[cfg(feature = "legacy-sse")]
            legacy_sse: None,
        }
    }

    /// Also serves the `2024-11-05` HTTP+SSE transport, beside Streamable HTTP.
    ///
    /// A client opens a stream with `GET /sse`, is told where to `POST` by the
    /// `endpoint` event, and speaks `2024-11-05` over the pair. The same
    /// tools, resources and prompts answer on both transports, and the
    /// authenticator, `Origin` policy and rate limit set on this builder apply
    /// to both. Use [`legacy_sse_at`](Self::legacy_sse_at) to move the paths.
    #[cfg(feature = "legacy-sse")]
    #[must_use]
    pub fn legacy_sse(self) -> Self {
        self.legacy_sse_at(
            crate::adapters::primary::transport::legacy_sse::DEFAULT_STREAM_PATH,
            crate::adapters::primary::transport::legacy_sse::DEFAULT_MESSAGE_PATH,
        )
    }

    /// [`legacy_sse`](Self::legacy_sse), on `stream` and `message` rather than
    /// `/sse` and `/messages`.
    ///
    /// Both must be absolute paths with no query; [`serve`](Self::serve)
    /// refuses to start otherwise.
    #[cfg(feature = "legacy-sse")]
    #[must_use]
    pub fn legacy_sse_at(mut self, stream: impl Into<String>, message: impl Into<String>) -> Self {
        self.legacy_sse = Some(LegacySsePaths {
            stream: stream.into(),
            message: message.into(),
        });
        self
    }

    /// The address to bind. Defaults to `127.0.0.1:3000`.
    ///
    /// A non-loopback address is permitted but not made safe: this server
    /// speaks plain HTTP, so whatever is in front of it has to terminate TLS
    /// for it to be exposed to a network rather than to a machine.
    #[must_use]
    pub const fn addr(mut self, addr: SocketAddr) -> Self {
        self.addr = addr;
        self
    }

    /// The revision to gate pre-handshake sessions with.
    #[must_use]
    pub const fn version(mut self, version: ProtocolVersion) -> Self {
        self.ceiling = version;
        self
    }

    /// The endpoint policy and framing limits.
    #[must_use]
    pub fn config(mut self, config: ServerConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Where session ids come from.
    ///
    /// Defaults to a counter from this crate's own source. Supply
    /// [`RandomIds`](crate::adapters::secondary::id_source::RandomIds) — behind
    /// the `random-ids` feature — for an id a peer cannot guess, which is the
    /// right choice anywhere a session id authorises anything.
    #[must_use]
    pub fn ids(mut self, ids: Ids) -> Self {
        self.ids = Some(ids);
        self
    }

    /// Replaces the anonymous default with a real implementation.
    ///
    /// Pair with [`with_policy`](Self::with_policy); a wired authenticator
    /// under the default `AuthPolicy::Public` is the silent fail-open the
    /// router's `into_router` refuses at mount time. The same contract
    /// applies here, surfaced one layer up: a deployment that sets this
    /// without `with_policy` will see the misconfiguration at
    /// [`serve`](Self::serve) rather than at the first request.
    #[must_use]
    pub fn with_authenticator(
        mut self,
        authenticator: Arc<dyn Authenticator + Send + Sync>,
    ) -> Self {
        self.authenticator = Some(authenticator);
        self
    }

    /// Replaces the default `AuthPolicy::Public` policy.
    ///
    /// A deployment that wired an authenticator above must also set
    /// `Require(scopes)` here, or `into_router` will refuse to build the
    /// route table. See `McpRouter::with_policy` for the matching
    /// contract.
    #[must_use]
    pub const fn with_policy(mut self, policy: AuthPolicy) -> Self {
        self.policy = Some(policy);
        self
    }

    /// Replaces the default `Origin` allow-list.
    ///
    /// The default is `OriginPolicy::strict(None)`, which refuses every
    /// `Origin` header sent and allows a request that sent none — see
    /// `OriginPolicy::check` for why absent is not the same as foreign.
    /// A deployment serving a browser over a non-loopback address will
    /// need to widen this to the page's origin.
    #[must_use]
    pub fn with_origin_policy(mut self, origin: OriginPolicy) -> Self {
        self.origin_policy = Some(origin);
        self
    }

    /// Replaces the per-subject request allowance.
    ///
    /// Defaults to `ServerConfig::rate_limit()`, so a configuration that
    /// set one does not have to set it twice.
    #[must_use]
    pub fn with_rate_limiter(mut self, limiter: RateLimiter) -> Self {
        self.rate_limiter = Some(limiter);
        self
    }

    /// Binds the listener without serving it.
    ///
    /// Split from serving because a caller terminating TLS itself needs the
    /// listener before anything is served on it, and because binding before
    /// accepting removes the window in which a connection is refused for a port
    /// this process is about to take.
    ///
    /// # Errors
    ///
    /// [`ServeError::Bind`] when the address is taken or unusable.
    pub async fn bind(self) -> Result<(tokio::net::TcpListener, ServeReport), ServeError> {
        let (listener, bound) = bind(self.addr).await?;
        let report = ServeReport {
            addr: bound,
            ceiling: self.ceiling,
        };
        Ok((listener, report))
    }

    /// Binds and serves until the accept loop fails.
    ///
    /// # Errors
    ///
    /// [`ServeError`] from building the route table, from binding, or from the
    /// accept loop itself. A protocol error is not one of those: a peer whose
    /// request this server cannot serve is answered on the wire rather than
    /// raised here.
    pub async fn serve(self) -> Result<ServeReport, ServeError> {
        let parts = routes(self)?;
        let (listener, bound) = bind(parts.addr).await?;
        axum::serve(listener, parts.router)
            .await
            .map_err(ServeError::Accept)?;
        Ok(ServeReport {
            addr: bound,
            ceiling: parts.ceiling,
        })
    }

    /// Serves on a listener the caller already bound.
    ///
    /// The other half of [`bind`](Self::bind), and the shape a deployment with
    /// TLS in front needs: bind, wrap the listener in a TLS acceptor, and serve
    /// on whatever that acceptor produces. Binding inside `serve` would make
    /// this impossible, because there would be no moment at which the raw
    /// listener was in the caller's hands.
    ///
    /// # Errors
    ///
    /// [`ServeError::Routes`] when the route table cannot be built, and
    /// [`ServeError::Accept`] when the loop does.
    pub async fn serve_on(
        self,
        listener: tokio::net::TcpListener,
    ) -> Result<ServeReport, ServeError> {
        let parts = routes(self)?;
        let bound = listener.local_addr().map_err(|source| ServeError::Bind {
            addr: parts.addr,
            source,
        })?;
        axum::serve(listener, parts.router)
            .await
            .map_err(ServeError::Accept)?;
        Ok(ServeReport {
            addr: bound,
            ceiling: parts.ceiling,
        })
    }
}

/// What `routes` returns: the assembled router and the two builder fields
/// the calling method still needs after the table is built.
#[derive(Debug)]
struct BuilderOutput {
    /// The mounted `axum::Router`.
    router: axum::Router,
    /// The address the caller is binding. Preserved for `ServeReport` and
    /// for the `Bind` error path.
    addr: SocketAddr,
    /// The pre-handshake revision. Preserved for `ServeReport`.
    ceiling: ProtocolVersion,
}

/// The route table this builder describes, and nothing that binds a socket.
///
/// Free so that both [`HttpBuilder::serve`] and [`HttpBuilder::serve_on`] build
/// the same table from the same parts. A second implementation here would be a
/// second answer to "what routes does this server have", and the two would
/// differ the first time either was edited.
fn routes(builder: HttpBuilder) -> Result<BuilderOutput, ServeError> {
    let HttpBuilder {
        server,
        addr,
        ceiling,
        config,
        ids,
        authenticator,
        policy,
        origin_policy,
        rate_limiter,
        #[cfg(feature = "legacy-sse")]
        legacy_sse,
    } = builder;
    let ids = match ids {
        Some(ids) => ids,
        None => default_ids()?,
    };
    #[cfg(feature = "random-ids")]
    let server = {
        let mut server = server;
        server.ensure_request_state_key(crate::adapters::secondary::id_source::random_state_key);
        server
    };
    let config = match config {
        Some(config) => config,
        None => ServerConfig::new()?,
    };
    let dispatcher = Arc::new(Dispatcher::new(Arc::new(server)));
    let sessions = Arc::new(SessionStore::<HttpSession>::new(Arc::clone(&ids)));
    let handler = HttpHandler::new(Arc::clone(&dispatcher), Arc::clone(&sessions), ceiling);
    #[cfg(feature = "legacy-sse")]
    let legacy = legacy_sse
        .map(|paths| {
            let mut router = crate::adapters::primary::transport::legacy_sse::LegacyRouter::new(
                config.clone(),
                Arc::new(SessionStore::new(ids)),
                // The transport belongs to `2024-11-05`; a client offering a
                // later revision is answered with this one and decides for
                // itself whether to continue.
                crate::adapters::primary::transport::legacy_sse::ServerDispatcher::new(
                    Arc::clone(&dispatcher),
                    ProtocolVersion::V2024_11_05,
                ),
            )
            .with_stream_path(paths.stream)
            .with_message_path(paths.message);
            if let Some(origin) = origin_policy.clone() {
                router = router.with_origins(origin);
            }
            if let Some(limiter) = rate_limiter.clone() {
                router = router.with_rate_limiter(limiter);
            }
            if let (Some(policy), Some(authenticator)) = (policy, authenticator.clone()) {
                router = router.with_auth(policy, authenticator);
            }
            router.into_router()
        })
        .transpose()?;
    let mut router = McpRouter::new(config, sessions, handler);
    if let Some(authenticator) = authenticator {
        router = router.with_authenticator(authenticator);
    }
    if let Some(policy) = policy {
        router = router.with_policy(policy);
    }
    if let Some(origin) = origin_policy {
        router = router.with_origin_policy(origin);
    }
    if let Some(limiter) = rate_limiter {
        router = router.with_rate_limiter(limiter);
    }
    let router = router.into_router().map_err(ServeError::from)?;
    #[cfg(feature = "legacy-sse")]
    let router = match legacy {
        Some(legacy) => router.merge(legacy),
        None => router,
    };
    Ok(BuilderOutput {
        router,
        addr,
        ceiling,
    })
}

/// Test-only pass-through that builds a `BuilderOutput` from the four
/// arguments the original `routes` took, so the existing tests keep their
/// one-line call site while the production path goes through the
/// builder.
///
/// Not `#[cfg(test)]` because the tests live in this module and need
/// `pub(crate)`, but the function is documented and named so a future
/// reader does not confuse it with the production entry point.
#[cfg(test)]
#[allow(
    clippy::redundant_pub_crate,
    reason = "the function is test-only; `pub(crate)` is needed in test builds for the tests to reach it, but in lib builds nothing outside the crate can see it, and the lint does not have a per-cfg answer."
)]
pub(crate) fn test_routes(
    server: Server,
    ceiling: ProtocolVersion,
    config: Option<ServerConfig>,
    ids: Option<Ids>,
) -> Result<axum::Router, ServeError> {
    routes(HttpBuilder {
        server,
        addr: SocketAddr::from(([127, 0, 0, 1], 3000)),
        ceiling,
        config,
        ids,
        authenticator: None,
        policy: None,
        origin_policy: None,
        rate_limiter: None,
        #[cfg(feature = "legacy-sse")]
        legacy_sse: None,
    })
    .map(|parts| parts.router)
}

/// Listens on `addr`, and reports what the OS actually gave.
///
/// Free rather than a method because [`HttpBuilder::serve`] has already moved
/// its own fields out by the time it calls this, and re-reading them off a
/// partially-consumed builder is how the two copies drift apart.
async fn bind(addr: SocketAddr) -> Result<(tokio::net::TcpListener, SocketAddr), ServeError> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|source| ServeError::Bind { addr, source })?;
    let bound = listener
        .local_addr()
        .map_err(|source| ServeError::Bind { addr, source })?;
    Ok((listener, bound))
}

/// The id source a builder uses when none was supplied.
///
/// [`RandomIds`] behind the `random-ids` feature, and an error without it. A
/// counter would be the obvious fallback and is the wrong one: a session id is
/// presented by the peer on every subsequent request, so a guessable sequence
/// is a guessable session. Failing is the honest answer for a lean build, and
/// it fails once at startup rather than per request.
#[cfg(feature = "random-ids")]
fn default_ids() -> Result<Ids, ServeError> {
    use crate::adapters::secondary::id_source::RandomIds;
    RandomIds::new()
        .map(|ids| Arc::new(ids) as Ids)
        .map_err(|error| ServeError::IdSource {
            reason: error.to_string(),
        })
}

/// No id source is available in this build, and none was supplied.
#[cfg(not(feature = "random-ids"))]
fn default_ids() -> Result<Ids, ServeError> {
    Err(ServeError::NoIdSource)
}

/// The newest revision this build can complete a handshake on.
///
/// The same fallback as the stdio builder's, and for the same reason: an empty
/// supported set leaves the ceiling a question rather than a wrong answer. Not
/// `latest_known()`, which is `2026-07-28` — the revision SEP-2575 removed
/// `initialize` from, and a server gated on it could never finish talking to
/// anybody.
fn default_ceiling() -> ProtocolVersion {
    SupportedVersions::all().map_or_else(
        |_| ProtocolVersion::latest_known(),
        |supported| supported.latest_handshake(),
    )
}

#[cfg(test)]
mod tests {
    use core::pin::Pin;
    use std::future::Future;
    use std::net::SocketAddr;
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::Value;
    use tower::ServiceExt;

    use super::{HttpBuilder, ServeError, default_ceiling, routes};
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
}
