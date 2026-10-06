//! Talking to a token endpoint over HTTP.
//!
//! RFC 6749 §4.1.3: form-encode the exchange, read back §5.1 or §5.2. What
//! this adapter deliberately does **not** do is retry.
//!
//! # No retries, and not because retries are fragile
//!
//! RFC 6749 §4.1.2 makes an authorization code single-use, so a retry after a
//! dropped connection may arrive *after* the server honoured the first attempt
//! and be refused with `invalid_grant`. A client that retried automatically
//! would then report a failure for a code that had already been spent, and the
//! only way to get a token would be to make the user authorize again. Not
//! retrying is the behaviour that leaves a recoverable situation recoverable.
//!
//! # Client authentication goes in the body
//!
//! RFC 6749 §2.3.1 permits HTTP Basic, `client_secret_post`, or both. This
//! adapter uses the body, and the reason is [`TokenRequest::client_secret`]'s
//! contract: a public client registered through a client identity metadata
//! document has no secret, and the member must be *absent* rather than empty —
//! its presence is the statement that the client is confidential. Basic auth
//! cannot express that without a scheme Basic auth does not have.

use std::sync::Arc;

use crate::adapters::secondary::oauth::http_body::{describe, read_capped};
use crate::application::oauth::redeem::{TokenEndpoint, TokenEndpointError, TokenRequest};
use crate::domain::oauth::issued_token::{IssuedToken, TokenResponseError};

/// Redeems authorization codes at one token endpoint.
///
/// [`Clone`] because a caller will hold this across the user-visible round trip
/// — or hand it to something that outlives the call that built it — and the
/// reqwest client underneath pools connections worth sharing.
#[derive(Debug, Clone)]
pub struct HttpTokenEndpoint {
    endpoint: Arc<str>,
    http: reqwest::Client,
}

impl HttpTokenEndpoint {
    /// An endpoint to post token requests to.
    ///
    /// The HTTP client is built once, so a per-request TLS handshake and a
    /// fresh pool do not happen on the authorization path. No timeout is set
    /// here: an endpoint that stops answering must eventually be reported as
    /// unavailable, and the transport's own timeout is where a deployment
    /// decides how patient to be.
    #[must_use]
    pub fn new(endpoint: impl AsRef<str>) -> Self {
        Self {
            endpoint: Arc::from(endpoint.as_ref()),
            http: reqwest::Client::new(),
        }
    }

    /// Replaces the HTTP client, for a caller that needs a timeout, a proxy or
    /// custom TLS settings.
    ///
    /// There is deliberately no `with_timeout` shortcut: `reqwest`'s builder
    /// returns a `Result`, and a shortcut would either swallow that error or
    /// force every caller through a `Result` for a knob they can already reach.
    #[must_use]
    pub fn with_http_client(mut self, http: reqwest::Client) -> Self {
        self.http = http;
        self
    }

    /// The endpoint this one posts to.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

impl TokenEndpoint for HttpTokenEndpoint {
    async fn redeem(&self, request: TokenRequest<'_>) -> Result<IssuedToken, TokenEndpointError> {
        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("code", request.code()),
            ("redirect_uri", request.redirect_uri()),
            ("code_verifier", request.code_verifier().as_str()),
            ("client_id", request.client_id().as_str()),
        ];

        // Absent for a public client. See the module docs: an empty member is a
        // value, and §2.3.1 reads the member's presence as a claim of
        // confidentiality.
        if let Some(secret) = request.client_secret() {
            form.push(("client_secret", secret.expose()));
        }

        // RFC 8707 §2: one `resource` member per indicator, repeated rather
        // than joined.
        for resource in request.resource().iter() {
            form.push(("resource", resource));
        }

        let response = self
            .http
            .post(&*self.endpoint)
            .form(&form)
            .send()
            .await
            .map_err(|failure| TokenEndpointError::Unavailable(describe(failure)))?;

        let status = response.status();
        let body = read_capped(response)
            .await
            .map_err(TokenEndpointError::Unavailable)?;

        // The body's own shape decides, not the status. A server answering 200
        // with a §5.2 error body must not be read as a token, and one
        // answering 400 with a §5.1 body is broken in a way the status alone
        // does not describe. `IssuedToken::parse` checks for the `error` member
        // first, so this is a single decision rather than two that can disagree.
        if !status.is_success() {
            // Try to recover the server's own reason, because `invalid_grant`
            // and `invalid_client` call for different responses from the caller
            // and a bare status code distinguishes neither. A body that is not
            // a §5.2 error at all still falls back to the status.
            return match IssuedToken::parse(&body) {
                Err(refused @ TokenResponseError::AuthorizationServer { .. }) => {
                    Err(TokenEndpointError::Refused(refused))
                }
                _ => Err(TokenEndpointError::Unavailable(format!(
                    "token endpoint answered {}",
                    status.as_u16(),
                ))),
            };
        }

        IssuedToken::parse(&body).map_err(TokenEndpointError::Refused)
    }
}

#[cfg(test)]
mod tests;
