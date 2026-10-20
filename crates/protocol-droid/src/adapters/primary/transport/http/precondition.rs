//! The two checks both HTTP transports make before they look at a message.
//!
//! Extracted from `router.rs` rather than shared from it, because `mod router`
//! is private and the two transports are siblings rather than parent and child:
//! a `2025-06-18` request and a `2024-11-05` request have nothing in common
//! about their body, their session, or their stream, and everything in common
//! about whether the caller is allowed to make the request at all. That shared
//! part is this module, and it is small on purpose.
//!
//! The module is public and not `pub(crate)` because that is the only spelling
//! both `unreachable_pub` and `redundant_pub_crate` accept: a crate-internal
//! module inside a public one makes a `pub(crate)` item redundant, and a `pub`
//! item inside it unreachable. Two lints, one answer, and the answer is that a
//! second transport mounting this machinery is a supported thing to do — the
//! same reason [`super::with_security`] is public.

use std::pin::Pin;

use axum::body::{Body, Bytes};
use axum::http::HeaderMap;
use futures_core::Stream;

use crate::adapters::primary::transport::http::{AuthPolicy, Authenticator, Credentials, Refusal};
use crate::domain::registry::AuthContext;

/// Runs the authenticator and the scope policy, in that order.
///
/// Shared with the `2024-11-05` transport, which authenticates on the same
/// terms and in the same order. A second implementation would be a second place
/// to get the ordering wrong, and the ordering is a security property.
///
/// Awaited, because establishing authenticity is network I/O — see
/// [`Authenticator`].
///
/// # Errors
///
/// The first [`Refusal`] the policy returns, if any. The authenticator itself
/// cannot refuse — it produces a context and lets the policy decide — so there
/// is no oracle here for a caller to probe.
pub async fn authenticate(
    authenticator: &dyn Authenticator,
    policy: &AuthPolicy,
    headers: &HeaderMap,
) -> Result<AuthContext, Refusal> {
    let context = authenticator
        .authenticate(&Credentials::from_headers(headers))
        .await;
    policy.refuse(&context).map_or(Ok(context), Err)
}

/// A request body, bounded by `limit`.
///
/// # Errors
///
/// [`Refusal::BodyTooLarge`] once the accumulated chunks would pass `limit`, or
/// [`Refusal::Handler`] if the body itself fails mid-stream. The check is on
/// the running total rather than on a final length, so a body that is too long
/// is refused before the rest of it is read.
///
/// Shared with the `2024-11-05` transport, which bounds a request body by the
/// same configured ceiling. The ceiling is the caller's to convert: both
/// transports read it out of the same [`crate::adapters::primary::transport::http::ServerConfig`]
/// and refuse every body when it does not fit a `usize`, and a limit that
/// silently wraps is worse than no limit.
pub async fn read_body(body: Body, limit: usize) -> Result<Bytes, Refusal> {
    let mut chunks = body.into_data_stream();
    let mut collected: Vec<u8> = Vec::new();
    while let Some(chunk) = next_item(&mut chunks).await {
        let chunk = chunk.map_err(|error| {
            Refusal::Handler(format!("the request body could not be read: {error}"))
        })?;
        if collected.len().saturating_add(chunk.len()) > limit {
            return Err(Refusal::BodyTooLarge);
        }
        collected.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(collected))
}

/// The next item from `stream`, waiting for as long as it takes.
///
/// Not an implementation detail of [`read_body`] despite living beside it:
/// this is the one place in the crate that spells out polling a stream, and a
/// second spelling would be a second set of wake-up semantics.
///
/// `Stream::next` lives on `futures_util::StreamExt`, and this crate depends
/// on `futures-core` rather than the whole of `futures`. Polling through
/// `poll_fn` is the same operation written out, and it is the only place in the
/// adapter that has to spell it.
async fn next_item<S>(stream: &mut S) -> Option<S::Item>
where
    S: Stream + Unpin,
{
    std::future::poll_fn(|context| Pin::new(&mut *stream).poll_next(context)).await
}
