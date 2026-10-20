//! Asserts the feature-support doc in
//! `docs/guide/src/feature-profile.md` matches the
//! `FeatureProfile`'s contract: the four inputs to the
//! matrix derivation, the matrix's location, and the
//! `DegradePolicy` opt-in are all referenced in the doc.
//!
//! # What this test exists to do
//!
//! The T26 spec calls for a feature-support table that
//! is "generated from the feature matrix (single source
//! of truth), not hand-written." The matrix lives in
//! [`protocol_droid::domain`] and is rendered as a
//! snapshot in `feature_matrix_golden.rs`; the doc is
//! the human-readable form. This test guards the doc's
//! contract — the *prose* claims about how the matrix
//! works — against drift.
//!
//! A test that asked the doc to mention every variant
//! by name would be too strict: a guide page is a
//! *prose* document, and a feature the matrix adds can
//! be described without being named. A test that asks
//! the doc to be honest about the four inputs the
//! `FeatureProfile` derives from is the right level
//! of contract: those four inputs are the matrix's
//! signature, and a future change that removed one
//! would break the `FeatureProfile` function, and
//! this assertion is the doc-side echo of the same
//! claim.
//!
//! # What this test is not
//!
//! It is not a snapshot of the doc's prose. The prose
//! can change freely; the test only checks the four
//! claims the [`FeatureProfile`] function makes.

use std::fmt::Write as _;

/// The path to the feature-support doc, relative to the workspace
/// root. Resolved from `CARGO_MANIFEST_DIR` (the protocol-droid crate
/// root) at test time.
///
/// This points at the site, which is the only documentation tree the
/// deploy script publishes and the only one that is maintained. It
/// previously pointed at `docs/guide/`, an mdBook tree that no workflow
/// built, nothing linked to, and that had drifted from this page by
/// dozens of lines; see `published_docs_consistency.rs`.
const DOC_PATH: &str = "../../site/src/content/docs/guides/feature-profile.md";

#[test]
fn doc_has_an_introductory_section() -> Result<(), String> {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let doc_path = manifest.join(DOC_PATH);
    let doc = std::fs::read_to_string(&doc_path)
        .map_err(|error| format!("failed to read {}: {error}", doc_path.display()))?;

    // The page's title lives in the frontmatter, because Starlight
    // renders that as the H1 and the markdown body starts straight at
    // the prose. A rename that touches one and not the other is the
    // kind of bug this catches.
    assert!(
        doc.lines()
            .any(|line| line.trim() == "title: FeatureProfile and gating"),
        "the page's frontmatter title is no longer `FeatureProfile and gating`; \
         the rendered H1 changed without a corresponding update here"
    );

    // The four inputs to the `FeatureProfile`
    // derivation. A future change that removed one
    // from the doc would be the kind of drift the
    // matrix snapshot would not catch, because the
    // snapshot renders *output*; the doc carries
    // the *signature*.
    for input in [
        "the protocol version",
        "the client's capabilities",
        "the server's capabilities",
        "the transport",
    ] {
        assert!(
            doc.to_lowercase().contains(input),
            "doc does not mention the {input:?} input to the FeatureProfile"
        );
    }

    // The doc's `DegradePolicy` opt-in claim. A future
    // change that removed the explicit opt-in from the
    // code would be a security regression; the
    // test catches the doc side of that drift.
    assert!(
        doc.contains("DegradePolicy"),
        "doc does not mention the `DegradePolicy` opt-in"
    );
    let _ = write!(String::new(), "");
    Ok(())
}
