//! Use case: verify the vendored spec tree against its provenance manifest.
//!
//! The verification itself is pure and lives in [`crate::domain::manifest`].
//! This module is the use case that binds it to the filesystem adapter, so that
//! callers — `build.rs`, the integration test, and a future CLI subcommand —
//! all go through one entry point rather than each wiring the pieces up.

use std::path::Path;

use crate::adapters::secondary::SpecDigestReader;
use crate::domain::manifest::{Manifest, ManifestError};

/// Load and verify the vendored spec tree rooted at `spec_root`.
///
/// `spec_root` is the directory containing `MANIFEST.toml` and one
/// subdirectory per revision.
///
/// # Errors
///
/// Returns the underlying I/O error string if `MANIFEST.toml` or a revision
/// directory cannot be read, and a [`ManifestError`] if the manifest is
/// malformed, or if any vendored file does not match its recorded digest, is
/// missing, or is unaccounted for.
///
/// # Panics
///
/// Never. All failure paths are returned as `Err`.
pub fn verify_vendored_specs(spec_root: &Path) -> Result<Manifest, VerifyError> {
    let reader = SpecDigestReader::new(spec_root);
    let source = reader.read_manifest().map_err(VerifyError::Io)?;
    let manifest = Manifest::parse(&source)?;

    // `files_in` is the one fallible call inside the closure the pure verifier
    // takes, and that verifier's signature allows it to be infallible. Reading a
    // directory we just successfully read a manifest from is not a recoverable
    // failure, so a missing directory surfaces as an empty listing and the
    // verifier's own MissingFile check produces the real diagnostic.
    manifest
        .verify(
            |revision, file| reader.digest(revision, file),
            |revision| reader.files_in(revision).unwrap_or_default(),
        )
        .map_err(VerifyError::Manifest)?;

    Ok(manifest)
}

/// Why the vendored tree could not be verified.
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    /// The spec tree could not be read at all.
    #[error("could not read the vendored spec tree: {0}")]
    Io(String),
    /// The manifest is malformed, or the tree does not match it.
    #[error(transparent)]
    Manifest(#[from] ManifestError),
}

#[cfg(test)]
mod tests {
    // Test code unwraps deliberately: the strict profile exists to keep
    // production paths fallible, and a test that cannot unwrap cannot assert.
    #![expect(
        clippy::expect_used,
        reason = "tests assert on fallible calls; propagating errors would replace a clear failure message with noise"
    )]

    use super::{VerifyError, verify_vendored_specs};
    use std::path::{Path, PathBuf};

    /// The spec tree shipped with this crate.
    fn shipped_spec_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("spec")
    }

    #[test]
    fn the_shipped_tree_verifies() {
        let manifest = verify_vendored_specs(&shipped_spec_root())
            .expect("the vendored spec tree must match its manifest");
        assert!(
            manifest.revision_names().len() >= 6,
            "expected all revisions to be vendored"
        );
    }

    #[test]
    fn a_spec_root_without_a_manifest_is_an_io_error() {
        let scratch = std::env::temp_dir().join("protocol-droid-verify-no-manifest");
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).expect("create scratch");

        let error = verify_vendored_specs(&scratch).expect_err("must fail");
        assert!(
            matches!(error, VerifyError::Io(_)),
            "expected an Io error, got {error:?}"
        );
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn a_malformed_manifest_is_a_manifest_error() {
        let scratch = std::env::temp_dir().join("protocol-droid-verify-bad-manifest");
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).expect("create scratch");
        std::fs::write(scratch.join("MANIFEST.toml"), "not toml at all").expect("write manifest");

        let error = verify_vendored_specs(&scratch).expect_err("must fail");
        assert!(
            matches!(error, VerifyError::Manifest(_)),
            "expected a Manifest error, got {error:?}"
        );
        let _ = std::fs::remove_dir_all(&scratch);
    }
}
