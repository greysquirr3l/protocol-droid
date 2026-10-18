//! Getting a bearer token for an MCP server.
//!
//! [`OAuthAuthorizer`] runs the flow the authorization page lays out, from the
//! `401` to the token, and hands the transport a [`BearerAuth`] to ask:
//!
//! ```text
//! challenge → protected resource metadata → authorization server metadata
//!   → client registration → authorization (PKCE, resource, scope, state)
//!   → callback checks (state, iss) → token request → bearer
//! ```
//!
//! Each arrow is a decision the sibling modules already make (`discovery`,
//! `metadata`, `registration`, `client`, `callback`, `redeem`, `flow`); this
//! module is the order they run in and the state that survives between runs —
//! the token, the scopes already granted, and the clients registered, each
//! filed under the issuer that issued it.
//! <https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization>
//!
//! # What it will not do
//!
//! Fetch an arbitrary URL on its own say-so. It reads the documents the
//! protocol names, from the places the protocol names them, and nothing a
//! response invents beyond that: a resource-metadata URL from a `401` is
//! followed because the specification says to, and the authorization server it
//! leads to is the one the resource metadata lists.

use core::future::Future;
use core::pin::Pin;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};
use url::Url;

use crate::application::client::bearer::{BearerAuth, BearerError};
use crate::application::oauth::callback::{CallbackError, authorize_callback};
use crate::application::oauth::client::{AuthorizationSecrets, BeginError, begin_authorization};
use crate::application::oauth::environment::{EnvironmentError, Fetched, OAuthEnvironment};
use crate::application::oauth::flow::{
    initial_scope, may_step_up, step_up_scope, with_offline_access,
};
use crate::application::oauth::metadata::{AuthorizationServer, MetadataError, ProtectedResource};
use crate::application::oauth::redeem::{
    RedeemError, RefreshRequest, TokenEndpoint as _, redeem_with_scope,
};
use crate::application::oauth::registration::ClientMetadata;
use crate::domain::ProtocolVersion;
use crate::domain::mcp::authorization::{AuthorizationModel, authorization_model};
use crate::domain::mcp::resource_metadata::ResourceUri;
use crate::domain::oauth::challenge::{BearerChallenge, parse_bearer};
use crate::domain::oauth::discovery::{
    authorization_server_metadata_urls, canonical_resource, protected_resource_metadata_urls,
};
use crate::domain::oauth::pkce::ChallengeMethod;
use crate::domain::oauth::registration::{
    ClientId, ClientSecret, RegisteredClient, TokenAuthMethod,
};
use crate::domain::oauth::resource::ResourceParameters;

/// Why a token could not be obtained.
#[derive(Debug, thiserror::Error)]
pub enum AuthorizeError {
    /// A document the flow needs could not be read.
    #[error("{what} could not be read: {reason}")]
    Discovery {
        /// Which document.
        what: &'static str,
        /// Why not.
        reason: String,
    },
    /// The resource metadata describes a different resource than the one asked.
    ///
    /// Following it would send the user to authorize access to something other
    /// than the server they are using.
    #[error("the resource metadata is for `{found}`, not `{expected}`")]
    ResourceMismatch {
        /// This server's canonical URI.
        expected: String,
        /// What the metadata claimed.
        found: String,
    },
    /// The authorization server does not offer PKCE with `S256`.
    ///
    /// "MCP clients MUST verify PKCE support": a server that does not list it,
    /// or does not list the method, is refused rather than assumed.
    #[error("the authorization server does not advertise PKCE with S256")]
    NoPkce,
    /// No way to obtain a client id: nothing pre-registered, no metadata
    /// document the server supports, no registration endpoint.
    #[error("no client registration is possible with this authorization server")]
    NoRegistration,
    /// Dynamic client registration was refused or unreadable.
    #[error("dynamic client registration failed: {0}")]
    Registration(String),
    /// Pre-registered credentials exist for a different authorization server.
    #[error("the pre-registered credentials belong to `{registered}`, but the server now names `{named}`")]
    CredentialsForAnotherIssuer {
        /// The issuer the credentials were registered with.
        registered: String,
        /// The issuer the resource metadata now names.
        named: String,
    },
    /// The authorization request could not be built.
    #[error(transparent)]
    Begin(#[from] BeginError),
    /// The callback was refused.
    #[error(transparent)]
    Callback(#[from] CallbackError),
    /// The code could not be redeemed.
    #[error(transparent)]
    Redeem(#[from] RedeemError),
    /// The environment could not do something the flow asked.
    #[error(transparent)]
    Environment(#[from] EnvironmentError),
    /// A metadata document was unusable.
    #[error(transparent)]
    Metadata(#[from] MetadataError),
    /// The server kept refusing, and the client stopped asking.
    #[error("the server still refused after {0} authorization attempts")]
    AttemptsExhausted(u32),
}

/// Credentials issued to this client ahead of time.
#[derive(Debug, Clone)]
pub struct PreRegistered {
    issuer: Option<String>,
    client_id: ClientId,
    client_secret: Option<ClientSecret>,
}

impl PreRegistered {
    /// A client id, and a secret when the client is confidential.
    #[must_use]
    pub fn new(client_id: &str, client_secret: Option<&str>) -> Self {
        Self {
            issuer: None,
            client_id: ClientId::opaque(client_id),
            client_secret: client_secret.map(ClientSecret::issued),
        }
    }

    /// These credentials, as issued by the authorization server `issuer`.
    ///
    /// Credentials are specific to the server that issued them. With an issuer
    /// recorded, a resource that later names a different authorization server
    /// is an error rather than a reason to present an id the new server never
    /// issued.
    #[must_use]
    pub fn issued_by(mut self, issuer: impl Into<String>) -> Self {
        self.issuer = Some(issuer.into());
        self
    }
}

/// What the flow is told about this client and this server.
#[derive(Debug, Clone)]
pub struct OAuthConfig {
    server: Url,
    model: AuthorizationModel,
    redirect_uri: String,
    client_name: String,
    client_metadata_url: Option<String>,
    pre_registered: Option<PreRegistered>,
    refresh_token: bool,
}

impl OAuthConfig {
    /// A client of the MCP server at `server`, speaking `version`.
    ///
    /// # Errors
    ///
    /// [`url::ParseError`] when `server` is not a URL.
    pub fn new(server: &str, version: ProtocolVersion) -> Result<Self, url::ParseError> {
        Ok(Self {
            server: Url::parse(server)?,
            model: authorization_model(version),
            redirect_uri: "http://localhost:3000/callback".to_owned(),
            client_name: "protocol-droid".to_owned(),
            client_metadata_url: None,
            pre_registered: None,
            refresh_token: false,
        })
    }

    /// Where the authorization server sends the user back to.
    #[must_use]
    pub fn redirect_to(mut self, redirect_uri: impl Into<String>) -> Self {
        self.redirect_uri = redirect_uri.into();
        self
    }

    /// The name shown to the user at consent, and sent when registering.
    #[must_use]
    pub fn named(mut self, client_name: impl Into<String>) -> Self {
        self.client_name = client_name.into();
        self
    }

    /// The HTTPS URL of this client's metadata document, used as its
    /// `client_id` with servers that support such ids.
    #[must_use]
    pub fn with_metadata_document(mut self, url: impl Into<String>) -> Self {
        self.client_metadata_url = Some(url.into());
        self
    }

    /// Credentials issued to this client by the authorization server.
    #[must_use]
    pub fn with_pre_registered(mut self, credentials: PreRegistered) -> Self {
        self.pre_registered = Some(credentials);
        self
    }

    /// Asks for a refresh token where the server offers one.
    ///
    /// Adds `offline_access` to the scope only when the server lists it, and
    /// `refresh_token` to the grant types this client registers with.
    #[must_use]
    pub const fn wanting_refresh_tokens(mut self) -> Self {
        self.refresh_token = true;
        self
    }
}

/// What it takes to trade a refresh token for a new access token without
/// asking the user again.
#[derive(Clone)]
struct Refreshable {
    token: String,
    token_endpoint: String,
    client: RegisteredClient,
    resource: ResourceParameters,
}

impl core::fmt::Debug for Refreshable {
    /// Never prints the token.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Refreshable").finish_non_exhaustive()
    }
}

#[derive(Default)]
struct State {
    token: Option<String>,
    granted: Option<String>,
    refresh: Option<Refreshable>,
    clients: HashMap<String, RegisteredClient>,
    pre_registered: Option<PreRegistered>,
    attempts: u32,
}

/// Runs the authorization flow and remembers its result.
pub struct OAuthAuthorizer<E, S> {
    config: OAuthConfig,
    environment: E,
    secrets: S,
    state: Mutex<State>,
}

impl<E, S> core::fmt::Debug for OAuthAuthorizer<E, S> {
    /// Never prints the token.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OAuthAuthorizer")
            .field("server", &self.config.server.as_str())
            .finish_non_exhaustive()
    }
}

impl<E: OAuthEnvironment, S: AuthorizationSecrets + Send + Sync + 'static> OAuthAuthorizer<E, S> {
    /// An authorizer for `config`, using `environment` and drawing its
    /// unguessable values from `secrets`.
    #[must_use]
    pub fn new(config: OAuthConfig, environment: E, secrets: S) -> Arc<Self> {
        let pre_registered = config.pre_registered.clone();
        Arc::new(Self {
            config,
            environment,
            secrets,
            state: Mutex::new(State {
                pre_registered,
                ..State::default()
            }),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keeps the token and the scope it was granted.
    fn record(&self, token: String, granted: Option<String>, refresh: Option<Refreshable>) {
        let mut state = self.state();
        state.token = Some(token);
        state.granted = granted;
        state.refresh = refresh;
    }

    /// Trades the held refresh token for a new access token.
    ///
    /// `false` when there is none, or the server refused it (spent, revoked,
    /// expired): the caller then runs the whole flow. A refused token is
    /// forgotten, so it is not offered a second time.
    async fn try_refresh(&self) -> bool {
        let Some(held) = self.state().refresh.clone() else {
            return false;
        };
        let endpoint = self.environment.token_endpoint(&held.token_endpoint);
        let request = RefreshRequest::new(&held.token, &held.client, &held.resource, None);
        let outcome = endpoint.refresh(request).await;
        let mut state = self.state();
        if let Ok(tokens) = outcome {
            // A server may rotate the refresh token; when it does not, the
            // old one stays valid.
            let rotated = tokens
                .refresh_token()
                .map_or_else(|| held.token.clone(), |token| token.as_str().to_owned());
            state.token = Some(tokens.access_token().as_str().to_owned());
            state.refresh = Some(Refreshable {
                token: rotated,
                ..held
            });
            true
        } else {
            state.refresh = None;
            false
        }
    }

    /// The client already held for this issuer.
    fn held_client(&self, server: &AuthorizationServer) -> Option<RegisteredClient> {
        self.state().clients.get(&server.issuer).cloned()
    }

    /// The pre-registered credentials, bound to this issuer the first time
    /// they are used — and only to it.
    fn bind_pre_registered(
        &self,
        server: &AuthorizationServer,
    ) -> Result<Option<RegisteredClient>, AuthorizeError> {
        let mut pending = self.state().pre_registered.take();
        let Some(credentials) = pending.take() else {
            return Ok(None);
        };
        if let Some(registered) = &credentials.issuer
            && *registered != server.issuer
        {
            // Put back: a later run against the right server can still use it.
            self.state().pre_registered = Some(credentials.clone());
            return Err(AuthorizeError::CredentialsForAnotherIssuer {
                registered: registered.clone(),
                named: server.issuer.clone(),
            });
        }
        let method = auth_method(credentials.client_secret.is_some(), server);
        let Ok(client) = RegisteredClient::dynamic(
            &server.issuer,
            credentials.client_id,
            credentials.client_secret,
        ) else {
            return Ok(None);
        };
        let client = client.with_auth_method(method);
        self.keep_client(&server.issuer, &client);
        Ok(Some(client))
    }

    /// Files `client` under the issuer that issued it.
    fn keep_client(&self, issuer: &str, client: &RegisteredClient) {
        self.state().clients.insert(issuer.to_owned(), client.clone());
    }

    /// Obtains a token for `challenge`, replacing any held.
    ///
    /// # Errors
    ///
    /// [`AuthorizeError`] naming the stage that failed.
    pub async fn authorize(&self, challenge: &BearerChallenge) -> Result<(), AuthorizeError> {
        let resource = self.protected_resource(challenge).await?;
        let issuer = resource
            .authorization_servers
            .first()
            .cloned()
            .ok_or_else(|| AuthorizeError::Discovery {
                what: "the protected resource metadata",
                reason: "it lists no authorization server".to_owned(),
            })?;
        let server = self.authorization_server(&issuer).await?;

        let advertises_s256 = server
            .code_challenge_methods_supported
            .as_ref()
            .is_some_and(|methods| methods.iter().any(|method| method == "S256"));
        if !advertises_s256 {
            return Err(AuthorizeError::NoPkce);
        }

        let client = self.client_for(&server).await?;
        let scope = self.scope_for(challenge, &resource, &server);

        // The identifier the resource published, not one derived from the URL
        // this client dialled. They are the same resource — `check_resource`
        // has already said so — but RFC 8707 asks for the identifier as the
        // resource names itself, and a server at `/mcp` may publish its origin.
        let resource_uri =
            ResourceUri::new(&resource.resource).map_err(|error| AuthorizeError::Discovery {
                what: "the resource identifier in the protected resource metadata",
                reason: error.to_string(),
            })?;
        let mut pending = begin_authorization(
            &self.config.model,
            &client,
            &self.config.redirect_uri,
            ResourceParameters::single(&resource_uri),
            &self.secrets,
        )?
        .with_iss_advertised(server.authorization_response_iss_parameter_supported);
        if let Some(scope) = &scope {
            pending = pending.with_scope(scope);
        }
        let url = pending.authorization_url(&server.authorization_endpoint, ChallengeMethod::S256)?;

        let callback = self.environment.authorize(url.as_str()).await?;
        let code = authorize_callback(&self.config.model, &pending, &callback)?;
        let endpoint = self.environment.token_endpoint(&server.token_endpoint);
        let tokens = redeem_with_scope(code, &client, &endpoint, scope.as_deref()).await?;

        let granted = tokens.scope().map(ToOwned::to_owned).or(scope);
        let refresh = tokens.refresh_token().map(|token| Refreshable {
            token: token.as_str().to_owned(),
            token_endpoint: server.token_endpoint.clone(),
            client: client.clone(),
            resource: ResourceParameters::single(&resource_uri),
        });
        self.record(
            tokens.access_token().as_str().to_owned(),
            granted,
            refresh,
        );
        Ok(())
    }

    /// The scope to ask for: the challenge, widened by what was granted before.
    fn scope_for(
        &self,
        challenge: &BearerChallenge,
        resource: &ProtectedResource,
        server: &AuthorizationServer,
    ) -> Option<String> {
        let previous = self.state().granted.clone();
        let scope = if previous.is_some() {
            step_up_scope(previous.as_deref(), challenge)
        } else {
            initial_scope(challenge, resource.scopes_supported.as_deref())
        };
        if self.config.refresh_token {
            with_offline_access(scope, &server.scopes_supported)
        } else {
            scope
        }
    }

    async fn get_document(&self, url: &str) -> Result<Fetched, EnvironmentError> {
        self.environment.get(url).await
    }

    /// Reads the protected resource metadata, from the challenge's URL when it
    /// names one and from the well-known locations when it does not.
    async fn protected_resource(
        &self,
        challenge: &BearerChallenge,
    ) -> Result<ProtectedResource, AuthorizeError> {
        let urls = challenge.resource_metadata.as_ref().map_or_else(
            || protected_resource_metadata_urls(&self.config.server),
            |url| vec![url.clone()],
        );
        let mut last = "no location answered".to_owned();
        for url in urls {
            let fetched = match self.get_document(&url).await {
                Ok(fetched) if fetched.is_success() => fetched,
                Ok(fetched) => {
                    last = format!("{url} answered {}", fetched.status);
                    continue;
                }
                Err(error) => {
                    last = error.to_string();
                    continue;
                }
            };
            let document = ProtectedResource::parse(&fetched.body)?;
            return self.check_resource(document);
        }
        Err(AuthorizeError::Discovery {
            what: "the protected resource metadata",
            reason: last,
        })
    }

    /// The metadata must describe this server, or a parent of it.
    ///
    /// A document served from the root describes the origin, and a server at a
    /// path under it is within that — so a prefix, on a path boundary, is the
    /// same resource. Anything else is a different resource.
    fn check_resource(&self, document: ProtectedResource) -> Result<ProtectedResource, AuthorizeError> {
        let expected = canonical_resource(&self.config.server);
        let found = Url::parse(&document.resource).map_or_else(
            |_| document.resource.clone(),
            |url| canonical_resource(&url),
        );
        let covers = expected == found
            || expected
                .strip_prefix(&found)
                .is_some_and(|rest| rest.starts_with('/'));
        if covers {
            Ok(document)
        } else {
            Err(AuthorizeError::ResourceMismatch { expected, found })
        }
    }

    /// Reads the authorization server's metadata, trying the locations in the
    /// order the specification gives and skipping any whose `issuer` is not the
    /// one the URL was built for.
    async fn authorization_server(&self, issuer: &str) -> Result<AuthorizationServer, AuthorizeError> {
        let parsed = Url::parse(issuer).map_err(|error| AuthorizeError::Discovery {
            what: "the authorization server's issuer",
            reason: error.to_string(),
        })?;
        let mut last = "no location answered".to_owned();
        for url in authorization_server_metadata_urls(&parsed) {
            let Ok(fetched) = self.get_document(&url).await else {
                continue;
            };
            if !fetched.is_success() {
                last = format!("{url} answered {}", fetched.status);
                continue;
            }
            match AuthorizationServer::parse(&fetched.body, issuer) {
                Ok(server) => return Ok(server),
                // "MUST NOT use the metadata" — and the next location may be the
                // genuine one, so this is a reason to keep looking, not to stop.
                Err(error) => last = error.to_string(),
            }
        }
        Err(AuthorizeError::Discovery {
            what: "the authorization server metadata",
            reason: last,
        })
    }

    /// The client to present to `server`, registering one if there is none.
    ///
    /// Priority is the specification's: what is already held for this issuer,
    /// then pre-registered credentials, then a metadata document the server
    /// supports, then dynamic registration. Credentials are kept per issuer, so
    /// a server that moves to a new authorization server is registered with
    /// afresh rather than shown an id it never issued.
    async fn client_for(&self, server: &AuthorizationServer) -> Result<RegisteredClient, AuthorizeError> {
        if let Some(held) = self.held_client(server) {
            return Ok(held);
        }
        if let Some(bound) = self.bind_pre_registered(server)? {
            return Ok(bound);
        }
        let model = &self.config.model;
        if server.client_id_metadata_document_supported
            && model.client_registration.supports_metadata_document()
            && let Some(url) = &self.config.client_metadata_url
            && let Ok(id) = ClientId::metadata_url(url)
        {
            let client = RegisteredClient::from_metadata_document(&server.issuer, id);
            self.keep_client(&server.issuer, &client);
            return Ok(client);
        }
        if model.client_registration.supports_dynamic()
            && let Some(endpoint) = &server.registration_endpoint
        {
            let client = self.register_dynamic(endpoint, server).await?;
            self.keep_client(&server.issuer, &client);
            return Ok(client);
        }
        Err(AuthorizeError::NoRegistration)
    }

    /// RFC 7591 registration, as a native application.
    ///
    /// `application_type` is required on `2026-07-28` and harmless elsewhere; a
    /// client redirecting to a loopback address is native.
    async fn register_dynamic(
        &self,
        endpoint: &str,
        server: &AuthorizationServer,
    ) -> Result<RegisteredClient, AuthorizeError> {
        let mut grants = vec!["authorization_code"];
        if self.config.refresh_token {
            grants.push("refresh_token");
        }
        let metadata = ClientMetadata::new(vec![self.config.redirect_uri.clone()])
            .map_err(|error| AuthorizeError::Registration(error.to_string()))?
            .with_client_name(&self.config.client_name)
            .with_application_type("native")
            .with_field("grant_types", json!(grants))
            .and_then(|metadata| metadata.with_field("response_types", json!(["code"])))
            .map_err(|error| AuthorizeError::Registration(error.to_string()))?;
        let metadata = match preferred_method(server) {
            Some(method) => metadata.with_token_endpoint_auth_method(method),
            None => metadata,
        };
        let body = serde_json::to_string(&metadata)
            .map_err(|error| AuthorizeError::Registration(error.to_string()))?;
        let fetched = self.environment.post_json(endpoint, &body).await?;
        if !fetched.is_success() {
            return Err(AuthorizeError::Registration(format!(
                "the registration endpoint answered {}",
                fetched.status
            )));
        }
        let response: Value = serde_json::from_slice(&fetched.body)
            .map_err(|error| AuthorizeError::Registration(error.to_string()))?;
        let id = response
            .get("client_id")
            .and_then(Value::as_str)
            .ok_or_else(|| AuthorizeError::Registration("the response has no client_id".to_owned()))?;
        let secret = response
            .get("client_secret")
            .and_then(Value::as_str)
            .map(ClientSecret::issued);
        let method = response
            .get("token_endpoint_auth_method")
            .and_then(Value::as_str)
            .and_then(TokenAuthMethod::parse)
            .unwrap_or_else(|| auth_method(secret.is_some(), server));
        let client = RegisteredClient::dynamic(&server.issuer, ClientId::opaque(id), secret)
            .map_err(|error| AuthorizeError::Registration(error.to_string()))?;
        Ok(client.with_auth_method(method))
    }
}

/// The method this client authenticates with, given what the server lists.
///
/// A public client sends no secret. A confidential one prefers HTTP Basic,
/// which RFC 8414 makes the default, and uses the form body when Basic is not
/// offered.
fn auth_method(has_secret: bool, server: &AuthorizationServer) -> TokenAuthMethod {
    if !has_secret {
        return TokenAuthMethod::None;
    }
    let offered = &server.token_endpoint_auth_methods_supported;
    if offered.is_empty() || offered.iter().any(|method| method == "client_secret_basic") {
        TokenAuthMethod::ClientSecretBasic
    } else if offered.iter().any(|method| method == "client_secret_post") {
        TokenAuthMethod::ClientSecretPost
    } else {
        TokenAuthMethod::None
    }
}

/// The method to ask for at registration, when the server lists any.
fn preferred_method(server: &AuthorizationServer) -> Option<&'static str> {
    let offered = &server.token_endpoint_auth_methods_supported;
    ["client_secret_basic", "client_secret_post", "none"]
        .into_iter()
        .find(|method| offered.iter().any(|candidate| candidate == method))
}

impl<E: OAuthEnvironment, S: AuthorizationSecrets + Send + Sync + 'static> BearerAuth
    for OAuthAuthorizer<E, S>
{
    fn bearer(&self) -> Option<String> {
        self.state().token.clone()
    }

    fn challenged<'a>(
        &'a self,
        status: u16,
        www_authenticate: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<(), BearerError>> + Send + 'a>> {
        Box::pin(async move {
            let attempts = {
                let mut state = self.state();
                state.attempts = state.attempts.saturating_add(1);
                state.attempts
            };
            // "Retry … no more than a few times and treat this as a permanent
            // authorization failure." The count is of refusals since the last
            // success, so an operation that keeps being refused stops.
            if !may_step_up(attempts.saturating_sub(1)) {
                return Err(BearerError(
                    AuthorizeError::AttemptsExhausted(attempts.saturating_sub(1)).to_string(),
                ));
            }
            let challenge = www_authenticate.and_then(parse_bearer).unwrap_or_default();
            let _ = status;
            // An expired token needs a new one, not a new consent. A challenge
            // for more scope does need the user, so it skips this.
            if !challenge.is_insufficient_scope() && self.try_refresh().await {
                return Ok(());
            }
            self.authorize(&challenge)
                .await
                .map_err(|error| BearerError(error.to_string()))
        })
    }

    fn accepted(&self) {
        self.state().attempts = 0;
    }
}

#[cfg(test)]
mod tests;
