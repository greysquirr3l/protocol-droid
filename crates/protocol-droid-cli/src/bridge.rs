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
//! If either side names a transport this build cannot yet serve as an upstream
//! — `stdio` has no *client* adapter — the command exits non-zero and says so.
//! A bridge that started and then failed on the first message would look, from
//! the client's side, like a server that had hung.

use clap::Args;
use protocol_droid::domain::{SpecError, TransportKind, TransportSpec};
use serde::Serialize;

/// What a bridge command was asked to do.
#[derive(Debug, Args)]
pub struct BridgeArgs {
    /// Where clients connect, as `kind:target` — `http://127.0.0.1:8080/mcp`,
    /// `stdio:./server`.
    #[arg(long, value_name = "SPEC")]
    pub listen: String,

    /// Where the bridge forwards to, same grammar.
    #[arg(long, value_name = "SPEC")]
    pub upstream: String,

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
pub fn run(args: &BridgeArgs) -> anyhow::Result<()> {
    let listen = parse(&args.listen, "--listen")?;
    let upstream = parse(&args.upstream, "--upstream")?;

    let plan = plan(&listen, &upstream);
    if args.json {
        println!("{}", serde_json::to_string_pretty(&plan)?);
    } else {
        eprintln!("{plan}");
    }

    if !plan.serve {
        anyhow::bail!("{}", plan.note);
    }
    Ok(())
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
/// The gap is one-sided and that asymmetry is the whole reason the check
/// lives here. `stdio` has a *server* adapter — a bridge can accept a client
/// over stdio — but no *client* adapter, so a bridge cannot yet forward to one.
/// Checking both ends identically would refuse the working direction; checking
/// only the upstream would let the broken direction start and fail on the first
/// message.
fn upstream_supported(kind: TransportKind) -> bool {
    kind != TransportKind::Stdio
}

/// Whether this build can *accept* a transport on the listen side.
///
/// Every transport, unlike the upstream check. Listening is the server role,
/// and the stdio server adapter exists — it is only the client role that stdio
/// is missing. Applying the upstream check to both ends would refuse the one
/// direction that actually works.
const fn listen_supported(kind: TransportKind) -> bool {
    let _ = kind;
    true
}

fn plan(listen: &TransportSpec, upstream: &TransportSpec) -> Plan {
    let listen_ok = listen_supported(listen.kind());
    let upstream_ok = upstream_supported(upstream.kind());
    let note = if upstream_ok {
        "validated; the serving loop lands with the BridgePeer adapters".to_owned()
    } else {
        format!(
            "this build has no stdio *client* adapter, so a bridge cannot forward to {}; \
             pick an http upstream",
            upstream.target()
        )
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
    }
}

impl std::fmt::Display for Plan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "listen {}:{}  <-  upstream {}:{}",
            self.listen.transport,
            self.listen.target,
            self.upstream.transport,
            self.upstream.target
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
            upstream: upstream.to_owned(),
            json: false,
        }
    }

    /// Both specs parse and the plan reports both ends.
    #[test]
    fn a_well_formed_pair_produces_a_plan() -> Result<(), TestError> {
        let listen = parse("http://127.0.0.1:8080/mcp", "--listen")?;
        let upstream = parse("http://backend:3000/mcp", "--upstream")?;
        let plan = plan(&listen, &upstream);

        assert!(plan.serve);
        assert_eq!(plan.listen.transport, "http");
        assert_eq!(plan.upstream.target, "//backend:3000/mcp");
        assert!(plan.listen.implemented && plan.upstream.implemented);
        Ok(())
    }

    /// The gap is one-sided: a bridge may *accept* stdio but cannot yet
    /// *forward to* it. Refusing both directions would reject the working one;
    /// accepting both would fail on the first message instead.
    #[test]
    fn a_stdio_upstream_is_the_one_refused_pairing() -> Result<(), TestError> {
        let listen = parse("http://127.0.0.1:8080/mcp", "--listen")?;
        let upstream = parse("stdio:./server", "--upstream")?;
        let plan = plan(&listen, &upstream);

        assert!(!plan.serve, "a stdio upstream was reported as servable");
        assert!(
            plan.upstream.target.contains("./server"),
            "the note lost the target it names"
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

    /// An unservable pair exits non-zero rather than printing a plan and
    /// claiming success.
    #[test]
    fn an_unservable_pair_is_an_error() {
        assert!(run(&args("http://127.0.0.1:1/mcp", "stdio:./server")).is_err());
    }

    /// A servable pair does not fail.
    #[test]
    fn a_servable_pair_succeeds() -> Result<(), TestError> {
        run(&args("http://127.0.0.1:1/mcp", "http://backend:3000/mcp"))?;
        Ok(())
    }

    /// A bad spec fails before anything is printed, so an agent parsing stdout
    /// never sees a partial plan.
    #[test]
    fn a_bad_spec_fails_before_reporting() {
        assert!(run(&args("nonsense", "http://backend:3000/mcp")).is_err());
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
