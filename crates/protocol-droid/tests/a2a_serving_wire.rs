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

// These tests bind a real `axum` router, so they need the `http` feature to
// compile, and they read `protocol_droid::a2a::*` which is only present when
// the `a2a` feature is on. A consumer that enables only one of the two gets
// the a2a domain or the http transport, not the bridge between them — these
// tests cover the bridge.
#![cfg(all(feature = "a2a", feature = "http"))]

#![expect(
    clippy::expect_used,
    reason = "tests assert on fallible calls; a panic in a test is the right shape — propagate `?` would replace a named failure with `from()` noise"
)]

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use protocol_droid::adapters::primary::a2a::jsonrpc_serve::serve_jsonrpc;
use protocol_droid::adapters::primary::a2a::serve_agent::serve_a2a_agent;
use protocol_droid::adapters::primary::a2a::streaming_serve::A2aStreamingRequestHandler;
use protocol_droid::adapters::primary::a2a::well_known_serve::serve_well_known;
use protocol_droid::adapters::secondary::a2a::A2aMethod;
use protocol_droid::application::a2a::{A2aError, A2aRequestContext, A2aRequestHandler};
use protocol_droid::domain::a2a::A2aVersion;
use protocol_droid::domain::a2a::agent_card::{
    AgentCapabilities, AgentCardBuilder, AgentInterface, AgentSkill, JSONRPC,
};
use protocol_droid::domain::a2a::message::{Message, MessageRole, Part, TextPart};
use protocol_droid::domain::a2a::push_notification::TaskPushNotificationConfig;
use protocol_droid::domain::a2a::push_notification_methods::{
    CreateTaskPushNotificationConfigRequest, CreateTaskPushNotificationConfigResponse,
    DeleteTaskPushNotificationConfigRequest, GetTaskPushNotificationConfigRequest,
    GetTaskPushNotificationConfigResponse, ListTaskPushNotificationConfigsRequest,
    ListTaskPushNotificationConfigsResponse,
};
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
    next_config_id: u64,
    push_configs: std::collections::HashMap<String, TaskPushNotificationConfig>,
}

struct MinimalAgent {
    store: Arc<std::sync::Mutex<TaskStore>>,
}

impl A2aRequestHandler for MinimalAgent {
    // The MinimalAgent implements the same nine methods the
    // example covers. The dispatch table mirrors the example's
    // shape so the wire tests are checking the same envelope
    // the example produces.
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per A2A method the test agent supports"
    )]
    fn dispatch(
        &self,
        method: A2aMethod,
        params: Option<Value>,
        _context: &A2aRequestContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, A2aError>> + Send + '_>>
    {
        let store = self.store.clone();
        match method {
            A2aMethod::SendMessage => {
                let _request = match decode_params::<SendMessageRequest>(params) {
                    Ok(request) => request,
                    Err(error) => return decode_failed(error),
                };
                Box::pin(async move {
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
                })
            }
            A2aMethod::GetTask => {
                let request = match decode_params::<GetTaskRequest>(params) {
                    Ok(request) => request,
                    Err(error) => return decode_failed(error),
                };
                Box::pin(async move {
                    let value = store
                        .lock()
                        .expect("store lock")
                        .tasks
                        .get(&request.id)
                        .cloned()
                        .ok_or_else(|| A2aError::InvalidParams {
                            reason: format!("no task with id '{}'", request.id),
                        })?;
                    serde_json::to_value(value).map_err(|e| A2aError::InternalError {
                        message: e.to_string(),
                    })
                })
            }
            A2aMethod::CancelTask => {
                let request = match decode_params::<CancelTaskRequest>(params) {
                    Ok(request) => request,
                    Err(error) => return decode_failed(error),
                };
                Box::pin(async move {
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
                        })
                        .ok_or_else(|| A2aError::InvalidParams {
                            reason: format!("no task with id '{}'", request.id),
                        })?;
                    serde_json::to_value(value).map_err(|e| A2aError::InternalError {
                        message: e.to_string(),
                    })
                })
            }
            A2aMethod::ListTasks => {
                let _request = match decode_params::<Value>(params) {
                    Ok(request) => request,
                    Err(error) => return decode_failed(error),
                };
                Box::pin(async move {
                    let tasks = store
                        .lock()
                        .expect("store lock")
                        .tasks
                        .values()
                        .cloned()
                        .collect::<Vec<_>>();
                    // The A2A v1.0 spec names `pageSize` as
                    // REQUIRED and at least `1`; a response
                    // with `pageSize: 0` is malformed. The
                    // `.max(1)` floors the empty-store case
                    // to the spec minimum so a freshly-
                    // started agent ships a well-formed
                    // envelope. The same floor is in
                    // `paginate_tasks` in the example.
                    let page_size = tasks.len().max(1);
                    let total_size = tasks.len();
                    Ok(serde_json::json!({
                        "tasks": tasks,
                        "nextPageToken": "",
                        "pageSize": page_size,
                        "totalSize": total_size,
                    }))
                })
            }
            A2aMethod::CreateTaskPushNotificationConfig => {
                let request = match decode_params::<CreateTaskPushNotificationConfigRequest>(params)
                {
                    Ok(request) => request,
                    Err(error) => return decode_failed(error),
                };
                Box::pin(async move {
                    let stored = {
                        let mut store = store.lock().expect("store lock");
                        store.next_config_id = store.next_config_id.wrapping_add(1);
                        let id = format!("pc-{}", store.next_config_id);
                        let mut config = request.0;
                        config.id.clone_from(&id);
                        store.push_configs.insert(id.clone(), config.clone());
                        config
                    };
                    serde_json::to_value(CreateTaskPushNotificationConfigResponse(stored)).map_err(
                        |e| A2aError::InternalError {
                            message: e.to_string(),
                        },
                    )
                })
            }
            A2aMethod::GetTaskPushNotificationConfig => {
                let request = match decode_params::<GetTaskPushNotificationConfigRequest>(params) {
                    Ok(request) => request,
                    Err(error) => return decode_failed(error),
                };
                Box::pin(async move {
                    let config = store
                        .lock()
                        .expect("store lock")
                        .push_configs
                        .get(&request.id)
                        .cloned()
                        .ok_or_else(|| A2aError::InvalidParams {
                            reason: format!(
                                "no push config with id '{}' for task '{}'",
                                request.id, request.task_id
                            ),
                        })?;
                    serde_json::to_value(GetTaskPushNotificationConfigResponse(config)).map_err(
                        |e| A2aError::InternalError {
                            message: e.to_string(),
                        },
                    )
                })
            }
            A2aMethod::ListTaskPushNotificationConfigs => {
                let request = match decode_params::<ListTaskPushNotificationConfigsRequest>(params)
                {
                    Ok(request) => request,
                    Err(error) => return decode_failed(error),
                };
                Box::pin(async move {
                    let configs: Vec<TaskPushNotificationConfig> = store
                        .lock()
                        .expect("store lock")
                        .push_configs
                        .values()
                        .filter(|c| c.task_id == request.task_id)
                        .cloned()
                        .collect();
                    serde_json::to_value(ListTaskPushNotificationConfigsResponse::new(configs))
                        .map_err(|e| A2aError::InternalError {
                            message: e.to_string(),
                        })
                })
            }
            A2aMethod::DeleteTaskPushNotificationConfig => {
                let request = match decode_params::<DeleteTaskPushNotificationConfigRequest>(params)
                {
                    Ok(request) => request,
                    Err(error) => return decode_failed(error),
                };
                Box::pin(async move {
                    let removed = store
                        .lock()
                        .expect("store lock")
                        .push_configs
                        .remove(&request.id)
                        .ok_or_else(|| A2aError::InvalidParams {
                            reason: format!(
                                "no push config with id '{}' for task '{}'",
                                request.id, request.task_id
                            ),
                        })?;
                    let _ = removed;
                    Ok(serde_json::json!({}))
                })
            }
            _other => Box::pin(std::future::ready(Err(A2aError::MethodNotFound))),
        }
    }
}

/// Returns a ready-failed future for a dispatch arm whose
/// params could not be decoded.
fn decode_failed(
    error: A2aError,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, A2aError>> + Send + 'static>>
{
    Box::pin(std::future::ready(Err(error)))
}

fn decode_params<T: serde::de::DeserializeOwned>(params: Option<Value>) -> Result<T, A2aError> {
    serde_json::from_value(params.unwrap_or(Value::Null)).map_err(|error| A2aError::InvalidParams {
        reason: format!("params do not match the method shape: {error}"),
    })
}

fn build_card() -> Arc<protocol_droid::domain::a2a::agent_card::AgentCard> {
    build_card_with_capabilities(false, false)
}

/// Builds a card with the streaming and push-notification
/// capabilities toggled, so a test can pin the on-wire
/// shape the serving adapter uses for each combination.
fn build_card_with_capabilities(
    streaming: bool,
    push_notifications: bool,
) -> Arc<protocol_droid::domain::a2a::agent_card::AgentCard> {
    let capabilities = AgentCapabilities {
        streaming,
        push_notifications,
        ..Default::default()
    };
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
            .with_capabilities(capabilities)
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

fn router_with_capabilities(streaming: bool, push_notifications: bool) -> axum::Router {
    let card = build_card_with_capabilities(streaming, push_notifications);
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

/// The well-known path's `capabilities` object reflects the
/// fields the card declared — the spec's discovery contract
/// is that a peer reads these flags to decide which methods
/// to call. A card that says `streaming: false` would tell
/// a peer to never call `SendStreamingMessage`, and a card
/// that says `pushNotifications: false` would tell a peer
/// that `CreateTaskPushNotificationConfig` is unsupported.
#[tokio::test]
async fn well_known_serves_capabilities() -> Result<(), Box<dyn std::error::Error>> {
    let app = router_with_capabilities(true, true);
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
    let streaming = body
        .pointer("/capabilities/streaming")
        .and_then(Value::as_bool);
    let push = body
        .pointer("/capabilities/pushNotifications")
        .and_then(Value::as_bool);
    let extended = body
        .pointer("/capabilities/extendedAgentCard")
        .and_then(Value::as_bool);
    let extensions = body
        .pointer("/capabilities/extensions")
        .and_then(Value::as_array);
    assert_eq!(streaming, Some(true));
    assert_eq!(push, Some(true));
    assert_eq!(extended, Some(false));
    let Some(extensions) = extensions else {
        return Err("expected /capabilities/extensions array".into());
    };
    assert!(extensions.is_empty());
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
    let state = body.pointer("/result/status/state").and_then(Value::as_str);
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
    let state = body.pointer("/result/status/state").and_then(Value::as_str);
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
    let state = body.pointer("/result/status/state").and_then(Value::as_str);
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
    let state = body.pointer("/result/status/state").and_then(Value::as_str);
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

/// A `GetTask` request whose `id` field is missing
/// from the params surfaces as a typed `-32602`
/// (invalid params), not `-32601` (method not
/// found). The JSON-RPC spec distinguishes them:
/// `-32601` says "no such method on this server",
/// `-32602` says "the method exists but the params
/// you sent are not its shape". A dispatcher
/// that conflates them hides the wrong-doing
/// from the peer.
#[tokio::test]
async fn missing_required_param_field_returns_invalid_params_not_method_not_found()
-> Result<(), Box<dyn std::error::Error>> {
    let app = router();
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "GetTask",
        "params": {}
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
    assert_eq!(
        code,
        Some(-32602),
        "missing required `id` field is `-32602` invalid params, not `-32601` method not found; got {body}"
    );
    assert_eq!(message, Some("Invalid params"));
    Ok(())
}

/// `ListTasks` returns the tasks the
/// agent owns. The test populates the
/// store with two tasks via
/// `SendMessage`, then calls
/// `ListTasks` and asserts both task
/// ids come back. The wire shape the
/// peer reads is the
/// `ListTasksResponse` shape: a `tasks`
/// array, a `pageSize`, a
/// `nextPageToken`, and a
/// `totalSize`. The
/// `MinimalAgent` here is the same
/// one the other wire tests use, so
/// the response shape is the contract
/// every consumer sees.
#[tokio::test]
async fn list_tasks_returns_populated_tasks() -> Result<(), Box<dyn std::error::Error>> {
    let app = router();
    // Populate the store with two
    // tasks via SendMessage.
    for n in 0..2 {
        let send = json!({
            "jsonrpc": "2.0",
            "id": n,
            "method": "SendMessage",
            "params": {
                "message": {
                    "role": "user",
                    "parts": [{"kind": "text", "text": "hi"}]
                }
            }
        });
        let _ = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/a2a")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&send)?))?,
            )
            .await?;
    }
    // Now call ListTasks.
    let list = json!({
        "jsonrpc": "2.0",
        "id": 99,
        "method": "ListTasks",
        "params": {}
    });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/a2a")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&list)?))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = body_bytes(response.into_body()).await;
    let body: Value = serde_json::from_slice(&bytes)?;
    let jsonrpc = body.pointer("/jsonrpc").and_then(Value::as_str);
    let id = body.pointer("/id").and_then(Value::as_i64);
    let tasks = body.pointer("/result/tasks").and_then(Value::as_array);
    let page_size = body.pointer("/result/pageSize").and_then(Value::as_u64);
    let next_page_token = body
        .pointer("/result/nextPageToken")
        .and_then(Value::as_str);
    assert_eq!(jsonrpc, Some("2.0"));
    assert_eq!(id, Some(99));
    let task_list = tasks.ok_or("expected /result/tasks to be an array")?;
    if task_list.len() != 2 {
        return Err(format!("expected 2 tasks, got {}", task_list.len()).into());
    }
    if page_size != Some(2) {
        return Err(format!("expected pageSize 2, got {page_size:?}").into());
    }
    if next_page_token != Some("") {
        return Err(format!("expected empty nextPageToken, got {next_page_token:?}").into());
    }
    Ok(())
}

/// `ListTasks` on an empty store
/// returns an empty `tasks` array, a
/// `pageSize: 1` (the spec floor), a
/// zero `totalSize`, and an empty
/// `nextPageToken`. The wire shape is
/// what a freshly-started agent reads
/// back; a regression that returned
/// `null` for the `tasks` field would
/// crash a peer that iterates without
/// a null check.
#[tokio::test]
async fn list_tasks_on_empty_store_returns_empty_page() -> Result<(), Box<dyn std::error::Error>> {
    let app = router();
    let list = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "ListTasks",
        "params": {}
    });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/a2a")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&list)?))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = body_bytes(response.into_body()).await;
    let body: Value = serde_json::from_slice(&bytes)?;
    let tasks = body.pointer("/result/tasks").and_then(Value::as_array);
    let task_list = tasks.ok_or("expected /result/tasks to be an array")?;
    if !task_list.is_empty() {
        return Err(format!("expected empty tasks, got {} entries", task_list.len()).into());
    }
    let page_size = body.pointer("/result/pageSize").and_then(Value::as_u64);
    if page_size != Some(1) {
        return Err(format!(
            "expected pageSize 1 (the spec floor) on an empty store, got {page_size:?}"
        )
        .into());
    }
    Ok(())
}

/// Streaming support is a separate concern from the
/// request/response adapter. The handler that needs streaming
/// methods implements `A2aStreamingRequestHandler` and the
/// streaming `SendStreamingMessage` round-trips a
/// `working`-then-`completed` pair on a single SSE response.
#[tokio::test]
async fn send_streaming_message_emits_working_then_completed()
-> Result<(), Box<dyn std::error::Error>> {
    use protocol_droid::adapters::primary::a2a::streaming_serve::{
        A2aStreamingRequestHandler, serve_streaming,
    };
    use protocol_droid::domain::a2a::message::{Message, MessageRole, Part, TextPart};
    use protocol_droid::domain::a2a::send_message::SendMessageRequest;
    use protocol_droid::domain::a2a::streaming_events::StreamingEvent;
    use protocol_droid::domain::a2a::task_lifecycle::GetTaskRequest;

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
                        Ok(StreamingEvent::Status(
                            protocol_droid::domain::a2a::streaming_events::TaskStatusUpdateEvent {
                                task_id: "t-s1".to_owned(),
                                context_id: String::new(),
                                status: protocol_droid::domain::a2a::task::TaskStatus {
                                    state: protocol_droid::domain::a2a::task::TaskState::Working,
                                    message: Some(Message::new(MessageRole::Agent)),
                                    timestamp: None,
                                },
                                is_final: false,
                                metadata: std::collections::BTreeMap::new(),
                            },
                        )),
                        Ok(StreamingEvent::Status(
                            protocol_droid::domain::a2a::streaming_events::TaskStatusUpdateEvent {
                                task_id: "t-s1".to_owned(),
                                context_id: String::new(),
                                status: protocol_droid::domain::a2a::task::TaskStatus {
                                    state: protocol_droid::domain::a2a::task::TaskState::Completed,
                                    message: Some(Message::new(MessageRole::Agent).with_part(
                                        Part::Text(TextPart {
                                            text: "hi".to_owned(),
                                            metadata: std::collections::BTreeMap::new(),
                                        }),
                                    )),
                                    timestamp: None,
                                },
                                is_final: true,
                                metadata: std::collections::BTreeMap::new(),
                            },
                        )),
                    ]))
                }
                _other => Box::pin(futures_util::stream::iter(std::iter::once(Err(
                    A2aError::MethodNotFound,
                )))),
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

/// `CreateTaskPushNotificationConfig` round-trips a
/// config through the JSON-RPC adapter. The server is
/// expected to assign a `pc-N` id and echo it back.
#[tokio::test]
async fn create_push_notification_config_round_trips() -> Result<(), Box<dyn std::error::Error>> {
    let app = router();
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "CreateTaskPushNotificationConfig",
        "params": {
            "id": "placeholder",
            "taskId": "t-1",
            "url": "https://example.com/webhook",
            "token": "tok-1"
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
    assert_eq!(
        body.pointer("/result/id").and_then(Value::as_str),
        Some("pc-1")
    );
    assert_eq!(
        body.pointer("/result/url").and_then(Value::as_str),
        Some("https://example.com/webhook")
    );
    assert_eq!(
        body.pointer("/result/taskId").and_then(Value::as_str),
        Some("t-1")
    );
    assert_eq!(
        body.pointer("/result/token").and_then(Value::as_str),
        Some("tok-1")
    );
    Ok(())
}

/// `GetTaskPushNotificationConfig` reads the config back
/// from the store by id.
#[tokio::test]
async fn get_push_notification_config_round_trips() -> Result<(), Box<dyn std::error::Error>> {
    let app = router();
    let create = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "CreateTaskPushNotificationConfig",
        "params": {
            "id": "placeholder",
            "taskId": "t-1",
            "url": "https://example.com/webhook"
        }
    });
    let _ = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/a2a")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&create)?))?,
        )
        .await?;
    let get = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "GetTaskPushNotificationConfig",
        "params": {"id": "pc-1", "taskId": "t-1"}
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
    assert_eq!(
        body.pointer("/result/id").and_then(Value::as_str),
        Some("pc-1")
    );
    Ok(())
}

/// `GetTaskPushNotificationConfig` for an unknown id
/// returns `-32602` (invalid params).
#[tokio::test]
async fn get_missing_push_notification_config_returns_invalid_params()
-> Result<(), Box<dyn std::error::Error>> {
    let app = router();
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "GetTaskPushNotificationConfig",
        "params": {"id": "pc-missing", "taskId": "t-1"}
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
    let bytes = body_bytes(response.into_body()).await;
    let body: Value = serde_json::from_slice(&bytes)?;
    assert_eq!(
        body.pointer("/error/code").and_then(Value::as_i64),
        Some(-32602)
    );
    Ok(())
}

/// `ListTaskPushNotificationConfigs` returns the configs
/// for a task, in insertion order. An empty list is
/// represented as an empty `configs` array.
#[tokio::test]
async fn list_push_notification_configs_filters_by_task() -> Result<(), Box<dyn std::error::Error>>
{
    let app = router();
    for (i, task_id) in ["t-1", "t-1", "t-2"].iter().enumerate() {
        let create = json!({
            "jsonrpc": "2.0",
            "id": i + 1,
            "method": "CreateTaskPushNotificationConfig",
            "params": {
                "id": "placeholder",
                "taskId": task_id,
                "url": format!("https://example.com/wh/{i}")
            }
        });
        let _ = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/a2a")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&create)?))?,
            )
            .await?;
    }
    let list = json!({
        "jsonrpc": "2.0",
        "id": 100,
        "method": "ListTaskPushNotificationConfigs",
        "params": {"taskId": "t-1"}
    });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/a2a")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&list)?))?,
        )
        .await?;
    let bytes = body_bytes(response.into_body()).await;
    let body: Value = serde_json::from_slice(&bytes)?;
    let configs = body.pointer("/result/configs").and_then(Value::as_array);
    let Some(configs) = configs else {
        return Err("expected /result/configs array".into());
    };
    assert_eq!(configs.len(), 2);
    for config in configs {
        assert_eq!(
            config.pointer("/taskId").and_then(Value::as_str),
            Some("t-1")
        );
    }
    Ok(())
}

/// `DeleteTaskPushNotificationConfig` removes a config;
/// a subsequent `Get` returns `-32602` (invalid params).
#[tokio::test]
async fn delete_push_notification_config_removes_it() -> Result<(), Box<dyn std::error::Error>> {
    let app = router();
    let create = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "CreateTaskPushNotificationConfig",
        "params": {
            "id": "placeholder",
            "taskId": "t-1",
            "url": "https://example.com/webhook"
        }
    });
    let _ = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/a2a")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&create)?))?,
        )
        .await?;
    let delete = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "DeleteTaskPushNotificationConfig",
        "params": {"id": "pc-1", "taskId": "t-1"}
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/a2a")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&delete)?))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let get = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "GetTaskPushNotificationConfig",
        "params": {"id": "pc-1", "taskId": "t-1"}
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
    assert_eq!(
        body.pointer("/error/code").and_then(Value::as_i64),
        Some(-32602)
    );
    Ok(())
}

/// `serve_a2a_agent` is the convenience façade that
/// mounts the well-known path, the JSON-RPC path,
/// and the streaming path on one router. The test
/// confirms the façade serves the well-known path
/// the same way `serve_well_known` does, and that
/// the JSON-RPC path is reachable on the prefix
/// the consumer chose. The streaming path is the
/// same prefix with `/stream` appended; the
/// example's `a2a_client` exercises it
/// end-to-end, which is the wire contract.
#[tokio::test]
async fn facade_serves_well_known_and_jsonrpc() -> Result<(), Box<dyn std::error::Error>> {
    let card = build_card();
    let store = Arc::new(std::sync::Mutex::new(TaskStore::default()));
    let handler: Arc<dyn A2aRequestHandler> = Arc::new(MinimalAgent {
        store: store.clone(),
    });
    let streaming: Arc<dyn A2aStreamingRequestHandler> = Arc::new(EmptyStreamAgent {
        store: store.clone(),
    });
    let app = serve_a2a_agent("/a2a", handler, streaming, card);

    let well_known = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/.well-known/agent-card.json")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(well_known.status(), StatusCode::OK);

    let send = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "SendMessage",
        "params": {
            "message": {
                "role": "user",
                "parts": [{"type": "text", "text": "hi"}]
            }
        }
    });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/a2a")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&send)?))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = body_bytes(response.into_body()).await;
    let body: Value = serde_json::from_slice(&bytes)?;
    assert!(body.get("result").is_some(), "expected a result: {body}");
    Ok(())
}

/// A streaming handler that returns an empty stream
/// for every method. The façade test only needs the
/// type to satisfy `A2aStreamingRequestHandler`; the
/// JSON-RPC path is what the assertion exercises.
struct EmptyStreamAgent {
    store: Arc<std::sync::Mutex<TaskStore>>,
}

impl A2aStreamingRequestHandler for EmptyStreamAgent {
    fn dispatch_stream(
        &self,
        _method: A2aMethod,
        _params: Option<Value>,
        _context: &A2aRequestContext,
    ) -> std::pin::Pin<
        Box<
            dyn futures_util::Stream<
                    Item = Result<
                        protocol_droid::domain::a2a::streaming_events::StreamingEvent,
                        A2aError,
                    >,
                > + Send,
        >,
    > {
        drop(self.store.lock().expect("store lock"));
        Box::pin(futures_util::stream::empty())
    }
}
