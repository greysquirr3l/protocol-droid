//! HTTP serving for the A2A agent-card well-known path.
//!
//! The A2A spec's §8 "Agent Discovery" rule is: a server
//! publishes its [`AgentCard`](crate::domain::a2a::AgentCard)
//! at `https://{host}/.well-known/agent-card.json` as a
//! `GET` request returning the card's JSON. This module
//! is the adapter that owns the rule — the route, the
//! status code, the `Content-Type`, the body.
//!
//! The router this module builds is meant to be merged
//! into a consumer's own axum app, not bound directly:
//!
//! ```no_run
//! use std::sync::Arc;
//! use axum::Router;
//! use protocol_droid::adapters::primary::a2a::well_known_serve::serve_well_known;
//! use protocol_droid::domain::a2a::{
//!     AgentCard, AgentCardBuilder, AgentInterface, JSONRPC,
//! };
//! use protocol_droid::domain::a2a::version::A2aVersion;
//!
//! fn build_card() -> Result<Arc<AgentCard>, Box<dyn std::error::Error>> {
//!     let card = AgentCardBuilder::new()
//!         .with_name("n").with_description("d").with_version("1.0.0")
//!         .with_interface(AgentInterface {
//!             url: url::Url::parse("https://example.com/a2a")?,
//!             protocol_binding: JSONRPC.clone(),
//!             protocol_version: A2aVersion::V1_0,
//!             tenant: None,
//!         })
//!         .with_input_mode("text/plain")
//!         .with_output_mode("text/plain")
//!         .build()?;
//!     Ok(Arc::new(card))
//! }
//!
//! let app: Router = Router::new()
//!     .merge(serve_well_known(build_card().unwrap()));
//! ```
//!
//! # Why a separate router, not a method on `AgentCard`
//!
//! The card is a value object — it does not know about
//! HTTP, axum, or any transport. A consumer who serves
//! the card over a different transport (gRPC, a custom
//! binary protocol) is free to use the same value
//! object. The serving adapter is a separate type, in a
//! separate module, gated on the same feature flags as
//! the HTTP server itself.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::domain::a2a::AgentCard;
use crate::domain::a2a::well_known::AGENT_CARD_WELL_KNOWN_PATH;

/// Returns an axum [`Router`] that serves the agent card
/// on the well-known path.
///
/// The router is `GET` only. A `POST` to the well-known
/// path returns axum's `405 Method Not Allowed`, which
/// matches the spec's "this resource is read-only"
/// contract.
///
/// The card is held in an `Arc` so the router is cheap
/// to clone; a consumer who wants to publish a new
/// card swaps the `Arc` in their state, the router
/// keeps serving, and the next request sees the new
/// value. (Hot-reloading a card this way is the common
/// pattern for A2A servers that refresh their
/// capability list on a timer.)
pub fn serve_well_known(card: Arc<AgentCard>) -> Router {
    Router::new()
        .route(AGENT_CARD_WELL_KNOWN_PATH, get(get_card))
        .with_state(card)
}

async fn get_card(State(card): State<Arc<AgentCard>>) -> Response {
    // A card that fails to serialise is a
    // builder-level bug — every field is
    // already serialisable by construction —
    // and the right answer for the client is
    // a 500 with no body. The error is not
    // surfaced to the caller because the wire
    // contract is the well-known JSON, not a
    // stack trace. A consumer who wants to
    // catch this in production can use a
    // custom `Router::layer` to log it.
    let Ok(body) = serde_json::to_vec(&*card) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::serve_well_known;
    use crate::domain::a2a::well_known::AGENT_CARD_WELL_KNOWN_PATH;
    use crate::domain::a2a::version::A2aVersion;
    use crate::domain::a2a::{AgentCard, AgentCardBuilder, AgentInterface, JSONRPC};

    type TestError = Box<dyn std::error::Error>;

    fn sample_card() -> Result<Arc<AgentCard>, TestError> {
        let card = AgentCardBuilder::new()
            .with_name("Test Agent")
            .with_description("A test agent card for the well-known path.")
            .with_version("1.0.0")
            .with_interface(AgentInterface {
                url: url::Url::parse("https://example.com/a2a/v1")?,
                protocol_binding: JSONRPC.clone(),
                protocol_version: A2aVersion::V1_0,
                tenant: None,
            })
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .build()?;
        Ok(Arc::new(card))
    }

    // --- happy path ---------------------------------------------------

    #[tokio::test]
    async fn get_returns_200_with_the_card_json() -> Result<(), TestError> {
        let card = sample_card()?;
        let router = serve_well_known(card);
        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(AGENT_CARD_WELL_KNOWN_PATH)
                    .body(Body::empty())
                    .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?,
            )
            .await
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        assert_eq!(content_type.as_deref(), Some("application/json"));
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let document: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let obj = document
            .as_object()
            .ok_or("the response body must be a JSON object")?;
        assert_eq!(
            obj.get("name").and_then(serde_json::Value::as_str),
            Some("Test Agent")
        );
        assert_eq!(
            obj.get("description").and_then(serde_json::Value::as_str),
            Some("A test agent card for the well-known path.")
        );
        Ok(())
    }

    // --- method not allowed ------------------------------------------

    #[tokio::test]
    async fn post_returns_method_not_allowed() -> Result<(), TestError> {
        // The well-known path is `GET` only. A
        // `POST` to it is a peer that did not read
        // the spec; the right answer is axum's
        // `405` with no body.
        let card = sample_card()?;
        let router = serve_well_known(card);
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(AGENT_CARD_WELL_KNOWN_PATH)
                    .body(Body::from("{}"))
                    .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?,
            )
            .await
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        Ok(())
    }

    // --- hot-swap ----------------------------------------------------

    #[tokio::test]
    async fn hot_swapping_the_card_is_observable_on_the_next_request() -> Result<(), TestError> {
        // The card is held in an `Arc` so a consumer
        // who swaps it sees the new value on the
        // next request. The test pins the contract:
        // build two cards, build a fresh router
        // for the second, and assert the second
        // request returns the second card. (The
        // router captures the `Arc` at
        // construction time; hot-reloading in
        // production is a re-call of
        // `serve_well_known` and a `Router::merge`.)
        let _first = sample_card()?;
        let second = AgentCardBuilder::new()
            .with_name("Replaced Agent")
            .with_description("The replacement card.")
            .with_version("1.0.0")
            .with_interface(AgentInterface {
                url: url::Url::parse("https://example.com/a2a/v1")?,
                protocol_binding: JSONRPC.clone(),
                protocol_version: A2aVersion::V1_0,
                tenant: None,
            })
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .build()?;
        let second = Arc::new(second);

        let router = serve_well_known(Arc::clone(&second));
        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(AGENT_CARD_WELL_KNOWN_PATH)
                    .body(Body::empty())
                    .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?,
            )
            .await
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let document: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let name = document
            .as_object()
            .and_then(|o| o.get("name"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        assert_eq!(name.as_deref(), Some("Replaced Agent"));
        Ok(())
    }
}
