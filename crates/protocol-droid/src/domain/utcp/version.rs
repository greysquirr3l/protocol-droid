//! `UtcpVersion` — the UTCP version, as a value object.
//!
//! # Why semver and not a date
//!
//! The UTCP specification identifies a revision the way MCP and A2A do not:
//! by a semver string. A manual carries two of them — `utcp_version` for the
//! protocol and `manual_version` for the document format — and the examples
//! in the specification are patch-level (`"utcp_version": "1.0.1"`,
//! `"manual_version": "1.0.0"`). So the model is
//! `{ major: u16, minor: u16, patch: u16 }`, and there is no date to
//! mistake for a revision.
//!
//! # Why the patch is part of the model
//!
//! The specification's published revisions are `0.1`, `1.0` and `1.1`, but
//! the version a manual carries is finer than that: `1.0.1` is a different
//! string from `1.0.0`, and a client that treated them as the same revision
//! would claim a compatibility the two documents do not have. A model that
//! stopped at `minor` would have to drop the patch, and dropping it is
//! exactly the kind of quiet loss the rest of this crate refuses to make.
//!
//! # Why the ordering is derived, and why that is safe
//!
//! `Ord` and `PartialOrd` are both derived, from the same field order, which
//! is what the 1.98 derive fast path requires. The struct has no
//! hand-written `PartialOrd` to disagree with the derive, so there is
//! nothing to keep in step — and `tests::the_derived_order_is_the_numeric_one`
//! pins the ordering rather than trusting it.

use core::fmt;
use core::str::FromStr;

/// A parsed UTCP version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UtcpVersion {
    major: u16,
    minor: u16,
    patch: u16,
}

impl UtcpVersion {
    /// The newest published revision, `1.1.0`.
    ///
    /// v1.1 added `allowed_communication_protocols` to every call template,
    /// which is an addition rather than a change: a manual that omits the
    /// field is a v1.0 manual and is still valid under v1.1.
    pub const V1_1: Self = Self::new(1, 1, 0);

    /// A version with the three components given.
    #[must_use]
    pub const fn new(major: u16, minor: u16, patch: u16) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// The major component.
    #[must_use]
    pub const fn major(self) -> u16 {
        self.major
    }

    /// The minor component.
    #[must_use]
    pub const fn minor(self) -> u16 {
        self.minor
    }

    /// The patch component.
    #[must_use]
    pub const fn patch(self) -> u16 {
        self.patch
    }

    /// Whether this version is at least `other`.
    ///
    /// The comparison is the numeric one, so `1.10` is newer than `1.2` — the
    /// lexicographic answer, which is what a string comparison would give, is
    /// wrong.
    #[must_use]
    pub fn is_at_least(self, other: Self) -> bool {
        self >= other
    }
}

impl fmt::Display for UtcpVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// A version string that is not a version.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VersionError {
    /// The string is not three dot-separated numbers.
    #[error("a UTCP version is `major.minor.patch`, not `{raw}`")]
    NotAVersion {
        /// The string that was offered.
        raw: String,
    },
    /// A component is not a number, or is too large to be a `u16`.
    #[error("the {component} component of `{raw}` is not a number: {detail}")]
    NotANumber {
        /// The string that was offered.
        raw: String,
        /// Which component failed.
        component: &'static str,
        /// What the parser saw instead of a number.
        detail: String,
    },
}

impl FromStr for UtcpVersion {
    type Err = VersionError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let mut parts = raw.split('.');
        let major = component(raw, "major", parts.next())?;
        let minor = component(raw, "minor", parts.next())?;
        let patch = component(raw, "patch", parts.next())?;
        if parts.next().is_some() {
            return Err(VersionError::NotAVersion {
                raw: raw.to_owned(),
            });
        }
        Ok(Self::new(major, minor, patch))
    }
}

/// One numeric component of a version.
fn component(raw: &str, name: &'static str, part: Option<&str>) -> Result<u16, VersionError> {
    let Some(part) = part else {
        return Err(VersionError::NotAVersion {
            raw: raw.to_owned(),
        });
    };
    part.parse::<u16>().map_err(|error| VersionError::NotANumber {
        raw: raw.to_owned(),
        component: name,
        detail: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_parses_from_the_specifications_own_form() {
        let parsed = UtcpVersion::from_str("1.0.1");
        assert_eq!(parsed, Ok(UtcpVersion::new(1, 0, 1)));
    }

    #[test]
    fn a_version_round_trips_through_its_string_form() {
        let version = UtcpVersion::new(1, 1, 0);
        assert_eq!(version.to_string(), "1.1.0");
    }

    #[test]
    fn the_derived_order_is_the_numeric_one() {
        // The 1.98 derive fast path assumes a hand-written `PartialOrd`
        // agrees with a derived `Ord`. There is no hand-written one here, and
        // this test is what says so.
        let mut versions = vec![
            UtcpVersion::new(1, 10, 0),
            UtcpVersion::new(1, 2, 0),
            UtcpVersion::new(0, 1, 0),
            UtcpVersion::new(2, 0, 0),
        ];
        versions.sort();
        assert_eq!(
            versions,
            vec![
                UtcpVersion::new(0, 1, 0),
                UtcpVersion::new(1, 2, 0),
                UtcpVersion::new(1, 10, 0),
                UtcpVersion::new(2, 0, 0),
            ]
        );
    }

    #[test]
    fn a_version_compares_by_number_not_by_spelling() {
        let newer = UtcpVersion::new(1, 10, 0);
        let older = UtcpVersion::new(1, 2, 0);
        assert!(newer.is_at_least(older));
        assert!(!older.is_at_least(newer));
    }

    #[test]
    fn a_malformed_version_is_refused() {
        for raw in ["1.0", "1.0.0.0", "", "v1.0.0", "1.x.0", "1.0.x"] {
            assert!(
                UtcpVersion::from_str(raw).is_err(),
                "`{raw}` should not parse as a version"
            );
        }
    }

    #[test]
    fn the_newest_published_revision_is_one_one() {
        assert_eq!(UtcpVersion::V1_1.to_string(), "1.1.0");
    }
}
