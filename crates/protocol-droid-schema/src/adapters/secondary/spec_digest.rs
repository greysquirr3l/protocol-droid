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
/// Files are addressed by revision and then by name, so a revision's contents
/// live in a subdirectory of the spec root.
///
/// # Examples
///
/// ```
/// use std::fs;
/// use protocol_droid_schema::adapters::secondary::SpecDigestReader;
///
/// let root = std::env::temp_dir().join("protocol-droid-digest-doctest");
/// let revision = root.join("2025-06-18");
/// fs::create_dir_all(&revision).expect("create revision dir");
/// fs::write(revision.join("schema.json"), b"{}").expect("write file");
///
/// let reader = SpecDigestReader::new(&root);
/// // SHA-256 of "{}".
/// assert_eq!(
///     reader.digest("2025-06-18", "schema.json").expect("digest"),
///     "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
/// );
/// assert_eq!(reader.files_in("2025-06-18").expect("list"), ["schema.json"]);
///
/// fs::remove_dir_all(&root).expect("clean up");
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

#[cfg(test)]
mod tests {
    // Test code unwraps deliberately: the strict profile exists to keep
    // production paths fallible, and a test that cannot unwrap cannot assert.
    #![expect(
        clippy::expect_used,
        reason = "tests assert on fallible calls; propagating errors would replace a clear failure message with noise"
    )]

    use super::SpecDigestReader;

    /// Build a throwaway spec tree, returning its root and a guard that removes
    /// it on drop so a failing assertion cannot leave a directory behind.
    fn scratch(name: &str) -> (std::path::PathBuf, Scratch) {
        let root = std::env::temp_dir().join(format!("protocol-droid-digest-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        (root.clone(), Scratch(root))
    }

    struct Scratch(std::path::PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn digest_matches_the_known_sha256_of_its_contents() {
        let (root, _guard) = scratch("digest");
        std::fs::create_dir_all(root.join("2025-06-18")).expect("create revision dir");
        std::fs::write(root.join("2025-06-18/schema.json"), b"{}").expect("write schema");

        let reader = SpecDigestReader::new(&root);
        // SHA-256 of "{}".
        assert_eq!(
            reader.digest("2025-06-18", "schema.json").expect("digest"),
            "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        );
    }

    #[test]
    fn digest_of_a_missing_file_is_an_error_not_a_panic() {
        let (root, _guard) = scratch("missing");
        std::fs::create_dir_all(&root).expect("create root");
        let reader = SpecDigestReader::new(&root);
        let error = reader
            .digest("2025-06-18", "schema.json")
            .expect_err("must fail");
        assert!(
            error.contains("2025-06-18"),
            "error should name the path: {error}"
        );
    }

    #[test]
    fn files_in_lists_only_files_sorted() {
        let (root, _guard) = scratch("files-in");
        let revision = root.join("2025-06-18");
        std::fs::create_dir_all(&revision).expect("create revision dir");
        // Created out of order, and with a subdirectory that must be ignored.
        std::fs::write(revision.join("schema.ts"), b"ts").expect("write");
        std::fs::write(revision.join("CHANGELOG.md"), b"ch").expect("write");
        std::fs::write(revision.join("schema.json"), b"{}").expect("write");
        std::fs::create_dir_all(revision.join("nested")).expect("create subdir");

        let reader = SpecDigestReader::new(&root);
        assert_eq!(
            reader.files_in("2025-06-18").expect("list"),
            vec!["CHANGELOG.md", "schema.json", "schema.ts"]
        );
    }

    #[test]
    fn files_in_of_a_missing_revision_is_an_error() {
        let (root, _guard) = scratch("missing-revision");
        std::fs::create_dir_all(&root).expect("create root");
        let reader = SpecDigestReader::new(&root);
        reader.files_in("nope").expect_err("must fail");
    }

    #[test]
    fn read_manifest_reads_from_the_spec_root() {
        let (root, _guard) = scratch("read-manifest");
        std::fs::create_dir_all(&root).expect("create root");
        std::fs::write(root.join("MANIFEST.toml"), "schema_version = 1\n").expect("write manifest");

        let reader = SpecDigestReader::new(&root);
        assert_eq!(
            reader.read_manifest().expect("read"),
            "schema_version = 1\n"
        );
    }

    #[test]
    fn read_manifest_of_a_missing_file_is_an_error() {
        let (root, _guard) = scratch("no-manifest");
        std::fs::create_dir_all(&root).expect("create root");
        SpecDigestReader::new(&root)
            .read_manifest()
            .expect_err("must fail");
    }

    #[test]
    fn path_for_and_root_locate_the_same_file() {
        let reader = SpecDigestReader::new("/tmp/example-spec-root");
        assert_eq!(reader.root().to_str(), Some("/tmp/example-spec-root"));
        assert_eq!(
            reader.path_for("2025-06-18", "schema.json"),
            std::path::Path::new("/tmp/example-spec-root/2025-06-18/schema.json")
        );
    }
}
