//! Build-time verification of the vendored spec tree.
//!
//! **This script never touches the network.** It reads the files already in
//! `spec/` and checks them against the digests in `spec/MANIFEST.toml`. A build
//! that needs to download a schema has already failed: refreshes are an
//! explicit `cargo xtask refresh-spec` away, never a side effect of compiling.
//!
//! `build.rs` cannot depend on the crate it builds, so the verification logic is
//! compiled from `src/domain/manifest.rs` directly via `#[path]`. That file must
//! stay free of `crate::` references and may only use the crates listed in
//! `[build-dependencies]`, or this script stops compiling.

use std::fs;
use std::path::Path;

// `pub` at the build-script crate root so the `pub` items in the shared module
// stay reachable. A private `mod` here makes `unreachable_pub` fire on every one
// of them, because nothing re-exports them from this crate.
#[path = "src/domain/manifest.rs"]
pub mod manifest;

use manifest::{Manifest, hex_digest};

fn main() {
    let spec_root = Path::new("spec");
    println!("cargo:rerun-if-changed=build.rs");

    let manifest_path = spec_root.join("MANIFEST.toml");
    println!("cargo:rerun-if-changed={}", manifest_path.display());

    let source = match fs::read_to_string(&manifest_path) {
        Ok(text) => text,
        Err(err) => fail(&format!(
            "could not read {}: {err}",
            manifest_path.display()
        )),
    };

    let manifest = match Manifest::parse(&source) {
        Ok(parsed) => parsed,
        Err(err) => fail(&format!("{err}")),
    };

    // Watch every vendored file, so editing one re-runs this script.
    for name in manifest.revision_names() {
        println!("cargo:rerun-if-changed=spec/{name}");
    }

    // A floating revision has no upstream tag, so it must be pinned by commit
    // alone; a tagged one must record the tag it claims. A mismatch means the
    // manifest was hand-edited into an inconsistent state.
    //
    // Also derive each vendored file's upstream path, so a typo in
    // `schema_prefix` fails here rather than at the next refresh, long after it
    // became wrong.
    for (name, spec) in &manifest.revisions {
        if spec.is_floating() && !spec.tag.is_empty() {
            fail(&format!(
                "revision `{name}` is marked floating but also records tag `{}`; a floating \
                 revision has no upstream tag",
                spec.tag
            ));
        }
        if !spec.is_floating() && spec.tag != *name {
            fail(&format!(
                "revision `{name}` records tag `{}`, which does not match its directory name",
                spec.tag
            ));
        }
        for required in manifest::RevisionSpec::SCHEMA_FILES {
            if !spec.files.contains_key(required) {
                fail(&format!(
                    "revision `{name}` does not record {required}; every revision must vendor both \
                     schema representations"
                ));
            }
        }
        for file in spec.files.keys() {
            if let Err(err) = spec.upstream_path(file) {
                fail(&format!("revision `{name}`: {err}"));
            }
        }
    }

    // Full digest and tree reconciliation, through the same implementation the
    // integration test uses. `files_on_disk` cannot fail here in a way worth
    // reporting differently: a missing revision directory shows up below as
    // every file in it being reported missing.
    if let Err(err) = manifest.verify(
        |revision, file| {
            let path = spec_root.join(revision).join(file);
            fs::read(&path)
                .map(|bytes| hex_digest(&bytes))
                .map_err(|err| format!("{}: {err}", path.display()))
        },
        |revision| {
            let dir = spec_root.join(revision);
            let Ok(entries) = fs::read_dir(&dir) else {
                return Vec::new();
            };
            let mut names: Vec<String> = entries
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
                .filter_map(|entry| entry.file_name().into_string().ok())
                .collect();
            names.sort();
            names
        },
    ) {
        fail(&format!("{err}"));
    }
}

/// Abort the build with a diagnostic.
///
/// A build script has no error type to return, and this project denies `panic!`
/// precisely so a failure never reads as a crash. A failed integrity check
/// genuinely is build-stopping, so this reports the problem as a normal error
/// and exits non-zero — which is what cargo surfaces to the user.
fn fail(message: &str) -> ! {
    eprintln!("error: vendored spec tree is invalid: {message}");
    eprintln!();
    eprintln!("  Run `cargo xtask refresh-spec <revision>` to re-download the vendored");
    eprintln!("  files and rewrite the manifest, then review `git diff`.");
    std::process::exit(1);
}
