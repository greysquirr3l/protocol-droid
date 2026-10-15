//! A minimal A2A server that exercises the
//! [`serve_a2a_agent`](protocol_droid::adapters::primary::a2a::serve_agent::serve_a2a_agent)
//! façade.
//!
//! The full
//! [`a2a_server`](protocol_droid::adapters::primary::a2a::jsonrpc_serve::serve_jsonrpc)
//! example shows the layer-by-layer assembly of
//! `serve_well_known`, `serve_jsonrpc`, and
//! `serve_streaming`. This one shows what a
//! consumer building a real product would
//! reach for: one function call that mounts
//! the same three routes from a single
//! `AgentCard` and a single `Echo` handler.
//!
//! # What this is
//!
//! - **One struct** that implements both
//!   [`A2aRequestHandler`] and
//!   [`A2aStreamingRequestHandler`]. The
//!   trait impls are 3 lines each: a
//!   `MethodNotImplemented` error for the
//!   ten methods the example does not
//!   serve, and an empty stream for the
//!   two streaming methods. A real
//!   consumer would fill these in.
//! - **One card** built with the
//!   `AgentCardBuilder`, the same builder
//!   the full example uses. The card is
//!   the same shape every A2A peer reads.
//! - **One call** to
//!   [`serve_a2a_agent`](protocol_droid::adapters::primary::a2a::serve_agent::serve_a2a_agent),
//!   which mounts the well-known path,
//!   the JSON-RPC path, and the streaming
//!   SSE path on one router.
//!
//! # What this is not
//!
//! The handler here is a stub. A real
//! consumer would implement at least
//! `SendMessage` and `GetTask` (the two
//! methods the A2A spec names as
//! required). The full
//! [`a2a_server.rs`](https://github.com/greysquirr3l/protocol-droid/blob/main/crates/protocol-droid/examples/a2a_server.rs)
//! example shows a complete handler with
//! all 11 methods, an in-memory task
//! store, a webhook delivery port with
//! the SSRF denylist, and the env-driven
//! bind address. This one is the
//! first-step reader; the full one is
//! the production reference.
//!
//! # Running
//!
//! ```text
//! # terminal 1
//! cargo run -p protocol-droid --example a2a_minimal_server --features a2a,http
//!
//! # terminal 2
//! curl http://127.0.0.1:8080/.well-known/agent-card.json
//! ```
//!
//! Override the bind address with `A2A_ADDR=0.0.0.0:9000`.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use futures_core::Stream;
use protocol_droid::adapters::primary::a2a::serve_agent::serve_a2a_agent;
use protocol_droid::adapters::primary::a2a::streaming_serve::A2aStreamingRequestHandler;
use protocol_droid::adapters::secondary::a2a::A2aMethod;
use protocol_droid::application::a2a::A2aRequestHandler;
use protocol_droid::application::a2a::{A2aError, A2aRequestContext};
use protocol_droid::domain::a2a::agent_card::{
    AgentCapabilities, AgentCard, AgentCardBuilder, AgentInterface, AgentSkill, JSONRPC,
};
use protocol_droid::domain::a2a::streaming_events::StreamingEvent;
use protocol_droid::domain::a2a::version::A2aVersion;
use serde_json::Value;

/// The handler. One struct, two trait impls,
/// zero state. The full example's `EchoAgent`
/// is the same shape with a task store, a
/// delivery port, and a card; this one is
/// what a first-step consumer would write.
struct Echo;

impl A2aRequestHandler for Echo {
    fn dispatch(
        &self,
        method: A2aMethod,
        _params: Option<Value>,
        _context: &A2aRequestContext,
    ) -> std::pin::Pin<Box<dyn futures_core::Future<Output = Result<Value, A2aError>> + Send + '_>>
    {
        Box::pin(async move { Err(A2aError::MethodNotImplemented { method }) })
    }
}

impl A2aStreamingRequestHandler for Echo {
    fn dispatch_stream(
        &self,
        _method: A2aMethod,
        _params: Option<Value>,
        _context: &A2aRequestContext,
    ) -> std::pin::Pin<Box<dyn Stream<Item = Result<StreamingEvent, A2aError>> + Send>> {
        Box::pin(futures_util::stream::empty())
    }
}

/// Builds the card the well-known endpoint
/// serves. The interface URL is derived from
/// the bind address, which is the only way to
/// keep the card's `supportedInterfaces[0].url`
/// in sync with where the JSON-RPC route
/// actually listens. The full example shows
/// the same pattern; this one inlines it.
fn build_card(addr: SocketAddr) -> Result<Arc<AgentCard>, Box<dyn std::error::Error>> {
    Ok(Arc::new(
        AgentCardBuilder::new()
            .with_name("echo")
            .with_description("A minimal A2A echo agent.")
            .with_version("0.1.0")
            .with_interface(AgentInterface {
                url: url::Url::parse(&format!("http://{addr}/a2a"))?,
                protocol_binding: JSONRPC.clone(),
                protocol_version: A2aVersion::V1_0,
                tenant: None,
            })
            .with_capabilities(AgentCapabilities::default())
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .with_skill(AgentSkill {
                id: "echo".into(),
                name: "Echo".into(),
                description: "Echoes a message back.".into(),
                tags: Vec::new(),
            })
            .build()?,
    ))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = bind_addr()?;
    let card = build_card(addr)?;
    let app: Router = serve_a2a_agent("/a2a", Arc::new(Echo), Arc::new(Echo), card);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(serve(app, addr))
}

async fn serve(app: Router, addr: SocketAddr) -> Result<(), Box<dyn std::error::Error>> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!(
        "a2a_minimal_server: listening on http://{}",
        listener.local_addr()?
    );
    eprintln!("  well-known:  /.well-known/agent-card.json");
    eprintln!("  json-rpc:    POST /a2a");
    eprintln!("  streaming:   POST /a2a/stream");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Reads the bind address from the `A2A_ADDR`
/// env var, or falls back to `127.0.0.1:8080`.
/// A malformed `A2A_ADDR` is reported on
/// stderr and the default is used.
fn bind_addr() -> Result<SocketAddr, Box<dyn std::error::Error>> {
    let raw = std::env::var("A2A_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_owned());
    match raw.parse() {
        Ok(addr) => Ok(addr),
        Err(error) => {
            eprintln!(
                "a2a_minimal_server: A2A_ADDR={raw:?} is not a valid socket address: {error}"
            );
            Ok("127.0.0.1:8080".parse()?)
        }
    }
}
