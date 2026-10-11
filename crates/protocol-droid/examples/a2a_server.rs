//! A complete A2A server in one runnable file.
//!
//! The smallest thing an external A2A peer can actually reach over
//! the wire: it serves the well-known `AgentCard` at
//! `/.well-known/agent-card.json` and the JSON-RPC 2.0 method table
//! at `/a2a`, on a configurable address (default `127.0.0.1:8080`).
//!
//! ```text
//! cargo run -p protocol-droid --example a2a_server --features a2a,http
//! ```
//!
//! A one-shot probe is `curl`:
//!
//! ```text
//! curl http://127.0.0.1:8080/.well-known/agent-card.json
//!
//! curl -X POST http://127.0.0.1:8080/a2a \
//!   -H "content-type: application/json" \
//!   -d '{"jsonrpc":"2.0","id":1,"method":"SendMessage",
//!        "params":{"message":{"role":"user","messageId":"m-1",
//!                             "parts":[{"kind":"text","text":"hello"}]}}}'
//! ```
//!
//! The well-known route is the A2A §8 discovery mechanism; the
//! JSON-RPC path is the method table the card names. A server that
//! only serves JSON-RPC is reachable, but not discoverable; a server
//! that only serves the card is not reachable at all.

#![allow(
    clippy::expect_used,
    reason = "examples assert on fallible calls with named failure messages; a panic in main is the right shape for an example"
)]

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use protocol_droid::adapters::primary::a2a::jsonrpc_serve::serve_jsonrpc;
use protocol_droid::adapters::primary::a2a::well_known_serve::serve_well_known;
use protocol_droid::adapters::secondary::a2a::A2aMethod;
use protocol_droid::application::a2a::{A2aError, A2aRequestContext, A2aRequestHandler};
use protocol_droid::domain::a2a::A2aVersion;
use protocol_droid::domain::a2a::agent_card::{
    AgentCapabilities, AgentCard, AgentCardBuilder, AgentInterface, AgentSkill, JSONRPC,
};
use protocol_droid::domain::a2a::message::{Message, MessageRole, Part, TextPart};
use protocol_droid::domain::a2a::send_message::SendMessageResponse;
use protocol_droid::domain::a2a::task::{Task, TaskState, TaskStatus};

/// The agent. The serving adapter decodes the envelope, looks the
/// method up in the table, and calls `dispatch` with the matched
/// method and the raw params.
struct EchoAgent;

impl A2aRequestHandler for EchoAgent {
    fn dispatch(
        &self,
        method: A2aMethod,
        params: Option<serde_json::Value>,
        _context: &A2aRequestContext,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<serde_json::Value, A2aError>> + Send + '_>,
    > {
        Box::pin(async move {
            match method {
                A2aMethod::GetTask => {
                    let task = Task::new("t-echo").with_status(TaskStatus {
                        state: TaskState::Completed,
                        message: None,
                        timestamp: None,
                    });
                    serde_json::to_value(task).map_err(|e| A2aError::InternalError {
                        message: format!("serialise task: {e}"),
                    })
                }
                A2aMethod::SendMessage => {
                    let _ = params;
                    let reply = Message::new(MessageRole::Agent).with_part(Part::Text(TextPart {
                        text: "echo".to_owned(),
                        metadata: std::collections::BTreeMap::new(),
                    }));
                    serde_json::to_value(SendMessageResponse::Message(reply)).map_err(|e| {
                        A2aError::InternalError {
                            message: format!("serialise reply: {e}"),
                        }
                    })
                }
                _other => Err(A2aError::MethodNotFound),
            }
        })
    }
}

/// Builds the agent card. The well-known path serves this exact value.
fn build_card() -> Arc<AgentCard> {
    Arc::new(
        AgentCardBuilder::new()
            .with_name("Echo Agent")
            .with_description("An A2A example that echoes messages back as agent messages.")
            .with_version("0.1.0")
            .with_interface(AgentInterface {
                url: url::Url::parse("http://127.0.0.1:8080/a2a").expect("static URL parses"),
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
                description: "Echoes the user's message back as an agent message.".into(),
                tags: Vec::new(),
            })
            .build()
            .expect("card is built from valid inputs"),
    )
}

/// Mounts the well-known path and the JSON-RPC path on one router.
fn build_router() -> Router {
    let card = build_card();
    let handler: Arc<dyn A2aRequestHandler> = Arc::new(EchoAgent);
    serve_well_known(card).merge(serve_jsonrpc("/a2a", handler, A2aVersion::V1_0))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(serve())
}

async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    let router = build_router();
    let addr: SocketAddr = "127.0.0.1:8080".parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("a2a_server: listening on {addr}");
    eprintln!("a2a_server: card at http://{addr}/.well-known/agent-card.json");
    eprintln!("a2a_server: JSON-RPC at http://{addr}/a2a");
    axum::serve(listener, router).await?;
    Ok(())
}
