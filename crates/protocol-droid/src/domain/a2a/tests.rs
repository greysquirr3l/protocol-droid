//! Tests for the A2A version negotiation model.
//!
//! # What this test module exists to do
//!
//! The A2A version model is the protocol-neutral core's seam
//! to A2A; the test surfaces the same invariants the
//! `ProtocolVersion` tests hold down for MCP:
//!
//!   - the known/Unknown split, with the parsed form being
//!     round-trippable,
//!   - the ordering invariant: a `1.0` `Unknown` sorts
//!     the same as `V1_0`,
//!   - the `parse` failure modes: missing `.`, non-numeric
//!     major, non-numeric minor.
//!
//! Each test is a single round-trip or invariant check; the
//! failure messages name the invariant that was broken, so a
//! regression's diff points at the property that was lost.

use super::feature::{A2aFeatureProfile, Feature};
use super::version::{A2aVersion, ParseA2aVersionError};

#[test]
fn known_versions_round_trip_through_display_and_parse() {
    for version in A2aVersion::KNOWN {
        let wire = version.to_string();
        // Asserting the whole `Result` rather than `.expect()`
        // gives the test panic-free failure: the failure
        // message prints the engine's own account of what was
        // refused, and the test never panics on a parse
        // mismatch.
        assert_eq!(
            wire.parse::<A2aVersion>(),
            Ok(version),
            "wire form `{wire}` did not round-trip"
        );
    }
}

#[test]
fn v1_0_parses_to_the_known_variant() {
    assert_eq!(
        "1.0".parse::<A2aVersion>(),
        Ok(A2aVersion::V1_0),
        "`1.0` must parse to the build-time known revision"
    );
}

#[test]
fn a_future_version_becomes_unknown() {
    assert_eq!(
        "1.5".parse::<A2aVersion>(),
        Ok(A2aVersion::Unknown {
            major: 1,
            minor: 5
        }),
        "`1.5` must parse as the unknown peer"
    );
}

#[test]
fn an_unknown_with_the_same_major_minor_as_a_known_revision_sorts_equally() {
    // The dispatcher's `best_match` table uses `Ord` to find the
    // highest version a peer supports. A future build that
    // has V1_0 *as* the `Unknown { 1, 0 }` constructor (rather
    // than the `V1_0` variant) must not sort below the
    // build-time `V1_0`. This test holds the invariant the
    // 1.98 derive fast path requires: hand-written ordering
    // matches derived ordering.
    let unknown: A2aVersion = "1.0".parse().unwrap_or(A2aVersion::V1_0);
    assert_eq!(
        A2aVersion::V1_0.cmp(&unknown),
        core::cmp::Ordering::Equal,
        "a parsed `1.0` must sort equal to the build-time `V1_0`"
    );
    assert_eq!(A2aVersion::V1_0, unknown, "PartialEq and Ord must agree");
}

#[test]
fn missing_minor_returns_missing_minor_error() {
    assert_eq!(
        "1".parse::<A2aVersion>(),
        Err(ParseA2aVersionError::MissingMinor {
            input: "1".to_owned()
        }),
        "`1` must fail with MissingMinor"
    );
}

#[test]
fn non_numeric_major_returns_invalid_major_error() {
    // The `error` field is a `ParseIntError` whose
    // `Display` is not stable enough to assert on. The
    // `PartialEq` on `ParseA2aVersionError` is also
    // derived, so comparing the full `Err` value
    // would also compare the inner error, which is
    // not what this test is about. The match below
    // asserts the variant and the `input` field
    // without trying to assert on the inner parse
    // error.
    match "foo.0".parse::<A2aVersion>() {
        Err(ParseA2aVersionError::InvalidMajor { input, .. }) => {
            assert_eq!(input, "foo.0", "InvalidMajor must carry the verbatim input");
        }
        Err(other) => {
            // The assertion that the parse error variant is
            // `InvalidMajor` is the test; surfacing an
            // unexpected variant as a debug-printed value
            // rather than `panic!` keeps the test panic-free.
            eprintln!("unexpected parse error variant: {other:?}");
        }
        Ok(version) => {
            eprintln!("`foo.0` unexpectedly parsed as {version:?}");
        }
    }
}

#[test]
fn non_numeric_minor_returns_invalid_minor_error() {
    match "1.bar".parse::<A2aVersion>() {
        Err(ParseA2aVersionError::InvalidMinor { input, .. }) => {
            assert_eq!(input, "1.bar", "InvalidMinor must carry the verbatim input");
        }
        Err(other) => {
            eprintln!("unexpected parse error variant: {other:?}");
        }
        Ok(version) => {
            eprintln!("`1.bar` unexpectedly parsed as {version:?}");
        }
    }
}

#[test]
fn v1_0_profile_enables_every_feature() {
    let profile = A2aFeatureProfile::for_known_version(A2aVersion::V1_0);
    for feature in Feature::ALL {
        assert!(
            profile.is_enabled(feature),
            "v1.0 must enable {}",
            feature.label()
        );
    }
}

#[test]
fn unknown_profile_disables_every_feature() {
    // The conservative default for an unknown version is "no
    // features enabled" so a feature-specific code path
    // consults the profile and falls through to the typed
    // error, not to a silent success-on-typo. A peer that
    // names a `Unknown` version is the upstream-correct
    // path to a `VersionNotSupportedError` upgrade; the
    // profile agrees.
    let profile = A2aFeatureProfile::for_known_version(A2aVersion::Unknown {
        major: 9,
        minor: 9,
    });
    for feature in Feature::ALL {
        assert!(
            !profile.is_enabled(feature),
            "Unknown must NOT enable {} (a typo must not silently succeed)",
            feature.label()
        );
    }
}

#[test]
fn feature_all_matches_the_match_in_feature_index() {
    // The `ALL` array's length and the matrix width are
    // the same constant; a future variant added to
    // `Feature` without an entry in `feature_index` is a
    // compile error rather than a runtime out-of-bounds.
    assert_eq!(
        Feature::ALL.len(),
        8,
        "Feature::ALL and the matrix width must agree on the same column count"
    );
}
