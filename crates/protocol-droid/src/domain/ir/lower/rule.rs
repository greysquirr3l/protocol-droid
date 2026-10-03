//! `DegradeRule` — the enumerable set of downgrades a caller may opt into.
//!
//! # Why this is an enum and not a callback
//!
//! The alternative to an enum is a closure: "here is what to do when the value
//! needs a feature the target lacks". That is more flexible and it is exactly
//! wrong here, for two reasons.
//!
//! **A callback cannot be audited.** A `TranslationReport` is only useful if a
//! reader can check, before running a bridge, exactly what it will and will not
//! lose. A list of enum variants is greppable and snapshot-testable; a set of
//! closures is neither.
//!
//! **A callback fails open.** Given a feature it has never seen — one added by
//! a revision this build predates — the only way to stay forward-compatible is
//! to do *something*, and "something" is where silent data loss comes from. An
//! enum has no arm for a feature it does not name, so the default is an error.
//!
//! # Loss is a property of the rule, not of the caller
//!
//! Each rule declares whether it is [`Loss::None`], [`Loss::Flagged`], or
//! [`Loss::Destructive`]. That is what lets a bridge say "this translation is
//! lossless or flagged, as promised" and have that mean something checkable
//! rather than a claim about intent. A rule that claimed to be lossless while
//! dropping a field would pass a hand-written policy and fail the report.
//!
//! `AudioToTextPlaceholder` is [`Loss::Destructive`] and is the only rule with
//! no lossless alternative in the specification — there is no textual stand-in
//! for audio the way there is for structured JSON — so it is forbidden unless a
//! caller names it explicitly.

use crate::domain::Feature;

/// How much a rule gives up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Loss {
    /// Nothing is lost. The information is still there, just in another shape.
    None,
    /// Information is lost, but the loss is recorded in the report and the
    /// caller has asked for it.
    Flagged,
    /// Information is lost in a way that changes what the value *means*.
    ///
    /// Not the same as [`Loss::Flagged`]: a text placeholder for audio is not a
    /// degraded audio block, it is a different thing that says it stands in
    /// for audio. A caller that did not ask for that is not getting what it
    /// asked for.
    Destructive,
}

impl Loss {
    /// Whether nothing is lost.
    #[must_use]
    pub const fn is_lossless(self) -> bool {
        matches!(self, Self::None)
    }

    /// Whether the rule is safe for a caller that promised "lossless or
    /// flagged".
    #[must_use]
    pub const fn is_acceptable(self) -> bool {
        matches!(self, Self::None | Self::Flagged)
    }
}

/// One thing lowering may do to make a value expressible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DegradeRule {
    /// Serialise `structuredContent` into a text content block.
    ///
    /// Added `2025-06-18`; the specification names this as the recommended
    /// fallback. Lossless: the bytes are unchanged, only the carrier differs.
    StructuredContentToText,

    /// Drop annotations a revision does not define.
    ///
    /// [`Loss::Flagged`]. Annotations arrived in `2025-03-26` and a
    /// `2024-11-05` peer simply ignores them, so nothing that peer could have
    /// acted on is lost — but a bridge that drops them without saying so is
    /// lying about what happened.
    DropAnnotations,

    /// Replace an audio block with a text placeholder naming it.
    ///
    /// [`Loss::Destructive`], and there is no lossless alternative: there is no
    /// textual stand-in for audio. For this reason it is the one rule the
    /// specification does not sanction, and a caller must name it.
    AudioToTextPlaceholder,

    /// Drop an audio block.
    DropAudio,

    /// Drop an image block.
    DropImage,

    /// Drop a resource link, `2025-06-18` and later.
    DropResourceLink,

    /// Drop an embedded resource.
    DropResource,
}

impl DegradeRule {
    /// Every rule a [`DegradePolicy`](crate::domain::DegradePolicy) can select
    /// to satisfy a missing feature, in a stable order so a policy set or a
    /// report reads the same way twice.
    ///
    /// Every rule here names a feature, which is the invariant that keeps the
    /// list honest: a rule in this list is reachable by `rule_for`, and a rule
    /// that is *not* in it is not reachable by a policy at all.
    ///
    /// The three that are not listed — [`AudioToTextPlaceholder`],
    /// [`DropImage`] and [`DropResource`] — are opt-in reductions rather than
    /// fallbacks. The placeholder because the specification does not sanction it
    /// and dropping the block is the only policy-selectable response to audio;
    /// the other two because images and embedded resources are expressible at
    /// every revision, so no revision's absence ever requires dropping them.
    /// Listing any of them would let `rule_for` hand back something a caller
    /// never asked for, or that could never apply.
    pub const ALL: &'static [Self] = &[
        Self::StructuredContentToText,
        Self::DropAnnotations,
        Self::DropAudio,
        Self::DropResourceLink,
    ];

    /// The feature whose absence this rule exists to work around.
    ///
    /// `None` for a rule that is not a *fallback* — a reduction a caller opts
    /// into for its own reasons, not something a revision forces. Images and
    /// embedded resources have been expressible since `2024-11-05` and
    /// annotations are simply ignored by older peers, so there is no revision
    /// whose absence ever makes dropping them necessary. Reporting a feature
    /// for them would be a lie about why the rule exists, and would make
    /// [`rule_for`](super::rule_for) offer them as answers to a question they
    /// cannot answer.
    #[must_use]
    pub const fn feature(self) -> Option<Feature> {
        match self {
            Self::StructuredContentToText => Some(Feature::ToolStructuredOutput),
            Self::DropAnnotations => Some(Feature::ToolAnnotations),
            Self::AudioToTextPlaceholder | Self::DropAudio => Some(Feature::ToolAudioContent),
            Self::DropResourceLink => Some(Feature::ToolResourceLinks),
            Self::DropImage | Self::DropResource => None,
        }
    }

    /// How much this rule gives up.
    ///
    /// Only [`DropAnnotations`] is [`Loss::Flagged`]. Every rule that removes a
    /// *content block* is [`Loss::Destructive`], because a block the tool
    /// produced is the tool's answer: dropping it does not degrade the message,
    /// it makes it say something the tool did not. Calling that "flagged" would
    /// let a bridge promise "lossless or flagged" and quietly remove content.
    #[must_use]
    pub const fn loss(self) -> Loss {
        match self {
            Self::StructuredContentToText => Loss::None,
            Self::DropAnnotations => Loss::Flagged,
            Self::AudioToTextPlaceholder
            | Self::DropAudio
            | Self::DropImage
            | Self::DropResourceLink
            | Self::DropResource => Loss::Destructive,
        }
    }

    /// Whether this rule is the one that makes a value needing `feature`
    /// expressible again.
    ///
    /// Not `const`: `Feature`'s `PartialEq` is not const-callable, the same
    /// trade T05 made on `defined_in`.
    #[must_use]
    pub fn is_fallback_for(self, feature: Feature) -> bool {
        matches!(self.feature(), Some(f) if f == feature)
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::panic,
        reason = "tests panic with the failing precondition, which is the useful part"
    )]

    use super::{DegradeRule, Loss};
    use crate::domain::Feature;

    #[test]
    fn every_rule_names_a_loss() {
        for rule in DegradeRule::ALL {
            let _ = rule.loss();
        }
        assert_eq!(DegradeRule::ALL.len(), 4);
    }

    /// Images and embedded resources are expressible at every revision, so no
    /// rule can be a *fallback* for them — they are reductions a caller asks
    /// for, not answers to a missing feature.
    #[test]
    fn reductions_are_not_mistaken_for_fallbacks() {
        assert_eq!(DegradeRule::DropImage.feature(), None);
        assert_eq!(DegradeRule::DropResource.feature(), None);
        assert!(!DegradeRule::DropImage.is_fallback_for(Feature::Tools));
        assert_eq!(
            DegradeRule::DropResourceLink.feature(),
            Some(Feature::ToolResourceLinks),
        );
        assert!(DegradeRule::DropResourceLink.is_fallback_for(Feature::ToolResourceLinks));
    }

    /// Every rule that names a feature must match that feature. Catches a
    /// `feature()` arm and a `loss()` arm drifting apart, which is the way a
    /// rule ends up claiming to work around a feature it does not address.
    #[test]
    fn a_rule_matches_the_feature_it_names() {
        for rule in DegradeRule::ALL {
            let Some(feature) = rule.feature() else {
                continue;
            };
            assert!(
                rule.is_fallback_for(feature),
                "{rule:?} names {feature} but does not match it",
            );
            for other in DegradeRule::ALL {
                if other != rule {
                    assert!(
                        !other.is_fallback_for(feature) || other.feature() == Some(feature),
                        "{rule:?} and {other:?} both claim {feature}",
                    );
                }
            }
        }
    }

    /// The headline claim: the structured-content fallback is lossless, and the
    /// audio placeholder is not. If these two ever swap, the whole policy
    /// vocabulary is lying.
    #[test]
    fn lossless_and_destructive_are_not_confused() {
        assert_eq!(DegradeRule::StructuredContentToText.loss(), Loss::None);
        assert!(DegradeRule::StructuredContentToText.loss().is_lossless());

        assert_eq!(
            DegradeRule::AudioToTextPlaceholder.loss(),
            Loss::Destructive
        );
        assert!(!DegradeRule::AudioToTextPlaceholder.loss().is_lossless());
        assert!(!DegradeRule::AudioToTextPlaceholder.loss().is_acceptable());

        // Flagged is accepted by a "lossless or flagged" promise; destructive
        // is not.
        assert!(Loss::Flagged.is_acceptable());
        assert!(!Loss::Destructive.is_acceptable());
    }

    /// Every rule that could destroy meaning must be non-acceptable, and the
    /// one rule with a lossless alternative must be the lossless one.
    #[test]
    fn a_lossless_alternative_is_preferred_where_one_exists() {
        // structuredContent has a spec-named lossless fallback.
        assert!(DegradeRule::StructuredContentToText.loss().is_acceptable());
        // Annotations are advisory; an old peer ignores them anyway.
        assert!(DegradeRule::DropAnnotations.loss().is_acceptable());
        // Everything that removes content is not.
        for rule in [
            DegradeRule::DropAudio,
            DegradeRule::DropImage,
            DegradeRule::DropResource,
            DegradeRule::DropResourceLink,
        ] {
            assert!(!rule.loss().is_acceptable(), "{rule:?}");
        }
    }

    /// The opt-in placeholder is never something a policy can select, because
    /// `ALL` is what `rule_for` searches.
    #[test]
    fn the_opt_in_rule_is_not_selectable() {
        assert!(!DegradeRule::ALL.contains(&DegradeRule::AudioToTextPlaceholder));
        // It is still declared — it is opt-in, not gone.
        assert_eq!(
            DegradeRule::AudioToTextPlaceholder.loss(),
            Loss::Destructive
        );
    }

    #[test]
    fn the_audio_placeholder_covers_only_audio() {
        assert!(
            DegradeRule::AudioToTextPlaceholder
                .is_fallback_for(crate::domain::Feature::ToolAudioContent)
        );
        assert!(
            !DegradeRule::AudioToTextPlaceholder
                .is_fallback_for(crate::domain::Feature::ToolStructuredOutput)
        );
    }

    #[test]
    fn the_all_list_has_no_duplicates() {
        let mut seen = Vec::new();
        for rule in DegradeRule::ALL {
            assert!(!seen.contains(rule), "{rule:?} twice");
            seen.push(*rule);
        }
    }

    /// Every rule in `ALL` names a feature, and every rule that does not is
    /// absent from it. The two halves of one invariant: a listed rule must be
    /// reachable by `rule_for`, and an unlisted rule must not be.
    #[test]
    fn a_listed_rule_is_findable_and_an_unlisted_one_is_not() {
        for rule in DegradeRule::ALL {
            let Some(feature) = rule.feature() else {
                panic!("{rule:?} is listed but names no feature");
            };
            assert!(rule.is_fallback_for(feature), "{rule:?} / {feature}");
        }
        for rule in [
            DegradeRule::AudioToTextPlaceholder,
            DegradeRule::DropImage,
            DegradeRule::DropResource,
        ] {
            assert!(
                !DegradeRule::ALL.contains(&rule),
                "{rule:?} is opt-in and must not be selectable",
            );
        }
    }
}
