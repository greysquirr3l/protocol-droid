//! JWT access tokens, checked against the authorization server's published
//! keys (RFC 7517 JWKS).
//!
//! The counterpart to [`introspection`](super::introspection), and the one
//! that costs no round trip per request. The trade is the usual one: the
//! resource server decides for itself whether a token is good, so a key the
//! authorization server has revoked is still good here until the token expires
//! or the key is withdrawn.
//!
//! # What this source checks, and what it leaves to someone else
//!
//! It establishes **authenticity** and nothing else. The audience, the clock,
//! the issuer and the scopes are [`verify`](crate::application::oauth::verify)'s
//! job, which is why [`JwtSource::claims`] returns a [`TokenClaims`] and not a
//! [`AuthContext`](crate::domain::registry::AuthContext) — the same two halves
//! [`IntrospectionSource`](super::introspection::IntrospectionSource) is built
//! from, so a deployment can swap one for the other without the rules moving.
//!
//! The one thing this source does check beyond the signature is the JWS
//! structure itself, because a token can fail there before any key exists: see
//! [`JwtSource::claims`] for the `crit` and embedded-`jwk` refusals.
//!
//! # The token is never in an error
//!
//! Every [`SourceError`] this module produces carries a status, a reason or a
//! [`Refusal`]. None of them borrows from the token, none of them formats it,
//! and `verify` sees only claims. A bearer credential that reaches a log line
//! through a `Display` impl is a credential that has to be rotated.

mod claims;
mod key_set;

use std::sync::Arc;

use jsonwebtoken::{Algorithm, Validation, decode, decode_header};

use crate::application::oauth::{Refusal, SourceError, TokenClaims, TokenSource};

use self::claims::JwtClaims;
use self::key_set::{KeyFailure, KeySet};

/// Verifies JWT access tokens against a published key set.
#[derive(Debug, Clone)]
pub struct JwtSource {
    keys: KeySet,
}

impl JwtSource {
    /// A source reading keys from `jwks_uri`.
    ///
    /// Opens no socket and reads no clock, so a source can be built at startup
    /// and the first fetch happens on the first request that needs it. A
    /// deployment that would rather pay that at startup has
    /// [`warm`](Self::warm).
    ///
    /// `accepted` is the algorithms this deployment will verify. It is a
    /// parameter rather than a default because the safe default is not obvious:
    /// a list that is too long accepts a downgrade, and a list that is too short
    /// refuses a signer that is working. `RS256` alone is the common answer,
    /// and it is what [`default_algorithms`] returns.
    #[must_use]
    pub fn new(jwks_uri: impl Into<Arc<str>>, accepted: Vec<Algorithm>) -> Self {
        Self {
            keys: KeySet::new(jwks_uri, accepted),
        }
    }

    /// Replaces the HTTP client, for a caller that needs a timeout or a proxy.
    ///
    /// There is no `with_timeout` shortcut: `reqwest`'s builder returns a
    /// `Result`, and a shortcut would either swallow that error or force every
    /// caller through a `Result` for a knob they can already reach.
    #[must_use]
    pub fn with_http_client(mut self, http: reqwest::Client) -> Self {
        self.keys = self.keys.with_http_client(http);
        self
    }

    /// Forgets the cached keys so the next request fetches again.
    ///
    /// For a deployment told out of band that the authorization server rotated
    /// its keys, rather than waiting out the refresh floor.
    pub fn invalidate(&self) {
        self.keys.invalidate();
    }

    /// Fetches the key set now, so a missing authorization server is a startup
    /// failure rather than a `401` under the first real request.
    ///
    /// # Errors
    ///
    /// A transport or parse failure. The reason is carried; nothing borrowed
    /// from a token is, because no token has been involved yet.
    pub async fn warm(&self) -> Result<(), SourceError> {
        self.keys.warm().await.map_err(|failure| match failure {
            KeyFailure::Unreadable(reason) => SourceError::Unavailable(reason),
            // `warm` never selects a key, so this arm is unreachable here
            // rather than impossible, and reporting it as unavailable is the
            // conservative reading either way.
            KeyFailure::NoKey(error) => SourceError::Unavailable(error.to_string()),
        })
    }

    /// The algorithms this source will verify.
    #[must_use]
    pub fn accepted_algorithms(&self) -> &[Algorithm] {
        self.keys.accepted()
    }
}

/// The algorithms [`JwtSource`] verifies when a deployment expresses no
/// preference.
///
/// `RS256` alone. Not because the others are unsafe, but because every entry
/// on this list is one an attacker would try, and a deployment that genuinely
/// signs `RS512` or `ES256` can say so and take on the review that implies.
#[must_use]
pub fn default_algorithms() -> Vec<Algorithm> {
    vec![Algorithm::RS256]
}

impl TokenSource for JwtSource {
    async fn claims(&self, presented: &str) -> Result<TokenClaims, SourceError> {
        let header = decode_header(presented).map_err(|_| Refusal::Malformed)?;

        // RFC 7515 §4.1.11: a `crit` header naming an extension the
        // recipient does not understand makes the JWS invalid. This crate
        // implements none, so any value here is one to refuse — and
        // `jsonwebtoken` parses the field without acting on it, which means
        // nothing downstream would catch it.
        if header.crit.as_ref().is_some_and(|crit| !crit.is_empty()) {
            return Err(Refusal::Malformed.into());
        }
        // RFC 7515 §4.1.3 lets the token carry its own key. It is ignored
        // here — see `key_set` — and saying so at the point the header is
        // in hand is what keeps it ignored if that module is ever edited.
        if header.jwk.is_some() {
            return Err(Refusal::Malformed.into());
        }

        // One forced refetch per unknown `kid`, which is what makes key
        // rotation work. `KeySet` rate-limits it, so a caller naming a
        // random `kid` on every request does not get a fetch every request.
        let key = self
            .keys
            .key_for(&header, true)
            .await
            .map_err(map_key_failure)?;

        // Every claim check is switched off here on purpose. `exp`, `nbf`
        // and `aud` are `verify`'s to apply, and running them twice would
        // mean the introspection source and this one disagreeing about
        // which clock and which resource — with the JWT path holding the
        // stricter of the two and no test saying so.
        let mut validation = Validation::new(header.alg);
        validation.validate_exp = false;
        validation.validate_nbf = false;
        validation.validate_aud = false;
        // Default is `{exp}`, which would refuse a token with no expiry
        // while the introspection path accepts whatever the authorization
        // server chose to say. `verify` owns that decision.
        validation.required_spec_claims.clear();

        let payload = decode::<JwtClaims>(presented, &key, &validation)
            .map_err(|_| Refusal::Malformed)?
            .claims;

        Ok(payload.into_token_claims())
    }
}

/// Maps a key-selection failure onto the two errors a caller can act on.
///
/// Two outcomes, and the split between them is the whole point of
/// [`SourceError`]. A key set that could not be read is
/// [`SourceError::Unavailable`] — nothing was decided, and the caller should
/// not be sent to re-authenticate against an authorization server that is
/// merely unreachable. A token no published key may verify is
/// [`Refusal::Unverified`]: it parsed, and this server declines to vouch for
/// it.
///
/// The specific [`KeyError`] is deliberately not carried past this point. Every
/// one of them answers the same question — may this token be used, no — and
/// `Refusal` is what a caller matches on, so giving seven reasons to a two-arm
/// decision would only move the choice of which to log to the log site.
fn map_key_failure(failure: KeyFailure) -> SourceError {
    match failure {
        KeyFailure::Unreadable(reason) => SourceError::Unavailable(reason),
        KeyFailure::NoKey(_) => Refusal::Unverified.into(),
    }
}

#[cfg(test)]
#[path = "jwt/tests.rs"]
mod tests;

#[cfg(test)]
impl JwtSource {
    /// Installs a key set without fetching, for `jwt::tests`.
    ///
    /// Not on the public API: a source that takes a key set would be a source
    /// that does not fetch, which is a different type wearing this one's name.
    pub(super) fn seed(&self, keys: jsonwebtoken::jwk::JwkSet) {
        self.keys.seed(keys);
    }
}
