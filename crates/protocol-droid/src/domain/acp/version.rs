//! `AcpVersion` — the pinned ACP release, as a value object.
//!
//! # Why a major/minor/patch triple
//!
//! ACP release tags are `schema-v<major>.<minor>.<patch>` (`schema-v1.25.0`).
//! The release tag is what the verifier pins; the parsed `AcpVersion`
//! is what crosses the dispatcher's API boundary. A `u16`-per-field
//! representation is decimal-clean, the same shape the wire never
//! carries (the wire does not pass a version) but the same shape
//! a comparable type like [`crate::domain::mcp::ProtocolVersion`]
//! uses.
//!
//! # The `Unknown` variant
//!
//! A future ACP release that is published after this build is
//! compiled shows up as `Unknown { major, minor, patch }`. The
//! three-tuple form means `Unknown` orders the same as a known
//! release with the same numbers would, and `serde_json` round-
//! trips the parsed form losslessly into a debug log. A peer
//! that announces a release older than the pin (e.g.
//! `schema-v1.0.0`) deserialises into the same shape the build
//! knows — the consumer's `FeatureProfile` is what gates
//! feature-specific code, not the version itself.
//!
//! # The ordering invariant
//!
//! `Ord` and `PartialOrd` are *hand-written*, comparing by
//! `(major, minor, patch)` in order. The 1.98 derive fast path
//! that assumes a derived `PartialOrd` agrees with a hand-written
//! `Ord` does not apply here; this is the case the rule was
//! written for.

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A pinned ACP release, or a future release published after this
/// build was compiled.
///
/// Ordered by `major` then `minor` then `patch`, oldest to newest.
/// The pin is recorded in
/// [`crates/protocol-droid-schema/spec/MANIFEST.toml`][manifest].
///
/// [manifest]: ../../../../../protocol-droid-schema/spec/MANIFEST.toml
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AcpVersion {
    /// The pin recorded in the schema `MANIFEST.toml`. The exact
    /// release is `schema-v1.25.0`; bumping the pin is its own
    /// task that re-verifies the wire contract.
    V1_25_0,
    /// A future ACP release, identified by its `major.minor.patch`.
    /// Carries the same numbers a known release would; what changes
    /// between a known release and its `Unknown` peer is whether
    /// this build's per-version codec has types for it.
    Unknown {
        /// The peer-named major version. `u16` because the
        /// release tag is decimal; a major number that
        /// overflows `u16` is not a real ACP release.
        major: u16,
        /// The peer-named minor version.
        minor: u16,
        /// The peer-named patch version.
        patch: u16,
    },
}

const fn parts(version: AcpVersion) -> (u16, u16, u16) {
    match version {
        AcpVersion::V1_25_0 => (1, 25, 0),
        AcpVersion::Unknown {
            major,
            minor,
            patch,
        } => (major, minor, patch),
    }
}

impl PartialOrd for AcpVersion {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for AcpVersion {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        parts(*self).cmp(&parts(*other))
    }
}

/// The error returned by [`AcpVersion::from_str`] when the input
/// is not a well-formed `Major.Minor.Patch` triple.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpVersionParseError {
    input: String,
    reason: &'static str,
}

impl fmt::Display for AcpVersionParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "ACP version {:?} is invalid: {}",
            self.input, self.reason
        )
    }
}

impl std::error::Error for AcpVersionParseError {}

impl FromStr for AcpVersion {
    type Err = AcpVersionParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        // The pin is `schema-v1.25.0`; the version-bearing
        // segment is everything after the `v`. The release
        // tag's `schema-v` prefix is a namespacing
        // convention, not part of the version itself.
        let version_part = input.strip_prefix("schema-v").unwrap_or(input);
        let mut iter = version_part.split('.');
        let Some(major) = iter.next() else {
            return Err(AcpVersionParseError {
                input: input.to_owned(),
                reason: "missing the major version",
            });
        };
        let Some(minor) = iter.next() else {
            return Err(AcpVersionParseError {
                input: input.to_owned(),
                reason: "missing the minor version",
            });
        };
        let Some(patch) = iter.next() else {
            return Err(AcpVersionParseError {
                input: input.to_owned(),
                reason: "missing the patch version",
            });
        };
        if iter.next().is_some() {
            return Err(AcpVersionParseError {
                input: input.to_owned(),
                reason: "expected exactly three numeric segments",
            });
        }
        let major = major.parse::<u16>().map_err(|_| AcpVersionParseError {
            input: input.to_owned(),
            reason: "the major version is not a u16",
        })?;
        let minor = minor.parse::<u16>().map_err(|_| AcpVersionParseError {
            input: input.to_owned(),
            reason: "the minor version is not a u16",
        })?;
        let patch = patch.parse::<u16>().map_err(|_| AcpVersionParseError {
            input: input.to_owned(),
            reason: "the patch version is not a u16",
        })?;
        if (1, 25, 0) == (major, minor, patch) {
            Ok(Self::V1_25_0)
        } else {
            Ok(Self::Unknown {
                major,
                minor,
                patch,
            })
        }
    }
}

impl fmt::Display for AcpVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (major, minor, patch) = parts(*self);
        write!(formatter, "{major}.{minor}.{patch}")
    }
}

impl Serialize for AcpVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for AcpVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cmp::Ordering;

    /// The well-known pin parses to its known
    /// variant, both with and without the
    /// `schema-v` prefix.
    #[test]
    fn the_pinned_version_parses_with_or_without_the_prefix() -> Result<(), AcpVersionParseError> {
        let with_prefix = "schema-v1.25.0".parse::<AcpVersion>()?;
        let without_prefix = "1.25.0".parse::<AcpVersion>()?;
        assert_eq!(with_prefix, AcpVersion::V1_25_0);
        assert_eq!(without_prefix, AcpVersion::V1_25_0);
        Ok(())
    }

    /// A future release parses into `Unknown` with
    /// the same `(major, minor, patch)` it carried
    /// on the wire, and sorts the same as a known
    /// release with those numbers would.
    #[test]
    fn an_unknown_version_orders_like_its_known_equivalent() -> Result<(), AcpVersionParseError> {
        let unknown: AcpVersion = "1.25.0".parse()?;
        assert_eq!(unknown, AcpVersion::V1_25_0);
        assert_eq!(unknown.cmp(&AcpVersion::V1_25_0), Ordering::Equal);
        let future: AcpVersion = "1.26.0".parse()?;
        assert!(future > AcpVersion::V1_25_0);
        let older: AcpVersion = "0.9.0".parse()?;
        assert!(older < AcpVersion::V1_25_0);
        Ok(())
    }

    /// The parse error carries the offending
    /// input and a human-readable reason. The
    /// `Display` impl is what an operator
    /// pasting a release tag into a log line
    /// reads.
    #[test]
    fn a_malformed_input_carries_the_input_and_the_reason() -> Result<(), String> {
        // `str::split('.')` on the empty string and
        // on `"1"` both yield a single non-None first
        // element, so the missing-segment check fires
        // on the *second* segment, not the first. The
        // `u16`-parse errors are reachable through
        // inputs the missing-segment check does not
        // catch.
        let cases: [(&str, &str); 5] = [
            ("", "missing the minor version"),
            ("1", "missing the minor version"),
            ("1.2", "missing the patch version"),
            ("1.2.3.4", "expected exactly three numeric segments"),
            ("a.b.c", "the major version is not a u16"),
        ];
        for (input, expected_reason) in cases {
            let Err(error) = input.parse::<AcpVersion>() else {
                return Err(format!("{input:?} should be an error"));
            };
            assert_eq!(error.reason, expected_reason, "input: {input:?}");
            let text = format!("{error}");
            assert!(
                text.contains(input),
                "the text should carry the input: {text}"
            );
        }
        Ok(())
    }

    /// The round-trip through `Display` and
    /// `FromStr` is identity for every variant
    /// the build knows, and for at least one
    /// `Unknown` peer. A consumer that logs an
    /// `AcpVersion` and re-parses it gets the
    /// same value back.
    #[test]
    fn display_then_from_str_round_trips_every_known_variant() -> Result<(), AcpVersionParseError> {
        let known: AcpVersion = AcpVersion::V1_25_0;
        let round_tripped: AcpVersion = format!("{known}").parse()?;
        assert_eq!(round_tripped, known);
        let future: AcpVersion = "1.27.3".parse()?;
        let round_tripped: AcpVersion = format!("{future}").parse()?;
        assert_eq!(round_tripped, future);
        Ok(())
    }
}
