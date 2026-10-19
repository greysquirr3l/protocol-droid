//! The convenience façade for serving an A2A agent.
//!
//! A consumer who wants one router that serves the
//! well-known card, the JSON-RPC endpoint, and the
//! streaming SSE endpoint on one path prefix calls
//! [`serve_a2a_agent`]. The function is a façade over
//! [`serve_well_known`],
//! [`serve_jsonrpc`],
//! and [`serve_streaming`];
//! the three layers are still public for a consumer
//! who wants to compose them with extra routes or
//! different paths.
//!
//! # Why a façade
//!
//! The three routes an A2A server serves are a
//! shape, not a free composition. A consumer who
//! wires the well-known path manually has to know
//! the constant, the path the spec names for the
//! card (`/.well-known/agent-card.json`); a consumer
//! who wires the streaming path manually has to
//! know it is `"{jsonrpc_path}/stream"`, not a
//! separate configurable path. The two paths are
//! derived from the JSON-RPC path because that is
//! what the example server uses, and it is the
//! shape a peer reading the agent card would
//! expect — the card's `supportedInterfaces[0].url`
//! is the JSON-RPC path, and the streaming path is
//! a peer convention derived from it. Encoding the
//! shape in one function stops a consumer from
//! getting the streaming path wrong.
//!
//! # What the façade does not include
//!
//! A health probe (`/health`) is not part of the
//! A2A spec; the example adds it as a container
//! orchestration convention. A consumer who wants
//! one is expected to `.merge(health_router())` on
//! top of the façade's router. The auth scaffolding
//! (`A2aError::AuthenticationRequired` and friends)
//! is the consumer's choice too; the façade does
//! not mount an auth middleware because the spec
//! leaves the shape of authentication to the
//! deployment.
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! use axum::Router;
//! use protocol_droid::adapters::primary::a2a::serve_agent::serve_a2a_agent;
//! use protocol_droid::adapters::primary::a2a::streaming_serve::A2aStreamingRequestHandler;
//! use protocol_droid::application::a2a::{A2aError, A2aRequestContext, A2aRequestHandler};
//! use protocol_droid::domain::a2a::agent_card::AgentCardBuilder;
//!
//! struct EchoAgent;
//!
//! impl A2aRequestHandler for EchoAgent {
//!     fn dispatch(
//!         &self,
//!         method: protocol_droid::adapters::secondary::a2a::A2aMethod,
//!         _params: Option<serde_json::Value>,
//!         _context: &A2aRequestContext,
//!     ) -> std::pin::Pin<
//!         Box<dyn futures_core::Future<Output = Result<serde_json::Value, A2aError>> + Send + '_>,
//!     > {
//!         Box::pin(async move {
//!             Err(A2aError::MethodNotImplemented {
//!                 method,
//!             })
//!         })
//!     }
//! }
//!
//! impl A2aStreamingRequestHandler for EchoAgent {
//!     fn dispatch_stream(
//!         &self,
//!         method: protocol_droid::adapters::secondary::a2a::A2aMethod,
//!         _params: Option<serde_json::Value>,
//!         _context: &A2aRequestContext,
//!     ) -> std::pin::Pin<
//!         Box<dyn futures_core::Stream<Item = Result<protocol_droid::domain::a2a::streaming_events::StreamingEvent, A2aError>> + Send>,
//!     > {
//!         Box::pin(futures_util::stream::empty())
//!     }
//! }
//!
//! # async fn build() -> Router {
//! let agent: Arc<dyn A2aRequestHandler> = Arc::new(EchoAgent);
//! let streaming: Arc<dyn A2aStreamingRequestHandler> = Arc::new(EchoAgent);
//! let card = AgentCardBuilder::new()
//!     .with_name("echo")
//!     .with_description("An A2A echo agent.")
//!     .with_version("0.1.0")
//!     .build()
//!     .expect("builder inputs are valid");
//! let app: Router = serve_a2a_agent("/a2a", agent, streaming, Arc::new(card));
//! app
//! # }
//! ```

use std::sync::Arc;

use axum::Router;

use crate::adapters::primary::a2a::jsonrpc_serve::serve_jsonrpc;
use crate::adapters::primary::a2a::streaming_serve::A2aStreamingRequestHandler;
use crate::adapters::primary::a2a::streaming_serve::serve_streaming;
use crate::adapters::primary::a2a::well_known_serve::serve_well_known;
use crate::application::a2a::A2aRequestHandler;
use crate::domain::a2a::agent_card::AgentCard;
use crate::domain::a2a::version::A2aVersion;

/// Returns an axum [`Router`] that serves a single A2A
/// agent on three routes:
///
/// - `/.well-known/agent-card.json` — the well-known
///   card discovery endpoint (fixed path per A2A
///   v1.0 §8).
/// - `{jsonrpc_path}` — the JSON-RPC endpoint for
///   the request/response methods (`SendMessage`,
///   `GetTask`, `ListTasks`, `CancelTask`, the four
///   push-notification config methods, and
///   `GetExtendedAgentCard`).
/// - `{jsonrpc_path}/stream` — the streaming SSE
///   endpoint for the two streaming methods
///   (`SendStreamingMessage` and `SubscribeToTask`).
///
/// The two handlers are independent so a consumer
/// who needs different types for the JSON-RPC
/// endpoint and the streaming endpoint can pass
/// different `Arc`s; the common case is one struct
/// that implements both traits, and the example
/// shows that shape. A consumer who wants the
/// layers directly can call the three underlying
/// serving adapters; they are still public and
/// they still compose the same way.
///
/// The `card` is the agent card the well-known
/// endpoint serves. The card is the consumer's
/// value object: the same struct that the
/// `AgentCardBuilder` builds, and the same struct
/// `GetExtendedAgentCard` returns. A card whose
/// `supportedInterfaces[0].url` does not point at
/// `jsonrpc_path` is a card a peer cannot reach;
/// the consumer is responsible for keeping the
/// two in sync. The example's `build_card(addr)`
/// derives the interface URL from the bind
/// address, which is the only way to keep them in
/// sync without manual coordination.
pub fn serve_a2a_agent(
    jsonrpc_path: &str,
    request_handler: Arc<dyn A2aRequestHandler>,
    streaming_handler: Arc<dyn A2aStreamingRequestHandler>,
    card: Arc<AgentCard>,
) -> Router {
    let version = A2aVersion::V1_0;
    let stream_path = format!("{jsonrpc_path}/stream");
    serve_well_known(card)
        .merge(serve_jsonrpc(jsonrpc_path, request_handler, version))
        .merge(serve_streaming(&stream_path, streaming_handler, version))
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "tests assert on fallible calls; .expect() is the standard test pattern — see tests/a2a_serving_wire.rs"
)]
mod tests {
    use super::*;
    use crate::application::a2a::A2aRequestContext;
    use crate::domain::a2a::agent_card::AgentCardBuilder;
    use crate::domain::a2a::streaming_events::StreamingEvent;
    use futures_core::Stream;
    use futures_util::stream;

    struct NoopAgent;

    impl A2aRequestHandler for NoopAgent {
        fn dispatch(
            &self,
            method: crate::adapters::secondary::a2a::A2aMethod,
            _params: Option<serde_json::Value>,
            _context: &A2aRequestContext,
        ) -> std::pin::Pin<
            Box<
                dyn futures_core::Future<
                        Output = Result<serde_json::Value, crate::application::a2a::A2aError>,
                    > + Send
                    + '_,
            >,
        > {
            Box::pin(async move {
                Err(crate::application::a2a::A2aError::MethodNotImplemented { method })
            })
        }
    }

    impl A2aStreamingRequestHandler for NoopAgent {
        fn dispatch_stream(
            &self,
            _method: crate::adapters::secondary::a2a::A2aMethod,
            _params: Option<serde_json::Value>,
            _context: &A2aRequestContext,
        ) -> std::pin::Pin<
            Box<
                dyn Stream<Item = Result<StreamingEvent, crate::application::a2a::A2aError>> + Send,
            >,
        > {
            Box::pin(stream::empty())
        }
    }

    fn minimal_card() -> Arc<AgentCard> {
        Arc::new(
            AgentCardBuilder::new()
                .with_name("test")
                .with_description("test")
                .with_version("0.1.0")
                .with_interface(crate::domain::a2a::agent_card::AgentInterface {
                    url: url::Url::parse("http://127.0.0.1:0/a2a").expect("static URL"),
                    protocol_binding: crate::domain::a2a::agent_card::JSONRPC.clone(),
                    protocol_version: A2aVersion::V1_0,
                    tenant: None,
                })
                .with_input_mode("text/plain")
                .with_output_mode("text/plain")
                .with_skill(crate::domain::a2a::agent_card::AgentSkill {
                    id: "test".into(),
                    name: "Test".into(),
                    description: "Test skill.".into(),
                    tags: Vec::new(),
                })
                .build()
                .expect("valid card"),
        )
    }

    /// The façade returns a router. The integration
    /// tests in `tests/a2a_serving_wire.rs` exercise
    /// the routes; this test pins that the function
    /// is reachable from the lib's own test target
    /// (the `dead_code_pub_in_binary` lint fires for
    /// unused `pub` items even in libraries when
    /// nothing in the test target uses them, and
    /// having a test here keeps the façade in the
    /// lib's own test set).
    #[test]
    fn facade_returns_router() {
        let request: Arc<dyn A2aRequestHandler> = Arc::new(NoopAgent);
        let streaming: Arc<dyn A2aStreamingRequestHandler> = Arc::new(NoopAgent);
        let _router: Router = serve_a2a_agent("/a2a", request, streaming, minimal_card());
    }
}
