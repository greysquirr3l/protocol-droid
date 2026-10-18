//! The refusals the flow makes before it asks a person for anything.
//!
//! The happy path is exercised end to end by the conformance suite's `auth/*`
//! scenarios; these pin the decisions that must hold without a server to
//! answer for them.

use core::future::Future;
use std::collections::HashMap;

use assert_matches::assert_matches;

use super::{AuthorizeError, OAuthAuthorizer, OAuthConfig, PreRegistered};
use crate::application::client::bearer::BearerAuth;
use crate::application::oauth::callback::AuthorizationCallback;
use crate::application::oauth::client::{AuthorizationSecrets, SecretError, State};
use crate::application::oauth::environment::{EnvironmentError, Fetched, OAuthEnvironment};
use crate::application::oauth::redeem::{TokenEndpoint, TokenEndpointError, TokenRequest};
use crate::domain::ProtocolVersion;
use crate::domain::oauth::challenge::BearerChallenge;
use crate::domain::oauth::pkce::CodeVerifier;
use crate::domain::oauth::issued_token::IssuedToken;

const SERVER: &str = "https://mcp.example/mcp";
const PRM_URL: &str = "https://mcp.example/.well-known/oauth-protected-resource/mcp";
const ISSUER: &str = "https://auth.example";

struct NoSecrets;

impl AuthorizationSecrets for NoSecrets {
    fn new_state(&self) -> Result<State, SecretError> {
        Err(SecretError::EntropyUnavailable)
    }
    fn new_code_verifier(&self) -> Result<CodeVerifier, SecretError> {
        Err(SecretError::EntropyUnavailable)
    }
}

struct NoTokens;

impl TokenEndpoint for NoTokens {
    fn redeem(
        &self,
        _request: TokenRequest<'_>,
    ) -> impl Future<Output = Result<IssuedToken, TokenEndpointError>> + Send {
        core::future::ready(Err(TokenEndpointError::Unavailable("not in this test".to_owned())))
    }
}

/// Answers from a table, and refuses to send anyone anywhere.
struct Documents(HashMap<String, String>);

impl OAuthEnvironment for Documents {
    type Token = NoTokens;

    fn token_endpoint(&self, _url: &str) -> NoTokens {
        NoTokens
    }

    fn get(&self, url: &str) -> impl Future<Output = Result<Fetched, EnvironmentError>> + Send {
        let fetched = self.0.get(url).map_or(
            Fetched {
                status: 404,
                body: Vec::new(),
            },
            |body| Fetched {
                status: 200,
                body: body.clone().into_bytes(),
            },
        );
        core::future::ready(Ok(fetched))
    }

    fn post_json(
        &self,
        _url: &str,
        _body: &str,
    ) -> impl Future<Output = Result<Fetched, EnvironmentError>> + Send {
        core::future::ready(Err(EnvironmentError::Unavailable("no registration here".to_owned())))
    }

    fn authorize(
        &self,
        _url: &str,
    ) -> impl Future<Output = Result<AuthorizationCallback, EnvironmentError>> + Send {
        core::future::ready(Err(EnvironmentError::Declined("no one is there".to_owned())))
    }
}

fn resource_document(resource: &str) -> String {
    format!(r#"{{"resource":"{resource}","authorization_servers":["{ISSUER}"]}}"#)
}

fn server_document(pkce: &str) -> String {
    format!(
        r#"{{"issuer":"{ISSUER}","authorization_endpoint":"{ISSUER}/authorize","token_endpoint":"{ISSUER}/token"{pkce}}}"#
    )
}

fn documents(resource: &str, pkce: &str) -> Documents {
    let mut table = HashMap::new();
    table.insert(PRM_URL.to_owned(), resource_document(resource));
    let server = server_document(pkce);
    table.insert(format!("{ISSUER}/.well-known/oauth-authorization-server"), server.clone());
    table.insert(format!("{ISSUER}/.well-known/openid-configuration"), server);
    Documents(table)
}

fn authorizer(environment: Documents) -> Result<std::sync::Arc<OAuthAuthorizer<Documents, NoSecrets>>, url::ParseError> {
    let config = OAuthConfig::new(SERVER, ProtocolVersion::V2025_11_25)?;
    Ok(OAuthAuthorizer::new(config, environment, NoSecrets))
}

fn challenge() -> BearerChallenge {
    BearerChallenge {
        resource_metadata: Some(PRM_URL.to_owned()),
        ..BearerChallenge::default()
    }
}

#[tokio::test]
async fn metadata_for_another_resource_is_refused() -> Result<(), url::ParseError> {
    let flow = authorizer(documents("https://elsewhere.example/mcp", ""))?;
    assert_matches!(
        flow.authorize(&challenge()).await,
        Err(AuthorizeError::ResourceMismatch { .. })
    );
    Ok(())
}

#[tokio::test]
async fn a_server_without_pkce_is_refused() -> Result<(), url::ParseError> {
    let flow = authorizer(documents(SERVER, ""))?;
    assert_matches!(flow.authorize(&challenge()).await, Err(AuthorizeError::NoPkce));
    Ok(())
}

#[tokio::test]
async fn a_server_offering_only_plain_is_refused() -> Result<(), url::ParseError> {
    let flow = authorizer(documents(SERVER, r#","code_challenge_methods_supported":["plain"]"#))?;
    assert_matches!(flow.authorize(&challenge()).await, Err(AuthorizeError::NoPkce));
    Ok(())
}

#[tokio::test]
async fn credentials_for_another_issuer_are_not_presented() -> Result<(), url::ParseError> {
    let config = OAuthConfig::new(SERVER, ProtocolVersion::V2025_11_25)?
        .with_pre_registered(PreRegistered::new("id", Some("secret")).issued_by("https://old.example"));
    let flow = OAuthAuthorizer::new(
        config,
        documents(SERVER, r#","code_challenge_methods_supported":["S256"]"#),
        NoSecrets,
    );
    assert_matches!(
        flow.authorize(&challenge()).await,
        Err(AuthorizeError::CredentialsForAnotherIssuer { .. })
    );
    Ok(())
}

#[tokio::test]
async fn the_client_stops_asking_after_three_refusals() -> Result<(), url::ParseError> {
    let flow = authorizer(documents("https://elsewhere.example/mcp", ""))?;
    for _ in 0..3 {
        let outcome = flow.challenged(401, None).await;
        assert!(outcome.is_err());
    }
    let fourth = flow.challenged(401, None).await.err().map(|error| error.0);
    assert!(
        fourth.is_some_and(|text| text.contains("3 authorization attempts")),
        "the fourth refusal should name the limit"
    );
    Ok(())
}

#[tokio::test]
async fn an_accepted_request_resets_the_count() -> Result<(), url::ParseError> {
    let flow = authorizer(documents("https://elsewhere.example/mcp", ""))?;
    for _ in 0..3 {
        let _ = flow.challenged(401, None).await;
    }
    flow.accepted();
    let outcome = flow.challenged(401, None).await.err().map(|error| error.0);
    assert!(outcome.is_some_and(|text| !text.contains("authorization attempts")));
    Ok(())
}

mod refresh {
    use core::future::Future;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::application::oauth::authorizer::Refreshable;
    use crate::domain::oauth::issued_token::IssuedToken;
    use crate::domain::oauth::registration::{ClientId, RegisteredClient};
    use crate::domain::oauth::resource::ResourceParameters;
    use crate::application::oauth::redeem::RefreshRequest;
    use crate::domain::mcp::resource_metadata::ResourceUri;

    /// Answers a refresh with a fixed body and counts the asks.
    struct Scripted {
        body: &'static str,
        asked: Arc<AtomicUsize>,
    }

    impl TokenEndpoint for Scripted {
        fn redeem(
            &self,
            _request: TokenRequest<'_>,
        ) -> impl Future<Output = Result<IssuedToken, TokenEndpointError>> + Send {
            core::future::ready(Err(TokenEndpointError::Unavailable("not here".to_owned())))
        }

        fn refresh(
            &self,
            request: RefreshRequest<'_>,
        ) -> impl Future<Output = Result<IssuedToken, TokenEndpointError>> + Send {
            self.asked.fetch_add(1, Ordering::SeqCst);
            assert_eq!(request.refresh_token(), "old-refresh");
            core::future::ready(
                IssuedToken::parse(self.body.as_bytes()).map_err(TokenEndpointError::Refused),
            )
        }
    }

    struct Env {
        body: &'static str,
        asked: Arc<AtomicUsize>,
    }

    impl OAuthEnvironment for Env {
        type Token = Scripted;

        fn token_endpoint(&self, _url: &str) -> Scripted {
            Scripted {
                body: self.body,
                asked: Arc::clone(&self.asked),
            }
        }

        fn get(&self, _url: &str) -> impl Future<Output = Result<Fetched, EnvironmentError>> + Send {
            core::future::ready(Ok(Fetched {
                status: 404,
                body: Vec::new(),
            }))
        }

        fn post_json(
            &self,
            _url: &str,
            _body: &str,
        ) -> impl Future<Output = Result<Fetched, EnvironmentError>> + Send {
            core::future::ready(Err(EnvironmentError::Unavailable("no".to_owned())))
        }

        fn authorize(
            &self,
            _url: &str,
        ) -> impl Future<Output = Result<AuthorizationCallback, EnvironmentError>> + Send {
            core::future::ready(Err(EnvironmentError::Declined("no one is there".to_owned())))
        }
    }

    type Flow = std::sync::Arc<OAuthAuthorizer<Env, NoSecrets>>;

    /// An authorizer holding a token and a refresh token, as after a login.
    fn holding(body: &'static str) -> Result<(Flow, Arc<AtomicUsize>), Box<dyn std::error::Error>> {
        let asked = Arc::new(AtomicUsize::new(0));
        let config = OAuthConfig::new(SERVER, ProtocolVersion::V2025_11_25)?;
        let flow = OAuthAuthorizer::new(
            config,
            Env {
                body,
                asked: Arc::clone(&asked),
            },
            NoSecrets,
        );
        let client = RegisteredClient::dynamic(ISSUER, ClientId::opaque("id"), None)?;
        flow.record(
            "old-access".to_owned(),
            Some("read".to_owned()),
            Some(Refreshable {
                token: "old-refresh".to_owned(),
                token_endpoint: format!("{ISSUER}/token"),
                client,
                resource: ResourceParameters::single(&ResourceUri::new(SERVER)?),
            }),
        );
        Ok((flow, asked))
    }

    const EXPIRED: Option<&str> = Some(r#"Bearer error="invalid_token""#);

    #[tokio::test]
    async fn an_expired_token_is_refreshed_without_asking_the_user() -> Result<(), Box<dyn std::error::Error>> {
        let (flow, asked) = holding(
            r#"{"access_token":"new-access","token_type":"Bearer","refresh_token":"old-refresh"}"#,
        )?;
        flow.challenged(401, EXPIRED).await?;
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        assert_eq!(flow.bearer().as_deref(), Some("new-access"));
        Ok(())
    }

    #[tokio::test]
    async fn a_refused_refresh_token_is_forgotten_and_the_flow_runs() -> Result<(), Box<dyn std::error::Error>> {
        let (flow, asked) = holding(r#"{"error":"invalid_grant"}"#)?;
        // Falls through to the full flow, which cannot discover anything here.
        assert!(flow.challenged(401, EXPIRED).await.is_err());
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        assert!(flow.state().refresh.is_none());
        // And is not offered again.
        let _ = flow.challenged(401, EXPIRED).await;
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[tokio::test]
    async fn a_challenge_for_more_scope_skips_the_refresh() -> Result<(), Box<dyn std::error::Error>> {
        let (flow, asked) = holding(
            r#"{"access_token":"new-access","token_type":"Bearer"}"#,
        )?;
        let wider = Some(r#"Bearer error="insufficient_scope", scope="write""#);
        let _ = flow.challenged(403, wider).await;
        assert_eq!(asked.load(Ordering::SeqCst), 0, "more scope needs the user, not a refresh");
        Ok(())
    }
}
