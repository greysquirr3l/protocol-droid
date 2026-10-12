//! `protocol-droid` command-line entry point.
//!
//! The binary is a thin adapter over the `protocol-droid` library. It parses
//! arguments into a typed contract, hands that contract to a use case, and maps
//! the resulting domain error into an exit code. It contains no protocol logic
//! of its own.

use clap::{Parser, Subcommand};
use protocol_droid_cli::{bridge, conformance, inspect, probe, record, replay};

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
    // `--version` reports the crate version, a short git SHA
    // (with a `-dirty` suffix when the working tree had
    // uncommitted changes at build time), and the last
    // commit's UTC date. The two env vars are baked by
    // `build.rs`; reading them with `env!` rather than a
    // `std::env::var` keeps the format a compile-time
    // constant and removes the runtime lookup.
    version = concat!(
        env!("CARGO_PKG_VERSION"),
        " (",
        env!("PROTOCOL_DROID_GIT_SHA"),
        ", ",
        env!("PROTOCOL_DROID_BUILD_DATE"),
        ")",
    ),
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
    /// List and call primitives on a server, one-shot.
    ///
    /// Runs the same connection the consumer path runs, then performs
    /// one `list`, `schema`, or `call` and writes the answer as JSON on
    /// stdout. A REPL would require a human at a terminal; an agent
    /// calling a tool should not have to hold the whole result in its
    /// context window to use it. The one-shot form is the scriptable
    /// form, and the scriptable form is the one that exists.
    Inspect(inspect::InspectArgs),
    /// Fingerprint what revisions an MCP server actually speaks.
    ///
    /// Tries `initialize` (or `server/discover` on `2026-07-28`) at every
    /// revision this build has wire types for, reports a version ×
    /// capability matrix on stdout, and exits non-zero when nothing was
    /// accepted.
    Probe(probe::ProbeArgs),
    /// Run the official MCP conformance suite against a server or client.
    ///
    /// Three modes: `server` (suite → your server URL), `client` (suite →
    /// your client subprocess), and `self` (suite → this crate's own
    /// fixtures — wired by T21 phase 2).
    Conformance(conformance::ConformanceArgs),
    /// Replay a captured session against a target revision, frame by frame.
    ///
    /// Reads a JSONL session file (one `{version, frame}` per line),
    /// runs each frame through the same translator the bridge uses, and
    /// writes a per-frame `forwarded` / `downgraded` / `refused` verdict
    /// to stdout. A human summary lands on stderr.
    Replay(replay::ReplayArgs),
    /// Record bridged traffic as a redacted JSONL session log.
    ///
    /// Wraps a bridge's input and output so every frame that crosses is
    /// written to a file as one JSONL line: `direction`, `version`,
    /// `clock_ns`, and the redacted frame. The output is the input the
    /// `replay` subcommand reads.
    Record(record::RecordArgs),
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
        Command::Conformance(args) => conformance::run(&args).await,
        Command::Inspect(args) => inspect::run(&args).await,
        Command::Probe(args) => probe::run(&args).await,
        Command::Replay(args) => replay::run(&args).await,
        Command::Record(args) => record::run(&args).await,
    }
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command};
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

    /// `conformance server` round-trips through clap with every flag the
    /// subcommand names. Exercising `try_parse_from` here, where the bin's
    /// `Cli` and `Command` types live, is what keeps the conformance types
    /// alive in the lib test build.
    #[test]
    fn conformance_server_round_trips_through_clap() {
        let parsed = Cli::try_parse_from([
            "protocol-droid",
            "conformance",
            "server",
            "--url",
            "http://127.0.0.1:8080/mcp",
            "--spec-version",
            "2024-11-05",
            "--scenario",
            "tools_call",
            "--suite",
            "active",
            "--json",
        ]);
        let Ok(parsed) = parsed else {
            assert_eq!(Ok(()), Err("clap parse failed"));
            return;
        };
        let server = match parsed.command {
            Command::Conformance(crate::conformance::ConformanceArgs {
                mode: crate::conformance::ConformanceMode::Server(server),
            }) => server,
            other => {
                assert_eq!(
                    "Conformance/Server".to_owned(),
                    format!("{other:?}"),
                    "expected Conformance/Server, got {other:?}"
                );
                return;
            }
        };
        assert_eq!(server.url, "http://127.0.0.1:8080/mcp");
        assert_eq!(server.spec_version, "2024-11-05");
        assert_eq!(server.scenario.as_deref(), Some("tools_call"));
        assert_eq!(server.suite.as_deref(), Some("active"));
        assert!(server.json);
    }

    /// `conformance client` parses through clap the same way as `server`.
    #[test]
    fn conformance_client_round_trips_through_clap() {
        let parsed = Cli::try_parse_from([
            "protocol-droid",
            "conformance",
            "client",
            "--command",
            "./my-client",
            "--spec-version",
            "2025-06-18",
        ]);
        let Ok(parsed) = parsed else {
            assert_eq!(Ok(()), Err("clap parse failed"));
            return;
        };
        let client = match parsed.command {
            Command::Conformance(crate::conformance::ConformanceArgs {
                mode: crate::conformance::ConformanceMode::Client(client),
            }) => client,
            other => {
                assert_eq!(
                    "Conformance/Client".to_owned(),
                    format!("{other:?}"),
                    "expected Conformance/Client, got {other:?}"
                );
                return;
            }
        };
        assert_eq!(client.command, "./my-client");
        assert_eq!(client.spec_version, "2025-06-18");
        assert!(client.scenario.is_none());
    }

    /// `conformance self` parses, and `--port` defaults to `0` so the OS
    /// chooses a free port for the fixture server.
    #[test]
    fn conformance_self_round_trips_through_clap() {
        let parsed = Cli::try_parse_from([
            "protocol-droid",
            "conformance",
            "self",
            "--spec-version",
            "2026-07-28",
        ]);
        let Ok(parsed) = parsed else {
            assert_eq!(Ok(()), Err("clap parse failed"));
            return;
        };
        let self_args = match parsed.command {
            Command::Conformance(crate::conformance::ConformanceArgs {
                mode: crate::conformance::ConformanceMode::Self_(self_args),
            }) => self_args,
            other => {
                assert_eq!(
                    "Conformance/Self".to_owned(),
                    format!("{other:?}"),
                    "expected Conformance/Self, got {other:?}"
                );
                return;
            }
        };
        assert_eq!(self_args.spec_version, "2026-07-28");
        assert_eq!(self_args.port, 0, "--port default must be 0");
    }
}
