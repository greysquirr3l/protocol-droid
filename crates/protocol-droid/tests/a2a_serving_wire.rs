//! End-to-end wire tests for the A2A serving adapters.
//!
//! These tests pin the wire shape of the A2A serving adapters —
//! `serve_well_known` and `serve_jsonrpc` — the same way an
//! external peer would observe them. The router is fed requests
//! in-process through `tower::ServiceExt::oneshot`, so the tests
//! do not bind a port, do not spawn a runtime, and do not
//! depend on the network. What they do pin is every wire
//! detail a conforming client reads back: the well-known URI,
//! the JSON-RPC envelope, the JSON-RPC error code, and the
//! response shape for `SendMessage` and `GetTask`.

#![expect(
    clippy::expect_used,
    reason = "tests assert on fallible calls; a panic in a test is the right shape — propagate `?` would replace a named failure with `from()` noise"
)]

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use protocol_droid::adapters::primary::a2a::jsonrpc_serve::serve_jsonrpc;
use protocol_droid::adapters::primary::a2a::well_known_serve::serve_well_known;
use protocol_droid::adapters::secondary::a2a::A2aMethod;
use protocol_droid::application::a2a::{A2aError, A2aRequestContext, A2aRequestHandler};
use protocol_droid::domain::a2a::A2aVersion;
use protocol_droid::domain::a2a::agent_card::{
    AgentCapabilities, AgentCardBuilder, AgentInterface, AgentSkill, JSONRPC,
};
use protocol_droid::domain::a2a::message::{Message, MessageRole, Part, TextPart};
use protocol_droid::domain::a2a::send_message::SendMessageResponse;
use protocol_droid::domain::a2a::task::{Task, TaskState, TaskStatus};
use serde_json::{Value, json};
use tower::ServiceExt;

/// The minimal agent: `SendMessage` and `GetTask` round-trip;
/// every other method returns `MethodNotFound` (JSON-RPC
/// `-32601`).
struct MinimalAgent;

impl A2aRequestHandler for MinimalAgent {
    fn dispatch(
        &self,
        method: A2aMethod,
        _params: Option<Value>,
        _context: &A2aRequestContext,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Value, A2aError>> + Send + '_>,
    > {
        let value = match method {
            A2aMethod::GetTask => serde_json::to_value(
                Task::new("t-1").with_status(TaskStatus {
                    state: TaskState::Completed,
                    message: None,
                    timestamp: None,
                }),
            ),
            A2aMethod::SendMessage => serde_json::to_value(SendMessageResponse::Message(
                Message::new(MessageRole::Agent).with_part(Part::Text(TextPart {
                    text: "hi".to_owned(),
                    metadata: std::collections::BTreeMap::new(),
                })),
            )),
            other => {
                let _ = other;
                return Box::pin(std::future::ready(Err(A2aError::MethodNotFound)));
            }
        };
        Box::pin(std::future::ready(value.map_err(|e| A2aError::InternalError {
            message: e.to_string(),
        })))
    }
}

fn build_card() -> Arc<protocol_droid::domain::a2a::agent_card::AgentCard> {
    Arc::new(
        AgentCardBuilder::new()
            .with_name("Test Agent")
            .with_description("A wire-test agent.")
            .with_version("0.1.0")
            .with_interface(AgentInterface {
                url: url::Url::parse("http://127.0.0.1:0/a2a").expect("static URL"),
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
                description: "Echoes.".into(),
                tags: Vec::new(),
            })
            .build()
            .expect("valid card"),
    )
}

fn router() -> axum::Router {
    let card = build_card();
    let handler: Arc<dyn A2aRequestHandler> = Arc::new(MinimalAgent);
    serve_well_known(card).merge(serve_jsonrpc("/a2a", handler, A2aVersion::V1_0))
}

async fn body_bytes(body: Body) -> Vec<u8> {
    to_bytes(body, 1_048_576)
        .await
        .expect("body to bytes")
        .to_vec()
}

/// The well-known URI returns 200 and the spec-shaped `AgentCard`.
#[tokio::test]
async fn well_known_serves_card() -> Result<(), Box<dyn std::error::Error>> {
    let app = router();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/.well-known/agent-card.json")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = body_bytes(response.into_body()).await;
    let body: Value = serde_json::from_slice(&bytes)?;
    let name = body.pointer("/name").and_then(Value::as_str);
    let version = body.pointer("/version").and_then(Value::as_str);
    let binding = body
        .pointer("/supportedInterfaces/0/protocolBinding")
        .and_then(Value::as_str);
    let pv = body
        .pointer("/supportedInterfaces/0/protocolVersion")
        .and_then(Value::as_str);
    let input_mode = body.pointer("/defaultInputModes/0").and_then(Value::as_str);
    let output_mode = body
        .pointer("/defaultOutputModes/0")
        .and_then(Value::as_str);
    let skill_id = body.pointer("/skills/0/id").and_then(Value::as_str);
    assert_eq!(name, Some("Test Agent"));
    assert_eq!(version, Some("0.1.0"));
    assert_eq!(binding, Some("JSONRPC"));
    assert_eq!(pv, Some("1.0"));
    assert_eq!(input_mode, Some("text/plain"));
    assert_eq!(output_mode, Some("text/plain"));
    assert_eq!(skill_id, Some("echo"));
    Ok(())
}

/// `SendMessage` returns a JSON-RPC `result` envelope whose
/// `result` is the agent's reply message.
#[tokio::test]
async fn send_message_round_trips() -> Result<(), Box<dyn std::error::Error>> {
    let app = router();
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "SendMessage",
        "params": {
            "message": {
                "role": "user",
                "messageId": "m-1",
                "parts": [{"kind": "text", "text": "hi"}]
            }
        }
    });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/a2a")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body)?))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = body_bytes(response.into_body()).await;
    let body: Value = serde_json::from_slice(&bytes)?;
    let jsonrpc = body.pointer("/jsonrpc").and_then(Value::as_str);
    let id = body.pointer("/id").and_then(Value::as_i64);
    let role = body.pointer("/result/role").and_then(Value::as_str);
    let text = body
        .pointer("/result/parts/0/text")
        .and_then(Value::as_str);
    assert_eq!(jsonrpc, Some("2.0"));
    assert_eq!(id, Some(1));
    assert_eq!(role, Some("agent"));
    assert_eq!(text, Some("hi"));
    Ok(())
}

/// `GetTask` returns a JSON-RPC `result` envelope whose
/// `result` is the task object.
#[tokio::test]
async fn get_task_round_trips() -> Result<(), Box<dyn std::error::Error>> {
    let app = router();
    let body = json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "GetTask",
        "params": {"id": "t-1"}
    });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/a2a")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body)?))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = body_bytes(response.into_body()).await;
    let body: Value = serde_json::from_slice(&bytes)?;
    let id = body.pointer("/id").and_then(Value::as_i64);
    let task_id = body.pointer("/result/id").and_then(Value::as_str);
    let state = body
        .pointer("/result/status/state")
        .and_then(Value::as_str);
    assert_eq!(id, Some(7));
    assert_eq!(task_id, Some("t-1"));
    assert_eq!(state, Some("completed"));
    Ok(())
}

/// An unknown method returns the spec-named `-32601` code
/// and a `Method not found` message.
#[tokio::test]
async fn unknown_method_returns_method_not_found() -> Result<(), Box<dyn std::error::Error>> {
    let app = router();
    let body = json!({
        "jsonrpc": "2.0",
        "id": 99,
        "method": "NotARealMethod"
    });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/a2a")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body)?))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = body_bytes(response.into_body()).await;
    let body: Value = serde_json::from_slice(&bytes)?;
    let code = body.pointer("/error/code").and_then(Value::as_i64);
    let message = body.pointer("/error/message").and_then(Value::as_str);
    assert_eq!(code, Some(-32601));
    assert_eq!(message, Some("Method not found"));
    Ok(())
}
