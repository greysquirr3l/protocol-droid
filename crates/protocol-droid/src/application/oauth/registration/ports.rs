//! What a deployment can reach, and the ports that get it there.
//!
//! Two ways to become a registered client: RFC 7591 dynamic registration,
//! where the client POSTs its metadata and the server issues a credential,
//! and the Client ID Metadata Document, where the client publishes a
//! document and asserts nothing. They differ in what they send and in what
//! they establish, so they are two ports rather than one with a mode flag.
//!
//! [`Available`] is the separate question of whether either is wired up at
//! all. It is an argument to [`plan_registration`](super::plan_registration)
//! rather than something the handler reads off the identity, because the two
//! fail independently — a client can publish a document to a URL that
//! resolves nowhere, and a deployment can wire a dynamic registrar while its
//! client has no metadata to present.

use super::{ClientMetadata, RegistrarError};
use crate::domain::oauth::registration::{ClientId, RegisteredClient};

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
/// A separate argument to [`plan_registration`](super::plan_registration) rather than something it reads
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
