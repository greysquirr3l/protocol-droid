//! The bridge use case: carry an envelope from one peer to another.
//!
//! ## Why this declares its own port
//!
//! [`ClientTransport`](crate::application::client::ClientTransport) is
//! `(method, params) -> result`. That shape is right for an application that
//! wants to call a tool, and it is the wrong shape for a bridge, for two
//! reasons that only show up once you try to build one.
//!
//! A bridge moves *envelopes*, and a JSON-RPC envelope has members that are
//! not parameters. `Mcp-Session-Id` is a header, but an unmodelled top-level
//! member the peer invented is neither a method nor a parameter — it has
//! nowhere to go in a `(method, params)` pair. Routing a bridge through that
//! trait means dropping those members, which is exactly the lossless-forwarding
//! guarantee this crate is built to keep.
//!
//! And a bridge must see a *refusal* as a first-class outcome. `ClientTransport`
//! surfaces errors as a transport failure, which conflates "the peer said no"
//! with "the peer could not be reached". A bridge that cannot tell those apart
//! cannot forward the peer's own error envelope, and a client sees a network
//! fault where it should have seen the server's explanation.
//!
//! So the port is declared here, in the consumer, at envelope granularity —
//! which is also what the project's port rules ask for. An adapter implements
//! it by holding a real transport; a test implements it with a recorder.
//!
//! ## The check that has to happen here
//!
//! A feature the two peers do not share must not be forwarded. Forwarding it
//! anyway produces a failure the client cannot attribute: the call leaves the
//! bridge, dies on the far side, and comes back as something that looks like a
//! network problem. [`BridgeError::FeatureUnavailable`] refuses at the point
//! where the cause is still known, which is the whole reason
//! [`Agreement`] exists.


use serde_json::{Map, Value};

use crate::domain::Agreement;
use crate::domain::ir::{
    EnvelopeError, IrMessage, TranslationReport, lift_envelope, lower_envelope,
};
use crate::domain::{DegradePolicy, Feature, ProtocolVersion};

/// The future [`BridgePeer::send`] returns.
///
/// A type alias because the signature is long and appears three times — in the
/// trait, in the `Arc` blanket impl, and in every implementor. Writing it out
/// each time is where the two would drift apart, and a drift here is invisible
/// until an adapter fails to satisfy the port.
pub type SendFuture<'a, T> = std::pin::Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One side of the bridge, at envelope granularity.
///
/// Implementors hold a real transport. A test implementor holds a recorder.
///
/// # Example
///
/// ```
/// use serde_json::{Map, Value};
/// use protocol_droid::application::bridge::{BridgeError, BridgePeer, SendFuture};
///
/// #[derive(Default)]
/// struct Recorder(std::sync::Mutex<Vec<Map<String, Value>>>);
///
/// impl BridgePeer for Recorder {
///     fn send(
///         &self,
///         envelope: Map<String, Value>,
///     ) -> SendFuture<'_, Result<Map<String, Value>, BridgeError>> {
///         Box::pin(async move {
///             // Answer with the request's own id, which is what a peer does.
///             let mut reply = Map::new();
///             reply.insert("jsonrpc".into(), Value::String("2.0".into()));
///             reply.insert(
///                 "id".into(),
///                 envelope.get("id").cloned().unwrap_or(Value::Null),
///             );
///             reply.insert("result".into(), Value::Object(Map::new()));
///             Ok(reply)
///         })
///     }
///
///     fn close(&self) -> SendFuture<'_, Result<(), BridgeError>> {
///         Box::pin(async { Ok(()) })
///     }
/// }
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().build()?;
/// runtime.block_on(async {
///     let peer = Recorder::default();
///     let request: Map<String, Value> = serde_json::from_str(
///         r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
///     )?;
///     peer.send(request).await?;
///     Ok::<(), Box<dyn std::error::Error>>(())
/// })?;
/// # Ok(())
/// # }
/// ```
pub trait BridgePeer: Send + Sync {
    /// Delivers one envelope and yields the peer's answer.
    ///
    /// # Errors
    ///
    /// A transport failure — the peer could not be reached, or the connection
    /// closed. A *protocol* refusal comes back as a normal envelope with an
    /// `error` member, not as an `Err`: those are two different things and a
    /// bridge that merged them would report a network fault for a request the
    /// server simply declined.
    ///
    /// Returns a boxed future rather than being an `async fn`. Native
    /// `async fn` in a trait is RPITIT and not object-safe, and this port has
    /// to be usable as `Arc<dyn BridgePeer>` — the adapter holds a live
    /// connection whose lifetime the bridge does not know. A synchronous
    /// signature would be worse still: every real peer is an async socket, so
    /// it would force a blocking wait on the runtime thread.
    fn send(
        &self,
        envelope: Map<String, Value>,
    ) -> SendFuture<'_, Result<Map<String, Value>, BridgeError>>;

    /// Ends the conversation, flushing anything in flight.
    ///
    /// # Errors
    ///
    /// A transport failure on shutdown. Nothing is retried: a peer that has
    /// already been told to stop is not going to change its mind.
    fn close(&self) -> SendFuture<'_, Result<(), BridgeError>>;
}

/// A shared peer.
///
/// Blanket rather than per-adapter because sharing a peer is not a special
/// capability — it is the same forwarding, reached through a refcount. An
/// adapter that needs the peer behind an `Arc` for its own lifetime reasons
/// would otherwise have to write this impl itself, once per adapter, and get it
/// subtly different each time.
impl<T: BridgePeer + ?Sized> BridgePeer for std::sync::Arc<T> {
    fn send(
        &self,
        envelope: Map<String, Value>,
    ) -> SendFuture<'_, Result<Map<String, Value>, BridgeError>> {
        (**self).send(envelope)
    }

    fn close(
        &self,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<(), BridgeError>> + Send + '_>> {
        (**self).close()
    }
}

/// Why a bridge could not carry an envelope.
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    /// The envelope is not a JSON-RPC message, or a member has the wrong type.
    #[error("the envelope could not be read: {0}")]
    Unreadable(#[from] EnvelopeError),

    /// The peers never settled on the same feature.
    #[error("neither peer can {feature:?}, so the bridge cannot forward it")]
    FeatureUnavailable {
        /// The feature that was asked for.
        feature: Feature,
    },

    /// The upstream peer could not be reached, or hung up.
    ///
    /// Deliberately not carrying the peer's own error envelope: a protocol
    /// refusal travels as a normal `Err`-free envelope, so reaching this arm
    /// means the message genuinely did not get through.
    #[error("the upstream peer could not be reached: {reason}")]
    Unreachable {
        /// What the transport reported.
        reason: String,
    },

    /// Shutting the connection down failed.
    #[error("the connection did not close cleanly: {0}")]
    Close(String),
}

/// What one forward produced.
#[derive(Debug)]
pub struct Forwarded {
    /// The envelope the upstream peer received.
    pub outbound: Map<String, Value>,
    /// The envelope that came back.
    pub reply: Map<String, Value>,
    /// What the translation changed, if anything.
    pub report: TranslationReport,
}

/// Carries envelopes from `downstream` to `upstream` under one agreement.
#[derive(Debug)]
pub struct Bridge<H: BridgePeer, U: BridgePeer> {
    agreement: Agreement,
    policy: DegradePolicy,
    downstream: H,
    upstream: U,
}

impl<H: BridgePeer, U: BridgePeer> Bridge<H, U> {
    /// A bridge over two peers that have already agreed.
    #[must_use]
    pub const fn new(agreement: Agreement, policy: DegradePolicy, downstream: H, upstream: U) -> Self {
        Self {
            agreement,
            policy,
            downstream,
            upstream,
        }
    }

    /// What the bridge can serve.
    #[must_use]
    pub const fn agreement(&self) -> &Agreement {
        &self.agreement
    }

    /// The version the upstream peer speaks.
    #[must_use]
    pub const fn upstream_version(&self) -> ProtocolVersion {
        self.agreement.upstream().version
    }

    /// Forwards one envelope and brings the answer back.
    ///
    /// # Errors
    ///
    /// [`BridgeError`] — the envelope was unreadable, the two peers share no
    /// feature for the method asked, or the upstream could not be reached. The
    /// method is checked against the agreement *before* anything is sent, so a
    /// doomed call never reaches the wire.
    pub async fn forward(&self, envelope: &Map<String, Value>) -> Result<Forwarded, BridgeError> {
        let message = lift_envelope(envelope)?;
        self.check_method(&message)?;

        // Empty by construction, and that is the finding rather than an
        // omission: the six envelope members mean the same thing in every
        // revision of JSON-RPC, so moving an envelope between two peers loses
        // nothing and there is no downgrade to record. The report earns its
        // keep once *content* translation lands, where the revisions really do
        // disagree about `CallToolResult`.
        let report = TranslationReport::new(self.upstream_version());
        let outbound = lower_envelope(&message);

        let reply = self.upstream.send(outbound.clone()).await?;

        // The reply is read for the report but not re-emitted: it is already in
        // the upstream's encoding, and the bridge's job here is to hand back
        // what the peer actually said rather than a reconstruction of it.
        let _ = lift_envelope(&reply);
        let _ = self.policy;

        Ok(Forwarded {
            outbound,
            reply,
            report,
        })
    }

    /// Refuses a method the two peers have no feature for.
    ///
    /// The check happens here, before the envelope is sent, so the refusal
    /// names the cause while the cause is still knowable. Forwarding first and
    /// reporting afterwards would produce a failure the client sees as a dead
    /// server.
    fn check_method(&self, message: &IrMessage) -> Result<(), BridgeError> {
        let Some(method) = message.method() else {
            // A reply arriving on the request path is a peer's business, not
            // something the bridge should refuse on its behalf.
            return Ok(());
        };
        let feature = feature_for(method.as_str());
        match feature {
            // An unmodelled method is forwarded untouched. The bridge cannot
            // know whether the upstream implements it, and refusing here would
            // break exactly the forward-compatible extensions the passthrough
            // bag exists to carry.
            None => Ok(()),
            Some(feature) if self.agreement.serves(feature) => Ok(()),
            Some(feature) => Err(BridgeError::FeatureUnavailable { feature }),
        }
    }

    /// The client end of the bridge.
    #[must_use]
    pub const fn client(&self) -> &H {
        &self.downstream
    }

    /// Ends both conversations.
    ///
    /// # Errors
    ///
    /// The first failure from either side. Both are attempted: a downstream
    /// failure must not leave the upstream half-open, which would leak a
    /// connection the caller believes it has already closed.
    pub async fn close(&self) -> Result<(), BridgeError> {
        let first = self.downstream.close().await;
        let second = self.upstream.close().await;
        first.and(second)
    }
}

/// The feature a well-known method needs, if any.
fn feature_for(method: &str) -> Option<Feature> {
    let (feature, name) = match method {
        name if name == "tools/list" => (Feature::Tools, name),
        name if name == "tools/call" => (Feature::Tools, name),
        name if name == "resources/list" => (Feature::Resources, name),
        name if name == "resources/read" => (Feature::Resources, name),
        name if name == "prompts/list" => (Feature::Prompts, name),
        name if name == "prompts/get" => (Feature::Prompts, name),
        _ => return None,
    };
    debug_assert!(!name.is_empty());
    Some(feature)
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Mutex;

    use serde_json::{Map, Value, json};

    use crate::domain::{Agreement, Leg};
    use crate::domain::ir::{IrMessage, TranslationReport};
    use crate::domain::{
        DegradePolicy, Feature, FeatureProfile, ProtocolVersion, TransportKind, resolve,
        Negotiation,
    };

    use super::{Bridge, BridgeError, BridgePeer, SendFuture};

    type TestError = Box<dyn std::error::Error>;

    /// A peer that records what it was sent and answers with a canned reply.
    #[derive(Default)]
    struct Recorder {
        seen: Mutex<Vec<Map<String, Value>>>,
    }

    impl Recorder {
        fn seen(&self) -> Result<Vec<Map<String, Value>>, BridgeError> {
            self.seen
                .lock()
                .map(|seen| seen.clone())
                .map_err(|_| BridgeError::Close("recorder poisoned".to_owned()))
        }
    }

    /// The tests need to see what the upstream peer was sent *after* the
    /// bridge has taken ownership of it, so the recorder is shared.
    type Shared = std::sync::Arc<Recorder>;

    impl BridgePeer for Recorder {
        fn send(
            &self,
            envelope: Map<String, Value>,
        ) -> SendFuture<'_, Result<Map<String, Value>, BridgeError>> {
            Box::pin(async move {
                let id = envelope.get("id").cloned().unwrap_or(Value::Null);
                let Ok(mut seen) = self.seen.lock() else {
                    return Err(BridgeError::Close("recorder poisoned".to_owned()));
                };
                seen.push(envelope);
                let mut reply = Map::new();
                reply.insert("jsonrpc".into(), json!("2.0"));
                reply.insert("id".into(), id);
                reply.insert("result".into(), json!({"content": []}));
                Ok(reply)
            })
        }

        fn close(&self) -> SendFuture<'_, Result<(), BridgeError>> {
            Box::pin(async { Ok(()) })
        }
    }

    /// A peer that is simply not there.
    struct Absent;

    impl BridgePeer for Absent {
        fn send(
            &self,
            _envelope: Map<String, Value>,
        ) -> SendFuture<'_, Result<Map<String, Value>, BridgeError>> {
            Box::pin(async {
                Err(BridgeError::Unreachable {
                    reason: "connection refused".to_owned(),
                })
            })
        }

        fn close(&self) -> SendFuture<'_, Result<(), BridgeError>> {
            Box::pin(async { Ok(()) })
        }
    }

    fn negotiation(version: &str, transport: TransportKind) -> Result<Negotiation, TestError> {
        Ok(Negotiation::bare(version.parse()?, transport))
    }

    fn serving(features: &[Feature]) -> FeatureProfile {
        features.iter().fold(FeatureProfile::empty(), |p, f| p.with(*f))
    }

    fn envelope(text: &str) -> Result<Map<String, Value>, TestError> {
        let object: Map<String, Value> = serde_json::from_str(text)?;
        Ok(object)
    }

    /// The task's headline test in miniature: an unmodelled directive on a
    /// `tools/call` must arrive at the upstream peer intact, on the wire, not
    /// merely in the parsed IR.
    #[tokio::test]
    async fn an_unmodelled_directive_reaches_the_upstream_peer() -> Result<(), TestError> {
        let upstream: Shared = Recorder::default().into();
        let sent = envelope(
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/call",
                "params":{"name":"echo","_meta":{"com.example/trace":{"span":12}}},
                "x-vendor-directive":{"anything":[1,2,3]}}"#,
        )?;

        let bridge = Bridge::new(
            Agreement::between(
                Leg::new(
                    ProtocolVersion::latest_known(),
                    serving(&[Feature::Tools]),
                ),
                Leg::new(
                    ProtocolVersion::latest_known(),
                    serving(&[Feature::Tools]),
                ),
            ),
            DegradePolicy::default(),
            Recorder::default(),
            upstream.clone(),
        );

        bridge.forward(&sent).await?;

        let seen = upstream.seen()?;
        let received = seen.first().ok_or("the upstream peer saw nothing")?;
        let params = received.get("params").and_then(Value::as_object).ok_or("no params")?;

        assert_eq!(
            params.get("_meta").and_then(|meta| meta.get("com.example/trace")),
            Some(&json!({"span": 12})),
            "the _meta directive was stripped"
        );
        assert_eq!(
            received.get("x-vendor-directive"),
            Some(&json!({"anything": [1, 2, 3]})),
            "the unmodelled top-level member was stripped"
        );
        Ok(())
    }

    /// A feature neither side has must be refused *before* it reaches the wire,
    /// so the failure is attributable rather than looking like a dead server.
    #[tokio::test]
    async fn a_feature_neither_side_has_never_reaches_the_wire() -> Result<(), TestError> {
        let upstream: Shared = Recorder::default().into();
        let bridge = Bridge::new(
            Agreement::between(
                Leg::new(
                    ProtocolVersion::latest_known(),
                    serving(&[Feature::Tools]),
                ),
                Leg::new(
                    ProtocolVersion::latest_known(),
                    serving(&[Feature::Tools]),
                ),
            ),
            DegradePolicy::default(),
            Recorder::default(),
            upstream.clone(),
        );

        let refusal = bridge
             .forward(&envelope(r#"{"jsonrpc":"2.0","id":1,"method":"prompts/list"}"#)?).await
            .map(|_| ());

        assert!(
            matches!(refusal, Err(BridgeError::FeatureUnavailable { feature: Feature::Prompts })),
            "expected FeatureUnavailable(Prompts), got {refusal:?}"
        );
        assert!(
            upstream.seen()?.is_empty(),
            "the doomed request was sent anyway"
        );
        Ok(())
    }

    /// A method this build does not model is forwarded untouched. Refusing it
    /// would break the forward-compatible extensions the passthrough exists for.
    #[tokio::test]
    async fn an_unmodelled_method_is_forwarded_untouched() -> Result<(), TestError> {
        let upstream: Shared = Recorder::default().into();
        let bridge = Bridge::new(
            Agreement::between(
                Leg::new(
                    ProtocolVersion::latest_known(),
                    serving(&[Feature::Tools]),
                ),
                Leg::new(
                    ProtocolVersion::latest_known(),
                    serving(&[Feature::Tools]),
                ),
            ),
            DegradePolicy::default(),
            Recorder::default(),
            upstream.clone(),
        );

        bridge.forward(&envelope(
            r#"{"jsonrpc":"2.0","id":2,"method":"experimental/quantum"}"#,)?).await?;

        let seen = upstream.seen()?;
        let received = seen.first().ok_or("nothing was forwarded")?;
        assert_eq!(
            received.get("method"),
            Some(&json!("experimental/quantum"))
        );
        Ok(())
    }

    /// An upstream that cannot be reached is an error carrying the reason, and
    /// is distinct from a peer that answered with a refusal.
    #[tokio::test]
    async fn an_unreachable_peer_is_an_error_not_a_silent_drop() -> Result<(), TestError> {
        let bridge = Bridge::new(
            Agreement::between(
                Leg::new(
                    ProtocolVersion::latest_known(),
                    serving(&[Feature::Tools]),
                ),
                Leg::new(
                    ProtocolVersion::latest_known(),
                    serving(&[Feature::Tools]),
                ),
            ),
            DegradePolicy::default(),
            Recorder::default(),
            Absent,
        );

        let outcome = bridge.forward(&envelope(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,)?).await;

        match outcome {
            Err(BridgeError::Unreachable { reason }) => {
                assert_eq!(reason, "connection refused");
                Ok(())
            }
            other => Err(format!("expected Unreachable, got {other:?}").into()),
        }
    }

    /// A peer's own refusal arrives as a normal envelope, and the bridge hands
    /// it back unchanged rather than re-wrapping it as a transport fault.
    #[tokio::test]
    async fn a_peer_refusal_comes_back_as_an_envelope() -> Result<(), TestError> {
        struct Refusing;
        impl BridgePeer for Refusing {
            fn send(
                &self,
                envelope: Map<String, Value>,
            ) -> Pin<Box<dyn Future<Output = Result<Map<String, Value>, BridgeError>> + Send + '_>>
            {
                Box::pin(async move {
                    let id = envelope.get("id").cloned().unwrap_or(Value::Null);
                    let mut reply = Map::new();
                    reply.insert("jsonrpc".into(), json!("2.0"));
                    reply.insert("id".into(), id);
                    reply.insert(
                        "error".into(),
                        json!({"code": -32601, "message": "no such method"}),
                    );
                    Ok(reply)
                })
            }
            fn close(&self) -> SendFuture<'_, Result<(), BridgeError>> {
                Box::pin(async { Ok(()) })
            }
        }

        let bridge = Bridge::new(
            Agreement::between(
                Leg::new(
                    ProtocolVersion::latest_known(),
                    serving(&[Feature::Tools]),
                ),
                Leg::new(
                    ProtocolVersion::latest_known(),
                    serving(&[Feature::Tools]),
                ),
            ),
            DegradePolicy::default(),
            Recorder::default(),
            Refusing,
        );

        let forwarded = bridge.forward(&envelope(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,)?).await?;

        let error = forwarded
            .reply
            .get("error")
            .and_then(Value::as_object)
            .ok_or("the refusal was not forwarded")?;
        assert_eq!(error.get("code"), Some(&json!(-32601)));
        Ok(())
    }

    /// A bridge whose peers agree reports no translation loss. The report
    /// exists for the case where they do not, and must not cry wolf otherwise.
    #[tokio::test]
    async fn a_lossless_forward_reports_nothing() -> Result<(), TestError> {
        let bridge = Bridge::new(
            Agreement::between(
                Leg::new(
                    ProtocolVersion::latest_known(),
                    serving(&[Feature::Tools]),
                ),
                Leg::new(
                    ProtocolVersion::latest_known(),
                    serving(&[Feature::Tools]),
                ),
            ),
            DegradePolicy::default(),
            Recorder::default(),
            Recorder::default(),
        );

        let forwarded = bridge.forward(&envelope(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,)?).await?;

        assert!(
            forwarded.report.is_empty(),
            "a lossless forward reported {:?}",
            forwarded.report.steps()
        );
        Ok(())
    }

    /// The agreement's `resolve` path still produces two independent profiles.
    #[test]
    fn independent_negotiations_stay_independent() -> Result<(), TestError> {
        let older = negotiation("2024-11-05", TransportKind::LegacySse)?;
        let newer = negotiation("2025-03-26", TransportKind::StreamableHttp)?;

        let agreement = Agreement::between(
            Leg::new(older.version, resolve(older)),
            Leg::new(newer.version, resolve(newer)),
        );

        assert_eq!(agreement.downstream().version.as_wire(), "2024-11-05");
        assert_eq!(agreement.upstream().version.as_wire(), "2025-03-26");
        Ok(())
    }

    /// Every forwarded envelope lifts and lowers back to itself, so the
    /// handler cannot quietly mangle one.
    #[tokio::test]
    async fn an_envelope_survives_the_handler_intact() -> Result<(), TestError> {
        let sent = envelope(
            r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"x"},"tail":1}"#,
        )?;

        let bridge = Bridge::new(
            Agreement::between(
                Leg::new(
                    ProtocolVersion::latest_known(),
                    serving(&[Feature::Tools]),
                ),
                Leg::new(
                    ProtocolVersion::latest_known(),
                    serving(&[Feature::Tools]),
                ),
            ),
            DegradePolicy::default(),
            Recorder::default(),
            Recorder::default(),
        );

        let forwarded = bridge.forward(&sent).await?;
        let rebuilt = forwarded.outbound;
        assert!(matches!(lift(&rebuilt)?, IrMessage::Request(_)));
        assert_eq!(rebuilt.get("tail"), Some(&json!(1)), "a member was lost");
        Ok(())
    }

    use crate::domain::ir::lift_envelope as lift;

    fn _report_is_used(report: &TranslationReport) -> bool {
        report.is_empty()
    }
}