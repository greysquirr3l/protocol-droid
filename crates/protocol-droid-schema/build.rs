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

use manifest::{Manifest, hex_digest, normalize_line_endings};

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
    let cited_dir = spec_root.join(manifest::CITED_DOCUMENT_DIR);
    println!("cargo:rerun-if-changed={}", cited_dir.display());
    // Watch each file individually as well as the directory. A directory entry
    // is re-stat'd, but appending to or editing a file *inside* it does not
    // change the directory's own mtime — only creating or deleting an entry
    // does. Watching the directory alone therefore misses exactly the tampering
    // the verification exists to catch: someone who edits a vendored document
    // in place would get a clean `cargo build` and a stale wire type.
    for document in manifest.rfc_documents.values() {
        println!(
            "cargo:rerun-if-changed={}",
            cited_dir.join(&document.file).display()
        );
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
                .map(|bytes| hex_digest(&normalize_line_endings(&bytes)))
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

    verify_cited_documents(&manifest, &cited_dir);

    // Generation runs only after the digests pass. Emitting wire types from an
    // unverified tree would let a tampered schema become a committed type, and
    // the whole point of the check above is that it cannot.
    generate_wire_types(&manifest, spec_root, out_dir);
}

/// Verify the cited external documents against the manifest.
///
/// These get the same treatment as the revision tree, for the same reason: they
/// are the source the OAuth wire types are written against, so a document that
/// changed under us must fail the build rather than leave a committed type
/// disagreeing with the text it claims to implement.
///
/// One directory, one flat reconciliation — a document added to `cited/`
/// without a manifest entry is fatal, exactly as an unrecorded file in a
/// revision directory is.
fn verify_cited_documents(manifest: &Manifest, cited_dir: &Path) {
    if let Err(err) = manifest.verify_rfc_documents(
        |file| {
            let path = cited_dir.join(file);
            fs::read(&path)
                .map(|bytes| hex_digest(&normalize_line_endings(&bytes)))
                .map_err(|err| format!("{}: {err}", path.display()))
        },
        || {
            let Ok(entries) = fs::read_dir(cited_dir) else {
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

    let mut cases = PropertyCases::new();

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
        cases.add(revision, &module, &generated.type_names);
    }

    let index_path = out_dir.join("wire.rs");
    if let Err(err) = fs::write(&index_path, index) {
        fail(&format!("could not write {}: {err}", index_path.display()));
    }

    cases.write(out_dir);
}

/// The exhaustive round-trip table the property suite consumes.
///
/// One entry per `(revision, definition)` pair the vendored schemas define.
/// The table is generated rather than written out because the alternative is a
/// hand-maintained list that goes stale the moment a revision gains a
/// definition — and a stale list is a hole in the coverage that nothing
/// reports, which is the failure this table exists to prevent.
///
/// The type path is the generator's own mapping, so the table cannot name a
/// type that does not exist: the test target fails to compile. What the
/// generated table does *not* assert is the mapping itself — that the
/// definition `CallToolResult` really is the type `CallToolResult` — and the
/// hand-written table in `tests/property.rs` is what asserts that.
struct PropertyCases {
    /// The macro invocation being built.
    body: String,
    /// How many entries have been added, so a collision can be numbered.
    count: usize,
}

impl PropertyCases {
    /// An empty table.
    const fn new() -> Self {
        Self {
            body: String::new(),
            count: 0,
        }
    }

    /// Add every definition of one revision.
    ///
    /// `module` is the revision's generated module and `definitions` its
    /// definition names, in the order the schema lists them.
    fn add(&mut self, revision: &str, module: &str, definitions: &[String]) {
        let mut used: Vec<String> = Vec::with_capacity(definitions.len());
        for definition in definitions {
            let mut ident = format!("case_{}_{}", module, snake(definition));
            // Two definitions can snake to the same identifier — `Foo-Bar` and
            // `Foo_Bar` both become `foo_bar`. Numbering the second one keeps
            // the names unique without making the first one uglier than it has
            // to be.
            let mut suffix = 1;
            while used.contains(&ident) {
                suffix += 1;
                ident = format!("case_{}_{}_{suffix}", module, snake(definition));
            }
            used.push(ident.clone());

            let _ = writeln!(
                self.body,
                "    {revision:?} / {definition:?} as {ident}\n        : protocol_droid_schema::wire::{module}::{},",
                emit::type_ident(definition)
            );
            self.count += 1;
        }
    }

    /// Write the table into `out_dir`.
    fn write(&self, out_dir: &Path) {
        let mut source = String::with_capacity(self.body.len() + 1024);
        let () = source.push_str("// GENERATED — do not edit. Produced by `build.rs`.\n");
        let () = source.push_str("//\n");
        let () = source.push_str("// One entry per `(revision, definition)` pair the vendored\n");
        let () =
            source.push_str("// schemas define, naming the Rust type the generator emits for\n");
        let () =
            source.push_str("// that definition. The property suite round-trips a value through\n");
        let _ = writeln!(source, "// every one of them: {} pairs.", self.count);
        let () = source.push_str("//\n");
        let () =
            source.push_str("// The type path is the generator's own mapping, so a definition\n");
        let () = source.push_str("// that did not produce a type would fail the test target's\n");
        let () = source.push_str("// compilation rather than quietly skip the round trip.\n\n");
        let () = source.push_str("exhaustive_round_trips! {\n");
        let () = source.push_str(&self.body);
        let () = source.push_str("}\n");

        let path = out_dir.join("property_cases.rs");
        if let Err(err) = fs::write(&path, source) {
            fail(&format!("could not write {}: {err}", path.display()));
        }
    }
}

/// A name in snake case, for a test identifier.
///
/// This is the *test* naming rule and not the wire naming rule: the wire names
/// come from the schema and are reproduced exactly, while a test identifier
/// only has to be unique and readable. Runs of capitals stay together, so
/// `JSONRPCRequest` becomes `jsonrpc_request` rather than
/// `j_s_o_n_r_p_c_request`.
fn snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    let chars: Vec<char> = name.chars().collect();
    for (index, ch) in chars.iter().copied().enumerate() {
        if ch.is_ascii_uppercase() {
            let previous_is_lower =
                index > 0 && chars.get(index - 1).is_some_and(char::is_ascii_lowercase);
            let next_is_lower = chars.get(index + 1).is_some_and(char::is_ascii_lowercase);
            if index > 0 && (previous_is_lower || next_is_lower) {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    out
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
    eprintln!("  A revision is re-downloaded with `cargo xtask refresh-spec <revision>`,");
    eprintln!("  and a cited document with `cargo xtask refresh-cited <document>`.");
    eprintln!("  Either rewrites the manifest's digests; review `git diff` afterwards.");
    std::process::exit(1);
}
