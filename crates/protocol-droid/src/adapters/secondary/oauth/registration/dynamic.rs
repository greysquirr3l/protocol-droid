//! RFC 7591 Dynamic Client Registration: POST the metadata, receive a
//! credential.
//!
//! # What the response is trusted to say, and what it is not
//!
//! The server is trusted to have issued the identifier it returned — that is
//! the entire point of registration. It is not trusted to have returned
//! something coherent. RFC 7591 §3.2.1 lets the server assign a `client_id`,
//! and a server that answers with a URL instead has handed back a *document*
//! identifier, which means no shared secret and a different verification
//! entirely. [`RegisteredClient::dynamic`] refuses that combination rather
//! than producing a credential whose `client_id` this adapter has no way to
//! authenticate.
//!
//! A `client_secret` is optional on the wire (RFC 7591 §3.2.1: issued when the
//! server requires authentication at the token endpoint) and is kept optional
//! here for the same reason. A public client that registers with
//! `token_endpoint_auth_method: "none"` gets no secret, and inventing an empty
//! one would make the client offer something the server never issued.
//!
//! # What is not logged
//!
//! `client_secret` is a credential that exists nowhere else and is not
//! recoverable — losing it means re-registering. Every error this module
//! produces names a status or a parse failure and never the body.

use std::sync::Arc;

use serde::Deserialize;

use crate::application::oauth::registration::{ClientMetadata, DynamicRegistrar, RegistrarError};
use crate::domain::oauth::registration::{ClientId, ClientSecret, RegisteredClient};

/// The most this module will read from one registration response.
///
/// An authorization server is not trusted to bound its own answer. 64 KiB is
/// far more than any legal response: the document is a handful of short
/// identifiers. The cap exists so a compromised or misbehaving server cannot
/// turn one registration into an allocation of whatever size it likes.
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

/// Registers clients through RFC 7591 §3.1.
#[derive(Debug, Clone)]
pub struct Rfc7591Registrar {
    endpoint: Arc<str>,
    http: reqwest::Client,
}

impl Rfc7591Registrar {
    /// A registrar pointed at `endpoint`, normally a `client_id`-adjacent
    /// registration endpoint discovered from authorization server metadata.
    ///
    /// Opens no socket and reads no clock, so a typo in `endpoint` surfaces on
    /// the first call rather than at construction — unlike the MCP client, the
    /// endpoint here belongs to an authorization server, which a deployment
    /// discovers from a metadata document rather than writes.
    #[must_use]
    pub fn new(endpoint: impl Into<Arc<str>>) -> Self {
        Self {
            endpoint: endpoint.into(),
            // Built once so a per-request TLS handshake and a fresh pool do not
            // happen during registration. No timeout is set here for the same
            // reason as every other adapter in this crate: how patient to be is
            // a deployment's decision, and `with_http_client` is where it is
            // expressed.
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

    /// The endpoint this registrar posts to.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

impl DynamicRegistrar for Rfc7591Registrar {
    async fn register_dynamic(
        &self,
        issuer: &str,
        metadata: &ClientMetadata,
    ) -> Result<RegisteredClient, RegistrarError> {
        // RFC 7591 §3.1: POST with `Content-Type: application/json`. The header is
        // set by hand rather than by `reqwest`'s `.json()` because this
        // workspace builds `reqwest` with `default-features = false` and only
        // `rustls-tls` and `stream` — `json` is off to keep the dependency
        // surface small, and one header is not worth turning it back on. The
        // passthrough fields in `ClientMetadata` still reach the wire exactly as
        // set, because the whole document is what gets serialized.
        let body = serde_json::to_vec(metadata)
            .map_err(|failure| RegistrarError::Malformed(failure.to_string()))?;

        let response = self
            .http
            .post(&*self.endpoint)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|failure| RegistrarError::Unavailable(failure.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            // RFC 7591 §3.2.2 makes a registration failure a 4xx with an
            // `error` field, and §3.2.1 says a server that cannot be
            // registered at all returns 400. All of them are refusals of the
            // request, not of the client, so the status is what survives.
            return Err(RegistrarError::Refused {
                status: status.as_u16(),
            });
        }

        let body = read_capped(response).await?;
        credential(issuer, &body)
    }
}

/// Turn a registration response body into a credential.
///
/// Pure, and separate from the HTTP above for the same reason
/// [`claims_from`](crate::adapters::secondary::oauth::introspection) is: the
/// rules worth testing are in how a response is *read*, and a round trip would
/// only add a place for them to hide.
///
/// # Errors
///
/// [`RegistrarError::Malformed`] if the body is not a registration response, or
/// [`RegistrarError::Document`] if `client_id` is a metadata document URL —
/// which `RegisteredClient::dynamic` refuses, because a credential that has no
/// verifiable secret behind it is not what dynamic registration produces.
fn credential(issuer: &str, body: &[u8]) -> Result<RegisteredClient, RegistrarError> {
    let parsed: RegistrationResponse = serde_json::from_slice(body)
        .map_err(|failure| RegistrarError::Malformed(failure.to_string()))?;

    Ok(RegisteredClient::dynamic(
        issuer,
        ClientId::opaque(&parsed.client_id),
        parsed.client_secret.map(ClientSecret::issued),
    )?)
}

/// The RFC 7591 §3.2.1 registration response.
///
/// Kept private and separate from [`RegisteredClient`] because it is a wire
/// shape: `client_id` is a plain string here and a validated value there, and
/// the optional secret is decided by what the server issued rather than by what
/// this client asked for.
#[derive(Debug, Deserialize)]
struct RegistrationResponse {
    /// The identifier the server issued.
    client_id: String,
    /// Issued only when the server requires authentication at the token
    /// endpoint.
    #[serde(default)]
    client_secret: Option<String>,
}

/// Reads a response body, stopping if it grows past [`MAX_RESPONSE_BYTES`].
async fn read_capped(mut response: reqwest::Response) -> Result<Vec<u8>, RegistrarError> {
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|failure| RegistrarError::Unavailable(failure.to_string()))?
    {
        // `saturating_add` because the running total is server-controlled: a
        // length near `usize::MAX` must not wrap into a small number and slip
        // past the check.
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(RegistrarError::Malformed(format!(
                "the registration response exceeded {MAX_RESPONSE_BYTES} bytes",
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
