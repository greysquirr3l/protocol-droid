//! The loop: read a frame, route it, answer it, until the peer leaves.
//!
//! # What this is
//!
//! The half of the stdio transport that knows there is a conversation. It owns
//! the read/answer/again cycle, decides what a frame that will not decode means
//! for the connection, and is the only place in the crate that both holds a
//! peer's bytes and decides what they mean. Everything below it —
//! [`StdioSource`], [`StdioSink`], [`Diagnostics`] — is a byte layer that knows
//! nothing about JSON-RPC, and the reply document itself lives in
//! [`reply`].
//!
//! # stdout carries messages and nothing else
//!
//! The specification requires that a stdio server's standard output contain
//! MCP messages only, because the peer is parsing that stream: anything else on
//! it arrives as a frame that does not decode. Every human-readable fact this
//! loop has — what it refused, why, and that the peer left cleanly — goes to
//! [`Diagnostics`], which the constructor takes as an argument precisely so
//! that it is a decision rather than an accident.
//!
//! The failure this prevents is silent, which is why
//! `stdout_carries_nothing_but_replies` exists. A `println!` anywhere in this
//! path corrupts a real peer's stream and no unit test notices, because a unit
//! test asserting "the reply is correct" never looks at the bytes around the
//! reply. That test reads the whole of stdout, splits it into frames, and
//! requires every one of them to be a well-formed reply.
//!
//! # A malformed frame is answered, not fatal
//!
//! The decision this module had to make, and the one a reviewer will look for:
//! **a frame that arrives complete but does not decode is answered with the
//! JSON-RPC error [`DecodeError::code`] names, and the loop continues.**
//!
//! The alternative — drop the connection — is defensible, and for a stream
//! protocol with length prefixes it is usually the right call. It is the wrong
//! call *here*, for one reason: on a newline-delimited transport a frame that
//! reached [`decode`] has already been delimited. The framing layer found its
//! terminator, so the next frame starts at a byte offset this loop already
//! knows. There is no desynchronisation to recover from, and nothing about the
//! failure says the peer's *next* message will be any worse — one malformed
//! frame is a bug in one sender, and a server that hangs up on it converts a
//! recoverable mistake into a dead session for every later, well-formed
//! request.
//!
//! What is fatal is the other failure: a [`SourceError`]. That is the stream
//! itself, not a message in it, and [`StdioSource`] has already latched it —
//! the bytes that would let us continue are gone.
//!
//! # The id is the peer's, all the way through
//!
//! The inbound frame is decoded to a [`WireId`], that same value is what
//! [`RequestId`] is built from for the handler's [`RequestContext`], and the
//! outbound document is built from the [`WireId`] again. Nothing re-derives it,
//! nothing stringifies it on the way, and [`reply`] re-escapes it as JSON
//! rather than debug-printing it. A reply whose id differs from its request's
//! by even a character is a request the peer can never match to an answer.
//!
//! # Protocol neutrality
//!
//! This module names no method. `ping`, `tools/list` and `tools/call` all
//! arrive as an opaque `&str` and are handed to
//! [`Dispatcher::dispatch`](crate::application::server::Dispatcher::dispatch),
//! which is the MCP-facing layer. Batching is the one protocol *shape* decision
//! taken here, and it is delegated rather than made: the loop holds a
//! [`BatchPolicy`] and asks it against the session's profile, because whether
//! a batch is legal is a negotiation outcome and not a transport fact.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};

use super::exchange::{StdioExchange, answer_of};
use super::{Diagnostics, StdioConfig, StdioError, StdioSink, StdioSource};
use crate::application::jsonrpc::{BatchPolicy, SourceError};
use crate::application::mcp::request_meta::progress_token;
use crate::application::server::{DispatchError, DynExchange};
use crate::application::server::{
    Dispatcher, ListKind, ListSubscription, PeerSession, RequestContext,
};
use crate::application::session::DynFrameSink;
use crate::domain::RequestId;
use crate::domain::ir::{IrErrorCode, JsonObject};
use crate::domain::jsonrpc::reply::{self, ReplyError};
use crate::domain::jsonrpc::{DecodeError, Envelope, Frame, WireId, decode};
use crate::domain::session::{AbortReason, CancellationToken};

/// The answer to a batch the session never negotiated.
const BATCHING_REFUSED: &str = "batching was not negotiated for this session";

/// The answer to a request whose `params` are not a JSON object.
const PARAMS_NOT_AN_OBJECT: &str = "the request's params are not a JSON object";

/// The answer to a handler result that could not be read back as JSON.
const RESULT_UNREADABLE: &str = "the handler's result could not be serialised";

/// What is said when a peer sends a reply shape. A reply is the answer to a
/// request we made, and this loop wires no callback exchange yet, so there is
/// nothing to hand it to.
const IGNORED_REPLY: &str = "ignored a reply-shaped frame: no callback exchange is wired";

/// What is said when the peer closes the stream cleanly.
const PEER_CLOSED: &str = "the peer closed the stream; the session is over";

mod stateless;

/// Reads a peer's frames, routes them, and writes the answers back.
///
/// Generic over both streams so that the bytes can come from a child process,
/// a pipe, a socket, or a test, and over the diagnostics channel so that
/// "somewhere that is not stdout" is a decision made by the caller rather than
/// hard-wired here. See the module docs for what the loop guarantees.
pub struct ServeLoop<R, W, DW, P> {
    source: StdioSource<R>,
    /// Shared with the peer, so a handler's notifications — progress, log
    /// records, list changes — go out on the same line-framed stream as the
    /// replies. The sink's own lock serialises the two, one whole line at a
    /// time, so a notification can never land inside a reply.
    sink: Arc<StdioSink<W>>,
    diagnostics: Diagnostics<DW>,
    dispatcher: Arc<Dispatcher>,
    peer: PeerSession,
    policy: P,
    /// This connection's standing interest in list changes, taken once the
    /// handshake has settled what the peer negotiated, and dropped with the
    /// loop.
    lists: Option<ListSubscription>,
    /// Requests this server has sent the peer and is waiting to hear back on.
    exchange: Arc<StdioExchange<W>>,
    /// Requests that arrived while a handler was waiting on the peer, served
    /// in arrival order once it finishes.
    backlog: VecDeque<String>,
    /// How the input ended while a handler was running, if it did: `Ok` for a
    /// clean close, `Err` for a failure, reported once the backlog is served.
    ended: Option<Result<(), SourceError>>,
    /// Open `2026-07-28` listen streams, by their request id as JSON text.
    /// Each guard ends its subscription when the client cancels it or the
    /// loop ends.
    listens: HashMap<String, ListSubscription>,
    /// The last session id handed to a stateless request.
    stateless_ids: u64,
}

impl<R, W, DW, P> core::fmt::Debug for ServeLoop<R, W, DW, P> {
    /// Prints the session and the routing table, and nothing about the
    /// streams.
    ///
    /// The streams are `Debug`-able in practice and useless in a log: a peer's
    /// buffered bytes and a `DuplexStream`'s internals say nothing about which
    /// session is misbehaving, and requiring `W: Debug` to print the one field
    /// that would is a bound nobody wants.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ServeLoop")
            .field("peer", &self.peer)
            .field("dispatcher", &self.dispatcher)
            .finish_non_exhaustive()
    }
}

impl<R, W, DW, P> ServeLoop<R, W, DW, P> {
    /// The session every request on this connection is gated by.
    ///
    /// Read-only, and the only way to see it: the handshake *replaces* the
    /// session's profile with the one resolved for the revision the peer
    /// negotiated, so a caller who read the session before serving began would
    /// be holding the ceiling rather than the answer.
    #[must_use]
    pub const fn peer(&self) -> &PeerSession {
        &self.peer
    }

    /// Binds a peer's streams, a routing table, and a diagnostics channel.
    ///
    /// Both halves of the transport are built here from one [`StdioConfig`],
    /// and that is the point: a caller who assembled a [`StdioSource`] and a
    /// [`StdioSink`] separately could give them different ceilings, and
    /// [`StdioConfig`] exists to make that impossible. This is the only
    /// constructor, so there is no way to bypass it.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::Arc;
    ///
    /// use protocol_droid::adapters::primary::stdio::{Diagnostics, ServeLoop, StdioConfig};
    /// use protocol_droid::domain::MaxFrameSize;
    ///
    /// # fn example<R, W, DW, P>(
    /// #     reader: R,
    /// #     writer: W,
    /// #     diagnostics: Diagnostics<DW>,
    /// #     dispatcher: Arc<protocol_droid::application::server::Dispatcher>,
    /// #     peer: protocol_droid::application::server::PeerSession,
    /// #     policy: P,
    /// # ) -> Result<(), Box<dyn std::error::Error>>
    /// # where
    /// #     W: tokio::io::AsyncWrite + Unpin + Send + 'static,
    /// # {
    /// let config = StdioConfig::new(MaxFrameSize::new(1024)?);
    /// let server = ServeLoop::new(reader, writer, diagnostics, config, dispatcher, peer, policy);
    /// # let _ = server;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// The peer's sink is replaced with this loop's own. A handler that
    /// publishes on any other sink is publishing somewhere the peer at the
    /// other end of this pipe will never read.
    #[must_use]
    pub fn new(
        reader: R,
        writer: W,
        diagnostics: Diagnostics<DW>,
        config: StdioConfig,
        dispatcher: Arc<Dispatcher>,
        peer: PeerSession,
        policy: P,
    ) -> Self
    where
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let sink = Arc::new(StdioSink::new(writer, config));
        let exchange = Arc::new(StdioExchange::new(Arc::clone(&sink)));
        let peer = peer
            .with_sink(Arc::clone(&sink) as Arc<dyn DynFrameSink>)
            .with_exchange(Arc::clone(&exchange) as Arc<dyn DynExchange>);
        Self {
            source: StdioSource::new(reader, config),
            sink,
            diagnostics,
            dispatcher,
            peer,
            policy,
            lists: None,
            exchange,
            backlog: VecDeque::new(),
            ended: None,
            listens: HashMap::new(),
            // Clear of the handshake session's id, so a stateless request's
            // idempotency keys can never coincide with the session's.
            stateless_ids: 1 << 32,
        }
    }
}

impl<R, W, DW, P> ServeLoop<R, W, DW, P>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send + 'static,
    DW: AsyncWrite + Unpin + Send,
    P: BatchPolicy + Send + Sync,
{
    /// Serves the peer until it closes the stream.
    ///
    /// Returns `Ok` when the peer closed and `Err` when the transport failed.
    /// Those are different events and the distinction is the point: a peer that
    /// exits normally and a pipe that breaks are the same `Ok(0)` from the
    /// reader's point of view, and only [`StdioSource`] knows which one it
    /// saw. Collapsing them here would leave a caller unable to tell a clean
    /// shutdown from a crash.
    ///
    /// # Errors
    ///
    /// [`ServeError::Source`] when the peer's stream failed, which is latched
    /// and unrecoverable. [`ServeError::Reply`] when a reply could not be
    /// written, including a reply the configured ceiling refused —
    /// [`StdioError::ReplyTooLarge`](StdioError::ReplyTooLarge) — and
    /// [`ServeError::Diagnostics`] when the log could not be written. All three
    /// are terminal: continuing to serve a peer we can no longer talk to, or
    /// describe, would be a loop that fails silently.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use std::sync::Arc;
    /// # use protocol_droid::adapters::primary::stdio::{Diagnostics, ServeLoop, ServeReport, StdioConfig};
    /// # use protocol_droid::domain::MaxFrameSize;
    /// # async fn example<R, W, P>(
    /// #     reader: R,
    /// #     writer: W,
    /// #     dispatcher: Arc<protocol_droid::application::server::Dispatcher>,
    /// #     peer: protocol_droid::application::server::PeerSession,
    /// #     policy: P,
    /// # ) -> Result<ServeReport, Box<dyn std::error::Error>>
    /// # where
    /// #     R: tokio::io::AsyncRead + Unpin + Send,
    /// #     W: tokio::io::AsyncWrite + Unpin + Send + 'static,
    /// #     P: protocol_droid::application::jsonrpc::BatchPolicy + Send + Sync,
    /// # {
    /// let server = ServeLoop::new(
    ///     reader,
    ///     writer,
    ///     Diagnostics::stderr(),
    ///     StdioConfig::default(),
    ///     dispatcher,
    ///     peer,
    ///     policy,
    /// );
    ///
    /// let mut server = server;
    /// let report = server.serve().await?;
    /// // The report is returned rather than logged. This transport's stdout
    /// // carries MCP messages and nothing else, so a caller that wants the
    /// // numbers goes to its own log — the loop never chooses a sink.
    /// # Ok(report)
    /// # }
    /// ```
    pub async fn serve(&mut self) -> Result<ServeReport, ServeError> {
        let mut report = ServeReport::default();
        loop {
            // `Ok(None)` here is the peer closing, and nothing else. The
            // decoder's "no terminator yet" is a different `None` and never
            // reaches this call — see the `stdio` module docs for why
            // forwarding it would be a quiet way to drop half of every
            // conversation.
            let frame = if let Some(queued) = self.backlog.pop_front() {
                queued
            } else {
                match self.ended.take() {
                    Some(Err(error)) => return Err(ServeError::Source(error)),
                    Some(Ok(())) => {
                        self.note(PEER_CLOSED).await?;
                        return Ok(report);
                    }
                    None => {}
                }
                let next = self.source.next_frame().await;
                let Some(frame) = next.map_err(ServeError::Source)? else {
                    self.exchange.close();
                    self.note(PEER_CLOSED).await?;
                    return Ok(report);
                };
                frame
            };
            report.frames += 1;
            if self.take_reply(&frame) {
                continue;
            }
            self.serve_frame(&frame, &mut report).await?;
        }
    }

    /// Serves one whole frame, and writes at most one document back.
    async fn serve_frame(
        &mut self,
        frame: &str,
        report: &mut ServeReport,
    ) -> Result<(), ServeError> {
        let decoded = match decode(frame.as_bytes()) {
            Ok(decoded) => decoded,
            // Answered, not fatal. The frame was complete — the framing layer
            // found its terminator — so the next one starts at an offset this
            // loop already knows. See the module docs.
            Err(error) => return self.answer_undecodable(error, report).await,
        };

        let document = match decoded {
            Frame::Single(envelope) => self.answer(&envelope, report).await?,
            Frame::Batch(envelopes) => self.answer_batch(&envelopes, report).await?,
        };

        let Some(document) = document else {
            return Ok(());
        };
        // Serialised here, once, and handed to the sink as text. The sink
        // checks the ceiling against these bytes, so the check has to see the
        // document that is actually written rather than the members that built
        // it.
        let document = document.to_string();
        self.sink.send(&document).await.map_err(ServeError::Reply)?;
        report.replies += 1;
        Ok(())
    }

    /// Answers a frame that is a complete frame and not a JSON-RPC message.
    ///
    /// The id is `null` because there was no id to read, and the loop carries
    /// on — both of which are the decision the module docs argue for.
    async fn answer_undecodable(
        &self,
        error: DecodeError,
        report: &mut ServeReport,
    ) -> Result<(), ServeError> {
        let code = error.code();
        let message = error.to_string();
        // The message is this crate's own, not the peer's bytes: `decode`
        // reports a static description and never echoes the input, which is
        // what makes a log line that a peer cannot end.
        self.note(&format!("undecodable frame: {message}")).await?;
        let document = reply::unidentified_error_reply(code, &message).to_string();
        self.sink.send(&document).await.map_err(ServeError::Reply)?;
        report.replies += 1;
        Ok(())
    }

    /// Serves every message in a batch, and collects their answers.
    ///
    /// A batch is answered with one document carrying one answer per
    /// request-bearing message, which is what JSON-RPC 2.0 requires. A batch
    /// of nothing but notifications produces no document at all: there is no
    /// request to answer, and writing an empty array would be a message the
    /// peer never asked for.
    async fn answer_batch(
        &mut self,
        envelopes: &[Envelope<'_>],
        report: &mut ServeReport,
    ) -> Result<Option<Value>, ServeError> {
        // Delegated, not decided here. Whether a batch is legal is a
        // negotiation outcome, and the profile is the session's — a loop that
        // answered batches from its own belief about the revision would be
        // exactly the peer-side version of resolving a profile from the wrong
        // inputs.
        if !self.policy.allows_batch(&self.peer.profile()) {
            self.note(BATCHING_REFUSED).await?;
            return Ok(Some(reply::unidentified_error_reply(
                IrErrorCode::InvalidRequest,
                BATCHING_REFUSED,
            )));
        }
        let mut answers = Vec::with_capacity(envelopes.len());
        for envelope in envelopes {
            if let Some(answer) = self.answer(envelope, report).await? {
                answers.push(answer);
            }
        }
        Ok((!answers.is_empty()).then_some(Value::Array(answers)))
    }

    /// Serves one message, and returns the document that answers it.
    ///
    /// `None` means the message wanted no answer: a notification, a reply
    /// shape, or — for a batch — a message inside a batch that turned out not
    /// to be a request.
    async fn answer(
        &mut self,
        envelope: &Envelope<'_>,
        report: &mut ServeReport,
    ) -> Result<Option<Value>, ServeError> {
        // A reply shape is the peer answering a request of ours. This loop
        // wires no callback exchange, so there is nowhere to put it; the frame
        // is noted and dropped rather than treated as an error, because the
        // peer did nothing wrong.
        if envelope.routeable().is_none() {
            self.note(IGNORED_REPLY).await?;
            return Ok(None);
        }
        report.requests += 1;

        let params = match envelope.params() {
            None => JsonObject::default(),
            // Decoded here, at the boundary, into the type the dispatcher
            // takes. A `params` that is valid JSON but not an object is a
            // well-formed request with a bad argument, which is `-32602` and
            // not a parse failure.
            Some(raw) => match JsonObject::new(raw.get()) {
                Ok(params) => params,
                Err(_) => {
                    return Ok(refusal(
                        envelope.id(),
                        IrErrorCode::InvalidParams,
                        PARAMS_NOT_AN_OBJECT,
                    ));
                }
            },
        };

        let method: &str = envelope.method_str().unwrap_or_default();

        // A cancellation that names an open listen stream ends it. Any other
        // cancellation names a request that is not in flight — the loop only
        // reaches here between requests — so it is a no-op, as the
        // specification asks.
        if method == "notifications/cancelled" {
            let _ = self.cancel_listen(&params);
            return Ok(None);
        }

        // SEP-2575: a request that names its own revision in `_meta` is served
        // from that alone, with no handshake behind it.
        if names_own_revision(&params) {
            return self.answer_stateless(envelope.id(), method, &params).await;
        }

        // The id is carried through untouched: the peer's `WireId` becomes
        // the handler's `RequestId` and is written back from the same value.
        let context = RequestContext::for_request_with_state(
            &self.peer,
            RequestId::from_wire(envelope.id()),
            CancellationToken::new(),
            progress_token(&params),
            Arc::clone(self.dispatcher.state()),
        );

        // `initialize` is the one method that changes the session rather than
        // being answered from it. The handshake resolves a profile for the
        // revision the client offered, and every gate after this point has to
        // read *that* one — a peer that negotiated `2024-11-05` must not be
        // gated on a profile resolved for whatever the process was built with.
        if method == "initialize" {
            let outcome = self.dispatcher.handshake(&context, &params);

            // A notification is served and never answered, exactly as below.
            let Some(id) = envelope.id() else {
                return Ok(None);
            };
            return Ok(Some(match outcome {
                Ok((reply, narrowed)) => {
                    // Subscribed against the negotiated profile, so a peer that
                    // did not negotiate `listChanged` for a kind is never sent
                    // one. Replacing an earlier subscription drops it.
                    self.lists = Some(self.dispatcher.server().handle().subscribe(
                        narrowed.sink(),
                        narrowed.profile(),
                        &ListKind::ALL,
                        None,
                    ));
                    self.peer = narrowed;
                    match reply::result_reply(id, &reply) {
                        Ok(document) => document,
                        Err(ReplyError::UnreadableResult) => {
                            reply::error_reply(id, IrErrorCode::InternalError, RESULT_UNREADABLE)
                        }
                    }
                }
                Err(error) => {
                    let (code, message) = error.as_wire();
                    self.note(&format!("refused a handshake: {message}"))
                        .await?;
                    reply::error_reply(id, code, &message)
                }
            }));
        }

        let (outcome, cancelled) = self.run_handler(&context, method, &params).await;
        // A cancelled request is answered with nothing: the specification has
        // the receiver send no further messages for it, and the client has
        // already stopped waiting.
        if cancelled {
            return Ok(None);
        }

        // A notification is served and never answered — not even with an
        // error, which is what JSON-RPC 2.0 requires, and which is also what
        // stops a peer that floods notifications from filling stdout with
        // messages nobody is waiting for.
        let Some(id) = envelope.id() else {
            return Ok(None);
        };

        Ok(Some(match outcome {
            Ok(result) => match reply::result_reply(id, &result) {
                Ok(document) => document,
                Err(ReplyError::UnreadableResult) => {
                    reply::error_reply(id, IrErrorCode::InternalError, RESULT_UNREADABLE)
                }
            },
            Err(error) => {
                let (code, message) = error.as_wire();
                // The message is the dispatcher's own text, never the method
                // name the peer chose — see `note`.
                self.note(&format!("refused a request: {message}")).await?;
                reply::error_reply(id, code, &message)
            }
        }))
    }

    /// Runs a handler while still listening to the peer.
    ///
    /// A handler that asks the peer to sample or elicit is waiting on an answer
    /// that arrives on stdin, so the loop cannot simply await it: nothing would
    /// read the answer. Instead it races the handler against the next frame. A
    /// frame that answers one of this server's requests is delivered to the
    /// exchange; any other frame is queued and served after this handler, so
    /// replies still leave in the order their requests arrived.
    async fn run_handler(
        &mut self,
        context: &RequestContext,
        method: &str,
        params: &JsonObject,
    ) -> (Result<JsonObject, DispatchError>, bool) {
        let dispatcher = Arc::clone(&self.dispatcher);
        let call = dispatcher.dispatch(context, method, params);
        tokio::pin!(call);
        let mut cancelled = false;
        loop {
            let listening = self.ended.is_none();
            let step = tokio::select! {
                biased;
                outcome = &mut call => Step::Done(outcome),
                frame = self.source.next_frame(), if listening => Step::Frame(frame),
            };
            match step {
                Step::Done(outcome) => return (outcome, cancelled),
                Step::Frame(Ok(Some(frame))) => {
                    if self.take_reply(&frame) {
                        continue;
                    }
                    if cancels(&frame, context) {
                        context.cancellation().cancel(AbortReason::Cancelled(None));
                        cancelled = true;
                        continue;
                    }
                    self.backlog.push_back(frame);
                }
                Step::Frame(Ok(None)) => {
                    self.exchange.close();
                    self.ended = Some(Ok(()));
                }
                Step::Frame(Err(error)) => {
                    self.exchange.close();
                    self.ended = Some(Err(error));
                }
            }
        }
    }

    /// Delivers `frame` to the exchange if it answers one of this server's
    /// requests, and reports whether it did.
    ///
    /// A reply is a JSON object with an `id`, no `method`, and a `result` or
    /// an `error`. Recognised here, before decoding, because the request-side
    /// decoder has no shape for a reply and would answer it as a bad request.
    fn take_reply(&self, frame: &str) -> bool {
        let Ok(serde_json::Value::Object(members)) = serde_json::from_str::<Value>(frame) else {
            return false;
        };
        if members.contains_key("method")
            || !(members.contains_key("result") || members.contains_key("error"))
        {
            return false;
        }
        if let Some(id) = members.get("id").and_then(Value::as_i64) {
            self.exchange
                .resolve(id, answer_of(&Value::Object(members)));
        }
        true
    }

    /// Writes one line to the diagnostics channel.
    ///
    /// Nothing a peer said is ever passed here. A method name is a JSON string
    /// the peer chose, and JSON permits a newline inside one, so logging it
    /// would let a peer close this log line and open one of its own. Every
    /// string this loop logs is a constant or a `thiserror` message, which is
    /// the only reason the channel can be read as a log at all.
    async fn note(&self, line: &str) -> Result<(), ServeError> {
        let mut buffered = String::with_capacity(line.len() + 1);
        buffered.push_str(line);
        buffered.push('\n');
        self.diagnostics
            .write_all(buffered.as_bytes())
            .await
            .map_err(ServeError::Diagnostics)
    }
}

/// The reply for a request, or nothing at all for a notification.
///
/// One place decides that, because the rule is easy to get wrong in either
/// direction: answering a notification is a protocol violation, and staying
/// silent on a request leaves a peer waiting forever for an answer that will
/// never come.
fn refusal(id: Option<&WireId<'_>>, code: IrErrorCode, message: &str) -> Option<Value> {
    id.map(|id| reply::error_reply(id, code, message))
}

/// The handler's view of the peer's id.
///
/// A notification has none, and [`RequestContext`] insists on one because a
/// handler may be handed a progress token or a cancellation token and needs
/// something to correlate against. A notification can be given neither, so
/// nothing it does can reach the peer and the value is unobservable — it
/// exists only so the context can be built at all.
///
/// The mapping itself is [`RequestId::from_wire`], in the domain, and the
/// refusal mapping is [`DispatchError::as_wire`], in the application layer:
/// both are properties of a value rather than of the way this loop writes it
/// down, and the HTTP router needs them for the same reasons this loop does.
///
/// What a finished session did.
///
/// Counted, not merely logged, so that "the loop ran once and served nothing"
/// is a number a caller can assert on rather than an absence nobody notices.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServeReport {
    /// Frames read off the peer's stream, including ones that did not decode.
    pub frames: usize,
    /// Messages routed to a handler, including notifications and refusals.
    ///
    /// Counts the *messages*, so a batch of three raises this by three.
    pub requests: usize,
    /// Documents written to the peer's stream.
    ///
    /// Counts the *documents*, so a batch of three requests raises this by
    /// one. The two counts differ by design: they answer different questions,
    /// and a report that used one number for both would be answering neither.
    pub replies: usize,
}

/// Why the session ended when it was not the peer ending it.
///
/// Every variant is terminal. The loop cannot tell a peer that has gone how it
/// went, and carrying on would mean serving a conversation nobody is in.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// The peer's stream could not be read.
    ///
    /// Latched by [`StdioSource`], so this is where the session ends rather
    /// than a frame that will be tried again.
    #[error("the peer's stream could not be read: {0}")]
    Source(#[source] SourceError),

    /// A reply could not be written.
    ///
    /// Includes [`StdioError::ReplyTooLarge`](StdioError::ReplyTooLarge), which
    /// is refused *before* any byte reaches the stream — so a peer never sees a
    /// truncated frame, which on a newline protocol it could not detect.
    #[error("a reply could not be written: {0}")]
    Reply(#[source] StdioError),

    /// The diagnostics channel could not be written.
    ///
    /// Fatal rather than ignored, which is the opposite of the advice
    /// [`Diagnostics::write_all`] offers a caller. The loop is the top of the
    /// process: if it cannot say what it is doing, it should stop doing it,
    /// rather than serve requests from a state only it knows about.
    #[error("the diagnostics channel could not be written: {0}")]
    Diagnostics(#[source] StdioError),
}

/// Whether `params` names its own revision in `_meta` (SEP-2575).
fn names_own_revision(params: &JsonObject) -> bool {
    serde_json::from_str::<Value>(params.as_str())
        .is_ok_and(|parsed| crate::application::mcp::request_meta::declares_version(&parsed))
}

/// Whether `frame` is a `notifications/cancelled` naming the request `context`
/// is serving.
fn cancels(frame: &str, context: &RequestContext) -> bool {
    let Ok(parsed) = serde_json::from_str::<Value>(frame) else {
        return false;
    };
    parsed.get("method").and_then(Value::as_str) == Some("notifications/cancelled")
        && parsed.pointer("/params/requestId").is_some_and(|target| {
            serde_json::from_str::<Value>(&context.request_id().to_json())
                .is_ok_and(|id| id == *target)
        })
}

/// What one turn of [`ServeLoop::run_handler`] produced.
enum Step<T> {
    /// The handler finished.
    Done(T),
    /// A frame arrived first.
    Frame(Result<Option<String>, SourceError>),
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unused_async_trait_impl,
        reason = "FrameSink::send is an async trait method, so a do-nothing sink carries \
                  the async signature whether or not it awaits anything"
    )]
    use std::collections::VecDeque;
    use std::future::Future;
    use std::io;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::task::{Context, Poll};

    use assert_matches::assert_matches;
    use serde_json::{Map, Value};
    use tokio::io::{AsyncRead, ReadBuf};

    use super::{ServeError, ServeLoop, ServeReport};
    use crate::adapters::primary::stdio::testing::Capture;
    use crate::adapters::primary::stdio::{Diagnostics, StdioConfig, StdioError};
    use crate::application::jsonrpc::{AllowBatching, BatchPolicy, DenyBatching, SourceError};
    use crate::application::server::{
        CallbackExchange, Dispatcher, ExchangeError, PeerSession, Server, ToolHandler,
    };
    use crate::application::session::{FrameSink, SinkError};
    use crate::domain::Feature;
    use crate::domain::feature::{Negotiation, resolve};
    use crate::domain::ir::{JsonObject, JsonObjectError};
    use crate::domain::registry::Tool;
    use crate::domain::session::SessionId;
    use crate::domain::{
        CapabilitySet, MaxFrameSize, MaxFrameSizeError, ProtocolVersion, TransportKind,
    };

    /// The tool every session in this file can see.
    const ECHO: &str = "echo";

    /// A ceiling comfortably above every reply these tests produce.
    const ROOMY: u32 = 64 * 1024;

    /// What a test here can fail with, so a fallible call still propagates
    /// instead of unwrapping.
    #[derive(Debug)]
    enum Failure {
        /// A fixture was not a JSON object.
        Object(JsonObjectError),
        /// A ceiling the test asked for was refused.
        Limit(MaxFrameSizeError),
        /// The loop failed, or stdout carried something that is not JSON.
        /// Both are failures of the same claim: what reached the peer is a
        /// sequence of well-formed frames.
        Json(String),
        /// A test needed a frame or a member and there was none.
        Missing(&'static str),
    }

    impl std::fmt::Display for Failure {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Object(error) => write!(f, "bad fixture: {error}"),
                Self::Limit(error) => write!(f, "bad ceiling: {error}"),
                Self::Json(reason) => write!(f, "not a JSON frame: {reason}"),
                Self::Missing(what) => write!(f, "expected {what}"),
            }
        }
    }

    impl std::error::Error for Failure {}

    impl From<JsonObjectError> for Failure {
        fn from(error: JsonObjectError) -> Self {
            Self::Object(error)
        }
    }

    impl From<MaxFrameSizeError> for Failure {
        fn from(error: MaxFrameSizeError) -> Self {
            Self::Limit(error)
        }
    }

    impl From<ServeError> for Failure {
        fn from(error: ServeError) -> Self {
            Self::Json(error.to_string())
        }
    }

    /// A reader that hands over one scripted chunk per read, then EOF.
    ///
    /// A pipe splits wherever it likes, so a fixture that delivered everything
    /// in one read would never exercise the loop re-entering a half-built
    /// frame, and one that delivered a frame per read would never exercise a
    /// frame the decoder had already seen the end of. Both shapes are used
    /// below, which is why the fixture is scripted rather than a buffer.
    struct Trickle {
        chunks: VecDeque<Vec<u8>>,
    }

    impl Trickle {
        /// Serves `chunks` in order, one read each.
        fn new(chunks: &[&str]) -> Self {
            Self {
                chunks: chunks
                    .iter()
                    .map(|chunk| chunk.as_bytes().to_vec())
                    .collect(),
            }
        }
    }

    impl AsyncRead for Trickle {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            // Putting nothing in the buffer is how `AsyncRead` spells `Ok(0)`,
            // which is how this fixture reports end-of-stream.
            let Some(chunk) = self.get_mut().chunks.pop_front() else {
                return Poll::Ready(Ok(()));
            };
            buf.put_slice(chunk.as_slice());
            Poll::Ready(Ok(()))
        }
    }

    /// A reader that fails on the first read.
    ///
    /// The counterpart to [`Trickle`]. Without it, "a broken stream is not a
    /// clean close" could only be tested with a real pipe, and a test that
    /// needs a real pipe fails on a platform nobody remembered to check.
    struct Rupture;

    impl AsyncRead for Rupture {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Ready(Err(io::Error::other("injected read failure")))
        }
    }

    /// An exchange that refuses every callback.
    ///
    /// This loop wires none, so nothing should ever reach it; the fixture
    /// exists so that a handler which did try would fail loudly rather than
    /// hang.
    struct NoExchange;

    impl CallbackExchange for NoExchange {
        fn exchange(
            &self,
            _method: &str,
            _params: &JsonObject,
        ) -> impl Future<Output = Result<JsonObject, ExchangeError>> + Send {
            core::future::ready(Err(ExchangeError::new("no callbacks in this test")))
        }
    }

    /// A sink the session can hold but this loop never writes to.
    ///
    /// The loop's replies go through its own [`StdioSink`](super::StdioSink).
    /// This one is only here because a [`PeerSession`] wants somewhere to put
    /// a server-initiated request, and a real stream would make the test
    /// depend on the order two writers happen to run in.
    struct NoSink;

    impl FrameSink for NoSink {
        async fn send(&self, _frame: &str) -> Result<(), SinkError> {
            Ok(())
        }
    }

    /// A handler that echoes its arguments back, so an assertion about the
    /// answer is a claim about the hop and not about a `format!` agreeing with
    /// itself.
    fn echo_handler() -> ToolHandler {
        Arc::new(|_ctx, arguments| Box::pin(async move { arguments }))
    }

    /// A routing table with one tool on it.
    fn dispatcher() -> Result<Arc<Dispatcher>, Failure> {
        let tool = Tool::new(ECHO, r#"{"type":"object"}"#)?;
        let server = Server::builder("serve-test")
            .tool(tool, echo_handler())
            .build();
        Ok(Arc::new(Dispatcher::new(Arc::new(server))))
    }

    /// A session with every feature its revision has.
    ///
    /// A server advertising less would make a gate look like a routing failure
    /// in any test that tripped over it.
    fn peer() -> PeerSession {
        let version = ProtocolVersion::V2024_11_05;
        PeerSession::new(
            version,
            resolve(Negotiation {
                version,
                client_caps: CapabilitySet::all_for(version),
                server_caps: CapabilitySet::all_for(version),
                transport: TransportKind::Stdio,
            }),
            SessionId::new(1),
            TransportKind::Stdio,
            Arc::new(NoExchange),
            Arc::new(NoSink),
        )
    }

    /// The loop, plus a handle on each of its two output streams.
    type Harness<P> = (ServeLoop<Trickle, Capture, Capture, P>, Capture, Capture);

    /// A loop over `chunks` with a ceiling of `limit`.
    fn harness<P>(chunks: &[&str], policy: P, limit: u32) -> Result<Harness<P>, Failure> {
        let stdout = Capture::shared();
        let stderr = Capture::shared();
        let server = ServeLoop::new(
            Trickle::new(chunks),
            stdout.clone(),
            Diagnostics::new(stderr.clone()),
            StdioConfig::new(MaxFrameSize::new(limit)?),
            dispatcher()?,
            peer(),
            policy,
        );
        Ok((server, stdout, stderr))
    }

    /// Runs a loop to its end, and hands back what it wrote to each stream.
    async fn run<P>(
        chunks: &[&str],
        policy: P,
    ) -> Result<(Result<ServeReport, ServeError>, Capture, Capture), Failure>
    where
        P: BatchPolicy + Send + Sync,
    {
        run_under(chunks, policy, ROOMY).await
    }

    /// [`run`] with the ceiling spelled out, for the one test that needs a
    /// reply to be refused.
    async fn run_under<P>(
        chunks: &[&str],
        policy: P,
        limit: u32,
    ) -> Result<(Result<ServeReport, ServeError>, Capture, Capture), Failure>
    where
        P: BatchPolicy + Send + Sync,
    {
        let (mut server, stdout, stderr) = harness(chunks, policy, limit)?;
        let report = server.serve().await;
        Ok((report, stdout, stderr))
    }

    /// Every frame the loop wrote to the peer's stream, parsed.
    ///
    /// A line here that does not parse is the whole point of the purity test:
    /// it is what a `println!` anywhere in the serve path looks like from the
    /// peer's side, and it is invisible to any test that only checks the reply
    /// it asked for.
    fn written(stdout: &Capture) -> Result<Vec<Value>, Failure> {
        stdout
            .contents()
            .split('\n')
            .filter(|line| !line.is_empty())
            .map(|line| {
                serde_json::from_str::<Value>(line)
                    .map_err(|error| Failure::Json(error.to_string()))
            })
            .collect()
    }

    /// The `code` member of a frame's `error`.
    fn error_code(frame: &Value) -> Result<i64, Failure> {
        frame
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(Value::as_i64)
            .ok_or(Failure::Missing("an error code"))
    }

    /// The `id` member of a frame.
    fn id_of(frame: &Value) -> Result<Value, Failure> {
        frame.get("id").cloned().ok_or(Failure::Missing("an id"))
    }

    /// One request in, one reply out, and the reply belongs to the request.
    #[tokio::test]
    async fn a_request_in_produces_a_reply_out() -> Result<(), Failure> {
        let (served, stdout, stderr) = run(
            &["{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n"],
            AllowBatching,
        )
        .await?;
        let report = served?;
        let frames = written(&stdout)?;

        assert_eq!(frames.len(), 1);
        assert_eq!(
            report,
            ServeReport {
                frames: 1,
                requests: 1,
                replies: 1
            }
        );
        let reply = frames.first().ok_or(Failure::Missing("a reply"))?;
        assert_eq!(id_of(reply)?, Value::from(1));
        assert_eq!(reply.get("result"), Some(&Value::Object(Map::default())));
        // The log really was written to, so the purity claim is not passing
        // because the loop happens to be silent.
        assert!(
            stderr.contents().contains("closed"),
            "{}",
            stderr.contents()
        );
        Ok(())
    }

    /// A frame is a line, and a line is only a line once its terminator has
    /// arrived. The chunk here is a whole JSON-RPC message and the loop still
    /// reads nothing, because the framing layer cannot tell a peer that is
    /// mid-message from a peer that has gone away. The report is therefore the
    /// same one a session that received no bytes at all produces, and that is
    /// the documented contract rather than an accident of this fixture.
    #[tokio::test]
    async fn an_unterminated_line_is_not_a_frame() -> Result<(), Failure> {
        let (served, stdout, stderr) = run(
            &[r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#],
            AllowBatching,
        )
        .await?;
        let report = served?;

        assert_eq!(
            report,
            ServeReport::default(),
            "a message that has not finished arriving is not served"
        );
        assert_eq!(stdout.contents(), "", "nothing was answered");
        assert!(
            stderr.contents().contains("closed"),
            "{}",
            stderr.contents()
        );
        Ok(())
    }

    /// A loop that serves one message and returns passes every other test in
    /// this file. The first chunk here holds two whole frames and the second a
    /// third, which also covers a frame the decoder had already seen the end of
    /// before the loop asked again.
    #[tokio::test]
    async fn three_requests_in_sequence_each_get_an_answer() -> Result<(), Failure> {
        let (served, stdout, _stderr) = run(
            &[
                "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n\
                 {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n",
                "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"ping\"}\n",
            ],
            AllowBatching,
        )
        .await?;
        let report = served?;
        let frames = written(&stdout)?;

        assert_eq!(
            report.frames, 3,
            "three frames arrived in two reads, and the report counts frames"
        );
        assert_eq!(report.requests, 3);
        assert_eq!(report.replies, 3);
        let ids: Vec<Value> = frames.iter().map(id_of).collect::<Result<_, _>>()?;
        assert_eq!(ids, vec![Value::from(1), Value::from(2), Value::from(3)]);
        Ok(())
    }

    /// A pipe splits wherever it likes, so a request arrives in pieces. The
    /// two reads are one frame, and the loop must not answer either of them.
    #[tokio::test]
    async fn a_request_split_across_two_reads_is_one_frame() -> Result<(), Failure> {
        let (served, stdout, _stderr) = run(
            &["{\"jsonrpc\":\"2.0\",\"id\":", "42,\"method\":\"ping\"}\n"],
            AllowBatching,
        )
        .await?;
        let report = served?;
        let frames = written(&stdout)?;

        assert_eq!(report.frames, 1, "two reads are one frame, not two");
        assert_eq!(frames.len(), 1);
        let reply = frames.first().ok_or(Failure::Missing("a reply"))?;
        assert_eq!(id_of(reply)?, Value::from(42));
        assert!(reply.get("result").is_some(), "{reply}");
        Ok(())
    }

    /// The rule this transport exists to keep: stdout is MCP messages and
    /// nothing else. Observed, not asserted in a comment — the whole of stdout
    /// is read, split into frames, and every frame must be a well-formed reply
    /// carrying the version, an id, and exactly one outcome. A `println!`
    /// anywhere in the serve path puts a line here that does not parse.
    #[tokio::test]
    async fn stdout_carries_nothing_but_replies() -> Result<(), Failure> {
        let (served, stdout, stderr) = run(
            &[
                // A result.
                "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n",
                // A refusal the dispatcher produced.
                "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"no/such/method\"}\n",
                // A frame the decoder refused, which the loop still answers.
                "this is not json at all\n",
                // A notification, which earns no reply.
                "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
            ],
            AllowBatching,
        )
        .await?;
        let report = served?;
        let frames = written(&stdout)?;

        assert_eq!(frames.len(), 3, "the notification earns none: {frames:?}");
        for frame in &frames {
            assert_eq!(frame.get("jsonrpc"), Some(&Value::from("2.0")), "{frame}");
            assert!(
                frame.get("id").is_some(),
                "every reply is addressed: {frame}"
            );
            let result = frame.get("result").is_some();
            let error = frame.get("error").is_some();
            assert!(result ^ error, "exactly one outcome: {frame}");
        }
        // Both channels really were used, so the check above is not passing
        // because the loop wrote nothing anywhere.
        let log = stderr.contents();
        assert!(log.contains("undecodable frame"), "{log}");
        assert!(log.contains("refused a request"), "{log}");
        assert_eq!(report.replies, 3);
        Ok(())
    }

    /// The decision this module exists to make: a frame that arrived whole and
    /// did not decode is answered with `-32700` and the session continues. The
    /// second request in the same session is the part that would fail if the
    /// loop dropped the connection.
    #[tokio::test]
    async fn a_malformed_frame_is_answered_and_serving_continues() -> Result<(), Failure> {
        let (served, stdout, _stderr) = run(
            &[
                "{not json at all\n",
                "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n",
            ],
            AllowBatching,
        )
        .await?;
        let report = served?;
        let frames = written(&stdout)?;

        assert_eq!(report.frames, 2, "both frames were read");
        assert_eq!(report.replies, 2);
        let (first, rest) = frames
            .split_first()
            .ok_or(Failure::Missing("a parse error"))?;
        let second = rest.first().ok_or(Failure::Missing("a second reply"))?;
        assert_eq!(id_of(first)?, Value::Null, "no id was ever read");
        assert_eq!(error_code(first)?, -32700);
        assert_eq!(id_of(second)?, Value::from(2), "the session carried on");
        assert!(second.get("result").is_some(), "{second}");
        Ok(())
    }

    /// A frame that is JSON but not a JSON-RPC message is a different failure
    /// from one that is not JSON, and the specification gives them different
    /// codes. Collapsing them would tell the peer to fix the wrong thing.
    #[tokio::test]
    async fn a_well_formed_document_that_is_not_a_message_is_an_invalid_request()
    -> Result<(), Failure> {
        let (served, stdout, _stderr) = run(&["{\"hello\":\"world\"}\n"], AllowBatching).await?;
        served?;
        let frames = written(&stdout)?;
        let reply = frames.first().ok_or(Failure::Missing("a reply"))?;

        assert_eq!(error_code(reply)?, -32600);
        Ok(())
    }

    /// A peer that closes is not a failure. `serve` answers `Ok`, and the
    /// report says the session did nothing rather than pretending it never
    /// happened.
    #[tokio::test]
    async fn a_clean_close_is_a_clean_close() -> Result<(), Failure> {
        let (served, stdout, stderr) = run(&[], AllowBatching).await?;

        let report = served?;
        assert_eq!(report, ServeReport::default());
        assert_eq!(stdout.contents(), "");
        assert!(
            stderr.contents().contains("closed"),
            "{}",
            stderr.contents()
        );
        Ok(())
    }

    /// The distinction the last test's `Ok` is only worth anything because of:
    /// a stream that breaks is a different event from a stream that ends, and
    /// a caller has to be able to tell them apart.
    #[tokio::test]
    async fn a_broken_stream_is_not_a_clean_close() -> Result<(), Failure> {
        let stdout = Capture::shared();
        let mut broken = ServeLoop::new(
            Rupture,
            stdout.clone(),
            Diagnostics::new(Capture::shared()),
            StdioConfig::new(MaxFrameSize::new(ROOMY)?),
            dispatcher()?,
            peer(),
            AllowBatching,
        );

        let served = broken.serve().await;

        assert_matches!(served, Err(ServeError::Source(SourceError::Transport)));
        assert_eq!(stdout.contents(), "", "nothing was answered");
        Ok(())
    }

    /// A reply the ceiling refuses is never written. `StdioConfig` is one value
    /// for both directions, so the ceiling here sits above the 45-byte request
    /// and below the listing that comes back.
    #[tokio::test]
    async fn an_oversized_reply_is_refused_and_nothing_reaches_the_wire() -> Result<(), Failure> {
        let (served, stdout, _stderr) = run_under(
            &["{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n"],
            AllowBatching,
            64,
        )
        .await?;

        assert_matches!(
            served,
            Err(ServeError::Reply(StdioError::ReplyTooLarge { .. }))
        );
        assert_eq!(
            stdout.contents(),
            "",
            "a refused reply must not be truncated onto the stream",
        );
        Ok(())
    }

    /// A notification is served and never answered — not even with an error,
    /// which is what JSON-RPC 2.0 requires and what stops a peer that floods
    /// them from filling stdout with messages nobody is waiting for.
    #[tokio::test]
    async fn a_notification_is_served_and_never_answered() -> Result<(), Failure> {
        let (served, stdout, stderr) = run(
            &["{\"jsonrpc\":\"2.0\",\"method\":\"ping\"}\n"],
            AllowBatching,
        )
        .await?;
        let report = served?;

        assert_eq!(report.requests, 1, "the method was still routed");
        assert_eq!(report.replies, 0);
        assert_eq!(stdout.contents(), "");
        assert!(stderr.contents().contains("closed"));
        Ok(())
    }

    /// A batch is answered with one document carrying one answer per request,
    /// which is what the specification requires — and not with one document per
    /// message, which a peer would read as three unrequested replies.
    #[tokio::test]
    async fn a_batch_is_answered_with_one_document() -> Result<(), Failure> {
        let (served, stdout, _stderr) = run(
            &["[{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"},\
                {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}]\n"],
            AllowBatching,
        )
        .await?;
        let report = served?;
        let frames = written(&stdout)?;

        assert_eq!(frames.len(), 1, "a batch is one document");
        assert_eq!(report.requests, 2);
        assert_eq!(report.replies, 1, "one document, not two");
        let answers = frames
            .first()
            .and_then(Value::as_array)
            .ok_or(Failure::Missing("an array of answers"))?;
        assert_eq!(answers.len(), 2);
        Ok(())
    }

    /// A peer is gated on the revision it negotiated, not the one the loop was
    /// constructed with.
    ///
    /// The loop here starts on a session whose profile carries `2025-06-18`
    /// features. The peer offers `2024-11-05`, and `completions/complete` did
    /// not exist until `2025-03-26`. If the handshake did not narrow the
    /// session, the call would be served — a peer on a revision whose schema
    /// has nowhere to put the answer.
    #[tokio::test]
    async fn the_session_is_narrowed_to_the_negotiated_revision() -> Result<(), Failure> {
        let (mut server, _stdout, _stderr) = harness(&[], AllowBatching, ROOMY)?;

        let handshake = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"probe","version":"1"}}}"#;
        let completion = r#"{"jsonrpc":"2.0","id":2,"method":"completions/complete","params":{"ref":{"type":"ref/prompt","name":"p"},"argument":{"name":"a","value":""}}}"#;
        let mut report = ServeReport::default();
        server
            .serve_frame(&format!("{handshake}\n"), &mut report)
            .await?;
        server
            .serve_frame(&format!("{completion}\n"), &mut report)
            .await?;

        assert!(
            !server.peer.profile().allows(Feature::Completions),
            "a 2024-11-05 peer must not keep a 2025-03-26 feature"
        );
        Ok(())
    }

    /// A batch the session never negotiated is refused whole, and nothing
    /// inside it is served. The decision belongs to the policy, not to the
    /// loop: a loop that answered batches from its own belief about the
    /// revision would be the peer-side version of resolving a profile from the
    /// wrong inputs.
    #[tokio::test]
    async fn a_batch_the_session_never_negotiated_is_refused() -> Result<(), Failure> {
        let (served, stdout, stderr) = run(
            &["[{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}]\n"],
            DenyBatching,
        )
        .await?;
        let report = served?;
        let frames = written(&stdout)?;

        assert_eq!(frames.len(), 1);
        assert_eq!(
            error_code(frames.first().ok_or(Failure::Missing("a reply"))?)?,
            -32600
        );
        assert_eq!(
            report.requests, 0,
            "nothing inside a refused batch is served"
        );
        assert!(
            stderr.contents().contains("batching"),
            "{}",
            stderr.contents()
        );
        Ok(())
    }

    /// The id the peer sent is the id the peer gets back, byte for byte, even
    /// when it contains characters only JSON knows how to escape. A reply whose
    /// id differs by one character is an answer no peer can match to its
    /// request.
    #[tokio::test]
    async fn the_request_id_survives_the_round_trip_escaped() -> Result<(), Failure> {
        let (served, stdout, _stderr) = run(
            &["{\"jsonrpc\":\"2.0\",\"id\":\"a\\u007fb\\\"c\",\"method\":\"ping\"}\n"],
            AllowBatching,
        )
        .await?;
        served?;
        let frames = written(&stdout)?;
        let reply = frames.first().ok_or(Failure::Missing("a reply"))?;

        assert_eq!(id_of(reply)?, Value::from("a\u{7f}b\"c"));
        Ok(())
    }

    /// A reply that a reader cannot match to its request is worse than no
    /// reply, so the number stays a number.
    #[tokio::test]
    async fn a_numeric_id_stays_numeric() -> Result<(), Failure> {
        let (served, stdout, _stderr) = run(
            &["{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\n"],
            AllowBatching,
        )
        .await?;
        served?;
        let frames = written(&stdout)?;
        let reply = frames.first().ok_or(Failure::Missing("a reply"))?;

        assert_eq!(id_of(reply)?, Value::from(7));
        Ok(())
    }

    /// `Debug` is public API on a type a caller will log at startup. It names
    /// the session and the routing table and nothing about the streams, so it
    /// neither needs `W: Debug` nor prints a peer's buffered bytes.
    #[test]
    fn the_debug_output_names_the_session_and_not_the_streams() -> Result<(), Failure> {
        let (server, _stdout, _stderr) = harness(&[], AllowBatching, ROOMY)?;

        let printed = format!("{server:?}");

        assert!(printed.contains("ServeLoop"), "{printed}");
        assert!(printed.contains("peer"), "{printed}");
        assert!(!printed.contains("Trickle"), "{printed}");
        Ok(())
    }
}
