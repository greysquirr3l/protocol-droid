//! Choosing a client registration mechanism and carrying it out.
//!
//! MCP changed its mind about this twice. `2025-03-26` and `2025-06-18` name
//! RFC 7591 and nothing else; `2025-11-25` adds Client ID Metadata Documents as
//! *recommended* without deprecating RFC 7591; `2026-07-28` deprecates RFC 7591
//! "in favor of" documents while keeping it for servers that implement nothing
//! else. Which of those a client must do is a pure function of the negotiated
//! revision, and it lives in
//! [`ClientRegistration`](crate::domain::mcp::authorization::ClientRegistration).
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

mod plan;
mod ports;

#[cfg(test)]
mod tests;

// The ports and the use case are separate modules because they are separate
// questions: `plan_registration` decides whether a client can be registered,
// and the traits say what a deployment has to offer for that to happen. The
// public items are re-exported here so that
// `application::oauth::registration::DynamicRegistrar` still means what it
// did when the module was one file.
pub use plan::{acquire_client, plan_registration};
pub use ports::{Available, DynamicRegistrar, MetadataDocumentLoader};

use crate::domain::oauth::registration::{
    ClientId, ClientIdError, RegisteredClient,
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
