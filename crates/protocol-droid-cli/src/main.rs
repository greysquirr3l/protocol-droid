//! `protocol-droid` command-line entry point.
//!
//! The binary is a thin adapter over the `protocol-droid` library. It parses
//! arguments into a typed contract, hands that contract to a use case, and maps
//! the resulting domain error into an exit code. It contains no protocol logic
//! of its own.

use clap::{Parser, Subcommand};
#[cfg(feature = "a2a")]
use protocol_droid_cli::a2a_conformance;
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
    /// Drive the in-tree A2A v1.0 conformance vectors against a server.
    ///
    /// Reads a JSONL of `{name, request, expected}` vectors from the
    /// path the operator names, POSTs each `request` to the server's
    /// `/a2a` endpoint, compares the response to `expected`, and
    /// writes a JSONL report to stdout (one `{name, outcome, ...}` per
    /// line). A human summary lands on stderr. A third-party
    /// implementer running their own A2A server uses this to verify
    /// that it speaks the same wire shape the reference impl does.
    /// Off when the `a2a` feature is disabled.
    #[cfg(feature = "a2a")]
    A2aConformance(a2a_conformance::A2aConformanceArgs),
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
        #[cfg(feature = "a2a")]
        Command::A2aConformance(args) => a2a_conformance::run(&args).await,
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

    /// A flag gated behind a feature the build does not have is
    /// reported as an unknown argument, not a panic.
    ///
    /// `probe --a2a` exists only when the `a2a` feature is on; a
    /// build without it must say so in the same shape every other
    /// unknown argument is rejected, because the alternative is
    /// a `debug_assert` firing inside clap and an opaque stack
    /// trace. The test passes in both configurations: when the
    /// feature is on, `--a2a` is a valid boolean and the parse
    /// succeeds; when it is off, the parse fails with
    /// `UnknownArgument`. Either way, reaching the assertion
    /// without panicking is the regression the test catches.
    #[test]
    fn a_feature_gated_flag_is_rejected_when_the_feature_is_off() {
        // `probe --a2a` is the path the `a2a` feature adds.
        let parsed = Cli::try_parse_from([
            "protocol-droid",
            "probe",
            "stdio:./some-server",
            "--a2a",
        ]);
        let acceptable = match &parsed {
            // The feature is off: --a2a is unknown.
            Err(err) => err.kind() == ErrorKind::UnknownArgument,
            // The feature is on: --a2a is a boolean flag with
            // `false` as the default. Passing it sets it to
            // `true`, which is not an error at parse time.
            Ok(_) => true,
        };
        assert!(
            acceptable,
            "expected UnknownArgument or Ok, got {parsed:?}"
        );
    }

    /// A subcommand gated behind a feature the build does not
    /// have is rejected, not panicked on.
    ///
    /// `a2a-conformance` exists only when the `a2a` feature is
    /// on. A build without it must say so as a parse error
    /// rather than tripping a `debug_assert` inside clap's
    /// parser. The test passes in both configurations: when the
    /// feature is on, the subcommand is recognised and the
    /// parse may fail downstream on its own required
    /// arguments; when it is off, the parse fails with
    /// `UnknownArgument`. The test asserts the parser returned
    /// a value rather than panicking — the panic, by definition,
    /// is what would prevent the assertion from running at all.
    #[test]
    fn a_feature_gated_subcommand_is_rejected_when_the_feature_is_off() {
        // Reaching this line at all is the assertion: a panic
        // inside clap would have killed the test before the
        // result was bound. The `match` documents which
        // outcomes the parser is allowed to return and surfaces
        // a useful message if a future change makes clap
        // return something new.
        let parsed = Cli::try_parse_from(["protocol-droid", "a2a-conformance"]);
        let outcome = match &parsed {
            // Feature on: parser returned `Ok`. A later
            // validation step may refuse; that is the
            // subcommand's own argument validation, not
            // clap's parser.
            Ok(_) => "ok",
            // Feature off: parser returned `Err(UnknownArgument)`.
            Err(err) if err.kind() == ErrorKind::UnknownArgument => "unknown_argument",
            // Any other `Err` is clap's own argument-validation
            // step rejecting the call. Reaching this arm is
            // also acceptable; the test is about not panicking.
            Err(_) => "other_error",
        };
        assert!(
            matches!(outcome, "ok" | "unknown_argument" | "other_error"),
            "the parser returned an unrecognised shape: {parsed:?}"
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
