//! Verifies the vendored spec tree against `spec/MANIFEST.toml`.
//!
//! The task requires a test that recomputes SHA-256 for every vendored file and
//! compares it with the manifest. `build.rs` does the same check at compile
//! time, but a build script's failure mode is a panic with a message — it never
//! shows up as a named test that can be read, filtered, or run on its own.

use std::path::Path;

use protocol_droid_schema::domain::manifest::{Manifest, hex_digest};
use protocol_droid_schema::verify_vendored_specs;

/// Root of the vendored tree, relative to this crate.
const SPEC_ROOT: &str = "spec";

fn spec_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(SPEC_ROOT)
}

#[test]
fn every_vendored_file_matches_its_recorded_digest() {
    let manifest =
        verify_vendored_specs(&spec_root()).expect("vendored spec tree must match MANIFEST.toml");

    // Recompute independently of the verification path, so a bug in the
    // verifier cannot make its own test pass.
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

    let expected = [
        "2024-11-05",
        "2025-03-26",
        "2025-06-18",
        "2025-11-25",
        "2026-07-28",
        "draft",
    ];
    for revision in expected {
        assert!(
            manifest.revisions.contains_key(revision),
            "revision {revision} is missing from the manifest"
        );
    }
    assert_eq!(
        manifest.revision_names().len(),
        expected.len(),
        "manifest has an unexpected number of revisions"
    );
}

#[test]
fn every_tagged_revision_pins_a_commit_and_a_tag() {
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
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "{name} commit must be lowercase hex"
        );
        if name == "draft" {
            assert!(spec.is_floating(), "draft must be marked floating");
        } else {
            assert_eq!(spec.tag, name, "{name} must record its own tag");
            assert!(!spec.is_floating(), "{name} must not be floating");
        }
    }
}

#[test]
fn only_draft_is_floating() {
    let manifest = verify_vendored_specs(&spec_root()).expect("manifest must verify");

    let floating: Vec<&str> = manifest
        .revision_names()
        .into_iter()
        .filter(|name| manifest.revisions[*name].is_floating())
        .collect();
    assert_eq!(floating, vec!["draft"]);
}

#[test]
fn each_revision_vendors_both_schema_representations() {
    let manifest = verify_vendored_specs(&spec_root()).expect("manifest must verify");

    for (name, spec) in &manifest.revisions {
        assert!(
            spec.files.contains_key("schema.json"),
            "{name} is missing schema.json"
        );
        assert!(
            spec.files.contains_key("schema.ts"),
            "{name} is missing schema.ts"
        );
    }
}

#[test]
fn a_changed_file_is_reported_as_a_digest_mismatch() {
    // The negative case. Copy the tree, corrupt one file, and confirm the
    // verifier rejects it rather than silently passing.
    let scratch = std::env::temp_dir().join("protocol-droid-t02-mismatch");
    let _ = std::fs::remove_dir_all(&scratch);
    copy_dir(&spec_root(), &scratch).expect("copy spec tree");

    let target = scratch.join("2025-06-18").join("schema.json");
    let mut bytes = std::fs::read(&target).expect("read schema");
    bytes.push(b'\n');
    std::fs::write(&target, &bytes).expect("write corrupted schema");

    let error = verify_vendored_specs(&scratch).expect_err("corruption must be detected");
    let rendered = error.to_string();
    assert!(
        rendered.contains("digest mismatch"),
        "expected a digest mismatch, got: {rendered}"
    );
    assert!(
        rendered.contains("refresh-spec 2025-06-18"),
        "error should name the refresh command, got: {rendered}"
    );

    std::fs::remove_dir_all(&scratch).expect("clean up scratch tree");
}

#[test]
fn an_unrecorded_file_is_rejected() {
    // A file vendored but never recorded would be verified by nothing, so the
    // verifier reconciles the other direction too.
    let scratch = std::env::temp_dir().join("protocol-droid-t02-unaccounted");
    let _ = std::fs::remove_dir_all(&scratch);
    copy_dir(&spec_root(), &scratch).expect("copy spec tree");

    let smuggled = scratch.join("2025-06-18").join("schema-extra.json");
    std::fs::write(&smuggled, b"{}").expect("write extra file");

    let error = verify_vendored_specs(&scratch).expect_err("unrecorded file must be detected");
    let rendered = error.to_string();
    assert!(
        rendered.contains("neither digest-locked nor declared"),
        "expected an unaccounted-file error, got: {rendered}"
    );

    std::fs::remove_dir_all(&scratch).expect("clean up scratch tree");
}

#[test]
fn a_manifest_with_a_future_schema_version_is_refused() {
    let source = "\
[meta]
schema_version = 999
repository = \"x/y\"
raw_base = \"https://example.invalid\"

[revisions.x]
tag = \"x\"
commit = \"0000000000000000000000000000000000000000\"
";
    let error = Manifest::parse(source).expect_err("future schema version must be refused");
    assert!(
        error.to_string().contains("newer than the supported"),
        "unexpected error: {error}"
    );
}

#[test]
fn a_short_commit_sha_is_refused() {
    let source = "\
[meta]
schema_version = 1
repository = \"x/y\"
raw_base = \"https://example.invalid\"

[revisions.x]
tag = \"x\"
commit = \"abc1234\"
";
    let error = Manifest::parse(source).expect_err("short SHA must be refused");
    assert!(
        error.to_string().contains("40-character"),
        "unexpected error: {error}"
    );
}

#[test]
fn a_non_hex_digest_is_refused() {
    let source = "\
[meta]
schema_version = 1
repository = \"x/y\"
raw_base = \"https://example.invalid\"

[revisions.x]
tag = \"x\"
commit = \"0000000000000000000000000000000000000000\"

[revisions.x.files]
\"schema.json\" = \"not-a-digest\"
";
    let error = Manifest::parse(source).expect_err("non-hex digest must be refused");
    assert!(
        error.to_string().contains("64-character"),
        "unexpected error: {error}"
    );
}

#[test]
fn an_manifest_with_no_revisions_is_refused() {
    let source = "\
[meta]
schema_version = 1
repository = \"x/y\"
raw_base = \"https://example.invalid\"
";
    let error = Manifest::parse(source).expect_err("empty manifest must be refused");
    assert!(
        error.to_string().contains("no revisions"),
        "unexpected error: {error}"
    );
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
