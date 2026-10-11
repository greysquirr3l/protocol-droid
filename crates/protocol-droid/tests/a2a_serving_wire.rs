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
use protocol_droid::domain::a2a::send_message::{SendMessageRequest, SendMessageResponse};
use protocol_droid::domain::a2a::task::{Task, TaskState, TaskStatus};
use protocol_droid::domain::a2a::task_lifecycle::{CancelTaskRequest, GetTaskRequest};
use serde_json::{Value, json};
use tower::ServiceExt;

/// A minimal task store, identical in shape to the one in the
/// `a2a_server` example. The wire tests exercise the same
/// `SendMessage → GetTask → ListTasks → CancelTask` flow the
/// example does, but driven through `tower::ServiceExt::oneshot`
/// so a port never has to bind.
#[derive(Default)]
struct TaskStore {
    next_id: u64,
    tasks: std::collections::HashMap<String, Task>,
}

struct MinimalAgent {
    store: Arc<std::sync::Mutex<TaskStore>>,
}

impl A2aRequestHandler for MinimalAgent {
    fn dispatch(
        &self,
        method: A2aMethod,
        params: Option<Value>,
        _context: &A2aRequestContext,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Value, A2aError>> + Send + '_>,
    > {
        let store = self.store.clone();
        match method {
            A2aMethod::SendMessage => Box::pin(async move {
                let _ = decode_params::<SendMessageRequest>(params)?;
                let next_id = {
                    let mut store = store.lock().expect("store lock");
                    store.next_id = store.next_id.wrapping_add(1);
                    store.next_id
                };
                let task_id = format!("t-{next_id}");
                let reply = Message::new(MessageRole::Agent).with_part(Part::Text(TextPart {
                    text: "hi".to_owned(),
                    metadata: std::collections::BTreeMap::new(),
                }));
                let task = Task::new(task_id.clone()).with_status(TaskStatus {
                    state: TaskState::Completed,
                    message: Some(reply),
                    timestamp: None,
                });
                store
                    .lock()
                    .expect("store lock")
                    .tasks
                    .insert(task_id, task.clone());
                serde_json::to_value(SendMessageResponse::Task(task)).map_err(|e| {
                    A2aError::InternalError {
                        message: e.to_string(),
                    }
                })
            }),
            A2aMethod::GetTask => Box::pin(async move {
                let request: GetTaskRequest = decode_params(params)?;
                let value = store
                    .lock()
                    .expect("store lock")
                    .tasks
                    .get(&request.id)
                    .cloned();
                let value = value.ok_or_else(|| A2aError::InvalidParams {
                    reason: format!("no task with id '{}'", request.id),
                })?;
                serde_json::to_value(value).map_err(|e| A2aError::InternalError {
                    message: e.to_string(),
                })
            }),
            A2aMethod::CancelTask => Box::pin(async move {
                let request: CancelTaskRequest = decode_params(params)?;
                let value = store
                    .lock()
                    .expect("store lock")
                    .tasks
                    .get_mut(&request.id)
                    .map(|t| {
                        t.status = TaskStatus {
                            state: TaskState::Cancelled,
                            message: None,
                            timestamp: None,
                        };
                        t.clone()
                    });
                let value = value.ok_or_else(|| A2aError::InvalidParams {
                    reason: format!("no task with id '{}'", request.id),
                })?;
                serde_json::to_value(value).map_err(|e| A2aError::InternalError {
                    message: e.to_string(),
                })
            }),
            A2aMethod::ListTasks => Box::pin(async move {
                let tasks = {
                    let store = store.lock().expect("store lock");
                    store.tasks.values().cloned().collect::<Vec<_>>()
                };
                Ok(serde_json::json!({
                    "tasks": tasks,
                    "nextPageToken": "",
                    "pageSize": tasks.len(),
                }))
            }),
            _other => Box::pin(std::future::ready(Err(A2aError::MethodNotFound))),
        }
    }
}

fn decode_params<T: serde::de::DeserializeOwned>(params: Option<Value>) -> Result<T, A2aError> {
    serde_json::from_value(params.unwrap_or(Value::Null)).map_err(|error| {
        A2aError::InvalidParams {
            reason: format!("params do not match the method shape: {error}"),
        }
    })
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
    let handler: Arc<dyn A2aRequestHandler> = Arc::new(MinimalAgent {
        store: Arc::new(std::sync::Mutex::new(TaskStore::default())),
    });
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
/// `result` is the agent's reply task. The reply's
/// `status.message` carries the agent message.
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
    let state = body
        .pointer("/result/status/state")
        .and_then(Value::as_str);
    let role = body
        .pointer("/result/status/message/role")
        .and_then(Value::as_str);
    let text = body
        .pointer("/result/status/message/parts/0/text")
        .and_then(Value::as_str);
    assert_eq!(jsonrpc, Some("2.0"));
    assert_eq!(id, Some(1));
    assert_eq!(state, Some("completed"));
    assert_eq!(role, Some("agent"));
    assert_eq!(text, Some("hi"));
    Ok(())
}

/// `GetTask` returns a JSON-RPC `result` envelope whose
/// `result` is the task object. The task is created by a
/// prior `SendMessage` and read back with `GetTask`.
#[tokio::test]
async fn get_task_round_trips() -> Result<(), Box<dyn std::error::Error>> {
    let app = router();
    let send = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "SendMessage",
        "params": {
            "message": {
                "role": "user",
                "parts": [{"kind": "text", "text": "hi"}]
            }
        }
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/a2a")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&send)?))?,
        )
        .await?;
    let bytes = body_bytes(response.into_body()).await;
    let body: Value = serde_json::from_slice(&bytes)?;
    let task_id = body
        .pointer("/result/id")
        .and_then(Value::as_str)
        .map(str::to_owned);

    let get = json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "GetTask",
        "params": {"id": task_id.clone().unwrap_or_default()}
    });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/a2a")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&get)?))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = body_bytes(response.into_body()).await;
    let body: Value = serde_json::from_slice(&bytes)?;
    let id = body.pointer("/id").and_then(Value::as_i64);
    let read_id = body.pointer("/result/id").and_then(Value::as_str);
    let state = body
        .pointer("/result/status/state")
        .and_then(Value::as_str);
    assert_eq!(id, Some(7));
    assert_eq!(read_id, task_id.as_deref());
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

/// `SendMessage` followed by `GetTask` round-trips the task id
/// the agent assigns, and the task's status carries the reply.
#[tokio::test]
async fn send_then_get_returns_same_task() -> Result<(), Box<dyn std::error::Error>> {
    let app = router();
    let send = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "SendMessage",
        "params": {
            "message": {
                "role": "user",
                "parts": [{"kind": "text", "text": "hi"}]
            }
        }
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/a2a")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&send)?))?,
        )
        .await?;
    let bytes = body_bytes(response.into_body()).await;
    let body: Value = serde_json::from_slice(&bytes)?;
    let task_id = body
        .pointer("/result/id")
        .and_then(Value::as_str)
        .map(str::to_owned);

    let get = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "GetTask",
        "params": {"id": task_id.clone().unwrap_or_default()}
    });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/a2a")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&get)?))?,
        )
        .await?;
    let bytes = body_bytes(response.into_body()).await;
    let body: Value = serde_json::from_slice(&bytes)?;
    let read_id = body.pointer("/result/id").and_then(Value::as_str);
    let state = body
        .pointer("/result/status/state")
        .and_then(Value::as_str);
    assert_eq!(read_id, task_id.as_deref());
    assert_eq!(state, Some("completed"));
    Ok(())
}

/// `CancelTask` flips a task's state to `cancelled`. The
/// follow-up `GetTask` observes the new state.
#[tokio::test]
async fn cancel_flips_state_to_cancelled() -> Result<(), Box<dyn std::error::Error>> {
    let app = router();
    let send = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "SendMessage",
        "params": {
            "message": {
                "role": "user",
                "parts": [{"kind": "text", "text": "hi"}]
            }
        }
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/a2a")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&send)?))?,
        )
        .await?;
    let bytes = body_bytes(response.into_body()).await;
    let body: Value = serde_json::from_slice(&bytes)?;
    let task_id = body
        .pointer("/result/id")
        .and_then(Value::as_str)
        .map(str::to_owned);

    let cancel = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "CancelTask",
        "params": {"id": task_id.clone().unwrap_or_default()}
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/a2a")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&cancel)?))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);

    let get = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "GetTask",
        "params": {"id": task_id.unwrap_or_default()}
    });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/a2a")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&get)?))?,
        )
        .await?;
    let bytes = body_bytes(response.into_body()).await;
    let body: Value = serde_json::from_slice(&bytes)?;
    let state = body
        .pointer("/result/status/state")
        .and_then(Value::as_str);
    assert_eq!(state, Some("cancelled"));
    Ok(())
}

/// `GetTask` for an unknown id returns `-32602` (invalid
/// params) with a reason naming the missing id.
#[tokio::test]
async fn get_missing_task_returns_invalid_params() -> Result<(), Box<dyn std::error::Error>> {
    let app = router();
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "GetTask",
        "params": {"id": "t-does-not-exist"}
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
    assert_eq!(code, Some(-32602));
    assert_eq!(message, Some("Invalid params"));
    Ok(())
}

/// Streaming support is a separate concern from the
/// request/response adapter. The handler that needs streaming
/// methods implements `A2aStreamingRequestHandler` and the
/// streaming `SendStreamingMessage` round-trips a
/// `working`-then-`completed` pair on a single SSE response.
#[tokio::test]
async fn send_streaming_message_emits_working_then_completed() -> Result<(), Box<dyn std::error::Error>> {
    use protocol_droid::adapters::primary::a2a::streaming_serve::{
        serve_streaming, A2aStreamingRequestHandler,
    };
    use protocol_droid::domain::a2a::send_message::SendMessageRequest;
    use protocol_droid::domain::a2a::streaming_events::StreamingEvent;
    use protocol_droid::domain::a2a::task_lifecycle::GetTaskRequest;
    use protocol_droid::domain::a2a::message::{Message, MessageRole, Part, TextPart};

    struct EchoStreamAgent;
    impl A2aStreamingRequestHandler for EchoStreamAgent {
        fn dispatch_stream(
            &self,
            method: protocol_droid::adapters::secondary::a2a::A2aMethod,
            _params: Option<Value>,
            _context: &A2aRequestContext,
        ) -> std::pin::Pin<
            Box<dyn futures_util::Stream<Item = Result<StreamingEvent, A2aError>> + Send>,
        > {
            match method {
                protocol_droid::adapters::secondary::a2a::A2aMethod::SendStreamingMessage => {
                    Box::pin(futures_util::stream::iter([
                        Ok(StreamingEvent::Status(protocol_droid::domain::a2a::streaming_events::TaskStatusUpdateEvent {
                            task_id: "t-s1".to_owned(),
                            context_id: String::new(),
                            status: protocol_droid::domain::a2a::task::TaskStatus {
                                state: protocol_droid::domain::a2a::task::TaskState::Working,
                                message: Some(Message::new(MessageRole::Agent)),
                                timestamp: None,
                            },
                            is_final: false,
                            metadata: std::collections::BTreeMap::new(),
                        })),
                        Ok(StreamingEvent::Status(protocol_droid::domain::a2a::streaming_events::TaskStatusUpdateEvent {
                            task_id: "t-s1".to_owned(),
                            context_id: String::new(),
                            status: protocol_droid::domain::a2a::task::TaskStatus {
                                state: protocol_droid::domain::a2a::task::TaskState::Completed,
                                message: Some(Message::new(MessageRole::Agent).with_part(Part::Text(TextPart {
                                    text: "hi".to_owned(),
                                    metadata: std::collections::BTreeMap::new(),
                                }))),
                                timestamp: None,
                            },
                            is_final: true,
                            metadata: std::collections::BTreeMap::new(),
                        })),
                    ]))
                }
                _other => Box::pin(futures_util::stream::iter(std::iter::once(Err(A2aError::MethodNotFound)))),
            }
        }
    }
    let _ = SendMessageRequest::new;
    let _: GetTaskRequest = GetTaskRequest::new(String::new());

    let app: axum::Router = serve_streaming(
        "/a2a/stream",
        Arc::new(EchoStreamAgent) as Arc<dyn A2aStreamingRequestHandler>,
        protocol_droid::domain::a2a::A2aVersion::V1_0,
    );
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "SendStreamingMessage",
        "params": {
            "message": {
                "role": "user",
                "parts": [{"kind": "text", "text": "hi"}]
            }
        }
    });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/a2a/stream")
                .header("content-type", "application/json")
                .header("accept", "text/event-stream")
                .body(Body::from(serde_json::to_vec(&body)?))?,
        )
        .await?;
    let bytes = body_bytes(response.into_body()).await;
    let text = std::str::from_utf8(&bytes)?;
    // Two SSE events, one `working` and one `completed`/`final`.
    assert!(text.contains(r#""state":"working""#));
    assert!(text.contains(r#""state":"completed""#));
    assert!(text.contains(r#""final":true"#));
    assert!(text.contains(r#""taskId":"t-s1""#));
    Ok(())
}
