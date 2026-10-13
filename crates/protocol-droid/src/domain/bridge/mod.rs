//! What a bridge can honestly promise.
//!
//! A bridge sits between two peers that negotiated independently. The client
//! on one side agreed to a set of features with the bridge; the server on the
//! other side agreed to a set with the bridge. Neither negotiation saw the
//! other, and neither one is the truth on its own.
//!
//! ## Why the intersection is the only honest answer
//!
//! A bridge that advertised the *downstream* profile would be promising the
//! client things it cannot deliver. The client sees `tools/call` supported, so
//! it calls — and the request dies somewhere behind the bridge that the client
//! cannot see and has no way to attribute. That is strictly worse than a
//! refusal at handshake time, where the client learns the truth before it acts.
//!
//! So the bridge advertises the intersection, and keeps every feature it had to
//! drop together with the side that refused it. A dropped feature with a
//! recorded reason is a limitation an operator can see; a dropped feature
//! with no record is a bug that only shows up as a failed tool call.
//!
//! ## Two versions, not one
//!
//! The downstream and upstream sessions each pin their own
//! [`ProtocolVersion`]. They are frequently different — that is the whole
//! reason to run a bridge — and they are stored separately rather than
//! collapsed, because "the client speaks 2025-03-26 and the server speaks
//! 2026-07-28" is exactly the fact an operator needs when a message is
//! translated.
//!
//! ## What a bridge does *not* claim
//!
//! Agreement is not reachability. Both peers negotiated with the bridge, and
//! the bridge has still not proved either one is currently answering. A bridge
//! that reported success here would be reporting on a connection it has not
//! made.

use core::fmt;

use crate::domain::{Feature, FeatureProfile, ProtocolVersion, resolve};

#[cfg(feature = "a2a")]
pub mod a2a_mcp;

/// The two sides of a bridge, each having negotiated with it independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Leg {
    /// The revision this side settled on.
    pub version: ProtocolVersion,
    /// What that side agreed to, after resolving its own capabilities.
    pub profile: FeatureProfile,
}

impl Leg {
    /// A leg from a version and an already-resolved profile.
    #[must_use]
    pub const fn new(version: ProtocolVersion, profile: FeatureProfile) -> Self {
        Self { version, profile }
    }
}

/// A feature the bridge cannot serve, and which side refused it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Withheld {
    /// The feature neither side can provide.
    pub feature: Feature,
    /// Which side said no.
    pub refused_by: RefusedBy,
}

/// Which of the two peers could not provide a feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusedBy {
    /// The server, behind the bridge, cannot do it.
    Upstream,
    /// The client, in front of the bridge, cannot do it.
    Downstream,
}

impl fmt::Display for RefusedBy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Upstream => formatter.write_str("the upstream server"),
            Self::Downstream => formatter.write_str("the downstream client"),
        }
    }
}

/// Both sides, and what the bridge can therefore serve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agreement {
    downstream: Leg,
    upstream: Leg,
    served: FeatureProfile,
    withheld: Vec<Withheld>,
}

impl Agreement {
    /// Reconciles two independently negotiated legs.
    ///
    /// Pure and total: any two legs produce an agreement, including two that
    /// share no features at all. Refusing to build one in that case would leave
    /// the caller with nothing to report on, and "nothing in common" is exactly
    /// the situation where an operator most needs to be told.
    #[must_use]
    pub fn between(downstream: Leg, upstream: Leg) -> Self {
        let served = downstream.profile.intersection(upstream.profile);

        // Downstream is checked first so a feature *neither* side has is
        // attributed to one side rather than to both. Both would be true, but
        // an operator reading "the client cannot do this" and then "the server
        // cannot do this" for the same feature reads as a bug in the report.
        let withheld = downstream
            .profile
            .difference(served)
            .iter()
            .map(|feature| Withheld {
                feature,
                refused_by: RefusedBy::Downstream,
            })
            .chain(
                upstream
                    .profile
                    .difference(served)
                    .iter()
                    .map(|feature| Withheld {
                        feature,
                        refused_by: RefusedBy::Upstream,
                    }),
            )
            .collect();

        Self {
            downstream,
            upstream,
            served,
            withheld,
        }
    }

    /// The client side of the bridge.
    #[must_use]
    pub const fn downstream(&self) -> &Leg {
        &self.downstream
    }

    /// The server side of the bridge.
    #[must_use]
    pub const fn upstream(&self) -> &Leg {
        &self.upstream
    }

    /// What the bridge can actually serve: the intersection.
    ///
    /// This is what gets advertised downstream. Anything else is a promise the
    /// bridge cannot keep.
    #[must_use]
    pub const fn served(&self) -> FeatureProfile {
        self.served
    }

    /// The features dropped, and which side refused each.
    #[must_use]
    pub fn withheld(&self) -> &[Withheld] {
        &self.withheld
    }

    /// Whether `feature` can be served, in both directions.
    #[must_use]
    pub fn serves(&self, feature: Feature) -> bool {
        self.served.allows(feature)
    }

    /// Whether the two sides settled on the same revision.
    ///
    /// Not required for a bridge — translating between revisions is its whole
    /// purpose — but a bridge answering `true` has nothing to translate and is
    /// usually a misconfiguration, so it is worth asking.
    #[must_use]
    pub fn same_version(&self) -> bool {
        self.downstream.version == self.upstream.version
    }

    /// A one-line summary for a log or a `--json` field.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "client {} / server {} — {} features served, {} withheld",
            self.downstream.version,
            self.upstream.version,
            self.served.iter().count(),
            self.withheld.len()
        )
    }
}

/// Resolves both legs from their negotiations and reconciles them.
///
/// The convenience path for the common case where the caller has
/// [`Negotiation`](crate::domain::Negotiation) values rather than resolved
/// profiles. Both legs are resolved *independently* — that is the point, and
/// collapsing them into one negotiation would produce a profile nobody agreed
/// to.
#[must_use]
pub fn agree(
    downstream: crate::domain::Negotiation,
    upstream: crate::domain::Negotiation,
) -> Agreement {
    Agreement::between(
        Leg::new(downstream.version, resolve(downstream)),
        Leg::new(upstream.version, resolve(upstream)),
    )
}

#[cfg(test)]
mod tests {
    use crate::domain::{
        Feature, FeatureProfile, Negotiation, ProtocolVersion, TransportKind, agree,
    };

    use super::{Agreement, Leg, RefusedBy};

    type TestError = Box<dyn std::error::Error>;

    fn profile_with(features: &[Feature]) -> FeatureProfile {
        features
            .iter()
            .fold(FeatureProfile::empty(), |profile, feature| {
                profile.with(*feature)
            })
    }

    /// The whole reason this type exists: a feature the client supports and
    /// the server does not must not be advertised, because the client would
    /// then use it and the failure would surface as a dead tool call.
    #[test]
    fn a_feature_only_the_client_has_is_not_served() {
        let agreement = Agreement::between(
            Leg::new(
                ProtocolVersion::latest_known(),
                profile_with(&[Feature::Tools, Feature::Resources]),
            ),
            Leg::new(
                ProtocolVersion::latest_known(),
                profile_with(&[Feature::Tools]),
            ),
        );

        assert!(
            agreement.serves(Feature::Tools),
            "the shared feature vanished"
        );
        assert!(
            !agreement.serves(Feature::Resources),
            "the bridge advertised a feature the server cannot serve"
        );
    }

    /// Every dropped feature names the side that refused it. A withheld list
    /// with no reason is the failure mode this type was written to prevent.
    #[test]
    fn each_withheld_feature_names_its_refuser() {
        let agreement = Agreement::between(
            Leg::new(
                ProtocolVersion::latest_known(),
                profile_with(&[Feature::Tools, Feature::Resources]),
            ),
            Leg::new(
                ProtocolVersion::latest_known(),
                profile_with(&[Feature::Tools, Feature::Prompts]),
            ),
        );

        let refused: Vec<(&str, RefusedBy)> = agreement
            .withheld()
            .iter()
            .map(|entry| (entry.feature.as_str(), entry.refused_by))
            .collect();

        assert!(
            refused.contains(&("resources", RefusedBy::Downstream)),
            "the client's own feature was not attributed to the client: {refused:?}"
        );
        assert!(
            refused.contains(&("prompts", RefusedBy::Upstream)),
            "the server's missing feature was not attributed to the server: {refused:?}"
        );
    }

    /// Two sides with nothing in common still produce an agreement, because
    /// "these share nothing" is the report an operator most needs.
    #[test]
    fn sides_with_nothing_in_common_still_agree() {
        let agreement = Agreement::between(
            Leg::new(
                ProtocolVersion::latest_known(),
                profile_with(&[Feature::Resources]),
            ),
            Leg::new(
                ProtocolVersion::latest_known(),
                profile_with(&[Feature::Prompts]),
            ),
        );

        assert!(agreement.served().is_empty(), "something was invented");
        assert_eq!(agreement.withheld().len(), 2);
    }

    /// Identical legs withhold nothing. A non-empty withheld list here would
    /// mean the intersection is wrong.
    #[test]
    fn identical_legs_withhold_nothing() {
        let features = profile_with(&[Feature::Tools, Feature::Resources, Feature::Prompts]);
        let agreement = Agreement::between(
            Leg::new(ProtocolVersion::latest_known(), features),
            Leg::new(ProtocolVersion::latest_known(), features),
        );

        assert!(
            agreement.withheld().is_empty(),
            "{:?}",
            agreement.withheld()
        );
    }

    /// The two versions are stored apart. Collapsing them would lose the one
    /// fact an operator needs when a message is translated.
    #[test]
    fn the_two_versions_are_kept_apart() -> Result<(), TestError> {
        let negotiation = |version| Negotiation::bare(version, TransportKind::StreamableHttp);
        let newer = ProtocolVersion::latest_known();
        let older = "2024-11-05".parse()?;

        let agreement = agree(negotiation(older), negotiation(newer));

        assert_eq!(agreement.downstream().version, older);
        assert_eq!(agreement.upstream().version, newer);
        assert!(!agreement.same_version());
        Ok(())
    }
}
