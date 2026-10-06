//! What a token endpoint hands back, and what it refuses to.
//!
//! RFC 6749 §5.1 defines the success shape and §5.2 the failure shape. Both
//! arrive as JSON, and both are worth modelling as types rather than as a
//! `serde_json::Value` a caller interrogates — the difference between a token
//! this crate can present and one it cannot is exactly the difference between
//! a parsed `IssuedToken` and a map that happened to have the right keys.
//!
//! # What is strict, and why
//!
//! `access_token` and `token_type` are both REQUIRED by §5.1, so a body
//! missing either is refused rather than defaulted. `token_type` is checked
//! against `Bearer` case-insensitively — §5.1 makes the value case
//! insensitive, and a server sending `bearer` is conforming, not broken.
//! A token of any *other* type is refused: this crate has no way to present a
//! DPoP-bound or Mac-authenticated token, and accepting one would mean
//! constructing an `IssuedToken` that looks usable and is not.
//!
//! # What is not strict
//!
//! Unrecognised members are preserved in [`IssuedToken::extra`] rather than
//! dropped, for the same reason the IR keeps unknown fields: a member this
//! build does not model may be one the peer relies on, and discarding it
//! silently is how a client quietly stops interoperating after a server
//! upgrade.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;

/// An access token, as a bearer credential.
///
/// `Debug` is redacted. `Display` is full, because `Display` here is the
/// presentation API — a caller putting the token in an `Authorization` header
/// is using it the way it was issued — and `as_str` is the escape hatch for
/// everything else.
#[derive(Clone)]
pub struct AccessToken {
    value: Arc<str>,
    expires_in: Option<Duration>,
}

impl AccessToken {
    /// A token, optionally with the lifetime the server reported.
    #[must_use]
    pub fn new(value: impl AsRef<str>, expires_in: Option<Duration>) -> Self {
        Self {
            value: Arc::from(value.as_ref()),
            expires_in,
        }
    }

    /// The token itself.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }

    /// How long the server said this token lives.
    ///
    /// A client is free to cache for exactly this long and no longer. An
    /// absent `expires_in` is not "never expires" — RFC 6749 §5.1 makes it
    /// RECOMMENDED rather than REQUIRED, so its absence says the server did
    /// not say, and the honest reading is that the client should not cache.
    #[must_use]
    pub const fn expires_in(&self) -> Option<Duration> {
        self.expires_in
    }
}

impl fmt::Debug for AccessToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AccessToken(…)")
    }
}

impl fmt::Display for AccessToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.value)
    }
}

/// A refresh token, as a bearer credential.
///
/// Same redaction contract as [`AccessToken`], and for the same reason: it is
/// presented as a credential and must not ride out on a `dbg!`.
#[derive(Clone)]
pub struct RefreshToken(Arc<str>);

impl RefreshToken {
    /// The token the server issued.
    #[must_use]
    pub fn issued(value: impl AsRef<str>) -> Self {
        Self(Arc::from(value.as_ref()))
    }

    /// The token itself.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for RefreshToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RefreshToken(…)")
    }
}

impl fmt::Display for RefreshToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A successfully redeemed grant.
///
/// Obtained only from [`IssuedToken::parse`], so an `IssuedToken` that exists
/// is one that parsed. `token_type` is not carried because the only value that
/// can produce this type is `Bearer` — the check happened at the door.
#[derive(Debug)]
pub struct IssuedToken {
    access_token: AccessToken,
    refresh_token: Option<RefreshToken>,
    scope: Option<Arc<str>>,
    extra: serde_json::Map<String, serde_json::Value>,
}

impl IssuedToken {
    /// The token to present as a bearer credential.
    #[must_use]
    pub const fn access_token(&self) -> &AccessToken {
        &self.access_token
    }

    /// The refresh token, if the server issued one.
    #[must_use]
    pub const fn refresh_token(&self) -> Option<&RefreshToken> {
        self.refresh_token.as_ref()
    }

    /// The scope actually granted.
    ///
    /// RFC 6749 §5.1 makes this OPTIONAL, and an absent value does **not**
    /// mean "everything that was asked for" — §5.1 says the server treated it
    /// as equal to the requested scope, and §3.3 lets it narrow that
    /// silently. A client that reads `None` as "unlimited" would present a
    /// token for a grant it never received.
    #[must_use]
    pub fn scope(&self) -> Option<&str> {
        self.scope.as_deref()
    }

    /// Members of the response this build does not model.
    #[must_use]
    pub const fn extra(&self) -> &serde_json::Map<String, serde_json::Value> {
        &self.extra
    }

    /// Parse an RFC 6749 §5.1 success body, or an RFC 6749 §5.2 error body.
    ///
    /// # Errors
    ///
    /// [`TokenResponseError::AuthorizationServer`] when the body names an
    /// `error`, carrying the server's own `error` code rather than a guess.
    /// [`TokenResponseError::Malformed`] when the body is not JSON, or omits
    /// the two REQUIRED members. [`TokenResponseError::UnsupportedTokenType`]
    /// when `token_type` is something other than `Bearer` under a
    /// case-insensitive comparison.
    ///
    /// # Examples
    ///
    /// ```
    /// # use protocol_droid::domain::oauth::issued_token::{IssuedToken, TokenResponseError};
    /// let body = br#"{"access_token":"at-1","token_type":"Bearer","expires_in":3600}"#;
    /// let issued = IssuedToken::parse(body)?;
    ///
    /// assert_eq!(issued.access_token().as_str(), "at-1");
    /// assert!(issued.refresh_token().is_none());
    ///
    /// let refused = br#"{"error":"invalid_grant","error_description":"expired"}"#;
    /// assert!(matches!(
    ///     IssuedToken::parse(refused),
    ///     Err(TokenResponseError::AuthorizationServer { error, .. })
    ///         if error == "invalid_grant"
    /// ));
    /// # Ok::<(), TokenResponseError>(())
    /// ```
    pub fn parse(body: &[u8]) -> Result<Self, TokenResponseError> {
        let wire: Wire =
            serde_json::from_slice(body).map_err(|failure| TokenResponseError::Malformed {
                reason: failure.to_string(),
            })?;

        // §5.2 is checked first and on the member's presence rather than on the
        // HTTP status: a server that answers 400 with a §5.1 body is broken,
        // and one that answers 200 with a §5.2 body must not be read as a
        // success with missing fields.
        if let Some(error) = wire.error {
            return Err(TokenResponseError::AuthorizationServer {
                error,
                description: wire.error_description,
            });
        }

        let access_token = wire
            .access_token
            .ok_or_else(|| TokenResponseError::Malformed {
                reason: "missing access_token".to_owned(),
            })?;

        let token_type = wire
            .token_type
            .ok_or_else(|| TokenResponseError::Malformed {
                reason: "missing token_type".to_owned(),
            })?;

        // §5.1: "Values used in the access token type are case insensitive."
        if !token_type.eq_ignore_ascii_case("Bearer") {
            return Err(TokenResponseError::UnsupportedTokenType { token_type });
        }

        let mut extra = wire.extra;
        // Everything the named members carried is removed, so `extra` holds
        // only what this build genuinely does not model. Leaving `scope` in
        // both places would give a caller two answers to "what was granted".
        for modelled in [
            "access_token",
            "token_type",
            "refresh_token",
            "scope",
            "expires_in",
        ] {
            extra.remove(modelled);
        }
        extra.remove("error");
        extra.remove("error_description");

        Ok(Self {
            access_token: AccessToken::new(access_token, wire.expires_in.map(Duration::from_secs)),
            refresh_token: wire.refresh_token.map(RefreshToken::issued),
            scope: wire.scope.map(Arc::from),
            extra,
        })
    }
}

/// The wire shape, read leniently and validated strictly.
///
/// Every member is optional here because the validation belongs in
/// [`IssuedToken::parse`], where each absence can carry its own reason. A
/// `serde` `deny_unknown_fields` would refuse a server that sends a member we
/// do not model, which is the opposite of what this crate wants.
#[derive(Deserialize)]
struct Wire {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

/// Why a token response was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TokenResponseError {
    /// The body was not JSON, or was missing a REQUIRED member.
    #[error("the token response could not be read: {reason}")]
    Malformed {
        /// What was wrong. Never contains the token.
        reason: String,
    },
    /// §5.2: the server named its own refusal.
    ///
    /// The `error` code is the part that goes in the message; `description` is
    /// kept as a field because a server's own words are worth having in a log
    /// and are not worth interpolating into an error string that might be
    /// pasted somewhere.
    #[error("the authorization server refused: {error}")]
    AuthorizationServer {
        /// The server's `error` code, verbatim.
        error: String,
        /// The server's `error_description`, if it sent one.
        description: Option<String>,
    },
    /// A `token_type` this crate cannot present.
    #[error("this crate cannot present a token of type {token_type}")]
    UnsupportedTokenType {
        /// The type the server named, verbatim.
        token_type: String,
    },
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use assert_matches::assert_matches;

    use super::{AccessToken, IssuedToken, RefreshToken, TokenResponseError};

    #[test]
    fn a_minimal_success_body_parses() -> Result<(), TokenResponseError> {
        let issued = IssuedToken::parse(br#"{"access_token":"at-1","token_type":"Bearer"}"#)?;

        assert_eq!(issued.access_token().as_str(), "at-1");
        assert_eq!(issued.access_token().expires_in(), None);
        assert!(issued.extra().is_empty());
        Ok(())
    }

    /// §5.1 makes the value case insensitive, so a conforming server sending
    /// `bearer` must not be refused for its capitalisation.
    #[test]
    fn the_token_type_is_compared_case_insensitively() {
        for spelling in ["Bearer", "bearer", "BEARER", "bEaReR"] {
            let body = format!(r#"{{"access_token":"at","token_type":"{spelling}"}}"#);
            assert!(
                IssuedToken::parse(body.as_bytes()).is_ok(),
                "{spelling} should be accepted"
            );
        }
    }

    /// The strictness that matters: a token this crate cannot present must
    /// never become an `IssuedToken`, because every `IssuedToken` downstream
    /// assumes it is presentable as a bearer credential.
    #[test]
    fn a_token_of_another_type_is_refused() {
        assert_matches!(
            IssuedToken::parse(br#"{"access_token":"at","token_type":"DPoP"}"#),
            Err(TokenResponseError::UnsupportedTokenType { token_type }) if token_type == "DPoP"
        );
    }

    #[test]
    fn both_required_members_are_required() {
        assert_matches!(
            IssuedToken::parse(br#"{"token_type":"Bearer"}"#),
            Err(TokenResponseError::Malformed { .. })
        );
        assert_matches!(
            IssuedToken::parse(br#"{"access_token":"at"}"#),
            Err(TokenResponseError::Malformed { .. })
        );
    }

    /// §5.2 checked on the member's presence, so a server answering 200 with an
    /// error body cannot be read as a success with missing fields.
    #[test]
    fn an_error_body_is_refused_even_though_it_looks_complete() {
        assert_matches!(
            IssuedToken::parse(
                br#"{"access_token":"at","token_type":"Bearer","error":"invalid_grant"}"#
            ),
            Err(TokenResponseError::AuthorizationServer { error, .. })
                if error == "invalid_grant"
        );
    }

    #[test]
    fn the_servers_own_error_code_is_carried_verbatim() {
        let body = br#"{"error":"invalid_client","error_description":"unknown client"}"#;

        assert_matches!(
            IssuedToken::parse(body),
            Err(TokenResponseError::AuthorizationServer { error, description: Some(description) })
                if error == "invalid_client" && description == "unknown client"
        );
    }

    /// The pass-through that keeps a client interoperating after a server
    /// upgrade. Dropping `id_token` silently is how that stops happening
    /// without an error.
    #[test]
    fn an_unmodelled_member_survives() -> Result<(), TokenResponseError> {
        let issued = IssuedToken::parse(
            br#"{"access_token":"at","token_type":"Bearer","id_token":"jwt","ext":7}"#,
        )?;

        assert_eq!(
            issued.extra().get("id_token"),
            Some(&serde_json::json!("jwt"))
        );
        assert_eq!(issued.extra().get("ext"), Some(&serde_json::json!(7)));
        Ok(())
    }

    /// The two answers to "what was granted" must not disagree. A `scope` left
    /// in `extra` while also modelled gives a caller two sources of truth.
    #[test]
    fn a_modelled_member_is_not_also_left_in_the_passthrough() -> Result<(), TokenResponseError> {
        let issued = IssuedToken::parse(
            br#"{"access_token":"at","token_type":"Bearer","scope":"read","expires_in":60}"#,
        )?;

        assert_eq!(issued.scope(), Some("read"));
        assert_eq!(
            issued.access_token().expires_in(),
            Some(Duration::from_secs(60))
        );
        assert!(
            issued.extra().is_empty(),
            "modelled members leaked into extra"
        );
        Ok(())
    }

    /// §5.1 makes `scope` OPTIONAL, and §3.3 lets a server narrow it without
    /// saying. Reading `None` as "everything requested" would hand the client
    /// a grant it never received.
    #[test]
    fn an_absent_scope_is_not_the_requested_scope() -> Result<(), TokenResponseError> {
        let issued = IssuedToken::parse(br#"{"access_token":"at","token_type":"Bearer"}"#)?;

        assert_eq!(issued.scope(), None);
        Ok(())
    }

    #[test]
    fn a_refresh_token_is_carried_when_issued() -> Result<(), TokenResponseError> {
        let issued = IssuedToken::parse(
            br#"{"access_token":"at","token_type":"Bearer","refresh_token":"rt"}"#,
        )?;

        assert_eq!(issued.refresh_token().map(RefreshToken::as_str), Some("rt"));
        Ok(())
    }

    #[test]
    fn a_body_that_is_not_json_is_malformed() {
        assert_matches!(
            IssuedToken::parse(b"<html>502</html>"),
            Err(TokenResponseError::Malformed { .. })
        );
    }

    // --- redaction ---

    /// The whole reason `Debug` is hand-written. A panic, an `assert_eq!` and
    /// a `dbg!` all expand to it, so a derived `Debug` here is the path a
    /// credential escapes through by accident.
    #[test]
    fn no_bearer_credential_rides_out_on_debug() -> Result<(), TokenResponseError> {
        let issued = IssuedToken::parse(
            br#"{"access_token":"at-secret","token_type":"Bearer","refresh_token":"rt-secret"}"#,
        )?;

        let rendered = format!("{issued:?}");
        assert!(!rendered.contains("at-secret"), "{rendered}");
        assert!(!rendered.contains("rt-secret"), "{rendered}");

        let access = format!("{:?}", issued.access_token());
        assert_eq!(access, "AccessToken(…)");
        Ok(())
    }

    /// `Display` is the presentation API and stays full; a client assembling an
    /// `Authorization` header needs the value.
    #[test]
    fn display_carries_the_value() -> Result<(), TokenResponseError> {
        let issued = IssuedToken::parse(br#"{"access_token":"at-1","token_type":"Bearer"}"#)?;

        assert_eq!(issued.access_token().to_string(), "at-1");
        Ok(())
    }

    #[test]
    fn an_access_token_can_be_built_directly() {
        let token = AccessToken::new("at", Some(Duration::from_secs(30)));

        assert_eq!(token.as_str(), "at");
        assert_eq!(format!("{token:?}"), "AccessToken(…)");
    }
}
