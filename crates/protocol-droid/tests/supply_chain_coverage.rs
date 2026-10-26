//! Asserts that every committed `Cargo.lock` is scanned by a CI job.
//!
//! # What this test exists to do
//!
//! The `supply chain` job used to scan the root lockfile only. Three
//! lockfiles sit outside the workspace — `fuzz`, `interop_rmcp` and
//! `interop_ts_sdk`, each its own `[workspace]` — so the job never saw
//! their dependency graphs. A committed `Cargo.lock` is a dependency graph
//! someone will build, and the gap was not theoretical: when `ctutils` was
//! bumped for RUSTSEC-2026-0385, the root lockfile was updated and the
//! interop fixture's was not, so the fixture kept the vulnerable version
//! until a human ran the pre-push hook.
//!
//! The pre-push hook does scan every lockfile, so this was a mitigation
//! rather than an absence of coverage. But a mitigation that depends on a
//! developer remembering to push is not the same thing as a gate. This
//! test is what stops the gate from silently going back to scanning one
//! lockfile: it reads the CI workflow, works out which manifests are
//! scanned, and fails if any committed lockfile is not among them.
//!
//! # Why a test and not a lint or a script
//!
//! The failure mode here is silent. A workflow edit that drops a step
//! leaves a green CI run and a lockfile nobody scans; nothing crashes and
//! nothing warns. A test that fails loudly in the same pull request is the
//! only thing that reliably closes it, and it costs nothing to run — it
//! reads two files and compares sets.
//!
//! # What this test is not
//!
//! It does not run `cargo deny`. That is CI's job, and the advisory
//! database moves too fast for a test to be the thing that checks it. This
//! test checks the *wiring*: that the CI workflow asks for a scan of every
//! lockfile that exists. It is the difference between a lockfile that is
//! scanned and a lockfile that is scheduled to be scanned.

use std::path::{Path, PathBuf};

/// The CI workflow that runs the supply-chain scan.
const WORKFLOW: &str = ".github/workflows/ci.yml";

/// The repository root, resolved from this crate's manifest directory so the
/// test works whatever the working directory is.
fn repo_root() -> Result<PathBuf, String> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            format!(
                "could not resolve the repo root from {}",
                manifest.display()
            )
        })
}

fn read(root: &Path, relative: &str) -> Result<String, String> {
    let path = root.join(relative);
    std::fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))
}

/// Every `Cargo.lock` committed to the repository.
///
/// Found by walking the tree rather than by asking git, because the walk
/// is what the CI job needs to match: a lockfile that exists on disk but
/// is not committed is not a graph anyone else builds. The walk skips the
/// directories that hold build output and dependencies, because those carry
/// lockfiles nobody committed and nobody scans.
fn committed_lockfiles(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut found = Vec::new();
    walk(root, root, &mut found)?;
    found.sort();
    Ok(found)
}

/// The directories that are never worth looking in: build output, the
/// dependency cache, and the version-control directory.
fn is_skipped(name: &str) -> bool {
    matches!(name, "target" | "node_modules" | ".git")
}

fn walk(root: &Path, dir: &Path, found: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|error| format!("failed to read {}: {error}", dir.display()))?;
    for entry in entries {
        let entry = entry
            .map_err(|error| format!("failed to read an entry in {}: {error}", dir.display()))?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| format!("a non-UTF-8 path under {}", dir.display()))?;
        if is_skipped(name) {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|error| format!("failed to stat {}: {error}", path.display()))?;
        if file_type.is_dir() {
            walk(root, &path, found)?;
        } else if name == "Cargo.lock" {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| format!("{} is not under the repo root", path.display()))?;
            found.push(relative.to_path_buf());
        }
    }
    Ok(())
}

/// The manifests the `supply chain` job scans, read out of the workflow.
///
/// The job runs the `cargo-deny-action`, whose `manifest-path` input says
/// which manifest to check. This returns every value that input is given,
/// plus the root manifest, which the job scans when the input is absent.
///
/// The parse is deliberately simple: the workflow is a file this repository
/// owns and edits by hand, and a YAML parser would be a dependency bought
/// for one list. What matters is that a value is found when it is there
/// and not invented when it is not — a test that guessed would be worse
/// than no test.
fn scanned_manifests(workflow: &str) -> Vec<String> {
    let mut manifests = Vec::new();
    let mut in_supply_chain = false;
    let mut job_indent = 0_usize;

    for line in workflow.lines() {
        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim();

        // A job header is at two-space indentation under `jobs:`.
        if indent == 2 && trimmed.ends_with(':') && !trimmed.starts_with('-') {
            in_supply_chain = trimmed == "deny:";
            job_indent = indent;
            continue;
        }

        if !in_supply_chain {
            continue;
        }

        // The job's body ends when the indentation drops back to the job
        // header's own level.
        if indent <= job_indent && !trimmed.is_empty() && !trimmed.starts_with('-') {
            in_supply_chain = false;
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("manifest-path:") {
            let value = rest.trim().trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                manifests.push(value.to_owned());
            }
        }
    }

    // The root manifest is scanned whenever the job does not override
    // `manifest-path`, which is the default the action ships with.
    if !manifests
        .iter()
        .any(|path| path == "./Cargo.toml" || path == "Cargo.toml")
    {
        manifests.push("./Cargo.toml".to_owned());
    }

    manifests
}

/// The lockfile a given manifest produces.
///
/// `cargo deny` reads the lockfile next to the manifest it is given, so
/// `crates/protocol-droid/fuzz/Cargo.toml` is scanned through
/// `crates/protocol-droid/fuzz/Cargo.lock`.
///
/// The root manifest is written `./Cargo.toml` in the workflow and its
/// lockfile is `Cargo.lock`, so the two have to be spelled the same way
/// before they can be compared. `.` as a directory is dropped rather than
/// kept, because `Path::join` would otherwise produce `./Cargo.lock` and
/// never match the `Cargo.lock` the walk found.
fn lockfile_for(manifest: &str) -> PathBuf {
    let path = Path::new(manifest);
    let directory = match path.parent() {
        Some(parent) if parent.as_os_str() != "." => parent,
        _ => Path::new(""),
    };
    directory.join("Cargo.lock")
}

/// Every committed lockfile is scanned by the `supply chain` job.
///
/// The assertion is on the lockfile side rather than the manifest side
/// because the lockfile is the thing that can go unscanned: it is what
/// names the exact dependency versions, and a graph nobody scans is a
/// graph nobody watches for advisories.
#[test]
fn every_committed_lockfile_is_scanned_by_ci() -> Result<(), String> {
    let root = repo_root()?;
    let workflow = read(&root, WORKFLOW)?;
    let manifests = scanned_manifests(&workflow);

    let scanned: Vec<PathBuf> = manifests.iter().map(|m| lockfile_for(m)).collect();

    let lockfiles = committed_lockfiles(&root)?;
    if lockfiles.is_empty() {
        return Err("found no committed Cargo.lock files, which cannot be right".to_owned());
    }

    let mut unscanned = Vec::new();
    for lockfile in &lockfiles {
        if !scanned.contains(lockfile) {
            unscanned.push(lockfile.display().to_string());
        }
    }

    if !unscanned.is_empty() {
        return Err(format!(
            "these committed lockfiles are not scanned by the `supply chain` job in \
             {WORKFLOW}:\n  {}\n\
             The job scans these manifests: {}.\n\
             A lockfile outside the workspace is excluded from the root scan, so it \
             needs its own step with `manifest-path:`. Without one, an advisory against \
             that graph reaches nobody but the developer who remembers to push.",
            unscanned.join("\n  "),
            manifests.join(", ")
        ));
    }

    Ok(())
}

/// The scan reads the committed lockfile rather than a re-resolved one.
///
/// Without `--locked`, `cargo metadata` re-resolves the graph and rewrites
/// the lockfile on disk, so the scan reports on a dependency set nobody
/// committed. That is not a stricter check — it is a different one, and it
/// is the check that missed the stale `ctutils` version in the first place.
///
/// The argument is asserted on the fixture steps rather than the root step
/// because the root lockfile is refreshed by `cargo fetch --locked` in the
/// `clippy` job. The fixtures have no such guard, which is exactly why
/// they needed one.
#[test]
fn the_fixture_scans_are_locked() -> Result<(), String> {
    let root = repo_root()?;
    let workflow = read(&root, WORKFLOW)?;

    // Each `- name: cargo-deny (...)` step and the arguments that follow
    // it, up to the next step.
    let mut step_name: Option<String> = None;
    let mut step_args: Vec<String> = Vec::new();
    let mut steps: Vec<(String, Vec<String>)> = Vec::new();

    for line in workflow.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("- name: cargo-deny (") {
            if let Some(previous) = step_name.take() {
                steps.push((previous, std::mem::take(&mut step_args)));
            }
            step_name = Some(rest.trim_end_matches(')').to_owned());
            continue;
        }
        // The root step is named `cargo-deny` with no suffix and is covered
        // by the `clippy` job's `cargo fetch --locked`.
        if trimmed == "- name: cargo-deny" {
            if let Some(previous) = step_name.take() {
                steps.push((previous, std::mem::take(&mut step_args)));
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("arguments:") {
            // The value is a single YAML scalar holding several flags, so
            // it is split on whitespace before it is compared. Comparing
            // the whole string would miss `--locked` every time.
            for flag in rest.split_whitespace() {
                step_args.push(flag.to_owned());
            }
        }
    }
    if let Some(previous) = step_name {
        steps.push((previous, step_args));
    }

    let mut missing = Vec::new();
    for (name, args) in &steps {
        if !args.iter().any(|arg| arg == "--locked") {
            missing.push(name.clone());
        }
    }

    if !missing.is_empty() {
        return Err(format!(
            "these `supply chain` steps do not pass --locked: {}\n\
             Without it, cargo re-resolves and rewrites the lockfile, so the scan \
             reports on a dependency set nobody committed.",
            missing.join(", ")
        ));
    }

    Ok(())
}
