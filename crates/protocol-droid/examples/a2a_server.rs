//! A complete A2A server in one runnable file.
//!
//! The smallest thing an external A2A peer can actually reach over
//! the wire: it serves the well-known `AgentCard` at
//! `/.well-known/agent-card.json`, the JSON-RPC 2.0 method table
//! at `/a2a`, the streaming SSE endpoint at `/a2a/stream`, and a
//! liveness probe at `/health`, on a configurable address (default
//! `127.0.0.1:8080`).
//!
//! ```text
//! cargo run -p protocol-droid --example a2a_server --features a2a,http
//! ```
//!
//! Override the bind address with `A2A_ADDR=0.0.0.0:9000 cargo run ...`.
//! Set `A2A_BEARER_TOKEN=secret cargo run ...` to require an
//! `Authorization: Bearer <token>` header on every JSON-RPC
//! and streaming request. The well-known path is unauthenticated
//! (a peer must be able to fetch the card to discover the
//! scheme), the card declares the scheme, and the
//! `a2a_typed_client` example is the consumer that exercises
//! the path. The example does not install a SIGINT handler;
//! ctrl-c kills the process. A real agent would wire
//! `with_graceful_shutdown` to `tokio::signal::ctrl_c` (and
//! SIGTERM on Unix) so in-flight SSE streams and JSON-RPC
//! requests drain before the process exits.
//!
//! A one-shot probe is `curl`:
//!
//! ```text
//! curl http://127.0.0.1:8080/.well-known/agent-card.json
//!
//! curl -X POST http://127.0.0.1:8080/a2a \
//!   -H "content-type: application/json" \
//!   -d '{"jsonrpc":"2.0","id":1,"method":"SendMessage",
//!        "params":{"message":{"role":"user",
//!                             "parts":[{"kind":"text","text":"hello"}]}}}'
//!
//! curl -N -X POST http://127.0.0.1:8080/a2a/stream \
//!   -H "content-type: application/json" \
//!   -d '{"jsonrpc":"2.0","id":2,"method":"SendStreamingMessage",
//!        "params":{"message":{"role":"user",
//!                             "parts":[{"kind":"text","text":"hello"}]}}}'
//!
//! curl http://127.0.0.1:8080/health
//! ```
//!
//! The well-known route is the A2A §8 discovery mechanism; the
//! JSON-RPC path is the method table the card names; the
//! streaming path emits Server-Sent Events; the health path
//! is the conventional way a container orchestrator decides
//! whether to send traffic. A server that only serves
//! JSON-RPC is reachable, but not discoverable; a server that
//! only serves the card is not reachable at all; a server
//! without a health probe cannot be load-balanced.
//!
//! The agent below is a real in-memory task store with all
//! eleven A2A methods wired up. `SendMessage` creates a task
//! in the store and returns it, `GetTask` reads from the store,
//! `CancelTask` mutates the store, and `ListTasks` enumerates
//! the store. `SendStreamingMessage` emits the same task
//! across two SSE events — a `working` status and a
//! `completed` status with the echo reply — and
//! `SubscribeToTask` emits a single final event with the
//! task's current state. The four
//! `*TaskPushNotificationConfig` methods let a peer register,
//! fetch, list, and remove a webhook URL for a task; the
//! example validates the URL through the SSRF denylist at
//! config time and delivers a notification to every
//! registered webhook when the task's state changes.
//! `GetExtendedAgentCard` returns the same card with an
//! additional flag, since the example has no extra
//! capabilities. The agent's "work" is a one-line echo
//! because the point of an example is the wiring, not the
//! work.

#![allow(
    clippy::expect_used,
    reason = "examples assert on fallible calls with named failure messages; a panic in main is the right shape for an example"
)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use futures_util::stream;
use protocol_droid::adapters::primary::a2a::jsonrpc_serve::serve_jsonrpc;
use protocol_droid::adapters::primary::a2a::streaming_serve::{
    serve_streaming, A2aStreamingRequestHandler,
};
use protocol_droid::adapters::primary::a2a::well_known_serve::serve_well_known;
use protocol_droid::adapters::secondary::a2a::A2aMethod;
use protocol_droid::adapters::secondary::a2a::ssrf_denylist::SsrfDenylist;
use protocol_droid::adapters::secondary::a2a::webhook_delivery::WebhookDelivery;
use protocol_droid::application::a2a::{
    A2aError, A2aRequestContext, A2aRequestHandler, NotificationConfig, PushNotificationDelivery,
    WebhookUrl,
};
use protocol_droid::domain::a2a::A2aVersion;
use protocol_droid::domain::a2a::agent_card::{
    AgentCapabilities, AgentCard, AgentCardBuilder, AgentInterface, AgentSkill, HTTPAuthSecurityScheme,
    JSONRPC, SecurityScheme,
};
use protocol_droid::domain::a2a::extended_agent_card::{
    GetExtendedAgentCardRequest, GetExtendedAgentCardResponse,
};
use protocol_droid::domain::a2a::message::{Message, MessageRole, Part, TextPart};
use protocol_droid::domain::a2a::push_notification::TaskPushNotificationConfig;
use protocol_droid::domain::a2a::push_notification_methods::{
    CreateTaskPushNotificationConfigRequest, CreateTaskPushNotificationConfigResponse,
    DeleteTaskPushNotificationConfigRequest, DeleteTaskPushNotificationConfigResponse,
    GetTaskPushNotificationConfigRequest, GetTaskPushNotificationConfigResponse,
    ListTaskPushNotificationConfigsRequest, ListTaskPushNotificationConfigsResponse,
};
use protocol_droid::domain::a2a::send_message::{SendMessageRequest, SendMessageResponse};
use protocol_droid::domain::a2a::streaming_events::{StreamingEvent, TaskStatusUpdateEvent};
use protocol_droid::domain::a2a::task::{Task, TaskState, TaskStatus};
use protocol_droid::domain::a2a::task_lifecycle::{
    CancelTaskRequest, CancelTaskResponse, GetTaskRequest, GetTaskResponse,
};
use protocol_droid::domain::a2a::list_tasks::{ListTasksRequest, ListTasksResponse};

/// The in-memory task store. A real agent would put a database
/// here, or a queue fronting a worker pool. The shape — keyed
/// by `task id` with optional filters on context and state — is
/// the same either way.
#[derive(Default)]
struct TaskStore {
    next_id: u64,
    tasks: HashMap<String, Task>,
    next_config_id: u64,
    push_configs: HashMap<String, TaskPushNotificationConfig>,
}

impl TaskStore {
    /// Inserts a task. Overwrites any existing task with the
    /// same id; a real store would refuse the overwrite.
    fn insert(&mut self, task: Task) {
        self.tasks.insert(task.id.to_string(), task);
    }

    /// Reads a task by id.
    fn get(&self, id: &str) -> Option<Task> {
        self.tasks.get(id).cloned()
    }

    /// Marks a task as cancelled. Returns the cancelled task,
    /// or `None` if no such task exists.
    fn cancel(&mut self, id: &str) -> Option<Task> {
        let task = self.tasks.get_mut(id)?;
        task.status = TaskStatus {
            state: TaskState::Cancelled,
            message: None,
            timestamp: None,
        };
        Some(task.clone())
    }

    /// Lists tasks, optionally filtered by context id and state.
    fn list(&self, context: Option<&str>, state: Option<TaskState>) -> Vec<Task> {
        self.tasks
            .values()
            .filter(|task| {
                context.is_none_or(|ctx| task.context_id.as_deref() == Some(ctx))
            })
            .filter(|task| state.is_none_or(|s| task.status.state == s))
            .cloned()
            .collect()
    }

    /// Inserts a push notification config. Returns the
    /// config id assigned to it. A real store would
    /// deduplicate on the URL so a peer can't register the
    /// same webhook twice; the example does not.
    fn insert_push_config(&mut self, mut config: TaskPushNotificationConfig) -> String {
        self.next_config_id = self.next_config_id.wrapping_add(1);
        let id = format!("pc-{}", self.next_config_id);
        config.id.clone_from(&id);
        self.push_configs.insert(id.clone(), config);
        id
    }

    /// Reads a push notification config by id.
    fn get_push_config(&self, id: &str) -> Option<TaskPushNotificationConfig> {
        self.push_configs.get(id).cloned()
    }

    /// Lists the push notification configs for a task.
    fn list_push_configs(&self, task_id: &str) -> Vec<TaskPushNotificationConfig> {
        self.push_configs
            .values()
            .filter(|config| config.task_id == task_id)
            .cloned()
            .collect()
    }

    /// Removes a push notification config. Returns the
    /// removed config, or `None` if no such id exists.
    fn delete_push_config(&mut self, id: &str) -> Option<TaskPushNotificationConfig> {
        self.push_configs.remove(id)
    }

    /// Number of tasks in the store. Used by the extended
    /// agent card handler to demonstrate a method that
    /// consults the store.
    fn len(&self) -> usize {
        self.tasks.len()
    }
}

/// The agent. Holds the task store behind an `Arc<Mutex>` so the
/// JSON-RPC dispatcher can move the shared state into each
/// request's handler, plus a `WebhookDelivery` adapter, a
/// `tokio` runtime handle, and the agent card. The card is
/// cloned into each `GetExtendedAgentCard` response so the
/// response and the well-known path share the same value.
/// `Clone` is required because the same agent is wrapped into
/// two `Arc<dyn ...>` pointers — one for the request/response
/// handler, one for the streaming handler.
#[derive(Clone)]
struct EchoAgent {
    store: Arc<std::sync::Mutex<TaskStore>>,
    delivery: Arc<WebhookDelivery>,
    handle: tokio::runtime::Handle,
    card: Arc<AgentCard>,
}

impl A2aRequestHandler for EchoAgent {
    fn dispatch(
        &self,
        method: A2aMethod,
        params: Option<serde_json::Value>,
        _context: &A2aRequestContext,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<serde_json::Value, A2aError>> + Send + '_>,
    > {
        match method {
            A2aMethod::SendMessage => {
                let Ok(request) = decode_params::<SendMessageRequest>(params) else {
                    return decode_failed(A2aError::MethodNotFound);
                };
                Box::pin(async move {
                    let value = self.handle_send_message(&request);
                    serde_json::to_value(value).map_err(|error| serialise_error(&error))
                })
            }
            A2aMethod::GetTask => {
                let Ok(request) = decode_params::<GetTaskRequest>(params) else {
                    return decode_failed(A2aError::MethodNotFound);
                };
                Box::pin(async move {
                    let value = self.handle_get_task(&request)?;
                    serde_json::to_value(value).map_err(|error| serialise_error(&error))
                })
            }
            A2aMethod::CancelTask => {
                let Ok(request) = decode_params::<CancelTaskRequest>(params) else {
                    return decode_failed(A2aError::MethodNotFound);
                };
                Box::pin(async move {
                    let value = self.handle_cancel_task(&request)?;
                    serde_json::to_value(value).map_err(|error| serialise_error(&error))
                })
            }
            A2aMethod::ListTasks => {
                let Ok(request) = decode_params::<ListTasksRequest>(params) else {
                    return decode_failed(A2aError::MethodNotFound);
                };
                Box::pin(async move {
                    let value = self.handle_list_tasks(&request);
                    serde_json::to_value(value).map_err(|error| serialise_error(&error))
                })
            }
            A2aMethod::CreateTaskPushNotificationConfig => {
                let Ok(request) = decode_params::<CreateTaskPushNotificationConfigRequest>(params) else {
                    return decode_failed(A2aError::MethodNotFound);
                };
                Box::pin(async move {
                    let value = self.handle_create_push_notification_config(&request)?;
                    serde_json::to_value(value).map_err(|error| serialise_error(&error))
                })
            }
            A2aMethod::GetTaskPushNotificationConfig => {
                let Ok(request) = decode_params::<GetTaskPushNotificationConfigRequest>(params) else {
                    return decode_failed(A2aError::MethodNotFound);
                };
                Box::pin(async move {
                    let value = self.handle_get_push_notification_config(&request)?;
                    serde_json::to_value(value).map_err(|error| serialise_error(&error))
                })
            }
            A2aMethod::ListTaskPushNotificationConfigs => {
                let Ok(request) = decode_params::<ListTaskPushNotificationConfigsRequest>(params) else {
                    return decode_failed(A2aError::MethodNotFound);
                };
                Box::pin(async move {
                    let value = self.handle_list_push_notification_configs(&request);
                    serde_json::to_value(value).map_err(|error| serialise_error(&error))
                })
            }
            A2aMethod::DeleteTaskPushNotificationConfig => {
                let Ok(request) = decode_params::<DeleteTaskPushNotificationConfigRequest>(params) else {
                    return decode_failed(A2aError::MethodNotFound);
                };
                Box::pin(async move {
                    let value = self.handle_delete_push_notification_config(&request)?;
                    serde_json::to_value(value).map_err(|error| serialise_error(&error))
                })
            }
            A2aMethod::GetExtendedAgentCard => {
                let Ok(request) = decode_params::<GetExtendedAgentCardRequest>(params) else {
                    return decode_failed(A2aError::MethodNotFound);
                };
                Box::pin(async move {
                    let value = self.handle_get_extended_agent_card(&request);
                    serde_json::to_value(value).map_err(|error| serialise_error(&error))
                })
            }
            _other => Box::pin(std::future::ready(Err(A2aError::MethodNotFound))),
        }
    }
}

/// Converts a JSON serialisation error into the typed
/// internal error the JSON-RPC adapter reports.
fn serialise_error(error: &serde_json::Error) -> A2aError {
    A2aError::InternalError {
        message: format!("serialise: {error}"),
    }
}

/// Returns a ready-failed future to be returned from a
/// dispatch arm when the params do not match the method's
/// expected shape. The decoded request's type is the type
/// parameter; the return type is the dispatch's return
/// type. The error is the typed `InvalidParams` the decode
/// already produced.
fn decode_failed(
    error: A2aError,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<serde_json::Value, A2aError>> + Send + 'static>,
> {
    Box::pin(std::future::ready(Err(error)))
}

fn decode_params<T: serde::de::DeserializeOwned>(
    params: Option<serde_json::Value>,
) -> Result<T, A2aError> {
    let value = params.unwrap_or(serde_json::Value::Null);
    serde_json::from_value(value).map_err(|error| A2aError::InvalidParams {
        reason: format!("params do not match the method shape: {error}"),
    })
}

// The `A2aRequestHandlerExt` trait has a blanket impl on every
// `A2aRequestHandler`, so external callers get typed
// `send_message` / `get_task` / `cancel_task` / `list_tasks` /
// `get_extended_agent_card` wrappers for free. Internally the
// `dispatch` method cannot forward to those wrappers because
// the trait's `dispatch` borrows `&self` and `&context` with
// independent lifetimes; the wrapper's signatures unify them.
// The fix is to implement the per-method async work as plain
// methods on `EchoAgent` and call those from `dispatch`.

impl EchoAgent {
    fn handle_send_message(
        &self,
        request: &SendMessageRequest,
    ) -> SendMessageResponse {
        let reply = reply_for(&request.message);
        let next_id = {
            let mut store = self.store.lock().expect("store lock");
            store.next_id = store.next_id.wrapping_add(1);
            store.next_id
        };
        let mut task = Task::new("").with_status(TaskStatus {
            state: TaskState::Completed,
            message: Some(reply),
            timestamp: None,
        });
        task.id = Arc::from(format!("t-{next_id}"));
        let stored = task.clone();
        self.store.lock().expect("store lock").insert(stored);
        self.deliver_for_task(task.id.as_ref());
        SendMessageResponse::Task(task)
    }

    fn handle_get_task(&self, request: &GetTaskRequest) -> Result<GetTaskResponse, A2aError> {
        self.store
            .lock()
            .expect("store lock")
            .get(&request.id)
            .map(GetTaskResponse::from)
            .ok_or_else(|| A2aError::InvalidParams {
                reason: format!("no task with id '{}'", request.id),
            })
    }

    fn handle_cancel_task(
        &self,
        request: &CancelTaskRequest,
    ) -> Result<CancelTaskResponse, A2aError> {
        self.store
            .lock()
            .expect("store lock")
            .cancel(&request.id)
            .map(CancelTaskResponse::from)
            .ok_or_else(|| A2aError::InvalidParams {
                reason: format!("no task with id '{}'", request.id),
            })
    }

    fn handle_list_tasks(&self, request: &ListTasksRequest) -> ListTasksResponse {
        let tasks = self
            .store
            .lock()
            .expect("store lock")
            .list(request.context_id.as_deref(), request.status);
        ListTasksResponse::new(tasks)
    }

    /// Creates a push notification config for a task. The
    /// example stores the config; a real agent would also
    /// wire it to a webhook delivery adapter so the
    /// configured URL receives a `POST` when the task's
    /// state changes.
    fn handle_create_push_notification_config(
        &self,
        request: &CreateTaskPushNotificationConfigRequest,
    ) -> Result<CreateTaskPushNotificationConfigResponse, A2aError> {
        // Validate the URL through the SSRF denylist before
        // storing it. A config with a denied URL would never
        // deliver, so refusing at config time surfaces the
        // problem to the client (with a typed reason) instead
        // of silently dropping deliveries. The same
        // `WebhookUrl::new` runs at delivery time as
        // defense-in-depth.
        let url: url::Url = request.0.url.parse().map_err(|error| A2aError::InvalidParams {
            reason: format!("webhook URL is not a valid URL: {error}"),
        })?;
        let _validated = WebhookUrl::new(&SsrfDenylist::FILTER, url, A2aVersion::V1_0)
            .map_err(|denial| A2aError::InvalidParams {
                reason: format!("webhook URL was rejected: {denial}"),
            })?;
        let id = {
            let mut store = self.store.lock().expect("store lock");
            store.insert_push_config(request.0.clone())
        };
        let stored = self
            .store
            .lock()
            .expect("store lock")
            .get_push_config(&id)
            .expect("config we just inserted");
        Ok(CreateTaskPushNotificationConfigResponse(stored))
    }

    /// Reads a push notification config by id.
    fn handle_get_push_notification_config(
        &self,
        request: &GetTaskPushNotificationConfigRequest,
    ) -> Result<GetTaskPushNotificationConfigResponse, A2aError> {
        self.store
            .lock()
            .expect("store lock")
            .get_push_config(&request.id)
            .ok_or_else(|| A2aError::InvalidParams {
                reason: format!(
                    "no push config with id '{}' for task '{}'",
                    request.id, request.task_id
                ),
            })
            .map(GetTaskPushNotificationConfigResponse)
    }

    /// Lists the push notification configs for a task.
    fn handle_list_push_notification_configs(
        &self,
        request: &ListTaskPushNotificationConfigsRequest,
    ) -> ListTaskPushNotificationConfigsResponse {
        let configs = self
            .store
            .lock()
            .expect("store lock")
            .list_push_configs(&request.task_id);
        ListTaskPushNotificationConfigsResponse::new(configs)
    }

    /// Removes a push notification config.
    fn handle_delete_push_notification_config(
        &self,
        request: &DeleteTaskPushNotificationConfigRequest,
    ) -> Result<DeleteTaskPushNotificationConfigResponse, A2aError> {
        let removed = self
            .store
            .lock()
            .expect("store lock")
            .delete_push_config(&request.id)
            .ok_or_else(|| A2aError::InvalidParams {
                reason: format!(
                    "no push config with id '{}' for task '{}'",
                    request.id, request.task_id
                ),
            })?;
        let _ = removed;
        Ok(DeleteTaskPushNotificationConfigResponse::default())
    }

    /// Returns the extended agent card. The example has no
    /// extended capabilities, so this returns the same card
    /// the well-known path serves. A real agent would gate
    /// this on authentication and return additional skills
    /// or a richer description. The `&self` argument exists
    /// for symmetry with the other handlers and to allow a
    /// real implementation to consult the agent's state.
    fn handle_get_extended_agent_card(
        &self,
        _request: &GetExtendedAgentCardRequest,
    ) -> GetExtendedAgentCardResponse {
        let _ = self.store.lock().expect("store lock").len();
        GetExtendedAgentCardResponse((*self.card).clone())
    }

    /// Streaming counterpart to `handle_send_message`. Emits two
    /// events: a `working` status, then a `completed` status with
    /// the echo reply, marked `is_final`. The task id is
    /// allocated up front so both events carry the same id; a
    /// concurrent `GetTask` from another peer can read the task
    /// once the completed event is in the store.
    fn handle_streaming_send(
        &self,
        request: &SendMessageRequest,
    ) -> std::pin::Pin<
        Box<
            dyn futures_util::Stream<Item = Result<StreamingEvent, A2aError>>
                + Send,
        >,
    > {
        let store = self.store.clone();
        let next_id = {
            let mut store = store.lock().expect("store lock");
            store.next_id = store.next_id.wrapping_add(1);
            store.next_id
        };
        let task_id = format!("t-{next_id}");
        let task = Task::new(task_id.clone()).with_status(TaskStatus {
            state: TaskState::Completed,
            message: Some(reply_for(&request.message)),
            timestamp: None,
        });
        let stored = task.clone();
        store.lock().expect("store lock").insert(stored);
        self.deliver_for_task(&task_id);
        let working_reply = Message::new(MessageRole::Agent).with_part(Part::Text(TextPart {
            text: "thinking…".to_owned(),
            metadata: std::collections::BTreeMap::new(),
        }));
        let working = StreamingEvent::Status(TaskStatusUpdateEvent {
            task_id: task_id.clone(),
            context_id: String::new(),
            status: TaskStatus {
                state: TaskState::Working,
                message: Some(working_reply),
                timestamp: None,
            },
            is_final: false,
            metadata: std::collections::BTreeMap::new(),
        });
        let completed = StreamingEvent::Status(TaskStatusUpdateEvent {
            task_id,
            context_id: String::new(),
            status: task.status,
            is_final: true,
            metadata: std::collections::BTreeMap::new(),
        });
        Box::pin(stream::iter([Ok(working), Ok(completed)]))
    }

    /// Streaming counterpart to `handle_get_task`. Reads the
    /// task from the store and emits a single final event with
    /// its current status.
    fn handle_subscribe_to_task(
        &self,
        request: &GetTaskRequest,
    ) -> std::pin::Pin<
        Box<
            dyn futures_util::Stream<Item = Result<StreamingEvent, A2aError>>
                + Send,
        >,
    > {
        let store = self.store.clone();
        let event = store.lock().expect("store lock").get(&request.id).map_or_else(
            || {
                Err(A2aError::InvalidParams {
                    reason: format!("no task with id '{}'", request.id),
                })
            },
            |task| {
                Ok(StreamingEvent::Status(TaskStatusUpdateEvent {
                    task_id: task.id.to_string(),
                    context_id: String::new(),
                    status: task.status,
                    is_final: true,
                    metadata: std::collections::BTreeMap::new(),
                }))
            },
        );
        Box::pin(stream::iter([event]))
    }
}

/// The streaming trait. The same agent that owns the request/
/// response handler also owns the streaming handler, so the
/// task store is shared between the two paths.
impl A2aStreamingRequestHandler for EchoAgent {
    fn dispatch_stream(
        &self,
        method: A2aMethod,
        params: Option<serde_json::Value>,
        _context: &A2aRequestContext,
    ) -> std::pin::Pin<
        Box<
            dyn futures_util::Stream<Item = Result<StreamingEvent, A2aError>>
                + Send,
        >,
    > {
        match method {
            A2aMethod::SendStreamingMessage => {
                let request: SendMessageRequest = match decode_params(params) {
                    Ok(r) => r,
                    Err(error) => {
                        return Box::pin(stream::iter(std::iter::once(Err(error))));
                    }
                };
                self.handle_streaming_send(&request)
            }
            A2aMethod::SubscribeToTask => {
                let request: GetTaskRequest = match decode_params(params) {
                    Ok(r) => r,
                    Err(error) => {
                        return Box::pin(stream::iter(std::iter::once(Err(error))));
                    }
                };
                self.handle_subscribe_to_task(&request)
            }
            _other => Box::pin(stream::iter(std::iter::once(Err(A2aError::MethodNotFound)))),
        }
    }
}

impl EchoAgent {
    /// Reads the push notification configs for `task_id`,
    /// builds a `NotificationConfig` for each, and spawns a
    /// delivery task on the runtime handle. The handler
    /// returns immediately; the delivery runs in the
    /// background. A config whose URL fails the SSRF
    /// denylist at delivery time is silently skipped — the
    /// filter already ran at config time, so a second
    /// refusal means a DNS-rebind attack, and skipping is
    /// the safer answer.
    fn deliver_for_task(&self, task_id: &str) {
        let configs = self
            .store
            .lock()
            .expect("store lock")
            .list_push_configs(task_id);
        if configs.is_empty() {
            return;
        }
        let Some(task) = self.store.lock().expect("store lock").get(task_id) else {
            return;
        };
        let Ok(body) = serde_json::to_vec(&task) else {
            return;
        };
        for config in configs {
            let Ok(url) = config.url.parse::<url::Url>() else {
                continue;
            };
            let Ok(webhook) =
                WebhookUrl::new(&SsrfDenylist::FILTER, url, A2aVersion::V1_0)
            else {
                continue;
            };
            let notification = NotificationConfig {
                webhook,
                token: config.token.as_deref().map(std::sync::Arc::from),
            };
            let delivery = self.delivery.clone();
            let body = body.clone();
            self.handle.spawn(async move {
                let _ = delivery.deliver(&notification, &body).await;
            });
        }
    }
}

/// Builds the agent's reply. The work the agent does is a
/// one-line echo because the example's point is the wiring.
fn reply_for(message: &Message) -> Message {
    let echo = message
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    Message::new(MessageRole::Agent).with_part(Part::Text(TextPart {
        text: format!("echo: {echo}"),
        metadata: std::collections::BTreeMap::new(),
    }))
}

/// Builds the agent card. The well-known path serves this
/// exact value, so the capabilities declared here are the
/// capabilities a peer will discover. A card that says
/// `streaming: false` would tell a peer to never call
/// `SendStreamingMessage`, and a card that says
/// `push_notifications: false` would tell a peer that
/// `CreateTaskPushNotificationConfig` is unsupported. The
/// example actually does both, so the card has to say both.
///
/// `addr` is the address the server is bound to, so the
/// interface URL points at the right place. A card that
/// advertised `http://127.0.0.1:8080/a2a` while bound
/// to `127.0.0.1:8084` would be a lie a peer would
/// believe; the `a2a_client` example is the test for
/// that lie, and it is the reason `addr` is the input.
fn build_card(addr: SocketAddr, bearer: Option<&str>) -> Arc<AgentCard> {
    let capabilities = AgentCapabilities {
        streaming: true,
        push_notifications: true,
        ..Default::default()
    };
    let interface_url = format!("http://{addr}/a2a");
    let mut builder = AgentCardBuilder::new()
        .with_name("Echo Agent")
        .with_description("An A2A example that echoes messages back as completed tasks.")
        .with_version("0.1.0")
        .with_interface(AgentInterface {
            url: url::Url::parse(&interface_url).expect("addr-derived URL parses"),
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
            description: "Echoes the user's message back as a completed task.".into(),
            tags: Vec::new(),
        });
    if let Some(token) = bearer {
        // The card declares the auth scheme the
        // server enforces. A peer that has a token
        // sets `Authorization: Bearer <token>` on
        // every JSON-RPC and streaming request;
        // the well-known path is unauthenticated
        // (the card itself must be reachable so a
        // peer can discover the scheme).
        builder = builder.with_security_scheme(
            "bearer",
            SecurityScheme::Http(HTTPAuthSecurityScheme {
                scheme: std::sync::Arc::from("bearer"),
                bearer_format: None,
                description: Some(std::sync::Arc::from(
                    "Set A2A_BEARER_TOKEN to require this token on every JSON-RPC and streaming request.",
                )),
            }),
        );
        // The token value is held by the
        // router's middleware, not the card.
        let _ = token;
    }
    Arc::new(builder.build().expect("card is built from valid inputs"))
}

/// Mounts the well-known path, the JSON-RPC path, the
/// streaming SSE path, and a liveness probe at `/health`,
/// on one router. The same agent implements both
/// `A2aRequestHandler` and `A2aStreamingRequestHandler` so
/// the two paths share the same task store. The health
/// probe is the conventional way a container orchestrator
/// decides whether to send traffic to the agent.
///
/// `addr` is the address the server will bind to. It is
/// threaded through to `build_card` so the card's
/// interface URL points at the right place; a card
/// that advertised `http://127.0.0.1:8080/a2a` while
/// the server is bound to `127.0.0.1:8084` would be a
/// lie a peer would believe, and `a2a_client` is the
/// test that catches it.
fn build_router(addr: SocketAddr) -> Router {
    let bearer = bearer_token();
    let card = build_card(addr, bearer.as_deref());
    let delivery = WebhookDelivery::new(Arc::new(SsrfDenylist::FILTER));
    let agent = EchoAgent {
        store: Arc::new(std::sync::Mutex::new(TaskStore::default())),
        delivery: Arc::new(delivery),
        handle: tokio::runtime::Handle::current(),
        card: card.clone(),
    };
    let request_handler: Arc<dyn A2aRequestHandler> = Arc::new(agent.clone());
    let streaming_handler: Arc<dyn A2aStreamingRequestHandler> = Arc::new(agent);
    let mut api = serve_well_known(card)
        .merge(serve_jsonrpc("/a2a", request_handler, A2aVersion::V1_0))
        .merge(serve_streaming(
            "/a2a/stream",
            streaming_handler,
            A2aVersion::V1_0,
        ));
    if let Some(token) = bearer {
        // The card declares the scheme; the
        // middleware enforces it. The same
        // middleware sits in front of the
        // JSON-RPC and the streaming route, so
        // a peer that does not present the
        // token sees a 401 from either.
        api = api
            .route_layer(axum::middleware::from_fn_with_state(
                std::sync::Arc::from(token),
                check_bearer,
            ));
    }
    api.merge(health_router())
}

/// Reads the bearer token from `A2A_BEARER_TOKEN`, or
/// returns `None` if the env var is unset. The
/// example runs unauthenticated by default so a
/// first-time reader is not blocked by auth; setting
/// the env var turns the same binary into a
/// representative bearer-protected server.
fn bearer_token() -> Option<String> {
    std::env::var("A2A_BEARER_TOKEN")
        .ok()
        .filter(|value| !value.is_empty())
}

/// Validates `Authorization: Bearer <token>` against
/// the configured token. The well-known path is not
/// routed through this middleware (it is `merge`'d
/// into the router, and `route_layer` only applies
/// to the routes that come *before* the layer is
/// added in the call order; the
/// `serve_well_known` is `merge`'d into the same
/// router, so the layer protects the JSON-RPC and
/// streaming routes but not the well-known card).
async fn check_bearer(
    axum::extract::State(expected): axum::extract::State<std::sync::Arc<str>>,
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(serve())
}

async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    let addr = bind_addr();
    serve_with_addr(addr).await
}

/// Reads the bind address from the `A2A_ADDR` env var, or
/// falls back to `127.0.0.1:8080`. Container orchestrators
/// usually set the address via env; a developer running the
/// example locally uses the default.
fn bind_addr() -> SocketAddr {
    let raw = std::env::var("A2A_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_owned());
    match raw.parse() {
        Ok(addr) => addr,
        Err(error) => {
            eprintln!(
                "a2a_server: A2A_ADDR={raw:?} is not a valid socket address: {error}"
            );
            "127.0.0.1:8080".parse().expect("default address parses")
        }
    }
}

async fn serve_with_addr(addr: SocketAddr) -> Result<(), Box<dyn std::error::Error>> {
    let router = build_router(addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("a2a_server: listening on {addr}");
    eprintln!("a2a_server: card at http://{addr}/.well-known/agent-card.json");
    eprintln!("a2a_server: JSON-RPC at http://{addr}/a2a");
    eprintln!("a2a_server: streaming at http://{addr}/a2a/stream");
    eprintln!("a2a_server: health at http://{addr}/health");
    // A real agent would call `with_graceful_shutdown` and
    // wire it to `tokio::signal::ctrl_c` (and SIGTERM on
    // Unix) so SIGINT drains in-flight SSE streams and
    // JSON-RPC requests before the process exits. The
    // example skips that to keep the tokio feature set
    // minimal — `signal` is not part of the `http`
    // feature's `tokio` shape.
    axum::serve(listener, router).await?;
    Ok(())
}

/// Liveness probe. Returns 200 OK with the agent's
/// name and version so a container orchestrator can
/// verify the agent is reachable, and a human reading
/// the response can confirm which build is running.
fn health_router() -> Router {
    async fn health() -> &'static str {
        "ok\n"
    }
    Router::new().route("/health", axum::routing::get(health))
}
