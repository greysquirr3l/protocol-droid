//! `protocol-droid bridge` — the version-translating proxy.
//!
//! ## Scope of this increment
//!
//! This command currently **validates and reports**; it does not yet serve.
//! Both transport specs are parsed into [`TransportSpec`], the pair is checked
//! against what this build actually implements, and the result is printed. The
//! serving loop lands with the `BridgePeer` adapters, and this command grows
//! into it rather than being replaced.
//!
//! That split is deliberate rather than a stub. The parsing, the error
//! messages and the reporting are the parts an agent needs to *check a
//! configuration before relying on it*, and they are complete. What is missing
//! is the socket, and a command that pretended to have one would fail in a way
//! its own exit code did not predict.
//!
//! ## Two sinks, one serialiser
//!
//! `--json` is the presence or absence of the stderr writer, never a second
//! code path. [`Plan`] is serialised once; the human form is a `Display` over
//! the same value. A `--json` flag that built a different string would drift
//! from the human form the first time one of them changed.
//!
//! ## Failing loudly rather than serving blind
//!
//! If either side names a transport this build cannot serve, the command exits
//! non-zero and says so. A bridge that started and then failed on the first
//! message would look, from the client's side, like a server that had hung.
//!
//! That check used to refuse `stdio` on the upstream side, on the stated
//! grounds that stdio had no *client* adapter. It does — `StdioClient` landed in
//! T13 and `StdioPeer` wraps it — so the check was refusing a pairing that
//! works. It is kept, because the grammar still accepts transports a future
//! build might not implement, but it now refuses nothing that exists.

use anyhow::Context as _;
use clap::Args;
#[cfg(feature = "bridge")]
use protocol_droid::domain::SupportedVersions;
use protocol_droid::domain::{
    DegradePolicy, ProtocolVersion, SpecError, TransportKind, TransportSpec,
};
use serde::Serialize;

use crate::check;

/// What a bridge command was asked to do.
#[derive(Debug, Args)]
pub struct BridgeArgs {
    /// Where clients connect, as `kind:target` — `stdio:./bridge`.
    ///
    /// The target is **unused** for `stdio`. The grammar requires one because a
    /// stdio *upstream* needs a program to run, and `FromStr` cannot know which
    /// role a spec is playing. A bridge listening on stdio is talking to its own
    /// stdin and stdout, so there is nothing to name; pass anything.
    #[arg(long, value_name = "SPEC")]
    pub listen: String,

    /// Where the bridge forwards to, same grammar.
    ///
    /// Optional when `--peer` is set: the A2A bridge has no
    /// MCP upstream to name, and requiring it would force a
    /// `kind:target` the A2A path never reads.
    #[arg(long, value_name = "SPEC")]
    pub upstream: Option<String>,

    /// Run the bridge against an A2A agent instead of an MCP upstream.
    ///
    /// Mutually exclusive with `--upstream`: an A2A agent is a
    /// JSON-RPC peer at an HTTP URL, not a typed `TransportSpec`,
    /// and the cross-protocol bridge builds the codec + peer
    /// itself. The bridge listens on `--listen` and translates
    /// each `tools/call` from the MCP downstream into the agent's
    /// `SendMessage`, with `bridge_skill` carrying the skill
    /// name through A2A `metadata`.
    #[cfg(feature = "a2a")]
    #[arg(long, value_name = "URL", conflicts_with = "upstream")]
    pub peer: Option<String>,

    /// Report what the bridge would do with a reply, and exit.
    ///
    /// Reads a JSON-RPC reply envelope from this path (`-` for stdin) and
    /// answers one question: forwarded untouched, downgraded, or refused. No
    /// socket is opened and no peer is contacted, so this is safe to run in a
    /// check that never wants to start a server.
    #[arg(long, value_name = "FILE")]
    pub check_result: Option<String>,

    /// The revision the downstream client negotiated.
    ///
    /// Required with `--check-result`, and meaningless without it: a probe has
    /// to know who is reading the reply to know what it can read.
    #[arg(long, value_name = "VERSION", requires = "check_result")]
    pub client_version: Option<String>,

    /// The revision the upstream server speaks.
    #[arg(long, value_name = "VERSION", requires = "check_result")]
    pub server_version: Option<String>,

    /// Authorise downgrades instead of refusing them.
    ///
    /// Off by default, because a downgrade is a loss and the operator is the
    /// one who should be choosing to take it. Some losses have no rule at all
    /// and stay refused regardless — `--allow-downgrade` cannot authorise a
    /// fallback the specification does not define.
    #[arg(long, requires = "check_result")]
    pub allow_downgrade: bool,

    /// Machine-readable JSON on stdout, with the human summary on stderr.
    #[arg(long)]
    pub json: bool,
}

/// The two ends of a bridge, as parsed and checked.
///
/// A `serde::Serialize` so the JSON and human forms are two renderings of one
/// value. The JSON goes to stdout for an agent; the human line goes to stderr
/// for a person watching the same command.
#[derive(Debug, Serialize)]
struct Plan {
    listen: Endpoint,
    upstream: Endpoint,
    serve: bool,
    note: String,
}

/// One end, as it will actually be used.
#[derive(Debug, Serialize)]
struct Endpoint {
    transport: &'static str,
    target: String,
    /// Whether this build has the adapter for it.
    implemented: bool,
    /// The spec as the user wrote it, for the human line.
    ///
    /// Rebuilt from `transport` and `target` this would read
    /// `http:http://host/mcp` for a URL target, because the target already
    /// carries its scheme. [`TransportSpec`] already knows how to print itself
    /// without doubling it, so this stores that rather than a second opinion.
    spelling: String,
}

/// Why a spec could not be used, in the shape `anyhow` prints.
#[derive(Debug, thiserror::Error)]
#[error("{field} is not usable: {source}")]
struct SpecFailure {
    /// Which argument was wrong.
    field: &'static str,
    /// What was wrong with it.
    #[source]
    source: SpecError,
}

/// Runs `protocol-droid bridge`.
///
/// # Errors
///
/// [`SpecFailure`] when either transport spec cannot be parsed, and a plain
/// `anyhow` error when the pair is well-formed but not servable by this build.
/// Both name the argument or the transport at fault, so the caller knows which
/// one to change.
pub async fn run(args: &BridgeArgs) -> anyhow::Result<()> {
    if let Some(path) = args.check_result.as_deref() {
        return probe(args, path);
    }

    #[cfg(feature = "a2a")]
    if let Some(peer) = args.peer.as_deref() {
        return crate::bridge_a2a::run(crate::bridge_a2a::A2aBridgeArgs {
            peer: peer.to_owned(),
            json: args.json,
        })
        .await;
    }

    let listen = parse(&args.listen, "--listen")?;
    let Some(upstream_text) = args.upstream.as_deref() else {
        anyhow::bail!("--upstream is required when --peer is not set");
    };
    let upstream = parse(upstream_text, "--upstream")?;

    let plan = plan(&listen, &upstream);
    if args.json {
        println!("{}", serde_json::to_string_pretty(&plan)?);
    } else {
        eprintln!("{plan}");
    }

    if !plan.serve {
        anyhow::bail!("{}", plan.note);
    }

    // A build without the `bridge` feature validated the pair and then has
    // nowhere to send it. Saying so beats exiting zero having printed a plan
    // that implies a bridge is now running.
    #[cfg(feature = "bridge")]
    {
        serve(&listen, &upstream, args).await
    }
    #[cfg(not(feature = "bridge"))]
    {
        anyhow::bail!(
            "this build validated the pair but has no serving path; rebuild with --features bridge"
        )
    }
}

/// Opens the upstream and runs the bridge until the downstream stream ends.
///
/// Split out so the only thing behind the `bridge` feature gate is the part
/// that needs a transport. The probe above stays reachable in a lean build.
///
/// # Errors
///
/// Whatever the upstream raised opening or negotiating, and
/// [`LoopError`](protocol_droid::application::bridge::LoopError) for a
/// downstream pipe that fails mid-run.
#[cfg(feature = "bridge")]
async fn serve(
    listen: &TransportSpec,
    upstream: &TransportSpec,
    args: &BridgeArgs,
) -> anyhow::Result<()> {
    let policy = if args.allow_downgrade {
        DegradePolicy::permissive()
    } else {
        DegradePolicy::default()
    };

    // The listen side decides which shape this is, and an `http` listen is a
    // different job from a `stdio` one: a server outlives its clients, so it
    // opens an upstream per session rather than one for the process.
    if listen.kind() == TransportKind::StreamableHttp {
        let addr = crate::serve::bind_addr(listen.target())?;
        crate::serve::listen(upstream, addr, &SupportedVersions::all()?, policy).await
    } else {
        serve_one_client(upstream, args, policy).await
    }
}

/// Runs the single-client `stdio` bridge, and reports what it did.
///
/// Split from the caller so the `http` arm above is the only thing holding a
/// `SocketAddr`, and this stays the path that can return a `LoopReport`.
#[cfg(feature = "bridge")]
async fn serve_one_client(
    upstream: &TransportSpec,
    args: &BridgeArgs,
    policy: DegradePolicy,
) -> anyhow::Result<()> {
    let report = crate::serve::run(upstream, &SupportedVersions::all()?, policy).await?;
    let summary = Summary {
        handshaken: report.handshaken,
        forwarded: report.forwarded,
        notified: report.notified,
        failed: report.failed,
    };

    // Two sinks, one value: the JSON an agent reads and the line a person
    // watches are the same serialisation, so they cannot drift.
    if args.json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        eprintln!("{summary}");
    }
    Ok(())
}

#[cfg(feature = "bridge")]
/// What a finished run moved.
///
/// The counters are the loop's own, named. "Forwarded nothing and failed
/// nothing" is otherwise indistinguishable from "no client ever connected",
/// which is the one thing an operator watching a bridge most needs told.
#[derive(Debug, Serialize)]
struct Summary {
    /// Whether a client completed a handshake.
    handshaken: bool,
    /// Envelopes forwarded to the upstream peer.
    forwarded: u64,
    /// Notifications forwarded upstream.
    notified: u64,
    /// Frames refused, each answered to the client.
    failed: u64,
}

#[cfg(feature = "bridge")]
impl std::fmt::Display for Summary {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let client = if self.handshaken {
            "a client connected"
        } else {
            "no client ever connected"
        };
        write!(
            formatter,
            "{client}; forwarded {}, notified {}, failed {}",
            self.forwarded, self.notified, self.failed
        )
    }
}

/// Runs `--check-result`, and fails the command if the bridge would refuse.
///
/// The transport check stays out of this path on purpose: the probe asks what
/// the bridge would do with a reply, and refusing to open a socket is not part
/// of that answer. It also means the probe works against pairings this build
/// cannot actually serve, which is when an operator most wants to know.
fn probe(args: &BridgeArgs, path: &str) -> anyhow::Result<()> {
    let client = version(args.client_version.as_deref(), "--client-version")?;
    let server = version(args.server_version.as_deref(), "--server-version")?;
    let policy = if args.allow_downgrade {
        DegradePolicy::permissive()
    } else {
        DegradePolicy::strict()
    };

    let check = check::run(path, client, server, policy, args.json)?;
    if check::refused(check.outcome) {
        anyhow::bail!("{}", check.note);
    }
    Ok(())
}

/// Parses a `--client-version` / `--server-version`, naming the argument.
fn version(text: Option<&str>, field: &'static str) -> anyhow::Result<ProtocolVersion> {
    let text = text.with_context(|| format!("{field} is required alongside --check-result"))?;
    text.parse::<ProtocolVersion>()
        .with_context(|| format!("{field} is not a revision this build knows: {text}"))
}

/// Parses one spec, naming the argument on the way out.
///
/// `TransportSpec::from_str` already returns a domain error carrying the
/// spelling it accepted; this only attaches *which* argument was at fault,
/// which the parser cannot know and the caller needs.
fn parse(spec: &str, field: &'static str) -> Result<TransportSpec, SpecFailure> {
    spec.parse().map_err(|source| SpecFailure { field, source })
}

/// Whether this build can serve a transport on the *upstream* side.
///
/// An upstream is the side the bridge *calls*, so it needs a `BridgePeer`
/// adapter that this build can also open a session with. `http` and
/// `legacy-sse` have both. `stdio` has a client adapter but no way to *name* a
/// program to run — `--upstream` carries `kind:target`, which has no room for a
/// command line — and `stateless-http` has no client adapter at all.
///
/// This check once read `kind != TransportKind::Stdio`, refusing stdio on the
/// grounds that it had a server adapter but no client one. That was true once
/// and stopped being true twice, but flipping it to "everything is supported"
/// was the wrong repair: it makes the plan promise a pairing that
/// [`serve::connect`] then refuses, and a plan that says yes and fails is worse
/// than a plan that says no.
///
/// The rule this follows: a predicate answers what the code below it can
/// actually do. It is not there to be reassuring.
const fn upstream_supported(kind: TransportKind) -> bool {
    matches!(
        kind,
        TransportKind::StreamableHttp | TransportKind::LegacySse
    )
}

/// Whether this build can back the bridge's *listen* side with a frame source.
///
/// Only `stdio`, and the reason is narrower than "only stdio has a server
/// adapter" — every transport has one of those. It is that a bridge's loop pulls
/// frames from a [`FrameSource`](protocol_droid::application::jsonrpc::FrameSource),
/// and `StdioSource` is the only thing in the crate that implements it. The
/// HTTP router is request-and-response shaped: a POST arrives, a response goes
/// back, and the long-lived part is an SSE stream the router itself owns. There
/// is no pull source behind it, and building one is a different piece of work
/// from having a server.
///
/// Checking the upstream and not the listen side would let an `http` listen
/// pass validation and then fail on the first frame.
const fn listen_supported(kind: TransportKind) -> bool {
    matches!(kind, TransportKind::Stdio | TransportKind::StreamableHttp)
}

fn plan(listen: &TransportSpec, upstream: &TransportSpec) -> Plan {
    let listen_ok = listen_supported(listen.kind());
    let upstream_ok = upstream_supported(upstream.kind());
    let note = if !listen_ok {
        format!(
            "a bridge needs a frame source to accept {}; this build pulls frames from stdio only",
            listen.target()
        )
    } else if !upstream_ok {
        format!(
            "this build cannot forward to {}; pick a transport with a client adapter",
            upstream.target()
        )
    } else {
        "validated; stdio listen and every client transport have the adapters a loop needs"
            .to_owned()
    };

    Plan {
        listen: endpoint(listen, listen_ok),
        upstream: endpoint(upstream, upstream_ok),
        serve: listen_ok && upstream_ok,
        note,
    }
}

fn endpoint(spec: &TransportSpec, implemented: bool) -> Endpoint {
    Endpoint {
        transport: spec.kind().as_str(),
        target: spec.target().to_owned(),
        implemented,
        spelling: spec.to_string(),
    }
}

impl std::fmt::Display for Plan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "listen {}  <-  upstream {}",
            self.listen.spelling, self.upstream.spelling
        )?;
        if !self.serve {
            write!(formatter, "\nnot servable: {}", self.note)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use protocol_droid::domain::TransportSpec;

    use super::{BridgeArgs, Plan, parse, plan, run};

    type TestError = Box<dyn std::error::Error>;

    fn args(listen: &str, upstream: &str) -> BridgeArgs {
        BridgeArgs {
            listen: listen.to_owned(),
            upstream: Some(upstream.to_owned()),
            #[cfg(feature = "a2a")]
            peer: None,
            check_result: None,
            client_version: None,
            server_version: None,
            allow_downgrade: false,
            json: false,
        }
    }

    /// Parses a literal spec, propagating a failure rather than asserting.
    ///
    /// These tests exercise `plan`, which takes specs the CLI already parsed. A
    /// literal that stopped parsing would be a bug in the grammar, and this
    /// reports it as one instead of hiding it behind a panic.
    fn spec(literal: &str) -> Result<TransportSpec, TestError> {
        Ok(TransportSpec::from_str(literal)?)
    }

    /// Both specs parse and the plan reports both ends.
    #[test]
    fn a_well_formed_pair_produces_a_plan() -> Result<(), TestError> {
        let listen = parse("stdio:./bridge", "--listen")?;
        let upstream = parse("http://backend:3000/mcp", "--upstream")?;
        let plan = plan(&listen, &upstream);

        assert!(plan.serve);
        assert_eq!(plan.listen.transport, "stdio");
        // The scheme is part of the target. It used to be stripped here and in
        // `TransportSpec`, so a plan reported `//backend:3000/mcp` — a string
        // no request can be built against, and the reason `--upstream
        // http://…` failed at connect rather than at parse.
        assert_eq!(plan.upstream.target, "http://backend:3000/mcp");
        assert_eq!(plan.upstream.spelling, "http://backend:3000/mcp");
        assert!(plan.listen.implemented && plan.upstream.implemented);
        Ok(())
    }

    /// A `stdio` upstream is served, because there is a `StdioPeer` for it.
    ///
    /// This assertion once read `assert!(plan.serve)` and pinned the claim that
    /// stdio had no client adapter. That claim stopped being true when
    /// `StdioClient` shipped in T13, so the test flipped — and then the bridge
    /// grew a serving path, which exposed that the *adapter* was never the
    /// blocker.
    ///
    /// It is the grammar. `--upstream` carries `kind:target`, and there is no
    /// room in that for a program and its arguments, so a stdio upstream has
    /// nothing to spawn. Supporting one means changing the argument shape, not
    /// relaxing this check.
    #[test]
    fn a_stdio_upstream_is_refused_for_its_grammar() -> Result<(), TestError> {
        let listen = parse("stdio:./bridge", "--listen")?;
        let upstream = parse("stdio:./server", "--upstream")?;
        let plan = plan(&listen, &upstream);

        assert!(!plan.serve, "a stdio upstream has no program to spawn");
        assert!(
            plan.note.contains("client adapter"),
            "the note blames the wrong thing: {}",
            plan.note
        );
        Ok(())
    }

    /// The reverse is fine: a bridge listening on stdio forwards to http.
    /// Listening is the server role, and that adapter exists.
    #[test]
    fn a_stdio_listener_is_allowed() -> Result<(), TestError> {
        let listen = parse("stdio:./server", "--listen")?;
        let upstream = parse("http://backend:3000/mcp", "--upstream")?;

        assert!(plan(&listen, &upstream).serve);
        Ok(())
    }

    /// A bad spec names the argument, not just the text.
    #[test]
    fn a_bad_spec_names_the_argument_it_broke() {
        let Err(failure) = parse("carrier-pigeon", "--upstream") else {
            return;
        };
        assert_eq!(failure.field, "--upstream");
    }

    /// The same malformed text on the other argument names *that* one.
    #[test]
    fn the_other_argument_names_itself() {
        let Err(failure) = parse("", "--listen") else {
            return;
        };
        assert_eq!(failure.field, "--listen");
    }

    /// A pair the bridge can actually run passes validation.
    ///
    /// `stdio` listen, because `StdioSource` is the only pull frame source in
    /// the crate, and an `http` upstream, because `HttpClient` has a peer.
    ///
    /// This asserts on the plan and not on `run`, and the split is deliberate:
    /// `run` now opens the upstream, so a test through it would dial a real
    /// address to check a pure predicate. `serve` has its own tests; this one
    /// answers the question the predicate answers.
    #[test]
    fn a_servable_pair_is_servable() -> Result<(), TestError> {
        assert!(plan(&spec("stdio:./bridge")?, &spec("http://backend:3000/mcp")?).serve);
        Ok(())
    }

    /// An upstream with no client adapter is refused before any socket opens.
    ///
    /// The refusal has to happen here rather than in `serve::connect`: a plan
    /// that says yes and then fails is worse than a plan that says no, because
    /// the operator has already started reading the run's output.
    #[test]
    fn an_unsupported_upstream_is_not_servable() -> Result<(), TestError> {
        assert!(
            !plan(
                &spec("stdio:./bridge")?,
                &spec("http-stateless://backend:3000/mcp")?
            )
            .serve
        );
        Ok(())
    }

    /// An `http` listen is servable, and for a reason worth pinning.
    ///
    /// This used to be the other way round, and the test that asserted it was
    /// right when written: there was no frame source behind the router. There
    /// is one now, which is the whole point of the listen side.
    #[test]
    fn an_http_listen_is_servable() -> Result<(), TestError> {
        let listen = parse("http://127.0.0.1:8080/mcp", "--listen")?;
        let upstream = parse("http://backend:3000/mcp", "--upstream")?;
        let plan = plan(&listen, &upstream);

        assert!(
            plan.serve,
            "an http listen was refused: {note}",
            note = plan.note
        );
        Ok(())
    }

    /// The same reason, from the outside: the command exits non-zero rather
    /// than printing a plan and claiming success.
    #[tokio::test]
    async fn an_unservable_pair_exits_non_zero() {
        assert!(
            run(&args("http://127.0.0.1:1/mcp", "http://backend:3000/mcp"))
                .await
                .is_err()
        );
    }

    /// A bad spec fails before anything is printed, so an agent parsing stdout
    /// never sees a partial plan.
    #[tokio::test]
    async fn a_bad_spec_fails_before_reporting() {
        assert!(
            run(&args("nonsense", "http://backend:3000/mcp"))
                .await
                .is_err()
        );
    }

    /// `Display` is the human rendering of the same value `--json` serialises,
    /// so the two cannot describe different things.
    #[test]
    fn the_human_form_shows_both_ends() -> Result<(), TestError> {
        let plan = plan(
            &TransportSpec::from_str("http://127.0.0.1:8080/mcp")?,
            &TransportSpec::from_str("http://backend:3000/mcp")?,
        );
        let rendered = plan.to_string();

        assert!(rendered.contains("127.0.0.1:8080"), "{rendered}");
        assert!(rendered.contains("backend:3000"), "{rendered}");
        Ok(())
    }

    /// The same value, serialised, carries the same two ends.
    #[test]
    fn the_json_form_carries_both_ends() -> Result<(), TestError> {
        let plan = plan(
            &TransportSpec::from_str("http://127.0.0.1:8080/mcp")?,
            &TransportSpec::from_str("http://backend:3000/mcp")?,
        );
        let rendered = serde_json::to_string(&plan)?;

        assert!(rendered.contains("127.0.0.1:8080"), "{rendered}");
        assert!(rendered.contains("backend:3000"), "{rendered}");
        Ok(())
    }

    fn _plan_is_used(_: &Plan) -> bool {
        true
    }
}
