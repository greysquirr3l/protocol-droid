//! The read path: a snapshot plus a session, filtered.
//!
//! # Why filtering happens on read rather than on publish
//!
//! A store holding only the primitives a session may see cannot answer for two
//! sessions at once, and a store publishing a per-session copy would turn one
//! registration into one snapshot per live session. So the store holds the
//! union and the *filter* is the session's, applied where the question is
//! asked: [`RegistryStore::list_visible`].
//!
//! The filter is [`is_visible`], which is a pure function of three inputs and
//! reads no shared state. That is what lets a listing be a cheap scan — a
//! `u128` mask test and a scope subset per primitive, no allocation, no lock,
//! and no promise about what another session would see.
//!
//! # The count must come from the same pass
//!
//! `list_visible` returns the visible primitives and the total. Computing the
//! total separately would be a second filter, and a second filter can disagree
//! with the first if a primitive's metadata were ever anything but a pure
//! function of its payload — which is why `PrimitiveMeta` is derived and
//! never stored. One pass, one answer, no way for the two numbers to come from
//! different worlds.

use std::sync::Arc;

use crate::application::registry::store::RegistryStore;
use crate::domain::FeatureProfile;
use crate::domain::registry::{AuthContext, PrimitiveKind, Registered, is_visible};

/// What one session can see, and how much exists.
///
/// `visible` and `total` differ exactly when the filter hid something, which
/// is the thing a caller needs to know: a `tools/list` that returns four of
/// five tools is a different answer from one that returns five of five, and
/// the client cannot tell them apart without the total.
#[derive(Debug)]
pub struct Listing {
    visible: Vec<Arc<Registered>>,
    total: usize,
    generation: u64,
}

impl Listing {
    /// The primitives this session may see, in listing order.
    #[must_use]
    pub fn visible(&self) -> &[Arc<Registered>] {
        &self.visible
    }

    /// How many primitives of this kind exist, before filtering.
    #[must_use]
    pub const fn total(&self) -> usize {
        self.total
    }

    /// How many were hidden from this session.
    #[must_use]
    pub const fn hidden(&self) -> usize {
        self.total.saturating_sub(self.visible.len())
    }

    /// The publish this listing was taken from.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }
}

impl RegistryStore {
    /// Every primitive of `kind` that `session` may see.
    ///
    /// Takes an owned [`Arc<Snapshot>`] rather than a guard, so a caller that
    /// goes on to build a response, or hand the primitives to a tool, is not
    /// holding the store against a publish for the duration.
    #[must_use]
    pub fn list_visible(
        &self,
        session: &AuthContext,
        profile: &FeatureProfile,
        kind: PrimitiveKind,
    ) -> Listing {
        let snapshot = self.snapshot();
        let all = snapshot.list(kind);
        let total = all.len();
        let visible = all
            .iter()
            .filter(|primitive| is_visible(session, profile, primitive.meta()))
            .map(Arc::clone)
            .collect();
        Listing {
            visible,
            total,
            generation: snapshot.generation(),
        }
    }

    /// One primitive, if `session` may see it.
    ///
    /// A miss is `None` whether the primitive is absent or merely hidden, and
    /// that is deliberate: telling a caller *which* would let a session probe
    /// for the existence of a tool it cannot see by reading the difference
    /// between a 404 and a 403. The server still logs the difference.
    #[must_use]
    pub fn get_visible(
        &self,
        session: &AuthContext,
        profile: &FeatureProfile,
        kind: PrimitiveKind,
        id: &str,
    ) -> Option<Arc<Registered>> {
        let snapshot = self.snapshot();
        let registered = snapshot.get(kind, &id.into())?.clone();
        is_visible(session, profile, registered.meta()).then_some(registered)
    }
}

#[cfg(test)]
mod tests {
    use super::RegistryStore;
    use crate::domain::registry::TestError;
    use crate::domain::registry::{AuthContext, Primitive, PrimitiveKind, Registered, Tool};
    use crate::domain::{Feature, FeatureProfile};

    fn tool(name: &str) -> Result<Registered, TestError> {
        Ok(Registered::from(Primitive::from(Tool::new(
            name,
            r#"{"type":"object"}"#,
        )?)))
    }

    fn everything() -> FeatureProfile {
        FeatureProfile::empty()
            .with(Feature::Tools)
            .with(Feature::ToolStructuredOutput)
    }

    /// The listing keeps registration order, because a page cursor is only
    /// meaningful if the order is defined.
    #[test]
    fn a_listing_is_in_registration_order() -> Result<(), TestError> {
        let store = RegistryStore::new();
        for name in ["c", "a", "b"] {
            store.register(tool(name)?);
        }
        let session = AuthContext::unrestricted("alice");
        let listing = store.list_visible(&session, &everything(), PrimitiveKind::Tool);

        let names: Vec<&str> = listing
            .visible()
            .iter()
            .map(|held| held.id().as_str())
            .collect();
        assert_eq!(names, vec!["c", "a", "b"]);
        Ok(())
    }

    /// `hidden` is derived from the same pass as `visible`, so the two can
    /// never come from different worlds.
    #[test]
    fn the_total_counts_what_the_filter_hid() -> Result<(), TestError> {
        let store = RegistryStore::new();
        let structured = Registered::from(Primitive::from(
            Tool::new("structured", r#"{"type":"object"}"#)?
                .with_output_schema(r#"{"type":"object"}"#)?,
        ));
        store.register(structured);
        store.register(tool("plain")?);

        let session = AuthContext::unrestricted("alice");
        let without = store.list_visible(
            &session,
            &FeatureProfile::empty().with(Feature::Tools),
            PrimitiveKind::Tool,
        );

        assert_eq!(without.visible().len(), 1);
        assert_eq!(without.total(), 2);
        assert_eq!(without.hidden(), 1);
        assert_eq!(
            without.total(),
            without.visible().len() + without.hidden(),
            "visible plus hidden must account for every primitive"
        );
        Ok(())
    }

    /// The generation travels with the listing, so a client can tell two
    /// identical-looking answers apart.
    #[test]
    fn a_listing_reports_the_generation_it_came_from() -> Result<(), TestError> {
        let store = RegistryStore::new();
        store.register(tool("a")?);
        let session = AuthContext::unrestricted("alice");
        let listing = store.list_visible(&session, &everything(), PrimitiveKind::Tool);
        assert_eq!(listing.generation(), 1);
        Ok(())
    }

    /// A primitive this session may not see is a miss, and is indistinguishable
    /// from one that is not there. Anything else turns `tools/call` into an
    /// oracle for the existence of tools the caller cannot see.
    #[test]
    fn a_hidden_primitive_and_an_absent_one_both_read_as_none() -> Result<(), TestError> {
        let store = RegistryStore::new();
        let guarded = Registered::from(Primitive::from(Tool::new(
            "guarded",
            r#"{"type":"object"}"#,
        )?))
        .requiring_scope(crate::domain::registry::Scope::Admin);
        store.register(guarded);

        let nobody = AuthContext::new("mallory", crate::domain::registry::ScopeSet::empty());
        let hidden = store.get_visible(&nobody, &everything(), PrimitiveKind::Tool, "guarded");
        let absent = store.get_visible(&nobody, &everything(), PrimitiveKind::Tool, "nope");

        assert!(hidden.is_none());
        assert!(absent.is_none());

        let root = AuthContext::unrestricted("root");
        assert!(
            store
                .get_visible(&root, &everything(), PrimitiveKind::Tool, "guarded")
                .is_some(),
            "the same lookup must succeed for a principal that does hold the scope"
        );
        Ok(())
    }
}
