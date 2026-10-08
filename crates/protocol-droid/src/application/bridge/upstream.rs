//! One upstream per downstream client.
//!
//! ## Why a factory rather than a connection
//!
//! A bridge proxies. Every client that connects needs a server to be proxied
//! *to*, and that server has to be its own: a session upstream is bound to one
//! client, so two clients sharing one connection are two clients inside a
//! session the upstream believes belongs to a single peer.
//!
//! That is invisible on `stdio`, where one process is one client, and it is why
//! the first version of this bridge opened its upstream once at startup and got
//! away with it. It is not a property of bridges, it is a property of `stdio`.
//! The moment a client arrives on a network transport the shape stops working:
//!
//! - A frame the server volunteers — `notifications/tools/list_changed` and the
//!   rest — names no client. Shared, it has nowhere to go that is honest.
//! - A request the server originates (sampling, elicitation) arrives on the same
//!   stream with an `id` and a `method`, and has to be answered by the client
//!   it was addressed to. Shared, there is no way to know which.
//!
//! Both are the same bug seen from two sides: one connection serving many
//! clients has no way to attribute a message to one of them. So
//! [`Upstream`] is called once per client, and [`run_client`] is the only
//! supported way to run a loop, which makes the multiplicity impossible to get
//! wrong by accident.
//!
//! ## Why the leg and the peer travel together
//!
//! [`OpenedUpstream`] exists to stop a caller opening one connection,
//! negotiating against another, and forwarding over a third. The result would
//! be an [`Agreement`](Agreement) describing a server
//! the bridge is not talking to — and that type exists to be the truth about
//! two real peers. The pairing is the claim; a `(Leg, Peer)` tuple is two
//! values that can be mixed, and this is not one of those.
//!
//! # Examples
//!
//! An upstream that reaches one fixed endpoint, opened once per client:
//!
//! ```
//! use std::future::Future;
//!
//! use protocol_droid::application::bridge::{OpenedUpstream, Upstream, UpstreamError};
//!
//! /// An upstream backed by whatever "dial and negotiate" means for a caller.
//! struct FromDial<F> {
//!     dial: F,
//! }
//!
//! impl<F, Fut> Upstream for FromDial<F>
//! where
//!     F: Fn() -> Fut + Send + Sync,
//!     Fut: Future<Output = Result<OpenedUpstream, UpstreamError>> + Send,
//! {
//!     fn open(&self) -> impl Future<Output = Result<OpenedUpstream, UpstreamError>> + Send {
//!         (self.dial)()
//!     }
//! }
//!
//! # async fn dial() -> Result<OpenedUpstream, UpstreamError> {
//! #     Err(UpstreamError::Unreachable("not connected".to_owned()))
//! # }
//! # async fn example() -> Result<(), UpstreamError> {
//! // Called once per client, and every call is expected to be a new connection.
//! let upstream = FromDial { dial };
//! let _opened = upstream.open().await?;
//! # Ok(())
//! # }
//! ```
//!
//! The closure is where the transport goes, and the one thing to keep straight
//! inside it: dial, then negotiate against the connection just dialled. Opening
//! one connection and negotiating over another produces a `Leg` describing a
//! server the bridge is not talking to, which `OpenedUpstream` cannot express
//! even if you wanted it to.

use std::sync::Arc;

use crate::application::bridge::{
    BridgeError, BridgeLoop, BridgePeer, DownstreamConfig, LoopError, LoopReport, ToolResultCodec,
    Wiring,
};
use crate::application::jsonrpc::DynFrameSource;
use crate::application::session::DynFrameSink;
use crate::domain::{DegradePolicy, Leg};

/// One upstream connection, and the leg describing it.
///
/// Constructed by [`Upstream::open`] rather than by a caller, so the pairing
/// survives into the wiring instead of being re-established at the call site.
#[derive(Clone)]
pub struct OpenedUpstream {
    leg: Leg,
    peer: Arc<dyn BridgePeer>,
}

impl core::fmt::Debug for OpenedUpstream {
    /// By hand, because a peer is a live connection and not something with a
    /// readable state. The leg is the claim worth seeing; the peer is the thing
    /// that makes it true and has no `Debug` to print.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OpenedUpstream")
            .field("leg", &self.leg)
            .field("peer", &"<a live connection>")
            .finish()
    }
}

impl OpenedUpstream {
    /// Pairs a negotiated leg with the peer it was negotiated against.
    #[must_use]
    pub fn new(leg: Leg, peer: Arc<dyn BridgePeer>) -> Self {
        Self { leg, peer }
    }

    /// What the upstream agreed to.
    #[must_use]
    pub const fn leg(&self) -> Leg {
        self.leg
    }

    /// The connection the leg was negotiated against.
    #[must_use]
    pub fn peer(&self) -> &Arc<dyn BridgePeer> {
        &self.peer
    }

    /// Splits into the two halves [`Wiring::new`] takes.
    #[must_use]
    pub fn into_parts(self) -> (Leg, Arc<dyn BridgePeer>) {
        (self.leg, self.peer)
    }
}

/// Opens upstreams, one per downstream client.
///
/// Called once for each client, and each call is expected to produce a fresh
/// connection. An implementation that hands back the same connection twice
/// reintroduces exactly the sharing this exists to prevent, and a bridge that
/// does it will lose a `tools/list_changed` or fail to answer a sampling
/// request with no error anywhere to show for it.
pub trait Upstream: Send + Sync {
    /// Opens and negotiates one upstream.
    ///
    /// # Errors
    ///
    /// [`UpstreamError`] when the server could not be reached or refused the
    /// handshake. This is the only failure channel on purpose: a client whose
    /// server is unreachable has no bridge to be proxied through, so the caller
    /// answers the client rather than starting a loop that would fail on its
    /// first frame.
    fn open(&self) -> impl Future<Output = Result<OpenedUpstream, UpstreamError>> + Send;
}

/// Why an upstream could not be opened.
#[derive(Debug, thiserror::Error)]
pub enum UpstreamError {
    /// The server could not be reached, or declined the handshake.
    ///
    /// A `String` because the transport names its own failures and the
    /// application layer has no business enumerating them — the same reason
    /// [`TransportError`](crate::application::client::TransportError) is a
    /// string.
    #[error("{0}")]
    Unreachable(String),
}

impl From<BridgeError> for UpstreamError {
    fn from(error: BridgeError) -> Self {
        Self::Unreachable(error.to_string())
    }
}

/// Why one client's bridge stopped.
#[derive(Debug, thiserror::Error)]
pub enum BridgeClientError {
    /// This client's upstream could not be opened.
    #[error("the upstream could not be opened: {0}")]
    Upstream(#[from] UpstreamError),

    /// The client's own channel failed.
    ///
    /// A refusal the upstream returned is **not** here. Those are answered to
    /// the client and counted in [`LoopReport::failed`]; only a broken pipe
    /// ends the loop.
    #[error("{0}")]
    Loop(#[from] LoopError),
}

/// Runs one client from end to end against its own upstream.
///
/// The only supported way to run a [`BridgeLoop`] against a caller-chosen
/// upstream, because it is the only one that guarantees the two halves were
/// opened together. A caller assembling a `Wiring` by hand can still do it, and
/// can still get it wrong; this is the shape that cannot be wrong by omission.
///
/// # Errors
///
/// [`BridgeClientError::Upstream`] when this client's server could not be
/// reached, and [`BridgeClientError::Loop`] when the client's own channel
/// fails. The two are kept apart because they mean opposite things to whoever
/// is serving: the first is a server the bridge cannot reach, the second is a
/// client that went away mid-conversation.
pub async fn run_client<U, C, S, K>(
    upstream: &U,
    policy: DegradePolicy,
    config: DownstreamConfig,
    codec: C,
    source: S,
    sink: K,
) -> Result<LoopReport, BridgeClientError>
where
    U: Upstream,
    C: ToolResultCodec,
    S: DynFrameSource,
    K: DynFrameSink,
{
    let (leg, peer) = upstream.open().await?.into_parts();
    let mut bridge_loop =
        BridgeLoop::new(Wiring::new(leg, policy, peer, codec), config, source, sink);
    Ok(bridge_loop.run().await?)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use serde_json::{Map, Value, json};

    use super::{BridgeClientError, OpenedUpstream, Upstream, UpstreamError, run_client};
    use crate::application::bridge::{
        BridgeError, BridgePeer, DownstreamConfig, LoopError, LoopReport, SendFuture,
        ToolResultCodec,
    };
    use crate::application::jsonrpc::{DynFrameSource, FrameSource, SourceError};
    use crate::application::session::{DynFrameSink, FrameSink, SinkError};
    use crate::domain::feature::{Feature, FeatureProfile};
    use crate::domain::{Agreement, DegradePolicy, Leg, ProtocolVersion, TransportKind};

    /// Every failure a test here can hit, so `?` works throughout.
    #[derive(Debug, thiserror::Error)]
    enum Failure {
        /// A test double's mutex was poisoned by an earlier panic.
        #[error("a test double's mutex was poisoned")]
        Poisoned,
        /// The bridge could not reach its upstream.
        #[error(transparent)]
        Bridge(#[from] BridgeError),
        /// A client's own loop failed.
        #[error(transparent)]
        Client(#[from] BridgeClientError),
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

    impl From<LoopError> for Failure {
        fn from(error: LoopError) -> Self {
            Self::Client(BridgeClientError::Loop(error))
        }
    }

    /// Hands out one peer per call, each recording into its own list.
    ///
    /// The recording is per peer and shared behind an `Arc`, so a test can read
    /// "which upstream saw this" after the loop has dropped the peer into the
    /// live [`Bridge`](crate::application::bridge::Bridge) — which it always
    /// does, and which is why a double that could only be inspected while
    /// owned would answer for nobody.
    #[derive(Default)]
    struct Factory {
        opened: AtomicUsize,
        seen: Arc<Mutex<Vec<Vec<String>>>>,
        leg: Option<Leg>,
    }

    impl Factory {
        fn offering(features: &[Feature]) -> Self {
            Self {
                opened: AtomicUsize::new(0),
                seen: Arc::new(Mutex::new(Vec::new())),
                leg: Some(*agreeing(features).upstream()),
            }
        }

        /// How many clients have been given an upstream.
        fn opened(&self) -> usize {
            self.opened.load(Ordering::SeqCst)
        }

        /// The methods the `n`th upstream was sent.
        fn methods(&self, nth: usize) -> Result<Vec<String>, Failure> {
            // Cloned so the guard is gone before the message is built — the
            // recording is a test's whole, small, and holding a lock across a
            // `format!` is the kind of thing that deadlocks the next run.
            let seen = self.seen.lock().map_err(|_| Failure::Poisoned)?.clone();
            let opened = seen.len();
            seen.into_iter().nth(nth).ok_or_else(|| {
                Failure::Claim(format!(
                    "no upstream was opened at index {nth}; {opened} were opened"
                ))
            })
        }
    }

    impl Upstream for Factory {
        /// Not `async`, because nothing here waits: the double hands back a
        /// peer rather than dialling one, and an `async fn` with no `.await`
        /// is a future that can never yield.
        fn open(&self) -> impl Future<Output = Result<OpenedUpstream, UpstreamError>> + Send {
            // The index is taken before the recording grows, so the two cannot
            // disagree about which peer is which when a client runs
            // concurrently.
            let nth = self.opened.fetch_add(1, Ordering::SeqCst);
            let Ok(mut seen) = self.seen.lock() else {
                return std::future::ready(Err(UpstreamError::Unreachable(
                    "the recording is poisoned".to_owned(),
                )));
            };
            seen.push(Vec::new());
            drop(seen);
            let Some(leg) = self.leg else {
                return std::future::ready(Err(UpstreamError::Unreachable(
                    "this factory was never given a leg".to_owned(),
                )));
            };
            std::future::ready(Ok(OpenedUpstream::new(
                leg,
                Arc::new(Numbered {
                    nth,
                    seen: Arc::clone(&self.seen),
                }),
            )))
        }
    }

    /// One peer, knowing which one it is.
    struct Numbered {
        nth: usize,
        seen: Arc<Mutex<Vec<Vec<String>>>>,
    }

    impl Numbered {
        /// Records what arrived, if this peer is still the one at `nth`.
        fn note(&self, method: &str) -> Result<(), BridgeError> {
            let Ok(mut seen) = self.seen.lock() else {
                return Err(BridgeError::Close("the recording is poisoned".to_owned()));
            };
            let Some(one) = seen.get_mut(self.nth) else {
                return Err(BridgeError::Close("this peer has no slot".to_owned()));
            };
            one.push(method.to_owned());
            Ok(())
        }
    }

    impl BridgePeer for Numbered {
        fn send(
            &self,
            envelope: Map<String, Value>,
        ) -> SendFuture<'_, Result<Map<String, Value>, BridgeError>> {
            Box::pin(async move {
                if let Some(method) = envelope.get("method").and_then(Value::as_str) {
                    self.note(method)?;
                }
                let id = envelope.get("id").cloned().unwrap_or(Value::Null);
                Ok(json!({"jsonrpc": "2.0", "id": id, "result": {"ok": true}})
                    .as_object()
                    .cloned()
                    .unwrap_or_default())
            })
        }

        fn notify(&self, envelope: Map<String, Value>) -> SendFuture<'_, Result<(), BridgeError>> {
            Box::pin(async move {
                if let Some(method) = envelope.get("method").and_then(Value::as_str) {
                    self.note(method)?;
                }
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
        fn of<S: AsRef<str>>(frames: &[S]) -> Self {
            let queue = frames
                .iter()
                .map(|frame| frame.as_ref().to_owned())
                .collect();
            Self {
                frames: Mutex::new(queue),
            }
        }
    }

    impl FrameSource for Scripted {
        fn next_frame(&self) -> impl Future<Output = Result<Option<String>, SourceError>> + Send {
            // Not `async`, and deliberately: nothing is awaited, and an `async`
            // body would hold the guard across a suspension point.
            let next = self
                .frames
                .lock()
                .map(|mut frames| frames.pop_front())
                .map_err(|_| SourceError::Transport);
            std::future::ready(next)
        }
    }

    /// Keeps every frame written, so a test can read the wire back.
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
    /// build's newest revision has none — SEP-2575 removed it — so an agreement
    /// there could never be settled with a client, and every test built on one
    /// would be exercising a path no client can take.
    fn agreeing(features: &[Feature]) -> Agreement {
        let version = ProtocolVersion::V2025_11_25;
        Agreement::between(
            Leg::new(version, serving(features)),
            Leg::new(version, serving(features)),
        )
    }

    /// A client that handshakes and then calls `tools/list`.
    fn client_frames() -> Vec<String> {
        vec![
            format!(
                r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"{}","capabilities":{{}},"clientInfo":{{"name":"scripted"}}}}}}"#,
                ProtocolVersion::V2025_11_25
            ),
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#.to_owned(),
        ]
    }

    async fn run_one(upstream: &Factory) -> Result<(Arc<Mutex<Vec<String>>>, LoopReport), Failure> {
        let recording: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let report = run_client(
            upstream,
            DegradePolicy::default(),
            DownstreamConfig::new("bridge", "0.1.0", TransportKind::Stdio)?,
            crate::application::bridge::content::doubles::Counting::reading_nothing(),
            Scripted::of(&client_frames()),
            Written {
                frames: Arc::clone(&recording),
            },
        )
        .await?;
        Ok((recording, report))
    }

    /// Two clients, two upstreams, neither sharing.
    ///
    /// The property the whole module exists for, and the one `stdio` could never
    /// test because one process is one client. Run sequentially so the two
    /// loops cannot interleave and muddy which peer saw what — the assertion is
    /// about attribution, not about concurrency.
    #[tokio::test]
    async fn two_clients_get_two_independent_upstreams() -> Result<(), Failure> {
        let upstream = Factory::offering(&[Feature::Tools]);

        let (_first_wire, first) = run_one(&upstream).await?;
        let (second_wire, second) = run_one(&upstream).await?;

        assert_eq!(
            upstream.opened(),
            2,
            "the upstream was not opened per client"
        );
        assert!(first.handshaken, "the first client never connected");
        assert!(second.handshaken, "the second client never connected");
        assert_eq!(
            first.forwarded, 1,
            "the first client's request did not reach its upstream"
        );
        assert_eq!(
            second.forwarded, 1,
            "the second client's request did not reach its upstream"
        );

        // The claim: each client reached a *different* upstream, and each saw
        // only its own request. A shared connection would put both clients'
        // calls in one peer's recording.
        assert_eq!(
            upstream.methods(0)?,
            vec!["tools/list".to_owned()],
            "the first upstream saw something other than its own client's call"
        );
        assert_eq!(
            upstream.methods(1)?,
            vec!["tools/list".to_owned()],
            "the second upstream saw something other than its own client's call"
        );

        // And each client got an answer, which a shared connection would also
        // have let through — so the recording above is what separates them.
        let wire = second_wire.lock().map_err(|_| Failure::Poisoned)?.clone();
        assert!(
            wire.len() >= 2,
            "the second client was not answered: the bridge dropped a reply rather than \
             forwarding it to the wrong peer"
        );
        Ok(())
    }

    /// An upstream that cannot be reached fails the client, not the process.
    ///
    /// A client whose server is unreachable has no bridge to be proxied
    /// through. Starting a loop anyway would report the failure as a broken
    /// client pipe, which is the opposite of what happened.
    #[tokio::test]
    async fn an_unreachable_upstream_is_reported_as_such() -> Result<(), Failure> {
        let outcome = run_client(
            &Unreachable,
            DegradePolicy::default(),
            DownstreamConfig::new("bridge", "0.1.0", TransportKind::Stdio)?,
            crate::application::bridge::content::doubles::Counting::reading_nothing(),
            Scripted::of(&client_frames()),
            Written::default(),
        )
        .await;

        assert!(
            matches!(outcome, Err(BridgeClientError::Upstream(_))),
            "an unreachable upstream was not reported as one: {outcome:?}"
        );
        Ok(())
    }

    /// The two doubles are the ones the trait promises to accept.
    ///
    /// A `Send`/`Sync` slip in either would not show up at the one call site a
    /// test happens to use, and would surface to the first caller trying to
    /// spawn the loop — so it is pinned here instead.
    #[test]
    fn the_doubles_are_the_bounds_the_traits_promise() {
        fn upstream<T: Send + Sync + Upstream + ?Sized>() {}
        fn source<T: DynFrameSource + ?Sized>() {}
        fn sink<T: DynFrameSink + ?Sized>() {}
        fn codec<T: ToolResultCodec + ?Sized>() {}

        upstream::<Factory>();
        upstream::<Unreachable>();
        source::<Scripted>();
        sink::<Written>();
        codec::<crate::application::bridge::content::doubles::Counting>();
    }

    /// An upstream that refuses, as a type rather than a local one.
    ///
    /// A local type inside a test body is fine to construct and awkward to name
    /// in a bound, and the bound is the claim worth making.
    struct Unreachable;

    impl Upstream for Unreachable {
        fn open(&self) -> impl Future<Output = Result<OpenedUpstream, UpstreamError>> + Send {
            std::future::ready(Err(UpstreamError::Unreachable(
                "connection refused".to_owned(),
            )))
        }
    }
}
