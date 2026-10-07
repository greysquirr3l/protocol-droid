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

use serde_json::{Map, Value};

use crate::application::bridge::forward::{Bridge, BridgeError, BridgePeer};
use crate::application::jsonrpc::{DynFrameSource, SourceError};
use crate::application::session::{DynFrameSink, SinkError};

/// A frame the bridge could not parse at all.
const PARSE_ERROR: i64 = -32_700;
/// A frame that parsed but is not a JSON-RPC message.
const INVALID_REQUEST: i64 = -32_600;
/// A method the two peers have no shared feature for.
const METHOD_NOT_FOUND: i64 = -32_601;
/// A transport or upstream fault.
const SERVER_ERROR: i64 = -32_000;

/// What one pass of the loop moved.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LoopReport {
    /// Envelopes the upstream peer answered.
    pub forwarded: u64,
    /// Envelopes the upstream peer was told about and correctly sent nothing back for.
    pub notified: u64,
    /// Envelopes the bridge refused or could not read, each answered to the client.
    pub failed: u64,
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
}

/// What one envelope produced.
#[derive(Debug)]
enum Outcome {
    /// A reply for the downstream peer.
    Reply(Value),
    /// The upstream peer had nothing to say, which is correct for a notification.
    Silent,
}

/// Carries frames from a downstream peer, through a [`Bridge`], back to it.
///
/// # Examples
///
/// ```
/// use protocol_droid::application::bridge::BridgeLoop;
///
/// # fn documented<H, U, S, K>(
/// #     bridge: protocol_droid::application::bridge::Bridge<H, U>,
/// #     source: S,
/// #     sink: K,
/// # ) where
/// #     H: protocol_droid::application::bridge::BridgePeer,
/// #     U: protocol_droid::application::bridge::BridgePeer,
/// #     S: protocol_droid::application::jsonrpc::DynFrameSource,
/// #     K: protocol_droid::application::session::DynFrameSink,
/// # {
/// let bridge_loop = BridgeLoop::new(bridge, source, sink);
/// # let _ = bridge_loop;
/// # }
/// ```
#[derive(Debug)]
pub struct BridgeLoop<H, U, S, K>
where
    H: BridgePeer,
    U: BridgePeer,
    S: DynFrameSource,
    K: DynFrameSink,
{
    bridge: Bridge<H, U>,
    source: S,
    sink: K,
}

impl<H, U, S, K> BridgeLoop<H, U, S, K>
where
    H: BridgePeer,
    U: BridgePeer,
    S: DynFrameSource,
    K: DynFrameSink,
{
    /// A loop over a bridge and the two framed peers around it.
    #[must_use]
    pub const fn new(bridge: Bridge<H, U>, source: S, sink: K) -> Self {
        Self {
            bridge,
            source,
            sink,
        }
    }

    /// Runs until the downstream peer's stream ends.
    ///
    /// # Errors
    ///
    /// [`LoopError`] when the downstream pipe itself fails, or when a reply the
    /// bridge built cannot be written. A refusal from the upstream peer is not
    /// an error here — it is answered to the client and counted in
    /// [`LoopReport::failed`].
    pub async fn run(&self) -> Result<LoopReport, LoopError> {
        let mut report = LoopReport::default();
        while let Some(frame) = self.source.next_frame().await? {
            self.step(&frame, &mut report).await?;
        }
        Ok(report)
    }

    /// Routes one inbound frame to whatever shape it turned out to have.
    async fn step(&self, frame: &str, report: &mut LoopReport) -> Result<(), LoopError> {
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
    async fn batch(&self, members: &[Value], report: &mut LoopReport) -> Result<(), LoopError> {
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
    async fn dispatch(&self, envelope: &Map<String, Value>, report: &mut LoopReport) -> Outcome {
        match self.bridge.forward(envelope).await {
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
    }
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
    use crate::domain::{Agreement, DegradePolicy, Feature, FeatureProfile, Leg, ProtocolVersion};

    use super::{Bridge, BridgeLoop, LoopError, LoopReport, SourceError, kind};

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
    }

    /// An upstream that answers with the request's own id.
    #[derive(Default)]
    struct Upstream {
        seen: Mutex<Vec<Map<String, Value>>>,
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
    }

    /// Hands over a fixed list of frames, then reports end-of-stream.
    #[derive(Default)]
    struct Scripted {
        frames: Mutex<VecDeque<String>>,
    }

    impl Scripted {
        fn of(frames: &[&str]) -> Self {
            let queue = frames.iter().map(|frame| (*frame).to_owned()).collect();
            Self {
                frames: Mutex::new(queue),
            }
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
            std::future::ready(next)
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
    fn agreeing(features: &[Feature]) -> Agreement {
        let version = ProtocolVersion::latest_known();
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

    /// Runs one loop over `frames`, handing back the report and what was written.
    async fn run(
        agreement: &Agreement,
        frames: &[&str],
    ) -> Result<(LoopReport, Vec<Value>), Failure> {
        let recording: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let bridge = Bridge::new(
            agreement.clone(),
            DegradePolicy::default(),
            Upstream::default(),
            Upstream::default(),
        );
        let loop_ = BridgeLoop::new(
            bridge,
            Scripted::of(frames),
            Written {
                frames: recording.clone(),
            },
        );
        let report = loop_.run().await?;
        Ok((report, written(&recording)?))
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

        assert_eq!(report, LoopReport::default());
        assert!(replies.is_empty());
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
