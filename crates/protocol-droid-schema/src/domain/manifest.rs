//! Typed model of the vendored-spec provenance manifest.
//!
//! This module is **pure**: it parses a manifest from a string and verifies it
//! against an injected digest source. It performs no I/O, which is what lets
//! `build.rs` and the integration test share one implementation — `build.rs`
//! cannot depend on the crate it builds, so it `include!`s this file directly.
//! Keep it free of `crate::` references and of any dependency that is not in
//! `[build-dependencies]`, or the build script breaks.

use std::collections::BTreeMap;
use std::fmt;

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Lowercase hex SHA-256 of a byte slice.
///
/// Lives here, not in the filesystem adapter, because `build.rs` `include!`s
/// this file and needs the same digest function. Hashing is pure — it performs
/// no I/O — so it belongs in the domain regardless.
///
/// # Examples
///
/// ```
/// use protocol_droid_schema::domain::manifest::hex_digest;
///
/// assert_eq!(
///     hex_digest(b""),
///     "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
/// );
/// ```
#[must_use]
pub fn hex_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push(HEX_DIGITS[(byte >> 4) as usize]);
        out.push(HEX_DIGITS[(byte & 0x0f) as usize]);
    }
    out
}

const HEX_DIGITS: [char; 16] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f',
];

/// A vendored MCP specification revision, as declared in `spec/MANIFEST.toml`.
///
/// ```
/// use std::collections::BTreeMap;
/// use protocol_droid_schema::domain::manifest::RevisionSpec;
///
/// let spec = RevisionSpec {
///     tag: "2025-06-18".to_owned(),
///     commit: "f5ccad944fdf2b7d9cc70cf817f66ca5a8aa03a4".to_owned(),
///     changelog: Some("docs/specification/2025-06-18/changelog.mdx".to_owned()),
///     changelog_is_placeholder: false,
///     floating: false,
///     files: BTreeMap::new(),
/// };
/// assert!(!spec.is_floating());
/// assert!(!spec.changelog_is_authored());
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionSpec {
    /// Upstream git tag. Empty for a floating revision such as `draft`.
    #[serde(default)]
    pub tag: String,
    /// Full commit SHA the files were fetched from. Never a branch name.
    pub commit: String,
    /// Upstream path of the changelog. `None` when the changelog is authored
    /// rather than vendored (see [`RevisionSpec::is_digest_locked`]).
    #[serde(default)]
    pub changelog: Option<String>,
    /// True when upstream's changelog is a placeholder rather than real content.
    /// Currently only `draft`, whose changelog is a sentence saying changes will
    /// accumulate there. Recorded so a future reader does not read the stub as a
    /// vendoring defect in this repository.
    #[serde(default)]
    pub changelog_is_placeholder: bool,
    /// True for a revision with no upstream tag, currently only `draft`.
    #[serde(default)]
    pub floating: bool,
    /// File name to SHA-256, lowercase hex. A file absent from this map is not
    /// digest-locked and is not verified.
    #[serde(default)]
    pub files: BTreeMap<String, String>,
}

impl RevisionSpec {
    /// A revision is floating when upstream publishes no tag for it.
    #[must_use]
    pub fn is_floating(&self) -> bool {
        self.floating || self.tag.is_empty()
    }

    /// Whether `CHANGELOG.md` in this revision is digest-locked.
    ///
    /// A revision whose changelog is authored rather than vendored has no
    /// digest for it. That is the only supported reason for a revision to carry
    /// a `CHANGELOG.md` with no entry in [`RevisionSpec::files`].
    #[must_use]
    pub fn changelog_is_authored(&self) -> bool {
        self.changelog.is_none()
    }
}

/// Manifest metadata, excluding the per-revision tables.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestMeta {
    /// Version of this manifest format, checked against
    /// [`SUPPORTED_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Upstream repository, as `owner/name`.
    pub repository: String,
    /// Base URL for raw file downloads.
    pub raw_base: String,
    /// Tags deliberately not vendored, each mapped to the reason. Recording the
    /// exclusion keeps it a decision rather than an omission.
    #[serde(default)]
    pub excluded_tags: BTreeMap<String, String>,
}

/// The whole of `spec/MANIFEST.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Provenance metadata common to every revision.
    pub meta: ManifestMeta,
    /// Per-revision entries, keyed by revision name.
    pub revisions: BTreeMap<String, RevisionSpec>,
}

/// Ways a manifest can fail to parse or to match the tree it describes.
#[derive(Debug)]
pub enum ManifestError {
    /// The manifest is not valid TOML, or does not match the expected shape.
    Parse(String),
    /// `schema_version` is newer than this build understands.
    UnsupportedSchemaVersion {
        /// The version the manifest declares.
        found: u32,
        /// The newest version this build can read.
        supported: u32,
    },
    /// A commit SHA is not 40 lowercase hex characters.
    MalformedCommit {
        /// Revision the bad SHA belongs to.
        revision: String,
        /// The value that was recorded.
        commit: String,
    },
    /// A recorded digest is not 64 lowercase hex characters.
    MalformedDigest {
        /// Revision the bad digest belongs to.
        revision: String,
        /// File the digest was recorded for.
        file: String,
        /// The value that was recorded.
        digest: String,
    },
    /// The manifest declares no revisions.
    NoRevisions,
    /// The digest reader could not produce a digest (missing or unreadable file).
    Unreadable {
        /// Revision the file belongs to.
        revision: String,
        /// File that could not be read.
        file: String,
        /// The underlying I/O failure.
        reason: String,
    },
    /// A vendored file's content does not match the recorded digest.
    DigestMismatch {
        /// Revision the file belongs to.
        revision: String,
        /// File whose content changed.
        file: String,
        /// The digest the manifest records.
        expected: String,
        /// The digest the file on disk actually produces.
        actual: String,
    },
    /// A file exists in the revision directory but is neither digest-locked nor
    /// declared as an authored changelog. Catches a file vendored without being
    /// recorded, which would otherwise be verified by nothing.
    UnaccountedFile {
        /// Revision the file was found in.
        revision: String,
        /// File that nothing in the manifest accounts for.
        file: String,
    },
    /// A digest-locked file is recorded but absent from the tree.
    MissingFile {
        /// Revision the file was recorded for.
        revision: String,
        /// File the manifest records but the tree does not contain.
        file: String,
    },
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(msg) => write!(f, "could not parse MANIFEST.toml: {msg}"),
            Self::UnsupportedSchemaVersion { found, supported } => write!(
                f,
                "manifest schema_version {found} is newer than the supported {supported}"
            ),
            Self::MalformedCommit { revision, commit } => write!(
                f,
                "revision `{revision}` has commit `{commit}`, which is not a 40-character \
                 lowercase hex SHA"
            ),
            Self::MalformedDigest {
                revision,
                file,
                digest,
            } => write!(
                f,
                "revision `{revision}` records digest `{digest}` for `{file}`, which is not \
                 64-character lowercase hex"
            ),
            Self::NoRevisions => {
                write!(f, "manifest declares no revisions")
            }
            Self::Unreadable {
                revision,
                file,
                reason,
            } => write!(
                f,
                "revision `{revision}`: could not read `{file}`: {reason}"
            ),
            Self::DigestMismatch {
                revision,
                file,
                expected,
                actual,
            } => write!(
                f,
                "revision `{revision}`: `{file}` digest mismatch\n  expected {expected}\n  \
                 actual   {actual}\n  the vendored file has changed without the manifest being \
                 refreshed — run `cargo xtask refresh-spec {revision}`"
            ),
            Self::UnaccountedFile { revision, file } => write!(
                f,
                "revision `{revision}`: `{file}` is on disk but is neither digest-locked nor \
                 declared as an authored changelog"
            ),
            Self::MissingFile { revision, file } => write!(
                f,
                "revision `{revision}`: `{file}` is recorded in the manifest but missing on disk"
            ),
        }
    }
}

impl std::error::Error for ManifestError {}

/// The manifest schema version this build understands.
pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

fn is_lower_hex(value: &str, expected_len: usize) -> bool {
    value.len() == expected_len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Manifest {
    /// Parse and structurally validate a manifest.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::Parse`] if the TOML is malformed or does not
    /// match the expected shape, [`ManifestError::UnsupportedSchemaVersion`] if
    /// it was written by a newer tool, [`ManifestError::NoRevisions`] if it
    /// declares nothing, and [`ManifestError::MalformedCommit`] or
    /// [`ManifestError::MalformedDigest`] if a recorded value is not the shape
    /// it claims to be.
    pub fn parse(source: &str) -> Result<Self, ManifestError> {
        let manifest: Self =
            toml::from_str(source).map_err(|err| ManifestError::Parse(err.to_string()))?;

        if manifest.meta.schema_version > SUPPORTED_SCHEMA_VERSION {
            return Err(ManifestError::UnsupportedSchemaVersion {
                found: manifest.meta.schema_version,
                supported: SUPPORTED_SCHEMA_VERSION,
            });
        }
        if manifest.revisions.is_empty() {
            return Err(ManifestError::NoRevisions);
        }

        for (name, revision) in &manifest.revisions {
            if !is_lower_hex(&revision.commit, 40) {
                return Err(ManifestError::MalformedCommit {
                    revision: name.clone(),
                    commit: revision.commit.clone(),
                });
            }
            for (file, digest) in &revision.files {
                if !is_lower_hex(digest, 64) {
                    return Err(ManifestError::MalformedDigest {
                        revision: name.clone(),
                        file: file.clone(),
                        digest: digest.clone(),
                    });
                }
            }
        }

        Ok(manifest)
    }

    /// Verify every recorded digest and reconcile the manifest against the tree.
    ///
    /// `digest_of` is injected so this stays pure: the caller supplies the I/O.
    /// `files_on_disk` lists the file names present in each revision directory.
    ///
    /// The check runs in both directions. A recorded file that is missing, or a
    /// file on disk that nothing accounts for, is an error. A revision whose
    /// changelog is authored has one legitimately unlisted file, `CHANGELOG.md`.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::Unreadable`] when a digest cannot be produced,
    /// [`ManifestError::DigestMismatch`] when content does not match,
    /// [`ManifestError::MissingFile`] when a recorded file is absent, or
    /// [`ManifestError::UnaccountedFile`] when a file is present but neither
    /// digest-locked nor declared authored.
    pub fn verify<F, G>(&self, mut digest_of: F, files_on_disk: G) -> Result<(), ManifestError>
    where
        F: FnMut(&str, &str) -> Result<String, String>,
        G: Fn(&str) -> Vec<String>,
    {
        for (name, revision) in &self.revisions {
            for (file, expected) in &revision.files {
                let actual = digest_of(name, file).map_err(|reason| ManifestError::Unreadable {
                    revision: name.clone(),
                    file: file.clone(),
                    reason,
                })?;
                if &actual != expected {
                    return Err(ManifestError::DigestMismatch {
                        revision: name.clone(),
                        file: file.clone(),
                        expected: expected.clone(),
                        actual,
                    });
                }
            }

            // One directory read per revision, reused for both directions of
            // the reconciliation.
            let on_disk = files_on_disk(name);

            let mut accounted: Vec<&str> = revision.files.keys().map(String::as_str).collect();
            if revision.changelog_is_authored() {
                accounted.push("CHANGELOG.md");
            }

            for file in &on_disk {
                if !accounted.contains(&file.as_str()) {
                    return Err(ManifestError::UnaccountedFile {
                        revision: name.clone(),
                        file: file.clone(),
                    });
                }
            }
            for file in &accounted {
                if !on_disk.iter().any(|present| present == file) {
                    return Err(ManifestError::MissingFile {
                        revision: name.clone(),
                        file: (*file).to_owned(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Revision names in the manifest, sorted.
    #[must_use]
    pub fn revision_names(&self) -> Vec<&str> {
        self.revisions.keys().map(String::as_str).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{Manifest, ManifestError, RevisionSpec, SUPPORTED_SCHEMA_VERSION, hex_digest};

    /// A manifest with one tagged revision whose files digest to these values.
    fn sample_manifest(files: &str) -> String {
        format!(
            "[meta]\nschema_version = {SUPPORTED_SCHEMA_VERSION}\n\
             repository = \"owner/name\"\nraw_base = \"https://example.invalid\"\n\
             \n[revisions.2025-06-18]\ntag = \"2025-06-18\"\n\
             commit = \"0000000000000000000000000000000000000000\"\n\
             {files}"
        )
    }

    const ZERO: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    #[test]
    fn hex_digest_matches_the_published_sha256_of_the_empty_string() {
        assert_eq!(
            hex_digest(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn hex_digest_is_lowercase_hex_of_the_right_width() {
        let digest = hex_digest(b"protocol-droid");
        assert_eq!(digest.len(), 64);
        assert!(
            digest
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        );
    }

    #[test]
    fn a_valid_manifest_parses() {
        let source = sample_manifest(&format!(
            "\n[revisions.2025-06-18.files]\n\"schema.json\" = \"{ZERO}\"\n"
        ));
        let manifest = Manifest::parse(&source).expect("manifest must parse");
        assert_eq!(manifest.revision_names(), vec!["2025-06-18"]);
        assert_eq!(manifest.revisions.len(), 1);
    }

    #[test]
    fn a_newer_schema_version_is_refused() {
        let source = sample_manifest("").replace(
            &format!("schema_version = {SUPPORTED_SCHEMA_VERSION}"),
            "schema_version = 99",
        );
        let error = Manifest::parse(&source).expect_err("future version must be refused");
        assert!(matches!(
            error,
            ManifestError::UnsupportedSchemaVersion { found: 99, .. }
        ));
    }

    #[test]
    fn a_manifest_with_no_revisions_is_refused() {
        let source = format!(
            "[meta]\nschema_version = {SUPPORTED_SCHEMA_VERSION}\n\
             repository = \"owner/name\"\nraw_base = \"https://example.invalid\"\n"
        );
        let error = Manifest::parse(&source).expect_err("empty manifest must be refused");
        assert!(matches!(error, ManifestError::NoRevisions));
    }

    #[test]
    fn a_commit_that_is_not_a_full_sha_is_refused() {
        let source =
            sample_manifest("").replace("0000000000000000000000000000000000000000", "main");
        let error = Manifest::parse(&source).expect_err("branch name must be refused");
        assert!(matches!(error, ManifestError::MalformedCommit { .. }));
    }

    #[test]
    fn an_uppercase_commit_is_refused() {
        let source = sample_manifest("").replace(
            "0000000000000000000000000000000000000000",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        );
        let error = Manifest::parse(&source).expect_err("uppercase SHA must be refused");
        assert!(matches!(error, ManifestError::MalformedCommit { .. }));
    }

    #[test]
    fn a_digest_that_is_not_64_hex_characters_is_refused() {
        let source = sample_manifest(&format!(
            "\n[revisions.2025-06-18.files]\n\"schema.json\" = \"abc123\"\n"
        ));
        let error = Manifest::parse(&source).expect_err("short digest must be refused");
        assert!(matches!(error, ManifestError::MalformedDigest { .. }));
    }

    #[test]
    fn malformed_toml_is_refused() {
        let error = Manifest::parse("this is not toml").expect_err("garbage must be refused");
        assert!(matches!(error, ManifestError::Parse(_)));
    }

    #[test]
    fn an_unknown_key_is_refused() {
        // A typo in the manifest must not be silently ignored, or a file could
        // go unverified while the manifest still looks complete.
        let source = sample_manifest("").replace("tag = \"2025-06-18\"", "tg = \"2025-06-18\"");
        let error = Manifest::parse(&source).expect_err("unknown key must be refused");
        assert!(matches!(error, ManifestError::Parse(_)));
    }

    /// Verify against in-memory content, so the pure check needs no filesystem.
    fn verify_against(source: &str, content: &[(&str, &str)]) -> Result<(), ManifestError> {
        let manifest = Manifest::parse(source).expect("manifest must parse");
        manifest.verify(
            |revision, file| {
                let key = format!("{revision}/{file}");
                content
                    .iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, bytes)| hex_digest(bytes.as_bytes()))
                    .ok_or_else(|| format!("no such file {key}"))
            },
            |revision| {
                content
                    .iter()
                    .filter(|(name, _)| name.starts_with(&format!("{revision}/")))
                    .map(|(name, _)| name.split('/').nth(1).unwrap_or_default().to_owned())
                    .collect()
            },
        )
    }

    #[test]
    fn matching_digests_verify() {
        let source = sample_manifest(&format!(
            "\n[revisions.2025-06-18.files]\n\"schema.json\" = \"{}\"\n",
            hex_digest(b"{}")
        ));
        let result = verify_against(&source, &[("2025-06-18/schema.json", "{}")]);
        assert!(result.is_ok(), "expected ok, got {result:?}");
    }

    #[test]
    fn a_changed_file_is_a_digest_mismatch() {
        let source = sample_manifest(&format!(
            "\n[revisions.2025-06-18.files]\n\"schema.json\" = \"{}\"\n",
            hex_digest(b"{}")
        ));
        let error = verify_against(
            &source,
            &[("2025-06-18/schema.json", r#"{"tampered":true}"#)],
        )
        .expect_err("tampering must be detected");
        assert!(matches!(error, ManifestError::DigestMismatch { .. }));
        // The message must name the command that fixes it.
        let rendered = error.to_string();
        assert!(
            rendered.contains("refresh-spec 2025-06-18"),
            "error should name the refresh command, got: {rendered}"
        );
    }

    #[test]
    fn an_unreadable_file_is_reported_as_unreadable() {
        let source = sample_manifest(&format!(
            "\n[revisions.2025-06-18.files]\n\"schema.json\" = \"{ZERO}\"\n"
        ));
        let error = verify_against(&source, &[]).expect_err("a missing file must be reported");
        assert!(matches!(error, ManifestError::Unreadable { .. }));
    }

    #[test]
    fn a_file_on_disk_that_nothing_accounts_for_is_refused() {
        let source = sample_manifest(&format!(
            "\n[revisions.2025-06-18.files]\n\"schema.json\" = \"{}\"\n",
            hex_digest(b"{}")
        ));
        let error = verify_against(
            &source,
            &[
                ("2025-06-18/schema.json", "{}"),
                ("2025-06-18/schema-extra.json", "{}"),
            ],
        )
        .expect_err("an unrecorded file must be refused");
        assert!(matches!(error, ManifestError::UnaccountedFile { .. }));
    }

    #[test]
    fn a_recorded_file_missing_from_disk_is_refused() {
        let source = sample_manifest(&format!(
            "\n[revisions.2025-06-18.files]\n\"schema.json\" = \"{}\"\n",
            hex_digest(b"{}")
        ));
        // The digest closure would succeed, but the directory listing is empty.
        let manifest = Manifest::parse(&source).expect("manifest must parse");
        let error = manifest
            .verify(|_, _| Ok(ZERO.to_owned()), |_| Vec::new())
            .expect_err("a missing recorded file must be refused");
        assert!(matches!(error, ManifestError::MissingFile { .. }));
    }

    #[test]
    fn an_authored_changelog_is_accepted_without_a_digest() {
        // 2024-11-05 ships no upstream changelog, so its CHANGELOG.md is
        // hand-written. It must be tolerated on disk without a recorded digest,
        // and without the digest changing when the prose is edited.
        let source = format!(
            "[meta]\nschema_version = {SUPPORTED_SCHEMA_VERSION}\n\
             repository = \"owner/name\"\nraw_base = \"https://example.invalid\"\n\
             \n[revisions.2024-11-05]\ntag = \"2024-11-05\"\n\
             commit = \"0000000000000000000000000000000000000000\"\n\
             \n[revisions.2024-11-05.files]\n\"schema.json\" = \"{}\"\n",
            hex_digest(b"{}")
        );
        let result = verify_against(
            &source,
            &[
                ("2024-11-05/schema.json", "{}"),
                ("2024-11-05/CHANGELOG.md", "# hand written, edited freely"),
            ],
        );
        assert!(result.is_ok(), "expected ok, got {result:?}");
    }

    #[test]
    fn a_vendored_changelog_must_be_digest_locked() {
        // The mirror image: a changelog that *is* vendored must be recorded, or
        // it could be swapped for a different document without detection.
        let source = format!(
            "[meta]\nschema_version = {SUPPORTED_SCHEMA_VERSION}\n\
             repository = \"owner/name\"\nraw_base = \"https://example.invalid\"\n\
             \n[revisions.2025-06-18]\ntag = \"2025-06-18\"\n\
             commit = \"0000000000000000000000000000000000000000\"\n\
             changelog = \"docs/specification/2025-06-18/changelog.mdx\"\n\
             \n[revisions.2025-06-18.files]\n\"schema.json\" = \"{}\"\n",
            hex_digest(b"{}")
        );
        let error = verify_against(
            &source,
            &[
                ("2025-06-18/schema.json", "{}"),
                ("2025-06-18/CHANGELOG.md", "# vendored"),
            ],
        )
        .expect_err("a vendored changelog must be digest-locked");
        assert!(matches!(error, ManifestError::UnaccountedFile { .. }));
    }

    #[test]
    fn floating_state_follows_the_absence_of_a_tag() {
        let draft = RevisionSpec {
            tag: String::new(),
            commit: "0".repeat(40),
            changelog: Some("docs/specification/draft/changelog.mdx".to_owned()),
            changelog_is_placeholder: true,
            floating: true,
            files: std::collections::BTreeMap::new(),
        };
        assert!(draft.is_floating());
        assert!(!draft.changelog_is_authored());

        let tagged = RevisionSpec {
            tag: "2026-07-28".to_owned(),
            ..draft
        };
        assert!(!tagged.is_floating());
    }

    #[test]
    fn an_authored_changelog_is_marked_by_an_absent_key() {
        let authored = RevisionSpec {
            tag: "2024-11-05".to_owned(),
            commit: "0".repeat(40),
            changelog: None,
            changelog_is_placeholder: false,
            floating: false,
            files: std::collections::BTreeMap::new(),
        };
        assert!(authored.changelog_is_authored());
    }

    #[test]
    fn excluded_tags_are_reported_as_metadata() {
        let source = format!(
            "[meta]\nschema_version = {SUPPORTED_SCHEMA_VERSION}\n\
             repository = \"owner/name\"\nraw_base = \"https://example.invalid\"\n\
             \n[meta.excluded_tags]\n\"2024-10-07\" = \"Superseded.\"\n\
             \n[revisions.2025-06-18]\ntag = \"2025-06-18\"\n\
             commit = \"0000000000000000000000000000000000000000\"\n"
        );
        let manifest = Manifest::parse(&source).expect("manifest must parse");
        assert_eq!(
            manifest
                .meta
                .excluded_tags
                .get("2024-10-07")
                .map(String::as_str),
            Some("Superseded.")
        );
        assert_eq!(manifest.meta.repository, "owner/name");
        assert_eq!(manifest.meta.raw_base, "https://example.invalid");
    }
}
