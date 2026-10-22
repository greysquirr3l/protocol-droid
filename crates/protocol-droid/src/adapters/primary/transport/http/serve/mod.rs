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
use std::sync::Arc;

use serde_json::Value;

use crate::adapters::primary::transport::http::config::Streaming;
use crate::adapters::primary::transport::http::documents::{
    answer_of, id_as_i64, method_of, params_of, refusal_document, refusal_document_from_refusal,
    result_document,
};
use crate::adapters::primary::transport::http::exchange::{ExchangeTable, HttpExchange};
use crate::adapters::primary::transport::http::refusal::Refusal;
use crate::adapters::primary::transport::http::router::{
    EventSource, Inbound, RequestHandler, SessionHandle, Unsessioned,
};
use crate::adapters::primary::transport::http::session_store::SessionStore;
use crate::adapters::primary::transport::http::stateless::{StatelessHeaders, StatelessRequest};
use crate::adapters::primary::transport::http::stream::{EventTail, Finishing};
use crate::application::server::context::{PeerSession, RequestContext};
use crate::application::server::dispatch::Dispatcher;
use crate::application::server::{ListKind, ListSubscription};
use crate::application::session::IdSource;
use crate::domain::feature::resolve;
use crate::domain::ir::JsonObject;
use crate::domain::jsonrpc::{Frame, WireId, decode};
use crate::domain::mcp::McpSessionId;
use crate::domain::session::{CancellationToken, ProgressToken, SessionId};
use crate::domain::{
    CapabilitySet, EventId, Negotiation, ProtocolVersion, RequestId, TransportKind,
};

mod builder;
mod check;
mod sink;

#[cfg(test)]
mod tests;

use check::{check_header, declares_own_version};

// `TailSink` is how the `listen` and `stateless` adapters hand a session's
// outgoing frames somewhere. `pub(super)` is `http`, which is where those two
// live — `serve` is private to `http`, so nothing wider is possible or needed.
pub(super) use sink::TailSink;

// `HttpBuilder` is the public face of this module: the four decisions a
// socket server needs, in a builder. It is defined in `builder` because the
// axum wiring is a different concern from the request handling, and it is
// re-exported here because this is where a consumer looks for it.
// `HttpBuilder`, `ServeError` and `ServeReport` are this module's public
// face: the four decisions a socket server needs, what fails them, and what a
// successful bind reports. They live in `builder` because the axum wiring and
// the socket are the same concern, and they are re-exported here because this
// is where a consumer looks for them.
pub use builder::{HttpBuilder, ServeError, ServeReport};

// The test-only entry point into the axum wiring, reached from the `listen`
// and `stateless` test modules as well as this module's own. `pub(super)` is
// `http`, which is where those siblings live — `pub(crate)` would be refused
// here, because `serve` is itself private to `http`. It is `#[cfg(test)]` on
// both sides, so a lean build never carries it.
#[cfg(test)]
pub(super) use builder::test_routes;

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
    /// The same state as `Self::peer_of` and for the same reason: a task
    /// that outlives the request it was spawned for has to keep reading the
    /// *session's* profile rather than a borrow of a body that is gone.
    pub fn peer(&self) -> PeerSession {
        self.peer.clone()
    }
}

/// The handler that bridges [`McpRouter`](crate::adapters::primary::transport::http::router::McpRouter)
/// to the real [`Dispatcher`].
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
