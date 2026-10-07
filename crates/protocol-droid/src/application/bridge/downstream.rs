//! The downstream leg: what a bridge promises a client it can do.
//!
//! A bridge is a server to the client in front of it and a client to the
//! server behind it. The upstream leg is negotiated by the caller before this
//! runs — it has to dial out, run a handshake, and hold a live connection. The
//! downstream leg arrives as a frame on a socket the caller is already reading,
//! and this is where it is settled.
//!
//! # Why the advertisement is not `CapabilitySet::all_for(version)`
//!
//! A server answers `initialize` with everything it can do at the negotiated
//! revision. A bridge must not. Behind the bridge is a specific upstream
//! session with a specific feature profile, and the client is being told what
//! *this bridge, right now, in front of that server* can do. Advertising
//! everything the revision defines would tell a client it could read resources
//! when the server behind the bridge has none, and the failure would surface
//! much later as a request the bridge answers `-32601` to — after the client
//! had every reason to believe the feature was there.
//!
//! So the bridge's own server capabilities are the upstream's advertised set,
//! and the client's offer narrows that further. What survives both is what
//! [`Agreement::served`] reports and what `InitializeResult.capabilities`
//! carries, which makes the two the same number computed once rather than two
//! numbers that can drift.
//!
//! # Why the bridge names itself
//!
//! `serverInfo` is the bridge's, not the upstream's. Passing the upstream's
//! through would tell a client it is talking to the process it named when the
//! peer on the other end of the socket is a proxy — and a client that
//! deliberately dials a bridge is making a routing decision that depends on
//! knowing that.

use crate::application::mcp::initialize::{read_offer, server_capabilities_json};
use crate::domain::ir::{JsonObject, JsonObjectError};
use crate::domain::{
    Agreement, FeatureProfile, Leg, Negotiation, ProtocolVersion, SupportedVersions, TransportKind,
    handshake_defined_in, negotiate_server, resolve,
};

/// What this bridge presents to a client, before any client has connected.
///
/// Fixed for the life of the deployment, unlike the negotiation itself: which
/// revisions are acceptable, which transport the client arrived over, and what
/// this bridge calls itself are properties of the running process, not of any
/// one session. Bundling them here is what lets [`negotiate`] take the one
/// thing that varies per connection and nothing else.
///
/// # Examples
///
/// ```
/// use protocol_droid::application::bridge::{DownstreamConfig, DownstreamError};
/// use protocol_droid::domain::TransportKind;
///
/// let config = DownstreamConfig::new("bridge", "0.1.0", TransportKind::Stdio)?;
/// assert_eq!(config.transport(), TransportKind::Stdio);
/// # Ok::<(), DownstreamError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownstreamConfig {
    supported: SupportedVersions,
    transport: TransportKind,
    identity: Identity,
}

impl DownstreamConfig {
    /// A config accepting every revision this build speaks.
    ///
    /// # Errors
    ///
    /// [`DownstreamError::UnnegotiableVersion`] if the build has no revisions
    /// at all. The list is a build-time constant and the specification has
    /// defined at least one since 2024, so this is a formality rather than a
    /// branch — but a caller building a config at startup deserves a real error
    /// rather than a panic further in.
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        transport: TransportKind,
    ) -> Result<Self, DownstreamError> {
        let supported =
            SupportedVersions::all().map_err(|_| DownstreamError::UnnegotiableVersion)?;
        Ok(Self::with(
            supported,
            Identity::new(name, version),
            transport,
        ))
    }

    /// A config restricted to `supported`, naming itself `identity`.
    ///
    /// For a bridge deliberately held to fewer revisions than the build
    /// supports — an operator who knows a client is broken at a revision and
    /// would rather see a counter-offer than a mis-negotiation.
    #[must_use]
    pub fn with(
        supported: SupportedVersions,
        identity: Identity,
        transport: TransportKind,
    ) -> Self {
        Self {
            supported,
            transport,
            identity,
        }
    }

    /// The transport a client reaches this bridge over.
    ///
    /// Part of the negotiation and not an incidental detail: the same feature
    /// set can resolve differently over stdio than over HTTP, because the
    /// transport gates features of its own.
    #[must_use]
    pub const fn transport(&self) -> TransportKind {
        self.transport
    }

    /// The revisions this bridge will accept downstream.
    #[must_use]
    pub const fn supported(&self) -> &SupportedVersions {
        &self.supported
    }

    /// What this bridge calls itself.
    #[must_use]
    pub const fn identity(&self) -> &Identity {
        &self.identity
    }
}

/// What this bridge calls itself in `serverInfo`.
///
/// A value object rather than two loose `&str` parameters so the two fields
/// stay together — they are the two members of one wire object, and a signature
/// that let a caller interleave two unrelated strings was one argument-swap
/// away from answering with somebody else's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    name: String,
    version: String,
}

impl Identity {
    /// A name and a version, both free-form on the wire.
    #[must_use]
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
        }
    }

    /// The bridge's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The bridge's version.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }
}

/// A settled downstream leg: what to answer, and what the bridge may forward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Downstream {
    leg: Leg,
    agreement: Agreement,
    body: JsonObject,
}

impl Downstream {
    /// The client's side of the bridge, on its own.
    ///
    /// The [`Agreement`] is the useful form for forwarding; this is the one a
    /// report wants, because it is the two numbers that went into the
    /// reconciliation without everything downstream of them.
    #[must_use]
    pub const fn leg(&self) -> &Leg {
        &self.leg
    }

    /// Both sides reconciled: what this bridge can serve, and what it cannot.
    #[must_use]
    pub const fn agreement(&self) -> &Agreement {
        &self.agreement
    }

    /// The `InitializeResult` to send back.
    ///
    /// Its `capabilities` are [`Agreement::served`] rendered through the
    /// writer, so the client is told exactly what the bridge will forward.
    #[must_use]
    pub const fn body(&self) -> &JsonObject {
        &self.body
    }

    /// What this bridge will serve for this client.
    #[must_use]
    pub const fn served(&self) -> FeatureProfile {
        self.agreement.served()
    }
}

/// Why a downstream handshake could not be settled.
///
/// Distinct from an upstream that cannot be reached: the upstream failed before
/// [`negotiate`] was ever called, and this type is only ever about what the
/// client in front of us said.
#[derive(Debug, thiserror::Error)]
pub enum DownstreamError {
    /// The `initialize` request was not readable as an offer.
    ///
    /// Collapsed to one variant for the same reason
    /// [`InitializeError`](crate::application::mcp::initialize::InitializeError)
    /// is: the caller answers `-32600` either way, and a proxy reporting which
    /// of six shapes failed would be telling the client about the proxy's
    /// internals.
    #[error("the initialize request was not readable as an offer")]
    Malformed,

    /// The negotiation settled on a revision with no handshake.
    ///
    /// SEP-2575 removed `initialize` from `2026-07-28`, and
    /// [`negotiate_server`] answers only with revisions this build supports —
    /// so this cannot happen on the path below. It is in the signature because
    /// a revision can be added to [`SupportedVersions`] without anyone
    /// re-reading this module, and a bridge that sent an `InitializeResult` for
    /// a revision defining none would be inventing wire format.
    #[error("{0} has no initialize handshake to answer")]
    NoHandshake(ProtocolVersion),

    /// This build has no revisions to negotiate over at all.
    #[error("this build supports no revisions that define a handshake")]
    UnnegotiableVersion,

    /// The reply could not be assembled into an object.
    ///
    /// Cannot happen for the body built below, which is a literal object of
    /// string members — but `from_value` is fallible and the honest signature
    /// is the one that says so. Inventing a second error for it would be a
    /// promise this function cannot keep.
    #[error("the initialize result could not be assembled: {0}")]
    Assemble(#[from] JsonObjectError),
}

/// Settles the downstream leg against an already-negotiated upstream.
///
/// Reads the client's offer with the same code a server uses, so a bridge and
/// a server cannot disagree about what a client is allowed to say, then
/// reconciles the two legs through [`Agreement::between`].
///
/// The two things returned are not two views of one value. The
/// [`Agreement`] is what the bridge may forward; the body is the wire form of
/// the agreement. A caller that forwarded against the downstream [`Leg`] alone
/// would be forwarding features it had just told the client it does not have.
///
/// # Errors
///
/// [`DownstreamError::Malformed`] for an `initialize` that is not an offer, and
/// [`DownstreamError::NoHandshake`] if the negotiated revision defines no
/// `InitializeResult` to answer with.
///
/// # Examples
///
/// ```
/// use protocol_droid::application::bridge::{DownstreamConfig, negotiate_downstream as negotiate};
/// use protocol_droid::domain::ir::JsonObject;
/// use protocol_droid::domain::{Agreement, TransportKind};
///
/// # fn example(upstream: &Agreement) -> Result<(), Box<dyn std::error::Error>> {
/// let config = DownstreamConfig::new("bridge", "0.1.0", TransportKind::Stdio)?;
/// let params = JsonObject::new(
///     r#"{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"probe"}}"#,
/// )?;
///
/// let downstream = negotiate(&params, upstream, &config)?;
///
/// // The advertisement is the agreement, never the client's own leg.
/// assert_eq!(downstream.served(), downstream.agreement().served());
/// # let _reply = downstream.body().as_str();
/// # Ok(())
/// # }
/// ```
pub fn negotiate(
    params: &JsonObject,
    upstream: &Agreement,
    config: &DownstreamConfig,
) -> Result<Downstream, DownstreamError> {
    let offer = read_offer(params).map_err(|_| DownstreamError::Malformed)?;
    let version = negotiate_server(offer.version, &config.supported).version();

    if !handshake_defined_in(version) {
        return Err(DownstreamError::NoHandshake(version));
    }

    // The bridge's own capabilities are the upstream's, not this build's. See
    // the module docs: advertising everything the revision defines is how a
    // bridge tells a client it can read resources from a server that has none.
    let profile = resolve(Negotiation {
        version,
        client_caps: offer.capabilities,
        server_caps: upstream.upstream().profile.advertised_by_server(),
        transport: config.transport,
    });

    let leg = Leg::new(version, profile);
    let agreement = Agreement::between(leg, *upstream.upstream());

    let body = serde_json::json!({
        "protocolVersion": version.as_wire(),
        "capabilities": server_capabilities_json(agreement.served()),
        "serverInfo": {
            "name": config.identity.name(),
            "version": config.identity.version(),
        },
    });

    Ok(Downstream {
        leg,
        agreement,
        body: JsonObject::from_value(body)?,
    })
}

#[cfg(test)]
mod tests {
    use super::{DownstreamConfig, DownstreamError, Identity, negotiate};
    use crate::domain::ir::JsonObject;
    use crate::domain::{
        Agreement, Capability, CapabilitySet, Feature, Leg, Negotiation, ProtocolVersion,
        TransportKind, resolve,
    };

    /// What a test here can fail with.
    #[derive(Debug)]
    enum Failure {
        /// The fixture was not a JSON object.
        Fixture(crate::domain::ir::JsonObjectError),
        /// The test's own complaint.
        Message(String),
    }

    impl core::fmt::Display for Failure {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::Fixture(error) => write!(f, "bad fixture: {error}"),
                Self::Message(text) => f.write_str(text),
            }
        }
    }

    impl std::error::Error for Failure {}

    impl From<crate::domain::ir::JsonObjectError> for Failure {
        fn from(error: crate::domain::ir::JsonObjectError) -> Self {
            Self::Fixture(error)
        }
    }

    impl From<String> for Failure {
        fn from(text: String) -> Self {
            Self::Message(text)
        }
    }

    impl From<DownstreamError> for Failure {
        fn from(error: DownstreamError) -> Self {
            Self::Message(format!("the downstream leg was refused: {error}"))
        }
    }

    fn params(text: &str) -> Result<JsonObject, Failure> {
        JsonObject::new(text).map_err(Failure::from)
    }

    /// An upstream that agreed everything at `version`.
    fn upstream_agreeing(version: ProtocolVersion, server_caps: CapabilitySet) -> Agreement {
        Agreement::between(
            Leg::new(
                version,
                resolve(Negotiation {
                    version,
                    client_caps: CapabilitySet::all_for(version),
                    server_caps: CapabilitySet::all_for(version),
                    transport: TransportKind::StreamableHttp,
                }),
            ),
            Leg::new(
                version,
                resolve(Negotiation {
                    version,
                    client_caps: CapabilitySet::all_for(version),
                    server_caps,
                    transport: TransportKind::StreamableHttp,
                }),
            ),
        )
    }

    fn config() -> Result<DownstreamConfig, Failure> {
        DownstreamConfig::new("bridge", "0.1.0", TransportKind::Stdio)
            .map_err(|e| Failure::Message(format!("a stdio bridge could not be configured: {e}")))
    }

    /// The whole point of the module: what the upstream can do and what the
    /// client asked for, and nothing else.
    ///
    /// A bridge that advertised `all_for(version)` here would pass a test that
    /// only checked the reply parses, and then refuse a `resources/read` the
    /// client had every reason to attempt.
    #[test]
    fn the_advertisement_is_the_intersection_not_the_revision() -> Result<(), Failure> {
        let version = ProtocolVersion::V2025_06_18;
        let upstream = upstream_agreeing(version, CapabilitySet::empty().with(Capability::Tools));
        let body = params(
            r#"{"protocolVersion":"2025-06-18","capabilities":{"roots":{}},"clientInfo":{"name":"p"}}"#,
        )?;

        let downstream = negotiate(&body, &upstream, &config()?)?;

        assert!(
            downstream.agreement().serves(Feature::Tools),
            "the one feature the upstream offers vanished"
        );
        assert!(
            !downstream.agreement().serves(Feature::Resources),
            "the bridge advertised a feature the upstream never agreed to"
        );
        let advertised = downstream.body().as_str();
        assert!(
            !advertised.contains("\"resources\""),
            "the reply contradicts the agreement: {advertised}"
        );
        Ok(())
    }

    /// The client's own capability set narrows it further. A client with no
    /// `roots` cannot be served roots even though the upstream has them, so
    /// advertising them would be a promise the bridge has no path to keep.
    #[test]
    fn the_clients_offer_narrows_it_too() -> Result<(), Failure> {
        let version = ProtocolVersion::V2025_06_18;
        let upstream = upstream_agreeing(version, CapabilitySet::all_for(version));
        let body = params(
            r#"{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"p"}}"#,
        )?;

        let downstream = negotiate(&body, &upstream, &config()?)?;

        assert!(
            !downstream.agreement().serves(Feature::Roots),
            "a client that never offered roots was promised them"
        );
        Ok(())
    }

    /// The bridge names itself, and says so in the reply.
    ///
    /// A client that deliberately dials a bridge is making a routing decision,
    /// and passing the upstream's `serverInfo` through would tell it it is
    /// talking to the process it named.
    #[test]
    fn the_reply_carries_the_bridges_own_name() -> Result<(), Failure> {
        let version = ProtocolVersion::V2025_06_18;
        let upstream = upstream_agreeing(version, CapabilitySet::all_for(version));
        let body = params(
            r#"{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"p"}}"#,
        )?;

        let reply = negotiate(&body, &upstream, &config()?)?
            .body()
            .as_str()
            .to_owned();

        assert!(reply.contains(r#""name":"bridge""#), "{reply}");
        assert!(reply.contains(r#""version":"0.1.0""#), "{reply}");
        Ok(())
    }

    /// The revision answered is the negotiated one, never the one the client
    /// asked for, and never the newest this build knows.
    ///
    /// `2025-03-26` is the case: a client offering only that gets that, while a
    /// client offering a revision from the future is walked back to something
    /// both sides speak.
    #[test]
    fn the_reply_names_the_negotiated_revision() -> Result<(), Failure> {
        let version = ProtocolVersion::V2025_06_18;
        let upstream = upstream_agreeing(version, CapabilitySet::all_for(version));

        let older = params(
            r#"{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"p"}}"#,
        )?;
        assert_eq!(
            negotiate(&older, &upstream, &config()?)?.leg().version,
            ProtocolVersion::V2025_03_26
        );

        let future = params(
            r#"{"protocolVersion":"2099-01-01","capabilities":{},"clientInfo":{"name":"p"}}"#,
        )?;
        let walked_back = negotiate(&future, &upstream, &config()?)?;
        assert_ne!(
            walked_back.leg().version,
            ProtocolVersion::latest_known(),
            "the newest revision has no handshake, so it cannot be the answer"
        );
        Ok(())
    }

    /// A malformed offer is refused before anything is promised.
    #[test]
    fn a_malformed_offer_is_refused() -> Result<(), Failure> {
        let version = ProtocolVersion::V2025_06_18;
        let upstream = upstream_agreeing(version, CapabilitySet::all_for(version));
        let body = params(r#"{"capabilities":{},"clientInfo":{"name":"p"}}"#)?;

        assert!(matches!(
            negotiate(&body, &upstream, &config()?),
            Err(DownstreamError::Malformed)
        ));
        Ok(())
    }

    /// A config restricted to one revision counter-offers rather than silently
    /// accepting a client the operator said not to serve.
    #[test]
    fn a_restricted_config_only_answers_with_what_it_supports() -> Result<(), Failure> {
        let supported = crate::domain::SupportedVersions::new([ProtocolVersion::V2025_03_26])
            .map_err(|e| Failure::Message(format!("a one-revision set was refused: {e}")))?;
        let held = DownstreamConfig::with(
            supported,
            Identity::new("bridge", "0.1.0"),
            TransportKind::Stdio,
        );
        let upstream = upstream_agreeing(ProtocolVersion::V2025_03_26, CapabilitySet::empty());
        let body = params(
            r#"{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"p"}}"#,
        )?;

        assert_eq!(
            negotiate(&body, &upstream, &held)?.leg().version,
            ProtocolVersion::V2025_03_26
        );
        Ok(())
    }
}
