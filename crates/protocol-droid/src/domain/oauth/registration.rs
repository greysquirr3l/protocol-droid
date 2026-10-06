//! Client identity and the credentials that travel with it.
//!
//! Two mechanisms, and they are not the same shape. RFC 7591 Dynamic Client
//! Registration asks an authorization server for an identifier and gets back a
//! secret the client proves possession of. A Client ID Metadata Document
//! inverts it: the client publishes a document at an HTTPS URL and uses that URL
//! as its `client_id`, so there is nothing to register and — the draft says so
//! directly — "there is no way to establish a shared secret to be used with
//! client metadata documents". [`ClientSecret`] therefore cannot be constructed
//! for the document form, rather than being optional and quietly absent.
//!
//! `2025-11-25` §8 (SEP-991) adds the document form alongside registration and
//! recommends it; `2026-07-28` §4 (PR #2858) deprecates registration "in favor
//! of" documents while keeping it for servers that do not support them. Both
//! facts are already in
//! [`ClientRegistration`](crate::domain::mcp::authorization::ClientRegistration);
//! this module is what a client does with the answer.
//!
//! # Why the issuer is part of the identity
//!
//! `2026-07-28` §9 (SEP-2352) requires that clients key persisted credentials by
//! the issuer identifier, never reuse them with a different authorization
//! server, and re-register when it changes. The rule exists because the revision
//! removes the other way to tell two servers apart — `Mcp-Session-Id` is gone
//! (SEP-2567) — so a client that keeps one `client_id` and one secret across
//! every authorization server it ever meets will hand that secret to whichever
//! one it is currently talking to, which is exactly the one that must not get
//! it.
//!
//! Making the issuer part of [`RegisteredClient`]'s identity rather than a
//! lookup key means a credential cannot be retrieved without also asserting
//! who issued it. See [`decide_registration`].

use core::fmt;
use std::sync::Arc;

use crate::domain::mcp::authorization::AuthorizationModel;

/// A client's identifier at its authorization server.
///
/// Two forms, and they are not interchangeable. RFC 7591 hands out an opaque
/// string; a Client ID Metadata Document uses the URL the document is served
/// from. [`verify_metadata_document`](Self::verify_metadata_document) checks
/// the second form's defining property.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ClientId(Arc<str>);

impl ClientId {
    /// An opaque identifier, as RFC 7591's `client_id` response member is.
    #[must_use]
    pub fn opaque(value: impl AsRef<str>) -> Self {
        Self(Arc::from(value.as_ref()))
    }

    /// The `client_id` of a Client ID Metadata Document.
    ///
    /// # Errors
    ///
    /// [`ClientIdError::NotHttps`] if the URL is not `https`. The draft
    /// requires an HTTPS URL, and a `client_id` that is not one is a
    /// configuration mistake worth naming rather than a value to discover at
    /// the authorization server.
    pub fn metadata_url(value: impl AsRef<str>) -> Result<Self, ClientIdError> {
        let value = value.as_ref();
        if value.starts_with("https://") {
            Ok(Self(Arc::from(value)))
        } else {
            Err(ClientIdError::NotHttps)
        }
    }

    /// The identifier as it goes on the wire.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Check that a metadata document vouches for this `client_id`.
    ///
    /// The draft requires the document's `client_id` property to "match the URL
    /// of the document using simple string comparison as defined in [RFC3986]
    /// Section 6.2.1" — the same discipline
    /// [`check_response_issuer`](crate::domain::oauth::check_response_issuer)
    /// applies to RFC 9207's `iss`, and for the same reason. Normalising here
    /// would let a document served at one URL claim the identity of another.
    ///
    /// # Errors
    ///
    /// [`ClientIdError::DocumentMismatch`] when the document's `client_id` is
    /// not this URL by simple string comparison.
    pub fn verify_metadata_document(&self, document_client_id: &str) -> Result<(), ClientIdError> {
        if document_client_id == self.0.as_ref() {
            Ok(())
        } else {
            Err(ClientIdError::DocumentMismatch)
        }
    }
}

impl fmt::Debug for ClientId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("ClientId").field(&self.0).finish()
    }
}

impl fmt::Display for ClientId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A client secret from an RFC 7591 registration response.
///
/// # Redaction
///
/// Renders as `ClientSecret(…)` under [`Debug`], which is what a panic and an
/// `assert_eq!` expand to. It authenticates the client to its authorization
/// server, so it is the credential a confused-deputy leak hands over, and it is
/// not the sort of value that belongs in a build log because a comparison
/// failed. [`Display`](fmt::Display) stays full: an RFC 7591 client
/// authenticates with `client_secret` in a token request body, so the value has
/// to be serialisable.
#[derive(Clone, PartialEq, Eq)]
pub struct ClientSecret(Arc<str>);

impl ClientSecret {
    /// The secret, as it goes in a `client_secret` form field.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ClientSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ClientSecret(…)")
    }
}

/// A client as one authorization server knows it.
///
/// The issuer is part of the identity rather than a field a caller must remember
/// to check, which is the whole point of `2026-07-28` §9. There is no accessor
/// that returns a `ClientSecret` for a given issuer, because asking is how the
/// mistake gets made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredClient {
    issuer: Arc<str>,
    client_id: ClientId,
    secret: Option<ClientSecret>,
}

impl RegisteredClient {
    /// A client that obtained its identifier through RFC 7591.
    ///
    /// # Errors
    ///
    /// [`ClientIdError::DocumentRegistration`] if `client_id` is a metadata
    /// document URL, or [`ClientIdError::SecretForDocument`] if a secret is
    /// offered for one. A document-issued client has no secret to present.
    pub fn dynamic(
        issuer: impl AsRef<str>,
        client_id: ClientId,
        secret: Option<ClientSecret>,
    ) -> Result<Self, ClientIdError> {
        if client_id.0.starts_with("https://") {
            return Err(ClientIdError::DocumentRegistration);
        }
        Ok(Self {
            issuer: Arc::from(issuer.as_ref()),
            client_id,
            secret,
        })
    }

    /// A client identified by a Client ID Metadata Document.
    ///
    /// Never carries a secret: the draft is explicit that no shared secret can
    /// be established for this form, so there is no constructor parameter in
    /// which to get it wrong.
    #[must_use]
    pub fn from_metadata_document(issuer: impl AsRef<str>, client_id: ClientId) -> Self {
        Self {
            issuer: Arc::from(issuer.as_ref()),
            client_id,
            secret: None,
        }
    }

    /// The issuer whose credential this is.
    #[must_use]
    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    /// The identifier to present at this issuer.
    #[must_use]
    pub const fn client_id(&self) -> &ClientId {
        &self.client_id
    }

    /// The secret, if this client has one.
    ///
    /// Returns `None` for a metadata-document client, and `Some` only when the
    /// caller has already established which issuer it is talking to.
    #[must_use]
    pub const fn secret(&self) -> Option<&ClientSecret> {
        self.secret.as_ref()
    }

    /// Whether this credential was issued by `issuer`.
    #[must_use]
    pub fn is_issued_by(&self, issuer: &str) -> bool {
        self.issuer.as_ref() == issuer
    }
}

/// What a client should do with its stored credential when starting a flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationDecision {
    /// Nothing is stored, so register.
    Register,
    /// A credential for this issuer is stored, so use it.
    Reuse,
    /// Something is stored but it belongs to a different authorization server.
    ///
    /// `2026-07-28` §9 (SEP-2352): a client **MUST** re-register when the
    /// authorization server changes, and **MUST NOT** reuse the old credential
    /// with the new one. Before that revision the rule is not written down, so
    /// reuse is permitted — the older models carry no such requirement, and a
    /// function that ignored the gate would be claiming the specification said
    /// something it does not.
    ReRegister,
}

/// Decide what to do with a stored credential.
///
/// Takes the revision's model rather than a bare boolean so the caller cannot
/// forget which revision it is negotiating, and so the decision is testable
/// against [`authorization_model`] rather than against a hand-written list.
#[must_use]
pub fn decide_registration(
    model: &AuthorizationModel,
    issuer: &str,
    stored: Option<&RegisteredClient>,
) -> RegistrationDecision {
    match stored {
        None => RegistrationDecision::Register,
        Some(stored) if stored.is_issued_by(issuer) => RegistrationDecision::Reuse,
        Some(_) if model.credentials_bound_to_issuer => RegistrationDecision::ReRegister,
        // No issuer binding required: reusing across servers is permitted,
        // though the new server will have its own view of whether it accepts it.
        Some(_) => RegistrationDecision::Reuse,
    }
}

/// A `client_id` or metadata document that does not satisfy what the mechanism
/// requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ClientIdError {
    /// A metadata document `client_id` that is not an HTTPS URL.
    #[error("a client id metadata document URL must use the https scheme")]
    NotHttps,

    /// A document whose `client_id` is not the URL it was served from.
    #[error("the metadata document's client_id does not match the URL it was served from")]
    DocumentMismatch,

    /// An RFC 7591 registration whose `client_id` is a metadata document URL.
    #[error("a dynamically registered client_id must not be a metadata document URL")]
    DocumentRegistration,

    /// A secret offered for a client identified by a metadata document.
    #[error("a client id metadata document client cannot hold a client secret")]
    SecretForDocument,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        ClientId, ClientIdError, ClientSecret, RegisteredClient, RegistrationDecision,
        decide_registration,
    };
    use crate::domain::ProtocolVersion;
    use crate::domain::mcp::authorization::{AuthorizationModel, authorization_model};

    const AS: &str = "https://auth.example.com";
    const OTHER_AS: &str = "https://auth.other.example";
    const DOCUMENT_URL: &str = "https://client.example.com/metadata.json";

    const fn model_for(version: ProtocolVersion) -> AuthorizationModel {
        authorization_model(version)
    }

    fn document_url() -> Result<ClientId, ClientIdError> {
        ClientId::metadata_url(DOCUMENT_URL)
    }

    fn secret() -> ClientSecret {
        ClientSecret(Arc::from("s3cret-value"))
    }

    fn document_client() -> Result<RegisteredClient, ClientIdError> {
        Ok(RegisteredClient::from_metadata_document(
            AS,
            document_url()?,
        ))
    }

    /// The secret is optional for a dynamic client — a public client with PKCE
    /// and no confidential leg is the ordinary case, not a degenerate one.
    #[test]
    fn a_dynamic_client_may_or_may_not_hold_a_secret() -> Result<(), ClientIdError> {
        let confidential =
            RegisteredClient::dynamic(AS, ClientId::opaque("client-abc"), Some(secret()))?;
        assert!(confidential.secret().is_some());
        assert_eq!(confidential.secret(), Some(&secret()));

        let public = RegisteredClient::dynamic(AS, ClientId::opaque("client-xyz"), None)?;
        assert!(public.secret().is_none());
        Ok(())
    }

    /// The draft is explicit that no shared secret can be established for this
    /// form, so the document-issued client cannot be given one at all.
    #[test]
    fn a_metadata_document_client_has_no_secret() -> Result<(), ClientIdError> {
        assert!(document_client()?.secret().is_none());
        Ok(())
    }

    /// A document URL offered as an RFC 7591 `client_id` means the two
    /// mechanisms have been confused.
    #[test]
    fn a_document_url_cannot_be_a_dynamic_registration() -> Result<(), ClientIdError> {
        assert_eq!(
            RegisteredClient::dynamic(AS, document_url()?, None),
            Err(ClientIdError::DocumentRegistration)
        );
        Ok(())
    }

    #[test]
    fn a_metadata_client_id_must_be_https() {
        for value in [
            "http://client.example.com/metadata.json",
            "ftp://client.example.com/metadata.json",
            "client.example.com/metadata.json",
        ] {
            assert_eq!(
                ClientId::metadata_url(value),
                Err(ClientIdError::NotHttps),
                "`{value}` is not an https URL"
            );
        }
    }

    /// The draft requires simple string comparison, citing RFC 3986 §6.2.1 —
    /// the same rule RFC 9207's `iss` follows.
    #[test]
    fn a_document_must_vouch_for_its_own_url() -> Result<(), ClientIdError> {
        let document = document_url()?;
        assert_eq!(document.verify_metadata_document(DOCUMENT_URL), Ok(()));
        for claimed in [
            "https://client.example.com/metadata.json/",
            "https://CLIENT.example.com/metadata.json",
            "https://client.example.com/metadata.json#",
            "https://client.example.com/other.json",
            "https://evil.example.com/metadata.json",
        ] {
            assert_eq!(
                document.verify_metadata_document(claimed),
                Err(ClientIdError::DocumentMismatch),
                "`{claimed}` must not verify as the document URL"
            );
        }
        Ok(())
    }

    /// `Debug` is where a secret escapes by accident; `expose` is where it escapes
    /// on purpose. Both halves are pinned so neither can drift into the other.
    #[test]
    fn a_secret_is_redacted_in_debug_and_exposed_only_on_request() {
        let secret = secret();
        let rendered = format!("{secret:?}");
        assert_eq!(rendered, "ClientSecret(…)");
        assert!(
            !rendered.contains("s3cret"),
            "the client secret leaked: {rendered}"
        );
        assert_eq!(secret.expose(), "s3cret-value");
    }

    #[test]
    fn no_stored_credential_means_register() {
        for version in [ProtocolVersion::V2024_11_05, ProtocolVersion::V2026_07_28] {
            assert_eq!(
                decide_registration(&model_for(version), AS, None),
                RegistrationDecision::Register
            );
        }
    }

    #[test]
    fn a_credential_for_this_issuer_is_reused() -> Result<(), ClientIdError> {
        let stored = document_client()?;
        for version in [ProtocolVersion::V2025_06_18, ProtocolVersion::V2026_07_28] {
            assert_eq!(
                decide_registration(&model_for(version), AS, Some(&stored)),
                RegistrationDecision::Reuse
            );
        }
        Ok(())
    }

    /// The rule this module exists for. SEP-2352: a changed authorization
    /// server means re-register, never reuse.
    #[test]
    fn a_credential_from_another_issuer_is_never_reused_in_2026_07_28() -> Result<(), ClientIdError>
    {
        let stored = document_client()?;
        let model = model_for(ProtocolVersion::V2026_07_28);
        assert!(model.credentials_bound_to_issuer);
        assert_eq!(
            decide_registration(&model, OTHER_AS, Some(&stored)),
            RegistrationDecision::ReRegister
        );
        Ok(())
    }

    /// Before the requirement existed, reuse across servers was permitted. The
    /// gate is real, so the older rows must actually behave differently rather
    /// than the function quietly ignoring it.
    #[test]
    fn an_earlier_revision_permits_cross_issuer_reuse() -> Result<(), ClientIdError> {
        let stored = document_client()?;
        for version in [
            ProtocolVersion::V2025_03_26,
            ProtocolVersion::V2025_06_18,
            ProtocolVersion::V2025_11_25,
        ] {
            let model = model_for(version);
            assert!(
                !model.credentials_bound_to_issuer,
                "{version:?} must not bind credentials to the issuer"
            );
            assert_eq!(
                decide_registration(&model, OTHER_AS, Some(&stored)),
                RegistrationDecision::Reuse,
                "{version:?} does not require re-registration"
            );
        }
        Ok(())
    }

    /// The issuer is part of the identity, so a credential cannot be matched by
    /// its `client_id` alone.
    #[test]
    fn a_credential_names_the_issuer_that_issued_it() -> Result<(), ClientIdError> {
        let stored = document_client()?;
        assert!(stored.is_issued_by(AS));
        assert!(!stored.is_issued_by(OTHER_AS));
        assert_eq!(stored.issuer(), AS);
        assert_eq!(stored.client_id().as_str(), DOCUMENT_URL);
        Ok(())
    }
}
