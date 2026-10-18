//! What the authorization flow needs from the outside world.
//!
//! The flow in [`authorizer`](super::authorizer) is decisions: which document to
//! read next, which client to present, which scope to ask for. Everything it
//! cannot decide by itself — fetching a URL, sending the user somewhere,
//! trading a code for a token — it asks of an [`OAuthEnvironment`], so the
//! flow runs unchanged against a test double, a headless agent, or a person
//! with a browser.
//!
//! The port belongs to its consumer, here, and returns this module's errors:
//! an adapter maps `reqwest` and the operating system's failures to them at
//! the boundary, and nothing above names a third-party type.

use core::future::Future;

use crate::application::oauth::callback::AuthorizationCallback;
use crate::application::oauth::redeem::TokenEndpoint;

/// A response read in full.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    /// The HTTP status.
    pub status: u16,
    /// The body, capped by the adapter.
    pub body: Vec<u8>,
}

impl Fetched {
    /// Whether the status is `2xx`.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        self.status >= 200 && self.status < 300
    }
}

/// Why the environment could not do what it was asked.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnvironmentError {
    /// The network, or the other side, failed.
    #[error("{0}")]
    Unavailable(String),
    /// The user, or whatever stands in for them, declined or could not be
    /// reached.
    #[error("the authorization was not completed: {0}")]
    Declined(String),
}

/// The outside world, as the authorization flow sees it.
pub trait OAuthEnvironment: Send + Sync + 'static {
    /// Where authorization codes are redeemed, bound to one endpoint.
    type Token: TokenEndpoint + Send + Sync;

    /// A token endpoint at `url`.
    fn token_endpoint(&self, url: &str) -> Self::Token;

    /// `GET`s `url` without following redirects.
    ///
    /// # Errors
    ///
    /// [`EnvironmentError::Unavailable`] when no response was read.
    fn get(&self, url: &str) -> impl Future<Output = Result<Fetched, EnvironmentError>> + Send;

    /// `POST`s `body` to `url` as `application/json`.
    ///
    /// # Errors
    ///
    /// [`EnvironmentError::Unavailable`] when no response was read.
    fn post_json(
        &self,
        url: &str,
        body: &str,
    ) -> impl Future<Output = Result<Fetched, EnvironmentError>> + Send;

    /// Sends the user to `authorization_url` and returns where they came back.
    ///
    /// A browser adapter opens the URL and waits for the loopback redirect; a
    /// headless one asks an authorization server that approves by itself and
    /// reads the `Location` it answers with.
    ///
    /// # Errors
    ///
    /// [`EnvironmentError::Declined`] when no callback arrived.
    fn authorize(
        &self,
        authorization_url: &str,
    ) -> impl Future<Output = Result<AuthorizationCallback, EnvironmentError>> + Send;
}
