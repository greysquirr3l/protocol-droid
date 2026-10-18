//! Telling the sessions that a list changed — and the sessions that must not
//! be told.
//!
//! # The gate, and why it is the whole point
//!
//! `notifications/tools/list_changed` is a capability a server advertises and a
//! client may or may not have asked about. Sending one to a session that never
//! advertised [`Feature::ToolsListChanged`] is a message the peer has no
//! contract for, and a peer that handles unknown notifications strictly — which
//! is most of them — answers with an error for a notification that has no id,
//! which cannot be correlated, and which then sits in a queue until the
//! connection times out.
//!
//! So the fan-out is not "tell everyone what changed". It is "tell the
//! sessions whose negotiated profile already says they want to hear this, and
//! tell each of them once". Both halves are load-bearing and both have a test.
//!
//! # What a subscriber carries, and why the caller supplies the method
//!
//! A [`Subscriber`] holds a [`Feature`] and a method name, both supplied by the
//! caller rather than derived here. The registry's job is "a session that
//! registered for this feature and this kind gets this frame"; which frame that
//! is, and which feature gates it, is a fact about the protocol, and the
//! protocol is not this module's to name. A2A has no `list_changed`, and a
//! registry that hardcoded the MCP spelling would be a module that could not
//! serve it.
//!
//! # Why one notification per publish, not per session
//!
//! Each subscriber is sent at most once per publish, and only if the publish
//! touched the kind it registered for. A publish that adds a tool and removes a
//! resource sends a tool notification to tool subscribers and a resource
//! notification to resource subscribers, and sends neither to anyone twice.
//!
//! # Failures are counted, not propagated
//!
//! A sink that has failed is counted in [`Fanout::failed`] and does not stop
//! the remaining subscribers from being told. One dead connection must not
//! silence the `list_changed` for every other live session — the count is
//! returned so the caller can drop the subscriber, and the fan-out carries on.

use std::sync::Arc;

use arc_swap::ArcSwap;

use crate::application::session::{DynFrameSink, SinkError};
use crate::domain::registry::{PrimitiveKind, Snapshot};
use crate::domain::{Feature, FeatureProfile};

/// One session's standing interest in being told that a list changed.
///
/// Built once, when the session registers, and read on every publish after
/// that. Holding the [`FeatureProfile`] by value rather than by reference is
/// deliberate: it is a `u128` bitset that `FeatureProfile` derives `Copy` for,
/// and a subscriber that outlives its session must not borrow one.
#[derive(Clone)]
pub struct Subscriber {
    kind: PrimitiveKind,
    /// A second kind announced under the same notification.
    ///
    /// `resources/list_changed` covers both resources and resource templates,
    /// and two subscribers on one sink would tell a session twice about a batch
    /// that touched both — the promise this module makes is once per publish.
    also: Option<PrimitiveKind>,
    feature: Feature,
    method: &'static str,
    frame: Arc<str>,
    profile: FeatureProfile,
    sink: Arc<dyn DynFrameSink>,
}

impl Subscriber {
    /// A subscriber for `kind`, gated on `feature`, told with `method`.
    ///
    /// `frame` is the encoded notification. It is built once here rather than
    /// per publish because the bytes are identical for every session receiving
    /// it, and encoding a notification per subscriber turns one publish into
    /// one serialisation per session — which is the difference between a
    /// fan-out that costs a socket write and one that costs a JSON encode.
    #[must_use]
    pub fn new(
        kind: PrimitiveKind,
        feature: Feature,
        method: &'static str,
        frame: &str,
        profile: FeatureProfile,
        sink: Arc<dyn DynFrameSink>,
    ) -> Self {
        Self {
            kind,
            also: None,
            feature,
            method,
            frame: Arc::from(frame),
            profile,
            sink,
        }
    }

    /// The same subscriber, also told when `kind` changes.
    ///
    /// For a notification that names more than one kind — see the field's own
    /// comment for the one MCP has.
    #[must_use]
    pub const fn also_for(mut self, kind: PrimitiveKind) -> Self {
        self.also = Some(kind);
        self
    }

    /// The kind of primitive this subscriber wants to hear about.
    #[must_use]
    pub const fn kind(&self) -> PrimitiveKind {
        self.kind
    }

    /// Whether a publish that touched `kinds` concerns this subscriber.
    #[must_use]
    pub fn covers(&self, kinds: &[PrimitiveKind]) -> bool {
        kinds
            .iter()
            .any(|kind| *kind == self.kind || Some(*kind) == self.also)
    }

    /// The method this subscriber is notified with.
    #[must_use]
    pub const fn method(&self) -> &'static str {
        self.method
    }

    /// Whether this subscriber's negotiated profile permits the notification.
    ///
    /// The one place the gate is decided, so there is no second caller that
    /// forgets to ask.
    #[must_use]
    pub fn is_eligible(&self) -> bool {
        self.profile.allows(self.feature)
    }
}

impl core::fmt::Debug for Subscriber {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // The sink is a trait object and the frame is a wire document; neither
        // is worth a multi-line dump in a log line about a registration.
        f.debug_struct("Subscriber")
            .field("kind", &self.kind)
            .field("feature", &self.feature)
            .field("method", &self.method)
            .field("profile", &self.profile)
            .field("eligible", &self.is_eligible())
            .finish_non_exhaustive()
    }
}

/// What one fan-out did.
#[derive(Debug, Default)]
pub struct Fanout {
    /// Subscribers that were told.
    pub sent: usize,
    /// Subscribers skipped because their profile did not permit it.
    pub gated: usize,
    /// Subscribers whose sink failed. Each is counted, not described: the
    /// session id is deliberately not carried here, because a report that
    /// named sessions would be a report that logs them.
    pub failed: usize,
}

impl Fanout {
    /// Whether anyone was actually told.
    #[must_use]
    pub const fn is_silent(&self) -> bool {
        self.sent == 0
    }
}

/// The set of sessions currently listening.
///
/// Reads are lock-free: the subscriber list is behind an [`ArcSwap`] for the
/// same reason the primitive set is, and a publish walks a `Vec` of `Arc`s
/// that no writer can mutate.
#[derive(Default)]
pub struct SubscriberSet {
    entries: ArcSwap<Vec<Arc<Subscriber>>>,
}

impl SubscriberSet {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: ArcSwap::from_pointee(Vec::new()),
        }
    }

    /// The current subscribers, as one owned snapshot.
    #[must_use]
    pub fn all(&self) -> Vec<Arc<Subscriber>> {
        self.entries.load_full().as_ref().clone()
    }

    /// How many sessions are listening.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.load().len()
    }

    /// Whether anyone is listening.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.load().is_empty()
    }

    /// Adds a session's standing interest.
    pub fn add(&self, subscriber: Subscriber) {
        let mut next: Vec<Arc<Subscriber>> = self.all();
        next.push(Arc::new(subscriber));
        self.entries.store(Arc::new(next));
    }

    /// Drops every subscriber whose sink is `sink`, by identity.
    ///
    /// By pointer identity rather than by session id because a session id is
    /// the thing this module is not allowed to log, and comparing two of them
    /// to decide who to remove would be a use of one anyway. The sink is the
    /// connection, and a connection is what a disconnect closes.
    ///
    /// Returns how many were dropped, so a caller can assert that a
    /// disconnect actually unsubscribed rather than assuming it.
    pub fn remove_by_sink(&self, sink: &Arc<dyn DynFrameSink>) -> usize {
        let current = self.all();
        let next: Vec<Arc<Subscriber>> = current
            .iter()
            .filter(|entry| !Arc::ptr_eq(&entry.sink, sink))
            .map(Arc::clone)
            .collect();
        let removed = current.len() - next.len();
        if removed > 0 {
            self.entries.store(Arc::new(next));
        }
        removed
    }

    /// Tells the eligible subscribers that `kinds` changed in `snapshot`.
    ///
    /// A subscriber is told when **both** hold: it registered for a kind this
    /// publish touched, and its own negotiated profile permits the
    /// notification. The kind test comes from the publish; the profile test
    /// comes from the subscriber. Neither can be derived from the other, which
    /// is why both are here.
    ///
    /// The subscriber list is loaded once, so a subscriber that disconnects
    /// mid-fan-out is either told or not told but never told twice, and a
    /// subscriber that joins mid-fan-out is not told about a change it could
    /// not have observed.
    pub async fn announce(&self, snapshot: &Snapshot, kinds: &[PrimitiveKind]) -> Fanout {
        let mut report = Fanout::default();
        if kinds.is_empty() {
            return report;
        }
        let generation = snapshot.generation();
        for entry in self.all() {
            if !entry.covers(kinds) {
                continue;
            }
            if !entry.is_eligible() {
                report.gated += 1;
                continue;
            }
            match entry.sink.send(&entry.frame).await {
                Ok(()) => report.sent += 1,
                Err(SinkError) => report.failed += 1,
            }
        }
        // The generation is read so a caller can log which publish this
        // fan-out belonged to. A fan-out that cannot say which generation it
        // announced is a fan-out that cannot be reasoned about after the fact.
        debug_assert!(
            generation > 0 || snapshot.is_blank(),
            "a published snapshot is numbered"
        );
        report
    }
}

impl core::fmt::Debug for SubscriberSet {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SubscriberSet")
            .field("len", &self.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{Subscriber, SubscriberSet};
    use crate::application::session::{DynFrameSink, FrameSink, SinkError};
    use crate::domain::registry::PrimitiveKind;
    use crate::domain::registry::TestError;
    use crate::domain::{Feature, FeatureProfile};

    const FRAME: &str = r#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#;

    #[derive(Default)]
    struct Counter(AtomicUsize);

    impl FrameSink for Counter {
        #[expect(
            clippy::unused_async_trait_impl,
            reason = "FrameSink is a native async fn trait; the signature is not ours to change"
        )]
        async fn send(&self, _frame: &str) -> Result<(), SinkError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    fn watch(profile: FeatureProfile, sink: &Arc<Counter>) -> Subscriber {
        Subscriber::new(
            PrimitiveKind::Tool,
            Feature::ToolsListChanged,
            "notifications/tools/list_changed",
            FRAME,
            profile,
            Arc::clone(sink) as Arc<dyn DynFrameSink>,
        )
    }

    fn snapshot() -> crate::domain::registry::Snapshot {
        crate::domain::registry::Snapshot::empty()
    }

    /// Eligibility is decided by the subscriber's own profile and nowhere else,
    /// so a fan-out cannot be written that forgets to ask.
    #[test]
    fn eligibility_is_the_negotiated_profile() {
        let sink = Arc::new(Counter::default());
        let opted_in = watch(
            FeatureProfile::empty().with(Feature::ToolsListChanged),
            &sink,
        );
        let opted_out = watch(FeatureProfile::empty().with(Feature::Tools), &sink);

        assert!(opted_in.is_eligible());
        assert!(!opted_out.is_eligible());
    }

    /// A subscriber remembers what it was told to send. Nothing recomputes the
    /// method at send time, because a fan-out that built a notification per
    /// subscriber would pay a serialisation per session.
    #[test]
    fn a_subscriber_carries_its_method() {
        let sink = Arc::new(Counter::default());
        let subscriber = watch(
            FeatureProfile::empty().with(Feature::ToolsListChanged),
            &sink,
        );
        assert_eq!(subscriber.kind(), PrimitiveKind::Tool);
        assert_eq!(subscriber.method(), "notifications/tools/list_changed");
    }

    /// Nothing to announce means nothing sent, and the counters say which of
    /// the three paths it took.
    #[tokio::test]
    async fn an_empty_fan_out_reports_silence() {
        let set = SubscriberSet::new();
        let report = set.announce(&snapshot(), &[]).await;
        assert!(report.is_silent());
        assert_eq!(report.sent, 0);
        assert_eq!(report.gated, 0);
        assert_eq!(report.failed, 0);
    }

    /// A subscriber is told once per announce, and a gated one is counted as
    /// gated rather than silently skipped.
    #[tokio::test]
    async fn a_gated_subscriber_is_counted_not_skipped() {
        let set = SubscriberSet::new();
        let sink = Arc::new(Counter::default());
        set.add(watch(FeatureProfile::empty().with(Feature::Tools), &sink));

        let report = set.announce(&snapshot(), &[PrimitiveKind::Tool]).await;
        assert_eq!(report.gated, 1);
        assert_eq!(report.sent, 0);
        assert_eq!(sink.0.load(Ordering::Relaxed), 0);
    }

    /// A kind nobody subscribed to reaches nobody.
    #[tokio::test]
    async fn an_unwatched_kind_reaches_nobody() {
        let set = SubscriberSet::new();
        let sink = Arc::new(Counter::default());
        set.add(watch(
            FeatureProfile::empty().with(Feature::ToolsListChanged),
            &sink,
        ));

        let report = set.announce(&snapshot(), &[PrimitiveKind::Resource]).await;
        assert!(report.is_silent());
        assert_eq!(sink.0.load(Ordering::Relaxed), 0);
    }

    /// Dropping a subscriber is by sink identity, and reports how many went —
    /// a disconnect has to be able to assert that it actually unsubscribed.
    #[tokio::test]
    async fn a_disconnect_removes_exactly_its_own_subscriber() -> Result<(), TestError> {
        let set = SubscriberSet::new();
        let staying = Arc::new(Counter::default());
        let leaving = Arc::new(Counter::default());
        let profile = FeatureProfile::empty().with(Feature::ToolsListChanged);
        set.add(watch(profile, &staying));
        set.add(watch(profile, &leaving));
        assert_eq!(set.len(), 2);

        let removed = set.remove_by_sink(&(Arc::clone(&leaving) as Arc<dyn DynFrameSink>));
        assert_eq!(removed, 1);
        assert_eq!(set.len(), 1);

        set.announce(&snapshot(), &[PrimitiveKind::Tool]).await;
        assert_eq!(staying.0.load(Ordering::Relaxed), 1);
        assert_eq!(leaving.0.load(Ordering::Relaxed), 0);
        Ok(())
    }

    /// Removing a sink nobody registered removes nothing, and says so rather
    /// than reporting a spurious success.
    #[test]
    fn removing_an_unknown_sink_removes_nothing() {
        let set = SubscriberSet::new();
        let unknown: Arc<dyn DynFrameSink> = Arc::new(Counter::default());
        assert_eq!(set.remove_by_sink(&unknown), 0);
        assert!(set.is_empty());
    }
}
