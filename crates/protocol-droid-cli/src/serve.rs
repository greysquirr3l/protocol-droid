//! Standing a bridge up from a validated plan.
//!
//! [`crate::bridge`] answers "can this build run this pair". This answers the
//! next question — "then do it" — and it is a separate module because that
//! answer has three parts the plan has none of: an upstream connection that has
//! to be *opened and negotiated* before the loop exists, a downstream pair
//! built from this process's own stdio, and a report of what moved.
//!
//! ## The upstream is negotiated before anything downstream exists
//!
//! A bridge does not know which revision a client will speak until that client
//! arrives, but it knows which one the server speaks as soon as it connects. So
//! the connection is opened here, [`handshake`] runs against it *by reference*,
//! and the resulting [`Leg`] goes into [`Wiring`].
//!
//! By reference is the load-bearing part. A handshake made over one connection
//! and forwarding done over another would produce an [`Agreement`] describing a
//! server the bridge is not talking to — and the whole point of that type is
//! that it is the truth about two real peers.
//!
//! ## The peer is `dyn`
//!
//! [`BridgeLoop`] is generic over its peer, so a library caller picks one at
//! compile time. This caller picks from `--upstream` at run time, and the two
//! transports with a peer adapter are unrelated types. `Arc<dyn BridgePeer>`
//! reconciles the two, and the crate already supplies the blanket impl that
//! makes an `Arc` a peer.
//!
//! That costs one pointer indirection per forwarded envelope. The alternative is
//! a generic function instantiated once per transport with a `match` on
//! [`TransportKind`] in each arm — two copies of the loop's call sequence to
//! save an indirection the peer's own network I/O already dwarfs.

use std::sync::Arc;

use protocol_droid::adapters::primary::stdio::{StdioConfig, StdioSink, StdioSource};
use protocol_droid::adapters::secondary::codec::result::WireToolResults;
use protocol_droid::adapters::secondary::transport::http::{HttpClient, HttpClientConfig};
use protocol_droid::adapters::secondary::transport::legacy_sse::{LegacySseConfig, LegacySsePeer};
use protocol_droid::application::bridge::{
    BridgeLoop, BridgePeer, DownstreamConfig, LoopReport, Wiring,
};
use protocol_droid::application::client::{ClientConfig, ClientTransport, handshake};
use protocol_droid::domain::{
    DegradePolicy, Leg, MaxFrameSize, SupportedVersions, TransportKind, TransportSpec,
};

/// Opens the upstream and settles what it can do.
///
/// # Errors
///
/// Propagates whatever the transport or the handshake raised. Both already name
/// the endpoint and the revision, and re-wrapping them would bury the fact an
/// operator needs: whether the server *refused*, or could not be reached.
///
/// A `stdio` upstream is refused rather than half-handled. `TransportSpec` can
/// parse one, and a program to run is what a `stdio` upstream needs — but
/// `kind:target` has no room for a program plus its arguments, so honouring it
/// would mean inventing a command line the operator never wrote.
pub async fn connect(
    upstream: &TransportSpec,
    versions: &SupportedVersions,
) -> anyhow::Result<(Leg, Arc<dyn BridgePeer>)> {
    match upstream.kind() {
        TransportKind::StreamableHttp => {
            let client = HttpClient::new(HttpClientConfig::new(upstream.target())?)?;
            let peer = Arc::new(client);
            let leg = negotiate(peer.as_ref(), TransportKind::StreamableHttp, versions).await?;
            Ok((leg, peer))
        }
        TransportKind::LegacySse => {
            let client = protocol_droid::adapters::secondary::transport::legacy_sse::LegacySseClient::connect(
                LegacySseConfig::new(upstream.target())?,
            )
            .await?;
            // The handshake runs against the *client*, and the peer wraps that
            // same client by `Arc`. One session, read two ways — not two
            // connections that could disagree about what was negotiated.
            let leg = negotiate(&client, TransportKind::LegacySse, versions).await?;
            Ok((leg, Arc::new(LegacySsePeer::new(Arc::new(client)))))
        }
        TransportKind::Stdio => anyhow::bail!(
            "a stdio upstream needs a program to run, and --upstream carries only \
             kind:target; spawn the server yourself and bridge to an http endpoint"
        ),
        // Refused here as well as by the plan's check. The plan already turns
        // this into a non-zero exit with a note naming the target, so reaching
        // this arm means the two disagreed — and the arm that says so is the one
        // that should be reached, not a silent `todo!()`.
        TransportKind::StatelessHttp => anyhow::bail!(
            "this build has no stateless-http client adapter, so it cannot open \
             an upstream session to forward to"
        ),
    }
}

/// Runs the handshake and keeps only the leg it produced.
///
/// Takes `&T` rather than `Arc<T>` so the caller can hand the *same* value to
/// the bridge afterwards.
async fn negotiate(
    transport: &(impl ClientTransport + ?Sized),
    kind: TransportKind,
    versions: &SupportedVersions,
) -> anyhow::Result<Leg> {
    let config = ClientConfig::speaking(versions.as_slice().to_vec(), kind)?;
    let session = handshake(transport, &config).await?;

    // The profile is the session's, not re-resolved from the two capability
    // sets. `resolve` is pure and would land in the same place, but reading it
    // off the session is the claim that this is what the server agreed to,
    // rather than a second derivation of it that could drift.
    Ok(Leg::new(session.version, session.profile))
}

/// Runs a bridge until the downstream stream ends.
///
/// The client's channel is this process's own stdio, which is why there is no
/// peer to construct for it: [`BridgeLoop`] reads frames from a source and
/// writes replies to a sink, and both are stdio.
///
/// # Errors
///
/// Whatever the upstream raised while opening or negotiating, and
/// [`LoopError`](protocol_droid::application::bridge::LoopError) for a
/// downstream pipe that fails. A bridge that cannot reach its server has nothing
/// to report per-client, because there are no clients.
pub async fn run(
    upstream: &TransportSpec,
    versions: &SupportedVersions,
    policy: DegradePolicy,
) -> anyhow::Result<LoopReport> {
    let (leg, peer) = connect(upstream, versions).await?;

    // One frame ceiling for both directions. They are the same stream pair, and
    // two ceilings would mean a frame the source accepts being refused by the
    // sink for being large — after the reply had already been built.
    let framing = StdioConfig::new(MaxFrameSize::DEFAULT);
    let mut bridge_loop = BridgeLoop::new(
        Wiring::new(leg, policy, peer, WireToolResults),
        DownstreamConfig::with(
            versions.clone(),
            protocol_droid::application::bridge::Identity::new(
                "protocol-droid-bridge",
                env!("CARGO_PKG_VERSION"),
            ),
            TransportKind::Stdio,
        ),
        StdioSource::new(tokio::io::stdin(), framing),
        StdioSink::new(tokio::io::stdout(), framing),
    );

    Ok(bridge_loop.run().await?)
}
