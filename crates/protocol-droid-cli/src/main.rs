//! `protocol-droid` command-line entry point.
//!
//! The binary is a thin adapter over the `protocol-droid` library. It parses
//! arguments into a typed contract, hands that contract to a use case, and maps
//! the resulting domain error into an exit code. It contains no protocol logic
//! of its own.

use clap::Parser;

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
struct Cli {}

/// Entry point.
///
/// `clap` handles `--help` and `--version` from inside `parse`, exiting the
/// process before it can return. Combined with `arg_required_else_help`, that
/// means reaching the end of this function implies the arguments were already
/// dealt with.
///
/// # Errors
///
/// Returns an error only if the entry point itself fails. Argument errors are
/// reported by `clap`, which exits the process with its own status code.
#[expect(
    clippy::unnecessary_wraps,
    reason = "Every handler in this binary returns `anyhow::Result` per the project \
              convention for bin crates, and T18 gives `main` a genuinely fallible \
              dispatch. The lint clears itself at that point, which is why this is an \
              `expect` and not an `allow`: the suppression announces its own expiry."
)]
fn main() -> anyhow::Result<()> {
    Cli::parse();
    Ok(())
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
