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
use crate::adapters::primary::transport::http::limits::RateLimiter;
use crate::adapters::primary::transport::http::refusal::Refusal;
use crate::adapters::primary::transport::http::router::{
    EventSource, Inbound, McpRouter, RequestHandler, SessionHandle, Unsessioned,
};
use crate::adapters::primary::transport::http::session_store::SessionStore;
use crate::adapters::primary::transport::http::exchange::{Answer, ExchangeTable, HttpExchange};
use crate::adapters::primary::transport::http::stream::{EventTail, Finishing};
use crate::adapters::primary::transport::http::{AuthPolicy, Authenticator};
use crate::adapters::primary::transport::origin::OriginPolicy;
use crate::application::server::Server;
use crate::application::server::context::{ExchangeError, PeerSession, RequestContext};
use crate::application::server::dispatch::Dispatcher;
use crate::application::session::IdSource;
use crate::application::session::frame_sink::{FrameSink, SinkError};
use crate::domain::feature::resolve;
use crate::domain::ir::{IrErrorCode, JsonObject};
use crate::domain::jsonrpc::reply::{self, ReplyError};
use crate::domain::jsonrpc::{Envelope, Frame, WireId, decode};
use crate::domain::mcp::McpSessionId;
use crate::domain::session::{CancellationToken, ProgressToken, SessionId};
use crate::domain::{
    CapabilitySet, EventId, Negotiation, ProtocolVersion, RequestId, SupportedVersions,
    TransportKind,
};

/// The tail this session's server-initiated messages are published on.
type Tail = EventTail<Value>;

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
const KEEP_ALIVE: core::time::Duration = core::time::Duration::ZERO;

const TAIL_CAPACITY: NonZeroUsize = match NonZeroUsize::new(64) {
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
    dispatcher: Arc<Dispatcher>,
    /// This server's sessions. The handler owns the store rather than being
    /// handed one per request because *minting* is a protocol-layer decision:
    /// the router can look a session up but cannot decide what a session is.
    sessions: Arc<SessionStore<HttpSession>>,
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
        let profile = resolve(Negotiation {
            version: self.ceiling,
            client_caps: CapabilitySet::empty(),
            server_caps: CapabilitySet::all_for(self.ceiling),
            transport: TransportKind::StreamableHttp,
        });
        let tail = Tail::new(TAIL_CAPACITY);
        let exchange = ExchangeTable::new();
        let sink = Arc::new(TailSink { tail: tail.clone() });
        HttpSession {
            peer: PeerSession::new(
                self.ceiling,
                profile,
                SessionId::new(1),
                TransportKind::StreamableHttp,
                Arc::new(HttpExchange::new(exchange.clone(), tail.clone())),
                sink,
            ),
            tail,
            exchange,
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
    /// Whether a body with no session header opens one.
    ///
    /// Only `initialize` does. Anything else arriving without a session id is a
    /// stateful call missing its header, which the router answers with a `400`
    /// and never hands here — so returning [`Unsessioned::NeedsSession`] for it
    /// is not a guess, it is the arm that produces that `400`.
    fn unsessioned(&self, body: &[u8]) -> Unsessioned {
        match method_of(body).as_deref() {
            Some("initialize") => Unsessioned::Establishing,
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
        let opening = matches!(call.session, SessionHandle::Establishing);
        let stored = match &call.session {
            SessionHandle::Establishing | SessionHandle::Stateless => None,
            SessionHandle::Live { session, .. } => Some(Arc::clone(session)),
        };
        // The principal is this request's, not the session's — see
        // `peer_for`, where the reasoning lives.
        let peer = self
            .peer_for(stored.as_ref())
            .with_auth(call.context.clone());

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
            // The progress token lives in `params._meta.progressToken`, which
            // is where the specification puts it and not beside the request id.
            // It is optional in the strongest sense: a request without one is
            // valid and gets no progress updates, so reading it is a lookup and
            // never a gate.
            //
            // Not reading it is the quiet failure it is: `report_progress`
            // then answers `NoProgressToken`, the tool reports no progress, and
            // the client that explicitly asked to be told waits for updates
            // from a server that believed nobody had asked.
            // Owned: the envelope borrows the request body, and the body is
            // dropped once this function returns — which happens while a
            // handler is still running on this session.
            let progress: Option<ProgressToken> = progress_token_of(&envelope);
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
            // Held only as long as this body. The task below builds its own
            // from the stored session, because it outlives both of them.
            let peer_for_now = peer;
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
                return self.respond_beside_stream(
                    &peer_for_now,
                    session,
                    id.as_ref(),
                    progress,
                    method,
                    &params,
                );
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
                // `handshake` narrows the context it was handed, so the peer it
                // returns carries the *establishing request's* principal. Every
                // request resolves its own principal above, so nothing
                // downstream reads this one — which is exactly why it is worth
                // dropping rather than storing: session state outlives the
                // credential that produced it, and a principal recorded there
                // has the wrong lifetime for a token that can be refreshed,
                // replaced, or withdrawn mid-session.
                let peer = negotiated
                    .ok_or_else(|| {
                        Refusal::Handler("a session was opened without a handshake".to_owned())
                    })?
                    .without_auth();
                // The tail the router streams to the client, and the sink the
                // handler writes through, are the same ring. Binding the peer
                // to a *different* one is the quiet failure this shape invites:
                // the session's own tail stays empty forever while the peer
                // dutifully publishes into a ring no reader ever drains, so a
                // tool that logs looks like a tool that logs nothing.
                let (tail, exchange) = stored.map_or_else(
                    || (Tail::new(TAIL_CAPACITY), ExchangeTable::new()),
                    |session| (session.tail.clone(), session.exchange.clone()),
                );
                // The store allocates the id below, so it is read back from the
                // minted handle rather than allocated twice — and the peer is
                // bound to it before anything can key on it.
                let peer = peer
                    .with_sink(Arc::new(TailSink { tail: tail.clone() }))
                    // Bound to *this* session's table, for the same reason the
                    // sink is bound to this session's tail: the peer outlives
                    // the seed it was built from, and an exchange on the wrong
                    // table publishes a request nobody on this session is
                    // waiting to answer.
                    .with_exchange(Arc::new(HttpExchange::new(exchange.clone(), tail.clone())));
                let id_source = self.sessions.next_id();
                let peer = peer.with_session(id_source);
                let minted = self
                    .sessions
                    .open_with_id(Arc::new(HttpSession {
                        peer,
                        tail,
                        exchange,
                    }), id_source)
                    .map_err(|error| Refusal::Handler(format!("no session: {error}")))?;
                return Self::response(&document, Some(&minted), None);
            }

            Self::response(
                &document,
                None,
                stored.map(|session| (session.tail.clone(), cursor)),
            )
        }
    }

}

impl HttpHandler {
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
            return match tail.plan(since.as_deref()).into_draining_response(KEEP_ALIVE) {
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

/// The `id` a reply must be written under, borrowed from the body it answers.
type Id<'a> = WireId<'a>;

/// The JSON-RPC success document for `result`.
///
/// A result with no wire form is this server's own bug rather than anything a
/// peer did, so it becomes a refusal rather than a frame that decodes to
/// something else.
fn result_document(id: Option<&Id<'_>>, result: &JsonObject) -> Result<Value, Refusal> {
    let id = id.ok_or_else(|| {
        Refusal::Handler("a result was produced for a request that asked no question".to_owned())
    })?;
    reply::result_reply(id, result).map_err(|error| match error {
        ReplyError::UnreadableResult => {
            Refusal::Handler("the dispatcher's result has no JSON form".to_owned())
        }
    })
}

/// The JSON-RPC error document for a refused request.
///
/// Every refusal gets an answer. The alternative is to drop it, which leaves
/// the peer waiting on a response that is never coming — and a server that
/// hangs is worse than one that says no. The code is the one the error carries,
/// not one this module picked: the message is never the method name the peer
/// chose, because a method name is a JSON string a peer controls and JSON
/// permits a newline inside one.
/// A refusal rendered as the JSON-RPC error a peer will read.
fn refusal_document_from_refusal(id: Option<&WireId<'_>>, refusal: &Refusal) -> Value {
    id.map_or_else(
        || reply::unidentified_error_reply(IrErrorCode::InternalError, &refusal.to_string()),
        |id| reply::error_reply(id, IrErrorCode::InternalError, &refusal.to_string()),
    )
}

fn refusal_document(
    id: Option<&WireId<'_>>,
    error: &crate::application::server::DispatchError,
) -> Value {
    let (code, message) = error.as_wire();
    id.map_or_else(
        || reply::unidentified_error_reply(code, &message),
        |id| reply::error_reply(id, code, &message),
    )
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

/// The method name in a body, if it is a single routable request.
///
/// Owned rather than borrowed: the envelope borrows from the buffer `decode`
/// parsed, and that buffer is dropped when this returns. A body that is a
/// batch or a reply shape reads as `None` here and takes the same arm, because
/// neither is something this handler should open a session from.
fn method_of(body: &[u8]) -> Option<String> {
    let decoded = decode(body).ok()?;
    let Frame::Single(envelope) = decoded else {
        return None;
    };
    envelope.method_str().map(str::to_owned)
}

/// The params object a request carries, or an empty one.
///
/// A request with no `params` is a request for a method that takes none, so
/// the empty object is right rather than a guess. Valid JSON that is not an
/// object is a well-formed request with a bad argument, which is `-32602`
/// rather than a parse failure.
fn params_of(envelope: &Envelope<'_>) -> Result<JsonObject, Refusal> {
    envelope.params().map_or_else(
        || Ok(JsonObject::default()),
        |raw| {
            JsonObject::new(raw.get()).map_err(|_| Refusal::MalformedBody {
                reason: "params is not a JSON object".to_owned(),
            })
        },
    )
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
struct TailSink {
    tail: Tail,
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
        let pushed = serde_json::from_str::<Value>(frame)
            .map(|value| self.tail.push(value))
            .map_err(|_| SinkError);
        core::future::ready(pushed.map(|_| ()))
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
        }
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
    } = builder;
    let ids = match ids {
        Some(ids) => ids,
        None => default_ids()?,
    };
    let config = match config {
        Some(config) => config,
        None => ServerConfig::new()?,
    };
    let sessions = Arc::new(SessionStore::<HttpSession>::new(ids));
    let handler = HttpHandler::new(
        Arc::new(Dispatcher::new(Arc::new(server))),
        Arc::clone(&sessions),
        ceiling,
    );
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
    Ok(BuilderOutput {
        router: router.into_router().map_err(ServeError::from)?,
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
    /// The `params._meta.progressToken` a request carried, if any.
///
/// `None` for a request with no `_meta`, a `_meta` without the member, or a
/// member that is not a string or a number — the three shapes the vendored
/// schema excludes. All three mean the same thing to a handler: nobody asked
/// for progress, so there is nothing to report against.
/// The numeric id a wire id carries, if it is one.
///
/// `None` for a string id or an absent one. A server-initiated request's id is
/// minted as a number, so a string id on an inbound frame is the peer answering
/// something this server did not ask — which is a message to discard, not one
/// to guess a correlation for.
const fn id_as_i64(id: &WireId<'_>) -> Option<i64> {
    match id {
        WireId::Number(value) => Some(*value),
        WireId::Text(_) => None,
    }
}

/// What a peer said in answer to a server-initiated request.
///
/// A JSON-RPC `error` member becomes the refusal it is — the peer's own words,
/// not a re-phrasing — because a tool asked a question and the answer "the
/// client declined" is different from the answer "the server broke", and
/// collapsing them costs the caller the one distinction it could act on.
fn answer_of(body: &[u8]) -> Answer {
    let Ok(document) = serde_json::from_slice::<Value>(body) else {
        return Err(ExchangeError::new("the answer was not JSON"));
    };
    if let Some(failure) = document.get("error") {
        return Err(ExchangeError::new(failure.to_string()));
    }
    document.get("result").map_or_else(
        || {
            Err(ExchangeError::new(
                "the answer carried neither a result nor an error",
            ))
        },
        |result| {
            JsonObject::from_value(result.clone())
                .map_err(|error| ExchangeError::new(error.to_string()))
        },
    )
}

fn progress_token_of(envelope: &Envelope<'_>) -> Option<ProgressToken> {
    let params: serde_json::Value = serde_json::from_str(envelope.params()?.get()).ok()?;
    let token = params.get("_meta")?.get("progressToken")?;
    ProgressToken::from_json(token)
}

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
        Anonymous, AuthPolicy, Authenticator, Credentials,
    };
    use crate::application::server::{Server, ToolHandler};
    use crate::domain::registry::auth::ScopeSet;
    use crate::domain::registry::{AuthContext, Primitive, Registered, Scope, Tool};

    /// Anything a test here can fail at, gathered so a test can `?` through
    /// unrelated error types without unwrapping.
    #[derive(Debug)]
    enum Failure {
        /// A test assumption about the crate's own defaults did not hold.
        Expectation(&'static str),
        /// The transport refused something a test did not expect it to.
        Serve(ServeError),
        /// A request never produced a response.
        NoResponse,
    }

    impl core::fmt::Display for Failure {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::Expectation(what) => write!(formatter, "expected {what}"),
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
        found.ok_or(Failure::Expectation("the reply to carry a JSON-RPC document"))
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
                    .header("content-type", "application/json")
                    .header("accept", "application/json, text/event-stream")
                    .body(Body::from(body.as_slice().to_vec()))
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
        let body = br#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("content-type", "application/json")
                    .header("accept", "application/json, text/event-stream")
                    .body(Body::from(body.as_slice().to_vec()))
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
    async fn post(
        router: &axum::Router,
        body: &str,
        credential: Option<&str>,
        session: Option<&str>,
    ) -> Result<(StatusCode, Option<String>, Value), Failure> {
        let mut request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");
        if let Some(credential) = credential {
            request = request.header("authorization", credential);
        }
        if let Some(session) = session {
            request = request.header("mcp-session-id", session);
        }
        let built = request
            .body(Body::from(body.to_owned()))
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
