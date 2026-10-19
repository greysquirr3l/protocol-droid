//! The authorization flow's outside world, over `reqwest`.
//!
//! [`HttpOAuthEnvironment`] does the three HTTP things the flow asks —
//! `GET` a document, `POST` registration JSON, redeem a code — and delegates
//! the one thing that is not HTTP, sending the user somewhere, to a
//! [`UserAgent`]. A deployment with a browser supplies its own agent;
//! [`FollowsRedirect`] is the headless one, for an authorization server that
//! approves without asking.

use core::future::Future;
use core::pin::Pin;
use std::sync::Arc;

use url::Url;

use crate::adapters::secondary::oauth::http_body::read_capped;
use crate::adapters::secondary::oauth::redeem::HttpTokenEndpoint;
use crate::application::oauth::callback::AuthorizationCallback;
use crate::application::oauth::environment::{EnvironmentError, Fetched, OAuthEnvironment};

/// Sends the user to an authorization URL and reports where they came back.
pub trait UserAgent: Send + Sync {
    /// Takes the user through `authorization_url` and returns the callback.
    ///
    /// # Errors
    ///
    /// [`EnvironmentError::Declined`] when no callback arrived.
    fn authorize<'a>(
        &'a self,
        authorization_url: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<AuthorizationCallback, EnvironmentError>> + Send + 'a>>;
}

/// A user agent for an authorization server that approves without asking.
///
/// `GET`s the authorization URL without following redirects and reads the
/// callback out of the `Location` it answers with. For tests, CI, and servers
/// that authenticate the client some other way; it is not a way to authorize a
/// real user, who has to be shown a consent screen.
#[derive(Debug, Clone)]
pub struct FollowsRedirect {
    http: reqwest::Client,
}

impl FollowsRedirect {
    /// A headless agent.
    ///
    /// # Errors
    ///
    /// The `reqwest` builder's failure, as a string.
    pub fn new() -> Result<Self, String> {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map(|http| Self { http })
            .map_err(|error| error.to_string())
    }
}

impl UserAgent for FollowsRedirect {
    fn authorize<'a>(
        &'a self,
        authorization_url: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<AuthorizationCallback, EnvironmentError>> + Send + 'a>>
    {
        Box::pin(async move {
            let response = self
                .http
                .get(authorization_url)
                .send()
                .await
                .map_err(|error| EnvironmentError::Declined(error.to_string()))?;
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| {
                    EnvironmentError::Declined(format!(
                        "the authorization endpoint answered {} with no Location",
                        response.status().as_u16()
                    ))
                })?;
            callback_from(location)
        })
    }
}

/// Reads an authorization response out of the URL the user was sent back to.
///
/// # Errors
///
/// [`EnvironmentError::Declined`] when `location` is not a URL or carries
/// neither a code nor an error.
pub fn callback_from(location: &str) -> Result<AuthorizationCallback, EnvironmentError> {
    let url =
        Url::parse(location).map_err(|error| EnvironmentError::Declined(error.to_string()))?;
    let mut code = None;
    let mut error = None;
    let mut state = String::new();
    let mut issuer: Option<Arc<str>> = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            "state" => state = value.into_owned(),
            "iss" => issuer = Some(Arc::from(value.as_ref())),
            _ => {}
        }
    }
    let callback = match (code, error) {
        (Some(code), _) => AuthorizationCallback::granted(code, state),
        (None, Some(error)) => AuthorizationCallback::refused(error, state),
        (None, None) => {
            return Err(EnvironmentError::Declined(
                "the redirect carried neither a code nor an error".to_owned(),
            ));
        }
    };
    Ok(callback.with_issuer(issuer))
}

/// [`OAuthEnvironment`] over HTTP, with a pluggable user agent.
#[derive(Clone)]
pub struct HttpOAuthEnvironment {
    http: reqwest::Client,
    agent: Arc<dyn UserAgent>,
}

impl core::fmt::Debug for HttpOAuthEnvironment {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("HttpOAuthEnvironment")
            .finish_non_exhaustive()
    }
}

impl HttpOAuthEnvironment {
    /// An environment that sends users through `agent`.
    ///
    /// # Errors
    ///
    /// The `reqwest` builder's failure, as a string.
    pub fn new(agent: Arc<dyn UserAgent>) -> Result<Self, String> {
        // Redirects are not followed: a discovery document that redirects
        // somewhere else is not the document the protocol named, and an
        // authorization server's redirect to the callback is the agent's to
        // read, not the HTTP client's to chase.
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self { http, agent })
    }
}

async fn read(response: reqwest::Response) -> Result<Fetched, EnvironmentError> {
    let status = response.status().as_u16();
    let body = read_capped(response)
        .await
        .map_err(EnvironmentError::Unavailable)?;
    Ok(Fetched { status, body })
}

impl OAuthEnvironment for HttpOAuthEnvironment {
    type Token = HttpTokenEndpoint;

    fn token_endpoint(&self, url: &str) -> HttpTokenEndpoint {
        HttpTokenEndpoint::new(url).with_http_client(self.http.clone())
    }

    async fn get(&self, url: &str) -> Result<Fetched, EnvironmentError> {
        let response = self
            .http
            .get(url)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|error| EnvironmentError::Unavailable(error.to_string()))?;
        read(response).await
    }

    async fn post_json(&self, url: &str, body: &str) -> Result<Fetched, EnvironmentError> {
        let response = self
            .http
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "application/json")
            .body(body.to_owned())
            .send()
            .await
            .map_err(|error| EnvironmentError::Unavailable(error.to_string()))?;
        read(response).await
    }

    async fn authorize(
        &self,
        authorization_url: &str,
    ) -> Result<AuthorizationCallback, EnvironmentError> {
        self.agent.authorize(authorization_url).await
    }
}

#[cfg(test)]
mod tests {
    use super::callback_from;

    #[test]
    fn a_grant_carries_its_code_state_and_issuer() -> Result<(), Box<dyn std::error::Error>> {
        let callback = callback_from(
            "http://localhost:3000/callback?code=abc&state=xyz&iss=https%3A%2F%2Fauth.example.com",
        )?;
        assert_eq!(callback.code(), Some("abc"));
        assert_eq!(callback.state(), Some("xyz"));
        assert_eq!(callback.issuer(), Some("https://auth.example.com"));
        Ok(())
    }

    #[test]
    fn a_refusal_carries_its_error() -> Result<(), Box<dyn std::error::Error>> {
        let callback = callback_from("http://localhost:3000/callback?error=access_denied&state=s")?;
        assert_eq!(callback.error(), Some("access_denied"));
        Ok(())
    }

    #[test]
    fn a_redirect_with_neither_is_declined() {
        assert!(callback_from("http://localhost:3000/callback").is_err());
    }
}
