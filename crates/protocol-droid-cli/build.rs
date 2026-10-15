//! Build script — embeds a build identifier at compile time.
//!
//! The `protocol-droid` binary's `--version` reports the crate
//! version, a short git SHA, and a `-dirty` suffix when the
//! working tree had uncommitted changes at build time. All three
//! are baked into the binary at compile time, so a `--version`
//! call does not need git to be installed.
//!
//! # Env vars set
//!
//! - `PROTOCOL_DROID_GIT_SHA`   — short git commit hash, or
//!   `"unknown"` when git is unavailable. A `-dirty` suffix is
//!   appended when the working tree was dirty at build time.
//! - `PROTOCOL_DROID_BUILD_DATE` — UTC date in `YYYY-MM-DD`
//!   format, derived from `git log -1` so it tracks the *last
//!   commit's* date, not the wall-clock time of the build. That
//!   makes two builds of the same commit produce the same date
//!   even if they happen a week apart, which is what release
//!   tooling wants.
//!
//! Both env vars are read at runtime via
//! `env!("PROTOCOL_DROID_GIT_SHA")` and
//! `env!("PROTOCOL_DROID_BUILD_DATE")`.
//!
//! # Why a hand-rolled script, not `vergen`
//!
//! `vergen` is a 200kb proc-macro dep that re-derives the same
//! three pieces of information. The papertowel / faceoff
//! precedent in this user's other projects is the hand-rolled
//! `Command::new("git")` approach, and the project standard is
//! "every crate uses the same `lints = workspace` profile" — so
//! adding a dep just to do what 30 lines of `std` does is the
//! wrong trade for a binary crate.
//!
//! # Cross-platform
//!
//! Uses `Command::new("git")` rather than shelling out to a
//! specific git binary, and falls back to `"unknown"` when
//! either `git` is missing or `git rev-parse` fails. That is
//! the right shape for a binary that needs to build in CI on
//! Linux, macOS and Windows runners where git may or may not
//! be on `PATH` for a `cargo install --git` consumer.

use std::process::Command;

/// The string the runtime sees when git could not be queried at
/// build time.
const UNKNOWN: &str = "unknown";

/// Run `git <args>` and return stdout on success, `None` on any
/// failure (missing binary, non-zero exit, non-UTF-8 output).
fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// The short SHA, with a `-dirty` suffix when the working tree
/// had uncommitted changes. `git diff --quiet` exits 1 on a
/// dirty tree, so the suffix is appended in the error branch.
fn detect_git_sha() -> String {
    let Some(sha) = git_output(&["rev-parse", "--short", "HEAD"]) else {
        return UNKNOWN.to_owned();
    };
    let dirty = Command::new("git")
        .args(["diff", "--quiet", "HEAD"])
        .status()
        .is_ok_and(|status| !status.success());
    if dirty { format!("{sha}-dirty") } else { sha }
}

/// The last commit's date, in `YYYY-MM-DD`. Falls back to
/// `UNKNOWN` when git is unavailable, which is the same
/// fallback `detect_git_sha` uses so the two are consistent.
fn detect_build_date() -> String {
    git_output(&["log", "-1", "--format=%cd", "--date=format:%Y-%m-%d"])
        .unwrap_or_else(|| UNKNOWN.to_owned())
}

fn main() {
    // Rerun whenever HEAD, any branch ref, or the build script
    // itself changes. The `git/refs/heads` glob is what makes a
    // `git checkout` of an existing branch re-link the binary,
    // and the `build.rs` line is what makes editing this file
    // (the cheap path while iterating on the format) re-link.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads/");
    println!("cargo:rerun-if-changed=build.rs");

    let sha = detect_git_sha();
    let build_date = detect_build_date();

    println!("cargo:rustc-env=PROTOCOL_DROID_GIT_SHA={sha}");
    println!("cargo:rustc-env=PROTOCOL_DROID_BUILD_DATE={build_date}");
}
