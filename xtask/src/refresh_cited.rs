//! Re-download the cited external documents and update their digests.
//!
//! The counterpart to [`crate::refresh`] for the documents the MCP
//! authorization sections cite. It is a separate command rather than a flag on
//! `refresh-spec` because the two fetch from unrelated sources under unrelated
//! pinning rules: a revision is a directory at a pinned upstream commit, and a
//! cited document is a single file at a URL. Sharing a command would mean
//! carrying both pinning models into one code path to reuse about twenty lines
//! of download and reporting.
//!
//! Like `refresh-spec`, this is the only way one of these files changes, and the
//! manifest edit and the file write land in the same commit or the build fails.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::manifest::{Manifest, hex_digest};
use crate::refresh::{download, locate_spec_root};

/// What happened to one document.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The downloaded text differs from what is vendored, so both the file and
    /// the recorded digest were rewritten.
    Updated,
    /// The local copy was stale or damaged but the digest already recorded
    /// matched. The file was rewritten and the manifest left alone.
    Repaired,
    /// Already identical. Nothing was written.
    Unchanged,
}

/// Refreshes every cited document, or only those named.
///
/// `ids` empty means every document in the manifest.
///
/// # Errors
///
/// Fails if the manifest does not parse, if a named document is not in the
/// manifest, or if any download fails. A partial refresh is possible if a later
/// document fails after an earlier one was written; the manifest is only
/// rewritten once every requested document has been fetched, so the tree and the
/// manifest cannot drift apart within one run.
pub fn refresh(spec_root: &Path, ids: &[String]) -> Result<()> {
    let manifest_path = spec_root.join("MANIFEST.toml");
    let source = fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let manifest = Manifest::parse(&source)
        .map_err(|err| anyhow::anyhow!("{err}"))
        .context(
            "parsing the manifest; add the document's entry before refreshing it, so the tool \
             knows where to fetch from",
        )?;

    if manifest.rfc_documents.is_empty() {
        bail!("the manifest cites no documents; there is nothing to refresh");
    }

    let selected: Vec<&str> = if ids.is_empty() {
        manifest.rfc_document_names()
    } else {
        ids.iter()
            .map(|id| {
                manifest
                    .rfc_documents
                    .get(id)
                    .map(|_| id.as_str())
                    .ok_or_else(|| anyhow::anyhow!("`{id}` is not a cited document"))
            })
            .collect::<Result<Vec<_>>>()?
    };

    let dir = spec_root.join(crate::manifest::CITED_DOCUMENT_DIR);
    let mut outcomes: Vec<(String, Outcome)> = Vec::new();
    let mut digests: BTreeMap<String, String> = BTreeMap::new();

    for id in selected {
        let document = manifest
            .rfc_documents
            .get(id)
            .with_context(|| format!("`{id}` vanished from the manifest mid-run"))?;
        let body = download(&document.url).with_context(|| format!("fetching {}", document.url))?;
        let digest = hex_digest(&body);
        let path = dir.join(&document.file);

        let on_disk = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let outcome = if on_disk == body {
            Outcome::Unchanged
        } else {
            let matches_recorded = digest == document.sha256;
            fs::write(&path, &body).with_context(|| format!("writing {}", path.display()))?;
            if matches_recorded {
                Outcome::Repaired
            } else {
                Outcome::Updated
            }
        };
        digests.insert(id.to_owned(), digest);
        outcomes.push((id.to_owned(), outcome));
    }

    // Every fetch succeeded, so the manifest can be rewritten in one pass. The
    // digests are identical to what was already recorded for any `Unchanged` or
    // `Repaired` document, so this is a no-op on those lines.
    let edited = apply_digest_updates(&source, &digests).map_err(|err| anyhow::anyhow!("{err}"))?;
    fs::write(&manifest_path, edited)
        .with_context(|| format!("writing {}", manifest_path.display()))?;

    for (id, outcome) in &outcomes {
        // The digest comes from `digests`, not from the outcome, so the
        // reported value is the same one written into the manifest rather than
        // a copy that could disagree with it.
        let digest = digests
            .get(id)
            .map_or("<no digest recorded>", String::as_str);
        match outcome {
            Outcome::Updated => println!("updated   {id:<40} {digest}"),
            Outcome::Repaired => println!("repaired  {id:<40} {digest}"),
            Outcome::Unchanged => println!("unchanged {id}"),
        }
    }
    Ok(())
}

/// Rewrites the `sha256` lines of the named documents, preserving everything
/// else.
///
/// `updates` maps a document identifier to its new digest. Lines outside the
/// named `[rfc_documents.<id>]` tables, and every comment, are returned
/// unchanged — the manifest is a hand-maintained file with recorded judgements
/// in it, and a rewrite that reformatted it would bury those in noise.
///
/// # Errors
///
/// Returns an error if a named document has no `sha256` line, which would mean
/// the identifier does not match the table and the rewrite would silently do
/// nothing.
fn apply_digest_updates(text: &str, updates: &BTreeMap<String, String>) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut current: Option<String> = None;
    let mut applied: BTreeMap<String, String> = BTreeMap::new();

    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix('[') {
            current = rest
                .strip_suffix(']')
                .and_then(|name| name.strip_prefix("rfc_documents."))
                .map(|id| unquote(id).to_owned());
        }

        // Only a line that actually *is* a `sha256 = "..."` assignment is rewritten.
        // The guard matters: without it any line inside the table would be replaced
        // once a digest was found for that table, and `url` and `title` would be
        // rewritten to a hex digest on the next refresh.
        let is_digest_line = sha256_of(line).is_some();
        let replacement = match current.as_deref() {
            Some(id) if is_digest_line => updates.get(id).map(|digest| {
                applied.insert(id.to_owned(), digest.clone());
                with_sha256(line, digest)
            }),
            _ => None,
        };
        out.push_str(replacement.as_deref().unwrap_or(line));
    }

    if let Some(missing) = updates.keys().find(|id| !applied.contains_key(*id)) {
        return Err(format!(
            "`{missing}` has no `sha256` line in the manifest; the identifier does not match \
             any `[rfc_documents.<id>]` table"
        ));
    }
    Ok(out)
}

/// The value of a `sha256 = "..."` line, if this line is one.
///
/// Deliberately narrow: a line that merely contains `sha256` in a comment or a
/// string value does not match, because rewriting that would corrupt content.
fn sha256_of(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix("sha256")?;
    let rest = rest.trim_start().strip_prefix('=')?.trim();
    rest.strip_prefix('"')
}

/// Replaces the quoted value of a `sha256 = "..."` line, preserving indent and
/// any trailing comment.
fn with_sha256(line: &str, digest: &str) -> String {
    let Some(open) = line.find('"') else {
        return line.to_owned();
    };
    let rest = &line[open + 1..];
    let Some(close) = rest.find('"') else {
        return line.to_owned();
    };
    // `line[..open + 1]` already *ends* with the opening quote, so the
    // replacement is the digest and the closing quote — adding another opening
    // one here is what produced `sha256 = ""abc""` and made the manifest
    // unparseable on the very first run of this command.
    format!("{}{digest}\"{}", &line[..=open], &rest[close + 1..])
}

/// Strips the quotes TOML allows around a bare table key.
fn unquote(key: &str) -> &str {
    key.trim_matches('"')
}

/// Locate the spec root and refresh documents.
pub fn run(ids: &[String]) -> Result<()> {
    let spec_root = locate_spec_root()?;
    refresh(&spec_root, ids)
}
