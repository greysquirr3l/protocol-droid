//! The visibility rule: may this session see this primitive?
//!
//! # One pure function, three inputs, no state
//!
//! [`is_visible`] is a total function of `(AuthContext, FeatureProfile,
//! PrimitiveMeta)`. It reads nothing else, holds nothing, and decides nothing
//! about ordering — two calls with the same three values give the same answer,
//! which is what makes a registry testable without a session, a transport, or a
//! clock. Everything that *looks like* a decision (whether the peer
//! authenticated, which revision it negotiated, which list a primitive is on)
//! has already happened by the time this runs, and arrives as one of the three
//! arguments.
//!
//! # Why the two subsets are the same operation
//!
//! Both halves are "does the subject have everything the requirement needs":
//! the profile must carry the primitive's [`FeatureSet`], and the principal
//! must hold the primitive's [`ScopeSet`]. The crate already makes features a
//! `u128` bitset and the same comparison in
//! [`FeatureSet::is_subset_of`](crate::domain::ir::FeatureSet::is_subset_of),
//! which is the direction the lowering gate uses. Reusing it here is the point:
//! a second "is this feature on" helper would be a second source of truth for
//! the same question, and the two would drift.
//!
//! # Default deny, and the one case that is not
//!
//! A primitive that requires a feature the session lacks is hidden, and one
//! that requires a scope the principal lacks is hidden. There is no way to
//! express "visible unless something goes wrong" here, and that is the point —
//! the failure mode of a registry is a tool appearing to a session that should
//! not have it, and that failure is silent. The converse, a tool hidden from a
//! session that could have used it, is visible in the logs and recoverable.
//!
//! A primitive with *no* requirements is visible to everyone, because an empty
//! set is a subset of every set. That is not an exception in the code; it is
//! the same subset test with nothing required.
//!
//! # Where the MCP-shaped parts live, and why that is not a problem
//!
//! This module names [`FeatureProfile`], which is MCP's profile, so it is
//! MCP-specific by construction. It lives here rather than under a `mcp/`
//! subdirectory for the same reason `crate::domain::ir::tool` does: the
//! `domain/ir/` layer established the precedent of a domain module naming
//! [`Feature`](crate::domain::Feature) directly, and splitting the IR from the
//! registry into separate `mcp/` folders would be one nesting level for no
//! boundary. The protocol-neutral part — the snapshot, the publish, the
//! `ArcSwap` — is in [`super::snapshot`] and names no MCP type at all.

use crate::domain::FeatureProfile;
use crate::domain::ir::FeatureSet;
use crate::domain::registry::auth::{AuthContext, ScopeSet};
use crate::domain::registry::primitive::PrimitiveMeta;

/// Whether `session` may see the primitive described by `meta`.
///
/// Both conditions must hold, and neither has a default that favours allowing:
///
/// - the session's profile carries every feature the primitive needs, and
/// - the principal holds every scope the primitive needs.
///
/// # Examples
///
/// A tool that needs `Tasks` support is hidden from a session that never
/// negotiated it, which is the case the rule exists for — the alternative is
/// advertising a tool the session cannot call.
///
/// ```
/// use protocol_droid::domain::Feature;
/// use protocol_droid::domain::FeatureProfile;
/// use protocol_droid::domain::ir::FeatureSet;
/// use protocol_droid::domain::registry::auth::{AuthContext, ScopeSet};
/// use protocol_droid::domain::registry::primitive::{PrimitiveKind, PrimitiveMeta};
/// use protocol_droid::domain::registry::visibility::is_visible;
///
/// let meta = PrimitiveMeta::new(PrimitiveKind::Tool)
///     .requires(FeatureSet::empty().with(Feature::Tasks));
/// let session = AuthContext::new("alice", ScopeSet::empty());
///
/// let without_tasks = FeatureProfile::empty();
/// let with_tasks = FeatureProfile::empty().with(Feature::Tasks);
///
/// assert!(!is_visible(&session, &without_tasks, &meta));
/// assert!(is_visible(&session, &with_tasks, &meta));
/// ```
#[must_use]
pub const fn is_visible(
    session: &AuthContext,
    profile: &FeatureProfile,
    meta: &PrimitiveMeta,
) -> bool {
    let has_features = meta.required_features().is_subset_of(profile.as_set());
    has_features && session.holds(meta.required_scopes())
}

/// The scopes a principal is missing for `meta`.
///
/// The visible half of the rule, for a message that has to say *which* rule
/// failed. [`is_visible`] answers yes or no and a caller that then logs
/// "not visible" has learned nothing actionable; this names the gap.
#[must_use]
pub const fn missing_scopes(session: &AuthContext, meta: &PrimitiveMeta) -> ScopeSet {
    meta.required_scopes().difference(session.scopes())
}

/// The features a profile is missing for `meta`.
///
/// The feature-shaped counterpart to [`missing_scopes`], and for the same
/// reason: a session that cannot see a tool should be able to say which
/// capability it did not negotiate.
#[must_use]
pub const fn missing_features(profile: &FeatureProfile, meta: &PrimitiveMeta) -> FeatureSet {
    meta.required_features().difference(profile.as_set())
}

#[cfg(test)]
mod tests {
    use crate::domain::Feature;
    use crate::domain::FeatureProfile;
    use crate::domain::ir::FeatureSet;
    use crate::domain::registry::auth::{AuthContext, Scope, ScopeSet};
    use crate::domain::registry::primitive::{PrimitiveKind, PrimitiveMeta};
    use crate::domain::registry::visibility::{is_visible, missing_features, missing_scopes};

    /// The case the rule exists for. A tool needing `Tasks` is hidden from a
    /// session that did not negotiate it, so the tool is never advertised to a
    /// peer that could not call it.
    #[test]
    fn a_tool_needing_tasks_is_hidden_without_it() {
        let meta = PrimitiveMeta::new(PrimitiveKind::Tool)
            .requires(FeatureSet::empty().with(Feature::Tasks));
        let session = AuthContext::unrestricted("alice");

        assert!(!is_visible(&session, &FeatureProfile::empty(), &meta));
        assert!(is_visible(
            &session,
            &FeatureProfile::empty().with(Feature::Tasks),
            &meta
        ));
    }

    /// A principal that holds nothing sees nothing that requires anything.
    /// This is the fail-secure direction and it is the default: both
    /// [`AuthContext`] and [`FeatureProfile`] start empty, so a context built
    /// by forgetting a field denies rather than allows.
    #[test]
    fn the_default_context_and_profile_deny() {
        let meta = PrimitiveMeta::new(PrimitiveKind::Tool)
            .requires(FeatureSet::empty().with(Feature::Tools))
            .scopes(ScopeSet::empty().with(Scope::Write));
        assert!(!is_visible(
            &AuthContext::default(),
            &FeatureProfile::empty(),
            &meta
        ));
    }

    /// An empty requirement is visible to everyone, and it is the same subset
    /// test rather than a special case — a caller cannot tell the two apart by
    /// reading the predicate.
    #[test]
    fn a_primitive_requiring_nothing_is_visible() {
        let meta = PrimitiveMeta::new(PrimitiveKind::Prompt);
        assert!(is_visible(
            &AuthContext::default(),
            &FeatureProfile::empty(),
            &meta
        ));
    }

    /// The two halves are independent. A session with every feature and no
    /// scopes still sees nothing, and a principal with every scope and no
    /// features still sees nothing.
    #[test]
    fn the_two_halves_are_independent() {
        let meta = PrimitiveMeta::new(PrimitiveKind::Tool)
            .requires(FeatureSet::empty().with(Feature::Tasks))
            .scopes(ScopeSet::empty().with(Scope::Admin));

        let features_only = AuthContext::unrestricted("a");
        let scopes_only = AuthContext::new(
            "b",
            ScopeSet::empty()
                .with(Scope::Read)
                .with(Scope::Write)
                .with(Scope::Admin),
        );

        assert!(!is_visible(&features_only, &FeatureProfile::empty(), &meta));
        // Holding every scope is not enough without the feature, and holding
        // the feature is not enough without the scopes: the two halves fail
        // independently.
        assert!(!is_visible(&scopes_only, &FeatureProfile::empty(), &meta));
        assert!(is_visible(
            &scopes_only,
            &FeatureProfile::empty().with(Feature::Tasks),
            &meta
        ));
    }

    /// The diagnostics have to name the gap, not just that there is one. A
    /// "not visible" with no reason is the thing that makes a session look
    /// broken when it is actually a missing capability.
    #[test]
    fn the_diagnostics_name_what_is_missing() {
        let meta = PrimitiveMeta::new(PrimitiveKind::Tool)
            .requires(
                FeatureSet::empty()
                    .with(Feature::Tasks)
                    .with(Feature::Elicitation),
            )
            .scopes(ScopeSet::empty().with(Scope::Write).with(Scope::Admin));

        let session = AuthContext::new("a", ScopeSet::empty().with(Scope::Read));
        let scopes = missing_scopes(&session, &meta);
        assert!(scopes.contains(Scope::Write));
        assert!(scopes.contains(Scope::Admin));
        assert!(!scopes.contains(Scope::Read));

        let profile = FeatureProfile::empty().with(Feature::Tools);
        let features = missing_features(&profile, &meta);
        assert!(features.contains(Feature::Tasks));
        assert!(features.contains(Feature::Elicitation));
        assert!(!features.contains(Feature::Tools));

        // A satisfied requirement has no gap, and the empty answer must be
        // empty rather than the full required set.
        let full = FeatureProfile::empty()
            .with(Feature::Tasks)
            .with(Feature::Elicitation);
        assert!(missing_features(&full, &meta).is_empty());
        assert!(
            missing_scopes(&AuthContext::unrestricted("a"), &meta).is_empty(),
            "an unrestricted principal is missing nothing"
        );
    }
}
