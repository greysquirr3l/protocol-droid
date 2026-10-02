//! Domain layer: value objects, aggregates and pure functions.
//!
//! This layer has zero I/O dependencies. Time, randomness and session-id
//! generation are injected from outside it rather than called globally, so
//! every function here is deterministic and testable.
//!
//! Populated by T04 (protocol version value objects) and T05-T08 (feature
//! profile, negotiation, canonical IR, per-revision codecs).

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    /// Stand-in for the ordered enums this module gains from T04 onwards.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    enum RevisionOrder {
        /// The oldest revision the project supports.
        Earliest,
        /// A revision published after it.
        Latest,
    }

    /// Guards the 1.98 trap: `derive(PartialOrd)` has a fast path that assumes
    /// a hand-written `PartialOrd` agrees with a derived `Ord`. A pair that
    /// disagrees silently yields wrong comparisons instead of a compile error.
    /// Every ordered type in this crate derives both traits from the same
    /// variant order and copies this test when it is added.
    #[test]
    fn derived_ordering_agrees_in_both_directions() {
        assert_eq!(
            RevisionOrder::Earliest.cmp(&RevisionOrder::Latest),
            Ordering::Less,
        );
        assert_eq!(
            RevisionOrder::Latest.cmp(&RevisionOrder::Earliest),
            Ordering::Greater,
        );
        assert_eq!(
            RevisionOrder::Earliest.cmp(&RevisionOrder::Earliest),
            Ordering::Equal,
        );
    }

    #[test]
    fn derived_partial_ordering_agrees_with_derived_ordering() {
        assert!(RevisionOrder::Earliest < RevisionOrder::Latest);
        assert!(RevisionOrder::Latest > RevisionOrder::Earliest);
        assert!(RevisionOrder::Earliest <= RevisionOrder::Earliest);
        assert!(RevisionOrder::Latest >= RevisionOrder::Latest);
    }
}
