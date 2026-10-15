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
    // `LowerHex` on a SHA-256 output already emits lowercase hex, one byte as
    // two digits, which is exactly the manifest's format. Formatting the whole
    // digest at once avoids a per-nibble lookup table indexed by a masked byte.
    format!("{digest:x}")
}

/// Normalize CRLF to LF for digest computation.
///
/// The vendored spec tree is checked in as LF and pinned to LF by
/// `.gitattributes`, but the build still sees whatever bytes a contributor
/// happens to have on disk — and a Windows checkout with `core.autocrlf=true`
/// will hand a vendored file back as CRLF even when the index entry is LF.
/// Normalising before hashing makes the digest match the LF content the
/// manifest was recorded against, so a Windows build does not fail with a
/// spurious "digest mismatch" on every run.
///
/// The function is conservative: it only rewrites `\r\n` to `\n`, and leaves
/// any trailing lone `\r` alone (none of the spec formats use it). The
/// already-correct path returns the input bytes without allocating.
///
/// # Examples
///
/// ```
/// use protocol_droid_schema::domain::manifest::normalize_line_endings;
///
/// let crlf = b"line one\r\nline two\r\n";
/// let lf = b"line one\nline two\n";
/// assert_eq!(normalize_line_endings(crlf), lf);
/// ```
// `dead_code` fires in `xtask` because xtask `include!`s this file but does
// not use the normaliser. The library uses it, `build.rs` uses it, and the
// integration test uses it; xtask only reads the manifest for refresh
// operations that do not touch byte content, so the crate-level allow in
// `xtask/src/main.rs` covers the `dead_code` part. `dead_code_pub_in_binary`
// is also set to allow in xtask for the same reason — it is an include!d
// shared file, not a freshly-introduced dead public symbol.
#[allow(
    dead_code,
    reason = "xtask include!s this file but does not normalise; the helper is used in the library, build.rs, and the integration test"
)]
#[must_use]
pub fn normalize_line_endings(bytes: &[u8]) -> Vec<u8> {
    if !bytes.windows(2).any(|w| w == b"\r\n") {
        return bytes.to_vec();
    }
    // CRLF appears in the input, so we have to walk every byte. The `Peekable`
    // adapter lets the iterator look one byte ahead without taking it; when
    // the next two bytes are `\r` `\n` we emit a single `\n` and advance past
    // both, otherwise we emit the current byte and let the iterator move on.
    // No index arithmetic: every read is through the iterator, which the
    // borrow checker (and `clippy::indexing_slicing`) accepts.
    let mut iter = bytes.iter().copied().peekable();
    let mut out = Vec::with_capacity(bytes.len());
    while let Some(byte) = iter.next() {
        if byte == b'\r' && iter.peek() == Some(&b'\n') {
            let _ = iter.next();
            out.push(b'\n');
        } else {
            out.push(byte);
        }
    }
    out
}

/// A vendored MCP specification revision, as declared in `spec/MANIFEST.toml`.
///
/// ```
/// use std::collections::BTreeMap;
/// use protocol_droid_schema::domain::manifest::RevisionSpec;
///
/// let spec = RevisionSpec {
///     tag: "2025-06-18".to_owned(),
///     commit: "f5ccad944fdf2b7d9cc70cf817f66ca5a8aa03a4".to_owned(),
///     schema_prefix: "schema/2025-06-18".to_owned(),
///     changelog: Some("docs/specification/2025-06-18/changelog.mdx".to_owned()),
///     changelog_is_placeholder: false,
///     floating: false,
///     branch: None,
///     files: BTreeMap::new(),
/// };
/// assert!(!spec.is_floating());
/// assert!(!spec.changelog_is_authored());
/// assert_eq!(
///     spec.upstream_path("schema.json").expect("a schema always has a path"),
///     "schema/2025-06-18/schema.json"
/// );
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionSpec {
    /// Upstream git tag. Empty for a floating revision such as `draft`.
    #[serde(default)]
    pub tag: String,
    /// Full commit SHA the files were fetched from. Never a branch name.
    pub commit: String,
    /// Upstream directory holding `schema.json` and `schema.ts`, relative to
    /// the repository root. Recorded rather than derived because upstream is
    /// not consistent: 2024-11-05 keeps them at `schema/`, every later revision
    /// at `schema/<version>/`, and draft at `schema/draft/`. Hardcoding that
    /// in the refresh tool would put a per-revision special case in the one
    /// place that cannot afford one.
    pub schema_prefix: String,
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
    /// Upstream branch a floating revision tracks. Meaningless for a tagged
    /// revision, and required for a floating one: a refresh must resolve this
    /// branch to a commit, because "whatever main points at right now" is not
    /// a reproducible pin.
    #[serde(default)]
    pub branch: Option<String>,
    /// File name to SHA-256, lowercase hex. A file absent from this map is not
    /// digest-locked and is not verified.
    #[serde(default)]
    pub files: BTreeMap<String, String>,
}

impl RevisionSpec {
    /// The two files every revision vendors, in a stable order.
    pub const SCHEMA_FILES: [&'static str; 2] = ["schema.json", "schema.ts"];

    /// Upstream path of a file within this revision.
    ///
    /// # Errors
    ///
    /// Returns a [`ManifestError::UnaccountedFile`] when `file` is neither one
    /// of the two schema representations nor the changelog, because there is
    /// then no upstream path to derive.
    pub fn upstream_path(&self, file: &str) -> Result<String, ManifestError> {
        if Self::SCHEMA_FILES.contains(&file) {
            return Ok(format!("{}/{file}", self.schema_prefix));
        }
        match (&self.changelog, file) {
            (Some(changelog), "CHANGELOG.md") => Ok(changelog.clone()),
            _ => Err(ManifestError::UnaccountedFile {
                revision: self.tag.clone(),
                file: file.to_owned(),
            }),
        }
    }

    /// The branch a floating revision tracks, or `None` for a tagged one.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::MalformedCommit`] when a floating revision
    /// records no branch, since a refresh would then have nothing to resolve.
    ///
    /// Not a `const fn`: `Option::as_deref` and `ok_or_else` are not
    /// const-stable on this toolchain, so `missing_const_for_fn` cannot be
    /// satisfied here and must not be silenced.
    pub fn floating_branch(&self) -> Result<&str, ManifestError> {
        if !self.is_floating() {
            return Ok("");
        }
        self.branch
            .as_deref()
            .ok_or_else(|| ManifestError::MalformedCommit {
                revision: self.tag.clone(),
                commit: "<no branch recorded>".to_owned(),
            })
    }
}

impl RevisionSpec {
    /// A revision is floating when upstream publishes no tag for it.
    #[must_use]
    pub const fn is_floating(&self) -> bool {
        self.floating || self.tag.is_empty()
    }

    /// Whether `CHANGELOG.md` in this revision is digest-locked.
    ///
    /// A revision whose changelog is authored rather than vendored has no
    /// digest for it. That is the only supported reason for a revision to carry
    /// a `CHANGELOG.md` with no entry in [`RevisionSpec::files`].
    #[must_use]
    pub const fn changelog_is_authored(&self) -> bool {
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

/// One normative document the MCP authorization sections cite, vendored so its
/// wire shapes can be read rather than remembered.
///
/// Not an RFC-only notion — the model is "a cited external document with a
/// stable URL", which is also how an IETF draft would be pinned. The name is
/// narrow because every document in the table today is an RFC.
///
/// A document lives in one flat directory (`spec/rfc/`) rather than in a
/// revision directory, because it is cited by *several* revisions and copying
/// it per revision would mean N copies that could drift.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RfcDocument {
    /// Human-readable title, used in the README and in error messages.
    pub title: String,
    /// Authoritative URL the text is fetched from. Never reconstructed from
    /// the document name: the IETF serves RFCs from several hosts and a
    /// derived URL would silently point somewhere unverified.
    pub url: String,
    /// File name inside the shared document directory.
    pub file: String,
    /// SHA-256 of the vendored file, lowercase hex. Same meaning as
    /// [`RevisionSpec::files`]: content-addressed, so a re-fetch that changes
    /// the text is a visible diff rather than a silent upgrade.
    pub sha256: String,
    /// Protocol revisions whose authorization section cites this document.
    ///
    /// Recorded rather than inferred. The mapping is a judgement about which
    /// revision's prose requires what, and that judgement belongs in a diff
    /// that a reviewer reads, not in code that recomputes it from the
    /// changelog.
    #[serde(default)]
    pub cited_by: Vec<String>,
}

/// Per-revision entries for the A2A protocol.
///
/// A2A's canonical wire definition is a proto file (`a2aproject/A2A`),
/// not a JSON Schema. The schema crate is JSON-Schema-shaped today, so
/// the A2A entries here are recorded for *provenance* — the digest
/// locks the proto against a silent re-fetch — without a per-version
/// JSON Schema to verify. A future revision of the schema crate that
/// adds a proto path can use the same `files` map the MCP entries
/// use; the field shape is identical.
///
/// The `package` field mirrors the proto file's `package` declaration
/// (`lf.a2a.v1`, etc.), so the codegen can route per-version types to
/// the right Rust module without a per-revision hand-edit.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct A2aRevisionSpec {
    /// Upstream git tag, e.g. `v1.0.0`.
    pub tag: String,
    /// Full commit SHA the proto was fetched from. Never a branch name.
    pub commit: String,
    /// Proto `package` declaration. Distinct revisions can share a
    /// `package`; the field is for codegen, not for ordering.
    pub package: String,
    /// Upstream directory holding the proto, relative to the
    /// repository root. Recorded rather than derived because the A2A
    /// repo's layout is the way it is, not a rule this crate can
    /// assume.
    pub spec_prefix: String,
    /// File name to SHA-256, lowercase hex. A file absent from this
    /// map is not digest-locked and is not verified. The same
    /// provenance story as [`RevisionSpec::files`].
    #[serde(default)]
    pub files: BTreeMap<String, String>,
}

/// Per-revision entry for the ACP table.
///
/// The shape is the same as [`A2aRevisionSpec`] without the
/// proto `package` field (ACP is a JSON Schema, not a proto)
/// and with the `meta.json` filename constraint lifted: the
/// A2A entry is a single proto, the ACP entry is the schema
/// plus a hand-written `meta.json` plus a `CHANGELOG.md`.
#[derive(Debug, Clone, Deserialize)]
pub struct AcpRevisionSpec {
    /// Upstream git tag, e.g. `schema-v1.25.0`. The `schema-v`
    /// prefix is the agent-client-protocol repo's
    /// release-tagging convention; the tag is what the
    /// manifest pins, not the part after the `v`.
    pub tag: String,
    /// Full commit SHA the schema was fetched from. Never a
    /// branch name.
    pub commit: String,
    /// Upstream directory holding the schema, relative to the
    /// repository root. The `schema/v1` prefix is the path
    /// inside the agent-client-protocol repo.
    pub spec_prefix: String,
    /// File name to SHA-256, lowercase hex. A file absent
    /// from this map is not digest-locked and is not verified.
    /// The same provenance story as [`A2aRevisionSpec::files`].
    #[serde(default)]
    pub files: BTreeMap<String, String>,
}

/// The whole of `spec/MANIFEST.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Provenance metadata common to every revision.
    pub meta: ManifestMeta,
    /// Per-revision entries, keyed by revision name. Defaults to empty so that
    /// a manifest with no `[revisions]` table parses and is rejected by
    /// [`Manifest::parse`] with [`ManifestError::NoRevisions`], which is a far
    /// more useful diagnostic than a serde "missing field" error.
    #[serde(default)]
    pub revisions: BTreeMap<String, RevisionSpec>,
    /// Cited external documents, keyed by a short stable identifier such as
    /// `rfc7636`. Defaults to empty so a manifest predating this table keeps
    /// parsing; absence is a claim that nothing external is cited, which the
    /// same "recorded exclusion rather than silent gap" rule as
    /// [`ManifestMeta::excluded_tags`] makes worth stating explicitly.
    #[serde(default)]
    pub rfc_documents: BTreeMap<String, RfcDocument>,
    /// A2A protocol revisions. Defaults to empty so a manifest
    /// predating Phase 9 keeps parsing. The field lives here rather
    /// than in a separate file because the MCP and A2A tables are
    /// both *what is vendored*; the shape difference (JSON Schema
    /// vs proto) is the per-table detail, not a structural
    /// difference.
    #[serde(default)]
    pub a2a_revisions: BTreeMap<String, A2aRevisionSpec>,
    /// ACP protocol revisions. Defaults to empty so a manifest
    /// predating Phase 9 keeps parsing. Same "what is vendored"
    /// rationale as the A2A field above; ACP is a JSON Schema
    /// plus a hand-written `meta.json` plus a `CHANGELOG.md`,
    /// and the per-table `files` map names the three.
    #[serde(default)]
    pub acp_revisions: BTreeMap<String, AcpRevisionSpec>,
}

/// Ways a manifest can fail to parse or to match the tree it describes.
///
/// The `revision` field of a tree error carries the owning table's key, so for
/// a cited document it is the document identifier rather than a protocol
/// revision. The two trees are different shapes and sharing the variants keeps
/// one error type for "the vendored tree does not match the manifest" without
/// a flag for which tree it was; the message is worded to stay true either way,
/// and remediation is left to the build script, which knows which command
/// applies.
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
                 refreshed"
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

/// Directory, relative to the spec root, holding the cited external documents.
///
/// One flat directory rather than one per revision: a document like RFC 7636 is
/// cited by several revisions, and a copy per revision would be N copies free to
/// drift apart.
pub const CITED_DOCUMENT_DIR: &str = "cited";

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

    /// Checks the cited-document directory against the manifest.
    ///
    /// Separate from [`Manifest::verify`] because the two trees are different
    /// shapes, not two instances of one: revisions are directories fetched from
    /// a single git repository at a pinned commit, while cited documents are a
    /// flat directory fetched from unrelated hosts. Folding them together would
    /// mean one method carrying a flag for which of the two it was walking.
    ///
    /// The contract is the same in both directions. Every recorded document is
    /// present with the recorded digest, and every file present is recorded —
    /// the second half is what makes an *added* document fail the build until
    /// someone has said where it came from.
    ///
    /// # Errors
    ///
    /// [`ManifestError::Unreadable`] when a digest cannot be produced,
    /// [`ManifestError::DigestMismatch`] when content does not match,
    /// [`ManifestError::MissingFile`] when a recorded document is absent, or
    /// [`ManifestError::UnaccountedFile`] when a file is present but unrecorded.
    pub fn verify_rfc_documents<F, G>(
        &self,
        mut digest_of: F,
        files_on_disk: G,
    ) -> Result<(), ManifestError>
    where
        F: FnMut(&str) -> Result<String, String>,
        G: Fn() -> Vec<String>,
    {
        for (id, document) in &self.rfc_documents {
            let actual = digest_of(&document.file).map_err(|reason| ManifestError::Unreadable {
                revision: id.clone(),
                file: document.file.clone(),
                reason,
            })?;
            if actual != document.sha256 {
                return Err(ManifestError::DigestMismatch {
                    revision: id.clone(),
                    file: document.file.clone(),
                    expected: document.sha256.clone(),
                    actual,
                });
            }
        }

        let on_disk = files_on_disk();
        let accounted: Vec<&str> = self
            .rfc_documents
            .values()
            .map(|document| document.file.as_str())
            .collect();

        for file in &on_disk {
            if !accounted.contains(&file.as_str()) {
                return Err(ManifestError::UnaccountedFile {
                    revision: CITED_DOCUMENT_DIR.to_owned(),
                    file: file.clone(),
                });
            }
        }
        for file in &accounted {
            if !on_disk.iter().any(|present| present == file) {
                return Err(ManifestError::MissingFile {
                    revision: CITED_DOCUMENT_DIR.to_owned(),
                    file: (*file).to_owned(),
                });
            }
        }
        Ok(())
    }

    /// Cited-document identifiers in the manifest, sorted.
    #[must_use]
    pub fn rfc_document_names(&self) -> Vec<&str> {
        self.rfc_documents.keys().map(String::as_str).collect()
    }
}

#[cfg(test)]
mod tests {
    // Test code unwraps deliberately. The strict `unwrap_used`/`expect_used`
    // profile exists to keep *production* paths fallible; a test that cannot
    // unwrap cannot assert anything, and a failure message is strictly more
    // useful than a propagated error. `expect` here still names the failing
    // precondition, which is the part that matters when a test breaks.
    #![expect(
        clippy::expect_used,
        reason = "tests assert on fallible calls; propagating errors would replace a clear failure message with noise"
    )]

    use super::{Manifest, ManifestError, RevisionSpec, SUPPORTED_SCHEMA_VERSION, hex_digest};
    use assert_matches::assert_matches;

    /// A manifest for one tagged, fully vendored revision (schema.json and a
    /// changelog, both digest-locked). The `changelog` key is what distinguishes
    /// a vendored changelog from an authored one; omitting it would make every
    /// test below expect a digest-free `CHANGELOG.md` on disk instead.
    fn sample_manifest(files: &str) -> String {
        format!(
            "[meta]\nschema_version = {SUPPORTED_SCHEMA_VERSION}\n\
             repository = \"owner/name\"\nraw_base = \"https://example.invalid\"\n\
             \n[revisions.2025-06-18]\ntag = \"2025-06-18\"\n\
             commit = \"0000000000000000000000000000000000000000\"\n\
             schema_prefix = \"schema/2025-06-18\"\n\
             changelog = \"docs/specification/2025-06-18/changelog.mdx\"\n\
             {files}"
        )
    }

    /// A fully vendored revision whose `schema.json` and `CHANGELOG.md` both
    /// digest-lock to the given bodies, plus the matching on-disk contents.
    ///
    /// The bodies are returned as `String` so a test can tamper with a copy
    /// before verifying, which is how the mismatch case is produced.
    fn vendored_pair(
        schema_body: &str,
        changelog_body: &str,
    ) -> (String, Vec<(&'static str, String)>) {
        let source = sample_manifest(&format!(
            "\n[revisions.2025-06-18.files]\n\
             \"schema.json\" = \"{}\"\n\"CHANGELOG.md\" = \"{}\"\n",
            hex_digest(schema_body.as_bytes()),
            hex_digest(changelog_body.as_bytes()),
        ));
        let content = vec![
            ("2025-06-18/schema.json", schema_body.to_owned()),
            ("2025-06-18/CHANGELOG.md", changelog_body.to_owned()),
        ];
        (source, content)
    }

    const ZERO: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    // ── Cited external documents ───────────────────────────────────────────────
    //
    // The revision-tree guard above is only worth anything because it is proven
    // to fail on each of its three conditions. The cited-document guard is the
    // same kind of claim about a different directory, so it gets the same
    // treatment rather than being trusted on inspection.

    /// A manifest citing one document, digest-locked to `body`.
    fn citing_manifest(body: &str) -> String {
        format!(
            "{}\n[rfc_documents.rfc7636]\ntitle = \"PKCE\"\n\
             url = \"https://example.invalid/rfc7636.txt\"\nfile = \"rfc7636.txt\"\n\
             sha256 = \"{}\"\ncited_by = [\"2025-03-26\"]\n",
            sample_manifest("").trim_end(),
            hex_digest(body.as_bytes()),
        )
    }

    /// The on-disk listing for one recorded document, plus whatever extras the
    /// test smuggles in.
    fn on_disk(extra: &[&str]) -> Vec<String> {
        let mut names: Vec<String> = std::iter::once("rfc7636.txt".to_owned())
            .chain(extra.iter().map(|name| (*name).to_owned()))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_cited_document_verifies_when_the_digest_and_the_tree_agree() {
        let manifest = Manifest::parse(&citing_manifest("the body")).expect("parses");
        assert_matches!(
            manifest.verify_rfc_documents(|_| Ok(hex_digest(b"the body")), || on_disk(&[]),),
            Ok(())
        );
    }

    #[test]
    fn a_cited_document_whose_text_changed_is_refused() {
        let manifest = Manifest::parse(&citing_manifest("the body")).expect("parses");
        let outcome =
            manifest.verify_rfc_documents(|_| Ok(hex_digest(b"a different body")), || on_disk(&[]));
        assert_matches!(outcome, Err(ManifestError::DigestMismatch { .. }));
    }

    /// The half that makes this a guard rather than a formality: a document
    /// dropped into the directory without a manifest entry is as fatal as one
    /// whose digest is wrong, because an unrecorded document is one nothing
    /// vouches for.
    #[test]
    fn an_unrecorded_document_in_the_tree_is_refused() {
        let manifest = Manifest::parse(&citing_manifest("the body")).expect("parses");
        let outcome = manifest.verify_rfc_documents(
            |_| Ok(hex_digest(b"the body")),
            || on_disk(&["rfc9999.txt"]),
        );
        assert_matches!(outcome, Err(ManifestError::UnaccountedFile { .. }));
    }

    #[test]
    fn a_recorded_document_that_is_absent_is_refused() {
        let manifest = Manifest::parse(&citing_manifest("the body")).expect("parses");
        let outcome = manifest.verify_rfc_documents(|_| Ok(hex_digest(b"the body")), Vec::new);
        assert_matches!(outcome, Err(ManifestError::MissingFile { .. }));
    }

    /// A manifest predating this table still parses.
    ///
    /// The table is `#[serde(default)]`, so "no external documents are cited" is
    /// a state a manifest can be in rather than a schema error.
    #[test]
    fn a_manifest_with_no_cited_documents_still_parses() {
        let manifest = Manifest::parse(&sample_manifest("")).expect("parses");
        assert!(manifest.rfc_document_names().is_empty());
        assert_matches!(
            manifest.verify_rfc_documents(|_| Ok(ZERO.to_owned()), Vec::new),
            Ok(())
        );
    }

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
        let source =
            sample_manifest("\n[revisions.2025-06-18.files]\n\"schema.json\" = \"abc123\"\n");
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
    ///
    /// `N` and `B` are separate because the on-disk listing is a path while the
    /// content is usually an owned `String` that a test wants to tamper with.
    fn verify_against<N: AsRef<str>, B: AsRef<str>>(
        source: &str,
        content: &[(N, B)],
    ) -> Result<(), ManifestError> {
        let manifest = Manifest::parse(source).expect("manifest must parse");
        manifest.verify(
            |revision, file| {
                let key = format!("{revision}/{file}");
                content
                    .iter()
                    .find(|(name, _)| name.as_ref() == key)
                    .map(|(_, bytes)| hex_digest(bytes.as_ref().as_bytes()))
                    .ok_or_else(|| format!("no such file {key}"))
            },
            |revision| {
                let prefix = format!("{revision}/");
                content
                    .iter()
                    .filter(|(name, _)| name.as_ref().starts_with(&prefix))
                    .filter_map(|(name, _)| name.as_ref().strip_prefix(&prefix).map(str::to_owned))
                    .collect()
            },
        )
    }

    #[test]
    fn matching_digests_verify() {
        let (source, content) = vendored_pair("{}", "# changelog");
        let result = verify_against(&source, &content);
        assert!(result.is_ok(), "expected ok, got {result:?}");
    }

    #[test]
    fn a_changed_file_is_a_digest_mismatch() {
        let (source, mut content) = vendored_pair("{}", "# changelog");
        // Tamper with the body after the manifest has locked its digest.
        let (_, schema_body) = content
            .get_mut(0)
            .expect("vendored_pair always yields schema.json first");
        *schema_body = r#"{"tampered":true}"#.to_owned();
        let error = verify_against(&source, &content).expect_err("tampering must be detected");
        assert!(
            matches!(error, ManifestError::DigestMismatch { .. }),
            "unexpected error: {error}"
        );
        // The message names the file and both digests, so a reader can act without
        // opening the manifest. It deliberately does *not* name a refresh
        // command: the build script prints that, because only it knows whether
        // it is looking at a revision or a cited document — and when this line
        // did hardcode `refresh-spec`, it suggested a command that cannot repair
        // a cited document at all.
        let rendered = error.to_string();
        assert!(
            rendered.contains("2025-06-18") && rendered.contains("schema.json"),
            "error should name what failed, got: {rendered}"
        );
        assert!(
            rendered.contains("expected") && rendered.contains("actual"),
            "error should show both digests, got: {rendered}"
        );
    }

    #[test]
    fn an_unreadable_file_is_reported_as_unreadable() {
        let (source, _content) = vendored_pair("{}", "# changelog");
        // Nothing on disk at all, so the digest source cannot produce a digest.
        let nothing: &[(&str, &str)] = &[];
        let error = verify_against(&source, nothing).expect_err("a missing file must be reported");
        assert!(
            matches!(error, ManifestError::Unreadable { .. }),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn a_file_on_disk_that_nothing_accounts_for_is_refused() {
        let (source, mut content) = vendored_pair("{}", "# changelog");
        // A file that is present, hashes fine, but nothing in the manifest
        // records. It would otherwise be verified by nothing at all.
        content.push(("2025-06-18/schema-extra.json", "{}".to_owned()));
        let error = verify_against(&source, &content).expect_err("an unrecorded file must fail");
        assert!(
            matches!(error, ManifestError::UnaccountedFile { .. }),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn a_recorded_file_missing_from_disk_is_refused() {
        let (source, content) = vendored_pair("{}", "# changelog");
        // Digests all resolve correctly, but the directory listing is empty, so
        // this exercises the other direction of the reconciliation: the manifest
        // records files the tree does not contain.
        let manifest = Manifest::parse(&source).expect("manifest must parse");
        let error = manifest
            .verify(
                |revision, file| {
                    let key = format!("{revision}/{file}");
                    content
                        .iter()
                        .find(|(name, _)| *name == key)
                        .map(|(_, bytes)| hex_digest(bytes.as_bytes()))
                        .ok_or_else(|| format!("no such file {key}"))
                },
                |_| Vec::new(),
            )
            .expect_err("a missing recorded file must be refused");
        assert!(
            matches!(error, ManifestError::MissingFile { .. }),
            "unexpected error: {error}"
        );
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
             schema_prefix = \"schema\"\n\
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
             schema_prefix = \"schema/2025-06-18\"\n\
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
            branch: None,
            schema_prefix: "schema/draft".to_owned(),
            files: std::collections::BTreeMap::new(),
        };
        assert!(draft.is_floating());
        assert!(!draft.changelog_is_authored());

        // `..draft` alone is not enough: it would carry `floating: true` across
        // and a tagged revision would still report as floating. The flag and the
        // tag are independent inputs, which is why build.rs also asserts the two
        // agree rather than trusting either one.
        let tagged = RevisionSpec {
            tag: "2026-07-28".to_owned(),
            floating: false,
            branch: None,
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
            branch: None,
            schema_prefix: "schema".to_owned(),
            files: std::collections::BTreeMap::new(),
        };
        assert!(authored.changelog_is_authored());
    }

    /// 2024-11-05 predates the per-version schema layout, so its files sit
    /// directly under `schema/`. Every later revision uses `schema/<version>/`.
    /// Both must resolve, and neither may guess.
    #[test]
    fn upstream_paths_follow_the_recorded_schema_prefix() {
        let first = RevisionSpec {
            tag: "2024-11-05".to_owned(),
            commit: "0".repeat(40),
            changelog: None,
            changelog_is_placeholder: false,
            floating: false,
            branch: None,
            schema_prefix: "schema".to_owned(),
            files: std::collections::BTreeMap::new(),
        };
        assert_eq!(
            first.upstream_path("schema.json").expect("path"),
            "schema/schema.json"
        );
        assert_eq!(
            first.upstream_path("schema.ts").expect("path"),
            "schema/schema.ts"
        );

        let later = RevisionSpec {
            tag: "2025-06-18".to_owned(),
            schema_prefix: "schema/2025-06-18".to_owned(),
            changelog: Some("docs/specification/2025-06-18/changelog.mdx".to_owned()),
            ..first
        };
        assert_eq!(
            later.upstream_path("schema.json").expect("path"),
            "schema/2025-06-18/schema.json"
        );
        // The changelog is fetched from its own recorded path, not from the
        // schema prefix.
        assert_eq!(
            later.upstream_path("CHANGELOG.md").expect("path"),
            "docs/specification/2025-06-18/changelog.mdx"
        );
    }

    #[test]
    fn a_file_with_no_upstream_path_is_reported() {
        let spec = RevisionSpec {
            tag: "2024-11-05".to_owned(),
            commit: "0".repeat(40),
            changelog: None,
            changelog_is_placeholder: false,
            floating: false,
            branch: None,
            schema_prefix: "schema".to_owned(),
            files: std::collections::BTreeMap::new(),
        };
        // An authored changelog has no upstream path, and a file that is not a
        // schema representation has none either.
        assert!(spec.upstream_path("CHANGELOG.md").is_err());
        assert!(spec.upstream_path("schema-extra.json").is_err());
    }

    #[test]
    fn the_shipped_manifest_derives_a_path_for_every_vendored_file() {
        let source = include_str!("../../spec/MANIFEST.toml");
        let manifest = Manifest::parse(source).expect("the shipped manifest must parse");
        for (name, spec) in &manifest.revisions {
            for file in spec.files.keys() {
                assert!(
                    spec.upstream_path(file).is_ok(),
                    "{name}/{file} has no derivable upstream path"
                );
            }
        }
    }

    #[test]
    fn excluded_tags_are_reported_as_metadata() {
        let source = format!(
            "[meta]\nschema_version = {SUPPORTED_SCHEMA_VERSION}\n\
             repository = \"owner/name\"\nraw_base = \"https://example.invalid\"\n\
             \n[meta.excluded_tags]\n\"2024-10-07\" = \"Superseded.\"\n\
             \n[revisions.2025-06-18]\ntag = \"2025-06-18\"\n\
             commit = \"0000000000000000000000000000000000000000\"\n\
             schema_prefix = \"schema/2025-06-18\"\n\
             changelog = \"docs/specification/2025-06-18/changelog.mdx\"\n"
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

    // `normalize_line_endings` is exercised in the integration test by the
    // manifest verifier itself, but the edge cases below are easier to read
    // here than as comments on the production code.

    #[test]
    fn lf_content_round_trips_through_normalization() {
        let input = b"line one\nline two\nline three\n";
        let output = super::normalize_line_endings(input);
        assert_eq!(output.as_slice(), input.as_slice());
    }

    #[test]
    fn crlf_content_collapses_to_lf() {
        let input = b"line one\r\nline two\r\nline three\r\n";
        let expected = b"line one\nline two\nline three\n";
        let output = super::normalize_line_endings(input);
        assert_eq!(output.as_slice(), expected.as_slice());
    }

    #[test]
    fn a_lone_carriage_return_is_left_alone() {
        // The spec formats do not use bare \r, and the normaliser must not
        // invent content where there was none.
        let input = b"a\r\nb\nc\rd\r\n";
        let expected = b"a\nb\nc\rd\n";
        let output = super::normalize_line_endings(input);
        assert_eq!(output.as_slice(), expected.as_slice());
    }

    #[test]
    fn empty_input_yields_empty_output() {
        let output = super::normalize_line_endings(b"");
        assert_eq!(output.as_slice(), b"" as &[u8]);
    }
}
