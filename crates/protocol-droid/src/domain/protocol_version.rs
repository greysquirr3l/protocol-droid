//! `ProtocolVersion` — the negotiated revision, as a value object.
//!
//! # Why `Unknown` carries a date rather than a string
//!
//! MCP identifies a revision by its release date and nothing else, so the
//! forward-compatible case is a well-formed date this build has not heard of.
//! Holding a bare `String` would mean an `Unknown` could be anything, including
//! a typo, and would make the comparison a lexicographic one that gets
//! `2024-11-05 < 2024-2-1` wrong. Holding a [`ReleaseDate`] means every
//! `ProtocolVersion` that exists is a real, ordered point in time.
//!
//! # The ordering invariant
//!
//! Both `Ord` and `PartialOrd` are derived, from the same variant order, which
//! is what the 1.98 derive fast path requires. For that to also be *correct*,
//! the declaration order has to be chronological — and it only is because of
//! one rule enforced in [`FromStr`]:
//!
//! > A well-formed date at or after the first revision that this build does not
//! > already know becomes `Unknown`; a well-formed date *before* the first
//! > revision is a parse error.
//!
//! That guarantees every `Unknown` sorts above every known variant, so
//! `V2024_11_05 < V2025_03_26 < … < V2026_07_28 < Unknown(_)` is true
//! chronologically as well as by declaration. Without the rule, a
//! `ProtocolVersion::Unknown` for `2020-01-01` would sort *after* `2026-07-28`
//! and silently corrupt `latest()` and `best_match`.
//!
//! The invariant is a construction-site property, not a type-level one: the
//! `Unknown` variant is public because callers need to match on it, so a future
//! edit could build one by hand. `tests::unknown_dates_sort_after_every_known_revision`
//! is what holds that down.

use core::fmt;
use core::str::FromStr;

use crate::domain::ReleaseDate;

use crate::domain::release_date::ParseReleaseDateError;

/// A published MCP revision, or one published after this build was compiled.
///
/// Ordered oldest to newest, which is also publication order. See the module
/// docs for why that is an invariant rather than a coincidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProtocolVersion {
    /// The first published revision. Defines stdio and the legacy HTTP+SSE
    /// transport, and is the only revision where [`TransportKind::LegacySse`]
    /// is defined at all.
    ///
    /// [`TransportKind::LegacySse`]: crate::domain::TransportKind::LegacySse
    V2024_11_05,
    /// Replaced the legacy HTTP+SSE transport with Streamable HTTP.
    V2025_03_26,
    /// Removed JSON-RPC batching and added structured tool output.
    V2025_06_18,
    /// Added URL-mode elicitation, icons, and experimental tasks.
    V2025_11_25,
    /// Made the protocol stateless: the `initialize` handshake is gone and every
    /// request carries its version in `_meta`.
    V2026_07_28,
    /// A revision published after this build, identified by its release date.
    ///
    /// Only ever holds a date at or after [`ReleaseDate::V2024_11_05`], and in
    /// practice only one newer than [`ProtocolVersion::V2026_07_28`] — see the
    /// module docs.
    Unknown(ReleaseDate),
}

impl ProtocolVersion {
    /// Every revision this build has wire types for, oldest first.
    ///
    /// `draft` is not a separate version: upstream's floating `draft` schema
    /// declares `LATEST_PROTOCOL_VERSION = "2026-07-28"`, the same as the final
    /// revision, so it negotiates as that revision. The two differ in schema
    /// shape, not in version string, and T08 keys the codec off the version.
    pub const KNOWN: [Self; 5] = [
        Self::V2024_11_05,
        Self::V2025_03_26,
        Self::V2025_06_18,
        Self::V2025_11_25,
        Self::V2026_07_28,
    ];

    /// The newest revision this build knows.
    #[must_use]
    pub const fn latest_known() -> Self {
        Self::V2026_07_28
    }

    /// The oldest revision this build supports.
    #[must_use]
    pub const fn earliest_known() -> Self {
        Self::V2024_11_05
    }

    /// The revision's release date.
    #[must_use]
    pub const fn date(self) -> ReleaseDate {
        match self {
            Self::V2024_11_05 => ReleaseDate::V2024_11_05,
            Self::V2025_03_26 => ReleaseDate::V2025_03_26,
            Self::V2025_06_18 => ReleaseDate::V2025_06_18,
            Self::V2025_11_25 => ReleaseDate::V2025_11_25,
            Self::V2026_07_28 => ReleaseDate::V2026_07_28,
            Self::Unknown(date) => date,
        }
    }

    /// The string this revision is negotiated as, borrowed when it is a known
    /// one.
    ///
    /// `Cow` rather than `&'static str` because [`Unknown`] has no static
    /// spelling; the six known revisions, which is what a codec asks about on
    /// the hot path, allocate nothing.
    ///
    /// [`Unknown`]: Self::Unknown
    #[must_use]
    pub fn as_wire(self) -> std::borrow::Cow<'static, str> {
        match self {
            Self::V2024_11_05 => "2024-11-05".into(),
            Self::V2025_03_26 => "2025-03-26".into(),
            Self::V2025_06_18 => "2025-06-18".into(),
            Self::V2025_11_25 => "2025-11-25".into(),
            Self::V2026_07_28 => "2026-07-28".into(),
            Self::Unknown(date) => date.to_string().into(),
        }
    }

    /// Whether this build has wire types for the revision.
    #[must_use]
    pub const fn is_known(self) -> bool {
        !matches!(self, Self::Unknown(_))
    }

    /// Whether this revision was published after the newest one this build
    /// knows.
    ///
    /// Not `const`: the comparison comes from a derived `Ord`, and a derived
    /// `Ord` is not a const-callable operator.
    #[must_use]
    pub fn is_newer_than_all_known(self) -> bool {
        self > Self::latest_known()
    }

    /// Builds an `Unknown` from a date, refusing one that would break the
    /// ordering invariant.
    ///
    /// The public enum variant means this cannot be the only way in, so the
    /// function is not private; making it the obvious one is what keeps the
    /// invariant from being broken by accident.
    ///
    /// # Errors
    ///
    /// Returns [`UnknownVersionError::NotAfterKnown`] when `date` is at or
    /// before [`Self::latest_known`] — a date this build should have a variant
    /// for, and [`FromStr`] would have matched it exactly.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::domain::{ProtocolVersion, ReleaseDate};
    ///
    /// let future = ProtocolVersion::unknown_after_known("2027-01-01".parse().unwrap())?;
    /// assert!(future.is_newer_than_all_known());
    /// # Ok::<(), protocol_droid::domain::UnknownVersionError>(())
    /// ```
    pub fn unknown_after_known(date: ReleaseDate) -> Result<Self, UnknownVersionError> {
        if date <= Self::latest_known().date() {
            return Err(UnknownVersionError::NotAfterKnown { date });
        }
        Ok(Self::Unknown(date))
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.as_wire())
    }
}

impl FromStr for ProtocolVersion {
    type Err = ParseProtocolVersionError;

    /// Parses a negotiated version string.
    ///
    /// Three outcomes, in order:
    ///
    /// 1. A known revision's string becomes that variant. Matched on the whole
    ///    string first, because that is the only way to reject `2025-03-26x`
    ///    without first having to date-parse it.
    /// 2. A well-formed date this build has not heard of becomes
    ///    [`Unknown`](Self::Unknown), provided it is at or after the first
    ///    revision.
    /// 3. Everything else is an error.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "2024-11-05" => return Ok(Self::V2024_11_05),
            "2025-03-26" => return Ok(Self::V2025_03_26),
            "2025-06-18" => return Ok(Self::V2025_06_18),
            "2025-11-25" => return Ok(Self::V2025_11_25),
            "2026-07-28" => return Ok(Self::V2026_07_28),
            _ => {}
        }
        let date = text.parse::<ReleaseDate>()?;
        if !date.is_at_or_after_first_revision() {
            return Err(ParseProtocolVersionError::ReleaseDate(
                ParseReleaseDateError::BeforeFirstRevision { date },
            ));
        }
        // Past this point `date` is strictly newer than every known revision, so
        // the derived ordering stays chronological. `unknown_after_known` is
        // the checked form of that claim and cannot fail here.
        Self::unknown_after_known(date)
            .map_err(ParseProtocolVersionError::UnknownVersion)
    }
}

/// Why a string is not a [`ProtocolVersion`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ParseProtocolVersionError {
    /// The string is not a well-formed release date.
    #[error(transparent)]
    ReleaseDate(#[from] ParseReleaseDateError),
    /// The date is well formed but is one this build should have a variant for.
    #[error("a date this build already knows cannot become an unknown revision: {0}")]
    UnknownVersion(#[from] UnknownVersionError),
}

/// Why a date cannot become [`ProtocolVersion::Unknown`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum UnknownVersionError {
    /// The date is at or before the newest known revision, so `Unknown` would
    /// sort at or below a known variant and break the ordering invariant.
    #[error("{date} is not after 2026-07-28, so it cannot be an unknown revision")]
    NotAfterKnown {
        /// The date that was offered.
        date: ReleaseDate,
    },
}

#[cfg(test)]
mod tests {
    // Test code unwraps deliberately: the strict profile exists to keep
    // production paths fallible, and a test that cannot unwrap cannot assert.
    // The messages name the failing precondition, which is the useful part.
    #![expect(
        clippy::unwrap_used,
        reason = "tests assert on fallible calls and panic with the failing precondition; propagating errors would replace a clear failure message with noise"
    )]

    use super::{ParseProtocolVersionError, ProtocolVersion, ReleaseDate, UnknownVersionError};
    use std::collections::BTreeSet;

    /// Every version the domain can produce, plus one `Unknown` per known
    /// revision so the exhaustive comparisons have something on both sides.
    fn corpus() -> Vec<ProtocolVersion> {
        let mut versions = ProtocolVersion::KNOWN.to_vec();
        versions.push(ProtocolVersion::Unknown("2027-01-01".parse().unwrap()));
        versions.push(ProtocolVersion::Unknown("2030-12-31".parse().unwrap()));
        versions
    }

    /// The 1.98 trap: `derive(PartialOrd)` has a fast path that assumes a
    /// hand-written `PartialOrd` agrees with a derived `Ord`. A pair that
    /// disagrees silently yields wrong comparisons instead of a compile error.
    /// Every ordered type in this crate derives both traits from the same
    /// variant order and copies this test when it is added.
    ///
    /// Exhaustive over every ordered pair, because "agrees in both directions"
    /// is a statement about the whole type and not about two samples of it.
    #[test]
    fn derived_ordering_agrees_in_both_directions() {
        for left in corpus() {
            for right in corpus() {
                let total = left.cmp(&right);
                assert_eq!(
                    left.partial_cmp(&right),
                    Some(total),
                    "cmp and partial_cmp disagree for {left} against {right}"
                );
                assert_eq!(left < right, total.is_lt());
                assert_eq!(left > right, total.is_gt());
                assert_eq!(left <= right, total.is_le());
                assert_eq!(left >= right, total.is_ge());
            }
        }
    }

    /// The invariant the module docs exist for: if `Unknown` could hold an old
    /// date, the derived ordering would silently become wrong. These are the
    /// cases that would break.
    #[test]
    fn unknown_dates_sort_after_every_known_revision() {
        let future = ProtocolVersion::Unknown("2027-01-01".parse().unwrap());
        for known in ProtocolVersion::KNOWN {
            assert!(known < future, "{known} must sort below an unknown revision");
            assert!(future > known, "{known} must sort below an unknown revision");
        }
    }

    #[test]
    fn declaration_order_is_chronological() {
        let sorted: BTreeSet<ProtocolVersion> = ProtocolVersion::KNOWN.into_iter().collect();
        let ascending: Vec<ProtocolVersion> = sorted.into_iter().collect();
        assert_eq!(ascending, ProtocolVersion::KNOWN.to_vec());
        assert_eq!(ProtocolVersion::earliest_known(), ProtocolVersion::V2024_11_05);
        assert_eq!(ProtocolVersion::latest_known(), ProtocolVersion::V2026_07_28);
    }

    #[test]
    fn parses_each_known_revision() {
        let cases = [
            ("2024-11-05", ProtocolVersion::V2024_11_05),
            ("2025-03-26", ProtocolVersion::V2025_03_26),
            ("2025-06-18", ProtocolVersion::V2025_06_18),
            ("2025-11-25", ProtocolVersion::V2025_11_25),
            ("2026-07-28", ProtocolVersion::V2026_07_28),
        ];
        for (text, expected) in cases {
            assert_eq!(text.parse::<ProtocolVersion>(), Ok(expected), "{text}");
        }
    }

    /// The forward-compatibility case, which is the reason `Unknown` exists.
    #[test]
    fn accepts_an_unknown_future_date() {
        let parsed = "2027-03-14".parse::<ProtocolVersion>().unwrap();
        assert_eq!(
            parsed,
            ProtocolVersion::Unknown("2027-03-14".parse::<ReleaseDate>().unwrap())
        );
        assert!(!parsed.is_known());
        assert!(parsed.is_newer_than_all_known());
        assert_eq!(parsed.as_wire(), "2027-03-14");
    }

    /// A well-formed date from before MCP existed is a different failure from
    /// a malformed one, and gets its own error variant.
    #[test]
    fn rejects_a_date_from_before_the_first_revision() {
        let error = "2020-01-01".parse::<ProtocolVersion>().unwrap_err();
        assert!(matches!(error, ParseProtocolVersionError::ReleaseDate(_)));
        assert!(error.to_string().contains("predates the first revision"));
    }

    #[test]
    fn rejects_malformed_strings() {
        for text in ["", "latest", "2025-3-26", "2025-03-26 ", " DRAFT-2025-v3"] {
            assert!(text.parse::<ProtocolVersion>().is_err(), "{text:?} parsed");
        }
    }

    /// Upstream's `2025-11-25` schema.ts declares
    /// `LATEST_PROTOCOL_VERSION = "DRAFT-2025-v3"` while its own tag is
    /// `2025-11-25` and every other revision's schema.ts carries its own date.
    /// The tag is what the protocol documents, so the date is what this accepts;
    /// the string is recorded in the README and in PROGRESS.md rather than
    /// being quietly accepted, because accepting it would make the bridge claim
    /// to speak a version the specification does not define.
    #[test]
    fn the_draft_string_upstream_ships_is_not_a_version() {
        assert!("DRAFT-2025-v3".parse::<ProtocolVersion>().is_err());
    }

    #[test]
    fn constructing_an_unknown_requires_a_later_date() {
        let unknown = ProtocolVersion::unknown_after_known;
        let error = unknown(ReleaseDate::V2025_06_18).unwrap_err();
        assert_eq!(error, UnknownVersionError::NotAfterKnown { date: ReleaseDate::V2025_06_18 });
        // Exactly the latest known revision is refused too, not just older ones.
        assert!(unknown(ReleaseDate::V2026_07_28).is_err());
        assert!(unknown("2026-07-29".parse().unwrap()).is_ok());
    }

    #[test]
    fn known_versions_never_report_as_unknown() {
        for version in ProtocolVersion::KNOWN {
            assert!(version.is_known());
            assert!(!version.is_newer_than_all_known());
            assert!(version <= ProtocolVersion::latest_known());
        }
    }

    #[test]
    fn round_trips_through_its_display_form() {
        for version in corpus() {
            assert_eq!(version.to_string().parse::<ProtocolVersion>(), Ok(version));
        }
    }

    /// The wire string is borrowed for known revisions and owned for unknown
    /// ones; both spellings are the same text, and the six known ones are the
    /// ones a codec asks about on the hot path.
    #[test]
    fn as_wire_borrows_when_it_can() {
        for version in ProtocolVersion::KNOWN {
            assert!(matches!(version.as_wire(), std::borrow::Cow::Borrowed(_)));
            assert_eq!(version.as_wire(), version.to_string());
        }
        let future = ProtocolVersion::Unknown("2027-03-14".parse().unwrap());
        assert!(matches!(future.as_wire(), std::borrow::Cow::Owned(_)));
        assert_eq!(future.as_wire(), "2027-03-14");
    }

    #[test]
    fn dates_match_the_variants() {
        assert_eq!(ProtocolVersion::V2024_11_05.date(), ReleaseDate::V2024_11_05);
        assert_eq!(ProtocolVersion::V2025_03_26.date(), ReleaseDate::V2025_03_26);
        assert_eq!(ProtocolVersion::V2025_06_18.date(), ReleaseDate::V2025_06_18);
        assert_eq!(ProtocolVersion::V2025_11_25.date(), ReleaseDate::V2025_11_25);
        assert_eq!(ProtocolVersion::V2026_07_28.date(), ReleaseDate::V2026_07_28);
    }
}
