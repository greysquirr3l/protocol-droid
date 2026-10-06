//! What each revision's authorization spec asks of the two sides.
//!
//! MCP's authorization model changed three times between `2025-03-26` and
//! `2026-07-28`, and the changes are not additive: `2026-07-28` *deprecates*
//! Dynamic Client Registration rather than extending it. So this is a table of
//! facts per revision rather than a set of feature bits, and
//! [`authorization_model`] is the one place a row is read.
//!
//! Every row below cites the vendored changelog that introduced it, under
//! `crates/protocol-droid-schema/spec/<version>/CHANGELOG.md`. The vendored
//! `schema.json` is not the source for any of them: none of these facts is a
//! message field, so none of them appears in a schema. They are spec
//! *prose*, and a row with no citation is a row nobody checked.
//!
//! # Why this is in `domain/`
//!
//! Every value here is a pure function of the negotiated revision. Which
//! client registration mechanism to try, whether a `WWW-Authenticate` header
//! should carry `resource_metadata`, and whether a presented token must be
//! audience-checked are all decidable before any socket exists — and all of
//! them are security decisions, so they belong where a deployment cannot
//! override them with an adapter's opinion.

use crate::domain::ProtocolVersion;

/// How a client is expected to register with an authorization server.
///
/// Not a bitmask because the three states are a progression, not a set: a
/// client that treats `2026-07-28` as "both work" would send a Dynamic Client
/// Registration request to a server that has been told to deprecate it, and a
/// server that rejects it looks like a broken server rather than a wrong client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum ClientRegistration {
    /// The revision specifies no authorization framework at all.
    ///
    /// `2024-11-05`. Its vendored changelog has no authorization entry, so a
    /// client that registers against an OAuth server is doing something the
    /// revision never asked for.
    #[default]
    None,
    /// RFC 7591 Dynamic Client Registration.
    ///
    /// `2025-03-26` and `2025-06-18`. The framework was introduced here
    /// (`2025-03-26` changelog §1, PR #133) with no alternative registration
    /// mechanism named, so this is what those revisions' servers implement.
    Dynamic,
    /// Both, with Client ID Metadata Documents *recommended*.
    ///
    /// `2025-11-25` changelog §8 (SEP-991, PR #1296) adds Client ID Metadata
    /// Documents as a recommended registration mechanism and does not deprecate
    /// Dynamic Client Registration, so a client may use either. Prefer
    /// [`Self::preferred`] over "either" when choosing.
    DynamicOrMetadataDocument,
    /// Client ID Metadata Documents; Dynamic Client Registration deprecated.
    ///
    /// `2026-07-28` changelog §4 (PR #2858) deprecates RFC 7591 "in favor of"
    /// Client ID Metadata Documents, keeping it only "for backwards
    /// compatibility with authorization servers that do not support" them. So
    /// this is a preference with a fallback, not a removal — see
    /// [`Self::supports_dynamic`].
    MetadataDocument,
}

impl ClientRegistration {
    /// Whether a client may register through RFC 7591.
    ///
    /// `true` even for [`Self::MetadataDocument`]: the deprecation is a
    /// statement about what the spec prefers, and the spec keeps the mechanism
    /// for servers that do not implement the replacement. A client that
    /// refused outright would fail against those servers, and a server that
    /// implements only RFC 7591 is not misconfigured.
    #[must_use]
    pub const fn supports_dynamic(self) -> bool {
        matches!(
            self,
            Self::Dynamic | Self::DynamicOrMetadataDocument | Self::MetadataDocument
        )
    }

    /// Whether a client may register with a Client ID Metadata Document.
    #[must_use]
    pub const fn supports_metadata_document(self) -> bool {
        matches!(
            self,
            Self::DynamicOrMetadataDocument | Self::MetadataDocument
        )
    }

    /// The mechanism to try first.
    ///
    /// Metadata documents wherever they exist, because a client that leads with
    /// the mechanism its own revision deprecates is doing the wrong thing
    /// against a server that supports both. Falls back to dynamic registration
    /// only where documents are not defined, so this never returns
    /// [`Self::None`] for a revision that can register at all.
    #[must_use]
    pub const fn preferred(self) -> Option<Self> {
        match self {
            Self::None => None,
            Self::Dynamic => Some(Self::Dynamic),
            Self::DynamicOrMetadataDocument | Self::MetadataDocument => {
                Some(Self::MetadataDocument)
            }
        }
    }

    /// What to try after `self` did not work, or `None` at the end of the list.
    ///
    /// The second half of [`Self::preferred`]: a client has to be able to fall
    /// back, and [`Self::MetadataDocument`] is precisely the revision whose
    /// servers may still implement only RFC 7591.
    #[must_use]
    pub const fn fallback(self) -> Option<Self> {
        match self {
            Self::DynamicOrMetadataDocument | Self::MetadataDocument => Some(Self::Dynamic),
            Self::Dynamic | Self::None => None,
        }
    }

    /// Whether `application_type` must be sent during registration.
    ///
    /// `2026-07-28` changelog §8 (SEP-837): clients "**MUST** specify an
    /// appropriate `application_type` during Dynamic Client Registration to
    /// avoid `OpenID Connect` redirect URI conflicts". A `MUST`, so this is
    /// about revisions that require the field, not about ones that tolerate it.
    #[must_use]
    pub const fn application_type_required(self) -> bool {
        matches!(self, Self::MetadataDocument)
    }
}

/// What a revision's authorization spec requires of the two sides.
///
/// Built by [`authorization_model`] and read by the resource-server and client
/// paths. `Copy` so it can be held on a session without a reference, which
/// matters because every request handler consults it.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::mcp::authorization::{
///     ClientRegistration, authorization_model,
/// };
/// use protocol_droid::domain::ProtocolVersion;
///
/// // 2026-07-28 deprecated RFC 7591 in favour of client id metadata documents.
/// assert_eq!(
///     authorization_model(ProtocolVersion::V2026_07_28).client_registration,
///     ClientRegistration::MetadataDocument,
/// );
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuthorizationModel {
    /// The revision defines an authorization framework.
    ///
    /// `false` only for `2024-11-05`, whose vendored changelog contains no
    /// authorization entry at all. Everything else here is meaningless when this
    /// is `false`, and a caller that has not checked it will advertise a
    /// protected-resource document to a peer whose revision never defined one.
    pub specified: bool,

    /// The server is an OAuth resource server and publishes protected-resource
    /// metadata naming its authorization servers.
    ///
    /// `2025-06-18` changelog §3 (PR #338): "Classify MCP servers as OAuth
    /// Resource Servers, adding protected resource metadata to discover the
    /// corresponding Authorization server." Absent from `2025-03-26`, whose
    /// framework predates the classification.
    pub protected_resource_metadata: bool,

    /// A client **MUST** send the resource indicator (RFC 8707).
    ///
    /// `2025-06-18` changelog §4 (PR #734): "Require MCP clients to implement
    /// Resource Indicators as described in RFC 8707 to prevent malicious
    /// servers from obtaining access tokens." A `MUST` on the client, and the
    /// threat is concrete: without it, a token minted for one server can be
    /// replayed at another.
    pub resource_indicators_required: bool,

    /// Authorization-server discovery may use `OpenID Connect Discovery 1.0`.
    ///
    /// `2025-11-25` changelog §1 (PR #797). An *enhancement* rather than a
    /// replacement — `/.well-known/oauth-authorization-server` still works —
    /// so this gates whether the client *offers* the other form, not which one
    /// it requires.
    pub oidc_discovery: bool,

    /// `WWW-Authenticate` may carry incremental scope consent.
    ///
    /// `2025-11-25` changelog §3 (SEP-835). Lets a server name the exact scope
    /// it wants instead of rejecting wholesale, so a client that does not read
    /// it still works — it just re-prompts for everything.
    pub incremental_scope_consent: bool,

    /// A client **MUST** validate an `iss` parameter when the server sends one.
    ///
    /// `2026-07-28` changelog §7 (SEP-2468): authorization servers "**SHOULD**
    /// include the `iss` parameter in authorization responses per RFC 9207, and
    /// MCP clients **MUST** validate a present `iss` against the recorded
    /// issuer before redeeming the authorization code". So the server's
    /// obligation is a `SHOULD` and the client's is a `MUST` — absent `iss` is
    /// tolerated, present-and-wrong is fatal. Modelled as one row rather than
    /// two because the client's obligation is what code has to implement.
    pub issuer_parameter: bool,

    /// Persisted client credentials are keyed by issuer and never reused across
    /// authorization servers.
    ///
    /// `2026-07-28` changelog §9 (SEP-2352): clients "**MUST** key persisted
    /// credentials by the issuer identifier, **MUST NOT** reuse them with a
    /// different authorization server, and **MUST** re-register when the
    /// authorization server changes". The first clause is why this is a domain
    /// fact and not a storage detail: the key is part of the contract.
    pub credentials_bound_to_issuer: bool,

    /// How a client registers. See [`ClientRegistration`].
    pub client_registration: ClientRegistration,
}

impl AuthorizationModel {
    /// Whether a token presented to this revision's server must be audience-
    /// bound to this server's canonical resource identifier.
    ///
    /// True for every revision with an authorization framework, and `true`
    /// regardless of [`Self::protected_resource_metadata`] — the metadata
    /// document is how a *client* learns the audience, and refusing a
    /// wrong-audience token is a separate rule that does not depend on the
    /// client having done that discovery. `2024-11-05` is the only revision with
    /// no authorization framework at all, and a token check with no audience
    /// concept behind it is not a weaker check, it is a different one that this
    /// crate does not implement.
    #[must_use]
    pub const fn audience_bound(self) -> bool {
        self.specified
    }
}

/// What `version`'s authorization spec requires.
///
/// A pure function of the revision, so it is testable per row without a socket
/// and cannot drift from the revision it was written against.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::mcp::authorization::authorization_model;
/// use protocol_droid::domain::ProtocolVersion;
///
/// // 2024-11-05 never defined an authorization framework.
/// assert!(!authorization_model(ProtocolVersion::V2024_11_05).specified);
///
/// // 2025-03-26 introduced OAuth 2.1 but not the resource-server
/// // classification, so it has no protected-resource metadata.
/// let m = authorization_model(ProtocolVersion::V2025_03_26);
/// assert!(m.specified);
/// assert!(!m.protected_resource_metadata);
/// ```
#[must_use]
pub const fn authorization_model(version: ProtocolVersion) -> AuthorizationModel {
    // Defaulted to the `2024-11-05` row: everything below then *turns on*
    // rather than off, so a revision added to this match without a row here
    // degrades to "no authorization framework" instead of silently inheriting
    // `2026-07-28`'s requirements. That is the right direction for a security
    // field — a revision this build has not read about gets no behaviour it
    // cannot justify, and the failure is visible rather than permissive.
    let mut model = AuthorizationModel {
        specified: false,
        protected_resource_metadata: false,
        resource_indicators_required: false,
        oidc_discovery: false,
        incremental_scope_consent: false,
        issuer_parameter: false,
        credentials_bound_to_issuer: false,
        client_registration: ClientRegistration::None,
    };

    match version {
        // Both of these turn nothing on, and saying so in one arm is the honest
        // shape: `2024-11-05`'s changelog has no authorization entry at all, and
        // an `Unknown` revision's spec is one this build has not read. Written
        // out rather than left to the default so a reader diffing this table
        // against the changelogs can see that the absence is a decision.
        ProtocolVersion::V2024_11_05 | ProtocolVersion::Unknown(_) => {}
        // `2025-03-26` changelog §1 (PR #133): "Added a comprehensive
        // authorization framework based on OAuth 2.1." No resource-server
        // classification (§3) and no resource indicators (§4) — both arrive in
        // `2025-06-18`.
        ProtocolVersion::V2025_03_26 => {
            model.specified = true;
            model.client_registration = ClientRegistration::Dynamic;
        }
        // `2025-06-18` changelog §3 (PR #338) adds protected-resource metadata
        // and §4 (PR #734) makes resource indicators mandatory. §5 (PR #338
        // family) clarifies security considerations but adds no behaviour.
        ProtocolVersion::V2025_06_18 => {
            model.specified = true;
            model.protected_resource_metadata = true;
            model.resource_indicators_required = true;
            model.client_registration = ClientRegistration::Dynamic;
        }
        // `2025-11-25` changelog §1 (PR #797) adds OpenID Connect Discovery,
        // §3 (SEP-835) adds incremental scope consent, and §8 (SEP-991,
        // PR #1296) adds Client ID Metadata Documents as a *recommended*
        // registration mechanism alongside RFC 7591.
        ProtocolVersion::V2025_11_25 => {
            model.specified = true;
            model.protected_resource_metadata = true;
            model.resource_indicators_required = true;
            model.oidc_discovery = true;
            model.incremental_scope_consent = true;
            model.client_registration = ClientRegistration::DynamicOrMetadataDocument;
        }
        // `2026-07-28` changelog §7 (SEP-2468) adds the `iss` requirement,
        // §8 (SEP-837) requires `application_type`, §9 (SEP-2352) binds
        // credentials to the issuer, and §4 (PR #2858) deprecates RFC 7591 in
        // favour of Client ID Metadata Documents. Nothing is withdrawn from
        // `2025-11-25`: every row carries forward.
        ProtocolVersion::V2026_07_28 => {
            model.specified = true;
            model.protected_resource_metadata = true;
            model.resource_indicators_required = true;
            model.oidc_discovery = true;
            model.incremental_scope_consent = true;
            model.issuer_parameter = true;
            model.credentials_bound_to_issuer = true;
            model.client_registration = ClientRegistration::MetadataDocument;
        }
    }

    model
}

#[cfg(test)]
#[path = "authorization/tests.rs"]
mod tests;
