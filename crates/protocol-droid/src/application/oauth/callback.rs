//! Handling the authorization callback: the redirect back from the browser.
//!
//! This is the half of the flow where the attack surface arrives. Everything
//! the client knows about its own request — the `state`, the issuer, the PKCE
//! verifier, the resource — was recorded before the user left; everything on
//! this side of the redirect came from a channel the client does not control.
//! The job here is to check the latter against the former and produce a
//! redemption-ready value, or refuse.
//!
//! # The order of the checks is the point
//!
//! `state` is checked **first**, before the response is interpreted as success
//! or failure. RFC 6749 §4.1.2 requires the client validate `state` on the
//! response whatever it contains. Two consequences follow, and both are why the
//! sequence is not incidental:
//!
//! - An attacker who can induce the client to follow a crafted redirect must
//!   not be able to make it report a failure either. A forged
//!   `error=access_denied` is a denial of service against a flow the user
//!   believes is in progress, and reading the error before checking `state`
//!   would let anyone with a link do it.
//! - An attacker must not be able to make the client redeem *someone else's*
//!   code by pairing a valid `code` with a forged `state`. The two are checked
//!   together precisely so neither is meaningful alone.
//!
//! `iss` is checked next, then the response is interpreted. RFC 9207's mix-up
//! defence only means anything if it runs before the code is accepted, and it
//! runs against [`PendingAuthorization::issuer`] — what the client recorded on
//! the way out — rather than against a value read from the response being
//! validated.
//!
//! # What this does not do
//!
//! It does not redeem the code and it does not talk to the authorization
//! server. [`AuthorizedCode`] is what a later use case takes to the token
//! endpoint.

use core::fmt;
use std::sync::Arc;

use super::client::PendingAuthorization;
use crate::domain::mcp::authorization::AuthorizationModel;
use crate::domain::oauth::issuer::{AuthorizationResponseError, check_response_issuer_advertised};
use crate::domain::oauth::pkce::CodeVerifier;
use crate::domain::oauth::registration::ClientId;
use crate::domain::oauth::resource::ResourceBinding;

/// The query parameters an authorization server redirected with.
///
/// Raw strings from a URL this crate did not construct. Parsed once, here, into
/// the two shapes it can mean — a grant or a refusal — before anything acts on
/// them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthorizationCallback {
    code: Option<Arc<str>>,
    state: Option<Arc<str>>,
    iss: Option<Arc<str>>,
    error: Option<Arc<str>>,
}

impl AuthorizationCallback {
    /// A successful response carrying `code` and `state`.
    #[must_use]
    pub fn granted(code: impl AsRef<str>, state: impl AsRef<str>) -> Self {
        Self {
            code: Some(Arc::from(code.as_ref())),
            state: Some(Arc::from(state.as_ref())),
            iss: None,
            error: None,
        }
    }

    /// A refusal carrying `error` and `state`.
    #[must_use]
    pub fn refused(error: impl AsRef<str>, state: impl AsRef<str>) -> Self {
        Self {
            code: None,
            state: Some(Arc::from(state.as_ref())),
            iss: None,
            error: Some(Arc::from(error.as_ref())),
        }
    }

    /// Add the `iss` the response carried, if any.
    #[must_use]
    pub fn with_issuer(mut self, iss: Option<Arc<str>>) -> Self {
        self.iss = iss;
        self
    }

    /// The authorization code, if the response carried one.
    #[must_use]
    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }

    /// The `state` the response carried.
    #[must_use]
    pub fn state(&self) -> Option<&str> {
        self.state.as_deref()
    }

    /// The `iss` the response carried.
    #[must_use]
    pub fn issuer(&self) -> Option<&str> {
        self.iss.as_deref()
    }

    /// The error code the response carried.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

/// A validated callback, ready to redeem.
///
/// Holds the code and everything needed to complete the token request, all of
/// it recorded before the user left the client.
///
/// Deliberately **not** [`Clone`]. RFC 6749 §4.1.2 makes an authorization code
/// single-use, and the way this crate holds that line is that redeeming
/// consumes the value: [`redeem`](super::redeem::redeem) takes it by value, so
/// one value cannot be spent twice. That is the scope of the guarantee, and it
/// is worth being exact about what it does *not* cover: a caller that invokes
/// [`authorize_callback`] twice against the same [`PendingAuthorization`]
/// still holds two values, and may try both. What stops the second is the
/// authorization server, which §4.1.2 requires to deny a reused code. The type
/// closes the accidental double-spend — a retry after a dropped connection,
/// where the first attempt may already have been honoured — and the server
/// closes the deliberate one.
pub struct AuthorizedCode {
    code: Arc<str>,
    verifier: CodeVerifier,
    resource: ResourceBinding,
    client_id: ClientId,
    redirect_uri: Arc<str>,
    issuer: Arc<str>,
}

impl AuthorizedCode {
    /// The authorization code to present at the token endpoint.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// The PKCE verifier to send alongside it.
    #[must_use]
    pub const fn code_verifier(&self) -> &CodeVerifier {
        &self.verifier
    }

    /// What the grant is bound to — the resource the token request must repeat.
    #[must_use]
    pub const fn resource(&self) -> &ResourceBinding {
        &self.resource
    }

    /// The client identifier.
    #[must_use]
    pub const fn client_id(&self) -> &ClientId {
        &self.client_id
    }

    /// The redirect URI the authorization request named, which RFC 6749 §4.1.3
    /// requires a client to repeat if it included one.
    #[must_use]
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// The issuer the code came from.
    #[must_use]
    pub fn issuer(&self) -> &str {
        &self.issuer
    }
}

impl fmt::Debug for AuthorizedCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The code is a single-use credential redeemable exactly once, and the
        // verifier is the secret that redeems it. Both redacted; the rest is
        // what an operator needs to see.
        formatter
            .debug_struct("AuthorizedCode")
            .field("code", &"…")
            .field("code_verifier", &self.verifier)
            .field("resource", &self.resource)
            .field("client_id", &self.client_id)
            .field("redirect_uri", &self.redirect_uri)
            .field("issuer", &self.issuer)
            .finish()
    }
}

/// Why an authorization callback was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CallbackError {
    /// The `state` did not match, or was absent.
    ///
    /// RFC 6749 §4.1.2 requires the client validate `state`. A mismatch means
    /// this response is not the answer to the request this client made, which
    /// covers a forged redirect, a stale tab, and a callback for a different
    /// flow. The two are not distinguished: an attacker learns nothing useful
    /// from which, and a client that reported them separately would leak the
    /// presence of an outstanding flow.
    #[error("the authorization response's state does not match the one this client issued")]
    StateMismatch,

    /// The response named a different authorization server.
    #[error(transparent)]
    Issuer(#[from] AuthorizationResponseError),

    /// The authorization server refused.
    ///
    /// Carries the RFC 6749 §4.1.2.1 `error` code. This is the server saying no
    /// — `access_denied`, `invalid_scope`, a user's "no" at the consent screen —
    /// and is deliberately not folded into a generic failure, because a client
    /// showing the user "access denied" is doing the right thing.
    #[error("the authorization server refused the request: {error}")]
    Refused {
        /// The `error` parameter from the response.
        error: Arc<str>,
    },

    /// Neither a code nor an error.
    ///
    /// A well-formed response carries exactly one. This is a server sending
    /// something this crate cannot interpret, which is not the same as a
    /// refusal and not the same as success.
    #[error("the authorization response carried neither a code nor an error")]
    NoOutcome,
}

/// Validate an authorization callback against the request that started it.
///
/// Checks, in this order and for the reasons in the module documentation:
/// `state`, then `iss`, then whether the response carries a grant or a refusal.
///
/// # Errors
///
/// [`CallbackError::StateMismatch`] if the `state` is absent or different —
/// checked first so that neither a forged success nor a forged refusal can be
/// acted on. [`CallbackError::Issuer`] if a present `iss` names another server.
/// [`CallbackError::Refused`] if the server refused, carrying its error code.
/// [`CallbackError::NoOutcome`] if the response carries neither a code nor an
/// error.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::ProtocolVersion;
/// use protocol_droid::domain::mcp::authorization::authorization_model;
/// use protocol_droid::domain::mcp::resource_metadata::ResourceUri;
/// use protocol_droid::domain::oauth::registration::{ClientId, RegisteredClient};
/// use protocol_droid::domain::oauth::resource::ResourceParameters;
/// use protocol_droid::application::oauth::client::{AuthorizationSecrets, SecretError, State, begin_authorization};
/// use protocol_droid::application::oauth::callback::{AuthorizationCallback, authorize_callback};
/// use protocol_droid::domain::oauth::pkce::{ChallengeMethod, CodeVerifier};
///
/// struct Fixed;
/// impl AuthorizationSecrets for Fixed {
///     fn new_state(&self) -> Result<State, SecretError> { Ok(State::new("state-value")) }
///     fn new_code_verifier(&self) -> Result<CodeVerifier, SecretError> {
///         CodeVerifier::new("a".repeat(43)).map_err(|_| SecretError::Rejected)
///     }
/// }
///
/// let model = authorization_model(ProtocolVersion::V2026_07_28);
/// let client = RegisteredClient::from_metadata_document(
///     "https://auth.example.com",
///     ClientId::metadata_url("https://client.example.com/metadata.json")?,
/// );
/// let resource = ResourceUri::new("https://api.example.com/app")?;
/// let pending = begin_authorization(
///     &model, &client, "https://client.example.com/callback",
///     ResourceParameters::single(&resource), &Fixed,
/// )?;
///
/// let callback = AuthorizationCallback::granted("the-code", "state-value");
/// let authorized = authorize_callback(&model, &pending, &callback)?;
/// assert_eq!(authorized.code(), "the-code");
/// assert!(authorize_callback(&model, &pending, &AuthorizationCallback::granted("c", "wrong")).is_err());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn authorize_callback(
    model: &AuthorizationModel,
    pending: &PendingAuthorization,
    callback: &AuthorizationCallback,
) -> Result<AuthorizedCode, CallbackError> {
    // First, unconditionally, before the response is read as good or bad.
    if !callback
        .state()
        .is_some_and(|state| pending.state().matches(state))
    {
        return Err(CallbackError::StateMismatch);
    }

    // Second, and against what the client recorded on the way out. SEP-2468
    // makes a present `iss` mandatory to check and an absent one tolerable, and
    // that asymmetry lives in the predicate rather than being re-decided here.
    check_response_issuer_advertised(
        model,
        pending.issuer(),
        callback.issuer(),
        pending.iss_advertised(),
    )?;

    // Third: a response is a grant or a refusal, never both and never neither.
    if let Some(code) = callback.code() {
        return Ok(AuthorizedCode {
            code: Arc::from(code),
            verifier: pending.code_verifier().clone(),
            resource: pending.resource().clone(),
            client_id: pending.client_id().clone(),
            redirect_uri: Arc::from(pending.redirect_uri()),
            issuer: Arc::from(pending.issuer()),
        });
    }
    callback
        .error()
        .map_or(Err(CallbackError::NoOutcome), |error| {
            Err(CallbackError::Refused {
                error: Arc::from(error),
            })
        })
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use super::{AuthorizationCallback, AuthorizedCode, CallbackError, authorize_callback};
    use crate::application::oauth::client::{
        AuthorizationSecrets, PendingAuthorization, SecretError, State, begin_authorization,
    };
    use crate::domain::ProtocolVersion;
    use crate::domain::mcp::authorization::{AuthorizationModel, authorization_model};
    use crate::domain::mcp::resource_metadata::{ResourceUri, ResourceUriError};
    use crate::domain::oauth::pkce::{CodeVerifier, PkceError};
    use crate::domain::oauth::registration::{ClientId, ClientIdError, RegisteredClient};
    use crate::domain::oauth::resource::{ResourceIndicatorError, ResourceParameters};

    const AS: &str = "https://auth.example.com";
    const CLIENT_ID: &str = "https://client.example.com/metadata.json";
    const REDIRECT: &str = "https://client.example.com/callback";
    const RESOURCE: &str = "https://api.example.com/app";
    const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    const STATE: &str = "a-state-value";

    /// Every fallible call in this module, in one type, so a test can `?`
    /// through the lot instead of stopping at whichever error comes first.
    #[derive(Debug)]
    enum Failure {
        Uri(ResourceUriError),
        ClientId(ClientIdError),
        Pkce(PkceError),
        Resource(ResourceIndicatorError),
        Begin(super::super::client::BeginError),
        Callback(CallbackError),
    }

    /// Delegates so a failing test prints the engine's own account of what it
    /// refused. It also reads the fields, which is why they are not dead: a
    /// derived `Debug` does not count as a read, so without this every variant
    /// would be reported unused.
    impl core::fmt::Display for Failure {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::Uri(error) => write!(formatter, "{error}"),
                Self::ClientId(error) => write!(formatter, "{error}"),
                Self::Pkce(error) => write!(formatter, "{error}"),
                Self::Resource(error) => write!(formatter, "{error}"),
                Self::Begin(error) => write!(formatter, "{error}"),
                Self::Callback(error) => write!(formatter, "{error}"),
            }
        }
    }

    impl From<ResourceUriError> for Failure {
        fn from(error: ResourceUriError) -> Self {
            Self::Uri(error)
        }
    }
    impl From<ClientIdError> for Failure {
        fn from(error: ClientIdError) -> Self {
            Self::ClientId(error)
        }
    }
    impl From<PkceError> for Failure {
        fn from(error: PkceError) -> Self {
            Self::Pkce(error)
        }
    }
    impl From<ResourceIndicatorError> for Failure {
        fn from(error: ResourceIndicatorError) -> Self {
            Self::Resource(error)
        }
    }
    impl From<super::super::client::BeginError> for Failure {
        fn from(error: super::super::client::BeginError) -> Self {
            Self::Begin(error)
        }
    }
    impl From<CallbackError> for Failure {
        fn from(error: CallbackError) -> Self {
            Self::Callback(error)
        }
    }

    struct Fixed;

    impl AuthorizationSecrets for Fixed {
        fn new_state(&self) -> Result<State, SecretError> {
            Ok(State::new(STATE))
        }
        fn new_code_verifier(&self) -> Result<CodeVerifier, SecretError> {
            CodeVerifier::new(VERIFIER).map_err(|_| SecretError::Rejected)
        }
    }

    const fn model() -> AuthorizationModel {
        authorization_model(ProtocolVersion::V2026_07_28)
    }

    fn pending() -> Result<PendingAuthorization, Failure> {
        let resource = ResourceUri::new(RESOURCE)?;
        let client =
            RegisteredClient::from_metadata_document(AS, ClientId::metadata_url(CLIENT_ID)?);
        begin_authorization(
            &model(),
            &client,
            REDIRECT,
            ResourceParameters::single(&resource),
            &Fixed,
        )
        .map_err(Failure::from)
    }

    #[test]
    fn a_matching_callback_carries_the_request_forward() -> Result<(), Failure> {
        let pending = pending()?;
        let callback = AuthorizationCallback::granted("the-code", STATE);

        let authorized = authorize_callback(&model(), &pending, &callback)?;

        assert_eq!(authorized.code(), "the-code");
        assert_eq!(authorized.code_verifier().as_str(), VERIFIER);
        assert_eq!(authorized.redirect_uri(), REDIRECT);
        assert_eq!(authorized.issuer(), AS);
        assert_eq!(authorized.client_id().as_str(), CLIENT_ID);
        Ok(())
    }

    #[test]
    fn the_grant_stays_bound_to_the_resource_it_was_requested_for() -> Result<(), Failure> {
        let pending = pending()?;
        let callback = AuthorizationCallback::granted("the-code", STATE);

        let authorized = authorize_callback(&model(), &pending, &callback)?;

        // The binding must survive the round trip to the browser; dropping it
        // here would mean the token request arrives without a resource and the
        // server is free to mint one for the wrong audience.
        let other = ResourceUri::new(RESOURCE)?;
        assert!(
            authorized
                .resource()
                .matches_token_request(&ResourceParameters::single(&other))
        );
        assert!(
            !authorized
                .resource()
                .matches_token_request(&ResourceParameters::many([]))
        );
        Ok(())
    }

    #[test]
    fn the_verifier_is_the_one_drawn_before_the_redirect_not_a_fresh_one() -> Result<(), Failure> {
        let pending = pending()?;
        let callback = AuthorizationCallback::granted("the-code", STATE);

        let authorized = authorize_callback(&model(), &pending, &callback)?;

        // Redeeming with a freshly drawn verifier would always fail S256 at the
        // token endpoint, so the value has to be the one the challenge was
        // derived from.
        assert_eq!(authorized.code_verifier(), pending.code_verifier());
        Ok(())
    }

    #[test]
    fn a_state_from_another_flow_is_refused() -> Result<(), Failure> {
        let pending = pending()?;
        let callback = AuthorizationCallback::granted("the-code", "some-other-state");

        assert_matches!(
            authorize_callback(&model(), &pending, &callback),
            Err(CallbackError::StateMismatch)
        );
        Ok(())
    }

    #[test]
    fn a_missing_state_is_refused() -> Result<(), Failure> {
        let pending = pending()?;
        let callback = AuthorizationCallback {
            code: Some("the-code".into()),
            state: None,
            iss: None,
            error: None,
        };

        assert_matches!(
            authorize_callback(&model(), &pending, &callback),
            Err(CallbackError::StateMismatch)
        );
        Ok(())
    }

    #[test]
    fn state_is_checked_before_a_refusal_is_reported() -> Result<(), Failure> {
        let pending = pending()?;
        // A forged error with a forged state. Reading the error first would
        // let anyone with a link abort an authorization the user is in the
        // middle of.
        let callback = AuthorizationCallback::refused("access_denied", "not-our-state");

        assert_matches!(
            authorize_callback(&model(), &pending, &callback),
            Err(CallbackError::StateMismatch)
        );
        Ok(())
    }

    #[test]
    fn state_is_checked_before_a_grant_is_redeemed() -> Result<(), Failure> {
        let pending = pending()?;
        // Someone else's code paired with a guess at the state. The code is
        // meaningless without the verifier, which never left this process.
        let callback = AuthorizationCallback::granted("attacker-code", "guess");

        assert_matches!(
            authorize_callback(&model(), &pending, &callback),
            Err(CallbackError::StateMismatch)
        );
        Ok(())
    }

    #[test]
    fn a_response_from_another_issuer_is_refused() -> Result<(), Failure> {
        let pending = pending()?;
        let callback = AuthorizationCallback::granted("the-code", STATE)
            .with_issuer(Some("https://evil.example.com".into()));

        assert_matches!(
            authorize_callback(&model(), &pending, &callback),
            Err(CallbackError::Issuer(
                crate::domain::oauth::issuer::AuthorizationResponseError::IssuerMismatch { .. }
            ))
        );
        Ok(())
    }

    #[test]
    fn a_response_from_the_right_issuer_is_accepted() -> Result<(), Failure> {
        let pending = pending()?;
        let callback =
            AuthorizationCallback::granted("the-code", STATE).with_issuer(Some(AS.into()));

        let authorized = authorize_callback(&model(), &pending, &callback)?;

        assert_eq!(authorized.code(), "the-code");
        Ok(())
    }

    #[test]
    fn an_absent_issuer_is_tolerated() -> Result<(), Failure> {
        let pending = pending()?;
        // SEP-2468 makes a present `iss` mandatory to check but an absent one
        // not an error, because a server that omits it is not mixing anything up.
        let callback = AuthorizationCallback::granted("the-code", STATE);

        assert_eq!(
            authorize_callback(&model(), &pending, &callback)?.code(),
            "the-code"
        );
        Ok(())
    }

    #[test]
    fn a_refusal_carries_the_servers_own_error_code() -> Result<(), Failure> {
        let pending = pending()?;
        let callback = AuthorizationCallback::refused("access_denied", STATE);

        assert_matches!(
            authorize_callback(&model(), &pending, &callback),
            Err(CallbackError::Refused { ref error }) if &**error == "access_denied"
        );
        Ok(())
    }

    #[test]
    fn a_response_that_is_neither_grant_nor_refusal_is_refused() -> Result<(), Failure> {
        let pending = pending()?;
        let callback = AuthorizationCallback {
            code: None,
            state: Some(STATE.into()),
            iss: None,
            error: None,
        };

        assert_matches!(
            authorize_callback(&model(), &pending, &callback),
            Err(CallbackError::NoOutcome)
        );
        Ok(())
    }

    #[test]
    fn a_code_alongside_an_error_is_still_a_code() -> Result<(), Failure> {
        let pending = pending()?;
        // A server sending both is malformed, but the conservative reading is
        // the code path — an error alongside a grant is not a signal to fail.
        let callback = AuthorizationCallback {
            code: Some("the-code".into()),
            state: Some(STATE.into()),
            iss: None,
            error: Some("access_denied".into()),
        };

        assert_eq!(
            authorize_callback(&model(), &pending, &callback)?.code(),
            "the-code"
        );
        Ok(())
    }

    #[test]
    fn neither_the_code_nor_the_verifier_reaches_the_log() -> Result<(), Failure> {
        let pending = pending()?;
        let callback = AuthorizationCallback::granted("the-code", STATE);

        let rendered = format!("{:?}", authorize_callback(&model(), &pending, &callback)?);

        assert!(
            !rendered.contains("the-code"),
            "the code leaked: {rendered}"
        );
        assert!(
            !rendered.contains(VERIFIER),
            "the verifier leaked: {rendered}"
        );
        assert!(
            rendered.contains(AS),
            "the issuer should be readable: {rendered}"
        );
        assert!(
            rendered.contains(REDIRECT),
            "the redirect should be readable: {rendered}"
        );
        Ok(())
    }

    #[test]
    fn an_authorized_code_reads_back_what_it_came_from() -> Result<(), Failure> {
        let pending = pending()?;
        let callback = AuthorizationCallback::granted("the-code", STATE);
        let authorized = authorize_callback(&model(), &pending, &callback)?;

        // Every field the token request needs, reachable without reaching back
        // into the pending value or re-deriving anything.
        let _: &AuthorizedCode = &authorized;
        assert_eq!(authorized.code(), "the-code");
        assert!(pending.state().matches(STATE));
        Ok(())
    }
}
