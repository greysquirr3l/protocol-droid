//! Choosing a client registration mechanism and carrying it out.
//!
//! MCP changed its mind about this twice. `2025-03-26` and `2025-06-18` name
//! RFC 7591 and nothing else; `2025-11-25` adds Client ID Metadata Documents as
//! *recommended* without deprecating RFC 7591; `2026-07-28` deprecates RFC 7591
//! "in favor of" documents while keeping it for servers that implement nothing
//! else. Which of those a client must do is a pure function of the negotiated
//! revision, and it lives in
//! [`ClientRegistration`].
//! This module is what acts on it.
//!
//! # Why planning is separate from doing
//!
//! [`plan_registration`] decides, and returns a value saying what it decided
//! and why. [`acquire_client`] then does it. The split is not ceremony: a
//! client that has to make this choice at the moment it is holding an HTTP
//! response has no way to log what it chose or to refuse before it opens a
//! socket. Concretely, `2026-07-28` §8 makes `application_type` a **MUST** for
//! dynamic registration, and a client that discovers that after the POST has
//! already registered a client the server will not honour.
//!
//! # What is not trusted
//!
//! Two things arrive from outside and neither is checked here, because both are
//! checks that belong with the thing that fetched it. A registration response
//! is the authorization server telling the client what it called it; the
//! adapter turns that into a [`RegisteredClient`] and refuses a `client_id`
//! that is a metadata document URL when dynamic registration was used. A
//! metadata document is the client publishing its own identity at a URL; the
//! adapter verifies the document's `client_id` is the URL it was served from,
//! because a document that claims to be some *other* client's is a forgery and
//! nothing downstream can detect it.
//!
//! What this module does guarantee is that neither result is ever issued by the
//! wrong issuer, because [`RegisteredClient`] carries its issuer and
//! [`acquire_client`] builds every one it returns itself.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::domain::mcp::authorization::{AuthorizationModel, ClientRegistration};
use crate::domain::oauth::registration::{
    ClientId, ClientIdError, RegisteredClient, RegistrationDecision, decide_registration,
};

/// The metadata a client presents to register.
///
/// This is RFC 7591 §2's client metadata document, and MCP adds one requirement
/// on top: `2026-07-28` §8 (SEP-837) makes `application_type` a **MUST** for
/// dynamic registration "to avoid `OpenID Connect` redirect URI conflicts".
///
/// # Unknown fields
///
/// [`extra`](Self::extra) holds anything not named here and is merged into the
/// serialized document. The named fields are the ones this crate has to read or
/// gate on; a client that publishes `client_uri`, `icon_uri` or `title` —
/// all of which the newer revisions name — sets them through
/// [`with_field`](Self::with_field) rather than losing them at a struct
/// boundary. Preserving them matters for the same reason it matters on the IR:
/// the authorization server sees the document this client published, and
/// silently dropping a field turns a working registration into a failing one.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ClientMetadata {
    redirect_uris: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    token_endpoint_auth_method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    application_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

impl ClientMetadata {
    /// Metadata with the redirect URIs it will be authorized to use.
    ///
    /// RFC 7591 §2 makes `redirect_uris` the one required member of a client
    /// metadata document, so this constructor refuses an empty list rather than
    /// letting a server reject it.
    ///
    /// # Errors
    ///
    /// [`ClientIdError::NotHttps`] if a redirect URI is not an absolute `https`
    /// URL, or [`ClientIdError::DocumentRegistration`] if the list is empty.
    pub fn new(redirect_uris: Vec<String>) -> Result<Self, ClientIdError> {
        if redirect_uris.is_empty() {
            return Err(ClientIdError::DocumentRegistration);
        }
        for uri in &redirect_uris {
            // A native-app loopback redirect is `http://127.0.0.1` and is
            // legal in RFC 8252 §7.3, so the check is absolute-and-known-scheme
            // rather than https-always. What is refused is a bare path, which
            // is what a half-filled configuration produces.
            let Ok(parsed) = url::Url::parse(uri) else {
                return Err(ClientIdError::NotHttps);
            };
            if !matches!(parsed.scheme(), "https" | "http") || parsed.host_str().is_none() {
                return Err(ClientIdError::NotHttps);
            }
        }
        Ok(Self {
            redirect_uris,
            ..Self::default()
        })
    }

    /// The redirect URIs, in the order they were given.
    #[must_use]
    pub fn redirect_uris(&self) -> &[String] {
        &self.redirect_uris
    }

    /// The name this client presents at a consent screen.
    #[must_use]
    pub fn client_name(&self) -> Option<&str> {
        self.client_name.as_deref()
    }

    /// Set the consent-screen name.
    #[must_use]
    pub fn with_client_name(mut self, name: impl Into<String>) -> Self {
        self.client_name = Some(name.into());
        self
    }

    /// How this client authenticates at the token endpoint.
    ///
    /// `"none"` declares a public client, which is what a Client ID Metadata
    /// Document client is: the draft is explicit that no shared secret can be
    /// established for that form.
    #[must_use]
    pub fn token_endpoint_auth_method(&self) -> Option<&str> {
        self.token_endpoint_auth_method.as_deref()
    }

    /// Set the token endpoint authentication method.
    #[must_use]
    pub fn with_token_endpoint_auth_method(mut self, method: impl Into<String>) -> Self {
        self.token_endpoint_auth_method = Some(method.into());
        self
    }

    /// The OIDC `application_type` this client is.
    #[must_use]
    pub fn application_type(&self) -> Option<&str> {
        self.application_type.as_deref()
    }

    /// Set the OIDC `application_type`.
    #[must_use]
    pub fn with_application_type(mut self, application_type: impl Into<String>) -> Self {
        self.application_type = Some(application_type.into());
        self
    }

    /// Set one field this crate does not name, preserving it on the wire.
    ///
    /// Takes precedence over a named field of the same name, so a caller cannot
    /// set `redirect_uris` through here and have it win over the typed value —
    /// that would let a document claim redirects the client never checked.
    ///
    /// [`ClientMetadataError::ReservedField`] names the fields that are typed.
    pub fn with_field(
        mut self,
        name: impl Into<String>,
        value: impl Into<Value>,
    ) -> Result<Self, ClientMetadataError> {
        let name = name.into();
        if RESERVED_FIELDS.contains(&name.as_str()) {
            return Err(ClientMetadataError::ReservedField(name));
        }
        self.extra.insert(name, value.into());
        Ok(self)
    }

    /// The fields this crate does not name, as they will be serialized.
    #[must_use]
    pub const fn extra(&self) -> &Map<String, Value> {
        &self.extra
    }
}

/// The fields [`ClientMetadata`] models, and so cannot be set through
/// [`ClientMetadata::with_field`].
const RESERVED_FIELDS: &[&str] = &[
    "redirect_uris",
    "client_name",
    "token_endpoint_auth_method",
    "application_type",
    "scope",
];

/// A [`ClientMetadata`] field name is one this crate models.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClientMetadataError {
    /// A field this crate models was set through the passthrough.
    #[error("'{0}' is a modelled client metadata field and cannot be set as an extra")]
    ReservedField(String),
}

/// What this client is willing to identify itself with.
///
/// A client may have both: a published metadata document *and* the metadata to
/// fall back on. That is the case `2026-07-28` describes — a server may still
/// implement only RFC 7591 — and it is the reason this is a pair of optionals
/// rather than an enum.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClientIdentity {
    document: Option<ClientId>,
    metadata: Option<ClientMetadata>,
}

impl ClientIdentity {
    /// A client identified by a published metadata document, with nothing to
    /// fall back on.
    #[must_use]
    pub const fn from_document(client_id: ClientId) -> Self {
        Self {
            document: Some(client_id),
            metadata: None,
        }
    }

    /// A client that will register dynamically, with no document.
    #[must_use]
    pub const fn from_metadata(metadata: ClientMetadata) -> Self {
        Self {
            document: None,
            metadata: Some(metadata),
        }
    }

    /// Add metadata to fall back on if the document cannot be used.
    #[must_use]
    pub fn with_metadata_fallback(mut self, metadata: ClientMetadata) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// The published document, if there is one.
    #[must_use]
    pub const fn document(&self) -> Option<&ClientId> {
        self.document.as_ref()
    }

    /// The metadata to register with, if there is any.
    #[must_use]
    pub const fn metadata(&self) -> Option<&ClientMetadata> {
        self.metadata.as_ref()
    }
}

/// What [`plan_registration`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrationPlan {
    /// Use this credential; nothing needs to be fetched.
    ///
    /// The stored value is returned rather than re-registered. That is the
    /// whole point of a stored credential: `2026-07-28` §9 binds it to its
    /// issuer precisely so it can be reused.
    Reuse(RegisteredClient),

    /// Fetch the metadata document at this URL.
    ///
    /// `fallback` is what to register dynamically with if the fetch fails and
    /// the revision still allows RFC 7591. It is `None` when the client has no
    /// metadata to fall back on, or when the revision does not permit dynamic
    /// registration at all.
    FetchDocument {
        /// The URL the document is published at.
        client_id: ClientId,
        /// What to register with instead, if the fetch fails.
        fallback: Option<ClientMetadata>,
    },

    /// Register dynamically with this metadata.
    RegisterDynamic {
        /// The metadata to present.
        metadata: ClientMetadata,
    },
}

/// Why a client could not be registered.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistrationError {
    /// The revision names no registration mechanism.
    ///
    /// `2024-11-05`'s vendored changelog has no authorization entry at all, so
    /// a client that registers against an OAuth server here is doing something
    /// the revision never asked for. Refusing is louder than proceeding, and a
    /// server that would have accepted it is not evidence the client was right.
    #[error("this revision specifies no client registration mechanism")]
    NoMechanism,

    /// The client has a published document the revision cannot use.
    #[error("this revision supports neither client id metadata documents nor dynamic registration")]
    NoUsableMechanism,

    /// `2026-07-28` §8 requires `application_type` for dynamic registration.
    ///
    /// Refused before anything is sent, because the alternative is registering
    /// a client the server will not honour.
    #[error(
        "this revision requires application_type on dynamic registration and the metadata does not set it"
    )]
    ApplicationTypeRequired,

    /// A stored credential was issued by another issuer.
    #[error(
        "the stored client was issued by a different authorization server and this revision requires issuer binding"
    )]
    IssuerBound,

    /// The document could not be used and no fallback was available.
    #[error("the client id metadata document could not be fetched: {0}")]
    DocumentUnavailable(#[from] RegistrarError),

    /// Dynamic registration failed.
    #[error("dynamic client registration failed: {0}")]
    DynamicFailed(RegistrarError),
}

/// Why a registration request did not produce a client.
///
/// Transport-neutral on purpose: the adapter maps its HTTP client and its JSON
/// parser into these, and nothing in `application/` names either.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistrarError {
    /// The request could not be completed.
    #[error("the registration request could not be completed: {0}")]
    Unavailable(String),

    /// The server answered with a non-success status.
    #[error("the authorization server answered {status} to a client registration request")]
    Refused {
        /// The HTTP status the server returned.
        status: u16,
    },

    /// The response was not a registration response this crate can use.
    #[error("the registration response could not be used: {0}")]
    Malformed(String),

    /// A metadata document failed the check that makes it usable.
    #[error(transparent)]
    Document(#[from] ClientIdError),
}

/// Registers a client dynamically through RFC 7591 §3.1.
///
/// A port of its own rather than a method on a wider trait, because a
/// deployment may be able to do only one of the two mechanisms: a client that
/// publishes a document and points at a server that implements only documents
/// has no use for this, and an adapter that claimed it would be promising a
/// capability it does not have.
///
/// Native `async fn`, so it is not object safe. That is the right trade:
/// registration happens once per client rather than per request, and the call
/// site takes `&impl DynamicRegistrar` for a zero-cost monomorphised call.
///
/// # Examples
///
/// ```
/// use protocol_droid::application::oauth::registration::{
///     ClientMetadata, DynamicRegistrar, RegistrarError,
/// };
/// use protocol_droid::domain::oauth::registration::{ClientId, RegisteredClient};
///
/// struct AlwaysTheSame;
///
/// impl DynamicRegistrar for AlwaysTheSame {
///     async fn register_dynamic(
///         &self,
///         issuer: &str,
///         _metadata: &ClientMetadata,
///     ) -> Result<RegisteredClient, RegistrarError> {
///         Ok(RegisteredClient::dynamic(
///             issuer,
///             ClientId::opaque("client-1"),
///             None,
///         )?)
///     }
/// }
/// # Ok::<(), RegistrarError>(())
/// ```
pub trait DynamicRegistrar {
    /// POST the metadata to the registration endpoint and read the credential
    /// the server issues.
    ///
    /// # Errors
    ///
    /// [`RegistrarError`] if the request could not be completed, the server
    /// refused it, or the response was not a usable registration response.
    fn register_dynamic(
        &self,
        issuer: &str,
        metadata: &ClientMetadata,
    ) -> impl Future<Output = Result<RegisteredClient, RegistrarError>> + Send;
}

/// Fetches the Client ID Metadata Document a client published.
///
/// The counterpart to [`DynamicRegistrar`], and separate for the same reason.
/// The two differ in what they establish as well as in what they send: this one
/// yields no secret at all.
///
/// # Examples
///
/// ```
/// use protocol_droid::application::oauth::registration::{
///     MetadataDocumentLoader, RegistrarError,
/// };
/// use protocol_droid::domain::oauth::registration::{ClientId, RegisteredClient};
///
/// struct RefusesEverything;
///
/// impl MetadataDocumentLoader for RefusesEverything {
///     async fn fetch_metadata_document(
///         &self,
///         _issuer: &str,
///         _client_id: &ClientId,
///     ) -> Result<RegisteredClient, RegistrarError> {
///         Err(RegistrarError::Refused { status: 404 })
///     }
/// }
/// # Ok::<(), RegistrarError>(())
/// ```
pub trait MetadataDocumentLoader {
    /// Fetch the document published at `client_id` and verify what it claims.
    ///
    /// # Errors
    ///
    /// [`RegistrarError`] if the document could not be fetched, or
    /// [`RegistrarError::Document`] if it does not claim the URL it was served
    /// from. The two are distinct because the use case reacts differently.
    fn fetch_metadata_document(
        &self,
        issuer: &str,
        client_id: &ClientId,
    ) -> impl Future<Output = Result<RegisteredClient, RegistrarError>> + Send;
}

/// What this deployment can actually reach.
///
/// A separate argument to [`plan_registration`] rather than something it reads
/// off the identity, because the two fail independently: a client can publish
/// a document to a URL that resolves nowhere, and a deployment can wire a
/// dynamic registrar while its client has no metadata to present. Planning on
/// one and discovering the other during execution is how a client ends up
/// explaining a failure it could have refused to start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Available {
    dynamic: bool,
    metadata_document: bool,
}

impl Available {
    /// Neither mechanism is reachable.
    #[must_use]
    pub const fn neither() -> Self {
        Self {
            dynamic: false,
            metadata_document: false,
        }
    }

    /// Only RFC 7591 dynamic registration.
    #[must_use]
    pub const fn dynamic_only() -> Self {
        Self {
            dynamic: true,
            metadata_document: false,
        }
    }

    /// Only Client ID Metadata Documents.
    #[must_use]
    pub const fn metadata_document_only() -> Self {
        Self {
            dynamic: false,
            metadata_document: true,
        }
    }

    /// Both, with a fallback available in either direction.
    #[must_use]
    pub const fn both() -> Self {
        Self {
            dynamic: true,
            metadata_document: true,
        }
    }

    /// Whether dynamic registration can be reached.
    #[must_use]
    pub const fn dynamic(self) -> bool {
        self.dynamic
    }

    /// Whether a metadata document can be fetched.
    #[must_use]
    pub const fn metadata_document(self) -> bool {
        self.metadata_document
    }
}

/// Decide how to register, without doing it.
///
/// The order is: reuse a stored credential the revision lets this client reuse,
/// then a published document, then dynamic registration. Document before
/// dynamic is not a preference — `2026-07-28` changelog §4 deprecates RFC 7591,
/// so leading with it against a server that supports both is a client
/// provoking a rejection.
///
/// # Errors
///
/// [`RegistrationError::IssuerBound`] if a stored credential belongs to another
/// issuer and the revision requires binding. [`RegistrationError::NoMechanism`]
/// if the revision names none. [`RegistrationError::NoUsableMechanism`] if what
/// the client has is a mechanism this revision does not define.
/// [`RegistrationError::ApplicationTypeRequired`] if dynamic registration is
/// planned for `2026-07-28` without `application_type` set.
pub fn plan_registration(
    model: &AuthorizationModel,
    issuer: &str,
    identity: &ClientIdentity,
    stored: Option<&RegisteredClient>,
    available: Available,
) -> Result<RegistrationPlan, RegistrationError> {
    if let Some(stored) = stored {
        match decide_registration(model, issuer, Some(stored)) {
            // Reused across issuers only where the revision does not require
            // binding, and even then the returned value carries its original
            // issuer, so nothing downstream can mistake it for this server's.
            RegistrationDecision::Reuse => {
                return Ok(RegistrationPlan::Reuse(stored.clone()));
            }
            RegistrationDecision::ReRegister => {
                return Err(RegistrationError::IssuerBound);
            }
            RegistrationDecision::Register => {}
        }
    }

    let mechanism = model.client_registration;
    if mechanism == ClientRegistration::None {
        return Err(RegistrationError::NoMechanism);
    }

    // A document the revision defines and this deployment can reach, with
    // dynamic registration as the fallback exactly when both of those are also
    // true and the client has metadata to fall back on.
    if let (true, Some(client_id)) = (
        mechanism.supports_metadata_document() && available.metadata_document,
        identity.document(),
    ) {
        let fallback = identity
            .metadata()
            .filter(|_| mechanism.supports_dynamic() && available.dynamic)
            .cloned();
        return Ok(RegistrationPlan::FetchDocument {
            client_id: client_id.clone(),
            fallback,
        });
    }

    // Otherwise dynamic registration, if both the revision and the client allow
    // it. A document this revision cannot use is not silently ignored: if it is
    // the only thing the client has, there is nothing left to try.
    if let (true, Some(metadata)) = (mechanism.supports_dynamic(), identity.metadata()) {
        if mechanism.application_type_required() && metadata.application_type().is_none() {
            return Err(RegistrationError::ApplicationTypeRequired);
        }
        return Ok(RegistrationPlan::RegisterDynamic {
            metadata: metadata.clone(),
        });
    }

    Err(RegistrationError::NoUsableMechanism)
}

/// Plan a registration and carry it out.
///
/// Falls back from a document to dynamic registration when [`RegistrationPlan`]
/// carries a fallback and the document could not be used. It does **not** fall
/// back the other way: a client that publishes a document and cannot fetch it
/// has no identity to present, and registering one dynamically would create a
/// second identity rather than recover the first.
///
/// # Errors
///
/// Whatever [`plan_registration`] returns, or [`RegistrationError::DynamicFailed`]
/// if the fallback was needed and also failed.
///
/// # Examples
///
/// ```
/// use protocol_droid::application::oauth::registration::{
///     ClientIdentity, ClientMetadata, acquire_client, plan_registration,
/// };
/// use protocol_droid::domain::ProtocolVersion;
/// use protocol_droid::domain::mcp::authorization::authorization_model;
/// use protocol_droid::domain::oauth::registration::ClientId;
///
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// use protocol_droid::application::oauth::registration::Available;
///
/// let model = authorization_model(ProtocolVersion::V2026_07_28);
/// let identity = ClientIdentity::from_document(
///     ClientId::metadata_url("https://client.example.com/metadata.json")?,
/// );
///
/// // Planning is pure: the document is preferred, and the plan says so.
/// let plan = plan_registration(
///     &model,
///     "https://auth.example.com",
///     &identity,
///     None,
///     Available::metadata_document_only(),
/// )?;
/// assert!(matches!(
///     plan,
///     protocol_droid::application::oauth::registration::RegistrationPlan::FetchDocument { .. }
/// ));
///
/// // Acting on it needs a loader to fetch with, so it is not exercised here.
/// # Ok(())
/// # }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub async fn acquire_client(
    model: &AuthorizationModel,
    issuer: &str,
    identity: &ClientIdentity,
    stored: Option<&RegisteredClient>,
    dynamic: Option<&(impl DynamicRegistrar + Sync)>,
    documents: Option<&(impl MetadataDocumentLoader + Sync)>,
) -> Result<RegisteredClient, RegistrationError> {
    let available = match (dynamic.is_some(), documents.is_some()) {
        (true, true) => Available::both(),
        (true, false) => Available::dynamic_only(),
        (false, true) => Available::metadata_document_only(),
        (false, false) => Available::neither(),
    };

    match plan_registration(model, issuer, identity, stored, available)? {
        RegistrationPlan::Reuse(client) => Ok(client),
        RegistrationPlan::FetchDocument {
            client_id,
            fallback,
        } => {
            let loader = documents.ok_or(RegistrationError::NoUsableMechanism)?;
            match loader.fetch_metadata_document(issuer, &client_id).await {
                Ok(client) => Ok(client),
                // Only a document that could not be *fetched* falls back. A
                // document that was fetched and is wrong is a forgery, and
                // falling back from it would let a client that has been
                // impersonated quietly register a fresh identity instead.
                Err(failure @ RegistrarError::Document(_)) => {
                    Err(RegistrationError::DocumentUnavailable(failure))
                }
                Err(failure) => match fallback {
                    Some(metadata) => {
                        let registrar = dynamic.ok_or(RegistrationError::NoUsableMechanism)?;
                        registrar
                            .register_dynamic(issuer, &metadata)
                            .await
                            .map_err(RegistrationError::DynamicFailed)
                    }
                    None => Err(RegistrationError::DocumentUnavailable(failure)),
                },
            }
        }
        RegistrationPlan::RegisterDynamic { metadata } => {
            let registrar = dynamic.ok_or(RegistrationError::NoUsableMechanism)?;
            registrar
                .register_dynamic(issuer, &metadata)
                .await
                .map_err(RegistrationError::DynamicFailed)
        }
    }
}
#[cfg(test)]
mod tests {
    use std::assert_matches;
    use std::future::Future;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::task::{Context, Poll, Waker};

    use super::{
        Available, ClientIdentity, ClientMetadata, ClientMetadataError, DynamicRegistrar,
        MetadataDocumentLoader, RegistrarError, RegistrationError, RegistrationPlan,
        acquire_client, plan_registration,
    };
    use crate::domain::ProtocolVersion;
    use crate::domain::mcp::authorization::{AuthorizationModel, authorization_model};
    use crate::domain::oauth::registration::{ClientId, ClientIdError, RegisteredClient};

    const AS: &str = "https://auth.example.com";
    const OTHER_AS: &str = "https://other-auth.example.com";
    const URL: &str = "https://client.example.com/metadata.json";

    /// The revisions whose authorization spec differs in a way this module
    /// acts on. A bare `AuthorizationModel` would let a test pick a revision by
    /// accident and assert something true of all of them.
    const EARLY: ProtocolVersion = ProtocolVersion::V2024_11_05;
    const DYNAMIC_ONLY: ProtocolVersion = ProtocolVersion::V2025_03_26;
    const BOTH: ProtocolVersion = ProtocolVersion::V2025_11_25;
    const DOCUMENT_FIRST: ProtocolVersion = ProtocolVersion::V2026_07_28;

    fn model(version: ProtocolVersion) -> AuthorizationModel {
        authorization_model(version)
    }

    /// The error types these tests answer, collected so `?` works and a failure
    /// prints the engine's own account of what it refused.
    #[derive(Debug, thiserror::Error)]
    enum Failure {
        #[error(transparent)]
        Id(#[from] ClientIdError),
        #[error(transparent)]
        Metadata(#[from] ClientMetadataError),
        #[error(transparent)]
        Registration(#[from] RegistrationError),
        #[error("a fake port pended, which none of them should")]
        Pended,
    }

    impl From<RegistrarError> for Failure {
        fn from(error: RegistrarError) -> Self {
            Self::Registration(RegistrationError::DynamicFailed(error))
        }
    }

    /// Drives a future that is expected never to pend, and says so when it
    /// does.
    ///
    /// `#[tokio::test]` is not available here: this module is gated on `oauth`,
    /// and the tokio macros come in with `http`. Pulling a runtime in to test
    /// fakes that return immediately would be a heavier dependency than the
    /// thing under test. `Waker::noop` is correct for a future that resolves on
    /// its first poll, and returning `None` on `Pending` is what makes that
    /// assumption checkable rather than merely stated.
    fn now<T>(future: impl Future<Output = T>) -> Option<T> {
        let mut future = std::pin::pin!(future);
        let waker = Waker::noop();
        match future.as_mut().poll(&mut Context::from_waker(waker)) {
            Poll::Ready(output) => Some(output),
            Poll::Pending => None,
        }
    }

    fn metadata() -> Result<ClientMetadata, Failure> {
        Ok(
            ClientMetadata::new(vec!["https://client.example.com/callback".to_owned()])?
                .with_client_name("Example"),
        )
    }

    fn document_identity() -> Result<ClientIdentity, Failure> {
        Ok(ClientIdentity::from_document(ClientId::metadata_url(URL)?))
    }

    /// A dynamic registrar that always succeeds, counting its calls so a test
    /// can prove it was *not* reached.
    struct Dynamic {
        calls: AtomicU32,
    }

    impl DynamicRegistrar for Dynamic {
        // Not `async`: this fake resolves on its first poll, so `async` here
        // would be an async block with no await point in it.
        fn register_dynamic(
            &self,
            issuer: &str,
            _metadata: &ClientMetadata,
        ) -> impl Future<Output = Result<RegisteredClient, RegistrarError>> + Send {
            self.calls.fetch_add(1, Ordering::Relaxed);
            std::future::ready(
                RegisteredClient::dynamic(issuer, ClientId::opaque("dynamic-1"), None)
                    .map_err(Into::into),
            )
        }
    }

    /// A document loader that answers with whatever it was built to answer.
    struct Loader {
        outcome: Result<(), RegistrarError>,
        calls: AtomicU32,
    }

    impl Loader {
        fn serving() -> Self {
            Self {
                outcome: Ok(()),
                calls: AtomicU32::new(0),
            }
        }

        fn failing(error: RegistrarError) -> Self {
            Self {
                outcome: Err(error),
                calls: AtomicU32::new(0),
            }
        }
    }

    impl MetadataDocumentLoader for Loader {
        fn fetch_metadata_document(
            &self,
            issuer: &str,
            client_id: &ClientId,
        ) -> impl Future<Output = Result<RegisteredClient, RegistrarError>> + Send {
            self.calls.fetch_add(1, Ordering::Relaxed);
            std::future::ready(match &self.outcome {
                Ok(()) => Ok(RegisteredClient::from_metadata_document(
                    issuer,
                    client_id.clone(),
                )),
                Err(failure) => Err(failure.clone()),
            })
        }
    }

    // --- what each revision allows -----------------------------------------

    /// `2024-11-05` has no authorization entry at all, so a client that
    /// registers here is doing something the revision never asked for. Proceeding
    /// quietly is how that goes unnoticed.
    #[test]
    fn a_revision_with_no_authorization_framework_refuses_to_register() -> Result<(), Failure> {
        assert_matches!(
            plan_registration(
                &model(EARLY),
                AS,
                &ClientIdentity::from_metadata(metadata()?),
                None,
                Available::both(),
            ),
            Err(RegistrationError::NoMechanism)
        );
        Ok(())
    }

    /// `2025-03-26` names RFC 7591 and nothing else. A client holding only a
    /// document has nothing it can use, and saying so beats silently
    /// registering a second identity.
    #[test]
    fn a_document_is_not_usable_where_the_revision_never_named_one() -> Result<(), Failure> {
        assert_matches!(
            plan_registration(
                &model(DYNAMIC_ONLY),
                AS,
                &document_identity()?,
                None,
                Available::both(),
            ),
            Err(RegistrationError::NoUsableMechanism)
        );
        Ok(())
    }

    #[test]
    fn a_revision_naming_only_dynamic_registers_dynamically() -> Result<(), Failure> {
        let plan = plan_registration(
            &model(DYNAMIC_ONLY),
            AS,
            &ClientIdentity::from_metadata(metadata()?),
            None,
            Available::dynamic_only(),
        )?;

        assert_matches!(plan, RegistrationPlan::RegisterDynamic { .. });
        Ok(())
    }

    /// The ordering the whole module exists for. `2026-07-28` changelog §4
    /// deprecates RFC 7591 "in favor of" documents, so a client with both must
    /// lead with the document — a client that leads with the deprecated
    /// mechanism against a server supporting both is provoking a rejection.
    #[test]
    fn a_document_is_preferred_even_when_the_client_can_also_register() -> Result<(), Failure> {
        let identity = document_identity()?.with_metadata_fallback(metadata()?);

        let plan = plan_registration(
            &model(DOCUMENT_FIRST),
            AS,
            &identity,
            None,
            Available::both(),
        )?;

        assert_matches!(plan, RegistrationPlan::FetchDocument { .. });
        Ok(())
    }

    /// `2025-11-25` changelog §8 recommends documents without deprecating RFC
    /// 7591, so the same ordering applies a revision earlier.
    #[test]
    fn the_preference_predates_the_deprecation() -> Result<(), Failure> {
        let identity = document_identity()?.with_metadata_fallback(metadata()?);

        let plan = plan_registration(&model(BOTH), AS, &identity, None, Available::both())?;

        assert_matches!(plan, RegistrationPlan::FetchDocument { .. });
        Ok(())
    }

    /// A deployment that wired no document loader must not be handed a plan to
    /// fetch one.
    #[test]
    fn a_document_needs_a_loader_to_fetch_it_with() -> Result<(), Failure> {
        let identity = document_identity()?;

        assert_matches!(
            plan_registration(
                &model(DOCUMENT_FIRST),
                AS,
                &identity,
                None,
                Available::dynamic_only(),
            ),
            Err(RegistrationError::NoUsableMechanism)
        );
        Ok(())
    }

    // --- the MUST that has to be caught before anything is sent ------------

    /// `2026-07-28` §8 makes `application_type` a MUST for dynamic
    /// registration. Catching it at plan time is the difference between a clear
    /// refusal and a registered client the server will not honour.
    #[test]
    fn the_required_application_type_is_refused_before_anything_is_sent() -> Result<(), Failure> {
        assert_matches!(
            plan_registration(
                &model(DOCUMENT_FIRST),
                AS,
                &ClientIdentity::from_metadata(metadata()?),
                None,
                Available::dynamic_only(),
            ),
            Err(RegistrationError::ApplicationTypeRequired)
        );
        Ok(())
    }

    #[test]
    fn supplying_the_application_type_satisfies_the_requirement() -> Result<(), Failure> {
        let identity = ClientIdentity::from_metadata(metadata()?.with_application_type("native"));

        assert_matches!(
            plan_registration(
                &model(DOCUMENT_FIRST),
                AS,
                &identity,
                None,
                Available::dynamic_only(),
            ),
            Ok(RegistrationPlan::RegisterDynamic { .. })
        );
        Ok(())
    }

    /// `2026-07-28` deprecates RFC 7591 rather than removing it, so a server
    /// implementing only RFC 7591 is not misconfigured and a client must still
    /// be able to use it.
    #[test]
    fn the_deprecated_mechanism_is_still_available() {
        assert!(
            model(DOCUMENT_FIRST).client_registration.supports_dynamic(),
            "2026-07-28 still permits RFC 7591"
        );
    }

    // --- stored credentials ------------------------------------------------

    #[test]
    fn a_stored_credential_from_this_issuer_is_reused() -> Result<(), Failure> {
        let stored = RegisteredClient::dynamic(AS, ClientId::opaque("stored"), None)?;

        let plan = plan_registration(
            &model(DOCUMENT_FIRST),
            AS,
            &document_identity()?,
            Some(&stored),
            Available::both(),
        )?;

        assert_matches!(plan, RegistrationPlan::Reuse(ref client) if client == &stored);
        Ok(())
    }

    /// `2026-07-28` binds a credential to its issuer so a credential minted by
    /// one server cannot be presented to another. A stored credential from
    /// elsewhere has to be refused, not quietly reused.
    #[test]
    fn a_credential_from_another_issuer_is_refused_where_binding_is_required() -> Result<(), Failure>
    {
        let stored = RegisteredClient::dynamic(OTHER_AS, ClientId::opaque("stored"), None)?;

        assert_matches!(
            plan_registration(
                &model(DOCUMENT_FIRST),
                AS,
                &document_identity()?,
                Some(&stored),
                Available::both(),
            ),
            Err(RegistrationError::IssuerBound)
        );
        Ok(())
    }

    /// Older revisions do not require binding, so the same stored credential
    /// is reused. It still carries the issuer it was issued by, so nothing
    /// downstream can mistake it for the current server's.
    #[test]
    fn a_credential_from_another_issuer_is_reused_where_binding_is_not_required()
    -> Result<(), Failure> {
        let stored = RegisteredClient::dynamic(OTHER_AS, ClientId::opaque("stored"), None)?;

        let plan = plan_registration(
            &model(DYNAMIC_ONLY),
            AS,
            &ClientIdentity::from_metadata(metadata()?),
            Some(&stored),
            Available::dynamic_only(),
        )?;

        assert_matches!(plan, RegistrationPlan::Reuse(ref client) if client.is_issued_by(OTHER_AS));
        Ok(())
    }

    // --- executing the plan ------------------------------------------------

    #[test]
    fn a_served_document_is_used_without_registering() -> Result<(), Failure> {
        let dynamic = Dynamic {
            calls: AtomicU32::new(0),
        };
        let loader = Loader::serving();

        let client = match now(acquire_client(
            &model(DOCUMENT_FIRST),
            AS,
            &document_identity()?,
            None,
            Some(&dynamic),
            Some(&loader),
        )) {
            Some(result) => result?,
            None => return Err(Failure::Pended),
        };

        assert_eq!(client.client_id().as_str(), URL);
        assert_eq!(loader.calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            dynamic.calls.load(Ordering::Relaxed),
            0,
            "the document path registered anyway"
        );
        Ok(())
    }

    /// `2026-07-28` keeps RFC 7591 for servers that do not implement documents,
    /// and a document that does not resolve is exactly that server.
    #[test]
    fn an_unreachable_document_falls_back_to_dynamic_registration() -> Result<(), Failure> {
        let dynamic = Dynamic {
            calls: AtomicU32::new(0),
        };
        let loader = Loader::failing(RegistrarError::Unavailable("connection refused".into()));
        let identity = document_identity()?.with_metadata_fallback(metadata()?);

        let client = match now(acquire_client(
            &model(DOCUMENT_FIRST),
            AS,
            &identity,
            None,
            Some(&dynamic),
            Some(&loader),
        )) {
            Some(result) => result?,
            None => return Err(Failure::Pended),
        };

        assert_eq!(client.client_id().as_str(), "dynamic-1");
        assert_eq!(dynamic.calls.load(Ordering::Relaxed), 1);
        Ok(())
    }

    /// The distinction that matters. A document that could not be *fetched* is a
    /// server without documents; a document that was fetched and does not claim
    /// the URL it came from is a forgery, and falling back from it would let an
    /// impersonated client quietly register a fresh identity instead of
    /// surfacing the problem.
    #[test]
    fn a_document_that_is_present_but_wrong_never_falls_back() -> Result<(), Failure> {
        let dynamic = Dynamic {
            calls: AtomicU32::new(0),
        };
        let loader = Loader::failing(RegistrarError::Document(ClientIdError::DocumentMismatch));
        let identity = document_identity()?.with_metadata_fallback(metadata()?);

        let Some(outcome) = now(acquire_client(
            &model(DOCUMENT_FIRST),
            AS,
            &identity,
            None,
            Some(&dynamic),
            Some(&loader),
        )) else {
            return Err(Failure::Pended);
        };

        assert_matches!(outcome, Err(RegistrationError::DocumentUnavailable(_)));
        assert_eq!(
            dynamic.calls.load(Ordering::Relaxed),
            0,
            "a forged document silently became a fresh registration"
        );
        Ok(())
    }

    /// With no metadata to fall back on, an unreachable document is just a
    /// failure.
    #[test]
    fn an_unreachable_document_with_no_fallback_is_an_error() -> Result<(), Failure> {
        let dynamic = Dynamic {
            calls: AtomicU32::new(0),
        };
        let loader = Loader::failing(RegistrarError::Unavailable("connection refused".into()));

        let Some(outcome) = now(acquire_client(
            &model(DOCUMENT_FIRST),
            AS,
            &document_identity()?,
            None,
            Some(&dynamic),
            Some(&loader),
        )) else {
            return Err(Failure::Pended);
        };

        assert_matches!(outcome, Err(RegistrationError::DocumentUnavailable(_)));
        assert_eq!(dynamic.calls.load(Ordering::Relaxed), 0);
        Ok(())
    }

    #[test]
    fn a_stored_credential_is_returned_without_any_port_being_called() -> Result<(), Failure> {
        let dynamic = Dynamic {
            calls: AtomicU32::new(0),
        };
        let loader = Loader::serving();
        let stored = RegisteredClient::dynamic(AS, ClientId::opaque("stored"), None)?;

        let client = match now(acquire_client(
            &model(DOCUMENT_FIRST),
            AS,
            &document_identity()?,
            Some(&stored),
            Some(&dynamic),
            Some(&loader),
        )) {
            Some(result) => result?,
            None => return Err(Failure::Pended),
        };

        assert_eq!(&client, &stored);
        assert_eq!(loader.calls.load(Ordering::Relaxed), 0);
        assert_eq!(dynamic.calls.load(Ordering::Relaxed), 0);
        Ok(())
    }

    // --- the metadata itself ----------------------------------------------

    /// RFC 7591 §2 makes `redirect_uris` the one required member.
    #[test]
    fn metadata_without_redirect_uris_is_refused() {
        assert!(ClientMetadata::new(Vec::new()).is_err());
    }

    /// A bare path is what a half-filled configuration produces, and a server
    /// would reject it.
    #[test]
    fn metadata_with_a_relative_redirect_uri_is_refused() {
        assert!(ClientMetadata::new(vec!["/callback".to_owned()]).is_err());
    }

    #[test]
    fn a_loopback_redirect_is_allowed() -> Result<(), Failure> {
        // RFC 8252 §7.3 makes the native-app loopback redirect legal, so an
        // https-only rule would refuse a legitimate client.
        let metadata = ClientMetadata::new(vec!["http://127.0.0.1:8080/callback".to_owned()])?;

        assert_eq!(metadata.redirect_uris().len(), 1);
        Ok(())
    }

    /// The named fields are the ones this crate gates on, so the passthrough
    /// must not be able to set one — otherwise a document could claim redirect
    /// URIs no constructor ever checked.
    #[test]
    fn the_passthrough_cannot_overwrite_a_modelled_field() -> Result<(), Failure> {
        let metadata = metadata()?;

        assert_matches!(
            metadata.with_field("redirect_uris", serde_json::json!(["https://evil.example.com"])),
            Err(ClientMetadataError::ReservedField(ref name)) if name == "redirect_uris"
        );
        Ok(())
    }

    /// `2025-11-25` and later name `client_uri`, `icon_uri` and `title`. A
    /// client that publishes one and has it dropped at a struct boundary has
    /// registered something other than what it meant to.
    #[test]
    fn an_unmodelled_field_reaches_the_wire() -> Result<(), Failure> {
        let metadata = metadata()?
            .with_field("client_uri", "https://client.example.com")?
            .with_field("title", "Example Client")?;

        let document = serde_json::to_value(&metadata)
            .map_err(|_| Failure::Registration(RegistrationError::NoMechanism))?;

        assert_eq!(
            document.get("client_uri"),
            Some(&serde_json::json!("https://client.example.com"))
        );
        assert_eq!(
            document.get("title"),
            Some(&serde_json::json!("Example Client"))
        );
        Ok(())
    }

    /// The named fields and the passthrough coexist on the wire without either
    /// shadowing the other.
    #[test]
    fn modelled_and_extra_fields_coexist() -> Result<(), Failure> {
        let metadata = metadata()?
            .with_application_type("native")
            .with_field("title", "Example")?;

        let document = serde_json::to_value(&metadata)
            .map_err(|_| Failure::Registration(RegistrationError::NoMechanism))?;

        assert_eq!(
            document.get("application_type"),
            Some(&serde_json::json!("native"))
        );
        assert_eq!(
            document.get("client_name"),
            Some(&serde_json::json!("Example"))
        );
        assert_eq!(document.get("title"), Some(&serde_json::json!("Example")));
        assert_eq!(
            document.get("redirect_uris"),
            Some(&serde_json::json!(["https://client.example.com/callback"]))
        );
        Ok(())
    }
}
