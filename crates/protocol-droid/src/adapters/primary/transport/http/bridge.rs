//! The bridge's listen side, over Streamable HTTP.
//!
//! ## One session, one conversation, one upstream
//!
//! On `stdio` the bridge is one process, one client, one upstream. A server
//! outlives the clients that connect to it, so here every `initialize` opens a
//! [`BridgeSession`], and each session is a separate conversation answered on
//! its own `mcp-session-id` and served by its own
//! [`Upstream`](crate::application::bridge::Upstream) — opened as the session
//! is created, which is where the per-client rule stops being an idea and
//! becomes a spawn.
//!
//! ## The two halves
//!
//! A `POST` carrying a message is not answered by the `POST`. It is answered
//! `202` and no body, and the reply arrives on the session's SSE stream — the
//! Streamable HTTP contract, and it falls out of the existing pieces:
//! [`EventTail`] is already the per-session buffer the `GET` route streams, and
//! it becomes the bridge's sink unchanged.
//!
//! The other half has no existing piece, because the router is
//! request-and-response shaped and a bridge's loop is a pull.
//! [`HttpFrameSource`] is the seam: one channel per session, the handler
//! pushes into it, the loop pulls from it. A `POST` costs the handler one send
//! and nothing else, because the reply is the tail's job rather than the
//! handler's.
//!
//! ## What the handler deliberately does not do
//!
//! It parses no message beyond the `method` it needs to answer
//! [`RequestHandler::unsessioned`], decides nothing about methods, and has no
//! idea a bridge is involved. A frame arriving before the handshake is refused
//! by the *loop*, not here, and that refusal reaches the client through the
//! tail like any other answer — one path, whatever the reason.

use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::mpsc;

use crate::adapters::primary::transport::http::router::{
    EventSource, Inbound, RequestHandler, Unsessioned,
};
use crate::adapters::primary::transport::http::{
    EventTail, McpRouter, Refusal, ServerConfig, SessionHandle, SessionStore,
};
use crate::application::bridge::{
    BridgeClientError, DownstreamConfig, DownstreamError, ToolResultCodec, Upstream, run_client,
};
use crate::application::jsonrpc::{FrameSource, SourceError};
use crate::application::session::{FrameSink, SinkError};
use crate::domain::{DegradePolicy, McpSessionId};

/// Large enough to cover a reconnect, small enough that an idle client is not
/// holding a megabyte of `Value`s nobody will read. The same value the MCP
/// server's own sessions use, and deliberately so: a bridge and a server differ
/// in who answers, not in how much a reconnecting client may have missed.
const TAIL_CAPACITY: NonZeroUsize = match NonZeroUsize::new(64) {
    Some(capacity) => capacity,
    // A const context cannot call `ok_or`, and `None` here would mean a zero,
    // which is not a capacity. Unreachable for any value above zero.
    None => NonZeroUsize::MIN,
};

/// One client's conversation with the bridge.
///
/// The tail and the inbox are one type because the router needs the tail to
/// answer a `GET` and the loop needs both, and a session whose sink and source
/// could be swapped for another client's is the sharing bug
/// [`Upstream`](crate::application::bridge::Upstream) exists to prevent, one
/// layer down.
#[derive(Debug)]
pub struct BridgeSession {
    tail: EventTail<Value>,
    /// Held exactly as long as the session. The loop owns the other end, and
    /// dropping this is what ends its `next_frame` with `None` — a task
    /// parked forever on a channel nobody will write to is the alternative.
    inbox: mpsc::UnboundedSender<String>,
}

impl EventSource for BridgeSession {
    fn tail(&self) -> &EventTail<Value> {
        &self.tail
    }
}

impl BridgeSession {
    /// A session, and the source its loop will pull from.
    ///
    /// Both halves come from one call because they are one channel: a session
    /// that handed out a source it had not made an inbox for would drop every
    /// frame on the floor, and the type system is the one place that can say
    /// so once rather than at every call site.
    fn new() -> (Self, HttpFrameSource) {
        let (inbox, frames) = mpsc::unbounded_channel();
        let session = Self {
            tail: EventTail::new(TAIL_CAPACITY),
            inbox,
        };
        let source = HttpFrameSource {
            frames: Arc::new(tokio::sync::Mutex::new(frames)),
        };
        (session, source)
    }

    /// Hands one of the client's messages to the loop.
    ///
    /// A closed channel means the loop has ended, and that is this `POST`
    /// arriving after its conversation finished — which a client does when it
    /// retries a message whose reply it never saw.
    fn push(&self, frame: String) -> Result<(), ()> {
        self.inbox.send(frame).map_err(|_| ())
    }

    /// The sink this session's replies go out on.
    fn sink(&self) -> HttpFrameSink {
        HttpFrameSink {
            tail: self.tail.clone(),
        }
    }
}

/// Pulls one session's messages, one `POST` at a time.
struct HttpFrameSource {
    frames: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<String>>>,
}

impl core::fmt::Debug for HttpFrameSource {
    /// By hand, because a receiver's contents are the frames a client is
    /// waiting on and reading them out to print them would be a data race and
    /// a privacy problem. The channel's state is the only thing worth showing.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("HttpFrameSource")
            .field("frames", &"<one session's inbound messages>")
            .finish()
    }
}

impl FrameSource for HttpFrameSource {
    fn next_frame(&self) -> impl Future<Output = Result<Option<String>, SourceError>> + Send {
        let frames = Arc::clone(&self.frames);
        async move {
            // Held across the await deliberately: `recv` needs `&mut self`,
            // and the loop is the only reader, so a second concurrent
            // `next_frame` would be a bug rather than a case to serve.
            let mut receiver = frames.lock().await;
            // The channel's own close is the end of the conversation — every
            // sender gone means the session was terminated. An unbounded
            // channel reports that as `None` rather than an error, which is
            // right: a client hanging up is not a fault, and `None` is the
            // loop's cue to finish.
            Ok(receiver.recv().await)
        }
    }
}

/// Writes one session's replies onto its tail, for the `GET` route to stream.
#[derive(Debug)]
struct HttpFrameSink {
    tail: EventTail<Value>,
}

impl FrameSink for HttpFrameSink {
    fn send(&self, frame: &str) -> impl Future<Output = Result<(), SinkError>> + Send {
        // Parsed here rather than trusted: the tail holds `Value`, and a frame
        // that is not JSON cannot be streamed. Refusing on the way out means
        // there is still a caller to tell.
        let pushed = serde_json::from_str::<Value>(frame)
            .map(|value| self.tail.push(value))
            .map_err(|_| SinkError);
        std::future::ready(pushed.map(|_| ()))
    }
}

/// Serves a bridge on the `POST` route, one loop per session.
///
/// Generic over the upstream factory and the wire codec rather than holding a
/// `dyn` of either: [`Upstream`] returns an `impl Future`, so it is not
/// dyn-safe, and a codec is a zero-sized type that storing per handler would
/// imply carries state between clients.
#[derive(Debug)]
struct BridgeHandler<U, C> {
    sessions: Arc<SessionStore<BridgeSession>>,
    upstream: Arc<U>,
    config: DownstreamConfig,
    policy: DegradePolicy,
    codec: C,
}

impl<U, C> BridgeHandler<U, C> {
    /// A handler that opens `upstream` once per client.
    fn new(
        sessions: Arc<SessionStore<BridgeSession>>,
        upstream: U,
        config: DownstreamConfig,
        policy: DegradePolicy,
        codec: C,
    ) -> Self {
        Self {
            sessions,
            upstream: Arc::new(upstream),
            config,
            policy,
            codec,
        }
    }
}

/// What a `POST` carrying a message is answered with.
///
/// `202` and no body, always — including for the handshake. The reply is on the
/// session's stream, and a client reading the handshake out of the `POST`
/// would be reading it from somewhere the specification does not put it.
fn accepted() -> axum::response::Response {
    axum::response::Response::builder()
        .status(axum::http::StatusCode::ACCEPTED)
        .body(axum::body::Body::empty())
        .unwrap_or_else(|_| axum::response::Response::new(axum::body::Body::empty()))
}

/// `accepted`, carrying the id a client needs to address its own stream.
fn accepted_with(id: McpSessionId) -> Result<axum::response::Response, Refusal> {
    let value = axum::http::HeaderValue::from_str(&id.to_string())
        .map_err(|error| Refusal::Handler(format!("the id is not a header: {error}")))?;
    let mut response = accepted();
    response.headers_mut().insert("mcp-session-id", value);
    Ok(response)
}

/// The `method` of a body, when it is a single JSON object naming one.
///
/// The one thing the handler looks at, and only to answer `unsessioned`:
/// whether this `POST` is the one that opens a conversation.
fn method_of(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<Value>(body)
        .ok()?
        .get("method")?
        .as_str()
        .map(String::from)
}

impl<U, C> RequestHandler<BridgeSession> for BridgeHandler<U, C>
where
    U: Upstream + 'static,
    C: ToolResultCodec + Clone + Send + Sync + 'static,
{
    fn unsessioned(&self, body: &[u8]) -> Unsessioned {
        match method_of(body).as_deref() {
            Some("initialize") => Unsessioned::Establishing,
            _ => Unsessioned::NeedsSession,
        }
    }

    fn dispatch(
        &self,
        call: Inbound<'_, BridgeSession>,
    ) -> impl Future<Output = Result<axum::response::Response, Refusal>> + Send {
        // Copied out before the future, because `Inbound` borrows the body and
        // the router's `Arc`, and neither outlives this function.
        let frame = String::from_utf8(call.body.to_vec()).map_err(|_| Refusal::MalformedBody {
            reason: "a request body is not UTF-8".to_owned(),
        });
        let session = match &call.session {
            SessionHandle::Live { session, .. } => Ok(Some(Arc::clone(session))),
            SessionHandle::Establishing => Ok(None),
            // A bridge is a conversation; the stateless lifecycle has none, and
            // answering a self-describing request as though it had a session
            // to join would invent one.
            SessionHandle::Stateless => Err(Refusal::Handler(
                "a bridge is a conversation, and the stateless lifecycle has \
                 none; open a session"
                    .to_owned(),
            )),
        };
        let sessions = Arc::clone(&self.sessions);
        let upstream = Arc::clone(&self.upstream);
        let config = self.config.clone();
        let policy = self.policy;
        let codec = self.codec.clone();

        async move {
            let frame = frame?;
            match session? {
                Some(session) => {
                    session
                        .push(frame)
                        .map_err(|()| Refusal::Handler("this session has ended".to_owned()))?;
                    Ok(accepted())
                }
                None => Self::open(&sessions, upstream, config, policy, codec, frame),
            }
        }
    }
}

impl<U, C> BridgeHandler<U, C>
where
    U: Upstream + 'static,
    C: ToolResultCodec + Clone + Send + Sync + 'static,
{
    /// Opens a session, its upstream, and its loop, then queues the `POST`
    /// that caused all three.
    ///
    /// Not `async`, and deliberately: a store insert, a `tokio::spawn` and a
    /// channel send are all synchronous, and an `async` body that never yields
    /// is a future that cannot be interrupted — which is the wrong shape for
    /// the one call that starts work on a client's behalf.
    fn open(
        sessions: &SessionStore<BridgeSession>,
        upstream: Arc<U>,
        config: DownstreamConfig,
        policy: DegradePolicy,
        codec: C,
        frame: String,
    ) -> Result<axum::response::Response, Refusal> {
        let (session, source) = BridgeSession::new();
        let session = Arc::new(session);
        let sink = session.sink();

        // Stored before the loop starts, so a client's very next `POST` — which
        // can arrive before the loop has been polled once — resolves.
        let id = sessions
            .open(Arc::clone(&session))
            .map_err(|error| Refusal::Handler(format!("no session for the client: {error}")))?;

        tokio::spawn(async move {
            // A client hanging up ends the loop, and that is not a fault worth
            // reporting. Anything else is dropped here, and that is a real gap
            // rather than a design choice: this crate has no logger, so there
            // is nowhere to put it, and the client learns the same thing by
            // watching its stream close. The operator does not. Closing that
            // wants a `LoopReport` the `GET` route can answer on, not a
            // print.
            let _ = run_client(upstream.as_ref(), policy, config, codec, source, sink).await;
        });

        session
            .push(frame)
            .map_err(|()| Refusal::Handler("the new session has ended".to_owned()))?;
        accepted_with(id)
    }
}

/// Why a bridge's HTTP side could not be built.
#[derive(Debug, thiserror::Error)]
pub enum BridgeServeError {
    /// The router refused to build.
    ///
    /// In practice a configured security header that is not a legal HTTP
    /// value, or the issue #11 fail-open combination of a wired
    /// authenticator and the default `AuthPolicy::Public` policy.
    #[error("the router could not be built: {0}")]
    Router(#[from] crate::adapters::primary::transport::http::router::RouterError),
    /// The downstream leg could not be built.
    #[error(transparent)]
    Downstream(#[from] DownstreamError),
    /// A client's loop ended.
    #[error(transparent)]
    Client(#[from] BridgeClientError),
    /// The address could not be bound, or the accept loop failed.
    #[error("could not serve on {addr}: {source}")]
    Bind {
        /// The address the bridge was asked to take.
        addr: SocketAddr,
        /// What the socket reported.
        source: std::io::Error,
    },
}

/// A bridge served over Streamable HTTP.
#[derive(Debug)]
pub struct HttpBridge<U, C> {
    config: ServerConfig,
    sessions: Arc<SessionStore<BridgeSession>>,
    handler: BridgeHandler<U, C>,
}

impl<U, C> HttpBridge<U, C>
where
    U: Upstream + 'static,
    C: ToolResultCodec + Clone + Send + Sync + 'static,
{
    /// A bridge that opens `upstream` once per client.
    ///
    /// # Errors
    ///
    /// [`DownstreamError::UnnegotiableVersion`] if the build speaks no
    /// revision a client could negotiate. That list is a build-time constant
    /// and the specification has defined at least one since 2024, so it is a
    /// formality — but a caller building this at startup deserves a real error
    /// rather than a panic later.
    pub fn new(
        config: ServerConfig,
        sessions: Arc<SessionStore<BridgeSession>>,
        upstream: U,
        identity: crate::application::bridge::Identity,
        policy: DegradePolicy,
        codec: C,
    ) -> Result<Self, BridgeServeError> {
        let versions = crate::domain::SupportedVersions::all()
            .map_err(|_| DownstreamError::UnnegotiableVersion)?;
        Ok(Self {
            config,
            sessions: Arc::clone(&sessions),
            handler: BridgeHandler::new(
                sessions,
                upstream,
                DownstreamConfig::with(
                    versions,
                    identity,
                    crate::domain::TransportKind::StreamableHttp,
                ),
                policy,
                codec,
            ),
        })
    }

    /// The router, for a caller that wants to serve it itself.
    ///
    /// # Errors
    ///
    /// [`RouterError`](crate::adapters::primary::transport::http::router::RouterError)
    /// — a configured security header that is not a legal HTTP value, or
    /// the issue #11 fail-open combination of a wired authenticator and
    /// the default `AuthPolicy::Public` policy. The same errors the MCP
    /// server's own builder reports, surfaced through
    /// [`BridgeServeError::Router`] on the bind path.
    pub fn into_router(
        self,
    ) -> Result<axum::Router, crate::adapters::primary::transport::http::router::RouterError> {
        McpRouter::new(self.config, self.sessions, self.handler).into_router()
    }

    /// Binds the address and reports what it got, without serving.
    ///
    /// Split from serving for the same reason
    /// [`HttpBuilder`](crate::adapters::primary::transport::http::HttpBuilder)
    /// splits it: a caller that passed port `0` has to learn the real address,
    /// and has to learn it *before* the accept loop starts rather than after.
    ///
    /// # Errors
    ///
    /// [`BridgeServeError::Bind`] when the address is taken or unusable, and
    /// [`BridgeServeError::Router`] for an illegal configured header.
    pub async fn bind(self, addr: SocketAddr) -> Result<(Bound, SocketAddr), BridgeServeError> {
        let router = self.into_router()?;
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|source| BridgeServeError::Bind { addr, source })?;
        let bound = listener
            .local_addr()
            .map_err(|source| BridgeServeError::Bind { addr, source })?;
        Ok((
            Bound {
                router,
                listener,
                addr: bound,
            },
            bound,
        ))
    }

    /// Binds the address and serves until the accept loop fails.
    ///
    /// The one-call path, for a caller that already knows its address.
    ///
    /// # Errors
    ///
    /// As [`HttpBridge::bind`], and whatever the accept loop itself reported.
    pub async fn serve(self, addr: SocketAddr) -> Result<SocketAddr, BridgeServeError> {
        let (bound, actual) = self.bind(addr).await?;
        bound.serve().await?;
        Ok(actual)
    }
}

/// A bound socket and the route table that will answer on it.
///
/// One type rather than a `(TcpListener, Router)` tuple, because the two are
/// only ever used together and a tuple is two things a caller can swap the
/// halves of — the same argument
/// [`OpenedUpstream`](crate::application::bridge::OpenedUpstream) makes about
/// a leg and a peer.
#[derive(Debug)]
pub struct Bound {
    router: axum::Router,
    listener: tokio::net::TcpListener,
    addr: SocketAddr,
}

impl Bound {
    /// The address this actually took, which is not the one asked for when the
    /// caller passed port `0`.
    #[must_use]
    pub const fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Serves until the accept loop fails.
    ///
    /// # Errors
    ///
    /// [`BridgeServeError::Bind`] if the socket fails while serving.
    pub async fn serve(self) -> Result<(), BridgeServeError> {
        axum::serve(self.listener, self.router)
            .await
            .map_err(|source| BridgeServeError::Bind {
                addr: self.addr,
                source,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{BridgeSession, HttpBridge, HttpFrameSource};
    use crate::adapters::primary::transport::http::{SecurityHeaders, ServerConfig, SessionStore};
    use crate::application::bridge::SendFuture;
    use crate::application::bridge::{BridgeError, BridgePeer, Identity, OpenedUpstream, Upstream};
    use crate::application::jsonrpc::FrameSource;
    use crate::application::session::{FrameSink, IdSource};
    use crate::domain::{DegradePolicy, Feature, FeatureProfile, Leg, ProtocolVersion};
    use serde_json::{Map, Value, json};
    use std::net::{Ipv4Addr, SocketAddr};
    use std::sync::Arc;

    /// Hands out a fixed sequence, so the test can name the id it expects.
    struct Counting(std::sync::atomic::AtomicU64);

    impl IdSource for Counting {
        fn next_session_id(&self) -> crate::domain::session::SessionId {
            crate::domain::session::SessionId::new(
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1,
            )
        }
    }

    /// A far end that answers nothing, because these tests are about the bind.
    struct Idle;

    struct IdlePeer;

    impl Upstream for Idle {
        fn open(
            &self,
        ) -> impl std::future::Future<
            Output = Result<OpenedUpstream, crate::application::bridge::UpstreamError>,
        > + Send {
            let version = ProtocolVersion::V2025_11_25;
            let tools = FeatureProfile::empty().with(Feature::Tools);
            std::future::ready(Ok(OpenedUpstream::new(
                Leg::new(version, tools),
                Arc::new(IdlePeer),
            )))
        }
    }

    impl BridgePeer for IdlePeer {
        fn send(
            &self,
            envelope: Map<String, Value>,
        ) -> SendFuture<'_, Result<Map<String, Value>, BridgeError>> {
            Box::pin(async move { Ok(envelope) })
        }

        fn notify(&self, envelope: Map<String, Value>) -> SendFuture<'_, Result<(), BridgeError>> {
            Box::pin(async move {
                let _ = envelope;
                Ok(())
            })
        }

        fn close(&self) -> SendFuture<'_, Result<(), BridgeError>> {
            Box::pin(async { Ok(()) })
        }
    }

    fn bridge() -> Result<
        HttpBridge<Idle, crate::adapters::secondary::codec::result::WireToolResults>,
        Box<dyn std::error::Error>,
    > {
        let ids: Arc<dyn IdSource + Send + Sync> =
            Arc::new(Counting(std::sync::atomic::AtomicU64::new(0)));
        let sessions: Arc<SessionStore<BridgeSession>> = Arc::new(SessionStore::new(ids));
        Ok(HttpBridge::new(
            ServerConfig::new()?.with_security(SecurityHeaders::default()),
            sessions,
            Idle,
            Identity::new("unit", "0.0.0"),
            DegradePolicy::default(),
            crate::adapters::secondary::codec::result::WireToolResults,
        )?)
    }

    #[tokio::test]
    async fn a_bridge_binds_and_reports_the_address_it_took()
    -> Result<(), Box<dyn std::error::Error>> {
        // Port `0` is the case that makes the split worth having: the caller
        // asked for *a* port and has to learn which one, before anything
        // connects to it.
        let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        let (bound, actual) = bridge()?.bind(loopback).await?;

        if actual.port() == 0 {
            return Err("the bridge reported port 0 after binding".into());
        }
        if bound.addr() != actual {
            return Err(format!(
                "the bound socket says {} and the report said {actual}",
                bound.addr()
            )
            .into());
        }
        Ok(())
    }

    #[tokio::test]
    async fn an_agreement_the_peer_cannot_hold_still_binds()
    -> Result<(), Box<dyn std::error::Error>> {
        // Binding is a transport decision and the handshake is a protocol one.
        // A bridge that refused to listen until it had reached a server would
        // be unusable in exactly the way this whole increment exists to fix.
        let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        let (bound, _actual) = bridge()?.bind(loopback).await?;
        drop(bound);
        Ok(())
    }

    /// A session and the source its loop pulls from, which is how a caller
    /// gets one. Built here so the unit tests and the wire test construct the
    /// same pair rather than two shapes that agree today.
    fn session() -> (BridgeSession, HttpFrameSource) {
        BridgeSession::new()
    }

    #[tokio::test]
    async fn a_frame_pushed_comes_back_out_in_order() -> Result<(), Box<dyn std::error::Error>> {
        let (session, source) = session();
        session
            .push("first".to_owned())
            .map_err(|()| "the loop had ended")?;
        session
            .push("second".to_owned())
            .map_err(|()| "the loop had ended")?;

        // Two clients' frames cannot be told apart here, because the source
        // only ever belongs to one session. That is the point: there is no
        // second session's source to confuse it with.
        let first = source.next_frame().await?;
        let second = source.next_frame().await?;
        if (first.as_deref(), second.as_deref()) != (Some("first"), Some("second")) {
            return Err(format!("the source yielded {first:?} then {second:?}").into());
        }
        Ok(())
    }

    #[tokio::test]
    async fn a_dropped_session_ends_the_source_rather_than_parking_it()
    -> Result<(), Box<dyn std::error::Error>> {
        let (session, source) = session();
        drop(session);
        // A task blocked on a channel nobody will write to is the failure this
        // rules out, so the assertion is that it *returns* — it is what tells
        // the loop to finish.
        if source.next_frame().await?.is_some() {
            return Err("a session that was dropped still produced a frame".into());
        }
        Ok(())
    }

    #[tokio::test]
    async fn a_reply_is_queued_on_the_tail_the_get_route_streams()
    -> Result<(), Box<dyn std::error::Error>> {
        let (session, _source) = session();
        let sink = session.sink();
        if !sink.tail.is_empty() {
            return Err("a fresh session already had a reply queued".into());
        }
        sink.send(&json!({"jsonrpc": "2.0", "id": 7, "result": {"ok": true}}).to_string())
            .await?;

        // Counted rather than read back: the tail's own reader is the SSE
        // route, and asserting through it would mean asserting the router
        // tested itself. `http_bridge_wire.rs` drives a real client against a
        // real socket and reads the reply off the stream, which is the claim
        // that the queue and the route agree — this one is here so a sink that
        // wrote nowhere at all is caught without a socket.
        if sink.tail.is_empty() {
            return Err("the reply was sent but nothing was queued".into());
        }
        Ok(())
    }

    #[tokio::test]
    async fn a_frame_that_is_not_json_is_refused_rather_than_queued()
    -> Result<(), Box<dyn std::error::Error>> {
        let (session, _source) = session();
        let sink = session.sink();
        // A frame the tail cannot hold would panic on its way in, so the parse
        // is the only thing standing between a bad loop and a broken server.
        if sink.send("not json at all").await.is_ok() {
            return Err("a frame that is not JSON was accepted".into());
        }
        Ok(())
    }
}
