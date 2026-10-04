//! The publish side: the one place a snapshot can change.
//!
//! # What this file is for
//!
//! [`crate::domain::registry::Snapshot`] knows how to describe a set of
//! primitives and refuses to be edited. This module is what a server calls when
//! a tool is registered, and its whole job is the sequence *copy, change,
//! swap*. The copy is shallow — every primitive is behind an `Arc` — so a
//! thousand unchanged tools cost a thousand pointer copies rather than a
//! thousand clones of the tools.
//!
//! # Why the swap is the transaction boundary
//!
//! A reader that held `&Snapshot` could not survive a publish at all, and a
//! reader that held a `Guard` would pin the old snapshot against reclamation
//! for as long as it lives. A `tools/call` that resolves a tool and then runs
//! it — which may block on a network call, a sandbox, or a human — would hold
//! the registry's old state against a publish for that whole duration. So the
//! reader takes an owned `Arc<Snapshot>`, the writer replaces the `Arc`, and
//! the two never share a borrow.
//!
//! The consequence worth stating: **an in-flight call runs against the
//! snapshot it started with.** A tool removed a microsecond after a call
//! resolved still completes. That is not a best-effort approximation, it is
//! the only behaviour that does not make a publish block on a running tool.
//!
//! # The generation
//!
//! Every snapshot carries the number of the publish that produced it, and
//! [`RegistryStore::generation`] reads it off the current value rather than
//! keeping a counter beside it — a counter that could drift from what it
//! describes. A session that was told about generation `N` can compare one
//! `u64` to learn whether anything changed, which is what makes the fan-out in
//! [`super::subscriber`] able to skip a notification it has already sent.

use std::cell::Cell;
use std::sync::Arc;

use arc_swap::ArcSwap;

use crate::domain::registry::{PrimitiveId, PrimitiveKind, Registered, Snapshot, SnapshotBuilder};

/// What a publish did, as the caller needs to hear it.
///
/// The returned snapshot is the one now published. Handing it back means the
/// caller can act on exactly the value readers will see, rather than
/// re-loading and racing a second publish.
#[derive(Debug)]
pub struct Published {
    /// The snapshot now live, owned so the caller can read it without a load.
    snapshot: Arc<Snapshot>,
    /// The kinds this publish touched, in declaration order.
    changed: Vec<PrimitiveKind>,
}

impl Published {
    /// The snapshot that is now published.
    #[must_use]
    pub const fn snapshot(&self) -> &Arc<Snapshot> {
        &self.snapshot
    }

    /// The number of this publish.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.snapshot.generation()
    }

    /// The kinds this publish touched.
    ///
    /// A publish that removes a tool that was not there touches nothing and
    /// reports nothing, so a caller that drives notifications off this list
    /// does not announce a change nobody made.
    #[must_use]
    pub fn changed(&self) -> &[PrimitiveKind] {
        &self.changed
    }

    /// Whether anything changed at all.
    #[must_use]
    pub const fn is_noop(&self) -> bool {
        self.changed.is_empty()
    }
}

/// The live set, and the only way to change it.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::registry::{Primitive, PrimitiveKind, Tool};
/// use protocol_droid::application::registry::RegistryStore;
///
/// let store = RegistryStore::new();
/// assert!(store.snapshot().is_blank());
///
/// let published = store.register(Primitive::from(
///     Tool::new("echo", r#"{"type":"object"}"#)?,
/// ));
///
/// assert_eq!(published.changed(), [PrimitiveKind::Tool]);
/// assert_eq!(store.snapshot().len(PrimitiveKind::Tool), 1);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct RegistryStore {
    live: ArcSwap<Snapshot>,
}

impl RegistryStore {
    /// An empty store at generation zero.
    #[must_use]
    pub fn new() -> Self {
        Self {
            live: ArcSwap::from_pointee(Snapshot::empty()),
        }
    }

    /// A store seeded with a snapshot, at that snapshot's generation.
    #[must_use]
    pub fn with(snapshot: Snapshot) -> Self {
        Self {
            live: ArcSwap::from_pointee(snapshot),
        }
    }

    /// The current snapshot, owned.
    ///
    /// An `Arc` clone, not a borrow. The caller may hold it for as long as it
    /// likes — across a publish, across a long-running tool — and no publish
    /// waits on it.
    #[must_use]
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.live.load_full()
    }

    /// The current publish number.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.live.load().generation()
    }

    /// Adds a primitive, replacing any primitive already filed under its id.
    ///
    /// A replacement is one publish and one generation, not a removal followed
    /// by an add: a subscriber that hears about it twice has been told the tool
    /// went away and came back, which is a lie that costs a re-list.
    ///
    /// The primitive is captured behind an `Arc` because [`Self::publish`] may
    /// run its closure more than once, and a retry must be able to install the
    /// *same* primitive without taking the first attempt's copy of it. The
    /// alternative is a clone per attempt, which on a contended registry means
    /// deep-copying a tool's schema on every retry.
    pub fn register(&self, primitive: impl Into<Registered>) -> Published {
        let primitive = Arc::new(primitive.into());
        let kind = primitive.kind();
        let snapshot = self.publish(move |mut next| {
            next.insert_arc(Arc::clone(&primitive));
            next
        });
        Published {
            snapshot,
            changed: vec![kind],
        }
    }

    /// Takes a primitive out of the set.
    ///
    /// Removing something that is not there is not an error: the observable
    /// state afterwards is the same either way, and a caller tearing down a
    /// tool it may never have registered should not have to check first. The
    /// returned [`Published`] reports `is_noop`, so a notification is still
    /// suppressed.
    pub fn unregister(&self, kind: PrimitiveKind, id: &PrimitiveId) -> Published {
        // `rcu` may run the closure more than once, so "was anything removed"
        // has to be read from the attempt that actually won rather than
        // captured by the first one to run.
        let removed = Cell::new(false);
        let id = id.clone();
        let snapshot = self.publish({
            let removed = &removed;
            move |mut next| {
                removed.set(next.remove(kind, &id).is_some());
                next
            }
        });
        let changed = if removed.get() {
            vec![kind]
        } else {
            Vec::new()
        };
        Published { snapshot, changed }
    }

    /// Publishes several changes as one generation.
    ///
    /// A batch is one publish rather than a sequence of them so that a
    /// subscriber told about the result never observes a half-applied set.
    /// That is the only reason this takes a slice: the alternative is a caller
    /// looping over [`Self::register`], and a reader racing the loop sees
    /// generation `N` with the first change and `N+2` with the last.
    pub fn apply(&self, primitives: impl IntoIterator<Item = Registered>) -> Published {
        let batch: Vec<Arc<Registered>> = primitives.into_iter().map(Arc::new).collect();
        let mut changed: Vec<PrimitiveKind> = Vec::new();
        for primitive in &batch {
            let kind = primitive.kind();
            if !changed.contains(&kind) {
                changed.push(kind);
            }
        }
        changed.sort_by_key(|kind| kind.as_str());
        let snapshot = self.publish(move |mut next| {
            for primitive in &batch {
                next.insert_arc(Arc::clone(primitive));
            }
            next
        });
        Published { snapshot, changed }
    }

    /// The one place a publish happens.
    ///
    /// # Why a compare-and-swap loop and not load-then-store
    ///
    /// An `ArcSwap` makes a read atomic and a write atomic. It does **not**
    /// make a read followed by a write atomic, and a registry built on
    /// load → modify → store loses a registration for every publish that
    /// overlaps another. Four threads registering a thousand tools between them
    /// kept a few hundred, and every lost write is a tool the server had
    /// already reported as registered and then could not serve.
    /// `a_reader_never_sees_a_torn_snapshot` is the test that found it.
    ///
    /// The loop builds a candidate from the snapshot it actually read, attempts
    /// a compare-and-swap, and on failure **retries against the value that
    /// won** — so a losing publish is not lost, it is re-derived from the newer
    /// snapshot and applied on top of it.
    ///
    /// That is why the closure is handed a `SnapshotBuilder` rather than being
    /// handed one made before the call: the builder has to come from whichever
    /// snapshot this attempt actually saw, and one built earlier cannot be
    /// re-derived.
    ///
    /// The generation is read from the snapshot inside the loop, not from the
    /// store, so a retried publish numbers itself off the value it is being
    /// applied to and two concurrent publishes cannot claim the same number.
    ///
    /// # Why this is not `ArcSwap::rcu`
    ///
    /// `rcu` is this same loop, and it returns the value that was *replaced* —
    /// the **previous** snapshot, not the one now live. That is the right
    /// return for a cache, where the caller wants the stale value back to
    /// recompute from. Here the caller needs the snapshot readers will
    /// actually see, to hand to the fan-out, so returning the old one would
    /// announce a generation that is not live. The two tests that pinned this
    /// down — `the_generation_advances_with_every_publish` and
    /// `a_batch_is_one_generation` — were reading 0 where 1 was published.
    fn publish(&self, apply: impl Fn(SnapshotBuilder) -> SnapshotBuilder) -> Arc<Snapshot> {
        let mut current = self.live.load();
        loop {
            let next = Arc::new(
                apply(current.to_builder()).build_at(current.generation().saturating_add(1)),
            );
            let previous = self.live.compare_and_swap(&current, Arc::clone(&next));
            if Arc::ptr_eq(&current, &previous) {
                return next;
            }
            current = previous;
        }
    }
}

impl Default for RegistryStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::RegistryStore;
    use crate::domain::registry::TestError;
    use crate::domain::registry::{Primitive, PrimitiveId, PrimitiveKind, Registered, Tool};

    fn tool(name: &str) -> Result<Registered, TestError> {
        Ok(Registered::from(Primitive::from(Tool::new(
            name,
            r#"{"type":"object"}"#,
        )?)))
    }

    /// An empty store is a real state, not a null: a listing of it has to
    /// answer zero of four without the caller special-casing it.
    #[test]
    fn an_empty_store_reports_itself_empty() {
        let store = RegistryStore::new();
        assert!(store.snapshot().is_blank());
        assert_eq!(store.generation(), 0);
        assert!(store.snapshot().is_empty(PrimitiveKind::Tool));
    }

    /// A replace is one publish and one generation. A store that removed and
    /// re-added would cost a subscriber two notifications to say one thing.
    #[test]
    fn a_replacement_is_one_publish() -> Result<(), TestError> {
        let store = RegistryStore::new();
        store.register(tool("a")?);
        let replaced = store.register(Registered::from(Primitive::from(
            Tool::new("a", r#"{"type":"object"}"#)?.with_output_schema(r#"{"type":"object"}"#)?,
        )));

        assert_eq!(replaced.changed(), [PrimitiveKind::Tool]);
        assert_eq!(store.snapshot().len(PrimitiveKind::Tool), 1);
        assert_eq!(store.generation(), 2);
        Ok(())
    }

    /// Removing something absent reports no change, and still advances the
    /// generation — a waiter has to be able to learn "nothing changed".
    #[test]
    fn removing_an_absent_primitive_is_a_reported_no_op() -> Result<(), TestError> {
        let store = RegistryStore::new();
        store.register(tool("a")?);
        let before = store.generation();

        let published = store.unregister(PrimitiveKind::Tool, &PrimitiveId::from("absent"));
        assert!(published.is_noop());
        assert!(published.changed().is_empty());
        assert_eq!(
            published.generation(),
            before + 1,
            "a no-op still publishes, so a waiter is not left hanging"
        );
        assert_eq!(store.snapshot().len(PrimitiveKind::Tool), 1);
        Ok(())
    }

    /// A real removal reports the kind, because that is what drives the
    /// notification.
    #[test]
    fn removing_a_present_primitive_reports_its_kind() -> Result<(), TestError> {
        let store = RegistryStore::new();
        store.register(tool("a")?);
        let published = store.unregister(PrimitiveKind::Tool, &PrimitiveId::from("a"));

        assert_eq!(published.changed(), [PrimitiveKind::Tool]);
        assert!(store.snapshot().is_empty(PrimitiveKind::Tool));
        Ok(())
    }

    /// An empty batch is a publish that touched nothing.
    #[test]
    fn an_empty_batch_changes_nothing() {
        let store = RegistryStore::new();
        let published = store.apply([]);
        assert!(published.is_noop());
    }

    /// The snapshot a publish returns is the one readers see, not the one that
    /// was replaced. The generation is what proves it.
    #[test]
    fn a_publish_returns_the_snapshot_that_is_live() -> Result<(), TestError> {
        let store = RegistryStore::new();
        let first = store.register(tool("a")?);
        assert_eq!(
            first.snapshot().generation(),
            store.generation(),
            "the returned snapshot must be the live one"
        );
        assert_eq!(first.snapshot().len(PrimitiveKind::Tool), 1);
        Ok(())
    }
}
