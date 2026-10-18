//! `protocol-droid conformance` — wrap the official MCP conformance suite.
//!
//! ## What this is
//!
//! A thin adapter over [`@modelcontextprotocol/conformance`] at the version
//! pinned in [`conformance/PINNED_VERSION`]. The suite is a TypeScript
//! package distributed through npm, and the wrapper exists for two reasons:
//!
//! 1. The pinned version is one file in this crate, not a `npx @latest` that
//!    silently tracks the alpha channel and turns a green CI into a red one
//!    with no code change. See [`conformance/README.md`] for the refresh
//!    protocol.
//! 2. The wrapper exposes the suite as a typed subcommand that takes parsed
//!    flags (`--spec-version`, `--scenario`) and forwards the rest of the
//!    arguments to the suite verbatim — `--suite-arg --foo` lands on the
//!    suite as `--foo`. The suite's own `--help` is the surface, just gated
//!    through a pin.
//!
//! ## Three modes
//!
//! - `conformance server` — runs the suite's *server* scenarios against an
//!   MCP server the operator names. `--url` is the server's endpoint; the
//!   suite speaks Streamable HTTP to it.
//! - `conformance client` — runs the suite's *client* scenarios against an
//!   MCP client the operator names. `--command` is the command the suite
//!   will spawn; the suite reads the server URL it is exercising from
//!   environment variables and the scenario from `MCP_CONFORMANCE_SCENARIO`.
//! - `conformance self` — runs the suite against this crate's own fixture
//!   server and client, so the full MVP wire is what gets validated. This is
//!   the mode CI runs.
//!
//! ## What the wrapper does *not* do
//!
//! It does not parse or pre-validate the suite's output. A pass/fail is the
//! suite's exit code, surfaced unchanged. The CI matrix post-processes the
//! output to apply the [`conformance/ALLOWLIST.md`] allow-list, but that
//! happens outside this binary.
//!
//! ## Why `npx`, not a vendored copy
//!
//! The suite is a long-running test framework, and pulling a frozen copy of
//! it into this repo would mean tracking every dependency of *its*
//! dependencies when the suite itself already does that. The pin is the
//! control surface; the npm registry is the cache.
//!
//! [`conformance/README.md`]: ../conformance/README.md
//! [`conformance/ALLOWLIST.md`]: ../conformance/ALLOWLIST.md
//! [`@modelcontextprotocol/conformance`]: https://github.com/modelcontextprotocol/conformance

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use clap::{Args, Subcommand};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::time::timeout;

/// What the operator asked the conformance wrapper to do.
#[derive(Debug, Args)]
pub struct ConformanceArgs {
    /// Which suite mode to run.
    #[command(subcommand)]
    pub mode: ConformanceMode,
}

/// The three conformance modes.
#[derive(Debug, Subcommand)]
pub enum ConformanceMode {
    /// Run the suite's server scenarios against an MCP server the operator names.
    Server(ServerArgs),
    /// Run the suite's client scenarios against an MCP client the operator names.
    Client(ClientArgs),
    /// Run the suite against this crate's own fixture server and client.
    ///
    /// The fixtures are started by this wrapper on a local port, and the
    /// suite is pointed at them. This is the mode the CI matrix exercises.
    Self_(SelfArgs),
}

/// Args for `protocol-droid conformance server`.
#[derive(Debug, Args)]
pub struct ServerArgs {
    /// The MCP server's URL, e.g. `http://127.0.0.1:8080/mcp`.
    #[arg(long, value_name = "URL")]
    pub url: String,

    /// The spec revision the suite should target.
    ///
    /// One of `2024-11-05`, `2025-03-26`, `2025-06-18`, `2025-11-25`,
    /// `2026-07-28`, `draft`. The suite decides stateful-vs-stateless
    /// lifecycle from this value.
    #[arg(long, value_name = "VERSION")]
    pub spec_version: String,

    /// A single scenario name to run, instead of the whole suite.
    ///
    /// Off by default — without this flag the suite runs every scenario it
    /// supports for the chosen revision. Useful in CI bisects.
    #[arg(long, value_name = "NAME")]
    pub scenario: Option<String>,

    /// The revision subset the suite should exercise.
    ///
    /// Forwarded to `--suite` unchanged. The suite's own choices are
    /// `active` (the default; excludes pending and draft-spec scenarios),
    /// `all`, `draft` (draft-spec scenarios only), and `pending`.
    #[arg(long, value_name = "SUITE")]
    pub suite: Option<String>,

    /// Extra arguments appended to the suite invocation verbatim.
    ///
    /// Each `--suite-arg X` becomes `X` on the suite's command line. Use
    /// this for flags the wrapper does not name explicitly — the suite's
    /// own `--help` is the surface.
    #[arg(long = "suite-arg", value_name = "ARG")]
    pub suite_args: Vec<String>,

    /// How long to wait for the suite to finish before giving up.
    ///
    /// Default 10 minutes, which is what a full server-scenario run takes on
    /// the slowest matrix leg. A bisect that picks one scenario can lower
    /// this so a hang surfaces as a typed error rather than a stuck job.
    #[arg(long, value_name = "DURATION", default_value = "10m")]
    pub timeout: String,

    /// Print a JSON report of what was run, on stdout.
    ///
    /// The report names the pinned version, the spec revision, the scenario
    /// subset, and the wrapper's exit code — it does not parse the suite's
    /// own output. The human summary (suite's stdout) goes to stderr.
    #[arg(long)]
    pub json: bool,
}

/// Args for `protocol-droid conformance client`.
#[derive(Debug, Args)]
pub struct ClientArgs {
    /// The command the suite should spawn to act as the client under test.
    ///
    /// `argv[0]` is the program; `argv[1..]` is the rest. The suite provides
    /// the server URL and the scenario name via `MCP_CONFORMANCE_SCENARIO`
    /// and a transport-specific URL env var, per the suite's contract.
    #[arg(long, value_name = "COMMAND")]
    pub command: String,

    /// The spec revision the suite should target.
    #[arg(long, value_name = "VERSION")]
    pub spec_version: String,

    /// A single scenario name to run, instead of the whole suite.
    #[arg(long, value_name = "NAME")]
    pub scenario: Option<String>,

    /// The revision subset — `active`, `all`, `draft`, `pending`.
    #[arg(long, value_name = "SUITE")]
    pub suite: Option<String>,

    /// Extra arguments appended to the suite invocation verbatim.
    #[arg(long = "suite-arg", value_name = "ARG")]
    pub suite_args: Vec<String>,

    /// How long to wait for the suite to finish before giving up.
    #[arg(long, value_name = "DURATION", default_value = "10m")]
    pub timeout: String,

    /// Print a JSON report of what was run.
    #[arg(long)]
    pub json: bool,
}

/// Args for `protocol-droid conformance self`.
#[derive(Debug, Args)]
pub struct SelfArgs {
    /// The spec revision the suite should target.
    #[arg(long, value_name = "VERSION")]
    pub spec_version: String,

    /// The local port the fixture server should listen on.
    ///
    /// The fixture server is started by this wrapper, exercised by the suite
    /// against `http://127.0.0.1:<port>/mcp`, and torn down on exit. The
    /// fixture client is spawned as a stdio subprocess by the suite's own
    /// client runner, with the same lifecycle.
    #[arg(long, value_name = "PORT", default_value = "0")]
    pub port: u16,

    /// A single scenario name to run, instead of the whole suite.
    #[arg(long, value_name = "NAME")]
    pub scenario: Option<String>,

    /// The revision subset — `active`, `all`, `draft`, `pending`.
    #[arg(long, value_name = "SUITE")]
    pub suite: Option<String>,

    /// Which half to run: this crate's server, its client, or both.
    ///
    /// Defaults to both, except that naming a `--scenario` runs the server half
    /// only, since a scenario belongs to one side. Pass `--side client` to run
    /// a client scenario by name.
    #[arg(long, value_enum)]
    pub side: Option<Side>,

    /// How long to wait for the suite to finish before giving up.
    #[arg(long, value_name = "DURATION", default_value = "10m")]
    pub timeout: String,

    /// Print a JSON report of what was run.
    #[arg(long)]
    pub json: bool,
}

/// Which half of `conformance self` to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Side {
    /// This crate's reference server, against the suite's server scenarios.
    Server,
    /// This crate's reference client, against the suite's mock servers.
    Client,
    /// Both, server first.
    Both,
}

/// What `npx` returned, and how the wrapper decided to report it.
#[derive(Debug, serde::Serialize)]
struct Report {
    /// The pinned suite version, exactly as `npx` resolved it.
    pinned_version: String,
    /// The spec revision the suite was asked to target.
    spec_revision: String,
    /// `server`, `client`, or `self`.
    mode: &'static str,
    /// The argv handed to `npx`, for the audit trail.
    argv: Vec<String>,
    /// The suite's own exit code, surfaced unchanged.
    suite_exit_code: i32,
    /// Wall-clock duration, for the run summary.
    elapsed: String,
    /// `pass`, `fail`, or `timeout` — three-valued, never a second opinion.
    outcome: &'static str,
}

/// Runs `protocol-droid conformance` against the parsed subcommand.
///
/// # Errors
///
/// Returns an `anyhow` error when the pinned-version file is missing or
/// unreadable, when `npx` itself failed to launch (e.g. Node not
/// installed), when the operator passed an unparseable `--timeout`, or
/// when the suite did not finish within the budget. A suite that exited
/// non-zero is **not** an error here — that is the run's answer, and it is
/// what CI post-processes against the allow-list.
pub async fn run(args: &ConformanceArgs) -> anyhow::Result<()> {
    match &args.mode {
        ConformanceMode::Server(server) => run_server(server).await,
        ConformanceMode::Client(client) => run_client(client).await,
        ConformanceMode::Self_(self_args) => run_self(self_args).await,
    }
}

/// Resolves the pinned version, then runs the suite in server mode.
async fn run_server(args: &ServerArgs) -> anyhow::Result<()> {
    let pinned = read_pinned_version()?;
    let npx_argv = build_server_argv(&pinned, args);
    invoke_suite(
        npx_argv,
        &args.timeout,
        &pinned,
        "server",
        &args.spec_version,
        args.json,
    )
    .await
}

/// Resolves the pinned version, then runs the suite in client mode.
async fn run_client(args: &ClientArgs) -> anyhow::Result<()> {
    let pinned = read_pinned_version()?;
    let npx_argv = build_client_argv(&pinned, args);
    invoke_suite(
        npx_argv,
        &args.timeout,
        &pinned,
        "client",
        &args.spec_version,
        args.json,
    )
    .await
}

/// Runs the suite against this crate's own fixtures.
///
/// `self` mode is the one the CI matrix exercises: it stands up the
/// reference server on a free port, waits for the port to answer, and
/// then runs the suite against it. The reference client fixture is a
/// follow-up (T21 phase 2) — until then, the client-side scenarios the
/// suite supports are exercised only by the operator against a
/// third-party client, never by `self`.
///
/// The server is started as a child process running the
/// `conformance_reference_server` example from the `protocol-droid`
/// crate. The example's path is resolved from `CARGO_BIN_EXE_<bin>` for
/// the binary that ships, and from the workspace's `target/` for a
/// dev build. The fixture's job is to be the simplest possible MCP
/// server that exercises the parts of the spec the suite reads; the
/// project's real `stdio_server` example is its own thing and is not
/// what `self` should exercise.
async fn run_self(args: &SelfArgs) -> anyhow::Result<()> {
    let pinned = read_pinned_version()?;
    let side = args.side.unwrap_or_else(|| {
        if args.scenario.is_some() {
            Side::Server
        } else {
            Side::Both
        }
    });
    let mut failures: Vec<String> = Vec::new();
    if side != Side::Client
        && let Err(error) = run_self_server(&pinned, args).await
    {
        failures.push(format!("server: {error:#}"));
    }
    if side != Side::Server
        && let Err(error) = run_self_client(&pinned, args).await
    {
        failures.push(format!("client: {error:#}"));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("{}", failures.join("; "))
    }
}

/// Runs the client half: the reference client against each scenario the
/// revision requires, one at a time so a failure names its scenario.
async fn run_self_client(pinned: &str, args: &SelfArgs) -> anyhow::Result<()> {
    let scenarios: Vec<&str> = args.scenario.as_ref().map_or_else(
        || crate::conformance_client::client_scenarios(&args.spec_version).to_vec(),
        |one| vec![one.as_str()],
    );
    if scenarios.is_empty() {
        eprintln!(
            "no client scenarios at {} for the pinned suite; nothing to run",
            args.spec_version
        );
        return Ok(());
    }
    let client = crate::conformance_client::build_reference_client().await?;
    let mut failed: Vec<&str> = Vec::new();
    for scenario in scenarios {
        let command =
            crate::conformance_client::client_argv(pinned, &client, &args.spec_version, scenario);
        if invoke_suite(
            command,
            &args.timeout,
            pinned,
            "self-client",
            &args.spec_version,
            args.json,
        )
        .await
        .is_err()
        {
            failed.push(scenario);
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("client scenarios failed: {}", failed.join(", "))
    }
}

/// Runs the server half: the reference server, against the suite's scenarios.
async fn run_self_server(pinned: &str, args: &SelfArgs) -> anyhow::Result<()> {
    let pinned = pinned.to_owned();
    let port = pick_port(args.port)?;
    let server = spawn_reference_server(port).await?;
    let url = format!("http://127.0.0.1:{port}/mcp");

    // Reap the server on every exit path. `kill_on_drop` on the child
    // means dropping `server` sends SIGKILL, but a clean SIGTERM first
    // gives the example a chance to flush stderr.
    let outcome = run_self_against(&pinned, &url, args).await;
    terminate_child(server).await;
    outcome
}

/// Runs the suite once against a server URL the caller already has.
///
/// Split from [`run_self`] so the test surface can exercise the suite
/// invocation without standing up a process — the only thing
/// `run_self` adds on top is the server lifecycle, and that is the
/// piece a unit test cannot reach.
async fn run_self_against(pinned: &str, url: &str, args: &SelfArgs) -> anyhow::Result<()> {
    let suite_args = ServerArgs {
        url: url.to_owned(),
        spec_version: args.spec_version.clone(),
        scenario: args.scenario.clone(),
        suite: args.suite.clone(),
        suite_args: Vec::new(),
        timeout: args.timeout.clone(),
        json: args.json,
    };
    let npx_argv = build_server_argv(pinned, &suite_args);
    invoke_suite(
        npx_argv,
        &args.timeout,
        pinned,
        "self",
        &args.spec_version,
        args.json,
    )
    .await
}

/// Picks a port for the fixture server: the operator's `--port` if it
/// was given and non-zero, otherwise a free port the OS hands out. A
/// free port is the right default for `self` because the wrapper
/// is the only thing that knows the port, and the suite needs the
/// URL after the server has bound.
fn pick_port(requested: u16) -> anyhow::Result<u16> {
    if requested != 0 {
        return Ok(requested);
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|error| anyhow::anyhow!("could not bind a free port: {error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| anyhow::anyhow!("local_addr on a fresh listener: {error}"))?
        .port();
    drop(listener);
    Ok(port)
}

/// Spawns the reference server fixture and waits for it to start
/// answering requests.
///
/// The fixture lives in the `protocol-droid` crate's `examples/`. The
/// binary path is resolved from `cargo run --example …` semantics: a
/// dev build runs it through `cargo`, a published binary looks for the
/// same example name in `$CARGO_BIN_EXE_<bin>`. Either way, the
/// fixture is the same example the conformance suite's
/// `tools-list` / `prompts-list` / `resources-list` scenarios were
/// validated against in T21 phase 1.
async fn spawn_reference_server(port: u16) -> anyhow::Result<tokio::process::Child> {
    use tokio::io::AsyncWriteExt;
    let mut command = build_reference_server_command(port);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|error| anyhow::anyhow!("could not spawn reference server: {error}"))?;

    // Wait up to 60 s for the port to accept an `initialize` request.
    // `cargo run` cold-compiles in CI, and a non-cold run still has to
    // bind a listener and start a tokio runtime; 60 s is a generous
    // bound without being a hang-detector false positive.
    match child.stderr.take() {
        Some(mut stderr) => {
            // Drain stderr in the background so a fixture that crashes
            // quickly is observable on the wrapper's stderr.
            let drain = tokio::spawn(async move {
                let mut buf = Vec::new();
                let _ = stderr.read_to_end(&mut buf).await;
                buf
            });
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while std::time::Instant::now() < deadline {
                if probe_server(port).await {
                    return Ok(child);
                }
                if let Ok(Some(_status)) = child.try_wait() {
                    let buf = drain.await.unwrap_or_default();
                    let _ = tokio::io::stderr().write_all(&buf).await;
                    return Err(anyhow::anyhow!(
                        "reference server exited before the port answered; see stderr above"
                    ));
                }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
            let buf = drain.await.unwrap_or_default();
            let _ = tokio::io::stderr().write_all(&buf).await;
            Err(anyhow::anyhow!(
                "reference server did not start answering within 60s; see stderr above"
            ))
        }
        None => Ok(child),
    }
}

/// Issues a single `initialize` POST against the fixture's port and
/// returns whether it answered with a 2xx. The body is the smallest
/// valid initialize; the fixture does not need to read it, only
/// confirm the listener is alive.
async fn probe_server(port: u16) -> bool {
    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .body(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"self-probe","version":"0"}}}"#,
        )
        .send()
        .await;
    matches!(response, Ok(r) if r.status().is_success())
}

/// Builds the command that runs the reference server fixture.
///
/// The `cargo run --example` form is what local dev and CI both
/// exercise. The example's `required-features = ["http", "random-ids"]`
/// means the wrapper has to forward those features, and forgetting one
/// is the most common build-then-fail path the operator hits.
fn build_reference_server_command(port: u16) -> tokio::process::Command {
    use tokio::process::Command;
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let mut command = Command::new(cargo);
    command
        .arg("run")
        .arg("--quiet")
        .arg("--manifest-path")
        .arg(workspace_manifest_path())
        .arg("--example")
        .arg("conformance_reference_server")
        .arg("-p")
        .arg("protocol-droid")
        .arg("--features")
        .arg("http,random-ids")
        .arg("--")
        .arg("--addr")
        .arg(format!("127.0.0.1:{port}"));
    command
}

/// Returns the path to the workspace `Cargo.toml`.
///
/// `CARGO_MANIFEST_DIR` is set at compile time to the directory of
/// this crate's manifest, and the workspace manifest sits one level
/// above it. Hard-coding that path is the same answer the
/// `cargo build --manifest-path` machinery gives, and it lets a
/// published binary reach the example from outside the workspace
/// without the operator passing `--manifest-path` themselves.
pub(crate) fn workspace_manifest_path() -> std::path::PathBuf {
    let mut cursor = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for _ in 0..2 {
        cursor = cursor.parent().unwrap_or_else(|| std::path::Path::new("."));
    }
    cursor.join("Cargo.toml")
}

/// Sends a SIGTERM to the fixture and waits for it to exit, with a
/// short deadline. SIGKILL is the fallback the `kill_on_drop` flag
/// already supplies — the SIGTERM-then-wait dance is what gives the
/// fixture a chance to flush its own stderr before it goes.
async fn terminate_child(mut child: tokio::process::Child) {
    let _ = child.start_kill();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), child.wait()).await;
}

/// Reads `conformance/PINNED_VERSION` and returns the trimmed contents.
///
/// The file is two lines, one of which is the version — the `#`-prefixed
/// comment that documents *why* the pin exists lives in `README.md`, not
/// here. A missing file is the pin never having been recorded, and the
/// wrapper refuses to make one up: a `npx @latest` invocation would
/// silently track the alpha channel, which is the failure mode the pin
/// prevents.
fn read_pinned_version() -> anyhow::Result<String> {
    let path = pinned_version_path();
    let raw = std::fs::read_to_string(&path).map_err(|error| {
        anyhow::anyhow!(
            "could not read {}: {error}. The pin file is the input to \
             `npx @modelcontextprotocol/conformance@<version>` — without it, \
             the wrapper would have to either pick a version (silent drift) \
             or fall back to @latest (silently tracks the alpha channel). \
             See crates/protocol-droid-cli/conformance/README.md.",
            path.display()
        )
    })?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow::anyhow!(
            "{} is empty — a blank pin is the same as no pin.",
            path.display()
        ));
    }
    Ok(trimmed.to_owned())
}

/// Resolves the pin file path relative to the crate's manifest dir.
///
/// `CARGO_MANIFEST_DIR` is set at compile time to the directory containing
/// this crate's `Cargo.toml`, so the path is stable across `cargo run`,
/// `cargo test`, and the published binary.
fn pinned_version_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("conformance")
        .join("PINNED_VERSION")
}

/// Builds the argv passed to `npx` for server-mode runs.
fn build_server_argv(pinned: &str, args: &ServerArgs) -> Vec<OsString> {
    let mut npx_argv: Vec<OsString> = Vec::with_capacity(8 + args.suite_args.len());
    npx_argv.push("--yes".into());
    npx_argv.push(format!("@modelcontextprotocol/conformance@{pinned}").into());
    npx_argv.push("server".into());
    npx_argv.push("--url".into());
    npx_argv.push(args.url.clone().into());
    npx_argv.push("--spec-version".into());
    npx_argv.push(args.spec_version.clone().into());
    if let Some(name) = &args.scenario {
        npx_argv.push("--scenario".into());
        npx_argv.push(name.clone().into());
    }
    if let Some(suite) = &args.suite {
        npx_argv.push("--suite".into());
        npx_argv.push(suite.clone().into());
    }
    for extra in &args.suite_args {
        npx_argv.push(extra.as_str().into());
    }
    npx_argv
}

/// Builds the argv passed to `npx` for client-mode runs.
fn build_client_argv(pinned: &str, args: &ClientArgs) -> Vec<OsString> {
    let mut npx_argv: Vec<OsString> = Vec::with_capacity(8 + args.suite_args.len());
    npx_argv.push("--yes".into());
    npx_argv.push(format!("@modelcontextprotocol/conformance@{pinned}").into());
    npx_argv.push("client".into());
    npx_argv.push("--command".into());
    npx_argv.push(args.command.clone().into());
    npx_argv.push("--spec-version".into());
    npx_argv.push(args.spec_version.clone().into());
    if let Some(name) = &args.scenario {
        npx_argv.push("--scenario".into());
        npx_argv.push(name.clone().into());
    }
    if let Some(suite) = &args.suite {
        npx_argv.push("--suite".into());
        npx_argv.push(suite.clone().into());
    }
    for extra in &args.suite_args {
        npx_argv.push(extra.as_str().into());
    }
    npx_argv
}

/// Spawns `npx` with the given argv, streams its output through, and reports.
///
/// # Exit-code handling
///
/// The wrapper **does not** translate the suite's exit code into its own
/// exit code. A passing suite returns `Ok(())` and a failing suite returns
/// `Err(anyhow!("suite exited with code {n}"))` — that is the
/// auditability choice the project's CLI rules demand: one serialiser,
/// one decision-tree, no second opinion about whether `1` means "the suite
/// said no" or "the wrapper said no".
///
/// CI post-processes the captured streams against
/// [`conformance/ALLOWLIST.md`] and treats allow-listed
/// `(spec-version, scenario)` failures as expected. A regression — a
/// previously-passing scenario now failing, or a new failing scenario not
/// on the list — still fails the run.
async fn invoke_suite(
    npx_argv: Vec<OsString>,
    budget: &str,
    pinned: &str,
    mode: &'static str,
    spec_version: &str,
    json: bool,
) -> anyhow::Result<()> {
    let duration = parse_duration(budget)
        .map_err(|error| anyhow::anyhow!("invalid --timeout `{budget}`: {error}"))?;
    let started = std::time::Instant::now();
    let (mut child, pump_out, pump_err) = spawn_suite_child(&npx_argv)?;

    let wait_outcome = timeout(duration, child.wait()).await;
    let status = match wait_outcome {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => return Err(anyhow::anyhow!("waiting on suite: {error}")),
        Err(_) => {
            // Timeout fired. `kill_on_drop(true)` will reap the child when
            // `child` is dropped at the end of this scope.
            let argv_for_report: Vec<String> = npx_argv
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect();
            write_json_report_if_requested(
                json,
                &Report {
                    pinned_version: pinned.to_owned(),
                    spec_revision: spec_version.to_owned(),
                    mode,
                    argv: argv_for_report,
                    suite_exit_code: -1,
                    elapsed: human_duration(started.elapsed()),
                    outcome: "timeout",
                },
            )?;
            return Err(anyhow::anyhow!(
                "suite did not finish within {budget}; child reaped via kill_on_drop"
            ));
        }
    };

    // Drain the pump tasks now that the child is done.
    let out_buf = pump_out.await.unwrap_or_default();
    let err_buf = pump_err.await.unwrap_or_default();

    let argv_strings: Vec<String> = npx_argv
        .iter()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    let exit_code = status.code().unwrap_or(-1);
    let outcome = if status.success() { "pass" } else { "fail" };

    if json {
        write_json_report_if_requested(
            json,
            &Report {
                pinned_version: pinned.to_owned(),
                spec_revision: spec_version.to_owned(),
                mode,
                argv: argv_strings.clone(),
                suite_exit_code: exit_code,
                elapsed: human_duration(started.elapsed()),
                outcome,
            },
        )?;
    }

    // Stream the suite's output. With `--json`, the JSON line is already
    // on stdout, so the suite's own output goes to stderr to keep the two
    // serialisers separate — a log scraper that wants the JSON shape and a
    // human that wants the suite's progress are different sinks, and the
    // `--json` flag is the presence/absence of the human sink rather than
    // a second code path.
    if json {
        let mut stderr_sink = tokio::io::stderr();
        let _ = stderr_sink.write_all(&out_buf).await;
        let _ = stderr_sink.write_all(&err_buf).await;
    } else {
        let _ = tokio::io::stdout().write_all(&out_buf).await;
        let _ = tokio::io::stderr().write_all(&err_buf).await;
    }

    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "suite exited with code {exit_code} (mode={mode}, spec={spec_version}). \
             argv was: [{}]. See crates/protocol-droid-cli/conformance/ALLOWLIST.md \
             if this is a known-suite bug.",
            argv_strings.join(", ")
        ))
    }
}

/// Spawns the suite process and arranges for its stdout and stderr to be
/// drained into owned buffers by two background tasks.
///
/// The returned tuple is the child plus the two `JoinHandle`s. Each task
/// closes its drain when the corresponding pipe closes (i.e. when the
/// child exits), and `invoke_suite` awaits them to recover the captured
/// output.
type SuiteHandles = (
    tokio::process::Child,
    tokio::task::JoinHandle<Vec<u8>>,
    tokio::task::JoinHandle<Vec<u8>>,
);

fn spawn_suite_child(npx_argv: &[OsString]) -> anyhow::Result<SuiteHandles> {
    let mut child = Command::new("npx")
        .args(npx_argv)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| {
            anyhow::anyhow!("could not spawn `npx` (is Node installed and on PATH?): {error}")
        })?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("suite stdout was not piped"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("suite stderr was not piped"))?;

    let mut out_buf = Vec::new();
    let mut err_buf = Vec::new();
    let pump_out = tokio::spawn(async move {
        let _ = stdout.read_to_end(&mut out_buf).await;
        out_buf
    });
    let pump_err = tokio::spawn(async move {
        let _ = stderr.read_to_end(&mut err_buf).await;
        err_buf
    });
    Ok((child, pump_out, pump_err))
}

/// Serialises the report when `--json` was passed.
///
/// Centralising the `serde_json` call keeps the JSON shape in one place —
/// every report variant goes through the same serialiser, and a future
/// change to the report's fields only edits this function.
fn write_json_report_if_requested(json: bool, report: &Report) -> anyhow::Result<()> {
    if !json {
        return Ok(());
    }
    let serialized = serde_json::to_string_pretty(report).map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("{serialized}");
    Ok(())
}

/// Parses a duration of the form `<n>(s|m|h)`.
///
/// Three units is enough for the budget a CI leg actually wants — a CI
/// matrix leg is either "ten seconds for one scenario" or "ten minutes for
/// the full suite", not the multi-day planning duration `humantime` would
/// add by accepting weeks.
fn parse_duration(input: &str) -> Result<Duration, String> {
    let (number, unit) = input.split_at(
        input
            .find(|c: char| !c.is_ascii_digit())
            .ok_or_else(|| "missing unit; expected one of `s`, `m`, `h`".to_string())?,
    );
    let n: u64 = number
        .parse()
        .map_err(|e| format!("could not parse `{number}`: {e}"))?;
    let multiplier = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        other => return Err(format!("unknown unit `{other}`")),
    };
    Ok(Duration::from_secs(n * multiplier))
}

/// Formats a duration as `Xm Ys` for the JSON report's elapsed field.
fn human_duration(d: std::time::Duration) -> String {
    let total = d.as_secs();
    let minutes = total / 60;
    let seconds = total % 60;
    format!("{minutes}m {seconds:02}s")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_version_path_points_at_conformance_dir() {
        let path = pinned_version_path();
        assert!(
            path.ends_with("conformance/PINNED_VERSION"),
            "path was {}",
            path.display()
        );
    }

    /// `run_self` is wired end-to-end now: it spawns the reference server
    /// on a free port, runs the suite against it, and surfaces the
    /// suite's exit code. Exercising the happy path with a scenario the
    /// suite supports (`server-initialize` is the smallest one) keeps
    /// the function and the `SelfArgs` it consumes alive in the lib
    /// test build without paying for a real fixture compile.
    ///
    /// `#[ignore]` because the test spawns `npx` to download and run the
    /// pinned conformance suite; in CI's `--no-default-features` matrix
    /// and on a developer machine without network, the npx call hangs
    /// forever. The dedicated `Conformance` workflow exercises the same
    /// code path with a guaranteed-working npx and a 5m timeout, so
    /// the coverage lives there rather than in the default test run.
    #[tokio::test]
    #[ignore = "spawns npx and the suite; covered by the Conformance workflow, not the default test run"]
    async fn run_self_executes_the_suite_end_to_end() {
        let args = SelfArgs {
            spec_version: "2025-06-18".to_owned(),
            port: 0,
            scenario: Some("server-initialize".to_owned()),
            suite: None,
            side: None,
            timeout: "60s".to_owned(),
            json: false,
        };
        // The suite may report its own failures — that is the run's
        // answer, not a wrapper bug. A passing test here is "we got an
        // outcome, not a hang or a panic"; the post-processed suite
        // output is what CI's allow-list applies to.
        let _ = run_self(&args).await;
    }

    /// `read_pinned_version` is the surface every other function reads
    /// through. The pin file is committed to this crate, so a real read is
    /// the only check that keeps the path resolution honest.
    #[test]
    fn read_pinned_version_reads_a_non_empty_string() -> Result<(), String> {
        let pinned = read_pinned_version().map_err(|e| format!("{e:#}"))?;
        if pinned.trim().is_empty() {
            return Err(format!("PINNED_VERSION must not be empty, got {pinned:?}"));
        }
        if !pinned.chars().all(|c| !c.is_whitespace()) {
            return Err(format!(
                "PINNED_VERSION must not contain whitespace, got {pinned:?}"
            ));
        }
        Ok(())
    }

    /// `run` is the entry point `main.rs` dispatches to. Calling it with a
    /// `self` mode runs the full end-to-end pipeline; we exercise the
    /// dispatch and the lifecycle (spawn the server, run the suite,
    /// reap the server) without asserting on a pass/fail — the suite's
    /// own pass/fail is the run's answer, and CI post-processes that
    /// against the allow-list.
    ///
    /// `#[ignore]` for the same reason as
    /// `run_self_executes_the_suite_end_to_end` above: the test spawns
    /// `npx` and the suite, and a default test run should not pay for
    /// the network. The Conformance workflow runs the same path with
    /// a timeout.
    #[tokio::test]
    #[ignore = "spawns npx and the suite; covered by the Conformance workflow, not the default test run"]
    async fn run_dispatches_to_run_self() {
        let args = ConformanceArgs {
            mode: ConformanceMode::Self_(SelfArgs {
                spec_version: "2025-06-18".to_owned(),
                port: 0,
                scenario: Some("server-initialize".to_owned()),
                suite: None,
                side: None,
                timeout: "60s".to_owned(),
                json: false,
            }),
        };
        let _ = run(&args).await;
    }

    /// `SelfArgs` is consumed by `run_self`. Building one in a test keeps
    /// the struct alive in the lib test target — the bin target wires
    /// `SelfArgs` into `main.rs`, but the lib test build does not see
    /// `main.rs` at all.
    #[test]
    fn self_args_can_be_constructed() {
        let _args = SelfArgs {
            spec_version: "2024-11-05".to_owned(),
            port: 8080,
            scenario: Some("tools_call".to_owned()),
            suite: Some("active".to_owned()),
            side: None,
            timeout: "5m".to_owned(),
            json: true,
        };
    }

    /// The same argument holds for `ConformanceArgs` and `ConformanceMode`:
    /// the lib test target needs to see them used. Pattern-matching on the
    /// dispatched variant is the cheapest way to keep all three alive.
    #[test]
    fn conformance_args_and_mode_are_used() -> Result<(), String> {
        let server = ConformanceArgs {
            mode: ConformanceMode::Server(ServerArgs {
                url: "http://127.0.0.1:1/mcp".to_owned(),
                spec_version: "2024-11-05".to_owned(),
                scenario: None,
                suite: None,
                suite_args: Vec::new(),
                timeout: "10m".to_owned(),
                json: false,
            }),
        };
        match server.mode {
            ConformanceMode::Server(_) => Ok(()),
            ConformanceMode::Client(_) => Err("expected Server, got Client".to_string()),
            ConformanceMode::Self_(_) => Err("expected Server, got Self".to_string()),
        }
    }

    #[test]
    fn parse_duration_accepts_each_unit() -> Result<(), String> {
        assert_eq!(parse_duration("5s")?, Duration::from_secs(5));
        assert_eq!(parse_duration("7m")?, Duration::from_mins(7));
        assert_eq!(parse_duration("1h")?, Duration::from_hours(1));
        Ok(())
    }

    #[test]
    fn parse_duration_rejects_missing_unit() {
        assert!(parse_duration("5").is_err());
    }

    #[test]
    fn parse_duration_rejects_unknown_unit() {
        assert!(parse_duration("5d").is_err());
    }

    #[test]
    fn human_duration_renders_minutes_and_seconds() {
        assert_eq!(human_duration(Duration::from_secs(75)), "1m 15s");
        assert_eq!(human_duration(Duration::from_secs(7)), "0m 07s");
    }

    #[test]
    fn build_server_argv_includes_pinned_version_and_url() {
        let pinned = "0.2.0-alpha.12";
        let args = ServerArgs {
            url: "http://127.0.0.1:8080/mcp".to_owned(),
            spec_version: "2024-11-05".to_owned(),
            scenario: None,
            suite: None,
            suite_args: Vec::new(),
            timeout: "10m".to_owned(),
            json: false,
        };
        let npx_argv = build_server_argv(pinned, &args);
        let as_strings: Vec<String> = npx_argv
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert!(as_strings.contains(&"--url".to_owned()));
        assert!(as_strings.contains(&"http://127.0.0.1:8080/mcp".to_owned()));
        assert!(
            as_strings
                .iter()
                .any(|s| s.contains("@modelcontextprotocol/conformance@0.2.0-alpha.12"))
        );
        assert!(as_strings.contains(&"server".to_owned()));
        assert!(as_strings.contains(&"--spec-version".to_owned()));
        assert!(as_strings.contains(&"2024-11-05".to_owned()));
    }

    #[test]
    fn build_server_argv_omits_optional_flags_when_unset() {
        let args = ServerArgs {
            url: "http://127.0.0.1:1/mcp".to_owned(),
            spec_version: "2025-06-18".to_owned(),
            scenario: None,
            suite: None,
            suite_args: Vec::new(),
            timeout: "10m".to_owned(),
            json: false,
        };
        let npx_argv = build_server_argv("0.2.0-alpha.12", &args);
        let as_strings: Vec<String> = npx_argv
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert!(
            !as_strings.contains(&"--scenario".to_owned()),
            "argv was {as_strings:?}"
        );
        assert!(
            !as_strings.contains(&"--suite".to_owned()),
            "argv was {as_strings:?}"
        );
    }

    #[test]
    fn build_server_argv_appends_scenario_when_set() {
        let args = ServerArgs {
            url: "http://127.0.0.1:1/mcp".to_owned(),
            spec_version: "2025-06-18".to_owned(),
            scenario: Some("tools".to_owned()),
            suite: Some("all".to_owned()),
            suite_args: vec!["--foo".to_owned(), "bar".to_owned()],
            timeout: "10m".to_owned(),
            json: false,
        };
        let npx_argv = build_server_argv("0.2.0-alpha.12", &args);
        let as_strings: Vec<String> = npx_argv
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert!(as_strings.contains(&"--scenario".to_owned()));
        assert!(as_strings.contains(&"tools".to_owned()));
        assert!(as_strings.contains(&"--suite".to_owned()));
        assert!(as_strings.contains(&"all".to_owned()));
        assert!(as_strings.contains(&"--foo".to_owned()));
        assert!(as_strings.contains(&"bar".to_owned()));
    }

    #[test]
    fn build_client_argv_uses_command_flag() {
        let args = ClientArgs {
            command: "./my-mcp-client --quiet".to_owned(),
            spec_version: "2024-11-05".to_owned(),
            scenario: None,
            suite: None,
            suite_args: Vec::new(),
            timeout: "10m".to_owned(),
            json: false,
        };
        let npx_argv = build_client_argv("0.2.0-alpha.12", &args);
        let as_strings: Vec<String> = npx_argv
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert!(as_strings.contains(&"client".to_owned()));
        assert!(as_strings.contains(&"--command".to_owned()));
        assert!(as_strings.contains(&"./my-mcp-client --quiet".to_owned()));
    }
}
