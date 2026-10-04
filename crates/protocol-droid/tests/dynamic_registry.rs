//! The two scenarios `capabilities/dynamic-registry.md` and
//! `capabilities/feature-gating.md` name, driven through the real types.
//!
//! These are integration tests rather than unit tests on purpose. The
//! interesting claims are about what a *session* observes while a *publish* is
//! happening, and neither the store nor the fan-out knows what a session is —
//! that only exists where a store, a subscriber set and a visibility rule meet.
//! A unit test on any one of them would be green for a registry that got the
//! interaction wrong.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

use protocol_droid::application::registry::{RegistryStore, Subscriber, SubscriberSet};
use protocol_droid::application::session::{DynFrameSink, FrameSink, SinkError};
use protocol_droid::domain::registry::{AuthContext, Primitive, PrimitiveKind, Registered, Tool};
use protocol_droid::domain::{Feature, FeatureProfile};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The wire notification a tool publish emits. MCP's spelling, supplied here
/// rather than built into the registry — the registry was told what to send by
/// this test, which is the whole of the protocol knowledge it has.
const TOOLS_CHANGED: &str = r#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#;
const RESOURCES_CHANGED: &str =
    r#"{"jsonrpc":"2.0","method":"notifications/resources/list_changed"}"#;

/// A sink that counts what it was given and remembers the text.
///
/// Counts rather than bools because "exactly one" is the claim several of these
/// tests turn on, and a bool cannot tell zero from two.
#[derive(Default)]
struct Recorder {
    frames: Mutex<Vec<String>>,
    fail: bool,
}

impl Recorder {
    fn failing() -> Arc<Self> {
        Arc::new(Self {
            frames: Mutex::new(Vec::new()),
            fail: true,
        })
    }

    fn count(&self) -> usize {
        self.frames
            .lock()
            .map_or_else(|poisoned| poisoned.into_inner().len(), |held| held.len())
    }

    fn all(&self) -> Vec<String> {
        self.frames.lock().map_or_else(
            |poisoned| poisoned.into_inner().clone(),
            |held| held.clone(),
        )
    }
}

impl FrameSink for Recorder {
    // The trait declares this `async`, so the impl has to be too even where
    // this particular recorder never suspends.
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "FrameSink is a native async fn trait; the signature is not ours to change"
    )]
    async fn send(&self, frame: &str) -> Result<(), SinkError> {
        if self.fail {
            return Err(SinkError);
        }
        {
            let mut held = self.frames.lock().unwrap_or_else(PoisonError::into_inner);
            held.push(frame.to_owned());
        }
        Ok(())
    }
}

/// A subscriber for tool changes, gated on `tools.listChanged`.
fn tool_watch(profile: FeatureProfile, recorder: Arc<Recorder>) -> Subscriber {
    Subscriber::new(
        PrimitiveKind::Tool,
        Feature::ToolsListChanged,
        "notifications/tools/list_changed",
        TOOLS_CHANGED,
        profile,
        recorder as Arc<dyn DynFrameSink>,
    )
}

fn tool(name: &str) -> Result<Registered, Box<dyn std::error::Error>> {
    Ok(Registered::from(Primitive::from(Tool::new(
        name,
        r#"{"type":"object"}"#,
    )?)))
}

/// A profile that negotiated `tools.listChanged`, and one that did not.
fn with_list_changed() -> FeatureProfile {
    FeatureProfile::empty().with(Feature::ToolsListChanged)
}

fn without_list_changed() -> FeatureProfile {
    FeatureProfile::empty().with(Feature::Tools)
}

// --------------------------------------------------------------------------
// hot-add-tool
// --------------------------------------------------------------------------

/// The scenario from `capabilities/dynamic-registry.md`, all three clauses at
/// once: in-flight calls complete against the old snapshot, the next
/// `tools/list` includes the new tool, and eligible sessions get exactly one
/// notification.
#[tokio::test]
async fn hot_add_tool_leaves_in_flight_calls_on_the_old_snapshot() -> TestResult {
    let store = RegistryStore::new();
    store.register(tool("before")?);

    let profile = with_list_changed();
    let alice = AuthContext::unrestricted("alice");
    let bob = AuthContext::unrestricted("bob");
    let carol = AuthContext::unrestricted("carol");

    // Three sessions are mid-call: each has resolved a tool and holds the
    // snapshot it resolved against. This is the `Arc` the store is about to
    // replace out from under them.
    let in_flight = [
        store.list_visible(&alice, &profile, PrimitiveKind::Tool),
        store.list_visible(&bob, &profile, PrimitiveKind::Tool),
        store.list_visible(&carol, &profile, PrimitiveKind::Tool),
    ];
    let resolved = in_flight
        .iter()
        .filter(|listing| !listing.visible().is_empty())
        .count();
    assert_eq!(
        resolved, 3,
        "each session should have resolved a tool before the publish"
    );

    // The publish lands while all three are still holding their snapshot.
    let published = store.register(tool("after")?);
    assert_eq!(published.changed(), [PrimitiveKind::Tool]);

    // Clause one: the in-flight calls finish against what they started with.
    for listing in &in_flight {
        assert_eq!(
            listing.visible().len(),
            1,
            "an in-flight listing must not grow when a tool is published"
        );
        assert!(
            listing
                .visible()
                .iter()
                .all(|held| held.id().as_str() == "before"),
            "an in-flight call must complete against the tool it resolved"
        );
    }

    // Clause two: the next list sees it.
    let next = store.list_visible(&alice, &profile, PrimitiveKind::Tool);
    let names: Vec<&str> = next
        .visible()
        .iter()
        .map(|held| held.id().as_str())
        .collect();
    assert_eq!(
        names,
        vec!["before", "after"],
        "the next list must include the new tool"
    );

    Ok(())
}

/// The "exactly one" clause on its own, so a failure names the count rather
/// than the shape.
#[tokio::test]
async fn one_publish_notifies_each_eligible_session_exactly_once() -> TestResult {
    let store = RegistryStore::new();
    let subscribers = SubscriberSet::new();
    let profile = with_list_changed();

    let alice = Arc::new(Recorder::default());
    let bob = Arc::new(Recorder::default());
    subscribers.add(tool_watch(profile, Arc::clone(&alice)));
    subscribers.add(tool_watch(profile, Arc::clone(&bob)));

    let published = store.register(tool("added")?);
    let report = subscribers
        .announce(published.snapshot(), published.changed())
        .await;

    assert_eq!(report.sent, 2, "both sessions negotiated listChanged");
    assert_eq!(report.gated, 0);
    assert_eq!(report.failed, 0);
    assert_eq!(alice.count(), 1, "alice must be told exactly once");
    assert_eq!(bob.count(), 1, "bob must be told exactly once");
    assert_eq!(alice.all(), vec![TOOLS_CHANGED.to_owned()]);

    Ok(())
}

/// A second publish is a second notification. A store that latched after the
/// first would pass the test above and be wrong here.
#[tokio::test]
async fn each_publish_notifies_again() -> TestResult {
    let store = RegistryStore::new();
    let subscribers = SubscriberSet::new();
    let recorder = Arc::new(Recorder::default());
    subscribers.add(tool_watch(with_list_changed(), Arc::clone(&recorder)));

    for name in ["a", "b", "c"] {
        let published = store.register(tool(name)?);
        subscribers
            .announce(published.snapshot(), published.changed())
            .await;
    }

    assert_eq!(recorder.count(), 3, "three publishes, three notifications");
    Ok(())
}

// --------------------------------------------------------------------------
// list-changed-respects-capability
// --------------------------------------------------------------------------

/// The scenario from `capabilities/feature-gating.md`: a tool registered at
/// runtime on a session whose capabilities did not advertise
/// `tools.listChanged` gets no notification, while the session that did gets
/// one from the same publish and the same fan-out.
#[tokio::test]
async fn list_changed_respects_the_negotiated_capability() -> TestResult {
    let store = RegistryStore::new();
    let subscribers = SubscriberSet::new();

    let opted_in = Arc::new(Recorder::default());
    let opted_out = Arc::new(Recorder::default());
    subscribers.add(tool_watch(with_list_changed(), Arc::clone(&opted_in)));
    subscribers.add(tool_watch(without_list_changed(), Arc::clone(&opted_out)));

    let published = store.register(tool("added")?);
    let report = subscribers
        .announce(published.snapshot(), published.changed())
        .await;

    assert_eq!(report.sent, 1, "only the session that negotiated it");
    assert_eq!(
        report.gated, 1,
        "the other was gated, not skipped by accident"
    );
    assert_eq!(opted_in.count(), 1);
    assert_eq!(
        opted_out.count(),
        0,
        "a session that never advertised tools.listChanged must not be sent one"
    );

    Ok(())
}

/// The gate is on the notification, not on the list. A registry that hid the
/// tool would be fixing a different problem, and one this test cannot see.
#[test]
fn a_gated_session_still_sees_the_tool_in_its_listing() -> TestResult {
    let store = RegistryStore::new();
    store.register(tool("added")?);

    let session = AuthContext::unrestricted("quiet");
    let listing = store.list_visible(&session, &without_list_changed(), PrimitiveKind::Tool);
    assert_eq!(listing.visible().len(), 1);
    assert_eq!(listing.total(), 1);

    Ok(())
}

/// A publish that touched nothing announces nothing.
#[tokio::test]
async fn a_publish_that_changed_nothing_sends_nothing() -> TestResult {
    let store = RegistryStore::new();
    let subscribers = SubscriberSet::new();
    let recorder = Arc::new(Recorder::default());
    subscribers.add(tool_watch(with_list_changed(), Arc::clone(&recorder)));

    let published = store.unregister(PrimitiveKind::Tool, &"never-registered".into());
    assert!(published.is_noop(), "removing an absent tool is a no-op");
    let report = subscribers
        .announce(published.snapshot(), published.changed())
        .await;

    assert!(report.is_silent());
    assert_eq!(recorder.count(), 0);
    Ok(())
}

/// A subscriber of one kind is not told about another's change.
#[tokio::test]
async fn a_tool_publish_does_not_reach_a_resource_subscriber() -> TestResult {
    let store = RegistryStore::new();
    let subscribers = SubscriberSet::new();

    let watching_tools = Arc::new(Recorder::default());
    let watching_resources = Arc::new(Recorder::default());
    subscribers.add(tool_watch(with_list_changed(), Arc::clone(&watching_tools)));
    subscribers.add(Subscriber::new(
        PrimitiveKind::Resource,
        Feature::ResourcesListChanged,
        "notifications/resources/list_changed",
        RESOURCES_CHANGED,
        with_list_changed(),
        Arc::clone(&watching_resources) as Arc<dyn DynFrameSink>,
    ));

    let published = store.register(tool("added")?);
    subscribers
        .announce(published.snapshot(), published.changed())
        .await;

    assert_eq!(watching_tools.count(), 1);
    assert_eq!(watching_resources.count(), 0);
    Ok(())
}

// --------------------------------------------------------------------------
// failure isolation
// --------------------------------------------------------------------------

/// One dead connection must not silence every other session.
#[tokio::test]
async fn a_failing_sink_does_not_stop_the_fan_out() -> TestResult {
    let store = RegistryStore::new();
    let subscribers = SubscriberSet::new();

    let dead = Recorder::failing();
    let alive = Arc::new(Recorder::default());
    subscribers.add(tool_watch(with_list_changed(), Arc::clone(&dead)));
    subscribers.add(tool_watch(with_list_changed(), Arc::clone(&alive)));

    let published = store.register(tool("added")?);
    let report = subscribers
        .announce(published.snapshot(), published.changed())
        .await;

    assert_eq!(report.sent, 1);
    assert_eq!(report.failed, 1);
    assert_eq!(alive.count(), 1, "the live session is still told");
    Ok(())
}

// --------------------------------------------------------------------------
// the generation
// --------------------------------------------------------------------------

/// The generation advances on every publish and is carried by the value, so a
/// session can compare one `u64` instead of diffing two sets.
#[test]
fn the_generation_advances_with_every_publish() -> TestResult {
    let store = RegistryStore::new();
    assert_eq!(store.generation(), 0);

    let first = store.register(tool("a")?);
    assert_eq!(first.generation(), 1);
    assert_eq!(store.generation(), 1);

    // A replace is one publish, not a removal and an add.
    let replaced = store.register(tool("a")?);
    assert_eq!(replaced.generation(), 2, "a replace is one generation");
    assert_eq!(store.snapshot().len(PrimitiveKind::Tool), 1);

    // Even a no-op advances it, so a waiter can learn that the answer is
    // "nothing changed" rather than waiting forever.
    let noop = store.unregister(PrimitiveKind::Tool, &"absent".into());
    assert!(noop.is_noop());
    assert_eq!(noop.generation(), 3);

    Ok(())
}

/// A batch is one generation, so no reader can see half of it.
#[test]
fn a_batch_is_one_generation() -> TestResult {
    let store = RegistryStore::new();
    let published = store.apply([tool("a")?, tool("b")?, tool("c")?]);

    assert_eq!(published.generation(), 1, "three tools, one generation");
    assert_eq!(published.changed(), [PrimitiveKind::Tool]);
    assert_eq!(store.snapshot().len(PrimitiveKind::Tool), 3);
    Ok(())
}

// --------------------------------------------------------------------------
// visibility
// --------------------------------------------------------------------------

/// A tool that needs a feature the session lacks is hidden from its listing,
/// and the count still reports what exists.
#[test]
fn a_tool_needing_an_absent_feature_is_hidden() -> TestResult {
    let store = RegistryStore::new();
    let structured: Primitive = Tool::new("structured", r#"{"type":"object"}"#)?
        .with_output_schema(r#"{"type":"object"}"#)?
        .into();
    store.register(structured);
    store.register(tool("plain")?);

    let session = AuthContext::unrestricted("alice");

    let without = store.list_visible(
        &session,
        &FeatureProfile::empty().with(Feature::Tools),
        PrimitiveKind::Tool,
    );
    assert_eq!(without.visible().len(), 1, "the structured tool is hidden");
    assert_eq!(without.total(), 2, "but the server still holds both");
    assert_eq!(without.hidden(), 1);

    let with = store.list_visible(
        &session,
        &FeatureProfile::empty()
            .with(Feature::Tools)
            .with(Feature::ToolStructuredOutput),
        PrimitiveKind::Tool,
    );
    assert_eq!(
        with.visible().len(),
        2,
        "with the feature, both are visible"
    );

    Ok(())
}

/// A session without the required scope is refused, and the miss is
/// indistinguishable from an absent primitive.
#[test]
fn an_unauthorised_principal_gets_a_miss_not_a_refusal() -> TestResult {
    let store = RegistryStore::new();
    let guarded = Registered::from(Primitive::from(Tool::new(
        "guarded",
        r#"{"type":"object"}"#,
    )?))
    .requiring_scope(protocol_droid::domain::registry::Scope::Admin);
    store.register(guarded);

    let plain = AuthContext::new(
        "mallory",
        protocol_droid::domain::registry::ScopeSet::empty(),
    );
    let admin = AuthContext::unrestricted("root");

    let miss = store.get_visible(&plain, &with_list_changed(), PrimitiveKind::Tool, "guarded");
    assert!(miss.is_none(), "a hidden tool is a miss, not an error");

    let absent = store.get_visible(
        &plain,
        &with_list_changed(),
        PrimitiveKind::Tool,
        "no-such-tool",
    );
    assert_eq!(
        miss.is_none(),
        absent.is_none(),
        "hidden and absent must be indistinguishable, or the tool list leaks"
    );
    assert!(
        store
            .get_visible(&admin, &with_list_changed(), PrimitiveKind::Tool, "guarded")
            .is_some()
    );

    Ok(())
}

// --------------------------------------------------------------------------
// concurrency
// --------------------------------------------------------------------------

/// Concurrent publish and read over the real type.
///
/// loom is not in the dependency graph because arc-swap carries no `loom`
/// instrumentation — checked in its own source, not assumed — so a loom model
/// here would exercise our code and treat the swap itself as an opaque external
/// call, which is the one part worth checking. This exercises both halves on
/// real threads instead, and asserts what the model would have: a reader never
/// sees a set whose listing and index disagree.
#[test]
fn a_reader_never_sees_a_torn_snapshot() -> TestResult {
    const WRITES: usize = 4;
    const READERS: usize = 4;
    const ROUNDS: usize = 250;

    let store = Arc::new(RegistryStore::new());
    let torn = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));

    let mut writers = Vec::new();
    for writer in 0..WRITES {
        let store = Arc::clone(&store);
        writers.push(std::thread::spawn(move || {
            for round in 0..ROUNDS {
                let name = format!("tool-{writer}-{round}");
                let parsed = Tool::new(name.as_str(), r#"{"type":"object"}"#)?;
                store.register(Primitive::from(parsed));
            }
            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
        }));
    }

    let mut readers = Vec::new();
    for _ in 0..READERS {
        let store = Arc::clone(&store);
        let torn = Arc::clone(&torn);
        let stop = Arc::clone(&stop);
        readers.push(std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let snapshot = store.snapshot();
                // Every id in the listing order must also resolve in the index.
                // A torn read would show an id in one and not the other.
                for primitive in snapshot.list(PrimitiveKind::Tool) {
                    let id = primitive.id().clone();
                    if snapshot.get(PrimitiveKind::Tool, &id).is_none() {
                        torn.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }));
    }

    for writer in writers {
        writer
            .join()
            .map_err(|_| "a writer panicked")?
            .map_err(|error| error.to_string())?;
    }
    stop.store(true, Ordering::Relaxed);
    for reader in readers {
        reader.join().map_err(|_| "a reader panicked")?;
    }

    assert_eq!(
        torn.load(Ordering::Relaxed),
        0,
        "a reader observed a snapshot whose listing and index disagreed"
    );
    assert_eq!(store.snapshot().len(PrimitiveKind::Tool), WRITES * ROUNDS);
    Ok(())
}
