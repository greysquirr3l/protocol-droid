//! Golden snapshots of the feature matrix.
//!
//! # Why a snapshot and not an assertion per cell
//!
//! The matrix is 37 features × 5 revisions × 4 transports. Asserting each cell
//! individually would be 740 assertions that all say the same thing, and a
//! change to one row would rewrite one line of test source instead of one line
//! of a reviewable file. A snapshot makes the whole matrix legible: the diff a
//! reviewer reads *is* the change to the matrix.
//!
//! That legibility is the point. The feature table is the crate's central claim
//! — "this revision has this feature" — and a claim nobody can read is a claim
//! nobody checks. `cargo insta review` (or reading the `.snap` file) shows
//! every row for every revision at once.
//!
//! # What is snapshotted
//!
//! Four cases per revision, which between them separate the four conditions
//! [`resolve`] applies:
//!
//! - **everything** — every capability that revision defines, advertised. The
//!   full matrix for that revision.
//! - **nothing** — no capabilities. Which features are unconditional.
//! - **client-only** — every client capability, no server ones. The features a
//!   server can drive on a client.
//! - **server-only** — the mirror.
//!
//! The golden files are written for the vendored revisions only. A feature
//! profile for an unknown revision is empty by construction and asserting that
//! is a unit test, not a snapshot.

// Test code unwraps deliberately: the strict profile exists to keep production
// paths fallible, and a test that cannot unwrap cannot assert.
#![expect(
    clippy::panic,
    reason = "tests assert on fallible calls and panic with the failing precondition; propagating errors would replace a clear failure message with noise"
)]

use std::fmt::Write as _;

use protocol_droid::domain::{
    CapabilitySet, Feature, FeatureProfile, Negotiation, ProtocolVersion, TransportKind, resolve,
};

/// Renders a profile as one indented line per feature, plus a count.
///
/// Declaration order rather than sorted order, so a diff shows a feature moving
/// rather than the whole list reshuffling when a variant is inserted.
fn render(label: &str, profile: FeatureProfile) -> String {
    let mut out = format!("{label}: {} feature(s)\n", profile.len());
    for feature in profile.iter() {
        let _ = writeln!(out, "  + {feature}");
    }
    for feature in profile.missing() {
        let _ = writeln!(out, "  - {feature}");
    }
    out
}

fn snapshot_for(version: ProtocolVersion) -> String {
    let all = CapabilitySet::all_for(version);
    let mut out = String::new();
    for transport in TransportKind::ALL {
        out.push_str(&render(
            &format!("== {version} / {transport} / everything"),
            resolve(Negotiation {
                version,
                client_caps: all,
                server_caps: all,
                transport,
            }),
        ));
        out.push('\n');
        out.push_str(&render(
            &format!("== {version} / {transport} / nothing"),
            resolve(Negotiation::bare(version, transport)),
        ));
        out.push_str("\n\n");
    }

    // The client/server split only differs by transport for the rows that name
    // one, so stdio is enough to pin the capability gating.
    for (label, client_caps, server_caps) in [
        ("client-only", all, CapabilitySet::empty()),
        ("server-only", CapabilitySet::empty(), all),
    ] {
        out.push_str(&render(
            &format!("== {version} / stdio / {label}"),
            resolve(Negotiation {
                version,
                client_caps,
                server_caps,
                transport: TransportKind::Stdio,
            }),
        ));
        out.push_str("\n\n");
    }
    out
}

#[test]
fn the_feature_matrix_matches_its_golden_snapshots() {
    for version in ProtocolVersion::KNOWN {
        insta::assert_snapshot!(format!("matrix_{version}"), snapshot_for(version));
    }
}

/// The golden files are only worth having if they change when the matrix does,
/// so this is checked rather than assumed: perturbing a single row has to move
/// the rendered text.
#[test]
fn the_rendering_reflects_the_profile() {
    let profile = FeatureProfile::empty().with(Feature::Tools).with(Feature::Prompts);
    let text = render("probe", profile);
    assert!(text.starts_with("probe: 2 feature(s)"), "{text}");
    assert!(text.contains("  + tools\n"), "{text}");
    assert!(text.contains("  + prompts\n"), "{text}");
    // Declaration order: `tools` is declared before `prompts` in `Feature::ALL`,
    // so the order is stable rather than incidental to a sort.
    let (Some(tools), Some(prompts)) = (text.find("+ tools"), text.find("+ prompts")) else {
        panic!("both features are rendered");
    };
    assert!(tools < prompts, "{text}");
    // And the missing ones are listed, which is what makes a row's *absence*
    // visible in the diff rather than inferred from it.
    assert!(text.contains("  - ping\n"), "{text}");
}
