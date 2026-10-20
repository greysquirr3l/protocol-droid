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
//! and the resulting [`Leg`] goes into
//! [`Wiring`](protocol_droid::application::bridge::Wiring).
//!
//! By reference is the load-bearing part. A handshake made over one connection
//! and forwarding done over another would produce an
//! [`Agreement`](protocol_droid::domain::bridge::Agreement) describing a
//! server the bridge is not talking to — and the whole point of that type is
//! that it is the truth about two real peers.
//!
//! ## The peer is `dyn`
//!
//! [`BridgeLoop`](protocol_droid::application::bridge::BridgeLoop) is generic
//! over its peer, so a library caller picks one at compile time. This caller picks from `--upstream` at run time, and the two
//! transports with a peer adapter are unrelated types. `Arc<dyn BridgePeer>`
//! reconciles the two, and the crate already supplies the blanket impl that
//! makes an `Arc` a peer.
//!
//! That costs one pointer indirection per forwarded envelope. The alternative is
//! a generic function instantiated once per transport with a `match` on
//! [`TransportKind`] in each arm — two copies of the loop's call sequence to
//! save an indirection the peer's own network I/O already dwarfs.

use std::net::SocketAddr;
use std::sync::Arc;

use protocol_droid::adapters::primary::stdio::{StdioConfig, StdioSink, StdioSource};
use protocol_droid::adapters::secondary::codec::result::WireToolResults;
use protocol_droid::adapters::secondary::transport::http::{HttpClient, HttpClientConfig};
use protocol_droid::adapters::secondary::transport::legacy_sse::{LegacySseConfig, LegacySsePeer};
use protocol_droid::application::bridge::{
    BridgePeer, DownstreamConfig, LoopReport, OpenedUpstream, TransportPeer, Upstream,
    UpstreamError, run_client,
};
use protocol_droid::application::client::{ClientConfig, ClientTransport, handshake, open};
use protocol_droid::domain::{
    DegradePolicy, Leg, MaxFrameSize, SupportedVersions, TransportKind, TransportSpec,
    handshake_defined_in,
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
        // The upstream has no session, so there is no handshake to forward:
        // `open` discovers the server's revision, and every request the
        // downstream client sends is carried with that revision and the
        // bridge's capabilities in its own `_meta`.
        TransportKind::StatelessHttp => {
            let (client, transport) = open(upstream, versions).await?;
            let leg = Leg::new(client.negotiated(), client.profile());
            Ok((leg, Arc::new(TransportPeer::new(transport))))
        }
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
    // Newest first: the first revision is the one offered, and `versions` is
    // sorted oldest-first, so offering it as given opens every upstream
    // connection by asking for 2024-11-05 and pins the bridge to it. Only
    // revisions with an `initialize` handshake; `2026-07-28` has none.
    let mut offered: Vec<_> = versions
        .as_slice()
        .iter()
        .copied()
        .filter(|version| handshake_defined_in(*version))
        .collect();
    offered.sort_by(|left, right| right.cmp(left));
    let config = ClientConfig::speaking(offered, kind)?;
    let session = handshake(transport, &config).await?;

    // The profile is the session's, not re-resolved from the two capability
    // sets. `resolve` is pure and would land in the same place, but reading it
    // off the session is the claim that this is what the server agreed to,
    // rather than a second derivation of it that could drift.
    Ok(Leg::new(session.version, session.profile))
}

/// The server a client is proxied to, opened once per client.
///
/// A `stdio` downstream is one process and so one client, which means this
/// struct is opened exactly once here. It exists anyway because the shape is
/// the point: a bridge that opens its upstream at startup and shares it would
/// be correct only for as long as `stdio` stays single-client, and the moment a
/// client arrives on a network transport one connection would have to serve
/// them all — with a `tools/list_changed` naming no client to go to, and a
/// sampling request no way to reach the client it asked.
struct ConfiguredUpstream {
    spec: TransportSpec,
    versions: SupportedVersions,
}

impl Upstream for ConfiguredUpstream {
    async fn open(&self) -> Result<OpenedUpstream, UpstreamError> {
        connect(&self.spec, &self.versions)
            .await
            .map(|(leg, peer)| OpenedUpstream::new(leg, peer))
            // The transports name their own failures and the application layer
            // has no business enumerating them; `connect` already said which
            // one, and the note survives the crossing.
            .map_err(|error| UpstreamError::Unreachable(format!("{error:#}")))
    }
}

/// Serves a bridge on a socket, one upstream per client.
///
/// The HTTP listen side, and the reason `ConfiguredUpstream` is a factory
/// rather than a connection: a server outlives the clients that connect to it,
/// so `Upstream::open` is called once per session and each client gets an
/// upstream of its own. The `stdio` path below opens once because there is one
/// client — which is a property of `stdio`, not of bridges.
///
/// # Errors
///
/// Propagates what the session store, the router, or the socket reported. A
/// client that cannot be reached upstream is reported on that client's own
/// stream and does not stop the server.
pub async fn listen(
    upstream: &TransportSpec,
    addr: SocketAddr,
    versions: &SupportedVersions,
    policy: DegradePolicy,
) -> anyhow::Result<()> {
    use protocol_droid::adapters::primary::transport::http::{
        BridgeSession, HttpBridge, SecurityHeaders, ServerConfig, SessionStore,
    };
    use protocol_droid::adapters::secondary::id_source::RandomIds;

    let config = ServerConfig::new()?.with_security(SecurityHeaders::default());
    let sessions: Arc<SessionStore<BridgeSession>> =
        Arc::new(SessionStore::new(Arc::new(RandomIds::new()?)));

    let bridge = HttpBridge::new(
        config,
        sessions,
        ConfiguredUpstream {
            spec: upstream.clone(),
            versions: versions.clone(),
        },
        protocol_droid::application::bridge::Identity::new(
            "protocol-droid-bridge",
            env!("CARGO_PKG_VERSION"),
        ),
        policy,
        WireToolResults,
    )?;

    // Bound before anything is announced, because port `0` is legal and the
    // address printed has to be the one a client can actually reach.
    let (bound, actual) = bridge.bind(addr).await?;
    eprintln!("bridge listening on http://{actual}/mcp");
    bound.serve().await?;
    Ok(())
}

/// The address a `--listen` target names, or why it does not.
///
/// A target is a URL — `http://127.0.0.1:3000/mcp` — and a `SocketAddr` is
/// the host and port out of it. Parsed here rather than in the domain because
/// only the listen side binds: the client side wants the whole URL, and a
/// shared parser would be a shape neither of them actually wants.
pub fn bind_addr(target: &str) -> anyhow::Result<SocketAddr> {
    let rest = target
        .strip_prefix("http://")
        .or_else(|| target.strip_prefix("https://"))
        .ok_or_else(|| {
            anyhow::anyhow!("{target} is not an http URL; an http listen needs http://host:port")
        })?;
    let host_port = rest.split('/').next().unwrap_or(rest);
    host_port
        .parse::<SocketAddr>()
        .map_err(|error| anyhow::anyhow!("{host_port} is not a host:port: {error}"))
}

/// Runs a bridge until the downstream stream ends.
///
/// The client's channel is this process's own stdio, which is why there is no
/// peer to construct for it: [`BridgeLoop`](protocol_droid::application::bridge::BridgeLoop)
/// reads frames from a source and writes replies to a sink, and both are stdio.
///
/// # Errors
///
/// Whatever the upstream raised while opening or negotiating, and
/// [`LoopError`](protocol_droid::application::bridge::LoopError) for a
/// downstream pipe that fails. A bridge that cannot reach its server has nothing
/// to report per-client, because there is only the one client.
pub async fn run(
    upstream: &TransportSpec,
    versions: &SupportedVersions,
    policy: DegradePolicy,
) -> anyhow::Result<LoopReport> {
    // One frame ceiling for both directions. They are the same stream pair, and
    // two ceilings would mean a frame the source accepts being refused by the
    // sink for being large — after the reply had already been built.
    let framing = StdioConfig::new(MaxFrameSize::DEFAULT);
    let opened = ConfiguredUpstream {
        spec: upstream.clone(),
        versions: versions.clone(),
    };

    Ok(run_client(
        &opened,
        policy,
        DownstreamConfig::with(
            versions.clone(),
            protocol_droid::application::bridge::Identity::new(
                "protocol-droid-bridge",
                env!("CARGO_PKG_VERSION"),
            ),
            TransportKind::Stdio,
        ),
        WireToolResults,
        StdioSource::new(tokio::io::stdin(), framing),
        StdioSink::new(tokio::io::stdout(), framing),
    )
    .await?)
}
