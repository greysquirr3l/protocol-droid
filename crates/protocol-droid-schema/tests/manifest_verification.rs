//! Verifies the vendored spec tree against `spec/MANIFEST.toml`.
//!
//! The task requires a test that recomputes SHA-256 for every vendored file and
//! compares it with the manifest. `build.rs` does the same check at compile
//! time, but a build script's failure mode is a panic with a message — it never
//! shows up as a named test that can be read, filtered, or run on its own.
//!
//! Scope: this file covers only what needs the real vendored tree or a real
//! filesystem. Manifest parsing and the reconciliation rules are pure, so they
//! are tested in `src/domain/manifest.rs` rather than duplicated here.

// Test code unwraps deliberately: the strict profile exists to keep production
// paths fallible, and a test that cannot unwrap cannot assert. The messages
// here name the failing precondition, which is the useful part.
#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on fallible calls and panic with the failing precondition; propagating errors would replace a clear failure message with noise"
)]

use std::path::{Path, PathBuf};

use protocol_droid_schema::domain::manifest::RevisionSpec;
use protocol_droid_schema::domain::manifest::hex_digest;
use protocol_droid_schema::verify_vendored_specs;

/// Every revision this project vendors, including the floating one.
const EXPECTED_REVISIONS: [&str; 6] = [
    "2024-11-05",
    "2025-03-26",
    "2025-06-18",
    "2025-11-25",
    "2026-07-28",
    "draft",
];

/// Root of the vendored tree, relative to this crate.
fn spec_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("spec")
}

/// A copy of the vendored tree in a temporary directory, removed on drop, so a
/// test can corrupt it without touching the real one.
struct Scratch(PathBuf);

impl Scratch {
    fn named(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("protocol-droid-t02-{name}"));
        let _ = std::fs::remove_dir_all(&path);
        copy_dir(&spec_root(), &path).expect("copy spec tree");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn every_vendored_file_matches_its_recorded_digest() {
    let manifest = verify_vendored_specs(&spec_root())
        .expect("the vendored spec tree must match MANIFEST.toml");

    // Recomputed independently of the verification path, so a bug in the
    // verifier cannot make the test that exercises it pass.
    let mut checked = 0_usize;
    for (revision, spec) in &manifest.revisions {
        for (file, expected) in &spec.files {
            let path = spec_root().join(revision).join(file);
            let bytes = std::fs::read(&path).expect("vendored file must be readable");
            let actual = hex_digest(&bytes);
            assert_eq!(
                &actual, expected,
                "{revision}/{file} does not match its recorded digest"
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "manifest recorded no files to check");
}

#[test]
fn every_published_revision_is_vendored() {
    let manifest = verify_vendored_specs(&spec_root()).expect("manifest must verify");

    for revision in EXPECTED_REVISIONS {
        assert!(
            manifest.revisions.contains_key(revision),
            "revision {revision} is missing from the manifest"
        );
        assert!(
            spec_root().join(revision).is_dir(),
            "revision {revision} has no directory on disk"
        );
    }
    assert_eq!(
        manifest.revision_names().len(),
        EXPECTED_REVISIONS.len(),
        "the manifest should contain exactly the vendored revisions"
    );
}

#[test]
fn every_revision_pins_a_full_lowercase_commit_sha() {
    let manifest = verify_vendored_specs(&spec_root()).expect("manifest must verify");

    for (name, spec) in &manifest.revisions {
        assert_eq!(
            spec.commit.len(),
            40,
            "{name} commit must be a full 40-character SHA, not a branch name"
        );
        assert!(
            spec.commit
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "{name} commit must be lowercase hex"
        );
    }
}

#[test]
fn a_tagged_revision_records_its_own_tag_and_only_draft_is_floating() {
    let manifest = verify_vendored_specs(&spec_root()).expect("manifest must verify");

    for (name, spec) in &manifest.revisions {
        if name == "draft" {
            assert!(spec.is_floating(), "draft must be marked floating");
            assert!(spec.tag.is_empty(), "draft must record no tag");
        } else {
            assert!(!spec.is_floating(), "{name} must not be floating");
            assert_eq!(spec.tag.as_str(), *name, "{name} must record its own tag");
        }
    }
}

#[test]
fn every_revision_vendors_both_schema_representations() {
    let manifest = verify_vendored_specs(&spec_root()).expect("manifest must verify");

    for (name, spec) in &manifest.revisions {
        for required in ["schema.json", "schema.ts"] {
            assert!(
                spec.files.contains_key(required),
                "{name} is missing {required}"
            );
            assert!(
                spec_root().join(name).join(required).is_file(),
                "{name}/{required} is recorded but absent on disk"
            );
        }
    }
}

#[test]
fn every_revision_ships_a_changelog_for_the_feature_matrix_to_cite() {
    // T05 cites a changelog section per feature row, so a revision with no
    // CHANGELOG.md would leave those rows uncitable.
    let manifest = verify_vendored_specs(&spec_root()).expect("manifest must verify");

    for name in &manifest.revision_names() {
        assert!(
            spec_root().join(name).join("CHANGELOG.md").is_file(),
            "{name} has no CHANGELOG.md for the feature matrix to cite"
        );
    }
}

#[test]
fn a_vendored_changelog_is_digest_locked_and_an_authored_one_is_not() {
    // 2024-11-05 is the only revision with an authored changelog, because it is
    // the first published revision and so has no prior revision to describe
    // changes from. It must stay editable without breaking verification; every
    // other revision's changelog must not be.
    let manifest = verify_vendored_specs(&spec_root()).expect("manifest must verify");

    let authored: Vec<&str> = manifest
        .revision_names()
        .into_iter()
        .filter(|name| {
            manifest
                .revisions
                .get(*name)
                .is_some_and(RevisionSpec::changelog_is_authored)
        })
        .collect();
    assert_eq!(authored, vec!["2024-11-05"]);

    for (name, spec) in &manifest.revisions {
        let expected = if spec.changelog_is_authored() {
            false
        } else {
            assert!(
                spec.changelog.is_some(),
                "{name} vendors a changelog but records no upstream path for it"
            );
            true
        };
        assert_eq!(
            spec.files.contains_key("CHANGELOG.md"),
            expected,
            "{name} changelog digest-lock state is wrong"
        );
    }
}

#[test]
fn a_changed_file_is_reported_as_a_digest_mismatch_naming_the_fix() {
    let scratch = Scratch::named("mismatch");
    let target = scratch.path().join("2025-06-18").join("schema.json");
    let mut bytes = std::fs::read(&target).expect("read schema");
    bytes.push(b'\n');
    std::fs::write(&target, &bytes).expect("write corrupted schema");

    let rendered = verify_vendored_specs(scratch.path())
        .expect_err("corruption must be detected")
        .to_string();
    assert!(
        rendered.contains("digest mismatch"),
        "expected a digest mismatch, got: {rendered}"
    );
    // The message names what failed and shows both digests, which is what a
    // reader needs. It does not name a refresh command: the same message type
    // is used for a cited external document, where `refresh-spec` is not the
    // command that fixes it, and remediation belongs to the build script that
    // knows which tree it walked.
    assert!(
        rendered.contains("2025-06-18") && rendered.contains("schema.json"),
        "the error should name the file it is about, got: {rendered}"
    );
    assert!(
        rendered.contains("expected") && rendered.contains("actual"),
        "the error should show both digests, got: {rendered}"
    );
}

#[test]
fn an_unrecorded_file_is_rejected() {
    // A file vendored but never recorded would be verified by nothing, so the
    // verifier reconciles the other direction too.
    let scratch = Scratch::named("unaccounted");
    std::fs::write(
        scratch.path().join("2025-06-18").join("schema-extra.json"),
        b"{}",
    )
    .expect("write extra file");

    let rendered = verify_vendored_specs(scratch.path())
        .expect_err("an unrecorded file must be detected")
        .to_string();
    assert!(
        rendered.contains("neither digest-locked nor declared"),
        "expected an unaccounted-file error, got: {rendered}"
    );
}

#[test]
fn a_deleted_vendored_file_is_rejected() {
    let scratch = Scratch::named("deleted");
    std::fs::remove_file(scratch.path().join("2025-11-25").join("schema.ts"))
        .expect("delete vendored file");

    let rendered = verify_vendored_specs(scratch.path())
        .expect_err("a missing vendored file must be detected")
        .to_string();
    // The digest check runs first, so a deleted file surfaces as unreadable
    // rather than as `MissingFile`. Either way it is rejected, and the message
    // has to name the file so the failure is actionable.
    assert!(
        rendered.contains("could not read"),
        "expected an unreadable-file error, got: {rendered}"
    );
    assert!(
        rendered.contains("schema.ts"),
        "the error should name the missing file, got: {rendered}"
    );
}

#[test]
fn the_excluded_tags_are_recorded_with_reasons() {
    // The tags this project chose not to vendor, so the choice is a recorded
    // decision rather than a silent gap.
    let manifest = verify_vendored_specs(&spec_root()).expect("manifest must verify");
    let excluded = &manifest.meta.excluded_tags;

    for tag in [
        "2024-10-07",
        "2024-11-05-final",
        "2025-11-25-RC",
        "2026-07-28-RC",
    ] {
        let reason = excluded
            .get(tag)
            .unwrap_or_else(|| panic!("excluded tag {tag} is not recorded with a reason"));
        assert!(!reason.is_empty(), "excluded tag {tag} has an empty reason");
    }
    for name in &manifest.revision_names() {
        assert!(
            !excluded.contains_key(*name),
            "{name} is both vendored and recorded as excluded"
        );
    }
}

/// Recursively copy a directory tree. Test-only helper.
fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}
