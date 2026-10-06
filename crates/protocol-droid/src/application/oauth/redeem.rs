//! Completing the authorization-code exchange: trading the code for a token.
//!
//! [`begin_authorization`](super::client::begin_authorization) sends the user
//! somewhere, and [`authorize_callback`](super::callback::authorize_callback)
//! validates what comes back. Neither of those produces a token, and this is
//! the step that does.
//!
//! # The request is assembled, never supplied
//!
//! Every parameter of the token request is read off the
//! [`AuthorizedCode`] — the value the callback step produced — rather than
//! passed in by the caller. That is not tidiness. RFC 6749 §4.1.3 requires the
//! `redirect_uri` to be **identical** to the one in the authorization request,
//! and a caller that supplies it separately can supply a different one. The
//! same goes for the `code_verifier`: presenting anything other than the value
//! the `code_challenge` was derived from is a failed PKCE check, and the
//! failure is the server refusing a code the client genuinely owns.
//!
//! # The code is consumed, so it cannot be spent twice
//!
//! [`redeem`] takes the [`AuthorizedCode`] by value, and `AuthorizedCode` is
//! not [`Clone`]. A second redemption needs a second code, which means another
//! user-visible round trip — so a retry after a dropped connection cannot
//! silently spend the same code twice, which is precisely the situation in
//! which the second caller would otherwise win.
//!
//! RFC 6749 §4.1.2 makes the server deny a reused code regardless. This is the
//! client half of the same rule, and it holds because the type system holds it,
//! not because a comment asks.

use std::fmt;

use crate::application::oauth::callback::AuthorizedCode;
use crate::domain::oauth::issued_token::{IssuedToken, TokenResponseError};
use crate::domain::oauth::pkce::CodeVerifier;
use crate::domain::oauth::registration::{ClientId, ClientSecret, RegisteredClient};
use crate::domain::oauth::resource::ResourceParameters;

/// One token request, assembled from an [`AuthorizedCode`] and a registration.
///
/// Borrowed rather than owned because every member already exists in one of
/// those two values and copying it would create a second copy of a credential
/// to keep alive for no reason.
///
/// # Examples
///
/// An implementor reads what it needs and encodes it however its transport
/// requires; the `oauth` adapter reads all of it:
///
/// ```
/// # use protocol_droid::application::oauth::redeem::{TokenEndpoint, TokenEndpointError, TokenRequest};
/// # use protocol_droid::domain::oauth::issued_token::{IssuedToken, TokenResponseError};
/// struct Transport;
///
/// impl TokenEndpoint for Transport {
///     async fn redeem(
///         &self,
///         request: TokenRequest<'_>,
///     ) -> Result<IssuedToken, TokenEndpointError> {
///         let body = format!(
///             "grant_type=authorization_code&code={}&redirect_uri={}",
///             request.code(),
///             request.redirect_uri(),
///         );
///         println!("{body}");
///         # Ok::<IssuedToken, TokenResponseError>(
///             #     IssuedToken::parse(br#"{"access_token":"at","token_type":"Bearer"}"#)?
///             # )
///         .map_err(TokenEndpointError::Refused)
///     }
/// }
/// ```
#[derive(Debug)]
pub struct TokenRequest<'a> {
    code: &'a str,
    redirect_uri: &'a str,
    verifier: &'a CodeVerifier,
    client_id: &'a ClientId,
    client_secret: Option<&'a ClientSecret>,
    resource: &'a ResourceParameters,
}

impl<'a> TokenRequest<'a> {
    /// The authorization code to redeem.
    #[must_use]
    pub const fn code(&self) -> &'a str {
        self.code
    }

    /// The redirect URI, exactly as the authorization request carried it.
    #[must_use]
    pub const fn redirect_uri(&self) -> &'a str {
        self.redirect_uri
    }

    /// The PKCE verifier the `code_challenge` was derived from.
    #[must_use]
    pub const fn code_verifier(&self) -> &'a CodeVerifier {
        self.verifier
    }

    /// The client this exchange is being conducted as.
    #[must_use]
    pub const fn client_id(&self) -> &'a ClientId {
        self.client_id
    }

    /// The secret, for a confidential client only.
    ///
    /// [`None`] for a public client — one registered through a client
    /// identity metadata document, which has nowhere to keep a secret that is
    /// still secret. An adapter must **omit** the member in that case rather
    /// than sending it empty: an empty `client_secret` is a value, and RFC 6749
    /// §2.3.1 treats the presence of the member as the statement that the
    /// client is confidential.
    #[must_use]
    pub const fn client_secret(&self) -> Option<&'a ClientSecret> {
        self.client_secret
    }

    /// The RFC 8707 resource indicators this grant is bound to.
    ///
    /// Repeated as one `resource` member per indicator, which is the encoding
    /// §2 of that RFC defines.
    #[must_use]
    pub const fn resource(&self) -> &'a ResourceParameters {
        self.resource
    }
}

/// Asks an authorization server to trade a code for a token.
///
/// One method, and one because redemption is a single act. An adapter does the
/// HTTP; this crate decides what to ask for, and the domain decides what came
/// back.
///
/// Not dyn compatible, and that is deliberate. Redemption happens once per
/// client rather than once per request, so the call site takes `impl
/// TokenEndpoint` and the monomorphised future costs nothing that a box would
/// not.
pub trait TokenEndpoint {
    /// Trade the authorization code for a token.
    ///
    /// # Errors
    ///
    /// [`TokenEndpointError::Refused`] when the server answered RFC 6749 §5.2,
    /// or with a body that was not a usable token. [`TokenEndpointError::Unavailable`]
    /// when the server could not be asked or its answer could not be read.
    fn redeem(
        &self,
        request: TokenRequest<'_>,
    ) -> impl Future<Output = Result<IssuedToken, TokenEndpointError>> + Send;
}

/// Why a redemption did not produce a token.
///
/// `#[error]` text never carries the code, the verifier or the secret. This is
/// the first place any of those three could reach a message somebody pastes
/// into a bug report.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenEndpointError {
    /// The server refused, or answered with something unusable.
    #[error(transparent)]
    Refused(#[from] TokenResponseError),

    /// The server could not be asked, or its answer could not be read.
    #[error("the token endpoint could not be reached: {0}")]
    Unavailable(String),
}

/// Why [`redeem`] did not produce a token.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RedeemError {
    /// The registration presented does not match the client that started the
    /// exchange.
    ///
    /// [`redeem`] compares rather than trusting, because the client id is a
    /// parameter the adapter is about to send: a caller that reached for the
    /// wrong registration would otherwise produce a token request naming one
    /// client with a `code_verifier` generated for another, and the server's
    /// answer would be an `invalid_grant` with nothing to explain it.
    #[error("the registration is for {presented}, but this code was issued to {expected}")]
    ClientMismatch {
        /// The client the code was issued to.
        expected: ClientId,
        /// The client the caller presented.
        presented: ClientId,
    },

    /// The endpoint refused, or could not be reached.
    #[error(transparent)]
    Endpoint(#[from] TokenEndpointError),
}

/// Trade a validated authorization code for a token.
///
/// The code is consumed. See the module docs for why that is the single-use
/// rule rather than a convention.
///
/// # Errors
///
/// [`RedeemError::ClientMismatch`] when the registration is not the client the
/// code was issued to. [`RedeemError::Endpoint`] when the endpoint refused or
/// could not be reached; that carries RFC 6749 §5.2's own error code when the
/// server named one.
///
/// # Examples
///
/// ```
/// # use protocol_droid::application::oauth::redeem::{RedeemError, redeem};
/// # use protocol_droid::domain::oauth::registration::{ClientId, RegisteredClient};
/// # async fn run(
/// #     code: protocol_droid::application::oauth::callback::AuthorizedCode,
/// #     client: RegisteredClient,
/// #     endpoint: &(impl protocol_droid::application::oauth::redeem::TokenEndpoint + Sync),
/// # ) -> Result<(), RedeemError> {
/// let issued = redeem(code, &client, endpoint).await?;
/// println!("scope granted: {:?}", issued.scope());
/// # Ok(())
/// # }
/// ```
pub async fn redeem(
    code: AuthorizedCode,
    client: &RegisteredClient,
    endpoint: &(impl TokenEndpoint + Sync),
) -> Result<IssuedToken, RedeemError> {
    if code.client_id() != client.client_id() {
        return Err(RedeemError::ClientMismatch {
            expected: code.client_id().clone(),
            presented: client.client_id().clone(),
        });
    }

    let request = TokenRequest {
        code: code.code(),
        // Read back off the code rather than taken from the registration: the
        // registration records where the client is willing to be redirected
        // *now*, and §4.1.3 asks for where this request was actually sent.
        redirect_uri: code.redirect_uri(),
        verifier: code.code_verifier(),
        client_id: code.client_id(),
        client_secret: client.secret(),
        resource: code.resource().token_request(),
    };

    endpoint
        .redeem(request)
        .await
        .map_err(RedeemError::Endpoint)
}

/// Rendered only so a `TokenRequest` in a log line names its fields without
/// their values.
impl fmt::Display for TokenRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "TokenRequest {{ grant_type: authorization_code, code: …, redirect_uri: {}, \
             code_verifier: …, client_id: {}, client_secret: {}, resource: [{}] }}",
            self.redirect_uri,
            self.client_id,
            self.client_secret.is_some(),
            self.resource.iter().collect::<Vec<_>>().join(", "),
        )
    }
}
#[cfg(test)]
mod tests {
    use std::assert_matches;
    use std::future::Future;
    use std::sync::Mutex;
    use std::task::{Context, Poll, Waker};

    use super::{RedeemError, TokenEndpoint, TokenEndpointError, TokenRequest, redeem};
    use crate::application::oauth::callback::{
        AuthorizationCallback, AuthorizedCode, authorize_callback,
    };
    use crate::application::oauth::client::{
        AuthorizationSecrets, PendingAuthorization, SecretError, State, begin_authorization,
    };
    use crate::domain::ProtocolVersion;
    use crate::domain::mcp::authorization::{AuthorizationModel, authorization_model};
    use crate::domain::mcp::resource_metadata::ResourceUri;
    use crate::domain::oauth::issued_token::{IssuedToken, TokenResponseError};
    use crate::domain::oauth::pkce::CodeVerifier;
    use crate::domain::oauth::registration::{ClientId, ClientSecret, RegisteredClient};
    use crate::domain::oauth::resource::ResourceParameters;

    const AS: &str = "https://auth.example.com";
    const CLIENT_ID: &str = "https://client.example.com/metadata.json";
    const REDIRECT: &str = "https://client.example.com/callback";
    const RESOURCE: &str = "https://api.example.com/app";
    const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    const STATE: &str = "a-state-value";
    const GRANTED: &[u8] = br#"{"access_token":"at-1","token_type":"Bearer","expires_in":3600}"#;

    /// Every error in play implements `std::error::Error`, so a test can `?`
    /// through the lot and print whatever the engine refused with, rather than
    /// unwrapping the first call that happens to be fallible.
    type Outcome = Result<(), Box<dyn std::error::Error>>;

    /// Drives a future that is expected never to pend.
    ///
    /// `#[tokio::test]` is not available under `oauth`: the tokio macros come in
    /// with `http`, and pulling a runtime in to test fakes that return
    /// immediately would be a heavier dependency than the thing under test.
    /// `Waker::noop` is correct for a future that resolves on its first poll,
    /// and returning `None` on `Pending` is what makes that assumption
    /// checkable rather than merely stated.
    fn now<T>(future: impl Future<Output = T>) -> Option<T> {
        let mut future = std::pin::pin!(future);
        match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(output) => Some(output),
            Poll::Pending => None,
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

    /// A client registered through a metadata document, so public: no secret.
    fn public_client() -> Result<RegisteredClient, Box<dyn std::error::Error>> {
        Ok(RegisteredClient::from_metadata_document(
            AS,
            ClientId::metadata_url(CLIENT_ID)?,
        ))
    }

    fn confidential_client() -> Result<RegisteredClient, Box<dyn std::error::Error>> {
        Ok(RegisteredClient::dynamic(
            AS,
            ClientId::opaque("client-1"),
            Some(ClientSecret::issued("s3cret")),
        )?)
    }

    /// A validated code for `client`, made through the real path so the values
    /// in it are the ones a live flow would carry.
    fn authorized(client: &RegisteredClient) -> Result<AuthorizedCode, Box<dyn std::error::Error>> {
        let resource = ResourceUri::new(RESOURCE)?;
        let pending: PendingAuthorization = begin_authorization(
            &model(),
            client,
            REDIRECT,
            ResourceParameters::single(&resource),
            &Fixed,
        )?;
        let callback = AuthorizationCallback::granted("the-code", STATE);
        Ok(authorize_callback(&model(), &pending, &callback)?)
    }

    /// What the fake saw, copied out so it can outlive the borrow the
    /// `TokenRequest` held.
    #[derive(Debug)]
    struct Recorded {
        code: String,
        redirect_uri: String,
        verifier: String,
        client_id: String,
        client_secret: Option<String>,
        resources: Vec<String>,
    }

    enum Outcome_ {
        Granted,
        Refused(TokenResponseError),
        Unreachable,
    }

    struct Recording {
        recorded: Mutex<Option<Recorded>>,
        outcome: Outcome_,
    }

    impl Recording {
        fn granting() -> Self {
            Self {
                recorded: Mutex::new(None),
                outcome: Outcome_::Granted,
            }
        }

        fn refusing(error: TokenResponseError) -> Self {
            Self {
                recorded: Mutex::new(None),
                outcome: Outcome_::Refused(error),
            }
        }

        fn unreachable() -> Self {
            Self {
                recorded: Mutex::new(None),
                outcome: Outcome_::Unreachable,
            }
        }

        fn seen(&self) -> Option<Recorded> {
            self.recorded.lock().ok().and_then(|mut guard| guard.take())
        }
    }

    impl TokenEndpoint for Recording {
        // Not `async`: this fake resolves on its first poll, so an `async fn`
        // here would be an async block with no await point in it — which is a
        // `ready` wearing a hat.
        fn redeem(
            &self,
            request: TokenRequest<'_>,
        ) -> impl Future<Output = Result<IssuedToken, TokenEndpointError>> + Send {
            let recorded = Recorded {
                code: request.code().to_owned(),
                redirect_uri: request.redirect_uri().to_owned(),
                verifier: request.code_verifier().as_str().to_owned(),
                client_id: request.client_id().as_str().to_owned(),
                client_secret: request
                    .client_secret()
                    .map(|secret| secret.expose().to_owned()),
                resources: request.resource().iter().map(str::to_owned).collect(),
            };
            if let Ok(mut guard) = self.recorded.lock() {
                *guard = Some(recorded);
            }

            std::future::ready(match &self.outcome {
                Outcome_::Granted => {
                    IssuedToken::parse(GRANTED).map_err(TokenEndpointError::Refused)
                }
                Outcome_::Refused(error) => Err(TokenEndpointError::Refused(error.clone())),
                Outcome_::Unreachable => Err(TokenEndpointError::Unavailable(
                    "no route to host".to_owned(),
                )),
            })
        }
    }

    /// A redeemed grant is the thing every downstream caller wants, so the
    /// happy path gets a test that says so rather than one that only checks it
    /// did not error.
    #[test]
    fn a_valid_code_becomes_a_token() -> Outcome {
        let client = public_client()?;
        let endpoint = Recording::granting();

        let issued =
            now(redeem(authorized(&client)?, &client, &endpoint)).ok_or("the future pended")??;

        assert_eq!(issued.access_token().as_str(), "at-1");
        Ok(())
    }

    /// §4.1.3 asks for a `redirect_uri` *identical* to the authorization
    /// request's, and the verifier the challenge was derived from. Both come
    /// off the code rather than off the caller for exactly that reason.
    #[test]
    fn the_request_is_assembled_from_the_code_not_the_caller() -> Outcome {
        let client = public_client()?;
        let endpoint = Recording::granting();

        now(redeem(authorized(&client)?, &client, &endpoint)).ok_or("the future pended")??;

        let seen = endpoint.seen().ok_or("the endpoint was never asked")?;
        assert_eq!(seen.code, "the-code");
        assert_eq!(seen.redirect_uri, REDIRECT);
        assert_eq!(seen.verifier, VERIFIER);
        assert_eq!(seen.client_id, CLIENT_ID);
        Ok(())
    }

    /// RFC 8707 §2. Dropping the indicator here would let the server mint a
    /// token for an audience the client never asked for.
    #[test]
    fn the_resource_binding_is_carried_to_the_token_request() -> Outcome {
        let client = public_client()?;
        let endpoint = Recording::granting();

        now(redeem(authorized(&client)?, &client, &endpoint)).ok_or("the future pended")??;

        let seen = endpoint.seen().ok_or("the endpoint was never asked")?;
        assert_eq!(seen.resources, vec![RESOURCE.to_owned()]);
        Ok(())
    }

    /// A public client has no secret, and §2.3.1 reads the member's *presence*
    /// as a claim of confidentiality — so the adapter must be able to omit it.
    #[test]
    fn a_public_client_sends_no_secret() -> Outcome {
        let client = public_client()?;
        let endpoint = Recording::granting();

        now(redeem(authorized(&client)?, &client, &endpoint)).ok_or("the future pended")??;

        let seen = endpoint.seen().ok_or("the endpoint was never asked")?;
        assert_eq!(seen.client_secret, None);
        Ok(())
    }

    #[test]
    fn a_confidential_client_sends_its_secret() -> Outcome {
        let client = confidential_client()?;
        let endpoint = Recording::granting();

        now(redeem(authorized(&client)?, &client, &endpoint)).ok_or("the future pended")??;

        let seen = endpoint.seen().ok_or("the endpoint was never asked")?;
        assert_eq!(seen.client_secret.as_deref(), Some("s3cret"));
        Ok(())
    }

    /// The check that stops a caller reaching for the wrong registration. The
    /// alternative is a token request naming one client with a verifier
    /// generated for another, and the server's `invalid_grant` explains
    /// nothing about which of the two was wrong.
    #[test]
    fn a_registration_for_another_client_is_refused_before_the_request() -> Outcome {
        let issued_for = confidential_client()?;
        let endpoint = Recording::granting();

        let outcome = now(redeem(
            authorized(&issued_for)?,
            &public_client()?,
            &endpoint,
        ));

        assert_matches!(outcome, Some(Err(RedeemError::ClientMismatch { .. })));
        assert!(
            endpoint.seen().is_none(),
            "a mismatched code was sent anyway"
        );
        Ok(())
    }

    /// §5.2's code reaches the caller, because `invalid_grant` and
    /// `invalid_client` call for different responses and a bare message does not
    /// distinguish them.
    #[test]
    fn the_servers_own_refusal_reaches_the_caller() -> Outcome {
        let client = public_client()?;
        let endpoint = Recording::refusing(TokenResponseError::AuthorizationServer {
            error: "invalid_grant".to_owned(),
            description: None,
        });

        let outcome = now(redeem(authorized(&client)?, &client, &endpoint));

        assert_matches!(
            outcome,
            Some(Err(RedeemError::Endpoint(TokenEndpointError::Refused(
                TokenResponseError::AuthorizationServer { error, ..
            })))) if error == "invalid_grant"
        );
        Ok(())
    }

    /// An endpoint that could not be reached is not the same as an endpoint
    /// that refused: the first is worth retrying later, the second is not.
    #[test]
    fn an_unreachable_endpoint_is_not_reported_as_a_refusal() -> Outcome {
        let client = public_client()?;
        let endpoint = Recording::unreachable();

        let outcome = now(redeem(authorized(&client)?, &client, &endpoint));

        assert_matches!(
            outcome,
            Some(Err(RedeemError::Endpoint(TokenEndpointError::Unavailable(
                _
            ))))
        );
        Ok(())
    }

    /// No error on any path carries the code, the verifier or the secret.
    /// These are the three values a pasted bug report would leak.
    #[test]
    fn no_error_text_carries_a_credential() -> Outcome {
        let client = confidential_client()?;
        let endpoint = Recording::refusing(TokenResponseError::AuthorizationServer {
            error: "invalid_grant".to_owned(),
            description: None,
        });

        let outcome = now(redeem(authorized(&client)?, &client, &endpoint));

        let rendered = match outcome {
            Some(Err(error)) => error.to_string(),
            _ => String::from("no error was produced"),
        };
        for secret in ["the-code", VERIFIER, "s3cret"] {
            assert!(!rendered.contains(secret), "{rendered} leaked {secret}");
        }
        Ok(())
    }
}
