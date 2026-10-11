//! End-to-end wire tests for [`A2aClient`].
//!
//! These tests pin the wire shape of the typed A2A client
//! the same way an external consumer would observe it: a
//! real HTTP listener on a free port, the
//! `serve_well_known` + `serve_jsonrpc` serving adapters
//! under test, and a real `reqwest` driven by
//! [`A2aClient`]. The patterns are the same ones an
//! external agent would exercise; what they pin is the
//! typed response shape for `SendMessage`, `GetTask`,
//! `CancelTask`, `ListTasks`, the four
//! push-notification-config methods, and the streaming
//! SSE shape for `SendStreamingMessage`.

#![expect(
    clippy::expect_used,
    reason = "tests assert on fallible calls; a panic in a test is the right shape"
)]

use std::pin::Pin;
use std::sync::{Arc, Mutex};

use axum::Router;
use futures_core::Stream;
use futures_util::stream;
use protocol_droid::adapters::primary::a2a::jsonrpc_serve::serve_jsonrpc;
use protocol_droid::adapters::primary::a2a::streaming_serve::{
    A2aStreamingRequestHandler, serve_streaming,
};
use protocol_droid::adapters::primary::a2a::well_known_serve::serve_well_known;
use protocol_droid::adapters::secondary::a2a::A2aClient;
use protocol_droid::adapters::secondary::a2a::A2aMethod;
use protocol_droid::application::a2a::{A2aError, A2aRequestContext, A2aRequestHandler};
use protocol_droid::domain::a2a::A2aVersion;
use protocol_droid::domain::a2a::agent_card::{
    AgentCardBuilder, AgentInterface, HTTPAuthSecurityScheme, JSONRPC, SecurityScheme,
};
use protocol_droid::domain::a2a::message::{Message, MessageRole, Part, TextPart};
use protocol_droid::domain::a2a::push_notification::TaskPushNotificationConfig;
use protocol_droid::domain::a2a::push_notification_methods::{
    CreateTaskPushNotificationConfigRequest, DeleteTaskPushNotificationConfigRequest,
    GetTaskPushNotificationConfigRequest, ListTaskPushNotificationConfigsRequest,
};
use protocol_droid::domain::a2a::send_message::{SendMessageRequest, SendMessageResponse};
use protocol_droid::domain::a2a::streaming_events::{
    StreamingEvent, TaskStatusUpdateEvent,
};
use protocol_droid::domain::a2a::task::{Task, TaskState, TaskStatus};
use protocol_droid::domain::a2a::task_lifecycle::{CancelTaskRequest, GetTaskRequest};
use serde_json::Value;
use tokio::net::TcpListener;

#[derive(Default)]
struct TaskStore {
    next_id: u64,
    tasks: std::collections::HashMap<String, Task>,
    next_config_id: u64,
    push_configs: std::collections::HashMap<String, TaskPushNotificationConfig>,
}

#[derive(Clone)]
struct MinimalAgent {
    store: Arc<Mutex<TaskStore>>,
}

impl A2aRequestHandler for MinimalAgent {
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per A2A method the test agent supports"
    )]
    #[expect(
        clippy::significant_drop_tightening,
        reason = "each match arm holds the params Value across the from_value call; binding a local is uglier than the warning"
    )]
    fn dispatch(
        &self,
        method: A2aMethod,
        params: Option<Value>,
        _context: &A2aRequestContext,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Value, A2aError>> + Send + '_>> {
        let store = self.store.clone();
        Box::pin(async move {
            match method {
                A2aMethod::SendMessage => {
                    let params = params.unwrap_or(Value::Null);
                    let _request: SendMessageRequest = serde_json::from_value(params)
                        .map_err(|error| A2aError::InvalidParams {
                            reason: error.to_string(),
                        })?;
                    let next_id = {
                        let mut guard = store.lock().expect("store lock");
                        guard.next_id = guard.next_id.wrapping_add(1);
                        guard.next_id
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
                    serde_json::to_value(SendMessageResponse::Task(task)).map_err(|error| {
                        A2aError::InternalError {
                            message: error.to_string(),
                        }
                    })
                }
                A2aMethod::GetTask => {
                    let request: GetTaskRequest = serde_json::from_value(
                        params.unwrap_or(Value::Null),
                    )
                    .map_err(|error| A2aError::InvalidParams {
                        reason: error.to_string(),
                    })?;
                    let guard = store.lock().expect("store lock");
                    let Some(task) = guard.tasks.get(&request.id).cloned() else {
                        return Err(A2aError::InvalidParams {
                            reason: format!("no task with id {}", request.id),
                        });
                    };
                    serde_json::to_value(
                        protocol_droid::domain::a2a::task_lifecycle::GetTaskResponse(task),
                    )
                    .map_err(|error| A2aError::InternalError {
                        message: error.to_string(),
                    })
                }
                A2aMethod::CancelTask => {
                    let request: CancelTaskRequest = serde_json::from_value(
                        params.unwrap_or(Value::Null),
                    )
                    .map_err(|error| A2aError::InvalidParams {
                        reason: error.to_string(),
                    })?;
                    let mut guard = store.lock().expect("store lock");
                    let Some(task) = guard.tasks.get_mut(&request.id) else {
                        return Err(A2aError::InvalidParams {
                            reason: format!("no task with id {}", request.id),
                        });
                    };
                    let cancelled = Task::new(task.id.clone()).with_status(TaskStatus {
                        state: TaskState::Cancelled,
                        message: task.status.message.clone(),
                        timestamp: None,
                    });
                    *task = cancelled.clone();
                    serde_json::to_value(
                        protocol_droid::domain::a2a::task_lifecycle::CancelTaskResponse(
                            cancelled,
                        ),
                    )
                    .map_err(|error| A2aError::InternalError {
                        message: error.to_string(),
                    })
                }
                A2aMethod::ListTasks => {
                    let _request: protocol_droid::domain::a2a::list_tasks::ListTasksRequest =
                        serde_json::from_value(params.unwrap_or(Value::Null)).map_err(|error| {
                            A2aError::InvalidParams {
                                reason: error.to_string(),
                            }
                        })?;
                    let tasks: Vec<Task> = store
                        .lock()
                        .expect("store lock")
                        .tasks
                        .values()
                        .cloned()
                        .collect();
                    let total = u32::try_from(tasks.len()).unwrap_or(u32::MAX);
                    serde_json::to_value(
                        protocol_droid::domain::a2a::list_tasks::ListTasksResponse {
                            tasks,
                            next_page_token: String::new(),
                            page_size: 1,
                            total_size: total,
                        },
                    )
                    .map_err(|error| A2aError::InternalError {
                        message: error.to_string(),
                    })
                }
                A2aMethod::CreateTaskPushNotificationConfig => {
                    let request: CreateTaskPushNotificationConfigRequest =
                        serde_json::from_value(params.unwrap_or(Value::Null)).map_err(|error| {
                            A2aError::InvalidParams {
                                reason: error.to_string(),
                            }
                        })?;
                    let next_id = {
                        let mut guard = store.lock().expect("store lock");
                        guard.next_config_id = guard.next_config_id.wrapping_add(1);
                        guard.next_config_id
                    };
                    let config_id = format!("c-{next_id}");
                    let config = TaskPushNotificationConfig {
                        id: config_id.clone(),
                        task_id: request.0.task_id,
                        url: request.0.url,
                        token: request.0.token,
                        authentication: request.0.authentication,
                        metadata: request.0.metadata,
                    };
                    store
                        .lock()
                        .expect("store lock")
                        .push_configs
                        .insert(config_id, config.clone());
                    serde_json::to_value(
                        protocol_droid::domain::a2a::push_notification_methods::CreateTaskPushNotificationConfigResponse(
                            config,
                        ),
                    )
                    .map_err(|error| A2aError::InternalError {
                        message: error.to_string(),
                    })
                }
                A2aMethod::GetTaskPushNotificationConfig => {
                    let request: GetTaskPushNotificationConfigRequest =
                        serde_json::from_value(params.unwrap_or(Value::Null)).map_err(|error| {
                            A2aError::InvalidParams {
                                reason: error.to_string(),
                            }
                        })?;
                    let guard = store.lock().expect("store lock");
                    let Some(config) = guard.push_configs.get(&request.id).cloned() else {
                        return Err(A2aError::InvalidParams {
                            reason: format!("no config with id {}", request.id),
                        });
                    };
                    serde_json::to_value(
                        protocol_droid::domain::a2a::push_notification_methods::GetTaskPushNotificationConfigResponse(
                            config,
                        ),
                    )
                    .map_err(|error| A2aError::InternalError {
                        message: error.to_string(),
                    })
                }
                A2aMethod::ListTaskPushNotificationConfigs => {
                    let _request: ListTaskPushNotificationConfigsRequest =
                        serde_json::from_value(params.unwrap_or(Value::Null)).map_err(|error| {
                            A2aError::InvalidParams {
                                reason: error.to_string(),
                            }
                        })?;
                    let configs: Vec<TaskPushNotificationConfig> = store
                        .lock()
                        .expect("store lock")
                        .push_configs
                        .values()
                        .cloned()
                        .collect();
                    serde_json::to_value(
                        protocol_droid::domain::a2a::push_notification_methods::ListTaskPushNotificationConfigsResponse {
                            configs,
                            next_page_token: None,
                        },
                    )
                    .map_err(|error| A2aError::InternalError {
                        message: error.to_string(),
                    })
                }
                A2aMethod::DeleteTaskPushNotificationConfig => {
                    let request: DeleteTaskPushNotificationConfigRequest =
                        serde_json::from_value(params.unwrap_or(Value::Null)).map_err(|error| {
                            A2aError::InvalidParams {
                                reason: error.to_string(),
                            }
                        })?;
                    let mut guard = store.lock().expect("store lock");
                    guard.push_configs.remove(&request.id);
                    Ok(Value::Null)
                }
                A2aMethod::SendStreamingMessage
                | A2aMethod::SubscribeToTask
                | A2aMethod::GetExtendedAgentCard => Err(A2aError::MethodNotFound),
            }
        })
    }
}

impl A2aStreamingRequestHandler for MinimalAgent {
    fn dispatch_stream(
        &self,
        _method: A2aMethod,
        _params: Option<Value>,
        _context: &A2aRequestContext,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamingEvent, A2aError>> + Send>> {
        let events: Vec<Result<StreamingEvent, A2aError>> = vec![
            Ok(StreamingEvent::Status(TaskStatusUpdateEvent {
                task_id: "t-1".to_owned(),
                context_id: String::new(),
                status: TaskStatus {
                    state: TaskState::Working,
                    message: None,
                    timestamp: None,
                },
                is_final: false,
                metadata: std::collections::BTreeMap::new(),
            })),
            Ok(StreamingEvent::Status(TaskStatusUpdateEvent {
                task_id: "t-1".to_owned(),
                context_id: String::new(),
                status: TaskStatus {
                    state: TaskState::Completed,
                    message: None,
                    timestamp: None,
                },
                is_final: true,
                metadata: std::collections::BTreeMap::new(),
            })),
        ];
        Box::pin(stream::iter(events))
    }
}

async fn spawn_test_peer() -> (String, tokio::task::JoinHandle<()>) {
    spawn_test_peer_with_bearer(None).await
}

/// A bearer-token check middleware used by the
/// auth-required test peer. Defined at the
/// module level so the
/// `from_fn_with_state` call in
/// `spawn_test_peer_with_bearer` can name it
/// without putting an `async fn` item after a
/// statement (which clippy flags as
/// "items after statements").
async fn check_bearer_middleware(
    axum::extract::State(expected): axum::extract::State<Arc<str>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let header = request
        .headers()
        .get(reqwest::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    if let Some(value) = header
        && let Some(token) = value.strip_prefix("Bearer ")
        && token == expected.as_ref()
    {
        return next.run(request).await;
    }
    axum::response::Response::builder()
        .status(axum::http::StatusCode::UNAUTHORIZED)
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from("{\"error\":\"unauthorized\"}"))
        .expect("a static 401 response builds")
}

async fn spawn_test_peer_with_bearer(
    required_token: Option<&str>,
) -> (String, tokio::task::JoinHandle<()>) {
    let mut builder = AgentCardBuilder::new()
        .with_name("a2a_client_wire_agent")
        .with_description("A test agent for A2aClient.")
        .with_version("0.1.0")
        .with_interface(AgentInterface {
            url: url::Url::parse("http://127.0.0.1/a2a").expect("valid URL"),
            protocol_binding: JSONRPC.clone(),
            protocol_version: A2aVersion::V1_0,
            tenant: None,
        })
        .with_input_mode("text/plain")
        .with_output_mode("text/plain")
        .with_skill(protocol_droid::domain::a2a::agent_card::AgentSkill {
            id: Arc::from("echo"),
            name: Arc::from("echo"),
            description: Arc::from("Echo the user message back as a reply."),
            tags: Vec::new(),
        });
    if let Some(token) = required_token {
        builder = builder.with_security_scheme(
            "bearer",
            SecurityScheme::Http(HTTPAuthSecurityScheme {
                scheme: Arc::from("bearer"),
                bearer_format: None,
                description: Some(Arc::from(
                    "Bearer token auth. Required for JSON-RPC and streaming; the well-known card is unauthenticated.",
                )),
            }),
        );
        // The token is referenced via the type
        // parameter so the compiler doesn't drop it.
        let _ = token.len();
    }
    let card = builder.build().expect("card builder invariants satisfied");
    let handler: Arc<dyn A2aRequestHandler> = Arc::new(MinimalAgent {
        store: Arc::new(Mutex::new(TaskStore::default())),
    });
    let streaming: Arc<dyn A2aStreamingRequestHandler> = Arc::new(MinimalAgent {
        store: Arc::new(Mutex::new(TaskStore::default())),
    });
    let mut protected = Router::new()
        .merge(serve_jsonrpc("/a2a", handler, A2aVersion::V1_0))
        .merge(serve_streaming("/a2a/stream", streaming, A2aVersion::V1_0));
    if let Some(expected) = required_token {
        let expected = Arc::from(expected);
        protected = protected.route_layer(axum::middleware::from_fn_with_state(
            Arc::clone(&expected),
            check_bearer_middleware,
        ));
    }
    let router = serve_well_known(Arc::new(card)).merge(protected);

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind 127.0.0.1:0");
    let port = listener.local_addr().expect("local_addr").port();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (format!("http://127.0.0.1:{port}"), handle)
}

fn user_message(text: &str) -> Message {
    Message::new(MessageRole::User).with_part(Part::Text(TextPart {
        text: text.to_owned(),
        metadata: std::collections::BTreeMap::new(),
    }))
}

#[tokio::test]
async fn card_fetches_the_well_known_agent_card() -> Result<(), Box<dyn std::error::Error>> {
    let (url, _handle) = spawn_test_peer().await;
    let client = A2aClient::new(url)?;
    let card = client.card().await?;
    assert_eq!(card.name.as_ref(), "a2a_client_wire_agent");
    Ok(())
}

#[tokio::test]
async fn send_message_returns_a_task() -> Result<(), Box<dyn std::error::Error>> {
    let (url, _handle) = spawn_test_peer().await;
    let client = A2aClient::new(url)?;
    let request = SendMessageRequest::new(user_message("hi"));
    let response = client.send_message(request).await?;
    let SendMessageResponse::Task(task) = response else {
        return Err("the minimal agent returns a Task, not a Message".into());
    };
    assert_eq!(task.status.state, TaskState::Completed);
    Ok(())
}

#[tokio::test]
async fn send_then_get_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    let (url, _handle) = spawn_test_peer().await;
    let client = A2aClient::new(url)?;
    let request = SendMessageRequest::new(user_message("hi"));
    let response = client.send_message(request).await?;
    let SendMessageResponse::Task(task) = response else {
        return Err("the minimal agent returns a Task, not a Message".into());
    };
    let task_id = task.id.to_string();
    let get = client.get_task(GetTaskRequest::new(task_id)).await?;
    assert_eq!(get.0.id.as_ref(), "t-1");
    Ok(())
}

#[tokio::test]
async fn cancel_flips_state_to_cancelled() -> Result<(), Box<dyn std::error::Error>> {
    let (url, _handle) = spawn_test_peer().await;
    let client = A2aClient::new(url)?;
    let request = SendMessageRequest::new(user_message("hi"));
    let response = client.send_message(request).await?;
    let SendMessageResponse::Task(task) = response else {
        return Err("the minimal agent returns a Task, not a Message".into());
    };
    let task_id = task.id.to_string();
    let cancelled = client.cancel_task(CancelTaskRequest::new(task_id)).await?;
    assert_eq!(cancelled.0.status.state, TaskState::Cancelled);
    Ok(())
}

#[tokio::test]
async fn list_tasks_returns_what_send_message_wrote() -> Result<(), Box<dyn std::error::Error>> {
    let (url, _handle) = spawn_test_peer().await;
    let client = A2aClient::new(url)?;
    let request = SendMessageRequest::new(user_message("hi"));
    let _ = client.send_message(request).await?;
    let listed = client
        .list_tasks(protocol_droid::domain::a2a::list_tasks::ListTasksRequest {
            context_id: None,
            status: None,
            page_size: None,
            page_token: None,
            history_length: None,
            status_timestamp_after: None,
            metadata: std::collections::BTreeMap::new(),
        })
        .await?;
    assert!(!listed.tasks.is_empty());
    Ok(())
}

#[tokio::test]
async fn push_notification_config_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    let (url, _handle) = spawn_test_peer().await;
    let client = A2aClient::new(url)?;

    let created = client
        .create_push_notification_config(CreateTaskPushNotificationConfigRequest::from(
            TaskPushNotificationConfig {
                id: "placeholder".to_owned(),
                task_id: "t-1".to_owned(),
                url: "http://1.2.3.4:9999/wh".to_owned(),
                token: Some("token".to_owned()),
                authentication: None,
                metadata: std::collections::BTreeMap::new(),
            },
        ))
        .await?;
    let config_id = created.0.id.clone();

    let got = client
        .get_push_notification_config(GetTaskPushNotificationConfigRequest::new(
            config_id.clone(),
            "t-1".to_owned(),
        ))
        .await?;
    let _ = got;
    assert_eq!(got.0.id, config_id);
    assert_eq!(got.0.url, "http://1.2.3.4:9999/wh");

    let listed = client
        .list_push_notification_configs(ListTaskPushNotificationConfigsRequest {
            task_id: "t-1".to_owned(),
            page_size: None,
            page_token: None,
        })
        .await?;
    assert!(!listed.configs.is_empty());

    client
        .delete_push_notification_config(DeleteTaskPushNotificationConfigRequest::new(
            config_id,
            "t-1".to_owned(),
        ))
        .await?;
    Ok(())
}

#[tokio::test]
async fn send_streaming_message_collects_events() -> Result<(), Box<dyn std::error::Error>> {
    let (url, _handle) = spawn_test_peer().await;
    let client = A2aClient::new(url)?;
    let request = SendMessageRequest::new(user_message("hi"));
    let events = client.send_streaming_message(request).await?;
    assert_eq!(events.len(), 2);
    let Some(last) = events.last() else {
        return Err("no last event after a 2-element list".into());
    };
    let StreamingEvent::Status(status) = last else {
        return Err("the last event is a status update".into());
    };
    assert!(status.is_final);
    Ok(())
}

#[tokio::test]
async fn with_paths_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    // A non-default card and JSON-RPC path
    // exercises the second constructor; the
    // test pin is that a custom path is
    // honored, not that the path is
    // non-conventional.
    let (url, _handle) = spawn_test_peer().await;
    let client = A2aClient::with_paths(
        url,
        "/.well-known/agent-card.json",
        "/a2a",
        "/a2a/stream",
    )?;
    let card = client.card().await?;
    assert_eq!(card.name.as_ref(), "a2a_client_wire_agent");
    Ok(())
}

#[tokio::test]
async fn bearer_token_unlocks_an_auth_required_peer() -> Result<(), Box<dyn std::error::Error>> {
    // The peer requires a bearer token; the
    // client attaches it via
    // `with_bearer_token`; the typed
    // `send_message` round-trip succeeds.
    let (url, _handle) = spawn_test_peer_with_bearer(Some("top-secret")).await;
    let client = A2aClient::new(url)?.with_bearer_token("top-secret");
    let response = client
        .send_message(SendMessageRequest::new(user_message("hi")))
        .await?;
    let SendMessageResponse::Task(task) = response else {
        return Err("the minimal agent returns a Task, not a Message".into());
    };
    assert_eq!(task.status.state, TaskState::Completed);
    Ok(())
}

#[tokio::test]
async fn missing_bearer_token_is_a_401() -> Result<(), Box<dyn std::error::Error>> {
    // The peer requires a bearer token; the
    // client has none. The wire-level
    // assertion is that the request fails
    // — the typed `send_message` surfaces
    // the status as `A2aClientError::Io`
    // because `reqwest::Response::error_for_status`
    // is what runs first.
    let (url, _handle) = spawn_test_peer_with_bearer(Some("top-secret")).await;
    let client = A2aClient::new(url)?;
    let result = client
        .send_message(SendMessageRequest::new(user_message("hi")))
        .await;
    // The exact error variant is a transport
    // failure on the status code (4xx is
    // surfaced by `error_for_status`); the
    // shape of the failure is the right
    // thing to pin, not its `Display` text.
    match result {
        Ok(_) => {
            return Err(
                "a missing token must be a wire-level rejection".into(),
            );
        }
        Err(error) => {
            let is_transport = matches!(
                error,
                protocol_droid::adapters::secondary::a2a::A2aClientError::Io(_)
            ) || matches!(
                error,
                protocol_droid::adapters::secondary::a2a::A2aClientError::JsonRpc { .. }
            );
            if !is_transport {
                return Err(format!("missing bearer should be a 401, got {error}").into());
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn the_well_known_card_is_unauthenticated_even_when_bearer_is_required(
) -> Result<(), Box<dyn std::error::Error>> {
    // A2A v1.0 §8 names the well-known card
    // as the discovery endpoint a peer reads
    // to learn the security scheme, so a
    // server that requires a bearer MUST
    // still answer the well-known request
    // without one — otherwise a peer that
    // knows nothing about the server has no
    // way to learn what to send. The
    // example's `a2a_server` and the wire
    // test peer in
    // `spawn_test_peer_with_bearer` both
    // claim this in their doc comments; the
    // wire-level assertion here is the
    // truth test.
    let (url, _handle) = spawn_test_peer_with_bearer(Some("top-secret")).await;
    let peer = url;
    let response = reqwest::get(format!("{peer}/.well-known/agent-card.json")).await?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(format!(
            "the well-known card is the discovery endpoint; an unauthenticated GET must \
             return 200 so a peer can learn the bearer scheme, got {}",
            response.status()
        )
        .into());
    }
    Ok(())
}

#[tokio::test]
async fn wrong_bearer_token_is_a_401() -> Result<(), Box<dyn std::error::Error>> {
    // The peer requires a specific token;
    // the client sends a different one.
    // The wire-level assertion is that the
    // request is rejected.
    let (url, _handle) = spawn_test_peer_with_bearer(Some("top-secret")).await;
    let client = A2aClient::new(url)?.with_bearer_token("not-the-secret");
    let result = client
        .send_message(SendMessageRequest::new(user_message("hi")))
        .await;
    match result {
        Ok(_) => return Err("a wrong token must be a wire-level rejection".into()),
        Err(error) => {
            let is_transport = matches!(
                error,
                protocol_droid::adapters::secondary::a2a::A2aClientError::Io(_)
            ) || matches!(
                error,
                protocol_droid::adapters::secondary::a2a::A2aClientError::JsonRpc { .. }
            );
            if !is_transport {
                return Err(format!("wrong bearer should be a 401, got {error}").into());
            }
        }
    }
    Ok(())
}
