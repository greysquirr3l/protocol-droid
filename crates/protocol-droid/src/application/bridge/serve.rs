//! The loop that turns a pair of framed peers into a running bridge.
//!
//! [`Bridge::forward`](super::Bridge::forward) moves one envelope. This moves
//! every envelope, in both directions, until the downstream peer stops talking.
//!
//! ## Why a refusal is answered rather than fatal
//!
//! The tempting shape is `bridge.forward(..).await?` — one error kills the
//! loop and the client's socket closes. That reports "the bridge is down" for
//! a feature the bridge has already explained it cannot serve, and a client
//! that retries against a perfectly healthy bridge gets the same failure
//! forever. Every [`BridgeError`] is therefore rendered into a JSON-RPC error
//! envelope carrying the request's own `id`, and the loop continues. The
//! client learns which call failed and why, and every other call keeps working.
//!
//! ## The same reasoning covers a malformed frame
//!
//! A frame that is not JSON is answered `-32700`; a frame that is not a
//! JSON-RPC message is answered `-32600`. Both carry `id: null`, because a
//! frame we could not parse has no id to quote. A proxy that died on the first
//! bad frame from one client would take down every other client with it.
//!
//! ## A notification writes nothing
//!
//! A conforming peer answers a notification with `202` and no body, so there
//! is nothing to send back and inventing an empty reply would put a response on
//! the wire that the server never sent.
//!
//! ## Batches
//!
//! A frame that is an array is a JSON-RPC batch, and it is answered with a
//! single array rather than with one frame per element — a client waiting on
//! one frame would otherwise read the first reply and leave the rest in the
//! buffer. The one case the specification calls out separately is an
//! **empty** array: "an Array with at least one value" is the requirement, so
//! `[]` is a single `-32600`, not an empty array back.

use std::future::Future as _;
use std::task::Poll;

use serde_json::{Map, Value};

use crate::application::bridge::content::ToolResultCodec;
use crate::application::bridge::forward::{Bridge, BridgeError, BridgePeer, SendFuture};
use crate::application::bridge::{DownstreamConfig, negotiate_downstream};
use crate::application::jsonrpc::{DynFrameSource, SourceError};
use crate::application::session::{DynFrameSink, SinkError};
use crate::domain::ir::JsonObject;
use crate::domain::{Agreement, DegradePolicy, Leg};

/// Which of the two sides had something to say.
#[derive(Debug)]
enum Woke {
    /// The client sent a frame, or `None` once its stream ended.
    Client(Result<Option<String>, SourceError>),
    /// The upstream peer has frames parked for the client.
    Peer,
}

/// A frame the bridge could not parse at all.
const PARSE_ERROR: i64 = -32_700;
/// A frame that parsed but is not a JSON-RPC message.
const INVALID_REQUEST: i64 = -32_600;
/// A method the two peers have no shared feature for.
const METHOD_NOT_FOUND: i64 = -32_601;
/// A transport or upstream fault.
const SERVER_ERROR: i64 = -32_000;

/// The handshake request a client opens a connection with.
///
/// A constant rather than a literal, because the loop branches on it in two
/// places — before and after the handshake — and a bridge that answered the
/// first and forwarded the second would be handing an `InitializeResult` to an
/// upstream that has no use for it.
const INITIALIZE: &str = "initialize";

/// The notification that closes a handshake.
const INITIALIZED: &str = "notifications/initialized";

/// What one pass of the loop moved.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LoopReport {
    /// Envelopes the upstream peer answered.
    pub forwarded: u64,
    /// Envelopes the upstream peer was told about and correctly sent nothing back for.
    pub notified: u64,
    /// Envelopes the upstream peer volunteered and the client was told about.
    ///
    /// Separate from [`forwarded`](Self::forwarded) and
    /// [`notified`](Self::notified) because they mean opposite things:
    /// `forwarded` counts what the bridge carried up and brought back,
    /// `notified` counts what it carried up and correctly got nothing for, and
    /// this counts what came *down* with no request behind it at all. A run
    /// that reports `relayed: 0` against a live session is a bridge that is
    /// losing half the protocol — the half that is not a request.
    pub relayed: u64,
    /// Envelopes the bridge refused or could not read, each answered to the client.
    pub failed: u64,
    /// Whether a client completed a handshake.
    ///
    /// Separate from the counters because it is the one number that says the
    /// bridge ever did anything: a run that forwarded nothing and failed
    /// nothing is either a client that connected and stayed quiet, or a client
    /// that never arrived, and the report has to tell those apart.
    pub handshaken: bool,
}

/// Why the loop stopped.
#[derive(Debug, thiserror::Error)]
pub enum LoopError {
    /// The downstream peer stopped being readable.
    ///
    /// A refused envelope is **not** here. Those are answered and counted in
    /// [`LoopReport::failed`]; only the pipe itself failing ends the loop.
    #[error("the downstream peer could not be read: {0}")]
    Source(#[from] SourceError),

    /// A reply did not reach the downstream peer.
    #[error("the reply could not be written to the downstream peer: {0}")]
    Sink(#[from] SinkError),

    /// An envelope the bridge itself built could not be written as text.
    ///
    /// Every value this loop serialises came out of JSON that already parsed,
    /// so it cannot happen — but the signature says it can, and an `expect`
    /// here would be a panic on the proxy's own output rather than on a peer's
    /// bytes, which is the one place a panic is least defensible.
    #[error("an envelope could not be written as text: {0}")]
    Encode(#[from] serde_json::Error),

    /// The upstream peer's out-of-band channel could not be opened.
    ///
    /// Its own variant because it is the one failure here that says nothing
    /// about the downstream pipe: every request already relayed still works, so
    /// a caller that wanted a degraded bridge can carry on, and one that did not
    /// now has the name of the thing that broke.
    #[error("the upstream peer's push channel could not be opened: {0}")]
    Listen(#[from] BridgeError),
}

/// What one envelope produced.
#[derive(Debug)]
enum Outcome {
    /// A reply for the downstream peer.
    Reply(Value),
    /// The upstream peer had nothing to say, which is correct for a notification.
    Silent,
}

/// The two peers a loop forwards between, and the policy it degrades under.
///
/// Not a half-built [`Bridge`] and not a way to make one: it is everything
/// true of a bridge before a client has said anything, gathered so
/// [`BridgeLoop::new`] takes four arguments rather than eight. The field order
/// matches [`Bridge::new`] — the same five facts, in the same sequence — so
/// moving between the two is not a re-reading exercise.
#[derive(Debug)]
pub struct Wiring<U, C> {
    upstream: Leg,
    policy: DegradePolicy,
    peer: U,
    codec: C,
}

impl<U, C> Wiring<U, C> {
    /// Wires a bridge to the upstream leg it negotiated when it connected.
    #[must_use]
    pub const fn new(upstream: Leg, policy: DegradePolicy, peer: U, codec: C) -> Self {
        Self {
            upstream,
            policy,
            peer,
            codec,
        }
    }
}

/// Carries frames from a downstream peer, through a [`Bridge`], back to it.
///
/// ## The handshake happens here, not upstream
///
/// A bridge is a server to the client in front of it, so the loop answers
/// `initialize` itself rather than letting the request reach the upstream. That
/// is what keeps the client on the revision the bridge chose and tells it only
/// the capabilities the bridge can actually serve; forwarding the handshake
/// would hand the client the upstream's own answers to both questions.
///
/// Before a handshake completes, a request is refused rather than forwarded —
/// the specification requires it, and a bridge that forwarded one would speak
/// for a client the upstream had never accepted.
///
/// `notifications/initialized` is answered locally and **not** forwarded. The
/// upstream session was initialised when the bridge connected, before any
/// client existed; forwarding this one would tell the upstream a second client
/// had arrived, describing a session it already considers open.
///
/// ## What a loop knows before a client arrives
///
/// Only one leg. The upstream negotiated with its server when the bridge
/// connected; the client that supplies the other leg has not shown up yet, and
/// inventing one would put a second opinion next to the real one. So
/// [`agreement`](Self::agreement) is `None` until a handshake completes, and a
/// caller reporting on a bridge can tell "these two sides share nothing" from
/// "no client ever arrived" — a distinction a placeholder leg would erase.
///
/// # Examples
///
/// ```
/// use protocol_droid::application::bridge::{BridgeLoop, ToolResultCodec, Wiring};
///
/// # fn documented<H, U, C, S, K>(
/// #     upstream: protocol_droid::domain::Leg,
/// #     config: protocol_droid::application::bridge::DownstreamConfig,
/// #     policy: protocol_droid::domain::DegradePolicy,
/// #     client: H,
/// #     server: U,
/// #     codec: C,
/// #     source: S,
/// #     sink: K,
/// # ) where
/// #     H: protocol_droid::application::bridge::BridgePeer,
/// #     U: protocol_droid::application::bridge::BridgePeer,
/// #     C: ToolResultCodec,
/// #     S: protocol_droid::application::jsonrpc::DynFrameSource,
/// #     K: protocol_droid::application::session::DynFrameSink,
/// # {
/// let wiring = Wiring::new(upstream, policy, server, codec);
/// let mut bridge_loop = BridgeLoop::new(wiring, config, source, sink);
/// # let _ = &mut bridge_loop;
/// # }
/// ```
#[derive(Debug)]
pub struct BridgeLoop<U, C, S, K>
where
    U: BridgePeer,
    C: ToolResultCodec,
    S: DynFrameSource,
    K: DynFrameSink,
{
    config: DownstreamConfig,
    wiring: Option<Wiring<U, C>>,
    live: Option<Box<Bridge<U, C>>>,
    source: S,
    sink: K,
}

impl<U, C, S, K> BridgeLoop<U, C, S, K>
where
    U: BridgePeer,
    C: ToolResultCodec,
    S: DynFrameSource,
    K: DynFrameSink,
{
    /// A loop waiting for a client's handshake.
    #[must_use]
    pub const fn new(wiring: Wiring<U, C>, config: DownstreamConfig, source: S, sink: K) -> Self {
        Self {
            config,
            wiring: Some(wiring),
            live: None,
            source,
            sink,
        }
    }

    /// What this bridge knows about its two sides.
    ///
    /// `None` until a handshake completes: there is one leg until then, and an
    /// agreement needs two. Inventing the missing one would put a second,
    /// fabricated opinion next to the real one.
    #[must_use]
    pub fn agreement(&self) -> Option<&Agreement> {
        self.live.as_deref().map(Bridge::agreement)
    }

    /// Whether a client has completed a handshake.
    #[must_use]
    pub const fn connected(&self) -> bool {
        self.live.is_some()
    }

    /// The bridge to forward through, once one exists.
    fn bridge(&self) -> Option<&Bridge<U, C>> {
        self.live.as_deref()
    }

    /// Answers one frame that arrived before any handshake completed.
    ///
    /// Two answers, both the specification's: `initialize` settles the leg and
    /// turns this loop into a live one, and everything else is refused with
    /// `-32600`. Nothing reaches the upstream, because the upstream has not
    /// agreed to speak to this client — the one refusal here that is not really
    /// about the bridge at all.
    fn handshake(
        &mut self,
        envelope: &Map<String, Value>,
        method: Option<&str>,
        report: &mut LoopReport,
    ) -> Outcome {
        match method {
            Some(name) if name == INITIALIZE => {}
            _ => {
                report.failed = report.failed.saturating_add(1);
                let id = envelope.get("id").cloned();
                return Outcome::Reply(error_envelope(
                    id,
                    INVALID_REQUEST,
                    "a client must complete the initialize handshake before sending anything else",
                ));
            }
        }

        // No wiring means the peers were moved into a bridge that this frame
        // cannot reach, which the dispatch above rules out. Refusing is the
        // honest answer to a state the loop should never be in, and it keeps a
        // synthetic agreement out of the code — a made-up upstream would let
        // this path answer with capabilities no peer ever agreed to.
        let Some(wiring) = self.wiring.as_ref() else {
            report.failed = report.failed.saturating_add(1);
            let id = envelope.get("id").cloned();
            return Outcome::Reply(error_envelope(
                id,
                SERVER_ERROR,
                "the bridge has no upstream peer to connect a client to",
            ));
        };

        let settled = envelope
            .get("params")
            .and_then(|value| JsonObject::from_value(value.clone()).ok());
        let downstream = match settled {
            Some(params) => negotiate_downstream(&params, &wiring.upstream, &self.config),
            None => Err(crate::application::bridge::DownstreamError::Malformed),
        };

        match downstream {
            Ok(downstream) => {
                let id = envelope.get("id").cloned();
                let reply = result_envelope(id, downstream.body().as_str());
                self.establish(&downstream);
                report.handshaken = true;
                Outcome::Reply(reply)
            }
            Err(error) => {
                report.failed = report.failed.saturating_add(1);
                let id = envelope.get("id").cloned();
                Outcome::Reply(error_envelope(id, INVALID_REQUEST, &error.to_string()))
            }
        }
    }

    /// Turns a settled downstream leg into the bridge this loop forwards through.
    ///
    /// The wiring is taken rather than borrowed: `Bridge` owns both peers, and
    /// a bridge that could forward while its wiring was still held would be two
    /// owners of one connection. A `None` means a handshake already established
    /// one, which the caller prevents by routing only un-handshaken frames here
    /// — so there is nothing to do and nothing to report.
    fn establish(&mut self, downstream: &crate::application::bridge::Downstream) {
        let Some(wiring) = self.wiring.take() else {
            return;
        };
        self.live = Some(Box::new(Bridge::new(
            downstream.agreement().clone(),
            wiring.policy,
            wiring.peer,
            wiring.codec,
        )));
    }

    /// Runs until the downstream peer's stream ends.
    ///
    /// The handshake is part of this, not a separate step the caller has to
    /// remember: a caller that forgot it would forward a client's `initialize`
    /// upstream and answer them with someone else's capabilities.
    ///
    /// # Errors
    ///
    /// [`LoopError`] when the downstream pipe itself fails, or when a reply the
    /// bridge built cannot be written. A refusal from the upstream peer is not
    /// an error here — it is answered to the client and counted in
    /// [`LoopReport::failed`].
    pub async fn run(&mut self) -> Result<LoopReport, LoopError> {
        let mut report = LoopReport::default();
        // Once per run, and only after a peer exists: asking before the
        // handshake would be asking a peer that has no session to open a
        // stream with.
        let mut listening = false;
        loop {
            if !listening && let Some(peer) = self.peer() {
                peer.listen().await?;
                listening = true;
            }
            // The pair is polled inside a block so the borrow `next_frame` takes
            // on the source ends before anything below takes `&mut self`. The
            // block yields *which* future finished, not the work done in
            // response; a select whose arms called `step` would need the source
            // and the whole loop at once, and the borrow checker was right to
            // refuse.
            let woke = {
                let mut pending = std::pin::pin!(self.wait_pending());
                let mut frame = std::pin::pin!(self.source.next_frame());
                std::future::poll_fn(|cx| {
                    if let Poll::Ready(value) = frame.as_mut().poll(cx) {
                        return Poll::Ready(Woke::Client(value));
                    }
                    if pending.as_mut().poll(cx).is_ready() {
                        return Poll::Ready(Woke::Peer);
                    }
                    Poll::Pending
                })
                .await
            };
            match woke {
                Woke::Client(Ok(Some(frame))) => {
                    self.step(&frame, &mut report).await?;
                    self.drain(&mut report).await?;
                }
                Woke::Client(Err(error)) => return Err(error.into()),
                Woke::Client(Ok(None)) => {
                    self.drain(&mut report).await?;
                    break;
                }
                Woke::Peer => self.drain(&mut report).await?,
            }
        }
        // The client stopped sending, which says nothing about whether the
        // server had finished pushing. Whatever arrived between the last
        // forward and the end of the stream still belongs to the client.
        self.drain(&mut report).await?;
        Ok(report)
    }

    /// Hands the client whatever the upstream pushed on its own.
    ///
    /// After each frame rather than alongside it, because the loop reads the
    /// client on one stream and the peer holds its own — there is no second
    /// source to select over. The consequence is stated rather than hidden: a
    /// notification that arrives while the client says nothing waits for the
    /// client's next frame. Pushing immediately would need the peer's read
    /// loop to be interruptible, and the two peers that buffer both hold a
    /// lock across a request for exactly the reason this cannot borrow one
    /// mid-call.
    async fn drain(&self, report: &mut LoopReport) -> Result<(), LoopError> {
        let Some(peer) = self.peer() else {
            return Ok(());
        };
        for envelope in peer.take_pending() {
            self.write(&Value::Object(envelope)).await?;
            report.relayed = report.relayed.saturating_add(1);
        }
        Ok(())
    }

    /// A future resolving when the peer has frames parked for the client.
    ///
    /// A loop with no peer yet has nothing to wait on, and gets the same
    /// never-resolving future a peer that opens no standing stream gets.
    fn wait_pending(&self) -> SendFuture<'static, ()> {
        self.peer().map_or_else(
            || Box::pin(std::future::pending()),
            BridgePeer::wait_pending,
        )
    }

    /// The upstream peer, once a client has connected to it.
    ///
    /// Through the live [`Bridge`] and not the wiring, which is where the peer
    /// sits before a handshake completes. That is a deliberate refusal to
    /// deliver early rather than a convenience: the only drain that runs before
    /// `establish` is the one after a frame that was *refused* for arriving
    /// pre-handshake, and handing that client a server notification would push
    /// a frame at it over a session it has not negotiated — the mirror of the
    /// rule the loop already applies to requests. A notification the server
    /// sent before any client arrived is held, not discarded, and goes out as
    /// soon as one has.
    fn peer(&self) -> Option<&U> {
        self.live.as_ref().map(|bridge| bridge.upstream())
    }

    /// Routes one inbound frame to whatever shape it turned out to have.
    async fn step(&mut self, frame: &str, report: &mut LoopReport) -> Result<(), LoopError> {
        let value: Value = match serde_json::from_str(frame) {
            Ok(value) => value,
            Err(error) => {
                report.failed = report.failed.saturating_add(1);
                return self.refuse(None, PARSE_ERROR, &error.to_string()).await;
            }
        };

        match value {
            Value::Array(members) => self.batch(&members, report).await,
            Value::Object(envelope) => {
                let outcome = self.dispatch(&envelope, report).await;
                self.deliver(outcome).await
            }
            // A JSON-RPC message is an object, or an array of them. A number, a
            // string or `null` is the peer sending something the protocol does
            // not define, and the specification's answer to that is a refusal.
            other => {
                report.failed = report.failed.saturating_add(1);
                self.refuse(
                    None,
                    INVALID_REQUEST,
                    &format!(
                        "a JSON-RPC message must be an object or an array of them, not {}",
                        kind(&other)
                    ),
                )
                .await
            }
        }
    }

    /// Forwards every element and answers with one array.
    ///
    /// An empty array is the case the specification separates out: the batch
    /// requirement is "an Array with at least one value", so `[]` is a single
    /// invalid-request object rather than an array back — an empty array would
    /// read to a client as "a batch of zero successful calls".
    async fn batch(&mut self, members: &[Value], report: &mut LoopReport) -> Result<(), LoopError> {
        if members.is_empty() {
            report.failed = report.failed.saturating_add(1);
            return self
                .refuse(
                    None,
                    INVALID_REQUEST,
                    "a JSON-RPC batch must carry at least one message",
                )
                .await;
        }

        let mut replies = Vec::new();
        for member in members {
            let outcome = match member {
                Value::Object(envelope) => self.dispatch(envelope, report).await,
                other => {
                    report.failed = report.failed.saturating_add(1);
                    Outcome::Reply(error_envelope(
                        None,
                        INVALID_REQUEST,
                        &format!(
                            "a JSON-RPC message must be an object or an array of them, not {}",
                            kind(other)
                        ),
                    ))
                }
            };
            if let Outcome::Reply(reply) = outcome {
                replies.push(reply);
            }
        }

        // Every element a notification: the batch has no answers, so it gets
        // no frame. Writing an empty array here would be the "invent an empty
        // reply" bug at batch granularity.
        if replies.is_empty() {
            return Ok(());
        }
        self.write(&Value::Array(replies)).await
    }

    /// Moves one envelope to the upstream peer and renders what came back.
    async fn dispatch(
        &mut self,
        envelope: &Map<String, Value>,
        report: &mut LoopReport,
    ) -> Outcome {
        let method = envelope.get("method").and_then(Value::as_str);

        // The client's half of a handshake this bridge answered itself. The
        // upstream session was initialised when the bridge connected, before
        // any client existed, so forwarding this would tell it a second client
        // had arrived describing a session it already considers open. Handled
        // here rather than in the pre-handshake path because this is the order
        // a conforming client actually sends it in.
        if method == Some(INITIALIZED) {
            report.notified = report.notified.saturating_add(1);
            return Outcome::Silent;
        }

        if self.live.is_none() {
            return self.handshake(envelope, method, report);
        }

        // A second `initialize` is a client that has forgotten it connected,
        // not a message with anywhere to go. Answering it would renegotiate a
        // session the bridge has already built a `Bridge` for, and the reply
        // would promise a profile the running bridge is not using.
        if method == Some(INITIALIZE) {
            report.failed = report.failed.saturating_add(1);
            let id = envelope.get("id").cloned();
            return Outcome::Reply(error_envelope(
                id,
                INVALID_REQUEST,
                "this connection has already completed its handshake",
            ));
        }

        // The only bridge to forward through. `expect`-free: the stage was
        // matched above and nothing between can have changed it, but a `None`
        // here is a bug in this function rather than in a peer's bytes, and a
        // panic in a proxy's own control flow is the least defensible place to
        // put one.
        let Some(bridge) = self.bridge() else {
            report.failed = report.failed.saturating_add(1);
            let id = envelope.get("id").cloned();
            return Outcome::Reply(error_envelope(
                id,
                SERVER_ERROR,
                "the bridge is not connected to an upstream peer",
            ));
        };

        match bridge.forward(envelope).await {
            Ok(forwarded) => {
                if forwarded.reply.is_empty() {
                    report.notified = report.notified.saturating_add(1);
                    return Outcome::Silent;
                }
                report.forwarded = report.forwarded.saturating_add(1);
                Outcome::Reply(Value::Object(forwarded.reply))
            }
            Err(error) => {
                report.failed = report.failed.saturating_add(1);
                let (code, message) = refusal(&error);
                let id = envelope.get("id").cloned();
                Outcome::Reply(error_envelope(id, code, message))
            }
        }
    }

    /// Writes whatever `dispatch` produced, if anything.
    async fn deliver(&self, outcome: Outcome) -> Result<(), LoopError> {
        match outcome {
            Outcome::Reply(reply) => self.write(&reply).await,
            Outcome::Silent => Ok(()),
        }
    }

    async fn write(&self, value: &Value) -> Result<(), LoopError> {
        self.sink
            .send(&serde_json::to_string(value)?)
            .await
            .map_err(LoopError::from)
    }

    /// Answers a frame that never became an envelope.
    async fn refuse(&self, id: Option<Value>, code: i64, message: &str) -> Result<(), LoopError> {
        self.write(&error_envelope(id, code, message)).await
    }
}

/// The JSON-RPC code a bridge-side failure carries downstream.
///
/// A refusal is an _answer_, not a failure: the client asked and got a reply.
/// `-32601` for a withheld feature is deliberate rather than a catch-all,
/// because it is the code that tells a client "this server does not offer
/// that", which is the one thing it can act on — retry elsewhere, or offer a
/// lower revision. `-32000` is reserved for a transport that genuinely failed,
/// where retrying the same call is the right instinct.
const fn refusal(error: &BridgeError) -> (i64, &'static str) {
    match error {
        BridgeError::FeatureUnavailable { .. } => (
            METHOD_NOT_FOUND,
            "the bridge cannot serve this method: the two peers share no feature for it",
        ),
        BridgeError::Unreachable { .. } => {
            (SERVER_ERROR, "the bridge could not reach the upstream peer")
        }
        BridgeError::Unreadable(_) => (INVALID_REQUEST, "the envelope is not a JSON-RPC message"),
        BridgeError::Close(_) => (SERVER_ERROR, "the connection did not close cleanly"),
        // The same code as a withheld feature, and for the same reason: the
        // only thing the client can do about either is negotiate a different
        // revision. A dedicated code would be inventing protocol to express a
        // distinction no client acts on.
        BridgeError::NotReadable(_) => (
            METHOD_NOT_FOUND,
            "the bridge cannot deliver this result at the revision you negotiated",
        ),
    }
}

/// Builds the result envelope a handshake is answered in.
///
/// The counterpart to [`error_envelope`], and deliberately built the same way:
/// the reply carries the request's own `id`, and `null` where there is none.
/// `result` is inserted as pre-parsed text rather than as a `Value` so the
/// negotiated body travels to the client byte-for-byte as the negotiation
/// built it, instead of through a second serialisation.
fn result_envelope(id: Option<Value>, body: &str) -> Value {
    let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let mut envelope = Map::new();
    envelope.insert("jsonrpc".to_owned(), Value::String("2.0".to_owned()));
    envelope.insert("id".to_owned(), id.unwrap_or(Value::Null));
    envelope.insert("result".to_owned(), parsed);
    Value::Object(envelope)
}

/// Builds the error envelope a refusal is delivered in.
///
/// The `id` is the request's own where there is one, and `null` where there is
/// not: a frame the bridge could not parse has no id to quote, and a response
/// carrying someone else's id is worse than one carrying none.
fn error_envelope(id: Option<Value>, code: i64, message: &str) -> Value {
    let mut detail = Map::new();
    detail.insert("code".to_owned(), Value::from(code));
    detail.insert("message".to_owned(), Value::String(message.to_owned()));

    let mut envelope = Map::new();
    envelope.insert("jsonrpc".to_owned(), Value::String("2.0".to_owned()));
    envelope.insert("id".to_owned(), id.unwrap_or(Value::Null));
    envelope.insert("error".to_owned(), Value::Object(detail));
    Value::Object(envelope)
}

/// The JSON type name, for saying what arrived instead of only what did not.
const fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::future::Future;
    use std::sync::{Arc, Mutex};

    use serde_json::{Map, Value, json};

    use crate::application::bridge::BridgeError;
    use crate::application::bridge::forward::SendFuture;
    use crate::application::jsonrpc::FrameSource;
    use crate::application::session::{FrameSink, SinkError};
    use crate::domain::{
        Agreement, DegradePolicy, Feature, FeatureProfile, Leg, ProtocolVersion, TransportKind,
    };

    use super::{BridgeLoop, DownstreamConfig, LoopError, LoopReport, SourceError, Wiring, kind};

    /// Every failure a test can hit, in one type, so `?` works throughout.
    #[derive(Debug, thiserror::Error)]
    enum Failure {
        /// A test double's mutex was poisoned by an earlier panic.
        #[error("a test double's mutex was poisoned")]
        Poisoned,
        /// The bridge refused or could not reach the upstream.
        #[error(transparent)]
        Bridge(#[from] BridgeError),
        /// The loop itself failed.
        #[error(transparent)]
        Loop(#[from] LoopError),
        /// A frame did not parse.
        #[error(transparent)]
        Json(#[from] serde_json::Error),
        /// A downstream leg could not be settled.
        #[error(transparent)]
        Downstream(#[from] crate::application::bridge::DownstreamError),
        /// A test's own complaint.
        #[error("{0}")]
        Claim(String),
    }

    /// An upstream that answers with the request's own id.
    ///
    /// The recording is shared rather than owned so a test can read it after the
    /// loop has dropped the peer into the [`Bridge`]. "What did the upstream
    /// see" is the whole question several tests here ask, and a double that can
    /// only be inspected while alive would answer it for nobody.
    #[derive(Clone, Default)]
    struct Upstream {
        seen: Arc<Mutex<Vec<Map<String, Value>>>>,
        pending: Arc<Mutex<VecDeque<Map<String, Value>>>>,
        push_on_send: Option<Map<String, Value>>,
    }

    impl Upstream {
        /// An upstream and the recording a test keeps hold of.
        fn recording() -> (Self, Seen) {
            let seen = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    seen: Arc::clone(&seen),
                    pending: Arc::new(Mutex::new(VecDeque::new())),
                    push_on_send: None,
                },
                Seen(seen),
            )
        }

        /// A handle on what this upstream was sent.
        ///
        /// The loop moves the peer into the live [`Bridge`] and the test still
        /// needs to read the recording afterwards, so the handle has to outlive
        /// the value it came from.
        fn seen(&self) -> Seen {
            Seen(Arc::clone(&self.seen))
        }

        /// An upstream already holding `frame`, before the loop starts.
        ///
        /// For the frames a server volunteers rather than answers. Holding one
        /// before the client has spoken anything is the case only the
        /// pre-handshake half of the drain can reach, and it is why the drain
        /// runs after the handshake frame rather than only once a request has
        /// been forwarded.
        fn holding(frame: &Map<String, Value>) -> Result<Self, Failure> {
            let (upstream, _) = Self::recording();
            upstream.hold(frame)?;
            Ok(upstream)
        }

        /// An upstream that holds `frame` every time it answers something.
        ///
        /// The mirror image: the frame is pending only once a request has
        /// already moved the peer out of the wiring and into the live
        /// [`Bridge`], so draining it proves the loop can still find the peer
        /// after the handshake moved it.
        fn pushing_on_send(frame: Map<String, Value>) -> Self {
            let (upstream, _) = Self::recording();
            Self {
                push_on_send: Some(frame),
                ..upstream
            }
        }

        /// Holds `frame` until something drains it.
        fn hold(&self, frame: &Map<String, Value>) -> Result<(), Failure> {
            let Ok(mut pending) = self.pending.lock() else {
                return Err(Failure::Poisoned);
            };
            pending.push_back(frame.clone());
            Ok(())
        }
    }

    /// A handle on what an upstream peer was sent.
    #[derive(Clone)]
    struct Seen(Arc<Mutex<Vec<Map<String, Value>>>>);

    impl Seen {
        fn methods(&self) -> Result<Vec<String>, Failure> {
            let seen = self.0.lock().map_err(|_| Failure::Poisoned)?;
            Ok(seen
                .iter()
                .filter_map(|envelope| envelope.get("method")?.as_str().map(String::from))
                .collect())
        }

        fn count(&self) -> Result<usize, Failure> {
            self.0
                .lock()
                .map(|seen| seen.len())
                .map_err(|_| Failure::Poisoned)
        }
    }

    impl crate::application::bridge::BridgePeer for Upstream {
        fn send(
            &self,
            envelope: Map<String, Value>,
        ) -> SendFuture<'_, Result<Map<String, Value>, BridgeError>> {
            Box::pin(async move {
                let id = envelope.get("id").cloned().unwrap_or(Value::Null);
                let Ok(mut seen) = self.seen.lock() else {
                    return Err(BridgeError::Close("upstream poisoned".to_owned()));
                };
                seen.push(envelope);
                drop(seen);
                // A server pushing mid-call is why the queue exists: the frame
                // is held *while* this call is in flight, which is exactly
                // where the real peers hold theirs.
                if let Some(frame) = self.push_on_send.clone()
                    && let Ok(mut pending) = self.pending.lock()
                {
                    pending.push_back(frame);
                }
                let mut reply = Map::new();
                reply.insert("jsonrpc".into(), json!("2.0"));
                reply.insert("id".into(), id);
                reply.insert("result".into(), json!({"ok": true}));
                Ok(reply)
            })
        }

        fn notify(&self, envelope: Map<String, Value>) -> SendFuture<'_, Result<(), BridgeError>> {
            Box::pin(async move {
                let Ok(mut seen) = self.seen.lock() else {
                    return Err(BridgeError::Close("upstream poisoned".to_owned()));
                };
                seen.push(envelope);
                Ok(())
            })
        }

        fn close(&self) -> SendFuture<'_, Result<(), BridgeError>> {
            Box::pin(async { Ok(()) })
        }

        /// The frames the server volunteered, which the trait default drops.
        ///
        /// Without this the double would agree with the default, and a test
        /// would pass against a loop that never drained anything — proving
        /// only that a queue nobody reads is a queue nobody reads.
        fn take_pending(&self) -> Vec<Map<String, Value>> {
            let Ok(mut pending) = self.pending.lock() else {
                return Vec::new();
            };
            pending.drain(..).collect()
        }
    }

    /// Hands over a fixed list of frames, then reports end-of-stream.
    #[derive(Default)]
    struct Scripted {
        frames: Mutex<VecDeque<String>>,
        witness: Option<Arc<Mutex<Vec<Vec<String>>>>>,
        sink: Option<Arc<Mutex<Vec<String>>>>,
    }

    impl Scripted {
        fn of<S: AsRef<str>>(frames: &[S]) -> Self {
            let queue = frames
                .iter()
                .map(|frame| frame.as_ref().to_owned())
                .collect();
            Self {
                frames: Mutex::new(queue),
                witness: None,
                sink: None,
            }
        }

        /// [`of`](Self::of), recording what the client had been sent each time
        /// it asked for another frame.
        ///
        /// The point of the witness is timing. A loop that drains at the very
        /// end and one that drains after every frame deliver the same frames in
        /// the same order, so any assertion about the final sink contents calls
        /// them identical — and a bridge that only drained at the end would
        /// relay nothing at all for a client that never closes its stream,
        /// which is every client that stays connected. Reading the sink at the
        /// moment the client asks its next question is the only way to tell
        /// them apart.
        fn witnessed<S: AsRef<str>>(
            frames: &[S],
            sink: &Arc<Mutex<Vec<String>>>,
        ) -> (Self, Arc<Mutex<Vec<Vec<String>>>>) {
            let witness = Arc::new(Mutex::new(Vec::new()));
            let mut scripted = Self::of(frames);
            scripted.witness = Some(Arc::clone(&witness));
            scripted.sink = Some(Arc::clone(sink));
            (scripted, witness)
        }

        /// Records what the client had been sent, as of right now.
        ///
        /// Its own function so the guards are dropped before it returns —
        /// `next_frame` hands back a `Ready`, not a `Result`, so it cannot use
        /// `?` and must not hold two locks across the record.
        fn mark(
            witness: &Mutex<Vec<Vec<String>>>,
            sink: &Mutex<Vec<String>>,
        ) -> Result<(), SourceError> {
            let written = sink.lock().map_err(|_| SourceError::Transport)?.clone();
            witness
                .lock()
                .map_err(|_| SourceError::Transport)?
                .push(written);
            Ok(())
        }
    }

    impl FrameSource for Scripted {
        fn next_frame(&self) -> impl Future<Output = Result<Option<String>, SourceError>> + Send {
            // Not `async`, and deliberately: there is nothing to await, and an
            // `async` body here holds the guard across a suspension point the
            // clippy lints would rightly object to. Taking the frame inside
            // `map` drops the guard before the future is ever built.
            let next = self
                .frames
                .lock()
                .map(|mut frames| frames.pop_front())
                .map_err(|_| SourceError::Transport);
            // After the guard above, so a poisoned witness never leaves the
            // sink locked. A failure to record is treated as a failure to read:
            // a test that cannot witness the wire cannot be trusted when it
            // asserts what the wire carried.
            let witnessed = match (&self.witness, &self.sink) {
                (Some(witness), Some(sink)) => Self::mark(witness, sink),
                _ => Ok(()),
            };
            std::future::ready(witnessed.and(next))
        }
    }

    /// Keeps every frame written, so a test can read the wire back.
    ///
    /// The recording is behind an `Arc` rather than the sink itself, because
    /// `Arc<Written>` satisfies neither `FrameSink` nor `DynFrameSink` — the
    /// blanket impls cover `T: FrameSink`, and an `Arc` is not one.
    #[derive(Clone, Default)]
    struct Written {
        frames: Arc<Mutex<Vec<String>>>,
    }

    impl FrameSink for Written {
        fn send(&self, frame: &str) -> impl Future<Output = Result<(), SinkError>> + Send {
            let pushed = self
                .frames
                .lock()
                .map(|mut frames| frames.push(frame.to_owned()))
                .map_err(|_| SinkError);
            std::future::ready(pushed)
        }
    }

    fn serving(features: &[Feature]) -> FeatureProfile {
        features
            .iter()
            .fold(FeatureProfile::empty(), |profile, f| profile.with(*f))
    }

    /// An agreement where both peers offer exactly `features`.
    ///
    /// At the newest revision that still has an `initialize` handshake. The
    /// build's newest revision has none — SEP-2575 removed it — so an
    /// agreement there could never be settled with a client, and every test
    /// built on one would be exercising a path no client can take.
    fn agreeing(features: &[Feature]) -> Agreement {
        let version = ProtocolVersion::V2025_11_25;
        Agreement::between(
            Leg::new(version, serving(features)),
            Leg::new(version, serving(features)),
        )
    }

    fn written(recording: &Arc<Mutex<Vec<String>>>) -> Result<Vec<Value>, Failure> {
        let frames = recording.lock().map_err(|_| Failure::Poisoned)?;
        frames
            .iter()
            .map(|frame| Ok(serde_json::from_str(frame)?))
            .collect()
    }

    /// The `initialize` a scripted client opens with.
    fn handshake_frame(version: ProtocolVersion) -> String {
        format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"{version}","capabilities":{{}},"clientInfo":{{"name":"scripted"}}}}}}"#
        )
    }

    /// Everything one scripted run produced.
    struct Session {
        report: LoopReport,
        /// Every frame written, the handshake reply first when one happened.
        written: Vec<Value>,
        /// What the upstream peer was sent.
        seen: Seen,
    }

    impl Session {
        /// The replies to the test's own frames, without the handshake's.
        fn after_handshake(self) -> Vec<Value> {
            self.written.into_iter().skip(1).collect()
        }
    }

    /// Runs one loop over `frames`, behind an opening handshake unless
    /// `handshake` says otherwise.
    async fn serve(
        agreement: &Agreement,
        frames: &[&str],
        handshake: bool,
    ) -> Result<Session, Failure> {
        let (upstream, _) = Upstream::recording();
        serve_with(agreement, frames, handshake, upstream).await
    }

    /// [`serve`], with the upstream supplied rather than built.
    ///
    /// The peer a test needs to reason about is not always a plain recorder: a
    /// test about frames the server pushes needs one that holds them, and
    /// building that into `serve` would mean every test opting out of a
    /// default it has no use for.
    async fn serve_with(
        agreement: &Agreement,
        frames: &[&str],
        handshake: bool,
        upstream: Upstream,
    ) -> Result<Session, Failure> {
        serve_witnessed(agreement, frames, handshake, upstream)
            .await
            .map(|(session, _)| session)
    }

    /// [`serve_with`], also handing back what the client had been sent each
    /// time it asked for another frame.
    ///
    /// The witness is a record of *timing*, not of contents: it says what was
    /// already on the wire each time the client came back for more. A test
    /// about what a bridge delivers should read the session; a test about
    /// *when* should read this, because only it can tell a loop that relays as
    /// it goes from one that relays everything at the end — the two produce
    /// identical sink contents and differ in every way a client would notice.
    async fn serve_witnessed(
        agreement: &Agreement,
        frames: &[&str],
        handshake: bool,
        upstream: Upstream,
    ) -> Result<(Session, Arc<Mutex<Vec<Vec<String>>>>), Failure> {
        let recording: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen = upstream.seen();

        let mut script = Vec::new();
        if handshake {
            script.push(handshake_frame(agreement.upstream().version));
        }
        script.extend(frames.iter().map(|frame| (*frame).to_owned()));

        let (source, witness) = Scripted::witnessed(&script, &recording);

        let mut loop_ = BridgeLoop::new(
            Wiring::new(
                *agreement.upstream(),
                DegradePolicy::default(),
                upstream,
                crate::application::bridge::content::doubles::Counting::reading_nothing(),
            ),
            DownstreamConfig::new("bridge", "0.1.0", TransportKind::Stdio)?,
            source,
            Written {
                frames: recording.clone(),
            },
        );

        let report = loop_.run().await?;
        if handshake && !report.handshaken {
            return Err(Failure::Claim(
                "the loop never completed a handshake".to_owned(),
            ));
        }
        let session = Session {
            report,
            written: written(&recording)?,
            seen,
        };
        Ok((session, witness))
    }

    /// Runs one loop: a handshake, then `frames`.
    ///
    /// Returns the report and everything written *after* the handshake reply,
    /// which is what a test about forwarding cares about. The handshake itself
    /// is asserted rather than trusted — a loop that never connected would
    /// otherwise leave every such assertion passing for the wrong reason.
    async fn run(
        agreement: &Agreement,
        frames: &[&str],
    ) -> Result<(LoopReport, Vec<Value>), Failure> {
        let session = serve(agreement, frames, true).await?;
        Ok((session.report, session.after_handshake()))
    }

    /// A request is answered, with the id the client used.
    #[tokio::test]
    async fn a_request_is_forwarded_and_its_reply_written() -> Result<(), Failure> {
        let (report, replies) = run(
            &agreeing(&[Feature::Tools]),
            &[r#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#],
        )
        .await?;

        assert_eq!(report.forwarded, 1);
        assert_eq!(replies.len(), 1);
        let Some(reply) = replies.first() else {
            return Err(Failure::Poisoned);
        };
        assert_eq!(reply.get("id"), Some(&json!(7)));
        assert!(reply.get("result").is_some(), "the reply lost its result");
        Ok(())
    }

    /// A notification produces no frame at all.
    ///
    /// A conforming peer answers one with `202` and no body, so writing an empty
    /// reply would put a response on the wire the server never sent.
    #[tokio::test]
    async fn a_notification_writes_nothing() -> Result<(), Failure> {
        let (report, replies) = run(
            &agreeing(&[Feature::Tools]),
            &[r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#],
        )
        .await?;

        assert_eq!(report.notified, 1);
        assert_eq!(report.forwarded, 0);
        assert!(replies.is_empty(), "a notification produced a frame");
        Ok(())
    }

    /// A frame the server volunteered reaches the client *while it is still
    /// connected*.
    ///
    /// The sharp test here, and the one that took a second attempt to write.
    /// Asserting that the notification is in the sink at the end is not enough:
    /// a loop that drained only after the client's stream ended would pass that,
    /// and would relay nothing at all for a client that never closes — which is
    /// every client that stays connected, and the only kind a bridge is run
    /// for. So this reads the sink at the moment the client asks its *next*
    /// question, and requires the notification to be there already.
    ///
    /// `notifications/tools/list_changed` because that is the one a client acts
    /// on: a tool registered upstream mid-session is invisible to it otherwise,
    /// which is half of what the project calls DYNAMIC.
    #[tokio::test]
    async fn a_frame_pushed_before_any_forward_reaches_a_still_connected_client()
    -> Result<(), Failure> {
        let (session, witness) = serve_witnessed(
            &agreeing(&[Feature::Tools]),
            &[r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#],
            true,
            Upstream::holding(&pushed_frame("notifications/tools/list_changed")?)?,
        )
        .await?;

        assert_eq!(session.report.relayed, 1);
        assert_eq!(session.report.forwarded, 1);
        assert_eq!(
            relayed_method(&session.written)?,
            Some("notifications/tools/list_changed"),
            "the frame that reached the client was not the one the server pushed, and one \
             that was not pushed is not there at all"
        );
        // The client's second frame is the `ping`. By the time it asked for it,
        // the notification had to be on the wire — not queued behind the end of
        // the stream.
        let marks = witness.lock().map_err(|_| Failure::Poisoned)?.clone();
        let before_ping = marks
            .iter()
            .rev()
            .find(|mark| !mark.is_empty())
            .ok_or_else(|| {
                Failure::Claim("the client was never sent anything at all".to_owned())
            })?;
        assert!(
            before_ping
                .iter()
                .any(|frame| frame.contains("notifications/tools/list_changed")),
            "the notification was still queued when the client asked its next question, so a \
             client that stayed connected would never have seen it: the loop relays only at \
             the end of the stream"
        );
        Ok(())
    }

    /// The same frame still arrives once the handshake has moved the peer.
    ///
    /// [`establish`](crate::application::bridge::BridgeLoop) takes the wiring's
    /// peer and drops it into the live [`Bridge`], so a drain that consulted
    /// only the wiring would look for a peer that is no longer there and relay
    /// nothing. This is the test for that: the notification is held during the
    /// very forward that causes the move.
    #[tokio::test]
    async fn a_frame_pushed_during_a_forward_reaches_the_client() -> Result<(), Failure> {
        let session = serve_with(
            &agreeing(&[Feature::Tools]),
            &[r#"{"jsonrpc":"2.0","id":9,"method":"tools/list"}"#],
            true,
            Upstream::pushing_on_send(pushed_frame("notifications/message")?),
        )
        .await?;

        assert_eq!(
            session.report.relayed, 1,
            "the peer had moved into the live bridge and the drain could not find it there"
        );
        assert_eq!(session.report.forwarded, 1);
        assert_eq!(
            relayed_method(&session.written)?,
            Some("notifications/message"),
            "the frame that arrived was not the one the server volunteered"
        );
        Ok(())
    }

    /// A server's notification reaches only a client that has handshaken.
    ///
    /// The first frame here is a request, which the loop refuses because no
    /// handshake has completed — and the server has a notification waiting the
    /// whole time. Delivering it would push a frame at a client over a session
    /// it has not negotiated, the mirror of the rule the loop already applies
    /// to requests in the other direction. It is held rather than discarded, and
    /// a later drain would still send it.
    #[tokio::test]
    async fn a_frame_pushed_before_any_handshake_waits_for_one() -> Result<(), Failure> {
        let session = serve_with(
            &agreeing(&[Feature::Tools]),
            &[r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#],
            false,
            Upstream::holding(&pushed_frame("notifications/tools/list_changed")?)?,
        )
        .await?;

        assert!(
            !session.report.handshaken,
            "the loop connected without a handshake"
        );
        assert_eq!(
            session.report.relayed, 0,
            "a server notification reached a client that had never completed an initialize"
        );
        assert_eq!(
            session.report.failed, 1,
            "the pre-handshake request should have been refused, not forwarded"
        );
        Ok(())
    }

    /// A server's notification, as the envelope the peers exchange it.
    fn pushed_frame(method: &str) -> Result<Map<String, Value>, Failure> {
        json!({"jsonrpc": "2.0", "method": method})
            .as_object()
            .cloned()
            .ok_or_else(|| Failure::Claim("the pushed fixture is not an object".to_owned()))
    }

    /// The method of the one frame in `written` that carries one.
    ///
    /// Returns `Ok(None)` when there is none, so a test can say "nothing was
    /// relayed" without the assertion itself being the thing that fails on an
    /// unrelated index.
    fn relayed_method(written: &[Value]) -> Result<Option<&str>, Failure> {
        let mut found = written
            .iter()
            .filter_map(|frame| frame.get("method").and_then(Value::as_str));
        match (found.next(), found.next()) {
            (Some(method), None) => Ok(Some(method)),
            _ => Err(Failure::Claim(
                "expected exactly one relayed frame, found a different number".to_owned(),
            )),
        }
    }

    /// A feature the peers do not share is refused in an answer, not a dead loop.
    ///
    /// This is the test the module exists for. Killing the loop would report
    /// "the bridge is down" for a call the bridge understood perfectly well and
    /// declined for a stated reason.
    #[tokio::test]
    async fn a_withheld_feature_is_answered_and_the_loop_continues() -> Result<(), Failure> {
        let (report, replies) = run(
            &agreeing(&[]),
            &[
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"echo"}}"#,
                r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#,
            ],
        )
        .await?;

        assert_eq!(report.failed, 1);
        assert_eq!(replies.len(), 2, "the refusal ended the loop");

        let Some(refused) = replies.first() else {
            return Err(Failure::Poisoned);
        };
        assert_eq!(refused.get("id"), Some(&json!(1)));
        assert_eq!(
            refused.pointer("/error/code").and_then(Value::as_i64),
            Some(-32601)
        );

        // The second call went through, which is the half that matters: the
        // bridge is healthy and says so by answering.
        let Some(served) = replies.get(1) else {
            return Err(Failure::Poisoned);
        };
        assert_eq!(served.get("id"), Some(&json!(2)));
        assert!(served.get("result").is_some());
        Ok(())
    }

    /// A frame that is not JSON is answered, and the next frame still lands.
    #[tokio::test]
    async fn an_unparseable_frame_is_answered_and_the_loop_continues() -> Result<(), Failure> {
        let (report, replies) = run(
            &agreeing(&[Feature::Tools]),
            &["{not json", r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#],
        )
        .await?;

        assert_eq!(report.forwarded, 1);
        assert_eq!(replies.len(), 2);
        let Some(refused) = replies.first() else {
            return Err(Failure::Poisoned);
        };
        assert_eq!(
            refused.pointer("/error/code").and_then(Value::as_i64),
            Some(-32700)
        );
        // No id to quote, so the answer carries none rather than someone else's.
        assert_eq!(refused.get("id"), Some(&Value::Null));
        Ok(())
    }

    /// An empty batch is one invalid request, not an empty array back.
    ///
    /// The specification's requirement is "an Array with at least one value",
    /// so `[]` is the one batch shape answered with a single object. An empty
    /// array would read to a client as a batch of zero successful calls.
    #[tokio::test]
    async fn an_empty_batch_is_a_single_invalid_request() -> Result<(), Failure> {
        let (_report, replies) = run(&agreeing(&[Feature::Tools]), &["[]"]).await?;

        assert_eq!(replies.len(), 1);
        let Some(refused) = replies.first() else {
            return Err(Failure::Poisoned);
        };
        assert!(refused.is_object(), "an empty batch was not refused");
        assert_eq!(
            refused.pointer("/error/code").and_then(Value::as_i64),
            Some(-32600)
        );
        Ok(())
    }

    /// A batch is answered with one array, in one frame.
    ///
    /// A client waiting on a single frame would read the first reply and leave
    /// the rest sitting in its buffer.
    #[tokio::test]
    async fn a_batch_is_answered_with_one_array() -> Result<(), Failure> {
        let (report, replies) = run(
            &agreeing(&[Feature::Tools]),
            &[r#"[{"jsonrpc":"2.0","id":1,"method":"ping"},
                 {"jsonrpc":"2.0","id":2,"method":"ping"}]"#],
        )
        .await?;

        assert_eq!(report.forwarded, 2);
        assert_eq!(
            replies.len(),
            1,
            "a batch answered with more than one frame"
        );
        let Some(Value::Array(members)) = replies.first() else {
            return Err(Failure::Poisoned);
        };
        assert_eq!(members.len(), 2);
        Ok(())
    }

    /// A frame that is valid JSON but not a message names what arrived.
    #[tokio::test]
    async fn a_frame_that_is_not_a_message_is_refused() -> Result<(), Failure> {
        let (_report, replies) = run(&agreeing(&[Feature::Tools]), &["42"]).await?;

        let Some(refused) = replies.first() else {
            return Err(Failure::Poisoned);
        };
        assert_eq!(
            refused.pointer("/error/code").and_then(Value::as_i64),
            Some(-32600)
        );
        let message = refused.pointer("/error/message").and_then(Value::as_str);
        assert!(
            message.is_some_and(|text| text.contains(kind(&Value::from(42)))),
            "the refusal did not say what arrived"
        );
        Ok(())
    }

    /// End of stream ends the loop and reports what it moved.
    #[tokio::test]
    async fn end_of_stream_ends_the_loop() -> Result<(), Failure> {
        let (report, replies) = run(&agreeing(&[Feature::Tools]), &[]).await?;

        assert!(report.handshaken, "the client connected and said nothing");
        assert_eq!(report.forwarded, 0);
        assert_eq!(report.notified, 0);
        assert_eq!(report.failed, 0);
        assert!(replies.is_empty());
        Ok(())
    }

    /// The whole reason the loop grew a handshake: the bridge answers
    /// `initialize` itself, and the upstream never sees one.
    ///
    /// A bridge that forwarded the handshake would answer the client with the
    /// upstream's revision and the upstream's capabilities — which is a
    /// pass-through wearing a proxy's name, and tells the client nothing about
    /// whether *this* bridge can serve them.
    #[tokio::test]
    async fn the_bridge_answers_initialize_itself() -> Result<(), Failure> {
        let session = serve(&agreeing(&[Feature::Tools]), &[], true).await?;

        assert!(session.report.handshaken, "no handshake was recorded");
        assert_eq!(
            session.seen.count()?,
            0,
            "the upstream saw the handshake: {:?}",
            session.seen.methods()?
        );

        let name = session
            .written
            .first()
            .and_then(|reply| reply.get("result"))
            .and_then(|result| result.get("serverInfo"))
            .and_then(|info| info.get("name"))
            .and_then(Value::as_str);
        assert_eq!(name, Some("bridge"), "{:?}", session.written);
        Ok(())
    }

    /// The reply carries the id the client used, like every other reply.
    #[tokio::test]
    async fn the_handshake_reply_carries_the_requests_own_id() -> Result<(), Failure> {
        let session = serve(&agreeing(&[Feature::Tools]), &[], true).await?;
        let id = session
            .written
            .first()
            .and_then(|reply| reply.get("id"))
            .cloned();
        assert_eq!(id, Some(json!(1)), "{:?}", session.written);
        Ok(())
    }

    /// A request that arrives before any handshake is refused, not forwarded.
    ///
    /// The specification requires it, and the reason is the one that matters:
    /// the upstream has never accepted this client, so forwarding would speak
    /// for somebody it has not met.
    #[tokio::test]
    async fn a_request_before_the_handshake_is_refused() -> Result<(), Failure> {
        let session = serve(
            &agreeing(&[Feature::Tools]),
            &[r#"{"jsonrpc":"2.0","id":4,"method":"tools/list"}"#],
            false,
        )
        .await?;

        assert!(!session.report.handshaken);
        assert_eq!(
            session.seen.count()?,
            0,
            "a pre-handshake request reached the upstream"
        );
        let code = session
            .written
            .first()
            .and_then(|reply| reply.get("error"))
            .and_then(|error| error.get("code"))
            .and_then(Value::as_i64);
        assert_eq!(code, Some(-32_600), "{:?}", session.written);
        Ok(())
    }

    /// The loop still accepts a handshake after a refusal.
    ///
    /// A client whose first frame was malformed, or which sent a request too
    /// early, is not poisoned for it — a proxy that killed the connection on a
    /// recoverable mistake would fail the one client most likely to make one.
    #[tokio::test]
    async fn a_handshake_after_a_refusal_still_connects() -> Result<(), Failure> {
        let session = serve(
            &agreeing(&[Feature::Tools]),
            &[
                r#"{"jsonrpc":"2.0","id":4,"method":"tools/list"}"#,
                &handshake_frame(ProtocolVersion::latest_known()),
            ],
            false,
        )
        .await?;

        assert!(session.report.handshaken, "the second frame was ignored");
        Ok(())
    }

    /// `notifications/initialized` is consumed by the bridge, not forwarded.
    ///
    /// The upstream session was initialised when the bridge connected, before
    /// any client existed. Forwarding this one would tell the upstream a second
    /// client had arrived, describing a session it already considers open.
    #[tokio::test]
    async fn the_initialized_notification_is_not_forwarded() -> Result<(), Failure> {
        let session = serve(
            &agreeing(&[Feature::Tools]),
            &[r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#],
            true,
        )
        .await?;

        assert_eq!(
            session.seen.methods()?,
            Vec::<String>::new(),
            "the bridge forwarded a notification about a session it owns"
        );
        assert_eq!(session.written.len(), 1, "a notification was answered");
        Ok(())
    }

    /// A client that handshakes twice is refused the second time.
    ///
    /// The first handshake is what the running bridge was built from. Answering
    /// a second would renegotiate a session the forwarding path is already
    /// committed to, and the reply would promise a profile that path is not
    /// using.
    #[tokio::test]
    async fn a_second_handshake_is_refused() -> Result<(), Failure> {
        let session = serve(
            &agreeing(&[Feature::Tools]),
            &[r#"{"jsonrpc":"2.0","id":9,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"p"}}}"#],
            true,
        )
        .await?;

        let second = session.written.get(1);
        let code = second
            .and_then(|reply| reply.get("error"))
            .and_then(|error| error.get("code"))
            .and_then(Value::as_i64);
        assert_eq!(code, Some(-32_600), "{:?}", session.written);
        assert_eq!(
            session.report.forwarded, 0,
            "a renegotiating client put something on the wire"
        );
        Ok(())
    }

    /// A client offering a revision the build cannot speak is walked back.
    ///
    /// `2026-07-28` removed `initialize` entirely (SEP-2575), so answering it
    /// is impossible; the client gets the newest revision that still has a
    /// handshake instead, which is what the negotiation is for.
    #[tokio::test]
    async fn a_client_offering_a_handshakeless_revision_is_walked_back() -> Result<(), Failure> {
        let session = serve(
            &agreeing(&[Feature::Tools]),
            &[r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"p"}}}"#],
            false,
        )
        .await?;

        let answered = session
            .written
            .first()
            .and_then(|reply| reply.get("result"))
            .and_then(|result| result.get("protocolVersion"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let Some(answered) = answered else {
            return Err(Failure::Claim("the reply named no revision".to_owned()));
        };
        let parsed = answered
            .parse::<ProtocolVersion>()
            .map_err(|e| Failure::Claim(format!("the bridge answered {answered:?}: {e}")))?;
        assert!(
            crate::domain::handshake_defined_in(parsed),
            "the bridge answered with {answered:?}, which has no initialize to answer"
        );
        assert_ne!(
            parsed,
            ProtocolVersion::V2026_07_28,
            "the client was walked forward to the revision that removed the handshake"
        );
        Ok(())
    }

    /// A client offering an older revision gets that revision, not the newest.
    ///
    /// The bridge is the version-translating half of this project; answering a
    /// `2024-11-05` client with a newer revision would be the one thing it
    /// must never do.
    #[tokio::test]
    async fn an_older_client_keeps_its_revision() -> Result<(), Failure> {
        let session = serve(
            &agreeing(&[Feature::Tools]),
            &[r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"p"}}}"#],
            false,
        )
        .await?;

        let answered = session
            .written
            .first()
            .and_then(|reply| reply.get("result"))
            .and_then(|result| result.get("protocolVersion"))
            .and_then(Value::as_str);
        assert_eq!(answered, Some("2024-11-05"), "{:?}", session.written);
        Ok(())
    }

    /// The advertisement is the agreement, not the revision's full capability
    /// list.
    ///
    /// `agreeing(&[Feature::Tools])` is an upstream that offered tools and
    /// nothing else, so a reply advertising anything more is a bridge promising
    /// to forward something its upstream never agreed to.
    #[tokio::test]
    async fn the_advertisement_is_the_upstreams_not_the_revisions() -> Result<(), Failure> {
        let session = serve(&agreeing(&[Feature::Tools]), &[], true).await?;

        let capabilities = session
            .written
            .first()
            .and_then(|reply| reply.get("result"))
            .and_then(|result| result.get("capabilities"));
        let Some(capabilities) = capabilities else {
            return Err(Failure::Claim("the reply advertised nothing".to_owned()));
        };
        assert!(
            capabilities.get("tools").is_some(),
            "the one agreed feature is missing: {capabilities}"
        );
        assert_eq!(
            capabilities.get("resources"),
            None,
            "the bridge advertised a feature the upstream never agreed to: {capabilities}"
        );
        Ok(())
    }

    /// Every refusal carries the request's own id, so a client can match it.
    #[tokio::test]
    async fn a_refusal_answers_with_the_requests_own_id() -> Result<(), Failure> {
        let (_report, replies) = run(
            &agreeing(&[]),
            &[r#"{"jsonrpc":"2.0","id":"call-9","method":"tools/list"}"#],
        )
        .await?;

        let Some(refused) = replies.first() else {
            return Err(Failure::Poisoned);
        };
        assert_eq!(refused.get("id"), Some(&json!("call-9")));
        Ok(())
    }
}
