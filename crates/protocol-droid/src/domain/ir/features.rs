//! `FeatureSet` — what an IR value *needs*, as distinct from what a session
//! *has*.
//!
//! # Why this is not `FeatureProfile`
//!
//! [`FeatureProfile`](crate::domain::FeatureProfile) answers "what may this
//! session do", and it is a consequence of negotiation: a version, two
//! capability sets and a transport. A `FeatureSet` answers "what does this
//! value require in order to be expressed", and it is a property of the value
//! alone, with no session in sight.
//!
//! They share a bit encoding — both are subsets of [`Feature::ALL`] — so
//! comparing them is a mask test, and that is the whole reason they stay
//! separate types. Conflating them would make
//! `session.profile.allows(..)` and `value.required_features().contains(..)`
//! the same call, and a reader could not tell which question was being asked.
//!
//! The one place they meet is T08's lowering, where the check reads
//! `required.is_subset_of(&profile_as_set)`. Keeping them distinct makes that
//! line say what it is.

use crate::domain::Feature;

/// A set of features.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct FeatureSet {
    bits: u128,
}

impl FeatureSet {
    /// No features. A value that needs nothing is one every revision can
    /// express.
    #[must_use]
    pub const fn empty() -> Self {
        Self { bits: 0 }
    }

    /// Adds a feature.
    #[must_use]
    pub fn with(self, feature: Feature) -> Self {
        Self {
            bits: self.bits | bit_of(feature),
        }
    }

    /// Adds a feature in place.
    pub fn insert(&mut self, feature: Feature) {
        self.bits |= bit_of(feature);
    }

    /// Whether the feature is in the set.
    #[must_use]
    pub fn contains(self, feature: Feature) -> bool {
        self.bits & bit_of(feature) != 0
    }

    /// Whether the set needs nothing.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.bits == 0
    }

    /// How many features the set holds.
    #[must_use]
    pub const fn len(self) -> u32 {
        self.bits.count_ones()
    }

    /// The union of two sets.
    ///
    /// This is the workhorse: a result with five content blocks needs the union
    /// of what each block needs, and a result with a content array plus
    /// `structuredContent` needs the union of both.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self {
            bits: self.bits | other.bits,
        }
    }

    /// The union of everything in `features`.
    #[must_use]
    pub fn union_of(features: impl IntoIterator<Item = Feature>) -> Self {
        features.into_iter().fold(Self::empty(), Self::with)
    }

    /// Whether every feature in `self` is also in `other`.
    ///
    /// This is the lowering gate: "can the session express this value".
    #[must_use]
    pub const fn is_subset_of(self, other: Self) -> bool {
        self.bits & !other.bits == 0
    }

    /// The features in the set, in declaration order.
    pub fn iter(self) -> impl Iterator<Item = Feature> {
        Feature::ALL.into_iter().filter(move |f| self.contains(*f))
    }
}

/// The bit a feature occupies, found by name.
///
/// The name is the identity rather than a hand-assigned number, so inserting a
/// variant into [`Feature::ALL`] cannot leave a stale bit behind in a second
/// place. Not `const`: `slice::get` is not const-callable on 1.98 and
/// `indexing_slicing` is denied, so the two cannot both be satisfied — T05 hit
/// this on `FeatureProfile` and made the same call.
fn bit_of(feature: Feature) -> u128 {
    let name = feature.as_str();
    let mut index = 0;
    while let Some(candidate) = Feature::ALL.get(index) {
        if candidate.as_str() == name {
            return 1u128 << index;
        }
        index += 1;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::FeatureSet;
    use crate::domain::Feature;

    #[test]
    fn an_empty_set_needs_nothing() {
        let empty = FeatureSet::empty();
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);
        assert!(!empty.contains(Feature::Tools));
    }

    #[test]
    fn a_single_feature_set_contains_exactly_that() {
        let set = FeatureSet::empty().with(Feature::Tools);
        assert!(set.contains(Feature::Tools));
        assert!(!set.contains(Feature::Prompts));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn union_merges_and_deduplicates() {
        let a = FeatureSet::empty()
            .with(Feature::Tools)
            .with(Feature::Prompts);
        let b = FeatureSet::empty()
            .with(Feature::Prompts)
            .with(Feature::Resources);
        let union = a.union(b);
        assert_eq!(union.len(), 3);
        for feature in [Feature::Tools, Feature::Prompts, Feature::Resources] {
            assert!(union.contains(feature), "{feature}");
        }
    }

    #[test]
    fn union_of_iterates() {
        let set = FeatureSet::union_of([Feature::Tools, Feature::Prompts]);
        assert_eq!(set.len(), 2);
        assert!(FeatureSet::union_of([]).is_empty());
    }

    /// The direction matters, and this is the direction the lowering gate uses:
    /// "does the session have everything this value needs".
    #[test]
    fn subset_reflects_inclusion() {
        let both = FeatureSet::empty()
            .with(Feature::Tools)
            .with(Feature::Prompts);
        let one = FeatureSet::empty().with(Feature::Tools);

        assert!(one.is_subset_of(both));
        assert!(both.is_subset_of(both));
        // An empty set is a subset of everything, including another empty one.
        assert!(FeatureSet::empty().is_subset_of(both));
        assert!(FeatureSet::empty().is_subset_of(one));
        assert!(!both.is_subset_of(one));
    }

    #[test]
    fn iter_yields_declaration_order() {
        let set = FeatureSet::empty()
            .with(Feature::Extensions)
            .with(Feature::Tools);
        let yielded: Vec<Feature> = set.iter().collect();
        assert_eq!(yielded, vec![Feature::Tools, Feature::Extensions]);
    }

    #[test]
    fn every_feature_gets_a_distinct_bit() {
        let mut seen = Vec::new();
        for feature in Feature::ALL {
            let set = FeatureSet::empty().with(feature);
            assert_eq!(set.len(), 1, "{feature} collided with another bit");
            assert!(set.contains(feature), "{feature} is missing");
            assert!(!seen.contains(&feature), "{feature} twice");
            seen.push(feature);
        }
        assert_eq!(seen.len(), Feature::ALL.len());
    }
}
