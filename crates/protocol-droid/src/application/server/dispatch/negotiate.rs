//! Settling the revision, and the profile that falls out of it.
//!
//! An `initialize` request names the revision the client wants; the server
//! answers with the one it will actually speak and the capabilities both sides
//! then have. That is a separate question from routing a method, and it
//! happens once per connection rather than once per request — so it lives here
//! rather than in the router.
//!
//! [`negotiate`] is the whole of it. [`supported_on`] is the narrow case that
//! decides the candidate list before negotiation runs: legacy SSE exists only
//! on `2024-11-05`, so a peer arriving on that transport cannot be offered a
//! revision the transport has no name for.

use crate::application::mcp::initialize::read_offer;
use crate::application::server::builder::DispatchError;
use crate::application::server::context::RequestContext;
use crate::domain::feature::FeatureProfile;
use crate::domain::ir::JsonObject;
use crate::domain::{
    CapabilitySet, Negotiation, ProtocolVersion, SupportedVersions, TransportKind,
    negotiate_server, resolve, transport_allowed,
};

/// The revision and profile `initialize` settles on with this peer.
///
/// Resolved for the revision that was *negotiated*, from the capabilities
/// the client actually sent, rather than read off the session. The session
/// was built before any frame arrived and knows nothing about the peer;
/// resolving from it would tell a `2024-11-05` client it had capabilities
/// that revision cannot express, which is the one thing a handshake must
/// never do.
///
/// Free rather than a method: what a revision can express does not depend
/// on which [`Server`] is serving it, and a `&self` here would imply a
/// per-server rule that does not exist.
pub(super) fn negotiate(
    ctx: &RequestContext,
    params: &JsonObject,
) -> Result<(ProtocolVersion, FeatureProfile), DispatchError> {
    let transport = ctx.peer().transport();
    let supported = supported_on(transport)?;
    let offer = read_offer(params).map_err(|_| DispatchError::MalformedInitialize)?;
    let version = negotiate_server(offer.version, &supported).version();
    let profile = resolve(Negotiation {
        version,
        client_caps: offer.capabilities,
        server_caps: CapabilitySet::all_for(version),
        transport,
    });
    Ok((version, profile))
}

/// The revisions this server will agree to over `transport`.
///
/// HTTP+SSE belongs to `2024-11-05` alone, so a legacy session is never
/// answered with a revision that does not define it: a client that offers
/// `2025-11-25` over it is counter-offered `2024-11-05` and decides for itself
/// whether it can continue. Other transports answer with whatever the client
/// offered when this build speaks it.
pub(super) fn supported_on(transport: TransportKind) -> Result<SupportedVersions, DispatchError> {
    let all = SupportedVersions::all().map_err(|_| DispatchError::UnnegotiableVersion)?;
    if transport != TransportKind::LegacySse {
        return Ok(all);
    }
    let defining: Vec<ProtocolVersion> = all
        .as_slice()
        .iter()
        .copied()
        .filter(|version| transport_allowed(*version, transport))
        .collect();
    SupportedVersions::from_slice(&defining).map_err(|_| DispatchError::UnnegotiableVersion)
}
