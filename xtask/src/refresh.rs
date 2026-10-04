//! `refresh-spec`: re-download a vendored revision and update the manifest.
//!
//! The rule this module exists to enforce: **a build never downloads anything.**
//! Refreshing is always an explicit, deliberate act by a developer who has
//! decided to take a new upstream state, and it leaves a reviewable diff behind
//! in `git`. A build that silently fetched a schema would make builds
//! unreproducible and would let the wire types change without a commit.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::manifest::{Manifest, hex_digest};
use crate::manifest_edit::{apply_digest_updates, revision_table};

/// GitHub's API host, used only to resolve a floating revision to a commit.
const GITHUB_API: &str = "https://api.github.com";

/// What a refresh run changed, for the final report.
#[derive(Debug, Default)]
struct Report {
    /// Files where upstream moved, so both the file and the manifest changed.
    updated: Vec<String>,
    /// Files whose local copy was stale or corrupted but upstream had not moved,
    /// so the file was repaired and the manifest was left alone.
    repaired: Vec<String>,
    /// Files already identical to what upstream serves.
    unchanged: Vec<String>,
    /// Notes about steps deliberately not taken.
    skipped: Vec<String>,
    /// True when `--newest` moved the pin to a different commit.
    commit_moved: bool,
}

/// Replace the `commit = "..."` line inside one revision's table.
///
/// Scoped to the revision by walking tables the same way the digest rewriter
/// does, so a commit line in a different revision is never touched.
fn rewrite_commit(text: &str, revision: &str, commit: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_table = false;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            in_table =
                revision_table(rest).is_some_and(|(rev, is_files)| !is_files && rev == revision);
        }
        if in_table && line.starts_with("commit = ") {
            out.push_str("commit = \"");
            out.push_str(commit);
            out.push('"');
            if line.ends_with('\n') {
                out.push('\n');
            }
            continue;
        }
        out.push_str(line);
    }
    out
}

/// Refresh one revision of the vendored spec tree.
///
/// `spec_root` is the directory holding `MANIFEST.toml`. `newest` re-resolves a
/// floating revision to the current `main` commit before downloading, so a
/// `draft` refresh actually moves forward; without it a floating revision is
/// re-fetched at the commit already recorded, which is how a no-op refresh is
/// expressed.
pub fn refresh(spec_root: &Path, revision: &str, newest: bool) -> Result<()> {
    let manifest_path = spec_root.join("MANIFEST.toml");
    let text = fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let manifest = Manifest::parse(&text)
        .map_err(|err| anyhow::anyhow!("{err}"))
        .context("parsing MANIFEST.toml")?;

    let spec = manifest
        .revisions
        .get(revision)
        .with_context(|| {
            format!(
                "revision `{revision}` is not vendored; known revisions are: {}",
                manifest.revision_names().join(", ")
            )
        })?
        .clone();

    // A floating revision has no tag, so its ref is a branch. Resolving it to a
    // commit is what makes the vendored copy reproducible.
    let (ref_name, commit) = if spec.is_floating() && newest {
        let branch = spec
            .floating_branch()
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        let resolved = resolve_branch(&manifest.meta.repository, branch)
            .with_context(|| format!("resolving {branch} for {revision}"))?;
        (String::new(), resolved)
    } else {
        (spec.tag.clone(), spec.commit.clone())
    };
    let ref_name = if ref_name.is_empty() {
        commit.clone()
    } else {
        ref_name
    };

    let revision_dir = spec_root.join(revision);
    fs::create_dir_all(&revision_dir)
        .with_context(|| format!("creating {}", revision_dir.display()))?;

    let mut report = Report::default();
    let mut updates: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut files: BTreeMap<String, String> = BTreeMap::new();

    for (file, recorded) in &spec.files {
        let upstream = spec
            .upstream_path(file)
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        let url = format!("{}/{ref_name}/{upstream}", manifest.meta.raw_base);
        let body = download(&url).with_context(|| format!("downloading {url}"))?;

        let digest = hex_digest(&body);
        let local = revision_dir.join(file);

        // Two independent questions, and conflating them is the bug that made
        // this tool report "unchanged" while leaving a tampered file in place:
        //   - Has UPSTREAM moved?  Compare against the manifest.
        //   - Is the LOCAL file stale? Compare the bytes on disk.
        // Repairing a corrupted tree depends on the second, so the file is
        // written whenever its contents differ from what we just downloaded,
        // regardless of what the manifest currently claims.
        let local_matches = fs::read(&local).is_ok_and(|existing| existing == body);
        if !local_matches {
            fs::write(&local, &body).with_context(|| format!("writing {}", local.display()))?;
        }

        if digest == *recorded && local_matches {
            report.unchanged.push(file.clone());
        } else if digest == *recorded {
            report.repaired.push(file.clone());
        } else {
            report.updated.push(file.clone());
        }
        files.insert(file.clone(), digest);
    }

    updates.insert(revision.to_owned(), files);
    let mut updated_text = apply_digest_updates(&text, &updates)
        .map_err(|err| anyhow::anyhow!(err))
        .context("rewriting MANIFEST.toml")?;

    // When `--newest` moves a floating revision to a new commit, the pin has to
    // be rewritten too. Leaving the old SHA would make the manifest claim the
    // files came from a commit they did not come from — the exact lie the
    // manifest exists to prevent.
    if commit != spec.commit {
        updated_text = rewrite_commit(&updated_text, revision, &commit);
        report.commit_moved = true;
    }

    if updated_text == text {
        report
            .skipped
            .push("MANIFEST.toml (already current)".to_owned());
    } else {
        fs::write(&manifest_path, &updated_text)
            .with_context(|| format!("writing {}", manifest_path.display()))?;
    }

    print_report(revision, &commit, &report);
    Ok(())
}

/// Print what a refresh did, in enough detail to review the diff.
fn print_report(revision: &str, commit: &str, report: &Report) {
    println!("refreshed {revision} at {commit}");
    if report.commit_moved {
        println!("  repinned   {revision} -> {commit}");
    }
    for file in &report.updated {
        println!("  updated   {file}  (upstream moved; manifest digest rewritten)");
    }
    for file in &report.repaired {
        println!("  repaired  {file}  (local copy was stale or corrupted)");
    }
    for file in &report.unchanged {
        println!("  unchanged {file}");
    }
    for note in &report.skipped {
        println!("  skipped   {note}");
    }
    if report.updated.is_empty() && report.repaired.is_empty() && report.unchanged.is_empty() {
        println!("  nothing to do");
    }
    println!("\nReview `git diff` before committing. The manifest must change in the");
    println!("same commit as the vendored files, or the build will fail verification.");
}

/// Resolve a branch to its current commit SHA via the GitHub API.
fn resolve_branch(repository: &str, branch: &str) -> Result<String> {
    let (owner, name) = repository
        .split_once('/')
        .with_context(|| format!("repository `{repository}` is not in `owner/name` form"))?;
    let url = format!("{GITHUB_API}/repos/{owner}/{name}/commits/{branch}");
    let body = download(&url).with_context(|| format!("resolving {branch} at {url}"))?;
    let value: serde_json::Value = serde_json::from_slice(&body)
        .with_context(|| format!("parsing the GitHub response for {branch}"))?;
    value
        .get("sha")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .with_context(|| format!("no `sha` in the GitHub response for {branch}"))
}

/// Fetch a URL, returning its body. Any non-2xx status is an error carrying the
/// status and a trimmed body, because a 404 here almost always means a wrong
/// path in the manifest rather than a transient problem.
fn download(url: &str) -> Result<Vec<u8>> {
    let mut response = ureq::get(url)
        .call()
        .map_err(|err| anyhow::anyhow!("{url}: {err}"))?;
    let status = response.status();
    let body = response
        .body_mut()
        .read_to_vec()
        .map_err(|err| anyhow::anyhow!("{url}: reading the body: {err}"))?;
    if !(200..300).contains(&status.as_u16()) {
        let preview = String::from_utf8_lossy(&body);
        let preview: String = preview.chars().take(200).collect();
        bail!("{url} returned HTTP {status}: {preview}");
    }
    Ok(body)
}

/// Locate the spec root relative to the workspace, so the command works from
/// anywhere in the tree.
pub fn locate_spec_root() -> Result<PathBuf> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    // xtask/ -> workspace root -> crates/protocol-droid-schema/spec
    let root = manifest_dir
        .parent()
        .context("xtask has no parent directory")?
        .join("crates/protocol-droid-schema/spec");
    if !root.join("MANIFEST.toml").is_file() {
        bail!(
            "no spec tree at {}; expected crates/protocol-droid-schema/spec/MANIFEST.toml",
            root.display()
        );
    }
    Ok(root)
}
