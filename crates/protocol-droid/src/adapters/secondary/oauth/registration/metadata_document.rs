//! Fetching a Client ID Metadata Document: the client publishes its identity
//! at a URL and the authorization server reads it.
//!
//! # Why the fetched document is not trusted
//!
//! This is the part of client registration where a failure is silent and
//! upstream. Dynamic registration hands back a credential the server minted;
//! a metadata document is something the *client* wrote and anyone on the path
//! could have rewritten. If a document served at
//! `https://client.example.com/metadata.json` claims
//! `"client_id": "https://attacker.example.com/metadata.json"`, then every
//! authorization server that fetches it sees a document that looks complete and
//! identifies the wrong party — and nothing downstream can detect that,
//! because nothing downstream re-reads the URL.
//!
//! So the check is here, and it is the one the draft requires: the document's
//! `client_id` must be the URL it was served from. That is
//! [`ClientId::verify_metadata_document`], and a document that fails it
//! surfaces as [`RegistrarError::Document`]. The use case treats that failure
//! differently from a fetch failure — it does **not** fall back to dynamic
//! registration — because a document that is present and wrong is a forgery,
//! and falling back would let an impersonated client quietly register a fresh
//! identity rather than surface the problem.
//!
//! # Why no secret is produced
//!
//! The draft is explicit that no shared secret can be established for this
//! form. There is no code path here that could produce one: the only
//! constructor used is [`RegisteredClient::from_metadata_document`], which
//! takes no secret parameter to get wrong.

use serde::Deserialize;

use crate::application::oauth::registration::{MetadataDocumentLoader, RegistrarError};
use crate::domain::oauth::registration::{ClientId, RegisteredClient};

/// The most this module will read from one metadata document.
///
/// A document is a handful of URLs and short strings. The cap is here so a
/// server that will not stop sending cannot turn one fetch into an allocation
/// of whatever size it likes — and the URL being fetched is one the client
/// chose, so the server is only half-trusted at best.
const MAX_DOCUMENT_BYTES: usize = 64 * 1024;

/// Fetches published Client ID Metadata Documents.
#[derive(Debug, Clone)]
pub struct PublishedDocumentLoader {
    http: reqwest::Client,
}

impl PublishedDocumentLoader {
    /// A registrar with no configuration.
    #[must_use]
    pub fn new() -> Self {
        Self {
            // Built once for the same reason as every other adapter here: a
            // per-request TLS handshake and a fresh pool are not something to
            // pay during registration.
            http: reqwest::Client::new(),
        }
    }

    /// Replaces the HTTP client, for a caller that needs a timeout, a proxy or
    /// custom TLS settings.
    #[must_use]
    pub fn with_http_client(mut self, http: reqwest::Client) -> Self {
        self.http = http;
        self
    }
}

impl Default for PublishedDocumentLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl MetadataDocumentLoader for PublishedDocumentLoader {
    async fn fetch_metadata_document(
        &self,
        issuer: &str,
        client_id: &ClientId,
    ) -> Result<RegisteredClient, RegistrarError> {
        // RFC 7591 §3.1 and CIMD both use GET, and both expect
        // `application/json`. `ClientId::metadata_url` already refused
        // anything that is not `https`, so this GET cannot be redirected by a
        // caller into fetching an arbitrary scheme.
        let response = self
            .http
            .get(client_id.as_str())
            .send()
            .await
            .map_err(|failure| RegistrarError::Unavailable(failure.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            return Err(RegistrarError::Refused {
                status: status.as_u16(),
            });
        }

        let body = read_capped(response).await?;
        document(issuer, client_id, &body)
    }
}

/// Turn a fetched document into a client, after checking what it claims.
///
/// Pure, and separate from the HTTP above for the same reason
/// [`claims_from`](crate::adapters::secondary::oauth::introspection) is: the
/// check the whole module exists for lives in how a document is *read*, and it
/// is worth testing without a socket in the way.
///
/// # Errors
///
/// [`RegistrarError::Malformed`] if the body is not a metadata document, or
/// [`RegistrarError::Document`] if the document's `client_id` is not the URL it
/// was served from. The two stay distinct because the use case reacts
/// differently: a malformed document can fall back to dynamic registration, a
/// document that is present and wrong cannot.
fn document(
    issuer: &str,
    client_id: &ClientId,
    body: &[u8],
) -> Result<RegisteredClient, RegistrarError> {
    let document: Document = serde_json::from_slice(body)
        .map_err(|failure| RegistrarError::Malformed(failure.to_string()))?;

    // The check the whole module exists for. Nothing downstream can re-detect
    // a forged document, because nothing downstream re-reads the URL.
    client_id
        .verify_metadata_document(&document.client_id)
        .map_err(RegistrarError::Document)?;

    Ok(RegisteredClient::from_metadata_document(
        issuer,
        client_id.clone(),
    ))
}

/// The document as it arrives.
///
/// Private, and separate from anything in `application/`: this is a wire shape
/// with one field this crate reads and a great many it does not. Unknown
/// fields are ignored here rather than preserved, because the document is
/// already published — the client holds the authoritative copy, and this crate
/// is a reader of it, not a relay.
#[derive(Debug, Deserialize)]
struct Document {
    /// The identifier the document claims. Verified against the URL it came
    /// from before anything is built from it.
    client_id: String,
}

/// Reads a response body, stopping if it grows past [`MAX_DOCUMENT_BYTES`].
async fn read_capped(mut response: reqwest::Response) -> Result<Vec<u8>, RegistrarError> {
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|failure| RegistrarError::Unavailable(failure.to_string()))?
    {
        if body.len().saturating_add(chunk.len()) > MAX_DOCUMENT_BYTES {
            return Err(RegistrarError::Malformed(format!(
                "the metadata document exceeded {MAX_DOCUMENT_BYTES} bytes",
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
