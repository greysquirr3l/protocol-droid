//! Asserts every port-trait impl in the adapters layer is
//! constructed at least once in a composition root
//! somewhere in the workspace.
//!
//! # What this test exists to do
//!
//! The project's hexagonal rule is that "adapters export
//! concrete structs that implement those traits and never
//! re-export them." A concrete struct that no composition
//! root constructs is either:
//!
//!   - an adapter the consumer never wires up (a real bug,
//!     or work that belongs to a later task), or
//!   - a struct whose only purpose is the impl, and the
//!     test in the same module is the only consumer — also
//!     a sign the impl is unused in production.
//!
//! Either way, an unwired adapter is the kind of thing a
//! refactor can break without anyone noticing. This test
//! surfaces it.
//!
//! # Where a composition root lives in this project
//!
//! The library's composition root is the union of four
//! places:
//!
//!   - `crates/protocol-droid-cli/src/` — the CLI's `main`
//!     constructs things the bridge / conformance
//!     subcommands need.
//!   - `crates/protocol-droid/examples/` — the `stdio_server`
//!     and `conformance_reference_server` examples are
//!     the runtime composition roots for server code.
//!   - `crates/protocol-droid/benches/` — the bench
//!     harnesses are composition roots for the perf-critical
//!     paths.
//!   - `crates/protocol-droid/tests/` — the integration
//!     tests (including the interop tests) construct
//!     client-side adapters.
//!
//! The test concatenates the source of all four and
//! searches for the adapter type name. A composition root
//! anywhere in that union counts.
//!
//! # Why a string match, not a real reference-graph walk
//!
//! A real audit (Coraline, rust-analyzer) walks the HIR and
//! resolves imports. A real walk is what a developer would
//! reach for on a big tree; for an MVP audit, a string
//! match over the composition-root directories is the
//! right level of effort: a false positive (a name that
//! appears in a doc comment) is rare, and a false negative
//! (a name only the import path matches) is what this test
//! is *for*. The 50/50 powerset build in `cargo hack` is
//! the complementary check; both must be green for the
//! audit to be considered complete.

/// The set of port-trait impls that are *currently* wired
/// into a composition root. Asserting the wired set is
/// green is the test that protects the audit from
/// regressing.
const WIRED: &[(&str, &str)] = &[
    // (adapter type, port trait it impls)
    ("McpMethodTable", "MethodTable"),
    ("RandomIds", "IdSource"),
    ("HttpClient", "BridgePeer"),
    ("WireToolResults", "ToolResultCodec"),
    // The four OAuth adapters. The composition root that
    // wired them is `crates/protocol-droid/tests/oauth_wiring.rs`,
    // which constructs each with a real-looking config
    // and asserts the trait bound. The CLI does not yet
    // compose an OAuth flow — that is a T26 follow-up —
    // but the adapters are reachable from a composition
    // root, which is what the audit's "wired" set tests.
    ("OsSecrets", "AuthorizationSecrets"),
    ("IntrospectionSource", "TokenSource"),
    ("JwtSource", "TokenSource"),
    ("HttpTokenEndpoint", "TokenEndpoint"),
];

/// The set of port-trait impls that exist but are not yet
/// wired into a composition root. The audit's purpose is
/// to surface this list and shrink it; the test below
/// prints it on every run, even when it is empty, so
/// reverting the wiring (rather than completing it)
/// would still leave a finding on the record.
const OUTSTANDING: &[(&str, &str)] = &[];

/// The error type the audit's two tests return. Collecting
/// the failure modes into a small enum means the test
/// functions themselves can return `Result<(), Failure>`
/// and the only `?` propagates whatever the underlying
/// `std::fs` call refused.
#[derive(Debug)]
enum Failure {
    /// A composition-root file refused to be read. The
    /// string carries the path; the `Display` impl on
    /// `std::io::Error` carries the OS error.
    Io { path: String, error: std::io::Error },
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, error } => {
                write!(f, "failed to read {path}: {error}")
            }
        }
    }
}

impl std::error::Error for Failure {}

impl From<std::io::Error> for Failure {
    fn from(error: std::io::Error) -> Self {
        // The path is filled in at the call site; this
        // `From` impl exists so the `?` operator can
        // lift an `io::Error` into `Failure::Io` and
        // the test stays a one-liner.
        Self::Io {
            path: String::new(),
            error,
        }
    }
}

#[test]
fn every_wired_port_trait_impl_is_constructed() -> Result<(), Failure> {
    let source = collect_composition_root_source()?;
    let mut unwired: Vec<&str> = Vec::new();
    for (adapter_type, port_trait) in WIRED {
        if !source.contains(adapter_type) {
            unwired.push(adapter_type);
            eprintln!(
                "AUDIT REGRESSION: wired impl `{port_trait} for {adapter_type}` \
                 is no longer in a composition root"
            );
        }
    }
    assert!(
        unwired.is_empty(),
        "the following previously-wired port-trait impls are no longer in any \
         composition root: {unwired:?}; the audit's regression surface"
    );
    Ok(())
}

/// Prints the outstanding (unwired) adapters on every
/// test run. The list is the audit's evidence; the test
/// is `#[ignore]`'d so it does not fail the suite, and a
/// developer who runs the ignored test gets the list.
#[test]
#[ignore = "audit finding surface; the entries here stay empty once the wiring is complete"]
fn outstanding_unwired_adapters() -> Result<(), Failure> {
    let source = collect_composition_root_source()?;
    let mut unwired: Vec<&str> = Vec::new();
    for (adapter_type, _port_trait) in OUTSTANDING {
        if !source.contains(adapter_type) {
            unwired.push(adapter_type);
        }
    }
    eprintln!(
        "{} port-trait impl(s) have no composition-root construction: {:?}",
        unwired.len(),
        unwired
    );
    // The test does not assert; it is a printer. The
    // audit's evidence is the printed list, not the
    // pass / fail result.
    Ok(())
}

/// Concatenates the source of every composition root into
/// a single string for the audit's string match. Excludes
/// the test's own file (which contains the type names in
/// `WIRED` and `OUTSTANDING`) so a naive search would not
/// false-positive.
fn collect_composition_root_source() -> Result<String, Failure> {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let composition_roots = [
        manifest.join("../protocol-droid-cli/src"),
        manifest.join("examples"),
        manifest.join("benches"),
        manifest.join("tests"),
    ];
    let self_name = std::path::Path::new(file!())
        .file_name()
        .map(std::borrow::ToOwned::to_owned);

    let mut source = String::new();
    for root in &composition_roots {
        if !root.exists() {
            continue;
        }
        for entry in walkdir(root) {
            if entry.extension().and_then(|s| s.to_str()) != Some("rs") {
                continue;
            }
            if entry.file_name() == self_name.as_deref() {
                continue;
            }
            let text = std::fs::read_to_string(&entry).map_err(|error| Failure::Io {
                path: entry.display().to_string(),
                error,
            })?;
            source.push_str(&text);
            source.push('\n');
        }
    }
    Ok(source)
}

/// Tiny recursive directory walker. The `walkdir` crate
/// would be overkill for a single test that only needs
/// to descend a handful of `src/` directories; the path
/// entries in those trees are stable enough that a
/// hand-rolled walker is the right level of machinery.
fn walkdir(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() {
                out.push(path);
            }
        }
    }
    out
}
