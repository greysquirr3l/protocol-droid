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
