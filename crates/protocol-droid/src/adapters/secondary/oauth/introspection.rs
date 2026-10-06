//! RFC 7662 token introspection: asking the authorization server what a token
//! means.
//!
//! The counterpart to the JWT/JWKS source, and the one that needs no crypto of
//! its own — the authorization server did the signing, so asking it is a
//! complete authenticity check. It costs a round trip per request, which is
//! the trade against local signature verification.
//!
//! # What is not trusted
//!
//! RFC 7662 §2.2 is emphatic that an inactive token's response "**MUST NOT**"
//! carry other information, and the obvious abuse is a server that returns
//! `active: false` alongside a plausible `aud`, `exp` and `scope` — an answer
//! shaped for a client that reads fields before reading `active`. So
//! [`claims_from`] returns at the first `active: false` and never looks at
//! anything after it.
//!
//! A *missing* `active` is a third case, and neither of the obvious ones. It is
//! not a refusal — the token is not known to be bad, and the caller must not be
//! sent off to re-authenticate. It is not a default either — absent means nobody
//! said yes. It is a malfunctioning authorization server, so it surfaces as
//! [`SourceError::Unavailable`] and says exactly that.
//!
//! # What is not logged
//!
//! No error this module produces contains the token. The request is a POST body
//! field precisely because the token is a bearer credential, and the one place
//! it could leak is a formatted message — so [`SourceError`] carries statuses
//! and reasons, and nothing borrows from the token.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::adapters::secondary::oauth::http_body::{describe, read_capped};
use crate::application::oauth::{Refusal, SourceError, TokenClaims, TokenSource};

/// Asks an authorization server, over HTTP, what a presented token means.
#[derive(Debug, Clone)]
pub struct IntrospectionSource {
    endpoint: Arc<str>,
    client_id: Arc<str>,
    client_secret: Arc<str>,
    http: reqwest::Client,
}

impl IntrospectionSource {
    /// A source pointed at `endpoint`.
    ///
    /// Opens no socket and reads no clock, so a typo in `endpoint` surfaces on
    /// the first call rather than at construction — unlike the MCP client, the
    /// endpoint here is an authorization server's, which a deployment discovers
    /// from a metadata document rather than writes.
    #[must_use]
    pub fn new(
        endpoint: impl Into<Arc<str>>,
        client_id: impl Into<Arc<str>>,
        client_secret: impl Into<Arc<str>>,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            // Built once so a per-request TLS handshake and a fresh pool do not
            // happen on the authorization path. No timeout is set here: an
            // endpoint that stops answering must eventually be reported as
            // unavailable, and the transport's own timeout is where a
            // deployment decides how patient to be.
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

    /// The endpoint this source asks.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

impl TokenSource for IntrospectionSource {
    async fn claims(&self, presented: &str) -> Result<TokenClaims, SourceError> {
        // RFC 7662 §2.1: form-encoded, with the token as a body field and the
        // client authenticating per RFC 6749 §2.3.1. `token_type_hint` is a
        // hint, not a filter — an authorization server is free to ignore it,
        // and one that misimplements it as a filter would answer 401 for a
        // perfectly good token.
        let form = [("token", presented), ("token_type_hint", "access_token")];

        let response = self
            .http
            .post(&*self.endpoint)
            .basic_auth(&*self.client_id, Some(&*self.client_secret))
            .form(&form)
            .send()
            .await
            .map_err(|failure| SourceError::Unavailable(describe(failure)))?;

        let status = response.status();
        if !status.is_success() {
            // Deliberately *not* a refusal. A 401 here is this resource server
            // failing to authenticate *to* the authorization server — a
            // misconfigured `client_id` or `client_secret` — and telling the
            // end user's client that its token was rejected sends it off to
            // re-authenticate against a server that is working fine.
            return Err(SourceError::Unavailable(format!(
                "introspection responded {}",
                status.as_u16(),
            )));
        }

        let body = read_capped(response)
            .await
            .map_err(SourceError::Unavailable)?;
        let parsed: IntrospectionResponse = serde_json::from_slice(&body)
            .map_err(|failure| SourceError::Unavailable(describe(failure)))?;

        claims_from(parsed)
    }
}

// The bounded read and the credential-free failure text now live in
// `http_body`, shared with the redemption adapter. A size cap copied into two
// files is a cap that will eventually be fixed in one of them.

/// The RFC 7662 response, as it arrives.
///
/// Kept private and separate from [`TokenClaims`] because it is a wire shape:
/// `active` is required, `aud` is string-or-array, and the times are unix
/// seconds. It is turned into claims exactly once, by [`claims_from`].
#[derive(Debug, Deserialize)]
struct IntrospectionResponse {
    /// Whether the token is currently valid.
    ///
    /// `Option` rather than `bool` only so a *missing* field can be told apart
    /// from a malformed one in [`claims_from`]. RFC 7662 §2.2 makes it
    /// required, and the absence is handled as a malfunction rather than as a
    /// serde error, because the two deserve different log lines.
    active: Option<bool>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    iss: Option<String>,
    #[serde(default)]
    sub: Option<String>,
    #[serde(default)]
    aud: Option<Audience>,
    #[serde(default)]
    exp: Option<u64>,
    #[serde(default)]
    nbf: Option<u64>,
}

/// RFC 7519 §4.1.3: `aud` is a string or an array of them.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

/// Turns one introspection response into claims, or refuses.
///
/// Split out from the HTTP so the rule that matters — `active` gates
/// everything — is testable without a socket, and so the next source (JWKS)
/// cannot accidentally acquire a laxer version of it.
///
/// # Errors
///
/// [`SourceError::Unavailable`] when the response does not say whether the token
/// is active at all, and [`SourceError::Refused`] when it says it is not. Every
/// other absence is tolerated: RFC 7662 allows a response to omit any field but
/// `active`, and [`verify`](crate::application::oauth::verify) is the stage that
/// decides whether what is left is enough.
fn claims_from(response: IntrospectionResponse) -> Result<TokenClaims, SourceError> {
    match response.active {
        Some(true) => {}
        Some(false) => {
            // Nothing after this point is read. RFC 7662 §2.2 says an inactive
            // token's response carries no other information, and a server that
            // sent some anyway is one a client must not believe.
            return Err(SourceError::Refused(Refusal::Unscoped));
        }
        None => {
            return Err(SourceError::Unavailable(
                "the introspection response has no `active` field".to_owned(),
            ));
        }
    }

    let mut claims = TokenClaims::new();
    if let Some(subject) = response.sub.or(response.username) {
        claims = claims.with_subject(subject);
    }
    if let Some(issuer) = response.iss {
        claims = claims.with_issuer(issuer);
    }
    match response.aud {
        Some(Audience::One(one)) => claims = claims.with_audience_string(one),
        Some(Audience::Many(many)) => {
            for audience in many {
                claims = claims.with_audience_string(audience);
            }
        }
        None => {}
    }
    if let Some(exp) = response.exp.and_then(system_time) {
        claims = claims.with_expiry(exp);
    }
    if let Some(nbf) = response.nbf.and_then(system_time) {
        claims = claims.with_not_before(nbf);
    }
    if let Some(scope) = response.scope {
        // RFC 6749 §3.3: space-delimited, not comma-delimited. Splitting on
        // commas would fold two scopes into one that the authorization server
        // never issued.
        for scope in scope.split(' ') {
            if !scope.is_empty() {
                claims = claims.with_scope(scope);
            }
        }
    }
    Ok(claims)
}

/// Unix seconds to a [`SystemTime`], or `None` for a value that cannot be one.
fn system_time(seconds: u64) -> Option<SystemTime> {
    UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
}

#[cfg(test)]
#[path = "introspection/tests.rs"]
mod tests;
