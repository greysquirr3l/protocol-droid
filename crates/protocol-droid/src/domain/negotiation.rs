//! Negotiation — turning what a peer asked for into the version this session
//! will speak, and refusing loudly when the two sides cannot agree.
//!
//! Every function here is pure. There is no session registry, no I/O, and no
//! clock: negotiation is a decision, and a decision you can replay is a
//! decision you can test. The stateful half — remembering which version a
//! connection settled on — belongs to T10's session engine, which calls into
//! this module rather than reimplementing it.
//!
//! # The three things negotiation has to get right
//!
//! **Echo when you can.** A client that asks for a revision the server speaks
//! gets that revision back, not the server's favourite. Getting this wrong is
//! rmcp issue #852, and it is the single most common negotiation bug: a server
//! that always answers with its oldest or its newest revision forces the peer
//! onto a version it did not ask for. [`negotiate_server`] is written so that
//! the "echo" branch is the first one taken, and
//! [`tests::a_supported_request_is_echoed_not_replaced`] pins it.
//!
//! **Never error on an unknown version.** A client asking for `2099-01-01` —
//! a revision from the future, or a typo, or a client newer than this build —
//! gets the server's latest back, not an error. Refusing is how a server
//! becomes unusable the day a new revision ships; answering with what you do
//! speak is how it stays usable. The error is reserved for the case where
//! *nothing* can be agreed, which for a server with at least one revision is
//! unreachable.
//!
//! **The header is an assertion, not a fallback.** A present
//! `MCP-Protocol-Version` that disagrees with the negotiated version is a
//! protocol error, and it is rejected rather than reconciled. Reconciling
//! means guessing which of the two a request was really meant for.
//!
//! # Why the fallback is 2025-03-26 and not the oldest
//!
//! [`ASSUMED_VERSION_WITHOUT_SIGNAL`] is what a Streamable HTTP request with
//! no version header and no other signal is assumed to be. It is
//! `2025-03-26`, not `2024-11-05`, because the header requirement was
//! introduced *at* `2025-06-18` (PR #548) and the version in force when the
//! requirement did not yet exist is the one before it. Assuming the oldest
//! revision instead would be the rmcp #852 bug wearing a different hat: it
//! silently demotes a modern peer to a 2024 profile.

use crate::domain::{
    CapabilitySet, FeatureProfile, ProtocolVersion, SupportedVersions, TransportKind, resolve,
};

/// The version a request is assumed to be when it carries no version signal at
/// all.
///
/// `2025-06-18` made the `MCP-Protocol-Version` header mandatory on HTTP
/// (PR #548), and the revision in force immediately before that requirement
/// existed is this one. See the module docs for why the oldest revision is the
/// wrong fallback.
#[must_use]
pub const fn assumed_version_without_signal() -> ProtocolVersion {
    ProtocolVersion::V2025_03_26
}

/// Whether `version` has an `initialize` handshake at all.
///
/// `2024-11-05` through `2025-11-25` define `InitializeRequest` and
/// `InitializeResult`, both with `protocolVersion`, `capabilities` and
/// `serverInfo` required. SEP-2575 removes the handshake in `2026-07-28`: the
/// vendored schema for that revision has no `InitializeRequest` and no
/// `InitializeResult`, and a session is established by the transport rather
/// than by a round trip.
///
/// A question with no schema answer behind it, and so the reason it is asked
/// here rather than being read off a feature row: the handshake is a
/// message, and a removed message is not a behaviour that switched off. `ping`
/// is a feature in [`FEATURES`](crate::domain::FEATURES) because something can
/// still depend on it being refused; the handshake has nothing left to gate.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::{ProtocolVersion, handshake_defined_in};
///
/// assert!(handshake_defined_in(ProtocolVersion::V2025_11_25));
/// // SEP-2575: no `initialize` request or result exists in this revision.
/// assert!(!handshake_defined_in(ProtocolVersion::V2026_07_28));
/// ```
#[must_use]
pub const fn handshake_defined_in(version: ProtocolVersion) -> bool {
    matches!(
        version,
        ProtocolVersion::V2024_11_05
            | ProtocolVersion::V2025_03_26
            | ProtocolVersion::V2025_06_18
            | ProtocolVersion::V2025_11_25
    )
}

/// What a peer calls itself.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PeerInfo {
    /// The peer's declared name.
    pub name: String,
    /// The peer's declared version, free-form.
    pub version: String,
}

impl PeerInfo {
    /// A peer description.
    #[must_use]
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
        }
    }
}

/// A session that has agreed on a version and a feature profile.
///
/// Deliberately not a connection: for `2026-07-28` this is per-request and
/// nothing is retained, which is why [`Self::is_stateless`] is part of the
/// value rather than a property of whatever holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegotiatedSession {
    /// The version both sides speak.
    pub version: ProtocolVersion,
    /// What this session may do.
    pub profile: FeatureProfile,
    /// What the client advertised.
    pub client_caps: CapabilitySet,
    /// What the server advertised.
    pub server_caps: CapabilitySet,
    /// The transport the session runs over.
    pub transport: TransportKind,
    /// The client implementation, when it identified itself.
    pub client_info: Option<PeerInfo>,
    /// The server implementation, when it identified itself.
    pub server_info: Option<PeerInfo>,
    /// Whether this profile is recomputed per request with nothing retained.
    pub is_stateless: bool,
}

impl NegotiatedSession {
    /// Builds a session from an agreed version and a pair of capability sets.
    #[must_use]
    pub fn new(
        version: ProtocolVersion,
        transport: TransportKind,
        client_caps: CapabilitySet,
        server_caps: CapabilitySet,
    ) -> Self {
        Self {
            version,
            profile: resolve(crate::domain::Negotiation {
                version,
                client_caps,
                server_caps,
                transport,
            }),
            client_caps,
            server_caps,
            transport,
            client_info: None,
            server_info: None,
            is_stateless: version >= ProtocolVersion::V2026_07_28,
        }
    }

    /// Attaches the client's identity.
    #[must_use]
    pub fn with_client_info(mut self, info: PeerInfo) -> Self {
        self.client_info = Some(info);
        self
    }

    /// Attaches the server's identity.
    #[must_use]
    pub fn with_server_info(mut self, info: PeerInfo) -> Self {
        self.server_info = Some(info);
        self
    }

    /// Whether the session may do `feature`.
    #[must_use]
    pub fn allows(&self, feature: crate::domain::Feature) -> bool {
        self.profile.allows(feature)
    }

    /// How many features the session may do.
    #[must_use]
    pub const fn len(&self) -> u32 {
        self.profile.len()
    }

    /// Whether the session may do nothing at all.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.profile.is_empty()
    }
}

/// Why negotiation could not produce a session.
///
/// Every variant is a case where answering with a fallback would be a guess
/// about what the peer meant. Cases where a fallback is *specified* — an
/// unknown version, a missing header — are not errors and do not appear here.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NegotiationError {
    /// The server answered with a revision this client cannot speak, and the
    /// client has no revision in common with it.
    #[error(
        "server answered with protocol version {answered}, which this client does not support; \
         it supports {supported}"
    )]
    UnsupportedServerVersion {
        /// What the server answered.
        answered: ProtocolVersion,
        /// What this client can speak, ascending.
        supported: String,
    },

    /// A present `MCP-Protocol-Version` header disagrees with the negotiated
    /// version.
    ///
    /// A rejection rather than a reconciliation: a header that says one thing
    /// while the session says another is a client that has lost track, and
    /// picking either would be guessing.
    #[error("MCP-Protocol-Version header says {header}, but the session negotiated {negotiated}")]
    HeaderMismatch {
        /// The negotiated version.
        negotiated: ProtocolVersion,
        /// The version the header named, verbatim.
        header: String,
    },

    /// The `MCP-Protocol-Version` header was not a version this build knows.
    #[error("MCP-Protocol-Version header {header:?} is not a protocol version")]
    HeaderNotAVersion {
        /// The header value, verbatim.
        header: String,
    },

    /// A stateless request carried no version in `_meta`, which `2026-07-28`
    /// requires.
    ///
    /// This one has no fallback on purpose. The whole point of the stateless
    /// lifecycle is that every request is self-describing, so a request that
    /// is not has left the only signal the protocol provides.
    #[error(
        "stateless {version} request carried no io.modelcontextprotocol/protocolVersion in _meta; \
         the stateless lifecycle has no fallback because it has no session to fall back to"
    )]
    MissingStatelessVersion {
        /// The lifecycle that demanded it.
        version: ProtocolVersion,
    },
}

/// What the server decided to speak.
///
/// Echo and counter-offer are different outcomes and callers need to tell them
/// apart: a bridge that was downgraded wants to log it, and
/// `server/discover` advertises what is *supported* rather than what a
/// particular client was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerDecision {
    /// The client asked for something the server speaks, and got it.
    Echoed(ProtocolVersion),
    /// The client asked for something the server does not speak. Not an error:
    /// the server answers with its latest, per the lifecycle spec.
    CounterOffered {
        /// What the client asked for.
        offered: ProtocolVersion,
        /// What the server speaks instead.
        answer: ProtocolVersion,
    },
}

impl ServerDecision {
    /// The version the session will speak either way.
    #[must_use]
    pub const fn version(self) -> ProtocolVersion {
        match self {
            Self::Echoed(version)
            | Self::CounterOffered {
                answer: version, ..
            } => version,
        }
    }

    /// Whether the client was downgraded.
    #[must_use]
    pub const fn is_counter_offer(self) -> bool {
        matches!(self, Self::CounterOffered { .. })
    }
}

/// Server-side version negotiation.
///
/// Echoes the offered version when the server supports it, and otherwise answers
/// with the server's latest. Never fails: the specification says a client
/// asking for `2099-01-01` gets the server's latest back, and a server that
/// errors instead stops working the day a new revision is published.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::{ProtocolVersion, ServerDecision, SupportedVersions, negotiate_server};
///
/// let all = SupportedVersions::all().unwrap();
/// // The echo branch, which is the one that gets missed.
/// assert_eq!(
///     negotiate_server(ProtocolVersion::V2025_03_26, &all),
///     ServerDecision::Echoed(ProtocolVersion::V2025_03_26),
/// );
/// // An unknown future version is answered, not rejected.
/// let decision = negotiate_server(ProtocolVersion::unknown_after_known("2099-01-01".parse().unwrap()).unwrap(), &all);
/// assert!(decision.is_counter_offer());
/// ```
#[must_use]
pub fn negotiate_server(offered: ProtocolVersion, supported: &SupportedVersions) -> ServerDecision {
    if supported.contains(&offered) {
        ServerDecision::Echoed(offered)
    } else {
        ServerDecision::CounterOffered {
            offered,
            answer: supported.latest_handshake(),
        }
    }
}

/// Client-side version negotiation.
///
/// The client offered `preferred`; the server answered `answered`. The client
/// accepts when it can speak the answer, and rejects with
/// [`NegotiationError::UnsupportedServerVersion`] when it cannot.
///
/// A rejection here means disconnect, per the lifecycle spec — not a retry with
/// a different version, and not acceptance of a version the client cannot parse.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::{ProtocolVersion, negotiate_client};
///
/// let all = protocol_droid::domain::SupportedVersions::all().unwrap();
/// let accepted =
///     negotiate_client(ProtocolVersion::V2025_06_18, ProtocolVersion::V2025_06_18, &all);
/// assert_eq!(accepted.unwrap(), ProtocolVersion::V2025_06_18);
/// ```
///
/// # Errors
///
/// [`NegotiationError::UnsupportedServerVersion`] when the client's supported
/// set and the server's answer are disjoint.
pub fn negotiate_client(
    _preferred: ProtocolVersion,
    answered: ProtocolVersion,
    supported: &SupportedVersions,
) -> Result<ProtocolVersion, NegotiationError> {
    if supported.contains(&answered) {
        return Ok(answered);
    }
    Err(NegotiationError::UnsupportedServerVersion {
        answered,
        supported: format_versions(supported),
    })
}

/// What a stateless `2026-07-28` request said about itself.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StatelessRequest {
    /// `io.modelcontextprotocol/protocolVersion` from the request's `_meta`.
    pub version: Option<ProtocolVersion>,
    /// The capabilities the client advertised in `_meta`.
    pub client_caps: CapabilitySet,
    /// The client identity from `io.modelcontextprotocol/clientInfo`.
    pub client_info: Option<PeerInfo>,
    /// The server identity to stamp into each result's `_meta`.
    pub server_info: Option<PeerInfo>,
}

impl StatelessRequest {
    /// A request with nothing in its `_meta`.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            version: None,
            client_caps: CapabilitySet::empty(),
            client_info: None,
            server_info: None,
        }
    }

    /// Sets the version.
    #[must_use]
    pub const fn with_version(mut self, version: ProtocolVersion) -> Self {
        self.version = Some(version);
        self
    }

    /// Sets the client capabilities.
    #[must_use]
    pub const fn with_client_caps(mut self, caps: CapabilitySet) -> Self {
        self.client_caps = caps;
        self
    }
}

/// Stateless per-request negotiation, as `2026-07-28` defines it.
///
/// Every request carries its own version and client capabilities in `_meta`,
/// so the profile is computed from the request alone and nothing is retained
/// between requests. This is why [`NegotiatedSession::is_stateless`] is true
/// for a session built here, and why there is no fallback when the version is
/// missing: with no session, there is nothing to fall back to.
///
/// The server's own capabilities come from `server_caps`, which is the server's
/// configuration rather than anything the request said — the request cannot
/// grant the server a capability.
///
/// # Errors
///
/// [`NegotiationError::MissingStatelessVersion`] when the request carries no
/// `io.modelcontextprotocol/protocolVersion`.
///
/// [`NegotiationError::UnsupportedServerVersion`] when the request names a
/// revision the server does not speak and the server has no revision to offer.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::{
///     Capability, CapabilitySet, ProtocolVersion, StatelessRequest, SupportedVersions,
///     TransportKind, negotiate_stateless,
/// };
///
/// let server = CapabilitySet::empty().with(Capability::Tools);
/// let session = negotiate_stateless(
///     &SupportedVersions::all().unwrap(),
///     server,
///     TransportKind::Stdio,
///     StatelessRequest::empty()
///         .with_version(ProtocolVersion::V2026_07_28)
///         .with_client_caps(CapabilitySet::all_for(ProtocolVersion::V2026_07_28)),
/// )?;
/// assert!(session.is_stateless);
/// # Ok::<(), protocol_droid::domain::NegotiationError>(())
/// ```
pub fn negotiate_stateless(
    supported: &SupportedVersions,
    server_caps: CapabilitySet,
    transport: TransportKind,
    request: StatelessRequest,
) -> Result<NegotiatedSession, NegotiationError> {
    let offered = request
        .version
        .ok_or(NegotiationError::MissingStatelessVersion {
            version: ProtocolVersion::V2026_07_28,
        })?;

    // Same rule as the stateful path, because a stateless request is still a
    // client asking for a version: an unknown one is answered, not rejected.
    let version = negotiate_server(offered, supported).version();
    // A capability set from a newer revision cannot switch on a feature the
    // agreed revision cannot express.
    let client_caps = request.client_caps.restricted_to(version);

    let mut session = NegotiatedSession::new(version, transport, client_caps, server_caps);
    session.is_stateless = true;
    if let Some(info) = request.client_info {
        session.client_info = Some(info);
    }
    if let Some(info) = request.server_info {
        session.server_info = Some(info);
    }
    Ok(session)
}

/// Checks an `MCP-Protocol-Version` header against the negotiated version.
///
/// Three outcomes, and the middle one is the point:
///
/// - Header agrees — `Ok`.
/// - Header disagrees — [`NegotiationError::HeaderMismatch`]. Rejected, not
///   reconciled.
/// - Header absent — `Ok`, and the caller decides what to assume via
///   [`assumed_version_without_signal`]. Absence is not disagreement.
///
/// The header only became mandatory at `2025-06-18` (PR #548). Before that a
/// Streamable HTTP request carried no version, so requiring one would reject
/// peers the specification had not yet required anything of.
///
/// # Errors
///
/// [`NegotiationError::HeaderMismatch`] when the header names a different
/// revision than the session negotiated.
///
/// [`NegotiationError::HeaderNotAVersion`] when the header is not a version
/// this build can parse. That is distinct from a mismatch: a malformed header
/// is a client that is broken, not one that disagrees.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::{ProtocolVersion, check_protocol_header};
///
/// let negotiated = ProtocolVersion::V2025_06_18;
/// assert!(check_protocol_header(negotiated, Some("2025-06-18")).is_ok());
/// assert!(check_protocol_header(negotiated, Some("2024-11-05")).is_err());
/// assert!(check_protocol_header(negotiated, None).is_ok());
/// ```
pub fn check_protocol_header(
    negotiated: ProtocolVersion,
    header: Option<&str>,
) -> Result<(), NegotiationError> {
    let Some(header) = header else {
        return Ok(());
    };
    let stated = header.trim().parse::<ProtocolVersion>().map_err(|_| {
        NegotiationError::HeaderNotAVersion {
            header: header.to_owned(),
        }
    })?;
    if stated == negotiated {
        return Ok(());
    }
    Err(NegotiationError::HeaderMismatch {
        negotiated,
        header: header.to_owned(),
    })
}

/// The version a request is assumed to be, given whatever signals it carries.
///
/// `signal` is the session's negotiated version, or the `_meta` version on a
/// stateless request. `header` is the HTTP header. The session wins: a header
/// that disagreed with the session would already have been rejected by
/// [`check_protocol_header`], and a session is a stronger statement of intent
/// than a header.
///
/// With neither, the request is assumed to be
/// [`assumed_version_without_signal`].
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::{ProtocolVersion, assumed_version};
///
/// // Nothing to go on at all.
/// assert_eq!(assumed_version(None, None), ProtocolVersion::V2025_03_26);
/// // A header beats the fallback.
/// assert_eq!(
///     assumed_version(Some("2025-11-25"), None),
///     ProtocolVersion::V2025_11_25,
/// );
/// ```
#[must_use]
pub fn assumed_version(header: Option<&str>, signal: Option<ProtocolVersion>) -> ProtocolVersion {
    if let Some(signal) = signal {
        return signal;
    }
    if let Some(version) = header.and_then(|h| h.trim().parse::<ProtocolVersion>().ok()) {
        return version;
    }
    assumed_version_without_signal()
}

/// `supported` rendered for an error message.
fn format_versions(supported: &SupportedVersions) -> String {
    let mut out = String::new();
    for (index, version) in supported.as_slice().iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        out.push_str(&version.to_string());
    }
    out
}

/// The capability names the stateless `_meta` is allowed to carry.
///
/// `2026-07-28` defines exactly two reserved keys, and the names are the whole
/// contract — a peer that misspells `protocolVersion` gets no version, which
/// is a diagnosable failure rather than a silent default.
pub mod stateless_meta_keys {
    /// `io.modelcontextprotocol/protocolVersion`.
    pub const PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
    /// `io.modelcontextprotocol/clientCapabilities`.
    pub const CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
    /// `io.modelcontextprotocol/clientInfo`.
    pub const CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";
    /// `io.modelcontextprotocol/serverInfo`.
    pub const SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";
}

/// Whether a `_meta` key is one the stateless lifecycle reserves.
#[must_use]
pub fn is_reserved_meta_key(key: &str) -> bool {
    matches!(
        key,
        stateless_meta_keys::PROTOCOL_VERSION
            | stateless_meta_keys::CLIENT_CAPABILITIES
            | stateless_meta_keys::CLIENT_INFO
            | stateless_meta_keys::SERVER_INFO
    )
}

#[cfg(test)]
mod tests {
    // Test code unwraps deliberately: the strict profile exists to keep
    // production paths fallible, and a test that cannot unwrap cannot assert.
    #![expect(
        clippy::panic,
        clippy::unwrap_used,
        reason = "tests assert on fallible calls and panic with the failing precondition; propagating errors would replace a clear failure message with noise"
    )]

    use super::{
        NegotiatedSession, NegotiationError, ServerDecision, StatelessRequest, assumed_version,
        assumed_version_without_signal, check_protocol_header, is_reserved_meta_key,
        negotiate_client, negotiate_server, negotiate_stateless, stateless_meta_keys,
    };
    use crate::domain::{
        Capability, CapabilitySet, Feature, ProtocolVersion, SupportedVersions, TransportKind,
    };

    fn all() -> SupportedVersions {
        SupportedVersions::all().unwrap()
    }

    fn subset(versions: &[ProtocolVersion]) -> SupportedVersions {
        SupportedVersions::new(versions.to_vec()).unwrap()
    }

    /// The rmcp #852 regression, by name.
    ///
    /// rmcp answered every client with `2024-11-05` regardless of what was
    /// asked for. A server that does that forces a modern client onto a
    /// year-old profile, and the client's features silently vanish. This is the
    /// test that fails if someone "simplifies" `negotiate_server` to always
    /// return the earliest or the latest.
    #[test]
    fn a_supported_request_is_echoed_not_replaced() {
        let supported = all();
        // Every revision the server speaks comes back as itself — including the
        // newest, which an "always answer latest" implementation gets right by
        // accident, and the oldest, which an "always answer oldest" one gets
        // right by accident. Neither would survive the middle rows.
        for version in ProtocolVersion::KNOWN {
            assert_eq!(
                negotiate_server(version, &supported),
                ServerDecision::Echoed(version),
                "{version} was not echoed",
            );
        }
    }

    /// The same property, from the other end: the answer is never a revision
    /// the client did not ask for *unless* the client asked for something we
    /// cannot speak.
    #[test]
    fn the_answer_is_always_one_the_server_speaks() {
        let supported = all();
        for offered in ProtocolVersion::KNOWN {
            let answer = negotiate_server(offered, &supported).version();
            assert!(supported.contains(&answer), "{offered} -> {answer}");
        }
        let future = ProtocolVersion::unknown_after_known("2099-01-01".parse().unwrap()).unwrap();
        assert_eq!(
            negotiate_server(future, &supported),
            ServerDecision::CounterOffered {
                offered: future,
                // Not `2026-07-28`: SEP-2575 removed `initialize` from that
                // revision, so counter-offering it would send the peer to a
                // protocol on which the handshake it just attempted does not
                // exist.
                answer: ProtocolVersion::V2025_11_25,
            },
        );
    }

    /// Scenario: echo-supported.
    ///
    /// A `2025-03-26` client gets `2025-03-26` back, and a profile that
    /// excludes everything added later.
    #[test]
    fn echo_supported_scenario() {
        let supported = all();
        let decision = negotiate_server(ProtocolVersion::V2025_03_26, &supported);
        assert_eq!(
            decision,
            ServerDecision::Echoed(ProtocolVersion::V2025_03_26)
        );

        let caps = CapabilitySet::all_for(ProtocolVersion::V2025_03_26);
        let session = NegotiatedSession::new(decision.version(), TransportKind::Stdio, caps, caps);
        assert_eq!(session.version, ProtocolVersion::V2025_03_26);
        assert!(!session.allows(Feature::Elicitation), "added 2025-06-18");
        assert!(
            !session.allows(Feature::ToolStructuredOutput),
            "added 2025-06-18"
        );
        assert!(!session.allows(Feature::Tasks), "added 2025-11-25");
        // And it does have what `2025-03-26` defined.
        assert!(session.allows(Feature::ToolAnnotations));
        assert!(session.allows(Feature::Completions));
    }

    /// Scenario: unknown-version. No error, an answer.
    #[test]
    fn unknown_version_scenario() {
        let supported = all();
        for raw in ["2099-01-01", "2030-06-15", "not-a-date-at-all"] {
            let offered = if raw == "not-a-date-at-all" {
                ProtocolVersion::unknown_after_known("2027-12-31".parse().unwrap()).unwrap()
            } else {
                ProtocolVersion::unknown_after_known(raw.parse().unwrap()).unwrap()
            };
            let decision = negotiate_server(offered, &supported);
            assert!(decision.is_counter_offer(), "{raw} should counter-offer");
            assert_eq!(decision.version(), ProtocolVersion::V2025_11_25);
        }
    }

    /// Scenario: stateless-discover. A `2026-07-28` request with no prior
    /// initialize gets a per-request profile, and nothing is retained.
    #[test]
    fn stateless_discover_scenario() {
        let version = ProtocolVersion::V2026_07_28;
        let client_caps = CapabilitySet::all_for(version);
        let server_caps = CapabilitySet::all_for(version);

        let session = negotiate_stateless(
            &all(),
            server_caps,
            TransportKind::Stdio,
            StatelessRequest::empty()
                .with_version(version)
                .with_client_caps(client_caps),
        )
        .unwrap();

        assert!(session.is_stateless, "2026-07-28 is stateless");
        assert_eq!(session.version, version);
        assert!(session.allows(Feature::ServerDiscover));
        assert!(session.allows(Feature::Tools));
        // The stateless lifecycle's own feature is on.
        assert!(session.allows(Feature::StatelessLifecycle));
    }

    /// Two stateless requests are independent. Nothing is retained, so the
    /// second one's profile is a function of the second request alone.
    #[test]
    fn stateless_requests_do_not_bleed_into_each_other() {
        let server_caps = CapabilitySet::all();
        let newer = negotiate_stateless(
            &all(),
            server_caps,
            TransportKind::Stdio,
            StatelessRequest::empty()
                .with_version(ProtocolVersion::V2026_07_28)
                .with_client_caps(CapabilitySet::all_for(ProtocolVersion::V2026_07_28)),
        )
        .unwrap();
        let older = negotiate_stateless(
            &all(),
            server_caps,
            TransportKind::Stdio,
            StatelessRequest::empty()
                .with_version(ProtocolVersion::V2024_11_05)
                .with_client_caps(CapabilitySet::all_for(ProtocolVersion::V2024_11_05)),
        )
        .unwrap();
        assert!(newer.allows(Feature::ServerDiscover));
        assert!(!older.allows(Feature::ServerDiscover));
        assert!(newer.len() > older.len());
    }

    #[test]
    fn a_stateless_request_with_no_version_is_rejected() {
        let error = negotiate_stateless(
            &all(),
            CapabilitySet::all(),
            TransportKind::Stdio,
            StatelessRequest::empty(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            NegotiationError::MissingStatelessVersion { .. }
        ));
    }

    /// A stateless request carrying a newer revision's capabilities cannot
    /// switch on a feature the agreed revision cannot express.
    #[test]
    fn stateless_caps_are_restricted_to_the_agreed_revision() {
        let session = negotiate_stateless(
            &all(),
            CapabilitySet::all(),
            TransportKind::Stdio,
            StatelessRequest::empty()
                .with_version(ProtocolVersion::V2024_11_05)
                .with_client_caps(CapabilitySet::all_for(ProtocolVersion::V2026_07_28)),
        )
        .unwrap();
        assert!(!session.client_caps.contains(Capability::Elicitation));
        assert!(!session.client_caps.contains(Capability::Extensions));
        assert!(session.client_caps.contains(Capability::Sampling));
        assert!(!session.allows(Feature::Elicitation));
    }

    /// The full (offered x supported) table, as the task asks.
    #[test]
    fn the_whole_offered_by_supported_table() {
        let future = ProtocolVersion::unknown_after_known("2099-01-01".parse().unwrap()).unwrap();
        let offered_by =
            |versions: &[ProtocolVersion]| -> Vec<ProtocolVersion> { versions.to_vec() };
        let _ = offered_by;

        for supported_slice in [
            vec![ProtocolVersion::V2024_11_05],
            vec![ProtocolVersion::V2025_03_26, ProtocolVersion::V2025_06_18],
            vec![ProtocolVersion::V2025_11_25, ProtocolVersion::V2026_07_28],
            ProtocolVersion::KNOWN.to_vec(),
        ] {
            let supported = subset(&supported_slice);
            let mut offered = ProtocolVersion::KNOWN.to_vec();
            offered.push(future);

            for offered_version in offered {
                let decision = negotiate_server(offered_version, &supported);
                let expected_echo = supported.contains(&offered_version);
                assert_eq!(
                    decision.is_counter_offer(),
                    !expected_echo,
                    "{offered_version} against {supported_slice:?}",
                );
                // Whatever happened, the answer is the offered version or the
                // server's latest, and always something the server speaks.
                let answer = decision.version();
                assert!(supported.contains(&answer), "{offered_version} -> {answer}");
                if !expected_echo {
                    assert_eq!(answer, supported.latest_handshake());
                }
            }
        }
    }

    #[test]
    fn the_client_accepts_a_version_it_speaks() {
        let supported = all();
        for answered in ProtocolVersion::KNOWN {
            let accepted = negotiate_client(ProtocolVersion::V2026_07_28, answered, &supported);
            assert_eq!(accepted.unwrap(), answered);
        }
    }

    #[test]
    fn the_client_rejects_a_version_it_does_not_speak() {
        // A server that only speaks 2024 cannot be talked to by a client that
        // only speaks 2026.
        let client = subset(&[ProtocolVersion::V2026_07_28]);
        let error = negotiate_client(
            ProtocolVersion::V2026_07_28,
            ProtocolVersion::V2024_11_05,
            &client,
        )
        .unwrap_err();
        match error {
            NegotiationError::UnsupportedServerVersion { answered, .. } => {
                assert_eq!(answered, ProtocolVersion::V2024_11_05);
            }
            other => panic!("expected UnsupportedServerVersion, got {other:?}"),
        }
        // And the error names what the client can speak, which is the only
        // thing the operator can act on.
        let rendered = negotiate_client(
            ProtocolVersion::V2026_07_28,
            ProtocolVersion::V2024_11_05,
            &client,
        )
        .unwrap_err()
        .to_string();
        assert!(rendered.contains("2026-07-28"), "{rendered}");
    }

    /// A future version answered by a server is a rejection, because the client
    /// cannot parse it — the mirror image of the server's counter-offer.
    #[test]
    fn a_client_cannot_accept_a_future_answer() {
        let future = ProtocolVersion::unknown_after_known("2099-01-01".parse().unwrap()).unwrap();
        assert!(negotiate_client(ProtocolVersion::V2026_07_28, future, &all()).is_err());
    }

    #[test]
    fn the_header_is_checked_not_reconciled() {
        let negotiated = ProtocolVersion::V2025_06_18;
        assert!(check_protocol_header(negotiated, Some("2025-06-18")).is_ok());
        // Surrounding whitespace is a transport detail, not a disagreement.
        assert!(check_protocol_header(negotiated, Some("  2025-06-18  ")).is_ok());

        for wrong in ["2024-11-05", "2026-07-28", "2025-03-26"] {
            assert!(
                check_protocol_header(negotiated, Some(wrong)).is_err(),
                "{wrong} should not be accepted"
            );
        }
    }

    #[test]
    fn a_missing_header_is_not_a_disagreement() {
        let negotiated = ProtocolVersion::V2025_06_18;
        assert!(check_protocol_header(negotiated, None).is_ok());
    }

    #[test]
    fn a_malformed_header_is_distinct_from_a_mismatch() {
        let error =
            check_protocol_header(ProtocolVersion::V2025_06_18, Some("banana")).unwrap_err();
        assert!(
            matches!(error, NegotiationError::HeaderNotAVersion { .. }),
            "{error:?}"
        );
        let error =
            check_protocol_header(ProtocolVersion::V2025_06_18, Some("DRAFT-2025-v3")).unwrap_err();
        assert!(
            matches!(error, NegotiationError::HeaderNotAVersion { .. }),
            "{error:?}",
        );
    }

    /// The fallback is `2025-03-26`, not the oldest revision. Getting this
    /// wrong is rmcp #852 in a different disguise.
    #[test]
    fn the_fallback_is_not_the_oldest_revision() {
        assert_eq!(
            assumed_version_without_signal(),
            ProtocolVersion::V2025_03_26
        );
        assert_ne!(
            assumed_version_without_signal(),
            ProtocolVersion::earliest_known()
        );
        assert_eq!(assumed_version(None, None), ProtocolVersion::V2025_03_26);
    }

    #[test]
    fn a_signal_beats_the_header_and_both_beat_the_fallback() {
        assert_eq!(
            assumed_version(Some("2024-11-05"), Some(ProtocolVersion::V2026_07_28)),
            ProtocolVersion::V2026_07_28,
        );
        assert_eq!(
            assumed_version(Some("2025-11-25"), None),
            ProtocolVersion::V2025_11_25,
        );
        // A header that does not parse falls through to the fallback rather
        // than propagating a parse failure: the caller has already been told
        // about the malformed header by `check_protocol_header`.
        assert_eq!(
            assumed_version(Some("garbage"), None),
            ProtocolVersion::V2025_03_26
        );
        // A future version in the header is still a version.
        let future = ProtocolVersion::unknown_after_known("2099-01-01".parse().unwrap()).unwrap();
        assert_eq!(assumed_version(Some("2099-01-01"), None), future);
    }

    /// `NegotiatedSession::is_stateless` tracks the lifecycle, not the caller.
    #[test]
    fn statelessness_follows_the_revision() {
        for version in ProtocolVersion::KNOWN {
            let session = NegotiatedSession::new(
                version,
                TransportKind::Stdio,
                CapabilitySet::all(),
                CapabilitySet::all(),
            );
            assert_eq!(
                session.is_stateless,
                version >= ProtocolVersion::V2026_07_28,
                "{version}"
            );
        }
    }

    #[test]
    fn the_reserved_meta_keys_are_the_documented_ones() {
        for key in [
            stateless_meta_keys::PROTOCOL_VERSION,
            stateless_meta_keys::CLIENT_CAPABILITIES,
            stateless_meta_keys::CLIENT_INFO,
            stateless_meta_keys::SERVER_INFO,
        ] {
            assert!(is_reserved_meta_key(key), "{key}");
        }
        assert!(!is_reserved_meta_key(
            "io.modelcontextprotocol/subscriptionId"
        ));
        assert!(!is_reserved_meta_key("progressToken"));
        // The spellings are the contract, so they are asserted literally.
        assert_eq!(
            stateless_meta_keys::PROTOCOL_VERSION,
            "io.modelcontextprotocol/protocolVersion",
        );
    }

    #[test]
    fn identity_is_carried_but_not_required() {
        let session = NegotiatedSession::new(
            ProtocolVersion::V2025_06_18,
            TransportKind::Stdio,
            CapabilitySet::all(),
            CapabilitySet::all(),
        );
        assert!(session.client_info.is_none());
        let profile = session.profile;
        let named = session
            .with_client_info(super::PeerInfo::new("test-client", "1.0"))
            .with_server_info(super::PeerInfo::new("protocol-droid", "0.1"));
        assert_eq!(named.client_info.as_ref().unwrap().name, "test-client");
        assert_eq!(named.server_info.as_ref().unwrap().name, "protocol-droid");
        // Identity does not change what the session may do.
        assert_eq!(named.profile, profile);
    }
}
