//! Build-time verification of the vendored spec tree, and generation of the
//! per-revision wire types.
//!
//! **This script never touches the network.** It reads the files already in
//! `spec/` and checks them against the digests in `spec/MANIFEST.toml`, then
//! emits Rust source for each revision into `OUT_DIR`. A build that needs to
//! download a schema has already failed: refreshes are an explicit
//! `cargo xtask refresh-spec` away, never a side effect of compiling.
//!
//! `build.rs` cannot depend on the crate it builds, so the shared code is
//! compiled from `src/` directly via `#[path]`. Those files must stay free of
//! `crate::` references and may only use the crates listed in
//! `[build-dependencies]`, or this script stops compiling.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

// `pub` at the build-script crate root so the `pub` items in the shared modules
// stay reachable. A private `mod` here makes `unreachable_pub` fire on every one
// of them, because nothing re-exports them from this crate.
#[path = "src/domain/manifest.rs"]
pub mod manifest;

// The generator lives under `src/` so it is compiled into the library too and
// can be tested there, but it is *invoked* here — the library exposes the types
// this script writes, it does not run the generator. Compiling it twice is
// cheaper than duplicating it, and duplicating it is how a wire type and its
// schema check stop agreeing.
#[path = "src/generator/schema_model.rs"]
pub mod schema_model;

#[path = "src/generator/emit.rs"]
pub mod emit;

use manifest::{Manifest, hex_digest};

fn main() {
    let spec_root = Path::new("spec");
    let out_dir_var = std::env::var("OUT_DIR").unwrap_or_else(|_| fail("OUT_DIR is unset"));
    let out_dir = Path::new(&out_dir_var);
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

    // Generation runs only after the digests pass. Emitting wire types from an
    // unverified tree would let a tampered schema become a committed type, and
    // the whole point of the check above is that it cannot.
    generate_wire_types(&manifest, spec_root, out_dir);
}

/// Emit one module per revision into `OUT_DIR`, plus the module that re-exports
/// them.
fn generate_wire_types(manifest: &Manifest, spec_root: &Path, out_dir: &Path) {
    let mut index = String::with_capacity(4096);
    let () = index.push_str("// GENERATED — do not edit. Produced by `build.rs`.\n");
    let () = index.push_str("//\n");
    let () = index.push_str("// One module per vendored revision. Every one is compiled\n");
    let () = index.push_str("// unconditionally: which revision a process speaks is decided at\n");
    let () =
        index.push_str("// runtime by negotiation, never at compile time by a feature flag.\n");
    let () = index.push_str("//\n");
    let () = index.push_str("// Each module exposes the same top-level type names, so the codec\n");
    let () = index.push_str("// layer can instantiate one implementation per revision against a\n");
    let () = index.push_str("// uniform set of names.\n\n");

    for revision in manifest.revision_names() {
        let path = spec_root.join(revision).join("schema.json");
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) => fail(&format!("could not read {}: {err}", path.display())),
        };

        let parsed = match schema_model::parse(revision, &text) {
            Ok(parsed) => parsed,
            Err(err) => fail(&format!("{err}")),
        };
        let generated = emit::emit(&parsed);

        if !generated.dangling_refs.is_empty() {
            fail(&format!(
                "{revision}: `$ref` to definitions that do not exist: {:?}",
                generated.dangling_refs
            ));
        }

        let module = emit::module_ident(revision);
        let target = out_dir.join(format!("{module}.rs"));
        if let Err(err) = fs::write(&target, &generated.source) {
            fail(&format!("could not write {}: {err}", target.display()));
        }
        let _ = writeln!(index, "pub mod {module};");
    }

    let index_path = out_dir.join("wire.rs");
    if let Err(err) = fs::write(&index_path, index) {
        fail(&format!("could not write {}: {err}", index_path.display()));
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
