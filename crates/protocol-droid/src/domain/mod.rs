//! Domain layer: value objects, aggregates and pure functions.
//!
//! This layer has zero I/O dependencies. Time, randomness and session-id
//! generation are injected from outside it rather than called globally, so
//! every function here is deterministic and testable.
//!
//! Populated by T04 (protocol version value objects), T05-T08 (feature profile,
//! negotiation, canonical IR, per-revision codecs), T10 (the session aggregate:
//! lifecycle, in-flight requests, cancellation, progress) and T11 (the registry's
//! pure half: primitives, visibility, snapshots).

pub mod bridge;
mod capability;
// T27. The A2A domain value objects: version negotiation, the
// per-revision feature profile, the `AgentCard`, and the well-known
// path. Gated on the `a2a` feature because the `url` dep is
// unconditional under the gate (only the A2A agent card uses it
// with the `serde` feature on) and a consumer that never speaks
// A2A should not pull it in.
#[cfg(feature = "a2a")]
pub mod a2a;
// T29. The ACP (Agent Client Protocol) domain value objects:
// version, method table, per-feature profile. The schema and the
// types in this module are pure data; gating is on the experimental
// flag because the spec is pre-1.0 (`schema-v1.25.0`) and the
// task explicitly excludes the module from semver guarantees. The
// schema is data and stays compiled in a lean build; the *types*
// are gated.
#[cfg(feature = "acp-experimental")]
pub mod acp;
pub mod feature;
pub mod idempotency;
pub mod ir;
pub mod jsonrpc;
pub mod mcp;
mod negotiation;
// OAuth's own vocabulary, behind the feature that pulls its dependencies. The
// authorization *model* in `mcp::authorization` is ungated because it is pure
// data every consumer needs to read a revision's requirements; this is gated
// because it pulls a SHA-256 implementation and a base64url codec, which a
// lean build has no other reason to link.
#[cfg(feature = "oauth")]
pub mod oauth;
mod protocol_version;
pub mod registry;
mod release_date;
mod request_id;
pub mod session;
mod stream;
mod supported_versions;
mod transport;

pub use bridge::{Agreement, Leg, RefusedBy, Withheld, agree};
pub use capability::{
    CAPABILITY_TABLE, CLIENT_CAPABILITY_WIRE, Capability, CapabilitySet, SERVER_CAPABILITY_WIRE,
    Side, defined_in,
};
pub use feature::{
    DegradePolicy, FEATURES, Feature, FeatureProfile, FeatureSpec, Negotiation, resolve,
};
pub use idempotency::{
    Claim, ClaimToken, CompletedResults, IdempotencyError, IdempotencyKey, StaleClaim,
};
pub use mcp::{
    AuthorizationModel, BearerMethod, ClientRegistration, MCP_METHOD_HEADER, MCP_NAME_HEADER,
    McpSessionId, McpSessionIdError, ResourceMetadata, ResourceUri, ResourceUriError,
    RoutingHeaderError, RoutingHeaders, RoutingName, RoutingTarget, VISIBLE_ASCII_RANGE,
    WELL_KNOWN, authorization_model,
};
pub use negotiation::{
    NegotiatedSession, NegotiationError, PeerInfo, ServerDecision, StatelessRequest,
    assumed_version, assumed_version_without_signal, check_protocol_header, handshake_defined_in,
    is_reserved_meta_key, negotiate_client, negotiate_server, negotiate_stateless,
    stateless_meta_keys,
};
pub use protocol_version::{ParseProtocolVersionError, ProtocolVersion, UnknownVersionError};
pub use release_date::{ParseReleaseDateError, ReleaseDate};
pub use request_id::RequestId;
pub use stream::{
    EventId, EventName, EventNameError, EventRing, RECOMMENDED_CAPACITY, RecommendedCapacity,
    Replay, StreamEvent, names,
};
pub use supported_versions::{SupportedVersions, SupportedVersionsError};
pub use transport::{
    FrameDecoder, FramingError, MaxFrameSize, MaxFrameSizeError, MessageEndpoint,
    MessageEndpointError, RateLimit, SpecError, TransportKind, TransportSpec, Verdict,
    check_rate_limit, encode_frame, transport_allowed,
};

#[cfg(test)]
mod tests {
    use crate::domain::{ProtocolVersion, ReleaseDate, TransportKind, transport_allowed};

    /// The three ordered types have to agree with each other, not only with
    /// themselves.
    ///
    /// Each of `ReleaseDate`, `ProtocolVersion` and `TransportKind` carries its
    /// own `derived_ordering_agrees_in_both_directions`, which is the 1.98
    /// derive trap — that one trait agrees with the other within a type. This
    /// is the weaker claim that no test inside a single module can make: the
    /// three orderings mean the same thing, so a revision's position in
    /// `ProtocolVersion` matches its `ReleaseDate`, and a transport's position
    /// matches the revision that introduced it.
    ///
    /// The disagreement this catches is not hypothetical. `TransportKind` is
    /// ordered by introducing revision, and `StreamableHttp` predates
    /// `StatelessHttp` by two revisions rather than one — a transport list
    /// sorted by "how new is this" and one sorted by declaration order would
    /// come out the same here, but only because the declarations were written
    /// in that order deliberately.
    #[test]
    fn the_three_orderings_tell_the_same_story() {
        // Every version sorts exactly where its date says it does.
        for version in ProtocolVersion::KNOWN {
            let date = version.date();
            for other in ProtocolVersion::KNOWN {
                assert_eq!(
                    version < other,
                    date < other.date(),
                    "{version} and {other} order differently by version than by date"
                );
            }
        }

        // The known versions are in date order, so `KNOWN` being sorted is a
        // fact about the calendar and not a habit.
        assert!(ProtocolVersion::KNOWN.is_sorted());
        assert!(ReleaseDate::V2024_11_05 < ReleaseDate::V2026_07_28);

        // Every transport was introduced by some known revision, and no
        // transport predates the first one — which is the fact that makes
        // `TransportKind::ALL` ordered the same way as the revisions are.
        let introduced_by = [
            TransportKind::Stdio,
            TransportKind::LegacySse,
            TransportKind::StreamableHttp,
            TransportKind::StatelessHttp,
        ];
        for (transport, first) in introduced_by.iter().zip([
            ProtocolVersion::V2024_11_05,
            ProtocolVersion::V2024_11_05,
            ProtocolVersion::V2025_03_26,
            ProtocolVersion::V2026_07_28,
        ]) {
            assert!(
                transport_allowed(first, *transport),
                "{transport} claims to start at {first} but is not allowed there"
            );
            for earlier in ProtocolVersion::KNOWN
                .into_iter()
                .take_while(|v| *v < first)
            {
                assert!(
                    !transport_allowed(earlier, *transport),
                    "{transport} is allowed on {earlier}, which predates {first}"
                );
            }
        }
    }
}
