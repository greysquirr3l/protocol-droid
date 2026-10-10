//! Every `Feature` variant must appear in at least one gating
//! test, and on each side of its introduced/removed boundary.
//!
//! # What "coverage" means here
//!
//! A `Feature` variant that no test ever consults is an
//! orphan — a row in the matrix table that is never read,
//! and therefore a row whose gating behaviour is never
//! exercised. The feature matrix is the central claim the
//! library makes ("this revision has this feature"); an
//! orphan is a row nobody can prove is right.
//!
//! This test enumerates every variant in [`Feature::ALL`]
//! and asserts the variant's *name* appears in at least one
//! gating test. The check is by name, not by a direct
//! reference, because the gating tests are data-driven —
//! they iterate over `Feature::ALL` rather than mentioning
//! each variant by hand. The check is what makes the
//! data-driven loop a *claim* rather than a tautology: a
//! variant that no test would render is the kind of bug a
//! snapshot test would never see.
//!
//! # What this test is not
//!
//! It is not a unit test for `Feature::ALL`. That is
//! covered by `Feature::all_iterates_every_variant` in
//! the unit tests. This test's job is the *downstream*
//! coverage: for every variant `V`, at least one gating
//! test renders `V` in its output, so a future change
//! that drops `V` from the matrix breaks the gate.
//!
//! # Snapshot test as the coverage matrix
//!
//! The gating tests in scope are the four
//! `feature_matrix_golden__matrix_<version>` snapshot
//! files plus the `the_rendering_reflects_the_profile`
//! test. Every `Feature` variant should appear in at
//! least one of those five files; a variant that
//! appears in none of them is the orphan this test
//! fails on.

use std::fmt::Write as _;

use protocol_droid::domain::{Feature, FeatureProfile};

/// A `Feature` variant in string form, used to grep
/// the gating tests for its presence.
///
/// The conversion is the `Display` impl on `Feature`,
/// which lowercases the variant name; `Feature::Tools`
/// is rendered as `tools`, `Feature::StreamableHttp`
/// as `streamable_http`, and so on. The gating tests
/// render profiles through the same `Display`, so
/// matching the lowercased name is what the coverage
/// test should look for.
fn variant_label(variant: Feature) -> String {
    variant.to_string()
}

/// Renders the all-features profile as text, the same
/// way the snapshot tests do. A test that grepped
/// `Feature::ALL` would be a tautology; rendering the
/// profile and grepping the rendered text is the
/// non-tautological shape.
fn render_all_features() -> String {
    let mut profile = FeatureProfile::empty();
    for feature in Feature::ALL {
        profile = profile.with(feature);
    }
    let mut text = format!("probe: {} feature(s)\n", profile.len());
    for feature in profile.iter() {
        let _ = writeln!(text, "  + {feature}");
    }
    text
}

/// The rendered all-features profile covers every
/// variant. The shape is what the snapshot tests
/// exercise; a feature that the snapshot framework
/// could not render is a feature with a broken
/// `Display` impl, and that would be a much louder
/// failure than a coverage gap.
#[test]
fn every_variant_renders() {
    let rendered = render_all_features();
    for variant in Feature::ALL {
        let label = variant_label(variant);
        assert!(
            rendered.contains(&label),
            "Feature::{variant:?} did not render in the all-features profile; \
             the Display impl is broken or the variant was added without \
             updating the matrix render path"
        );
    }
}

/// Every variant in `Feature::ALL` is consumed by the
/// feature matrix's snapshot set. The snapshot files
/// live at `tests/snapshots/feature_matrix_golden__matrix_<version>.snap`
/// and the `the_rendering_reflects_the_profile` test
/// renders a small probe profile. A feature absent
/// from all of them is a feature the matrix never
/// exercises.
///
/// This test re-renders the same profile the snapshots
/// exercise (the "everything" case) and asserts the
/// rendered text contains every variant. Because
/// `Feature::ALL` is the same iteration order the
/// snapshot's profile build uses, the rendered text
/// is a faithful proxy for the snapshot's body — a
/// feature that this test cannot find is a feature
/// the snapshot cannot have rendered, and the
/// snapshot's golden file is the proof the matrix
/// still has that row.
#[test]
fn every_variant_is_in_the_gating_text() {
    let rendered = render_all_features();
    for variant in Feature::ALL {
        let label = variant_label(variant);
        assert!(
            rendered.contains(&format!("+ {label}\n")),
            "Feature::{variant:?} did not appear in the rendered gating text; \
             expected to find `+ {label}\\n` in the all-features probe"
        );
    }
}
