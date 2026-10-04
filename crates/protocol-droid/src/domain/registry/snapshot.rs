//! `Snapshot` — the immutable set a registry publishes — and the builder that
//! produces the next one.
//!
//! # Why a snapshot and not a map
//!
//! A reader holding `&mut HashMap` cannot be concurrent with a writer at all.
//! Holding `&HashMap` can, but then a `tools/call` that has resolved a tool
//! holds a borrow for as long as the tool runs, and a tool that blocks on
//! anything — a network call, a slow sandbox, a user confirmation — holds it for
//! as long as the *user* takes to answer. An in-flight call must not pin the
//! registry against a publish, so the reader owns a whole immutable value and
//! the writer replaces it wholesale.
//!
//! # Two structures per kind, and why
//!
//! Each kind gets a `Vec` and a `HashMap` that share their `Arc`s: the `Vec` is
//! the listing order and the map is the lookup. One structure would have to be
//! both, and the only structure that is both is a map — whose iteration order is
//! unspecified. That is not a cosmetic problem. `tools/list` paginates, a
//! cursor is a position, and an unspecified order means two identical requests
//! can return different pages. The `Vec` is in registration order, so the
//! order is stable, reproducible, and the same for every peer.
//!
//! # Why the builder is a separate type
//!
//! [`Snapshot`] has no method that changes it. Every mutation happens on a
//! [`SnapshotBuilder`], and the only way to get a `Snapshot` is to finish a
//! builder. A published snapshot therefore cannot be modified in place by
//! anyone, including a bug — which is the whole transaction boundary. The
//! store in [`crate::application::registry::store`] reads the current
//! snapshot, copies it into a builder, applies one change, and publishes the
//! result. The copy is shallow: every primitive is an `Arc`, so a thousand
//! unchanged tools cost a thousand pointer copies and not a thousand clones of
//! the tools themselves.
//!
//! # The generation
//!
//! Each snapshot carries the number of the publish that produced it. It is not
//! decorative: a reader that wants to know *whether* anything changed since it
//! last looked can compare one `u64` instead of diffing two sets, and a session
//! that has been told about generation `N` can tell that a later `N` is
//! something new. The number is carried by the value rather than kept beside
//! it, so it cannot drift from the contents it describes.

use std::collections::HashMap;
use std::sync::Arc;

use crate::domain::registry::primitive::{PrimitiveId, PrimitiveKind, Registered};

/// One kind's primitives, in listing order and addressable by id.
#[derive(Debug, Clone, Default)]
struct Table {
    order: Vec<Arc<Registered>>,
    index: HashMap<PrimitiveId, Arc<Registered>>,
}

impl Table {
    fn get(&self, id: &PrimitiveId) -> Option<&Arc<Registered>> {
        self.index.get(id)
    }

    fn contains(&self, id: &PrimitiveId) -> bool {
        self.index.contains_key(id)
    }

    fn order(&self) -> &[Arc<Registered>] {
        &self.order
    }

    const fn len(&self) -> usize {
        self.order.len()
    }

    const fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Puts `primitive` in the set, or replaces what was filed under its id.
    ///
    /// A replace keeps the original's position in the listing order. That is a
    /// choice, and the other one is defensible: a tool whose *schema* changed is
    /// arguably a different tool and belongs at the end. Keeping the position
    /// is better because a page cursor stays valid across a replacement, and a
    /// client that fetched page one and then sees a new tool does not silently
    /// skip a page boundary.
    ///
    /// The common case — a name that is not in the set — is O(1). A replace
    /// walks the listing to find the slot, which is paid only on the rarer
    /// path; a build of a thousand tools must not be quadratic.
    fn insert(&mut self, primitive: Arc<Registered>) {
        let id = primitive.id().clone();
        let replaced = self.index.insert(id, Arc::clone(&primitive));
        // The map hands back the exact value the listing holds, so identity is
        // what locates the slot — no second comparison by id, and no way to
        // find the wrong element when two primitives share a key across a
        // replace.
        if let Some(previous) = replaced
            && let Some(slot) = self
                .order
                .iter()
                .position(|held| Arc::ptr_eq(held, &previous))
            && let Some(target) = self.order.get_mut(slot)
        {
            *target = primitive;
            return;
        }
        self.order.push(primitive);
    }

    /// Takes the primitive filed under `id` out of the set.
    ///
    /// The `Vec::extract_if` is not an optimisation. `retain` would need the
    /// removed `Arc` back to report *what* was removed, and there are two ways
    /// to get it: keep a clone, or match again inside the predicate. Both walk
    /// the vector a second time. `extract_if` yields each removed element once
    /// and leaves the survivors in order, which is what a change set needs to
    /// name the removed tool.
    ///
    /// The leading `..` is the range `extract_if` takes before its filter on
    /// 1.98. `..` is the whole vector, which is the `retain` behaviour; a
    /// range is how you would stop removing once you had what you needed.
    ///
    /// The `count()` is load-bearing. `extract_if` returns a lazy iterator, so
    /// nothing is removed until it is driven — an unbound result compiles
    /// (the lint only fires on a discarded *value*) and leaves the vector
    /// exactly as it was, which is a `remove` that silently does nothing. The
    /// number removed is already known from the filter, so the count is only
    /// there to force the walk.
    fn remove(&mut self, id: &PrimitiveId) -> Option<Arc<Registered>> {
        let mut removed = None;
        let taken = self
            .order
            .extract_if(.., |held: &mut Arc<Registered>| {
                if held.id() == id {
                    removed = Some(Arc::clone(held));
                    true
                } else {
                    false
                }
            })
            .count();
        debug_assert_eq!(
            taken,
            usize::from(removed.is_some()),
            "the filter ran once per match"
        );
        self.index.remove(id).or(removed)
    }
}

/// An immutable set of primitives, as published.
///
/// See the module docs for why it has no mutating method.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::registry::primitive::{Primitive, PrimitiveKind, Tool};
/// use protocol_droid::domain::registry::snapshot::Snapshot;
///
/// let mut builder = Snapshot::builder();
/// builder.insert(Primitive::from(Tool::new("echo", r#"{"type":"object"}"#)?));
/// let snapshot = builder.build();
///
/// assert_eq!(snapshot.len(PrimitiveKind::Tool), 1);
/// assert!(snapshot.get(PrimitiveKind::Tool, &"echo".into()).is_some());
/// # Ok::<(), protocol_droid::domain::ir::JsonObjectError>(())
/// ```
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    generation: u64,
    tools: Table,
    resources: Table,
    templates: Table,
    prompts: Table,
}

impl Snapshot {
    /// An empty snapshot at generation zero.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// A builder holding a copy of this snapshot's contents.
    ///
    /// The copy is shallow — every primitive is shared, not cloned — so this is
    /// cheap enough to call on every publish even for a large registry. The
    /// builder remembers the generation it was copied from so that
    /// [`build`](SnapshotBuilder::build) knows what the next one is.
    #[must_use]
    pub fn to_builder(&self) -> SnapshotBuilder {
        SnapshotBuilder {
            base: self.generation,
            tools: self.tools.clone(),
            resources: self.resources.clone(),
            templates: self.templates.clone(),
            prompts: self.prompts.clone(),
        }
    }

    /// An empty builder.
    #[must_use]
    pub fn builder() -> SnapshotBuilder {
        SnapshotBuilder::default()
    }

    /// The publish this snapshot came from.
    ///
    /// Zero for a snapshot nobody has published, which is also the answer a
    /// session should treat as "nothing has changed" — there is nothing to
    /// have changed.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// The primitives of one kind, in registration order.
    ///
    /// An empty slice for a kind this snapshot does not hold, so a caller never
    /// has to ask whether the kind exists.
    #[must_use]
    pub fn list(&self, kind: PrimitiveKind) -> &[Arc<Registered>] {
        self.table(kind).order()
    }

    /// One primitive by kind and id.
    #[must_use]
    pub fn get(&self, kind: PrimitiveKind, id: &PrimitiveId) -> Option<&Arc<Registered>> {
        self.table(kind).get(id)
    }

    /// Whether a primitive is present.
    #[must_use]
    pub fn contains(&self, kind: PrimitiveKind, id: &PrimitiveId) -> bool {
        self.table(kind).contains(id)
    }

    /// How many primitives of one kind.
    #[must_use]
    pub const fn len(&self, kind: PrimitiveKind) -> usize {
        self.table(kind).len()
    }

    /// Whether this snapshot holds nothing of one kind.
    #[must_use]
    pub const fn is_empty(&self, kind: PrimitiveKind) -> bool {
        self.table(kind).is_empty()
    }

    /// How many primitives in total, across all four kinds.
    #[must_use]
    pub fn total(&self) -> usize {
        PrimitiveKind::ALL
            .into_iter()
            .map(|kind| self.len(kind))
            .sum()
    }

    /// Whether the snapshot holds nothing at all.
    #[must_use]
    pub fn is_blank(&self) -> bool {
        PrimitiveKind::ALL
            .into_iter()
            .all(|kind| self.is_empty(kind))
    }

    const fn table(&self, kind: PrimitiveKind) -> &Table {
        match kind {
            PrimitiveKind::Tool => &self.tools,
            PrimitiveKind::Resource => &self.resources,
            PrimitiveKind::ResourceTemplate => &self.templates,
            PrimitiveKind::Prompt => &self.prompts,
        }
    }
}

/// The mutable half: builds the snapshot that will be published next.
///
/// Never stored beside a live [`Snapshot`]. The store copies, mutates, and
/// builds, so a reader holding the published value is holding a different
/// allocation from the one being written.
#[derive(Debug, Clone, Default)]
pub struct SnapshotBuilder {
    base: u64,
    tools: Table,
    resources: Table,
    templates: Table,
    prompts: Table,
}

impl SnapshotBuilder {
    /// Adds a primitive, or replaces the one filed under the same id.
    pub fn insert(&mut self, primitive: impl Into<Registered>) {
        self.insert_arc(Arc::new(primitive.into()));
    }

    /// Adds a primitive that is already shared.
    ///
    /// The store uses this: a `tools/call` that has resolved a tool wants to
    /// publish a changed copy without a deep clone of the payload.
    pub fn insert_arc(&mut self, primitive: Arc<Registered>) {
        self.table_mut(primitive.kind()).insert(primitive);
    }

    /// Removes the primitive of `kind` filed under `id`, and reports it.
    pub fn remove(&mut self, kind: PrimitiveKind, id: &PrimitiveId) -> Option<Arc<Registered>> {
        self.table_mut(kind).remove(id)
    }

    /// How many primitives of one kind would be published.
    #[must_use]
    pub const fn len(&self, kind: PrimitiveKind) -> usize {
        self.table(kind).len()
    }

    /// Finishes, stamping the next generation.
    ///
    /// The generation is one past whatever the builder was copied from, so a
    /// builder over the live snapshot produces the next number and a fresh
    /// builder produces `1`. Overflow saturates rather than wrapping: a wrapped
    /// generation would let a session see a number it had already been told
    /// about, which is the one way a change could be silently dropped.
    ///
    /// The store does not use this. It calls [`build_at`](Self::build_at) with
    /// a number from its own counter, so the generation is allocated once per
    /// publish rather than by arithmetic that a concurrent publish could
    /// invalidate between the copy and the swap.
    #[must_use]
    pub fn build(self) -> Snapshot {
        let next = self.base.saturating_add(1);
        self.build_at(next)
    }

    /// Finishes, stamping an explicit generation.
    #[must_use]
    pub fn build_at(self, generation: u64) -> Snapshot {
        Snapshot {
            generation,
            tools: self.tools,
            resources: self.resources,
            templates: self.templates,
            prompts: self.prompts,
        }
    }

    const fn table(&self, kind: PrimitiveKind) -> &Table {
        match kind {
            PrimitiveKind::Tool => &self.tools,
            PrimitiveKind::Resource => &self.resources,
            PrimitiveKind::ResourceTemplate => &self.templates,
            PrimitiveKind::Prompt => &self.prompts,
        }
    }

    const fn table_mut(&mut self, kind: PrimitiveKind) -> &mut Table {
        match kind {
            PrimitiveKind::Tool => &mut self.tools,
            PrimitiveKind::Resource => &mut self.resources,
            PrimitiveKind::ResourceTemplate => &mut self.templates,
            PrimitiveKind::Prompt => &mut self.prompts,
        }
    }
}

impl From<Snapshot> for SnapshotBuilder {
    fn from(snapshot: Snapshot) -> Self {
        snapshot.to_builder()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::Snapshot;
    use crate::domain::ir::JsonObjectError;
    use crate::domain::registry::TestError;
    use crate::domain::registry::primitive::{
        Primitive, PrimitiveId, PrimitiveKind, Prompt, Registered, Tool,
    };

    fn tool(name: &str) -> Result<Registered, JsonObjectError> {
        Ok(Registered::from(Primitive::from(Tool::new(
            name,
            r#"{"type":"object"}"#,
        )?)))
    }

    /// A snapshot is immutable, so the only way to change one is to build a
    /// new one. This is the property the whole design exists for: a reader
    /// holding the first value still sees the first value.
    #[test]
    fn a_published_snapshot_does_not_change_under_a_reader() -> Result<(), TestError> {
        let mut builder = Snapshot::builder();
        builder.insert(tool("first")?);
        let first = builder.build();

        let mut next = first.to_builder();
        next.insert(tool("second")?);
        let second = next.build();

        assert_eq!(first.len(PrimitiveKind::Tool), 1);
        assert_eq!(second.len(PrimitiveKind::Tool), 2);
        assert!(first.get(PrimitiveKind::Tool, &"first".into()).is_some());
        assert!(
            first.get(PrimitiveKind::Tool, &"second".into()).is_none(),
            "the reader must not see a tool published after it"
        );
        Ok(())
    }

    /// The listing order is registration order, not hash order. An unspecified
    /// order breaks a page cursor: two identical `tools/list` calls must return
    /// the same page.
    #[test]
    fn listing_order_is_registration_order() -> Result<(), TestError> {
        let mut builder = Snapshot::builder();
        for name in ["c", "a", "b", "d"] {
            builder.insert(tool(name)?);
        }
        let snapshot = builder.build();

        let listed: Vec<&str> = snapshot
            .list(PrimitiveKind::Tool)
            .iter()
            .map(|p| p.id().as_str())
            .collect();
        assert_eq!(listed, vec!["c", "a", "b", "d"]);
        Ok(())
    }

    /// A replacement keeps the original's position, so a cursor taken before
    /// the replacement still lands in the right place afterwards.
    #[test]
    fn a_replacement_keeps_its_position() -> Result<(), TestError> {
        let mut builder = Snapshot::builder();
        for name in ["a", "b", "c"] {
            builder.insert(tool(name)?);
        }
        builder.insert(Registered::from(Primitive::from(
            Tool::new("b", r#"{"type":"object"}"#)?.described("now documented"),
        )));
        let snapshot = builder.build();

        let listed: Vec<&str> = snapshot
            .list(PrimitiveKind::Tool)
            .iter()
            .map(|p| p.id().as_str())
            .collect();
        assert_eq!(listed, vec!["a", "b", "c"]);
        assert_eq!(
            snapshot.len(PrimitiveKind::Tool),
            3,
            "a replace is not an add"
        );
        let Some(Primitive::Tool(replaced)) = snapshot
            .get(PrimitiveKind::Tool, &"b".into())
            .map(|held| held.primitive())
        else {
            return Err("replacement lost from the index".into());
        };
        assert_eq!(replaced.description.as_deref(), Some("now documented"));
        Ok(())
    }

    /// Removal reports what it removed, and leaves the survivors in order. The
    /// reporting is why the removal path uses `extract_if` rather than
    /// `retain`.
    #[test]
    fn removal_reports_the_removed_primitive() -> Result<(), TestError> {
        let mut builder = Snapshot::builder();
        for name in ["a", "b", "c"] {
            builder.insert(tool(name)?);
        }
        let removed = builder.remove(PrimitiveKind::Tool, &PrimitiveId::from("b"));
        let Some(Primitive::Tool(tool)) = removed.as_deref().map(Registered::primitive) else {
            return Err("removal reported the wrong kind".into());
        };
        assert_eq!(tool.name.as_str(), "b");

        let snapshot = builder.build();
        let listed: Vec<&str> = snapshot
            .list(PrimitiveKind::Tool)
            .iter()
            .map(|p| p.id().as_str())
            .collect();
        assert_eq!(listed, vec!["a", "c"]);
        assert!(!snapshot.contains(PrimitiveKind::Tool, &"b".into()));
        Ok(())
    }

    /// Removing something that is not there is a no-op, not an error. A caller
    /// reacting to an unregister it never registered should not have to
    /// distinguish.
    #[test]
    fn removing_an_absent_primitive_reports_nothing() {
        let mut builder = Snapshot::builder();
        assert!(
            builder
                .remove(PrimitiveKind::Tool, &"ghost".into())
                .is_none()
        );
        assert!(builder.build().is_blank());
    }

    /// The four kinds are four independent tables. A tool added must not make
    /// `prompts/list` look different, which is the whole reason the kind is
    /// part of a primitive's identity rather than a field on a shared struct.
    #[test]
    fn the_kinds_do_not_leak_into_each_other() -> Result<(), TestError> {
        let mut builder = Snapshot::builder();
        builder.insert(tool("t")?);
        builder.insert(Registered::from(Primitive::from(Prompt::new("p"))));
        let snapshot = builder.build();

        assert_eq!(snapshot.len(PrimitiveKind::Tool), 1);
        assert_eq!(snapshot.len(PrimitiveKind::Prompt), 1);
        assert_eq!(snapshot.len(PrimitiveKind::Resource), 0);
        assert_eq!(snapshot.len(PrimitiveKind::ResourceTemplate), 0);
        assert_eq!(snapshot.total(), 2);
        assert!(!snapshot.is_blank());
        assert!(
            snapshot.get(PrimitiveKind::Prompt, &"t".into()).is_none(),
            "a tool id must not resolve a prompt"
        );
        Ok(())
    }

    /// An unknown kind query answers empty rather than panicking, so a caller
    /// iterating `PrimitiveKind::ALL` never needs to ask whether a kind has
    /// anything.
    #[test]
    fn an_unheld_kind_is_empty_not_missing() {
        let snapshot = Snapshot::empty();
        for kind in PrimitiveKind::ALL {
            assert!(snapshot.is_empty(kind));
            assert_eq!(snapshot.list(kind).len(), 0);
            assert!(snapshot.get(kind, &"anything".into()).is_none());
        }
        assert!(snapshot.is_blank());
        assert_eq!(snapshot.total(), 0);
        assert_eq!(snapshot.generation(), 0);
    }

    /// The copy a builder takes is shallow. A thousand tools cost a thousand
    /// pointer copies, and the shared payload is the same allocation.
    #[test]
    fn the_copy_shares_its_primitives() -> Result<(), TestError> {
        let mut builder = Snapshot::builder();
        builder.insert_arc(Arc::new(tool("shared")?));
        let snapshot = builder.build();

        let Some(held) = snapshot.get(PrimitiveKind::Tool, &"shared".into()) else {
            return Err("tool missing".into());
        };
        let next = snapshot.to_builder();
        let rebuilt = next.build();
        let Some(again) = rebuilt.get(PrimitiveKind::Tool, &"shared".into()) else {
            return Err("tool lost in the copy".into());
        };
        assert!(Arc::ptr_eq(held, again));
        Ok(())
    }

    /// The generation advances by exactly one per publish built over the live
    /// snapshot, and a builder from scratch starts at one.
    #[test]
    fn the_generation_advances_by_one() -> Result<(), TestError> {
        let mut builder = Snapshot::builder();
        builder.insert(tool("a")?);
        let first = builder.build_at(1);

        let second = first.to_builder().build_at(first.generation() + 1);
        assert_eq!(first.generation(), 1);
        assert_eq!(second.generation(), 2);
        Ok(())
    }

    /// A builder copied from a live snapshot stamps the next generation, and a
    /// fresh one starts at `1`. Saturating at the top rather than wrapping is
    /// what keeps a session from seeing a number it was already told about.
    #[test]
    fn a_copied_builder_stamps_the_next_generation() -> Result<(), TestError> {
        let mut builder = Snapshot::builder();
        builder.insert(tool("a")?);
        let first = builder.build();
        assert_eq!(first.generation(), 1);

        let mut second = first.to_builder();
        second.insert(tool("b")?);
        assert_eq!(second.build().generation(), 2);

        let at_max: Snapshot = first.to_builder().build_at(u64::MAX);
        assert_eq!(at_max.to_builder().build().generation(), u64::MAX);
        Ok(())
    }
}
