//! Beginning an OAuth authorization flow: what the client sends the user to,
//! and what it must keep until the user comes back.
//!
//! # The half that happens in a browser
//!
//! `GET` the authorization endpoint, the user authenticates there, and the
//! authorization server redirects to the client's registered redirect URI with
//! a `code` and a `state`. Between those two points the client holds two
//! secrets it generated and must not lose: the `state` that proves this
//! callback belongs to this request, and the PKCE `code_verifier` that the
//! `code_challenge` was derived from.
//!
//! Both are redacted under [`Debug`](core::fmt::Debug) on
//! [`PendingAuthorization`], because that value is the one a client will hold
//! in memory for the whole user-visible round trip, and it is exactly the value
//! a panic message, an `assert_eq!` or a future log line would otherwise render.
//!
//! # What this deliberately does not do
//!
//! It does not redeem the code, and it does not talk to the authorization
//! server. [`PendingAuthorization`] is the value that survives the redirect; a
//! separate use case consumes it with the callback and owns the token request.
//! Splitting them is what lets the same pending value be tested against a
//! recorded callback without a socket.

use core::fmt;
use std::sync::Arc;

use url::Url;

use crate::domain::mcp::authorization::AuthorizationModel;
use crate::domain::oauth::pkce::{ChallengeMethod, CodeChallenge, CodeVerifier};
use crate::domain::oauth::registration::{ClientId, RegisteredClient};
use crate::domain::oauth::resource::{ResourceBinding, ResourceIndicatorError, ResourceParameters};

/// A source of the unguessable values an authorization request is built from.
///
/// Narrow, and owned by this use case because this use case is what consumes
/// it. An adapter implements it; the crate ships none by default, because the
/// right source depends on where the client runs and a wrong default is a
/// predictable token rather than a compile error.
///
/// Both methods are fallible: the usual implementation draws from the OS
/// entropy source, which is a runtime facility that can fail, and a client that
/// quietly falls back to something guessable is the failure mode this avoids.
pub trait AuthorizationSecrets {
    /// A fresh `state` for one authorization request.
    ///
    /// # Errors
    ///
    /// Whatever the underlying source raises when it cannot produce one.
    fn new_state(&self) -> Result<State, SecretError>;

    /// A fresh PKCE `code_verifier` for one authorization request.
    ///
    /// # Errors
    ///
    /// Whatever the underlying source raises when it cannot produce one.
    fn new_code_verifier(&self) -> Result<CodeVerifier, SecretError>;
}

/// A source of unguessable values could not produce one.
///
/// The variants exist to separate the two failures a caller has to tell apart.
/// A source that cannot reach the operating system's entropy is an environment
/// problem, fixed by restarting or by moving somewhere the source exists;
/// [`Rejected`](Self::Rejected) means this crate's own encoder produced a
/// value its own value object refused, which is an internal invariant and a bug
/// here. One unit error for both would make the second indistinguishable from
/// the first, and the second is the one worth a bug report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SecretError {
    /// The operating system's entropy source could not be read.
    #[error("the operating system's entropy source could not be read")]
    EntropyUnavailable,

    /// This platform provides no usable entropy source.
    #[error("this platform provides no usable entropy source")]
    EntropyUnsupported,

    /// The value drawn was refused by the value object that owns it.
    ///
    /// An adapter drawing 32 bytes and encoding them base64url produces 43
    /// characters drawn from a subset of the unreserved set, so it cannot fail
    /// the length or charset rule it is checked against. Reaching this variant
    /// means the encoding and that rule have diverged.
    #[error("the value drawn was refused by the value object that owns it")]
    Rejected,
}

/// An OAuth `state` value: the nonce tying one authorization response to one
/// request.
///
/// # Redaction
///
/// Renders as `State(…)`. A `state` is a CSRF token — an attacker who learns
/// one can replay the callback it belongs to — so it does not belong in a log
/// line, and [`Debug`](fmt::Debug) is the boundary that keeps it out.
#[derive(Clone, PartialEq, Eq)]
pub struct State(Arc<str>);

impl State {
    /// Wrap a value the client generated elsewhere.
    #[must_use]
    pub fn new(value: impl AsRef<str>) -> Self {
        Self(Arc::from(value.as_ref()))
    }

    /// The value as it travels in the query string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this is the `state` the request expected.
    ///
    /// Plain equality, and deliberately not a constant-time comparison: `state`
    /// is compared against a value the caller already holds, it is not a
    /// bearer secret being presented to a verifier that would otherwise leak
    /// its contents through timing. The PKCE verifier is the one compared
    /// against an attacker-supplied value, and that comparison happens at the
    /// authorization server, not here.
    #[must_use]
    pub fn matches(&self, presented: &str) -> bool {
        self.0.as_ref() == presented
    }
}

impl fmt::Debug for State {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("State(…)")
    }
}

impl fmt::Display for State {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Everything a client must keep between sending the user away and handling the
/// callback.
///
/// # Redaction
///
/// [`Debug`] renders the secrets as `…` and the rest normally. The verifier and
/// the `state` are both load-bearing — one redeems the code, the other proves
/// the callback belongs to this request — and this is the value a client holds
/// in memory across a user-visible redirect.
#[derive(Clone)]
pub struct PendingAuthorization {
    state: State,
    verifier: CodeVerifier,
    resource: ResourceBinding,
    client_id: ClientId,
    redirect_uri: Arc<str>,
    issuer: Arc<str>,
    /// The `scope` to request, already chosen by the caller.
    scope: Option<Arc<str>>,
    /// Whether the server's metadata advertised
    /// `authorization_response_iss_parameter_supported`.
    iss_advertised: bool,
}

impl PendingAuthorization {
    /// The `state` this request issued, for comparing against the callback.
    #[must_use]
    pub const fn state(&self) -> &State {
        &self.state
    }

    /// The PKCE verifier to present when redeeming the code.
    #[must_use]
    pub const fn code_verifier(&self) -> &CodeVerifier {
        &self.verifier
    }

    /// What the grant is bound to, and what the token request must repeat.
    #[must_use]
    pub const fn resource(&self) -> &ResourceBinding {
        &self.resource
    }

    /// The redirect URI the authorization request named.
    #[must_use]
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// The client this request was made as.
    ///
    /// The token request has to name the same client, so a caller resuming the
    /// flow after the redirect needs it back from here rather than keeping a
    /// second copy alongside the pending value.
    #[must_use]
    pub const fn client_id(&self) -> &ClientId {
        &self.client_id
    }

    /// The issuer this flow is with.
    ///
    /// Carried so the redemption step can check an `iss` response against
    /// something the client recorded when it *started*, rather than against a
    /// value read from the response it is trying to validate.
    #[must_use]
    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    /// This request, asking for `scope` (space-separated).
    ///
    /// Omitted when not set: the authorization page has the client omit
    /// `scope` entirely when neither the challenge nor `scopes_supported`
    /// names any.
    #[must_use]
    pub fn with_scope(mut self, scope: impl AsRef<str>) -> Self {
        self.scope = Some(Arc::from(scope.as_ref()));
        self
    }

    /// The scope this request asks for, if any.
    #[must_use]
    pub fn scope(&self) -> Option<&str> {
        self.scope.as_deref()
    }

    /// This request, recording that the server advertised `iss` support, so a
    /// response without one is refused.
    #[must_use]
    pub const fn with_iss_advertised(mut self, advertised: bool) -> Self {
        self.iss_advertised = advertised;
        self
    }

    /// Whether the server advertised `iss` support.
    #[must_use]
    pub const fn iss_advertised(&self) -> bool {
        self.iss_advertised
    }

    /// Render the URL to send the user to.
    ///
    /// Appends to whatever query the endpoint already carries, because the
    /// endpoint came from a discovery document this crate does not control.
    ///
    /// # Errors
    ///
    /// [`BeginError::Endpoint`] when the authorization endpoint is not a URL
    /// this crate can append to.
    pub fn authorization_url(
        &self,
        endpoint: &str,
        method: ChallengeMethod,
    ) -> Result<Url, BeginError> {
        let mut url = Url::parse(endpoint).map_err(|source| BeginError::Endpoint {
            endpoint: endpoint.to_owned(),
            source,
        })?;
        let challenge: CodeChallenge = self.verifier.challenge(method);
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("response_type", "code");
            query.append_pair("client_id", self.client_id.as_str());
            query.append_pair("redirect_uri", &self.redirect_uri);
            query.append_pair("state", self.state.as_str());
            query.append_pair("code_challenge", challenge.as_str());
            query.append_pair("code_challenge_method", method.as_str());
            for resource in self.resource.token_request().iter() {
                query.append_pair("resource", resource);
            }
            if let Some(scope) = &self.scope {
                query.append_pair("scope", scope);
            }
        }
        Ok(url)
    }
}

impl fmt::Debug for PendingAuthorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PendingAuthorization")
            .field("state", &self.state)
            .field("code_verifier", &self.verifier)
            .field("resource", &self.resource)
            .field("client_id", &self.client_id)
            .field("redirect_uri", &self.redirect_uri)
            .field("issuer", &self.issuer)
            .field("scope", &self.scope)
            .field("iss_advertised", &self.iss_advertised)
            .finish()
    }
}

/// Why an authorization request could not be started.
#[derive(Debug, thiserror::Error)]
pub enum BeginError {
    /// The secret source could not produce a `state` or a verifier.
    #[error("the authorization secret source failed")]
    Secrets(#[source] SecretError),

    /// The revision requires a resource indicator and none was named.
    #[error(transparent)]
    Resource(#[from] ResourceIndicatorError),

    /// The authorization endpoint is not a URL this crate can append to.
    #[error("the authorization endpoint `{endpoint}` is not a usable URL: {source}")]
    Endpoint {
        /// The endpoint as configured or discovered.
        endpoint: String,
        /// Why it was refused.
        #[source]
        source: url::ParseError,
    },
}

/// Build the authorization request and the state a client must keep for it.
///
/// # Errors
///
/// [`BeginError::Secrets`] if the source cannot produce both unguessable
/// values, [`BeginError::Resource`] when the revision requires a resource
/// indicator and the caller named none, and [`BeginError::Endpoint`] when the
/// endpoint will not parse.
///
/// The result is deliberately a pair — the URL and the pending value — because
/// the two are easy to take independently and must not be: a client that
/// renders the URL and then persists a *different* pending value has a `state`
/// mismatch on return and no way to tell whether that is an attack or a bug.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::ProtocolVersion;
/// use protocol_droid::domain::mcp::authorization::authorization_model;
/// use protocol_droid::domain::mcp::resource_metadata::ResourceUri;
/// use protocol_droid::domain::oauth::registration::{ClientId, RegisteredClient};
/// use protocol_droid::domain::oauth::resource::ResourceParameters;
/// use protocol_droid::application::oauth::client::{AuthorizationSecrets, SecretError};
/// use protocol_droid::application::oauth::client::begin_authorization;
/// use protocol_droid::domain::oauth::pkce::{ChallengeMethod, CodeVerifier};
///
/// struct Fixed;
/// impl AuthorizationSecrets for Fixed {
///     fn new_state(&self) -> Result<protocol_droid::application::oauth::client::State, SecretError> {
///         Ok(protocol_droid::application::oauth::client::State::new("state-value"))
///     }
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
///     &model,
///     &client,
///     "https://client.example.com/callback",
///     ResourceParameters::single(&resource),
///     &Fixed,
/// )?;
///
/// let url = pending.authorization_url("https://auth.example.com/authorize", ChallengeMethod::S256)?;
/// assert!(url.as_str().contains("code_challenge_method=S256"));
/// assert!(url.as_str().contains("resource=https%3A%2F%2Fapi.example.com%2Fapp"));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn begin_authorization(
    model: &AuthorizationModel,
    client: &RegisteredClient,
    redirect_uri: &str,
    resource: ResourceParameters,
    secrets: &impl AuthorizationSecrets,
) -> Result<PendingAuthorization, BeginError> {
    let state = secrets.new_state().map_err(BeginError::Secrets)?;
    let verifier = secrets.new_code_verifier().map_err(BeginError::Secrets)?;
    // Built after the secrets rather than before: an empty resource set is a
    // caller mistake the model can reject, and there is no reason to have drawn
    // randomness for a request that will not be sent.
    let resource = ResourceBinding::bind(model, resource)?;
    Ok(PendingAuthorization {
        state,
        verifier,
        resource,
        client_id: client.client_id().clone(),
        redirect_uri: Arc::from(redirect_uri),
        issuer: Arc::from(client.issuer()),
        scope: None,
        iss_advertised: false,
    })
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use super::{
        AuthorizationSecrets, PendingAuthorization, SecretError, State, begin_authorization,
    };
    use crate::domain::ProtocolVersion;
    use crate::domain::mcp::authorization::{AuthorizationModel, authorization_model};
    use crate::domain::mcp::resource_metadata::{ResourceUri, ResourceUriError};
    use crate::domain::oauth::pkce::{ChallengeMethod, CodeVerifier, PkceError};
    use crate::domain::oauth::registration::{ClientId, ClientIdError, RegisteredClient};
    use crate::domain::oauth::resource::{ResourceIndicatorError, ResourceParameters};

    const AS: &str = "https://auth.example.com";
    const CLIENT_ID: &str = "https://client.example.com/metadata.json";
    const REDIRECT: &str = "https://client.example.com/callback";
    const ENDPOINT: &str = "https://auth.example.com/authorize";
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
        Begin(super::BeginError),
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
    impl From<super::BeginError> for Failure {
        fn from(error: super::BeginError) -> Self {
            Self::Begin(error)
        }
    }

    /// Delegates so a failing test prints the engine's own account of what it
    /// refused. It also reads the fields, which is why they are not dead: a
    /// derived `Debug` does not count as a read, so without this every variant
    /// trips `dead_code`.
    impl core::fmt::Display for Failure {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::Uri(error) => write!(formatter, "{error}"),
                Self::ClientId(error) => write!(formatter, "{error}"),
                Self::Pkce(error) => write!(formatter, "{error}"),
                Self::Resource(error) => write!(formatter, "{error}"),
                Self::Begin(error) => write!(formatter, "{error}"),
            }
        }
    }

    /// A source with fixed output, so the assertions are about the request this
    /// module builds and not about whatever a real entropy source produced.
    struct Fixed;

    impl AuthorizationSecrets for Fixed {
        fn new_state(&self) -> Result<State, SecretError> {
            Ok(State::new(STATE))
        }
        fn new_code_verifier(&self) -> Result<CodeVerifier, SecretError> {
            CodeVerifier::new(VERIFIER).map_err(|_| SecretError::Rejected)
        }
    }

    /// A source that cannot produce anything, which is the branch that matters:
    /// a client that carried on regardless would be sending a guessable state.
    struct Unavailable;

    impl AuthorizationSecrets for Unavailable {
        fn new_state(&self) -> Result<State, SecretError> {
            Err(SecretError::EntropyUnavailable)
        }
        fn new_code_verifier(&self) -> Result<CodeVerifier, SecretError> {
            Err(SecretError::EntropyUnavailable)
        }
    }

    fn model_for(version: ProtocolVersion) -> AuthorizationModel {
        authorization_model(version)
    }

    fn client() -> Result<RegisteredClient, ClientIdError> {
        Ok(RegisteredClient::from_metadata_document(
            AS,
            ClientId::metadata_url(CLIENT_ID)?,
        ))
    }

    fn resource() -> Result<ResourceParameters, ResourceUriError> {
        Ok(ResourceParameters::single(&ResourceUri::new(
            "https://api.example.com/app",
        )?))
    }

    fn pending(version: ProtocolVersion) -> Result<PendingAuthorization, Failure> {
        Ok(begin_authorization(
            &model_for(version),
            &client()?,
            REDIRECT,
            resource()?,
            &Fixed,
        )?)
    }

    /// RFC 7636 Appendix A: the S256 derivation is checked against the wire
    /// form here too, so a change to the query rendering cannot quietly pass a
    /// challenge the server will reject.
    #[test]
    fn the_request_carries_the_challenge_not_the_verifier() -> Result<(), Failure> {
        let pending = pending(ProtocolVersion::V2026_07_28)?;
        let url = pending.authorization_url(ENDPOINT, ChallengeMethod::S256)?;
        let query = url.query().unwrap_or_default();

        assert!(
            query.contains("code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"),
            "{query}"
        );
        assert!(query.contains("code_challenge_method=S256"), "{query}");
        assert!(
            !query.contains(VERIFIER),
            "the verifier must not appear in the redirect: {query}"
        );
        Ok(())
    }

    #[test]
    fn the_request_carries_the_four_parameters_a_redirect_needs() -> Result<(), Failure> {
        let pending = pending(ProtocolVersion::V2026_07_28)?;
        let url = pending.authorization_url(ENDPOINT, ChallengeMethod::S256)?;
        let query = url.query().unwrap_or_default();

        assert!(query.contains("response_type=code"), "{query}");
        assert!(
            query.contains(&format!(
                "client_id={}",
                CLIENT_ID.replace(':', "%3A").replace('/', "%2F")
            )),
            "{query}"
        );
        assert!(
            query.contains(&format!(
                "redirect_uri={}",
                REDIRECT.replace(':', "%3A").replace('/', "%2F")
            )),
            "{query}"
        );
        assert!(query.contains(&format!("state={STATE}")), "{query}");
        Ok(())
    }

    /// The endpoint comes from a discovery document this crate does not
    /// control, so it may already carry a query. Appending must not discard it.
    #[test]
    fn an_endpoint_that_already_has_a_query_keeps_it() -> Result<(), Failure> {
        let pending = pending(ProtocolVersion::V2026_07_28)?;
        let url = pending.authorization_url(
            "https://auth.example.com/authorize?tenant=acme",
            ChallengeMethod::S256,
        )?;
        let query = url.query().unwrap_or_default();
        assert!(
            query.contains("tenant=acme"),
            "an existing query was dropped: {query}"
        );
        assert!(query.contains("response_type=code"), "{query}");
        Ok(())
    }

    /// A resource indicator is required from `2025-06-18`, and the whole point
    /// is that it reaches the wire.
    #[test]
    fn the_resource_indicator_reaches_the_wire() -> Result<(), Failure> {
        let pending = pending(ProtocolVersion::V2026_07_28)?;
        let url = pending.authorization_url(ENDPOINT, ChallengeMethod::S256)?;
        let query = url.query().unwrap_or_default();
        assert!(
            query.contains("resource=https%3A%2F%2Fapi.example.com%2Fapp"),
            "{query}"
        );
        Ok(())
    }

    /// The whole request is refused rather than sent without one.
    #[test]
    fn a_revision_requiring_indicators_refuses_a_request_naming_none() -> Result<(), Failure> {
        let outcome = begin_authorization(
            &model_for(ProtocolVersion::V2026_07_28),
            &client()?,
            REDIRECT,
            ResourceParameters::default(),
            &Fixed,
        );
        // `assert_matches!` rather than `assert!(matches!(..))`, because the latter
        // prints nothing about what it actually saw.
        assert_matches!(
            outcome.err(),
            Some(super::BeginError::Resource(ResourceIndicatorError::Missing))
        );
        Ok(())
    }

    /// A source that cannot produce unguessable values must stop the flow. A
    /// client that carried on with something guessable would defeat both PKCE
    /// and the CSRF binding in one step.
    #[test]
    fn a_failing_secret_source_stops_the_request() -> Result<(), Failure> {
        let outcome = begin_authorization(
            &model_for(ProtocolVersion::V2026_07_28),
            &client()?,
            REDIRECT,
            resource()?,
            &Unavailable,
        );
        assert_matches!(
            outcome.err(),
            Some(super::BeginError::Secrets(SecretError::EntropyUnavailable))
        );
        Ok(())
    }

    /// The pending value is the one a client holds across a user-visible round
    /// trip, and it carries both secrets.
    #[test]
    fn debug_redacts_both_secrets_and_keeps_the_rest() -> Result<(), Failure> {
        let pending = pending(ProtocolVersion::V2026_07_28)?;
        let rendered = format!("{pending:?}");

        assert!(
            !rendered.contains(VERIFIER),
            "the verifier leaked: {rendered}"
        );
        assert!(!rendered.contains(STATE), "the state leaked: {rendered}");
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
    fn state_matches_only_the_value_this_request_issued() -> Result<(), Failure> {
        let pending = pending(ProtocolVersion::V2026_07_28)?;
        assert!(pending.state().matches(STATE));
        assert!(!pending.state().matches("a-different-state"));
        assert!(!pending.state().matches(""));
        Ok(())
    }

    /// The redemption step needs these to validate the callback against what
    /// was recorded at the start, not against the response being validated.
    #[test]
    fn the_pending_value_carries_what_redemption_needs() -> Result<(), Failure> {
        let pending = pending(ProtocolVersion::V2026_07_28)?;
        assert_eq!(pending.issuer(), AS);
        assert_eq!(pending.redirect_uri(), REDIRECT);
        assert_eq!(pending.code_verifier().as_str(), VERIFIER);
        assert_eq!(pending.resource().token_request().len(), 1);
        Ok(())
    }

    /// A source that hands out a different value each time, so the difference
    /// between two requests is observable.
    struct Sequence(std::cell::Cell<u32>);

    impl AuthorizationSecrets for Sequence {
        fn new_state(&self) -> Result<State, SecretError> {
            let next = self.0.get() + 1;
            self.0.set(next);
            Ok(State::new(format!("state-{next}")))
        }

        fn new_code_verifier(&self) -> Result<CodeVerifier, SecretError> {
            let next = self.0.get() + 1;
            self.0.set(next);
            CodeVerifier::new(format!("{next}{}", "a".repeat(42)))
                .map_err(|_| SecretError::Rejected)
        }
    }

    /// A callback from one request must not satisfy another's `state` check,
    /// so every request draws its own. `Fixed` cannot show this — it returns the
    /// same value every time — which is why this uses a counting source.
    #[test]
    fn each_request_draws_its_own_state() -> Result<(), Failure> {
        let secrets = Sequence(std::cell::Cell::new(0));
        let first = begin_authorization(
            &model_for(ProtocolVersion::V2026_07_28),
            &client()?,
            REDIRECT,
            resource()?,
            &secrets,
        )?;
        let second = begin_authorization(
            &model_for(ProtocolVersion::V2026_07_28),
            &client()?,
            REDIRECT,
            resource()?,
            &secrets,
        )?;

        assert_ne!(first.state(), second.state());
        assert!(
            !first.state().matches(second.state().as_str()),
            "one request's state must not satisfy another's check"
        );
        assert_ne!(first.code_verifier(), second.code_verifier());
        Ok(())
    }

    #[test]
    fn an_unusable_endpoint_is_named_rather_than_rendered() -> Result<(), Failure> {
        let pending = pending(ProtocolVersion::V2026_07_28)?;
        match pending.authorization_url("not a url", ChallengeMethod::S256) {
            Err(super::BeginError::Endpoint { endpoint, .. }) => {
                assert_eq!(endpoint, "not a url");
                Ok(())
            }
            Ok(_) | Err(_) => Err(Failure::Begin(super::BeginError::Secrets(
                SecretError::EntropyUnavailable,
            ))),
        }
    }
}
