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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
        let (major, minor) = s.split_once('.').ok_or_else(|| {
            ParseA2aVersionError::MissingMinor {
                input: s.to_owned(),
            }
        })?;
        let major: u16 = major.parse().map_err(|error| {
            ParseA2aVersionError::InvalidMajor {
                input: s.to_owned(),
                error,
            }
        })?;
        let minor: u16 = minor.parse().map_err(|error| {
            ParseA2aVersionError::InvalidMinor {
                input: s.to_owned(),
                error,
            }
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
