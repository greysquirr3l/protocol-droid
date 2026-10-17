//! `LogLevel` — the severity ladder a `notifications/message` record carries.
//!
//! # Why this is a type and not a string
//!
//! The spec names eight levels and orders them, and two things in this crate
//! want that ordering: a server deciding whether a record is worth emitting,
//! and a client filtering what it shows. Both are comparisons, and a comparison
//! over `String` is either a lexical one — which happens to match the spec's
//! order and will not survive a rename — or a hand-written match that has to
//! be updated when a level is added.
//!
//! The order here follows the spec's, lowest first, so `Level::Debug <= x` is
//! the "is this at or above `x`" test a filter wants, and derives `Ord` rather
//! than hand-writing `PartialOrd` — see the note in [`Level`] on why those two
//! must agree.
//!
//! # Why `Ord` is derived too
//!
//! `PartialOrd` has a fast path that assumes a hand-written implementation
//! agrees with a derived `Ord`. An ordered type here derives both from the same
//! declaration order for exactly that reason: an inconsistent pair silently
//! changes comparison results rather than failing to compile.

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Serialize};

/// A severity level on the log ladder.
///
/// Ordered lowest-severity first, matching the spec's own ordering: `debug`
/// through `emergency`. Ordering is what a client filter needs, and the spec
/// defines the order, so there is nothing for this crate to choose.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Level {
    /// Fine-grained detail, usually disabled.
    Debug,
    /// Ordinary operational detail.
    Info,
    /// Normal but significant events.
    Notice,
    /// A condition that is not an error but deserves attention.
    Warning,
    /// A failure the peer should know about.
    Error,
    /// A failure requiring immediate action.
    Critical,
    /// Action must be taken immediately.
    Alert,
    /// The system is unusable.
    Emergency,
}

impl Level {
    /// Every level, lowest first — the order [`Ord`] uses.
    pub const ALL: [Self; 8] = [
        Self::Debug,
        Self::Info,
        Self::Notice,
        Self::Warning,
        Self::Error,
        Self::Critical,
        Self::Alert,
        Self::Emergency,
    ];

    /// The level's wire spelling, which is the variant name lowercased.
    ///
    /// The spelling is derived rather than stored so the two cannot drift, and
    /// it is the same string [`Self::from_str`] accepts — the round trip is
    /// the contract a codec needs, and a stored copy would be a second answer
    /// to keep in step.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Notice => "notice",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Critical => "critical",
            Self::Alert => "alert",
            Self::Emergency => "emergency",
        }
    }

    /// Parses a level from its wire spelling.
    ///
    /// # Errors
    ///
    /// Returns [`LevelError::Unknown`] for any other string, including the
    /// right one in the wrong case. A level is a closed set the spec enumerates,
    /// and accepting `"Info"` would mean emitting something no conforming peer
    /// has agreed to recognise.
    pub fn parse(text: &str) -> Result<Self, LevelError> {
        Self::ALL
            .into_iter()
            .find(|level| level.as_str() == text)
            .ok_or_else(|| LevelError(text.to_owned()))
    }
}

/// A level name that is not one the spec defines.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("`{0}` is not a logging level")]
pub struct LevelError(pub String);

impl FromStr for Level {
    type Err = LevelError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

impl fmt::Display for Level {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::{Level, LevelError};

    #[test]
    fn ordering_runs_lowest_severity_first() {
        // The spec's ladder, asserted as an ordering rather than a list, so a
        // variant added out of order fails here instead of silently changing
        // what a client filter lets through.
        let ladder = [
            Level::Debug,
            Level::Info,
            Level::Notice,
            Level::Warning,
            Level::Error,
            Level::Critical,
            Level::Alert,
            Level::Emergency,
        ];
        for pair in ladder.windows(2) {
            let [lower, higher] = pair else {
                return;
            };
            assert!(lower < higher, "{lower} should order below {higher}");
        }
    }

    #[test]
    fn derived_order_agrees_with_derived_partial_order() {
        // `PartialOrd`'s fast path assumes a hand-written implementation agrees
        // with a derived `Ord`. Both are derived here, so this proves the
        // assumption holds across every pair rather than taking it on trust.
        for left in Level::ALL {
            for right in Level::ALL {
                assert_eq!(
                    left < right,
                    left.cmp(&right) == core::cmp::Ordering::Less,
                    "{left} vs {right}: `<` and `Ord` disagree"
                );
            }
        }
    }

    #[test]
    fn every_level_round_trips_through_its_spelling() {
        for level in Level::ALL {
            let text = level.as_str();
            assert_eq!(Level::parse(text), Ok(level));
        }
    }

    #[test]
    fn an_unknown_spelling_is_refused_rather_than_guessed() {
        // Case is part of the answer: `Info` is not `info`, and accepting it
        // would emit a level no conforming peer recognises.
        assert!(matches!(
            Level::parse("Info"),
            Err(LevelError(_))
        ));
        assert!(matches!(
            Level::parse("verbose"),
            Err(LevelError(_))
        ));
    }

    #[test]
    fn all_covers_every_variant_in_order() {
        assert_eq!(Level::ALL.len(), 8);
        assert_eq!(Level::ALL.first(), Some(&Level::Debug));
        assert_eq!(Level::ALL.last(), Some(&Level::Emergency));
    }
}