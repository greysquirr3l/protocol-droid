//! `protocol-droid` command-line entry point.
//!
//! The binary is a thin adapter over the `protocol-droid` library. It parses
//! arguments into a typed contract, hands that contract to a use case, and maps
//! the resulting domain error into an exit code. It contains no protocol logic
//! of its own.

use clap::{Parser, Subcommand};
use protocol_droid_cli::bridge;

/// Top-level command-line contract for the `protocol-droid` binary.
///
/// The derived struct is the contract, not the raw arguments. `clap` has
/// already validated and converted every value by the time
/// [`Parser::parse`] returns, so nothing downstream re-checks the arguments and
/// no untyped string crosses into the library.
///
/// The subcommand groups — `bridge`, `probe` and `inspect`, `record` and
/// `replay`, and `conformance` — are added by T18 to T21. Until then a bare
/// invocation prints help and exits non-zero, rather than silently doing
/// nothing.
#[derive(Debug, Parser)]
#[command(
    name = "protocol-droid",
    version,
    about = "Model Context Protocol toolkit: bridge, probe, inspect, record, replay.",
    long_about = None,
    arg_required_else_help = true
)]
struct Cli {
    /// What to do.
    #[command(subcommand)]
    command: Command,
}

/// The subcommands that exist today.
///
/// Each is a thin dispatch into its own module. The group is a non-exhaustive
/// list on purpose — `probe`, `inspect`, `record`, `replay` and `conformance`
/// are T18 to T21, and naming only what is built keeps `--help` honest about
/// what the binary can actually do.
#[derive(Debug, Subcommand)]
enum Command {
    /// Validate and report a version-translating bridge between two peers.
    Bridge(bridge::BridgeArgs),
}

/// Entry point.
///
/// `clap` handles `--help` and `--version` from inside `parse`, exiting the
/// process before it can return. Combined with `arg_required_else_help`, that
/// means reaching this function implies the arguments were already dealt with
/// and `main` has something to dispatch.
///
/// # Errors
///
/// Propagates whatever the subcommand reports. Argument errors are reported by
/// `clap`, which exits the process with its own status code.
fn main() -> anyhow::Result<()> {
    // The runtime exists so the serving path can be `.await`ed from a plain
    // `fn main`. It is built once and used for whichever subcommand ran; a
    // subcommand with nothing to await never touches it.
    // Timers as well as IO. The router's rate limiter keeps its window on a
    // `tokio::time` sleep, so an `http` listen panicked on its first request
    // with a runtime that enabled IO alone — after the socket had already been
    // bound and announced, which is the worst moment to find out.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(dispatch(Cli::parse().command))
}

/// Runs the subcommand the arguments selected.
///
/// # Errors
///
/// Propagates whatever the subcommand reports.
async fn dispatch(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Bridge(args) => bridge::run(&args).await,
    }
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::error::ErrorKind;
    use clap::{CommandFactory, Parser};

    /// Clap's own consistency check over the command definition. It panics
    /// inside clap if an argument, group or default is inconsistent, which is
    /// the cheapest way to catch a malformed derive.
    #[test]
    fn command_definition_is_internally_consistent() {
        Cli::command().debug_assert();
    }

    /// The happy path: `--version` is recognised and short-circuits with a
    /// display-version error rather than being treated as an unknown argument.
    #[test]
    fn version_flag_is_recognised() {
        let parsed = Cli::try_parse_from(["protocol-droid", "--version"]);
        assert!(
            matches!(&parsed, Err(err) if err.kind() == ErrorKind::DisplayVersion),
            "expected DisplayVersion, got {parsed:?}",
        );
    }

    /// The failure path: a bare invocation must print help and exit non-zero
    /// instead of reporting success while doing nothing.
    #[test]
    fn bare_invocation_is_rejected_with_help() {
        let parsed = Cli::try_parse_from(["protocol-droid"]);
        assert!(
            matches!(
                &parsed,
                Err(err) if err.kind() == ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            ),
            "expected help-on-missing-argument, got {parsed:?}",
        );
    }

    /// The failure path: an unknown flag is rejected rather than ignored.
    #[test]
    fn unknown_flag_is_rejected() {
        let parsed = Cli::try_parse_from(["protocol-droid", "--not-a-flag"]);
        assert!(
            matches!(&parsed, Err(err) if err.kind() == ErrorKind::UnknownArgument),
            "expected UnknownArgument, got {parsed:?}",
        );
    }
}
