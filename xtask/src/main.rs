//! Workspace maintenance tasks, run as `cargo xtask <task>`.
//!
//! This binary is developer tooling and is never published. Its one job in T02
//! is `refresh-spec`, which re-downloads a vendored MCP spec revision and
//! updates the provenance manifest so the build's verification still passes.
//!
//! Everything here is deliberately explicit. Nothing runs during a build, and
//! nothing here mutates a vendored file without also updating the manifest — a
//! mismatch between the two fails verification, which is the point of T02.

use anyhow::Result;
use clap::{Parser, Subcommand};

// `pub` rather than private: these modules' items are used across sibling
// modules, and a private module would force either redundant `pub(crate)` or an
// `unreachable_pub` error on every item. In a binary crate, `pub` at the crate
// root is the maximum, so nothing is actually exported.
pub mod manifest_edit;
pub mod refresh;
pub mod refresh_cited;

// The vendored spec manifest, compiled from the canonical source file.
//
// `xtask` deliberately does **not** depend on `protocol-droid-schema`. That
// crate's build script verifies the vendored tree, so a dependency on it would
// make `cargo xtask refresh-spec` fail to build whenever the tree is broken —
// which is exactly the moment somebody needs to run it. The tool that repairs a
// corrupted tree must not be gated on the tree being valid.
//
// Compiling the same file with `#[path]` keeps a single definition of the
// manifest model across the whole workspace, so the tool and the thing it
// verifies can never disagree about the format.
//
// `pub` at the crate root, because the items inside are `pub` and a private
// module would make `unreachable_pub` fire on every one of them.
#[path = "../../crates/protocol-droid-schema/src/domain/manifest.rs"]
pub mod manifest;

#[derive(Debug, Parser)]
#[command(
    name = "xtask",
    about = "Workspace maintenance tasks for protocol-droid.",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Re-download a vendored MCP spec revision and update MANIFEST.toml.
    ///
    /// This is the ONLY way a vendored file changes. A build never touches the
    /// network, so the schemas in the tree are exactly what the diff shows.
    RefreshSpec(RefreshSpecArgs),
    /// Re-download a cited external document and update its digest in
    /// MANIFEST.toml.
    RefreshCited(RefreshCitedArgs),
}

#[derive(Debug, clap::Args)]
struct RefreshSpecArgs {
    /// Revision to refresh, e.g. `2026-07-28` or `draft`.
    ///
    /// Must already be vendored: this command updates a revision, it does not
    /// add one. Adding a revision means adding its entry to MANIFEST.toml, then
    /// running this to fill in the digests.
    revision: String,

    /// For a floating revision, re-resolve `main` to its current commit.
    ///
    /// Without this, `draft` is re-fetched at the commit already recorded,
    /// which is a deliberate no-op. With it, `draft` actually moves forward.
    #[arg(long)]
    newest: bool,
}

/// Re-download the external documents the MCP authorization sections cite.
///
/// Separate from `refresh-spec` because the pinning differs: a revision is
/// pinned to an upstream commit, a cited document to a URL. Like `refresh-spec`
/// it is the only way one of those files changes.
#[derive(Debug, clap::Args)]
struct RefreshCitedArgs {
    /// Documents to refresh. Omit to refresh all of them.
    ///
    /// A document must already have an entry in MANIFEST.toml; this command
    /// fills in digests for entries that exist, it does not invent entries.
    /// That is where the URL to fetch from is recorded, so a document with no
    /// entry has nothing to fetch and nothing to verify against.
    ids: Vec<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match &cli.command {
        Command::RefreshSpec(args) => {
            let spec_root = refresh::locate_spec_root()?;
            refresh::refresh(&spec_root, &args.revision, args.newest)
        }
        Command::RefreshCited(args) => refresh_cited::run(&args.ids),
    }
}
