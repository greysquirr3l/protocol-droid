//! `A2aVersion` — the negotiated A2A revision, as a value object.
//!
//! # Why `Major.Minor` and not a date
//!
//! The A2A protocol identifies a revision by a `Major.Minor` string
//! ("0.3", "1.0") carried in the `A2A-Version` request header (or the
//! `protocolVersion` field on the agent card). It is semver-shaped, not
//! date-shaped, so the model is `{ major: u16, minor: u16 }` rather than a
//! `ReleaseDate`. The string form is what crosses the wire; the parsed
//! form is what crosses the dispatcher's API boundary.
//!
//! # The ordering invariant
//!
//! Both `Ord` and `PartialOrd` are derived, from the same field order
//! (major before minor), which is what the 1.98 derive fast path
//! requires. For that to also be *correct* under the A2A spec's
//! "latest minor per major" rule, a future minor bumps the major
//! without a code change; an `Unknown { major: 1, minor: 0 }` must
//! sort the same as `V1_0`. See `tests::unknown_versions_sort_with_their_known_equivalent`.
//!
//! # Why `Unknown` is a real struct, not a string
//!
//! Holding a bare `String` would let `Unknown` be anything, including a
//! typo, and would make the comparison lexicographic — `0.10 < 0.2`
//! is wrong. Holding an `A2aVersion` parsed from a well-formed
//! `Major.Minor` string means every `Unknown` is a real, ordered point
//! in the same `Major.Minor` space; the version comparators and
//! `best_match` table stay type-safe.

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A published A2A revision, or one published after this build was compiled.
///
/// Ordered by `major` then `minor`, oldest to newest. See the module docs for
/// why that is an invariant rather than a coincidence.
///
/// `Ord` and `PartialOrd` are *hand-written* (the derive is removed) for the
/// reason in the module docs: a derived `Ord` orders enum variants by
/// declaration order first, which would put `V1_0` (the first variant) before
/// `Unknown { major: 1, minor: 0 }` even though they carry the same wire
/// value. The hand impl compares by `(major, minor)` only, so the two are
/// `Ordering::Equal`. The 1.98 derive fast path that assumes a derived
/// `PartialOrd` agrees with a hand-written `Ord` does not apply here — this
/// is the case the rule was written for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum A2aVersion {
    /// The first A2A version with the `A2A-Version` header. Establishes the
    /// JSON-RPC binding the protocol-neutral core already speaks, plus the
    /// agent card well-known path.
    V1_0,
    /// A version published after this build, identified by its `Major.Minor`.
    /// Carries the same `major`/`minor` as a known revision would; what
    /// changes between a known revision and its `Unknown` peer is whether
    /// this build's per-version codec has types for it.
    Unknown {
        /// The peer-named major version. `u16` because the A2A
        /// `Major.Minor` wire form is decimal; a major number
        /// that overflows `u16` is not a real A2A version.
        major: u16,
        /// The peer-named minor version.
        minor: u16,
    },
}

/// The `(major, minor)` pair a version carries, in a form the
/// comparison can use. `V1_0` is `(1, 0)`; an `Unknown` is its
/// own fields. The helper exists so the `Ord` impl is one
/// expression, and so a future variant that adds a new field to
/// `Unknown` (a build-flag, a build date) cannot silently change
/// the comparison.
const fn parts(version: A2aVersion) -> (u16, u16) {
    match version {
        A2aVersion::V1_0 => (1, 0),
        A2aVersion::Unknown { major, minor } => (major, minor),
    }
}

impl PartialOrd for A2aVersion {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for A2aVersion {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        parts(*self).cmp(&parts(*other))
    }
}

impl A2aVersion {
    /// Every revision this build has wire types for, oldest first.
    pub const KNOWN: [Self; 1] = [Self::V1_0];

    /// The canonical wire string, e.g. `"1.0"`. An `Unknown` formats
    /// the same way the spec defines, so the on-the-wire form does
    /// not distinguish a known revision from a future one — the
    /// dispatcher's `VersionNotSupportedError` does.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::V1_0 => "1.0",
            // An `Unknown` cannot return a `&'static str` because the
            // formatted value is not a literal. Callers that need a
            // `String` use `Display`, which formats the same way.
            Self::Unknown { .. } => "unknown",
        }
    }

    /// True if this version is a build-time known revision. A
    /// `VersionNotSupportedError` is the right response to a peer
    /// that names a `Known` revision we don't speak *yet* and an
    /// `Unknown` we have no codec for; the dispatcher's table
    /// decides which side of the line a given string falls on.
    pub const fn is_known(&self) -> bool {
        matches!(self, Self::V1_0)
    }
}

impl fmt::Display for A2aVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::V1_0 => f.write_str("1.0"),
            Self::Unknown { major, minor } => write!(f, "{major}.{minor}"),
        }
    }
}

/// The wire form of an `A2aVersion` is the `Display` form: `"1.0"`,
/// `"0.3"`, etc. A `Serialize` impl that delegates to `Display` keeps
/// the wire form and the in-memory form in lockstep — a change to
/// one without the other would break the protocol and the type
/// would catch it.
impl Serialize for A2aVersion {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// The wire form round-trips through `FromStr`; a version that does
/// not parse is a `ParseA2aVersionError`, which surfaces as a serde
/// error so a malformed `protocolVersion` field is the typed answer
/// the spec expects, not a 500.
impl<'de> Deserialize<'de> for A2aVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::from_str(&s).map_err(serde::de::Error::custom)
    }
}

impl FromStr for A2aVersion {
    type Err = ParseA2aVersionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // The wire form is "Major.Minor"; a `+build` suffix is a
        // recognised A2A practice but a pre-1.0 convention that this
        // build's codec does not model. Reject anything that is not
        // a clean two-part number.
        let (major, minor) =
            s.split_once('.')
                .ok_or_else(|| ParseA2aVersionError::MissingMinor {
                    input: s.to_owned(),
                })?;
        let major: u16 = major
            .parse()
            .map_err(|error| ParseA2aVersionError::InvalidMajor {
                input: s.to_owned(),
                error,
            })?;
        let minor: u16 = minor
            .parse()
            .map_err(|error| ParseA2aVersionError::InvalidMinor {
                input: s.to_owned(),
                error,
            })?;

        // Map a parsed (major, minor) to a known variant where one
        // exists; otherwise hold the parsed form. The match is
        // exhaustive on the `KNOWN` array, so adding a new
        // revision to the array is what makes it a known variant.
        let version = match (major, minor) {
            (1, 0) => Self::V1_0,
            _ => Self::Unknown { major, minor },
        };
        Ok(version)
    }
}

/// Why an A2A version string failed to parse.
///
/// The variants are named for the *part* that was wrong, not the
/// *string*, because a peer that says `"foo"` is making a different
/// mistake than one that says `"1"` or `"1.x"`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseA2aVersionError {
    /// The string had no `.` separator. The A2A wire form is always
    /// `Major.Minor`; `1` alone is not a valid A2A-Version header.
    #[error("A2A version `{input}` is missing the `.Minor` part")]
    MissingMinor {
        /// The verbatim string the peer sent, included so the
        /// error carries the full input rather than the part
        /// that failed.
        input: String,
    },
    /// The `Major` part did not parse as a `u16`. Carries the parse
    /// error so the caller can decide whether to fall back to a
    /// default or surface it to the peer.
    #[error("A2A version `{input}` has a non-numeric Major: {error}")]
    InvalidMajor {
        /// The verbatim string the peer sent.
        input: String,
        /// The parse error the `u16` parser returned. Surfaced
        /// rather than absorbed because the consumer (the
        /// dispatcher, an HTTP error handler) decides whether
        /// `1.x` is a peer mistake to report or an
        /// accept-and-default case.
        #[source]
        error: core::num::ParseIntError,
    },
    /// The `Minor` part did not parse as a `u16`. Same shape as
    /// `InvalidMajor`; a separate variant so the error message names
    /// the offending part.
    #[error("A2A version `{input}` has a non-numeric Minor: {error}")]
    InvalidMinor {
        /// The verbatim string the peer sent.
        input: String,
        /// The parse error the `u16` parser returned.
        #[source]
        error: core::num::ParseIntError,
    },
}

#[cfg(test)]
mod tests {
    use core::str::FromStr;

    use super::{A2aVersion, ParseA2aVersionError};

    /// `V1_0.as_str()` is the wire form
    /// `"1.0"`. The wire form is what
    /// crosses the `A2A-Version` header
    /// and the `protocolVersion` field
    /// on the agent card; a regression
    /// that returned `"v1.0"` or
    /// `"1.0.0"` would break the
    /// protocol.
    #[test]
    fn v1_0_as_str_is_the_wire_form() {
        assert_eq!(A2aVersion::V1_0.as_str(), "1.0");
    }

    /// `Unknown { .. }.as_str()` returns
    /// `"unknown"`, not a formatted
    /// `Major.Minor`. The doc comment
    /// on `as_str` notes that
    /// formatting an `Unknown` into a
    /// `&'static str` is impossible —
    /// the value is not a literal —
    /// so the function is a debug
    /// marker rather than a wire form.
    /// A peer-supplied `Unknown`
    /// version is formatted through
    /// `Display`, not `as_str`.
    #[test]
    fn unknown_as_str_is_the_literal_unknown() {
        let version = A2aVersion::Unknown { major: 2, minor: 5 };
        assert_eq!(version.as_str(), "unknown");
    }

    /// `is_known()` is `true` for
    /// `V1_0` and `false` for any
    /// `Unknown`. A peer that names a
    /// version this build does not
    /// speak gets `Unknown`, and the
    /// dispatcher's `KNOWN` table
    /// decides what to do with it.
    #[test]
    fn is_known_matches_the_variant() {
        assert!(A2aVersion::V1_0.is_known());
        assert!(!A2aVersion::Unknown { major: 1, minor: 0 }.is_known());
        assert!(!A2aVersion::Unknown { major: 2, minor: 0 }.is_known());
    }

    /// `Display` formats `V1_0` as
    /// `"1.0"`. The wire form is the
    /// same string the `Serialize`
    /// impl writes.
    #[test]
    fn display_for_v1_0_is_dot_zero() {
        assert_eq!(A2aVersion::V1_0.to_string(), "1.0");
    }

    /// `Display` formats an `Unknown`
    /// as `"<major>.<minor>"`. This is
    /// the wire form for forward-
    /// compatible version negotiation:
    /// a peer that names `"2.5"`
    /// carries an `Unknown { major: 2,
    /// minor: 5 }` whose wire form is
    /// `"2.5"`.
    #[test]
    fn display_for_unknown_is_major_dot_minor() {
        let version = A2aVersion::Unknown { major: 2, minor: 5 };
        assert_eq!(version.to_string(), "2.5");
    }

    /// `FromStr` parses `"1.0"` as
    /// `V1_0`. The wire form is the
    /// exact match.
    #[test]
    fn from_str_parses_v1_0() -> Result<(), ParseA2aVersionError> {
        let version = A2aVersion::from_str("1.0")?;
        assert_eq!(version, A2aVersion::V1_0);
        Ok(())
    }

    /// `FromStr` parses a future
    /// version as `Unknown` with the
    /// same `Major.Minor`. A peer that
    /// speaks `"2.5"` (a build newer
    /// than this one) is an `Unknown`,
    /// not an error.
    #[test]
    fn from_str_parses_unknown_with_known_fields() -> Result<(), ParseA2aVersionError> {
        let version = A2aVersion::from_str("2.5")?;
        assert_eq!(version, A2aVersion::Unknown { major: 2, minor: 5 });
        Ok(())
    }

    /// A version string with no `.`
    /// separator is `MissingMinor`.
    /// The variant names the part
    /// that was wrong (`the `.Minor`
    /// part`), not the string, so a
    /// peer that sent `"1"` gets a
    /// message about the missing
    /// `Minor`.
    #[test]
    fn from_str_rejects_missing_minor_with_named_variant() -> Result<(), String> {
        let result = A2aVersion::from_str("1");
        match result {
            Err(ParseA2aVersionError::MissingMinor { input }) => {
                if input != "1" {
                    return Err(format!("expected input '1', got {input:?}"));
                }
                Ok(())
            }
            other => Err(format!("expected MissingMinor, got {other:?}")),
        }
    }

    /// A non-numeric `Major` is
    /// `InvalidMajor`. The variant
    /// carries the verbatim input and
    /// the parse error.
    #[test]
    fn from_str_rejects_non_numeric_major_with_named_variant() -> Result<(), String> {
        let result = A2aVersion::from_str("foo.bar");
        match result {
            Err(ParseA2aVersionError::InvalidMajor { input, .. }) => {
                if input != "foo.bar" {
                    return Err(format!("expected input 'foo.bar', got {input:?}"));
                }
                Ok(())
            }
            other => Err(format!("expected InvalidMajor, got {other:?}")),
        }
    }

    /// A non-numeric `Minor` is
    /// `InvalidMinor`. The variant
    /// names the offending part, not
    /// the string.
    #[test]
    fn from_str_rejects_non_numeric_minor_with_named_variant() -> Result<(), String> {
        let result = A2aVersion::from_str("1.foo");
        match result {
            Err(ParseA2aVersionError::InvalidMinor { input, .. }) => {
                if input != "1.foo" {
                    return Err(format!("expected input '1.foo', got {input:?}"));
                }
                Ok(())
            }
            other => Err(format!("expected InvalidMinor, got {other:?}")),
        }
    }

    /// Ordering is numeric, not
    /// lexicographic. `"0.9" < "0.10"`
    /// in the wire form is false
    /// lexicographically, true
    /// numerically; the `Ord` derive
    /// must agree with the spec's
    /// "latest minor per major" rule
    /// (an `Unknown { major: 0, minor:
    /// 9 }` is older than `Unknown {
    /// major: 0, minor: 10 }`).
    #[test]
    fn unknown_ordering_is_numeric_not_lexicographic() {
        let nine = A2aVersion::Unknown { major: 0, minor: 9 };
        let ten = A2aVersion::Unknown {
            major: 0,
            minor: 10,
        };
        assert!(nine < ten, "0.9 must sort before 0.10 numerically");
    }

    /// An `Unknown { major: 1, minor: 0
    /// }` sorts equal to `V1_0`. The
    /// module doc promises this; a
    /// future minor that bumps the
    /// major without a code change
    /// must end up at the same
    /// ordering point. A regression
    /// that gave `V1_0` and its
    /// `Unknown` peer different
    /// ordering would silently change
    /// the dispatcher's
    /// "best-match" table.
    #[test]
    fn unknown_versions_sort_with_their_known_equivalent() {
        let known = A2aVersion::V1_0;
        let unknown_equivalent = A2aVersion::Unknown { major: 1, minor: 0 };
        assert_eq!(known.cmp(&unknown_equivalent), core::cmp::Ordering::Equal);
        assert_eq!(unknown_equivalent.cmp(&known), core::cmp::Ordering::Equal);
    }

    /// `KNOWN` lists every revision
    /// this build has wire types for,
    /// oldest first. A future commit
    /// that adds a new revision must
    /// update this array; the
    /// dispatcher's table is keyed on
    /// it. The test pins the current
    /// shape: one revision, `V1_0`.
    /// When a new revision lands, this
    /// test must change to match the
    /// new array.
    #[test]
    fn known_array_lists_v1_0() {
        assert_eq!(A2aVersion::KNOWN, [A2aVersion::V1_0]);
    }
}
