//! The claims a JWS access token carries, and the mapping onto
//! [`TokenClaims`].
//!
//! This module deliberately reads *nothing*. It converts a verified payload
//! into the shape `verify` wants, and every rule about whether that payload is
//! acceptable — the audience, the clock, the issuer, the scopes — belongs to
//! `verify`, which is the only place those rules are written and the only place
//! the introspection source has to agree with.
//!
//! # Three shapes that are each specified more than once
//!
//! **`aud`** is a string in RFC 7519 §4.1.3's own example and an array of
//! strings in its general case, and both occur in the wild. [`Audience`] is
//! untagged for that reason, and neither spelling is preferred.
//!
//! **`scope`** is a space-delimited string in RFC 6749 §3.3 and an array of
//! strings in RFC 9068 §2.2.3, which is the JWT access token profile and the
//! one a server advertising `token_endpoint_auth_methods_supported` is likely
//! to follow. Splitting on `' '` rather than on [`char::is_whitespace`] is not
//! a detail: RFC 6749's ABNF is `1*( %x21 / %x23-5B / %x5D-7E )`, so a tab is
//! not a separator and treating one as one would invent a scope nobody issued.
//!
//! **`scp`** is the same claim under the name Microsoft Entra ID issues it
//! under. A delegated v2.0 token carries its granted permissions in `scp` and
//! no `scope` at all, so a parser that reads only `scope` sees no permissions
//! and refuses a perfectly good token for being unscoped. Both are read, and
//! their contents are unioned.
//!
//! **`exp` and `nbf`** are `NumericDate`: seconds since the epoch, possibly
//! fractional. They become `SystemTime` here so that the comparison in `verify`
//! is the same comparison the introspection source's integers go through.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::application::oauth::TokenClaims;

/// A verified JWS payload, restricted to the claims this crate reads.
#[derive(Debug, Default, Deserialize)]
pub(super) struct JwtClaims {
    /// `sub` — who the token is about. RFC 7519 §4.1.2.
    #[serde(default)]
    sub: Option<String>,
    /// `iss` — who minted it. RFC 7519 §4.1.1.
    #[serde(default)]
    iss: Option<String>,
    /// `aud` — what it is for. Checked against this server's canonical
    /// resource URI by `verify`, never here.
    #[serde(default)]
    aud: Option<Audience>,
    /// `exp` — RFC 7519 §4.1.4.
    #[serde(default)]
    exp: Option<f64>,
    /// `nbf` — RFC 7519 §4.1.5.
    #[serde(default)]
    nbf: Option<f64>,
    /// `scope` — the granted permissions.
    #[serde(default)]
    scope: Option<Scopes>,
    /// `scp` — the granted permissions, under Microsoft's name for them.
    ///
    /// Microsoft Entra ID issues delegated v2.0 tokens with the permissions in
    /// `scp` and no `scope` claim at all, so a parser that reads only `scope`
    /// concludes the token carries nothing and refuses it for being unscoped.
    /// `scp` is `scope` under another name: the same space-delimited list, from
    /// the same OAuth 2.0 grant.
    #[serde(default)]
    scp: Option<Scopes>,
}

/// RFC 7519 §4.1.3, which is "a string or an array of strings".
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Audience {
    /// One audience.
    One(String),
    /// Several. Equality against any element is what the RFC means.
    Many(Vec<String>),
}

/// RFC 6749 §3.3 and RFC 9068 §2.2.3, which do not agree.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Scopes {
    /// RFC 6749 §3.3: one string, `%x20`-separated.
    Delimited(String),
    /// RFC 9068 §2.2.3: an array.
    Listed(Vec<String>),
}

/// The space delimiter RFC 6749 §3.3 permits, as opposed to any whitespace.
const DELIMITER: char = ' ';

impl JwtClaims {
    /// Converts to the claim shape `verify` applies its rules to.
    ///
    /// An `aud` element that is not a parseable resource URI is dropped by
    /// [`TokenClaims::with_audience_string`] rather than failing the token, and
    /// that is the right direction: dropping can only ever *remove* a candidate
    /// from the equality check in `verify`, so a token carrying a nonsense
    /// audience alongside a real one is still usable, and a token carrying only
    /// nonsense is refused exactly as it would be if the element had been
    /// well-formed and wrong. Failing the parse instead would hand a peer that
    /// mis-canonicalises its own resource URI a hard error where a `401` is the
    /// correct answer.
    pub(super) fn into_token_claims(self) -> TokenClaims {
        let mut claims = TokenClaims::new();

        if let Some(subject) = self.sub {
            claims = claims.with_subject(subject);
        }
        if let Some(issuer) = self.iss {
            claims = claims.with_issuer(issuer);
        }
        for audience in self.aud.into_iter().flat_map(Audience::into_entries) {
            claims = claims.with_audience_string(audience);
        }
        for scope in self.scope.into_iter().flat_map(Scopes::into_entries) {
            claims = claims.with_scope(scope);
        }
        // `scp` is unioned with `scope` rather than replacing it: an issuer
        // that sends both has granted both, and reading only one would drop
        // half the grant.
        for scope in self.scp.into_iter().flat_map(Scopes::into_entries) {
            claims = claims.with_scope(scope);
        }
        if let Some(exp) = self.exp.and_then(instant) {
            claims = claims.with_expiry(exp);
        }
        if let Some(nbf) = self.nbf.and_then(instant) {
            claims = claims.with_not_before(nbf);
        }

        claims
    }
}

impl Audience {
    /// The audiences, whichever spelling carried them.
    fn into_entries(self) -> impl Iterator<Item = String> {
        let (one, many) = match self {
            Self::One(one) => (Some(one), Vec::new()),
            Self::Many(many) => (None, many),
        };
        one.into_iter().chain(many)
    }
}

impl Scopes {
    /// The scope strings, whichever spelling carried them.
    fn into_entries(self) -> impl Iterator<Item = Arc<str>> {
        // Collected rather than chained, because the two spellings produce
        // different iterator types and the only thing they share is the element.
        let entries: Vec<Arc<str>> = match self {
            Self::Delimited(delimited) => delimited.split(DELIMITER).map(Arc::from).collect(),
            Self::Listed(listed) => listed.into_iter().map(Arc::from).collect(),
        };
        // An empty `scope` is `""`, and `split` yields one empty piece for it.
        // That is not a scope string anyone issued, and admitting it would
        // hand a deployment's policy an empty name to map.
        entries.into_iter().filter(|scope| !scope.is_empty())
    }
}

/// A `NumericDate` as a `SystemTime`.
///
/// RFC 7519 §3.1 allows a non-integer, so this is an `f64` rather than an
/// `i64`, and the fraction is *kept* rather than truncated. Truncating toward
/// zero sounds safer for `exp`, but it is not what the issuer said: a sender
/// writing `exp: 1700000060.5` granted half a second past the whole second, and
/// rounding that away reads a live token as expired. Keeping the fraction makes
/// the accepted window exactly the window that was issued.
///
/// `pub(super)` so the sibling tests module can exercise the arithmetic
/// directly. `TokenClaims` keeps `expires_at` and `not_before` private, which
/// is right — they are `verify`'s to read — but it does mean there is no
/// accessor for a test to assert through, and widening the public API to give
/// one would be a worse trade than this.
pub(super) fn instant(seconds: f64) -> Option<SystemTime> {
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    UNIX_EPOCH.checked_add(Duration::from_secs_f64(seconds))
}
