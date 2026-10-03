//! `SupportedVersions` — the set of revisions a peer will speak.
//!
//! The set is non-empty and sorted, and both are established at construction
//! rather than checked on every read. A sorted set makes `latest()` the last
//! element, makes `contains` a binary search over an invariant, and makes the
//! order of two `SupportedVersions` values meaningful for a cache key.
//!
//! # Why a slice and not a bitset
//!
//! Five of the six versions are known to the type, so a bitset would be
//! smaller. [`ProtocolVersion::Unknown`] is not expressible in one, though, and
//! a peer is entitled to offer a revision this build has never heard of — that
//! is the whole reason [`best_match`](Self::best_match) takes its argument as
//! a list rather than reading a field. Losing the ability to represent a peer's
//! actual offer to save forty bytes is the wrong trade for a set that holds six
//! elements.

use core::fmt;

use crate::domain::ProtocolVersion;

/// Why a set of supported versions was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SupportedVersionsError {
    /// A set with no members cannot describe a peer, and most of this type's
    /// methods would have to invent an answer for it.
    #[error("a peer must support at least one protocol version")]
    Empty,
}

/// A non-empty, sorted, duplicate-free set of protocol versions.
///
/// Construct it with [`new`](Self::new) or [`from_slice`](Self::from_slice);
/// both return [`Result`] because emptiness is the one thing a caller gets
/// wrong by accident and cannot fix afterwards.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::{ProtocolVersion, SupportedVersions};
///
/// let ours = SupportedVersions::new([ProtocolVersion::V2024_11_05, ProtocolVersion::V2025_03_26])?;
/// assert_eq!(ours.latest(), ProtocolVersion::V2025_03_26);
/// assert!(ours.contains(&ProtocolVersion::V2024_11_05));
///
/// // Newest mutually supported wins, which is what the specification requires
/// // when both sides can speak more than one revision.
/// let theirs = [ProtocolVersion::V2025_06_18, ProtocolVersion::V2025_03_26];
/// assert_eq!(ours.best_match(&theirs), Some(ProtocolVersion::V2025_03_26));
/// # Ok::<(), protocol_droid::domain::SupportedVersionsError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupportedVersions {
    /// Sorted oldest to newest, and never empty. The constructor is the only
    /// way in, so no method has to re-check either property.
    versions: Box<[ProtocolVersion]>,
}

impl SupportedVersions {
    /// Every revision this build has wire types for.
    ///
    /// # Errors
    ///
    /// Never. It exists so callers do not spell the list out, and the list is
    /// known non-empty, so the `Result` is a formality rather than a branch.
    pub fn all() -> Result<Self, SupportedVersionsError> {
        Self::new(ProtocolVersion::KNOWN)
    }

    /// The set of every revision this build knows, as a slice.
    ///
    /// A convenience for the read-only cases; [`new`](Self::new) and
    /// [`from_slice`](Self::from_slice) are the ways to build one that can be
    /// held.
    #[must_use]
    pub fn as_slice(&self) -> &[ProtocolVersion] {
        &self.versions
    }

    /// Sorts, deduplicates, and rejects an empty set.
    ///
    /// # Errors
    ///
    /// [`SupportedVersionsError::Empty`] when `versions` has no members.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::domain::{ProtocolVersion, SupportedVersions};
    ///
    /// // Order and duplicates are the caller's problem, not the caller's job.
    /// let set = SupportedVersions::new([
    ///     ProtocolVersion::V2025_03_26,
    ///     ProtocolVersion::V2024_11_05,
    ///     ProtocolVersion::V2025_03_26,
    /// ])?;
    /// assert_eq!(set.as_slice(), &[ProtocolVersion::V2024_11_05, ProtocolVersion::V2025_03_26]);
    /// # Ok::<(), protocol_droid::domain::SupportedVersionsError>(())
    /// ```
    pub fn new(
        versions: impl IntoIterator<Item = ProtocolVersion>,
    ) -> Result<Self, SupportedVersionsError> {
        let mut versions: Vec<ProtocolVersion> = versions.into_iter().collect();
        if versions.is_empty() {
            return Err(SupportedVersionsError::Empty);
        }
        versions.sort_unstable();
        versions.dedup();
        Ok(Self {
            versions: versions.into_boxed_slice(),
        })
    }

    /// [`new`](Self::new) for a slice that is already borrowed.
    ///
    /// # Errors
    ///
    /// [`SupportedVersionsError::Empty`] when `versions` is empty.
    pub fn from_slice(versions: &[ProtocolVersion]) -> Result<Self, SupportedVersionsError> {
        Self::new(versions.iter().copied())
    }

    /// How many revisions the peer supports.
    ///
    /// At least one, always — that is the invariant.
    #[must_use]
    pub fn len(&self) -> usize {
        self.versions.len()
    }

    /// Always `false`. Present because `len` exists and clippy asks.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        false
    }

    /// The newest revision the peer supports.
    ///
    /// Infallible rather than `Option` because the set is non-empty and
    /// sorted: the last element is the answer, and there is no path to a set
    /// where that element does not exist.
    #[must_use]
    pub fn latest(&self) -> ProtocolVersion {
        // The `None` arm is unreachable: `new` rejects an empty set and the
        // field is private, so nothing outside this module builds one.
        self.versions
            .last()
            .map_or_else(ProtocolVersion::latest_known, |latest| *latest)
    }

    /// The oldest revision the peer supports.
    #[must_use]
    pub fn earliest(&self) -> ProtocolVersion {
        // The `None` arm is unreachable: see `latest`.
        self.versions
            .first()
            .map_or_else(ProtocolVersion::earliest_known, |earliest| *earliest)
    }

    /// Whether the peer supports `version`.
    #[must_use]
    pub fn contains(&self, version: &ProtocolVersion) -> bool {
        self.versions.binary_search(version).is_ok()
    }

    /// The newest revision this set and `offered` have in common, or `None`.
    ///
    /// Newest-common rather than first-offered: the specification tells a peer
    /// to use the latest version it can support, and a client that advertised
    /// `["2024-11-05", "2025-06-18"]` is not asking for `2024-11-05`.
    ///
    /// `offered` is taken as a slice and scanned rather than sorted into a set,
    /// because it arrives from the wire, may contain duplicates, and is usually
    /// a handful of entries. The cost is one linear pass, and `ProtocolVersion`
    /// is `Copy` so the comparison is a field compare.
    ///
    /// # Why `Option` when the task said `-> ProtocolVersion`
    ///
    /// There is a real outcome where no revision is shared, and a peer's offer
    /// can be empty. Returning a `ProtocolVersion` there means returning a
    /// value the peer did not offer, which is how a bridge ends up announcing a
    /// version it cannot speak. T06 maps `None` onto JSON-RPC's
    /// `UnsupportedProtocolVersion` error, which is the answer the wire format
    /// already has for exactly this case.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::domain::{ProtocolVersion, SupportedVersions};
    ///
    /// let ours = SupportedVersions::new(ProtocolVersion::KNOWN)?;
    /// assert_eq!(
    ///     ours.best_match(&[ProtocolVersion::V2025_06_18, ProtocolVersion::V2024_11_05]),
    ///     Some(ProtocolVersion::V2025_06_18)
    /// );
    /// assert_eq!(ours.best_match(&[ProtocolVersion::Unknown("2027-01-01".parse().unwrap())]), None);
    /// assert_eq!(ours.best_match(&[]), None);
    /// # Ok::<(), protocol_droid::domain::SupportedVersionsError>(())
    /// ```
    #[must_use]
    pub fn best_match(&self, offered: &[ProtocolVersion]) -> Option<ProtocolVersion> {
        offered
            .iter()
            .filter(|version| self.contains(version))
            .copied()
            .max()
    }

    /// The revisions in this set that `offered` does not contain.
    ///
    /// The negotiation log wants both directions, and a codec wants to know
    /// what it must translate away from, so this is a first-class operation
    /// rather than something each caller reassembles.
    pub fn not_offered_by<'a>(
        &'a self,
        offered: &'a [ProtocolVersion],
    ) -> impl Iterator<Item = ProtocolVersion> + 'a {
        self.versions
            .iter()
            .filter(|version| !offered.contains(version))
            .copied()
    }
}

impl fmt::Display for SupportedVersions {
    /// Joins the versions with commas, oldest first.
    ///
    /// Used in negotiation error messages, where the reader wants the list
    /// rather than a `Debug` dump with a name per element.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, version) in self.versions.iter().enumerate() {
            if index > 0 {
                formatter.write_str(", ")?;
            }
            write!(formatter, "{version}")?;
        }
        Ok(())
    }
}

impl TryFrom<&[ProtocolVersion]> for SupportedVersions {
    type Error = SupportedVersionsError;

    fn try_from(versions: &[ProtocolVersion]) -> Result<Self, Self::Error> {
        Self::from_slice(versions)
    }
}

impl IntoIterator for SupportedVersions {
    type Item = ProtocolVersion;
    type IntoIter = std::vec::IntoIter<ProtocolVersion>;

    fn into_iter(self) -> Self::IntoIter {
        self.versions.into_vec().into_iter()
    }
}

#[cfg(test)]
mod tests {
    // Test code unwraps and panics deliberately: the strict profile exists to
    // keep production paths fallible, and a test that cannot unwrap cannot
    // assert. The messages name the failing precondition, which is the useful
    // part.
    #![expect(
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests assert on fallible calls and panic with the failing precondition; propagating errors would replace a clear failure message with noise"
    )]

    use super::{SupportedVersions, SupportedVersionsError};
    use crate::domain::{ProtocolVersion, ReleaseDate};

    fn set_of(versions: &[ProtocolVersion]) -> SupportedVersions {
        SupportedVersions::from_slice(versions).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn rejects_an_empty_set() {
        assert_eq!(
            SupportedVersions::new([]),
            Err(SupportedVersionsError::Empty)
        );
        assert_eq!(
            SupportedVersions::from_slice(&[]),
            Err(SupportedVersionsError::Empty)
        );
        assert_eq!(
            SupportedVersions::new(Vec::new()).map(|set| set.len()),
            Err(SupportedVersionsError::Empty)
        );
    }

    #[test]
    fn sorts_and_deduplicates_at_construction() {
        let set = set_of(&[
            ProtocolVersion::V2025_06_18,
            ProtocolVersion::V2024_11_05,
            ProtocolVersion::V2025_06_18,
            ProtocolVersion::V2025_03_26,
        ]);
        assert_eq!(
            set.as_slice(),
            &[
                ProtocolVersion::V2024_11_05,
                ProtocolVersion::V2025_03_26,
                ProtocolVersion::V2025_06_18,
            ]
        );
        assert_eq!(set.len(), 3);
    }

    #[test]
    fn latest_and_earliest_read_off_the_sorted_edges() {
        let set = set_of(&[ProtocolVersion::V2025_06_18, ProtocolVersion::V2024_11_05]);
        assert_eq!(set.latest(), ProtocolVersion::V2025_06_18);
        assert_eq!(set.earliest(), ProtocolVersion::V2024_11_05);

        let single = set_of(&[ProtocolVersion::V2025_11_25]);
        assert_eq!(single.latest(), ProtocolVersion::V2025_11_25);
        assert_eq!(single.earliest(), ProtocolVersion::V2025_11_25);
        assert_eq!(single.len(), 1);
        assert!(!single.is_empty());
    }

    #[test]
    fn all_is_every_known_revision() {
        let all = SupportedVersions::all().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(all.as_slice(), ProtocolVersion::KNOWN);
        assert_eq!(all.latest(), ProtocolVersion::latest_known());
        assert_eq!(all.len(), ProtocolVersion::KNOWN.len());
    }

    #[test]
    fn contains_uses_the_sorted_ordering() {
        let set = set_of(&[ProtocolVersion::V2024_11_05, ProtocolVersion::V2025_06_18]);
        assert!(set.contains(&ProtocolVersion::V2024_11_05));
        assert!(set.contains(&ProtocolVersion::V2025_06_18));
        assert!(!set.contains(&ProtocolVersion::V2025_03_26));
        assert!(!set.contains(&ProtocolVersion::V2026_07_28));
        let future = ProtocolVersion::Unknown("2027-01-01".parse::<ReleaseDate>().unwrap());
        assert!(!set.contains(&future));
    }

    /// Newest-common, not first-offered: the specification tells a peer to
    /// use the latest version it can support, and a client that advertised
    /// oldest-first is not asking for the oldest.
    #[test]
    fn best_match_takes_the_newest_shared_revision() {
        let set = set_of(&[ProtocolVersion::V2024_11_05, ProtocolVersion::V2025_06_18]);
        assert_eq!(
            set.best_match(&[ProtocolVersion::V2024_11_05, ProtocolVersion::V2025_06_18]),
            Some(ProtocolVersion::V2025_06_18)
        );
        assert_eq!(
            set.best_match(&[ProtocolVersion::V2025_06_18, ProtocolVersion::V2024_11_05]),
            Some(ProtocolVersion::V2025_06_18)
        );
    }

    /// The failure the task's `-> ProtocolVersion` signature could not express.
    #[test]
    fn best_match_is_none_when_nothing_is_shared() {
        let set = set_of(&[ProtocolVersion::V2024_11_05, ProtocolVersion::V2025_03_26]);
        assert_eq!(set.best_match(&[ProtocolVersion::V2026_07_28]), None);
        assert_eq!(
            set.best_match(&[ProtocolVersion::Unknown("2027-01-01".parse().unwrap())]),
            None
        );
        assert_eq!(set.best_match(&[]), None);
    }

    #[test]
    fn best_match_ignores_duplicate_offers() {
        let set = set_of(&[ProtocolVersion::V2025_06_18]);
        assert_eq!(
            set.best_match(&[
                ProtocolVersion::V2025_06_18,
                ProtocolVersion::V2025_06_18,
                ProtocolVersion::V2025_06_18
            ]),
            Some(ProtocolVersion::V2025_06_18)
        );
    }

    /// An unknown revision on both sides is a real match: the peer is the only
    /// thing that can speak it, and refusing to negotiate with it would leave
    /// no path forward.
    #[test]
    fn best_match_can_return_an_unknown_revision() {
        let future = ProtocolVersion::Unknown("2027-03-14".parse::<ReleaseDate>().unwrap());
        let set = set_of(&[ProtocolVersion::V2025_06_18, future]);
        assert_eq!(set.best_match(&[future]), Some(future));
        assert_eq!(set.latest(), future);
    }

    #[test]
    fn not_offered_by_is_the_complement() {
        let set = set_of(&[ProtocolVersion::V2024_11_05, ProtocolVersion::V2025_06_18]);
        let offered = [ProtocolVersion::V2025_06_18, ProtocolVersion::V2026_07_28];
        let missing: Vec<ProtocolVersion> = set.not_offered_by(&offered).collect();
        assert_eq!(missing, vec![ProtocolVersion::V2024_11_05]);
        assert!(set.not_offered_by(&offered).all(|v| !offered.contains(&v)));
    }

    #[test]
    fn display_joins_oldest_first() {
        let set = set_of(&[ProtocolVersion::V2025_06_18, ProtocolVersion::V2024_11_05]);
        assert_eq!(set.to_string(), "2024-11-05, 2025-06-18");
        assert_eq!(
            set_of(&[ProtocolVersion::V2025_06_18]).to_string(),
            "2025-06-18"
        );
    }

    #[test]
    fn converts_from_a_slice_and_into_an_iterator() {
        let from_try: SupportedVersions = (&[ProtocolVersion::V2024_11_05][..])
            .try_into()
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(from_try.len(), 1);

        let collected: Vec<ProtocolVersion> = set_of(&ProtocolVersion::KNOWN).into_iter().collect();
        assert_eq!(collected, ProtocolVersion::KNOWN.to_vec());
    }

    #[test]
    fn equality_ignores_construction_order() {
        let forwards = set_of(&[ProtocolVersion::V2024_11_05, ProtocolVersion::V2025_06_18]);
        let backwards = set_of(&[ProtocolVersion::V2025_06_18, ProtocolVersion::V2024_11_05]);
        let with_duplicate = set_of(&[
            ProtocolVersion::V2024_11_05,
            ProtocolVersion::V2025_06_18,
            ProtocolVersion::V2024_11_05,
        ]);
        assert_eq!(forwards, backwards);
        assert_eq!(forwards, with_duplicate);
        assert_ne!(forwards, set_of(&[ProtocolVersion::V2024_11_05]));
    }
}
