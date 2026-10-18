//! The layers between a socket and a handler, and the order they run in.
//!
//! Everything here is adapter: it reads headers, consults state, and turns the
//! result into a status. The rules live in `domain::transport` and in
//! `transport::origin`; this module's only job is to make sure a request that
//! must be refused never reaches a handler.
//!
//! ## Order, and why it is this order
//!
//! ```text
//!   concurrency limit  — shed load before doing any work
//!   request timeout    — a shed request must not hold a slot forever
//!   origin check       — 403 before authentication, so a rebinding page is
//!                        refused without this server revealing anything about
//!                        the credential it sent
//!   rate limit         — the only check that must consult mutable state
//!   handler
//! ```
//!
//! Origin sits ahead of authentication on purpose. Refusing a foreign
//! `Origin` *after* authenticating would tell a page that is trying to rebind
//! whether the token it guessed was any good — which is precisely the oracle
//! DNS rebinding is after. Refusing first answers the same way whether the
//! credential was valid, invalid, or absent.
//!
//! Every check before the rate limiter is free, and the rate limiter spends a
//! lock and a map lookup. A limiter that paid that to refuse a request an
//! earlier check would have refused is a limiter doing work for nothing, so it
//! goes last.
//!
//! ## What the rate limit keys on
//!
//! `AuthContext` would be the ideal subject and is not available here: axum
//! middleware runs *before* the handler, and the authenticator runs inside it.
//! So the key is what a pre-handler layer can actually see —
//!
//! 1. `Mcp-Session-Id`, for a stateful client, because a session is stable
//!    across requests and is the natural unit of "this caller".
//! 2. Otherwise a single shared `"anonymous"` allowance.
//!
//! (2) is a deliberate choice rather than a fallback of convenience: an
//! unauthenticated flood must not be able to mint itself a fresh allowance by
//! inventing a session id, and it cannot be given per-client isolation it has
//! not proved any right to. A caller that wants isolation establishes a
//! session, which is what sessions are for.
//!
//! ## What is deliberately absent
//!
//! No body limit. [`RouterState`] already refuses an oversized body with
//! [`Refusal::BodyTooLarge`] before reading it, and a second `DefaultBodyLimit`
//! here would be a second `413` path with its own idea of the ceiling — two
//! limits that agree today and will not after the first one is reconfigured.

use axum::extract::{Request, State};
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

use super::limits::RateLimiter;
use super::refusal::Refusal;
use super::router::RouterState;
use crate::domain::Verdict;

/// The key used when a request carries no session id.
pub(crate) const ANONYMOUS_SUBJECT: &str = "anonymous";

/// Refuses a request whose `Origin` is not one this server serves.
pub(crate) async fn enforce_origin<S, H>(
    State(state): State<Arc<RouterState<S, H>>>,
    request: Request,
    next: Next,
) -> Response
where
    S: Send + Sync + 'static,
    H: Send + Sync + 'static,
{
    let header = request
        .headers()
        .get(axum::http::header::ORIGIN)
        .and_then(|value| value.to_str().ok());
    if let Err(refusal) = state.origin().check(header) {
        return refusal.into_response();
    }
    next.run(request).await
}

/// Refuses a request whose `Host` is not one this server serves.
///
/// The pair with [`enforce_origin`], and the one that catches what the other
/// cannot: `Origin` is absent from every non-browser client, so a defence
/// built only on it answers "allowed" to exactly the requests a rebinding
/// attack is shaped to look like. `Host` is present on every HTTP/1.1 request
/// and is what a browser sets from the URL it dialled, so a rebound request
/// naming `evil.example` is refused here.
pub(crate) async fn enforce_host<S, H>(
    State(state): State<Arc<RouterState<S, H>>>,
    request: Request,
    next: Next,
) -> Response
where
    S: Send + Sync + 'static,
    H: Send + Sync + 'static,
{
    let header = request
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|value| value.to_str().ok());
    if let Err(refusal) = state.origin().check_host(header) {
        return refusal.into_response();
    }
    next.run(request).await
}

/// Counts the request against its subject's allowance, refusing with `429`.
///
/// The refusal carries `Retry-After` because a client told only "429" is a
/// client that retries immediately, which is the one behaviour a rate limit
/// cannot survive.
pub(crate) async fn enforce_rate_limit<S, H>(
    State(state): State<Arc<RouterState<S, H>>>,
    request: Request,
    next: Next,
) -> Response
where
    S: Send + Sync + 'static,
    H: Send + Sync + 'static,
{
    let limiter: &RateLimiter = state.rate_limiter();
    match limiter.check(subject_of(&request)) {
        Verdict::Allowed { remaining } => {
            let mut response = next.run(request).await;
            if let Ok(value) = HeaderValue::from_str(&remaining.get().to_string()) {
                response
                    .headers_mut()
                    .insert("x-ratelimit-remaining", value);
            }
            response
        }
        Verdict::Limited { retry_after } => Refusal::RateLimited { retry_after }.into_response(),
    }
}

/// The key this request is counted against.
///
/// Private to the module because the choice of key is part of the threat
/// model, not a detail: a caller who could pick their own key could pick a
/// fresh one per request, and the limit would measure nothing.
fn subject_of(request: &Request) -> &str {
    request
        .headers()
        .get(axum::http::header::HeaderName::from_static(
            "mcp-session-id",
        ))
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .unwrap_or(ANONYMOUS_SUBJECT)
}

/// Drops a request whose handler has not answered in time.
///
/// Written as `from_fn` rather than as `tower::timeout::TimeoutLayer` because
/// axum's `Router` has `Error = Infallible`, and `TimeoutLayer` requires its
/// inner service's error to absorb `BoxError`. The type gymnastics that gets
/// around that are worse than thirty lines of the obvious thing.
pub(crate) async fn enforce_deadline<S, H>(
    State(state): State<Arc<RouterState<S, H>>>,
    request: Request,
    next: Next,
) -> Response
where
    S: Send + Sync + 'static,
    H: Send + Sync + 'static,
{
    let deadline = state.config().request_timeout();
    tokio::time::timeout(deadline, next.run(request))
        .await
        .unwrap_or_else(|_| Refusal::Timeout.into_response())
}

/// Wraps `router` in the concurrency ceiling.
///
/// Applied as one stack so the order is stated once and cannot drift between
/// the transports that mount it. The body limit is deliberately not here, and
/// neither is the deadline — see the module docs and [`enforce_deadline`].
///
/// No type parameters: by the time this runs the router's state has already
/// been applied, so there is nothing here to infer `S` or `H` from.
pub fn apply_layers(router: axum::Router, max_concurrency: core::num::NonZeroU32) -> axum::Router {
    // `NonZeroU32` because a limit of zero is nonsense; `usize` because that is
    // what tower's layer counts in. The conversion cannot fail on any target
    // with a 32-bit `usize`, and saturating keeps it total on a 16-bit one
    // rather than panicking at start-up.
    let ceiling = usize::try_from(max_concurrency.get()).unwrap_or(usize::MAX);
    router.layer(tower::limit::ConcurrencyLimitLayer::new(ceiling.max(1)))
}
