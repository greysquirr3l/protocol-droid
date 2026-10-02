//! Filesystem and hashing for the vendored spec tree.
//!
//! The domain model in [`crate::domain::manifest`] is pure and takes a digest
//! source as a parameter. This module is the secondary adapter that supplies it:
//! it reads files off disk and hashes them. All I/O for spec verification lives
//! here so the domain model stays testable without a filesystem.

use std::fs;
use std::path::{Path, PathBuf};

/// Computes SHA-256 digests of files in a vendored spec tree.
///
/// # Examples
///
/// ```
/// use std::fs;
/// use protocol_droid_schema::adapters::secondary::SpecDigestReader;
///
/// let dir = std::env::temp_dir().join("protocol-droid-digest-doctest");
/// fs::create_dir_all(&dir).expect("create temp dir");
/// fs::write(dir.join("schema.json"), b"{}").expect("write file");
///
/// let reader = SpecDigestReader::new(dir.clone());
/// let digest = reader.digest("2025-06-18", "schema.json").expect("digest");
/// assert_eq!(digest.len(), 64);
///
/// fs::remove_dir_all(&dir).expect("clean up");
/// ```
#[derive(Debug, Clone)]
pub struct SpecDigestReader {
    root: PathBuf,
}

impl SpecDigestReader {
    /// Root of the spec tree, i.e. the directory holding `MANIFEST.toml`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The vendored root this reader is bound to.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path of one file within a revision directory.
    #[must_use]
    pub fn path_for(&self, revision: &str, file: &str) -> PathBuf {
        self.root.join(revision).join(file)
    }

    /// Lowercase hex SHA-256 of one vendored file.
    ///
    /// # Errors
    ///
    /// Returns the underlying I/O error string if the file is missing or cannot
    /// be read.
    pub fn digest(&self, revision: &str, file: &str) -> Result<String, String> {
        let path = self.path_for(revision, file);
        let bytes = fs::read(&path).map_err(|err| format!("{}: {err}", path.display()))?;
        Ok(hex_digest(&bytes))
    }

    /// File names present in a revision directory, sorted.
    ///
    /// # Errors
    ///
    /// Returns the underlying I/O error string if the directory is missing or
    /// cannot be read.
    pub fn files_in(&self, revision: &str) -> Result<Vec<String>, String> {
        let dir = self.root.join(revision);
        let entries = fs::read_dir(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|err| format!("{}: {err}", dir.display()))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if entry
                .file_type()
                .map_err(|err| format!("{}: {err}", entry.path().display()))?
                .is_file()
            {
                names.push(name.to_owned());
            }
        }
        names.sort();
        Ok(names)
    }

    /// Read and parse `MANIFEST.toml` from the spec root.
    ///
    /// # Errors
    ///
    /// Returns the underlying I/O error string if the manifest is unreadable.
    pub fn read_manifest(&self) -> Result<String, String> {
        let path = self.root.join("MANIFEST.toml");
        fs::read_to_string(&path).map_err(|err| format!("{}: {err}", path.display()))
    }
}

/// Lowercase hex SHA-256 of a byte slice.
///
/// Re-exported from [`crate::domain::manifest::hex_digest`] so the hashing
/// algorithm has exactly one implementation, shared with `build.rs`.
///
/// # Examples
///
/// ```
/// use protocol_droid_schema::adapters::secondary::spec_digest::hex_digest;
///
/// assert_eq!(
///     hex_digest(b""),
///     "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
/// );
/// ```
pub use crate::domain::manifest::hex_digest;
