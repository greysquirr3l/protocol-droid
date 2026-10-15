//! The A2A leg of `protocol-droid bridge`.
//!
//! Wires the three pieces that turn `protocol-droid bridge --peer
//! https://agent.example/a2a` into a working bridge:
//!
//! - [`A2aJsonRpcPeer`] posts raw JSON-RPC envelopes to the
//!   agent's well-known JSON-RPC method table and returns
//!   raw envelopes back. The bridge treats the agent as a
//!   wire-level peer, not a typed `A2aClient`.
//! - [`A2aMcpCodec`](protocol_droid::application::bridge::A2aMcpCodec)
//!   lifts `tools/call` envelopes to `SendMessage` and lowers
//!   the agent's `Message` / `Task` reply back to a
//!   `CallToolResult`.
//! - [`CrossBridgePeer`](protocol_droid::application::bridge::CrossBridgePeer)
//!   is the `BridgePeer` that runs every envelope through the
//!   codec, with the loop untouched underneath.
//!
//! The MCP-downstream `BridgeLoop` does not know A2A exists;
//! it just sees a `BridgePeer`. The codec is the only thing
//! that knows both protocols.

use std::sync::Arc;

use anyhow::Context as _;
use protocol_droid::adapters::primary::stdio::{StdioConfig, StdioSink, StdioSource};
use protocol_droid::adapters::secondary::a2a::A2aJsonRpcPeer;
use protocol_droid::adapters::secondary::codec::result::WireToolResults;
use protocol_droid::application::bridge::{
    A2aMcpCodec, BridgeError, BridgePeer, CrossBridgePeer, CrossProtocolCodec, DownstreamConfig,
    OpenedUpstream, ProtocolSide, run_client,
};
use protocol_droid::domain::feature::{Feature, FeatureProfile};
use protocol_droid::domain::{
    DegradePolicy, Leg, MaxFrameSize, ProtocolVersion, SupportedVersions, TransportKind,
};
use serde::Serialize;
use tokio::io;

/// What the CLI told the A2A bridge to do.
#[derive(Debug)]
pub struct A2aBridgeArgs {
    /// The A2A agent's base URL.
    pub peer: String,
    /// Whether to emit JSON on stdout, with the human
    /// summary on stderr.
    pub json: bool,
}

/// One A2A bridge session's `Upstream`.
#[derive(Debug)]
pub struct A2aUpstream {
    inner: Arc<A2aJsonRpcPeer>,
    /// The profile the bridge advertises to the
    /// downstream. Empty: the A2A bridge has no
    /// MCP-side feature the consumer's `WireToolResults`
    /// codec would care about.
    profile: FeatureProfile,
}

impl A2aUpstream {
    fn new(peer: A2aJsonRpcPeer) -> Self {
        // The leg's profile is what the loop's `check_method`
        // consults when a downstream call arrives. The A2A
        // bridge's downstream is an MCP client; the codec maps
        // MCP `tools/call` to A2A `SendMessage` and back, and
        // an empty profile would have the loop refuse every
        // `tools/call` as a feature the agreement does not
        // serve. Seeding the profile with the features the
        // codec bridges is the difference between "the bridge
        // exists" and "the bridge actually moves calls".
        let mut profile = FeatureProfile::empty();
        profile.insert(Feature::Tools);
        Self {
            inner: Arc::new(peer),
            profile,
        }
    }
}

impl protocol_droid::application::bridge::Upstream for A2aUpstream {
    /// The A2A upstream's `open` is a pure construction:
    /// wrapping the peer in a `CrossBridgePeer` and
    /// returning a `Leg`. There is no I/O to wait on, so
    /// the future returns immediately — a hand-rolled
    /// `Ready` keeps the call site synchronous and lets
    /// the loop pull the bridge up without an
    /// `await`.
    fn open(
        &self,
    ) -> impl core::future::Future<
        Output = Result<OpenedUpstream, protocol_droid::application::bridge::UpstreamError>,
    > + Send {
        let codec: Arc<dyn CrossProtocolCodec> = Arc::new(A2aMcpCodec::new());
        let inner = Arc::clone(&self.inner);
        let result = (|| {
            let wrapped = CrossBridgePeer::new(ProtocolSide::Mcp, codec, inner).map_err(
                |err: BridgeError| {
                    protocol_droid::application::bridge::UpstreamError::Unreachable(format!(
                        "could not wrap the A2A peer: {err}"
                    ))
                },
            )?;
            // A2A has no JSON-RPC handshake the loop runs
            // today; the `Leg` is the loop's view of the
            // transport, with the latest revision the loop
            // knows about and the profile the bridge
            // advertises.
            let leg = Leg::new(latest_version(), self.profile);
            Ok(OpenedUpstream::new(
                leg,
                Arc::new(wrapped) as Arc<dyn BridgePeer>,
            ))
        })();
        core::future::ready(result)
    }
}

/// The latest revision the loop knows about, used for
/// the A2A leg's `Leg`.
///
/// A bridge that did not negotiate a version — because
/// the upstream is A2A and there is no MCP handshake —
/// still needs *some* `Leg::version`, and the newest
/// one is the right choice: a future `Leg::profile`
/// check is no stricter than the current one, and an
/// operator whose downstream asks for a feature the
/// newest revision does not have is asking for
/// something the rest of the build already refuses.
const fn latest_version() -> ProtocolVersion {
    ProtocolVersion::V2026_07_28
}

/// Runs the A2A bridge: opens the agent peer, runs the
/// bridge loop on stdio, prints a summary.
///
/// # Errors
///
/// `anyhow` errors name the stage that failed: peer URL
/// construction, the upstream open, or a `LoopError`
/// from the bridge itself. The agent's refusal to
/// answer a tool call is a per-request error and is
/// reflected in the summary's `failed` counter, not
/// here.
pub async fn run(args: A2aBridgeArgs) -> anyhow::Result<()> {
    let framing = StdioConfig::new(MaxFrameSize::DEFAULT);
    let summary = run_with_io(
        &args,
        StdioSource::new(io::stdin(), framing),
        StdioSink::new(io::stdout(), framing),
    )
    .await?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        eprintln!("{summary}");
    }
    Ok(())
}

/// Runs the A2A bridge against caller-supplied IO streams,
/// returning the loop's summary as a typed value.
///
/// Split from [`run`] so a test can drive the bridge with
/// a `tokio::io::DuplexStream` pair instead of the
/// process's own stdin/stdout — `tokio::io::stdin()` is
/// the process's stdin, and a `#[tokio::test]` cannot
/// reach in and replace it. The test owns the streams,
/// the bridge owns the loop, and the seam is here.
///
/// The streams are taken by value (not by `&mut`) so the
/// bridge can move them into the loop's `BridgeLoop`,
/// which `Send`s each frame to the sink. A test that
/// needs to read what the bridge wrote is also the
/// stream's other half, and the loop returns the sink
/// end of the duplex when the loop is done.
///
/// # Errors
///
/// The same as [`run`]: peer construction, upstream
/// open, or a loop error. The summary is returned
/// only when the loop completed normally; a
/// mid-loop error short-circuits with `?`.
pub async fn run_with_io<R, W>(
    args: &A2aBridgeArgs,
    source: StdioSource<R>,
    sink: StdioSink<W>,
) -> anyhow::Result<LoopSummary>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let peer = A2aJsonRpcPeer::new(&args.peer)
        .with_context(|| format!("could not build the A2A peer for {}", args.peer))?;
    let upstream = A2aUpstream::new(peer);

    let policy = DegradePolicy::default();
    let versions = SupportedVersions::all()?;

    let report = run_client(
        &upstream,
        policy,
        DownstreamConfig::with(
            versions,
            protocol_droid::application::bridge::Identity::new(
                "protocol-droid-bridge",
                env!("CARGO_PKG_VERSION"),
            ),
            TransportKind::Stdio,
        ),
        WireToolResults,
        source,
        sink,
    )
    .await?;

    Ok(LoopSummary::from(report))
}

/// The bridge summary, in the shape the rest of the CLI
/// uses so the JSON and human forms stay the same
/// between `--upstream` and `--peer`.
#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub struct LoopSummary {
    /// Whether a client completed a handshake.
    pub handshaken: bool,
    /// Envelopes forwarded to the agent.
    pub forwarded: u64,
    /// Notifications forwarded to the agent.
    pub notified: u64,
    /// Frames refused, each answered to the client.
    pub failed: u64,
}

impl From<protocol_droid::application::bridge::LoopReport> for LoopSummary {
    fn from(report: protocol_droid::application::bridge::LoopReport) -> Self {
        Self {
            handshaken: report.handshaken,
            forwarded: report.forwarded,
            notified: report.notified,
            failed: report.failed,
        }
    }
}

impl core::fmt::Display for LoopSummary {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
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

#[cfg(test)]
mod tests {
    use super::LoopSummary;
    use protocol_droid::application::bridge::LoopReport;

    /// The A2A bridge's summary is the same shape the
    /// MCP bridge prints, so the operator's terminal
    /// looks the same regardless of `--upstream` or
    /// `--peer`.
    #[test]
    fn the_a2a_summary_renders_the_same_shape_as_mcp() {
        let summary = LoopSummary {
            handshaken: true,
            forwarded: 3,
            notified: 1,
            failed: 0,
        };
        let rendered = format!("{summary}");
        assert!(rendered.contains("a client connected"));
        assert!(rendered.contains("forwarded 3"));
        assert!(rendered.contains("notified 1"));
    }

    /// A bridge whose loop never saw a client reports
    /// it on the human line, not as a silent zero.
    #[test]
    fn a_bridge_with_no_client_says_so() {
        let summary = LoopSummary {
            handshaken: false,
            forwarded: 0,
            notified: 0,
            failed: 0,
        };
        let rendered = format!("{summary}");
        assert!(rendered.contains("no client ever connected"));
    }

    /// The summary can be built from a `LoopReport`,
    /// so the bridge loop's report lands in the same
    /// struct the rest of the CLI prints.
    #[test]
    fn the_summary_comes_from_a_loop_report() {
        let report = LoopReport {
            handshaken: true,
            forwarded: 7,
            notified: 2,
            failed: 1,
            relayed: 0,
        };
        let summary = LoopSummary::from(report);
        assert_eq!(summary.forwarded, 7);
        assert_eq!(summary.failed, 1);
    }
}
