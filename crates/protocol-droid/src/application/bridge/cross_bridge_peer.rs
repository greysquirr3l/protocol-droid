//! The cross-protocol bridge peer: a `BridgePeer` that translates
//! requests and replies through a [`CrossProtocolCodec`] before the
//! inner upstream peer ever sees them.
//!
//! # What this type is
//!
//! A bridge to an A2A agent from an MCP client (or vice versa) needs to
//! rename methods and reshape parameters on the way out, and rename +
//! reshape the reply on the way back. The same `BridgePeer` trait the
//! MCP-only bridge uses is the seam — a cross-protocol peer is just a
//! `BridgePeer` whose `send` runs the envelope through a codec first
//! and the reply through the codec again, and whose `notify` does the
//! request half of the same.
//!
//! # What this type is *not*
//!
//! It is not a `Bridge` in the sense of [`super::forward::Bridge`]. The
//! same-protocol bridge is a use case over a peer; this is the peer
//! that lets that use case talk across protocols. A loop that holds a
//! `CrossBridgePeer` as its upstream runs unchanged, because the
//! downstream leg never sees the codec — it sees envelopes in its own
//! language on the way in and its own language on the way out.
//!
//! # Why the wrapper looks the same shape as the inner peer
//!
//! A `CrossBridgePeer` *contains* an `Arc<U>` and a `CrossProtocolCodec`,
//! and exposes a `BridgePeer` impl. The inner peer is reachable through
//! [`inner`](Self::inner) for the rare operator who needs it; the codec
//! is reachable through [`codec`](Self::codec) for diagnostics. A
//! bridge that needed a *second* upstream for the same downstream
//! would need a different type, because it has different ownership —
//! not because the trait was hidden.
//!
//! # What happens when the codec cannot bridge the method
//!
//! A `SendMessage` from an A2A client is bridged; a `GetTask` is not.
//! The wrapper hands `GetTask` to the codec, the codec returns
//! [`CrossProtocolError::MethodNotBridged`], the wrapper returns
//! [`BridgeError::MethodNotBridged`] carrying the same name, and the
//! loop answers the client with `-32601 Method not found`. The
//! alternative — passthrough-shipping a method the upstream rejects
//! with `-32601` of its own — would look the same on the wire but
//! would lose the codec's `RequestCodecReport`, which the operator
//! needs when a method they expected to bridge comes back unbridged.

use std::sync::Arc;

use serde_json::{Map, Value};

use super::cross_protocol::{
    CrossProtocolCodec, CrossProtocolError, ProtocolSide, RequestCodecReport,
};
use super::forward::{BridgeError, BridgePeer, SendFuture};

/// A bridge peer that runs every request and reply through a
/// cross-protocol codec before reaching the inner peer.
///
/// Constructed cheaply: the only state is the inner peer's `Arc` and
/// the codec. Both are expected to be `Send + Sync` (the codec is
/// required to be, and the inner peer is a `BridgePeer`, which already
/// is).
#[derive(Debug)]
pub struct CrossBridgePeer<U: BridgePeer + ?Sized> {
    /// The protocol the downstream leg speaks, fixed at construction.
    downstream_side: ProtocolSide,
    /// The codec that does the translation. Held by `Arc` so the same
    /// codec can be shared with the diagnostics surface (T28 follow-up)
    /// without copying the rules.
    codec: Arc<dyn CrossProtocolCodec>,
    /// The upstream peer the codec writes to and reads from.
    inner: Arc<U>,
}

impl<U: BridgePeer + ?Sized> CrossBridgePeer<U> {
    /// A wrapper around `inner` that translates through `codec`.
    ///
    /// `downstream_side` is the protocol the *downstream* leg speaks;
    /// the wrapper passes it to every call so the codec knows which
    /// direction to drive. A symmetric codec that handles both
    /// directions declares one of them in
    /// [`downstream_protocol`](CrossProtocolCodec::downstream_protocol)
    /// and lets the bridge pick the other; the bridge's
    /// `downstream_side` is the source of truth, not the codec's
    /// declaration.
    pub fn new(
        downstream_side: ProtocolSide,
        codec: Arc<dyn CrossProtocolCodec>,
        inner: Arc<U>,
    ) -> Result<Self, BridgeError> {
        Ok(Self {
            downstream_side,
            codec,
            inner,
        })
    }

    /// The protocol this peer reports for the downstream leg.
    #[must_use]
    pub const fn downstream_side(&self) -> ProtocolSide {
        self.downstream_side
    }

    /// The codec the wrapper runs envelopes through.
    #[must_use]
    pub fn codec(&self) -> &Arc<dyn CrossProtocolCodec> {
        &self.codec
    }

    /// The inner upstream peer the codec writes to.
    #[must_use]
    pub const fn inner(&self) -> &Arc<U> {
        &self.inner
    }

    /// Whether the codec bridges the given IR method.
    ///
    /// The bridge loop asks this once at feature-check time, so a
    /// method that is not bridged is refused with `-32601` *before*
    /// the codec is invoked and the inner peer is contacted. The
    /// check is the codec's `wire_method` table — the same table the
    /// codec uses to translate — so the answer cannot disagree with
    /// the codec.
    #[must_use]
    pub fn bridges(&self, ir_method: &str) -> bool {
        self.downstream_side.wire_method(ir_method).is_some()
    }
}

impl<U: BridgePeer + ?Sized> BridgePeer for CrossBridgePeer<U> {
    fn send(
        &self,
        envelope: Map<String, Value>,
    ) -> SendFuture<'_, Result<Map<String, Value>, BridgeError>> {
        Box::pin(async move {
            let ir_method = ir_method_of(&envelope);
            let mut report = RequestCodecReport::empty();
            let outbound = self
                .codec
                .encode_request(self.downstream_side, ir_method, &envelope, &mut report)
                .map_err(|e| bridge_error_from(e, self.downstream_side))?;
            let reply = self.inner.send(outbound).await?;
            self.codec
                .decode_reply(self.downstream_side, ir_method, &reply, &mut report)
                .map_err(|e| bridge_error_from(e, self.downstream_side))
        })
    }

    fn notify(
        &self,
        envelope: Map<String, Value>,
    ) -> SendFuture<'_, Result<(), BridgeError>> {
        Box::pin(async move {
            let ir_method = ir_method_of(&envelope);
            let mut report = RequestCodecReport::empty();
            let outbound = self
                .codec
                .encode_request(self.downstream_side, ir_method, &envelope, &mut report)
                .map_err(|e| bridge_error_from(e, self.downstream_side))?;
            self.inner.notify(outbound).await
        })
    }

    fn close(&self) -> SendFuture<'_, Result<(), BridgeError>> {
        self.inner.close()
    }

    fn listen(&self) -> SendFuture<'_, Result<(), BridgeError>> {
        self.inner.listen()
    }
}

/// The IR method name the codec needs.
///
/// Equal to the envelope's `method` member, because the bridge lifts
/// the wire method verbatim into the IR — the IR has no per-protocol
/// names yet, and a method the bridge cannot lift is one the codec
/// should never see.
///
/// Returns the empty string when the envelope has no `method`, which
/// is the case for replies (an inbound reply has no `method`, only
/// `id` + `result` / `error`). A reply that reaches this function
/// already passed through the inner peer; the codec will see it on
/// `decode_reply`, not here.
fn ir_method_of(envelope: &Map<String, Value>) -> &str {
    envelope
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("")
}

/// Map a codec error into a bridge error the loop can answer.
fn bridge_error_from(err: CrossProtocolError, downstream: ProtocolSide) -> BridgeError {
    match err {
        CrossProtocolError::MethodNotBridged(method, _) => {
            BridgeError::MethodNotBridged { method, downstream }
        }
        CrossProtocolError::UnknownDownstream(side) => BridgeError::MethodNotBridged {
            method: format!("<codec for {side:?}>"),
            downstream,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, TryLockError};

    use serde_json::{Map, Value, json};

    use super::{
        super::cross_protocol::{
            CrossProtocolCodec, CrossProtocolError, ProtocolSide, RequestCodecReport,
            RequestCodecStep,
        },
        super::forward::{BridgeError, BridgePeer, SendFuture},
        CrossBridgePeer,
    };

    /// A codec that renames the method but does nothing else.
    ///
    /// Built for the test that proves the wrapper translates and the
    /// test that proves a wrong-side constructor is refused, both of
    /// which are about the wrapper, not the real A2A ↔ MCP rules.
    #[derive(Debug)]
    struct RenameOnlyCodec {
        downstream: ProtocolSide,
        from: &'static str,
        to: &'static str,
    }

    impl CrossProtocolCodec for RenameOnlyCodec {
        fn downstream_protocol(&self) -> ProtocolSide {
            self.downstream
        }

        fn encode_request(
            &self,
            _downstream_side: ProtocolSide,
            ir_method: &str,
            envelope: &Map<String, Value>,
            report: &mut RequestCodecReport,
        ) -> Result<Map<String, Value>, CrossProtocolError> {
            if ir_method != self.from {
                return Err(CrossProtocolError::MethodNotBridged(
                    ir_method.to_owned(),
                    self.downstream,
                ));
            }
            report.record(RequestCodecStep::MethodRenamed {
                from: self.from,
                to: self.to,
            });
            let mut out = envelope.clone();
            out.insert("method".to_owned(), Value::String(self.to.to_owned()));
            Ok(out)
        }

        fn decode_reply(
            &self,
            _downstream_side: ProtocolSide,
            _ir_method: &str,
            reply: &Map<String, Value>,
            _report: &mut RequestCodecReport,
        ) -> Result<Map<String, Value>, CrossProtocolError> {
            Ok(reply.clone())
        }
    }

    /// A peer that records what it was sent and answers with a fixed reply.
    #[derive(Debug, Default)]
    struct RecorderPeer {
        sent: Mutex<Vec<Map<String, Value>>>,
        reply: Map<String, Value>,
    }

    impl RecorderPeer {
        fn answering_with(reply: Map<String, Value>) -> Self {
            Self {
                sent: Mutex::new(Vec::new()),
                reply,
            }
        }
    }

    impl RecorderPeer {
        /// Records `envelope` if the mutex is healthy, otherwise returns the
        /// poison as a `BridgeError`. The lock is taken and released before
        /// the future is returned so the guard is not held across an `await`
        /// — clippy's `significant_drop_tightening` flag and the loop's
        /// expectation that `send` does not block are the same preference.
        fn record(&self, envelope: Map<String, Value>) -> Result<(), BridgeError> {
            self.sent
                .lock()
                .map_err(|_| BridgeError::Close("recorder mutex poisoned".to_owned()))
                .map(|mut guard| guard.push(envelope))
        }
    }

    impl BridgePeer for RecorderPeer {
        fn send(
            &self,
            envelope: Map<String, Value>,
        ) -> SendFuture<'_, Result<Map<String, Value>, BridgeError>> {
            let reply = self.reply.clone();
            // Recording happens synchronously so the recorder is correct
            // even if the future is later dropped before it polls. A
            // poisoned mutex becomes a `Close` error in the future, which
            // is the same shape the real bridge uses for "I could not
            // record the frame" — the recorder does not invent a new
            // error type.
            let Ok(()) = self.record(envelope) else {
                return Box::pin(async {
                    Err(BridgeError::Close("recorder mutex poisoned".to_owned()))
                });
            };
            Box::pin(async move { Ok(reply) })
        }

        fn notify(
            &self,
            envelope: Map<String, Value>,
        ) -> SendFuture<'_, Result<(), BridgeError>> {
            let Ok(()) = self.record(envelope) else {
                return Box::pin(async {
                    Err(BridgeError::Close("recorder mutex poisoned".to_owned()))
                });
            };
            Box::pin(async move { Ok(()) })
        }

        fn close(&self) -> SendFuture<'_, Result<(), BridgeError>> {
            Box::pin(async { Ok(()) })
        }
    }

    fn make_envelope(method: &str) -> Map<String, Value> {
        let mut map = Map::new();
        map.insert("jsonrpc".to_owned(), Value::String("2.0".to_owned()));
        map.insert("id".to_owned(), Value::Number(1.into()));
        map.insert("method".to_owned(), Value::String(method.to_owned()));
        map.insert("params".to_owned(), Value::Object(Map::new()));
        map
    }

    fn reply_with(message: &str) -> Map<String, Value> {
        let mut map = Map::new();
        map.insert("jsonrpc".to_owned(), Value::String("2.0".to_owned()));
        map.insert("id".to_owned(), Value::Number(1.into()));
        map.insert("result".to_owned(), Value::String(message.to_owned()));
        map
    }

    fn codec_renaming_send_message_to_tools_call() -> Arc<dyn CrossProtocolCodec> {
        Arc::new(RenameOnlyCodec {
            downstream: ProtocolSide::A2a,
            from: "SendMessage",
            to: "tools/call",
        })
    }

    type TestError = Failure;

    /// What a single test can fail with, collapsed into one error type.
    ///
    /// Each variant's `From` impl lets the test use `?` without the
    /// `.expect()` calls clippy forbids. The text in the messages is
    /// what `cargo test` shows on failure, so each one names the
    /// specific thing the test could not get at.
    #[derive(Debug)]
    enum Failure {
        RecorderPoisoned,
        RecorderEmpty,
        RecordedEnvelopeMissingMethod,
        ReplyMissingResult,
        BridgeError(String),
    }

    impl core::fmt::Display for Failure {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::RecorderPoisoned => formatter.write_str("the recorder mutex was poisoned"),
                Self::RecorderEmpty => formatter.write_str("the recorder saw nothing"),
                Self::RecordedEnvelopeMissingMethod => {
                    formatter.write_str("recorded envelope had no `method` member")
                }
                Self::ReplyMissingResult => formatter.write_str("reply had no `result` member"),
                Self::BridgeError(message) => formatter.write_str(message),
            }
        }
    }

    impl std::error::Error for Failure {}

    impl<T> From<TryLockError<T>> for Failure {
        fn from(_: TryLockError<T>) -> Self {
            Self::RecorderPoisoned
        }
    }

    impl From<BridgeError> for Failure {
        fn from(err: BridgeError) -> Self {
            Self::BridgeError(format!("{err:?}"))
        }
    }

    /// Locks `mutex` once and yields a guard, with a typed error on
    /// poisoning. The clippy-forbidden `.lock().expect("...")` is the
    /// pattern this replaces; the `From` impl above makes the call
    /// site a single `?`.
    fn lock<T>(mutex: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, Failure> {
        mutex.lock().map_err(|_| Failure::RecorderPoisoned)
    }

    #[tokio::test]
    async fn a_send_envelope_is_translated_then_forwarded_then_reversed() -> Result<(), TestError> {
        let peer = Arc::new(RecorderPeer::answering_with(reply_with("ok")));
        let codec = codec_renaming_send_message_to_tools_call();
        let wrapper = CrossBridgePeer::new(ProtocolSide::A2a, codec, peer.clone())?;

        let sent = make_envelope("SendMessage");
        let reply = wrapper.send(sent).await?;

        // The inner peer saw `tools/call`, not `SendMessage`.
        let recorded = lock(&peer.sent)?;
        let only = recorded.first().ok_or(Failure::RecorderEmpty)?;
        let method = only
            .get("method")
            .and_then(Value::as_str)
            .ok_or(Failure::RecordedEnvelopeMissingMethod)?;
        assert_eq!(method, "tools/call");
        drop(recorded);

        // The reply made it back to the caller unchanged: the codec
        // returned it on `decode_reply` and the wrapper passed it through.
        let result = reply
            .get("result")
            .and_then(Value::as_str)
            .ok_or(Failure::ReplyMissingResult)?;
        assert_eq!(result, "ok");
        Ok(())
    }

    #[tokio::test]
    async fn a_method_the_codec_does_not_bridge_is_refused_with_method_not_bridged() -> Result<(), TestError> {
        let peer = Arc::new(RecorderPeer::default());
        let codec = codec_renaming_send_message_to_tools_call();
        let wrapper = CrossBridgePeer::new(ProtocolSide::A2a, codec, peer.clone())?;

        let sent = make_envelope("GetTask");
        let Err(err) = wrapper.send(sent).await else {
            return Err(Failure::BridgeError(
                "unbridged method must be refused".to_owned(),
            ));
        };

        match err {
            BridgeError::MethodNotBridged { method, downstream } => {
                assert_eq!(method, "GetTask");
                assert_eq!(downstream, ProtocolSide::A2a);
            }
            other => {
                return Err(Failure::BridgeError(format!(
                    "expected MethodNotBridged, got {other:?}"
                )));
            }
        }
        // The inner peer was never contacted.
        let recorded = lock(&peer.sent)?;
        let empty = recorded.is_empty();
        drop(recorded);
        assert!(empty, "recorder saw an envelope it should not have");
        Ok(())
    }

    #[tokio::test]
    async fn a_notify_envelope_is_translated_then_forwarded() -> Result<(), TestError> {
        let peer = Arc::new(RecorderPeer::default());
        let codec = codec_renaming_send_message_to_tools_call();
        let wrapper = CrossBridgePeer::new(ProtocolSide::A2a, codec, peer.clone())?;

        let mut envelope = Map::new();
        envelope.insert("jsonrpc".to_owned(), Value::String("2.0".to_owned()));
        envelope.insert("method".to_owned(), Value::String("SendMessage".to_owned()));
        envelope.insert("params".to_owned(), json!({}));
        wrapper.notify(envelope).await?;

        let recorded = lock(&peer.sent)?;
        let only = recorded.first().ok_or(Failure::RecorderEmpty)?;
        let method = only
            .get("method")
            .and_then(Value::as_str)
            .ok_or(Failure::RecordedEnvelopeMissingMethod)?;
        assert_eq!(method, "tools/call");
        drop(recorded);
        Ok(())
    }

    #[test]
    fn a_symmetric_codec_can_be_wrapped_for_either_downstream() -> Result<(), TestError> {
        // The codec declares `Mcp` as its downstream, but the
        // constructor accepts it for an A2a downstream too — the
        // bridge's `downstream_side` is the source of truth, not the
        // codec's declaration, and a symmetric codec handles both.
        let peer: Arc<RecorderPeer> = Arc::new(RecorderPeer::default());
        let codec: Arc<dyn CrossProtocolCodec> = Arc::new(RenameOnlyCodec {
            downstream: ProtocolSide::Mcp,
            from: "tools/call",
            to: "SendMessage",
        });
        let wrapper = match CrossBridgePeer::new(ProtocolSide::A2a, codec, peer) {
            Ok(wrapper) => wrapper,
            Err(err) => {
                return Err(Failure::BridgeError(format!(
                    "construction should succeed: {err:?}"
                )));
            }
        };
        assert_eq!(wrapper.downstream_side(), ProtocolSide::A2a);
        Ok(())
    }

    fn assert_send_sync<T: Send + Sync>(_: &T) {}

    #[test]
    fn the_wrapper_is_send_and_sync_via_arc() -> Result<(), TestError> {
        let peer: Arc<RecorderPeer> = Arc::new(RecorderPeer::default());
        let codec = codec_renaming_send_message_to_tools_call();
        let wrapper = match CrossBridgePeer::new(ProtocolSide::A2a, codec, peer) {
            Ok(wrapper) => wrapper,
            Err(err) => {
                return Err(Failure::BridgeError(format!("construction: {err:?}")));
            }
        };
        assert_send_sync(&wrapper);
        Ok(())
    }

    #[test]
    fn bridges_reports_true_only_for_methods_in_the_codec_table() -> Result<(), TestError> {
        let peer: Arc<RecorderPeer> = Arc::new(RecorderPeer::default());
        let codec = codec_renaming_send_message_to_tools_call();
        let wrapper = match CrossBridgePeer::new(ProtocolSide::A2a, codec, peer) {
            Ok(wrapper) => wrapper,
            Err(err) => {
                return Err(Failure::BridgeError(format!("construction: {err:?}")));
            }
        };
        assert!(wrapper.bridges("SendMessage"));
        assert!(!wrapper.bridges("GetTask"));
        assert!(!wrapper.bridges("tools/call"));
        Ok(())
    }

    /// The wire-level end-to-end: an A2A `SendMessage` request goes
    /// through the wrapper with the real `A2aMcpCodec` and a fake
    /// upstream MCP peer that answers with a `CallToolResult`, and the
    /// bridge returns an A2A-shaped reply.
    #[tokio::test]
    async fn the_real_a2a_mcp_codec_round_trips_through_the_wrapper() -> Result<(), TestError> {
        use crate::application::bridge::A2aMcpCodec;

        // The fake MCP peer: answers `tools/call` with a one-line
        // `CallToolResult` whose content is a `TextContent`.
        let mcp_reply: Map<String, Value> = serde_json::from_str(
            r#"{
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "content": [{"type": "text", "text": "hello"}],
                    "isError": false
                }
            }"#,
        )
        .map_err(|err| Failure::BridgeError(format!("fixture parse: {err}")))?;
        let peer = Arc::new(RecorderPeer::answering_with(mcp_reply));

        let codec: Arc<dyn CrossProtocolCodec> = Arc::new(A2aMcpCodec::new());
        let wrapper = CrossBridgePeer::new(ProtocolSide::A2a, codec, peer.clone())
            .map_err(|err| Failure::BridgeError(format!("construction: {err:?}")))?;

        // The A2A client sent: a `SendMessage` whose `message.metadata`
        // names the skill "echo" and whose `message.parts` carry a
        // data part with `{"message": "hi"}`.
        let mut envelope = Map::new();
        envelope.insert("jsonrpc".to_owned(), Value::String("2.0".to_owned()));
        envelope.insert("id".to_owned(), Value::Number(1.into()));
        envelope.insert("method".to_owned(), Value::String("SendMessage".to_owned()));
        envelope.insert(
            "params".to_owned(),
            json!({
                "message": {
                    "role": "user",
                    "parts": [
                        {"kind": "data", "data": {"message": "hi"}}
                    ]
                },
                "metadata": {"bridge_skill": "echo"}
            }),
        );

        let reply = wrapper.send(envelope).await?;

        // The inner MCP peer saw `tools/call`, not `SendMessage`.
        let recorded = lock(&peer.sent)?;
        let only = recorded.first().ok_or(Failure::RecorderEmpty)?;
        let method = only
            .get("method")
            .and_then(Value::as_str)
            .ok_or(Failure::RecordedEnvelopeMissingMethod)?;
        assert_eq!(method, "tools/call");
        let params = only
            .get("params")
            .and_then(Value::as_object)
            .ok_or_else(|| Failure::BridgeError("params was not an object".to_owned()))?;
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| Failure::BridgeError("`name` was not a string".to_owned()))?;
        assert_eq!(name, "echo");
        drop(recorded);

        // The reply came back shaped like an A2A Message, not an MCP
        // CallToolResult.
        let result = reply
            .get("result")
            .ok_or(Failure::ReplyMissingResult)?;
        let message = result
            .as_object()
            .and_then(|m| m.get("message"))
            .and_then(Value::as_object)
            .ok_or_else(|| {
                Failure::BridgeError("reply.result.message was not an object".to_owned())
            })?;
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .ok_or_else(|| Failure::BridgeError("`role` was not a string".to_owned()))?;
        assert_eq!(role, "assistant");
        let parts = message
            .get("parts")
            .and_then(Value::as_array)
            .ok_or_else(|| Failure::BridgeError("`parts` was not an array".to_owned()))?;
        assert_eq!(parts.len(), 1, "expected one part in the reply");
        Ok(())
    }
}
