//! How a client authenticates to an HTTP server.
//!
//! The transports ask this port for a token and tell it when the server
//! refuses one. They know nothing of OAuth: an authorizer that runs the whole
//! flow implements it (`application::oauth::authorizer`), and so does a
//! deployment that holds a static key.

use core::future::Future;
use core::pin::Pin;

/// A server refused authentication and the client could not recover.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct BearerError(pub String);

/// A token to send, and a way to get another.
pub trait BearerAuth: Send + Sync {
    /// The token to send in `Authorization: Bearer …`, if there is one.
    fn bearer(&self) -> Option<String>;

    /// The server answered `status` (`401` or `403`) with `www_authenticate`.
    ///
    /// `Ok` means a new token is in place and the request should be retried;
    /// `Err` means it should not be.
    fn challenged<'a>(
        &'a self,
        status: u16,
        www_authenticate: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<(), BearerError>> + Send + 'a>>;

    /// The last request was accepted, so the count of refusals starts over.
    fn accepted(&self);
}
