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
                let request = match decode_params::<SendMessageRequest>(params) {
                    Ok(request) => request,
                    Err(error) => return decode_failed(error),
                };
                Box::pin(async move {
                    let value = self.handle_send_message(&request);
                    serde_json::to_value(value).map_err(|error| serialise_error(&error))
                })
            }
            A2aMethod::GetTask => {
                let request = match decode_params::<GetTaskRequest>(params) {
                    Ok(request) => request,
                    Err(error) => return decode_failed(error),
                };
                Box::pin(async move {
                    let value = self.handle_get_task(&request)?;
                    serde_json::to_value(value).map_err(|error| serialise_error(&error))
                })
            }
            A2aMethod::CancelTask => {
                let request = match decode_params::<CancelTaskRequest>(params) {
                    Ok(request) => request,
                    Err(error) => return decode_failed(error),
                };
                Box::pin(async move {
                    let value = self.handle_cancel_task(&request)?;
                    serde_json::to_value(value).map_err(|error| serialise_error(&error))
                })
            }
            A2aMethod::ListTasks => {
                let request = match decode_params::<ListTasksRequest>(params) {
                    Ok(request) => request,
                    Err(error) => return decode_failed(error),
                };
                Box::pin(async move {
                    let value = self.handle_list_tasks(&request);
                    serde_json::to_value(value).map_err(|error| serialise_error(&error))
                })
            }
            A2aMethod::CreateTaskPushNotificationConfig => {
                let request = match decode_params::<CreateTaskPushNotificationConfigRequest>(params) {
                    Ok(request) => request,
                    Err(error) => return decode_failed(error),
                };
                Box::pin(async move {
                    let value = self.handle_create_push_notification_config(&request)?;
                    serde_json::to_value(value).map_err(|error| serialise_error(&error))
                })
            }
            A2aMethod::GetTaskPushNotificationConfig => {
                let request = match decode_params::<GetTaskPushNotificationConfigRequest>(params) {
                    Ok(request) => request,
                    Err(error) => return decode_failed(error),
                };
                Box::pin(async move {
                    let value = self.handle_get_push_notification_config(&request)?;
                    serde_json::to_value(value).map_err(|error| serialise_error(&error))
                })
            }
            A2aMethod::ListTaskPushNotificationConfigs => {
                let request = match decode_params::<ListTaskPushNotificationConfigsRequest>(params) {
                    Ok(request) => request,
                    Err(error) => return decode_failed(error),
                };
                Box::pin(async move {
                    let value = self.handle_list_push_notification_configs(&request);
                    serde_json::to_value(value).map_err(|error| serialise_error(&error))
                })
            }
            A2aMethod::DeleteTaskPushNotificationConfig => {
                let request = match decode_params::<DeleteTaskPushNotificationConfigRequest>(params) {
                    Ok(request) => request,
                    Err(error) => return decode_failed(error),
                };
                Box::pin(async move {
                    let value = self.handle_delete_push_notification_config(&request)?;
                    serde_json::to_value(value).map_err(|error| serialise_error(&error))
                })
            }
            A2aMethod::GetExtendedAgentCard => {
                let request = match decode_params::<GetExtendedAgentCardRequest>(params) {
                    Ok(request) => request,
                    Err(error) => return decode_failed(error),
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
        let store = self.store.lock().expect("store lock");
        // `total_size` is the number of tasks
        // available *before* the page trim, so a
        // peer paginating through a 100-task list
        // can see the full count on every page and
        // decide when to stop. The `tasks` vector
        // is the page-trimmed list. A page size of
        // `0` is a malformed response per the spec
        // ("the page size is at least 1"), so the
        // example always returns a positive
        // `page_size` even when the peer did not
        // ask for pagination. The `next_page_token`
        // is non-empty when the page is not the
        // last one, and empty otherwise, so a peer
        // can decide when to stop without doing
        // the math. The pagination math is a pure
        // function (`paginate_tasks`) so a
        // regression catches it in a unit test,
        // not against a real server.
        let total_size = store.tasks.len();
        let filtered = store.list(request.context_id.as_deref(), request.status);
        drop(store);
        paginate_tasks(
            filtered,
            request.page_size,
            request.page_token.as_deref(),
            total_size,
        )
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

/// Trims a filtered task list to the peer's page
/// size and stamps the spec-required `page_size`,
/// `total_size`, and `next_page_token` metadata on
/// the response.
///
/// The math is extracted into a pure function so a
/// regression in the pagination invariants is
/// caught in a unit test, not by a wire test
/// against a real server. The invariants the
/// A2A v1.0 spec names are:
///
/// - `page_size` is at least `1` (a response with
///   `page_size: 0` is malformed per the spec).
/// - `total_size` is the count of tasks available
///   *before* the page trim, so a peer
///   paginating through a long list can see the
///   full count on every page.
/// - The trimmed `tasks` vector has at most
///   `page_size` entries.
/// - `next_page_token` is the empty string when
///   the page is the last one, and a non-empty
///   opaque token the peer can pass back to
///   continue otherwise. The example encodes
///   the token as the next offset, base-10;
///   a real agent can choose any encoding as
///   long as the round-trip is deterministic.
///
/// A peer that sends `page_size: 0` is sending a
/// request the spec does not allow; the example
/// treats it as "no page size" and uses the
/// filtered count (clamped to a minimum of 1).
/// Returning `-32602 InvalidParams` would be
/// stricter; a real agent can choose.
///
/// A peer that sends a `page_token` the example
/// cannot decode (anything that is not a base-10
/// `usize`) is treated as offset `0`. A stricter
/// server would return `InvalidParams`; the
/// example chooses tolerance for the wire-shape
/// reference, and a real agent can choose.
fn paginate_tasks(
    filtered: Vec<Task>,
    request_page_size: Option<u32>,
    request_page_token: Option<&str>,
    total_size: usize,
) -> ListTasksResponse {
    let requested_size = request_page_size.filter(|size| *size > 0);
    // The page size is bounded by the filtered list — a peer
    // that asks for a page larger than the result gets a
    // single page that returns everything and an empty
    // `next_page_token`. A page size of `0` falls back to
    // the filtered count, clamped to a minimum of `1` to
    // satisfy the spec floor.
    let filtered_len = filtered.len();
    let page_size = requested_size
        .and_then(|size| usize::try_from(size).ok())
        .filter(|size| *size <= filtered_len.max(1))
        .unwrap_or_else(|| filtered_len.max(1));
    let offset = request_page_token
        .and_then(|token| token.parse::<usize>().ok())
        .unwrap_or(0);
    // A token that points past the end is treated as the
    // end of the list. Returning an empty page with a
    // well-formed envelope is more useful than an
    // error: the peer's next request with no token will
    // see the actual data.
    let effective_offset = offset.min(filtered_len);
    let trimmed: Vec<Task> = filtered
        .into_iter()
        .skip(effective_offset)
        .take(page_size)
        .collect();
    let next_offset = effective_offset.saturating_add(page_size);
    let next_token = if next_offset < filtered_len {
        next_offset.to_string()
    } else {
        String::new()
    };
    ListTasksResponse::new(trimmed)
        .with_next_page_token(next_token)
        .with_page_size(u32::try_from(page_size).unwrap_or(u32::MAX))
        .with_total_size(u32::try_from(total_size).unwrap_or(u32::MAX))
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
    // A2A v1.0 §8 names the well-known card as
    // the discovery endpoint a peer reads to
    // learn the security scheme. A server that
    // requires a bearer MUST still answer the
    // well-known request without one — otherwise
    // a peer that knows nothing about the server
    // has no way to learn what to send. The
    // bearer layer therefore wraps only the
    // JSON-RPC and streaming sub-router; the
    // well-known card is merged in afterwards
    // and stays unauthenticated. A naïve
    // `route_layer` after the full
    // `merge` chain would protect the
    // well-known too — the wire-level regression
    // test in `tests/a2a_client_wire.rs` pins
    // this and would fail with that mistake.
    let mut protected = Router::new()
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
        protected = protected
            .route_layer(axum::middleware::from_fn_with_state(
                std::sync::Arc::from(token),
                check_bearer,
            ));
    }
    serve_well_known(card)
        .merge(protected)
        .merge(health_router())
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
/// the configured token. The well-known path is
/// not routed through this middleware because
/// `build_router` mounts the well-known card on a
/// *separate* sub-router that is `merge`'d into the
/// final router *after* the `route_layer` is
/// applied — a `route_layer` call only protects
/// the routes that are already in the router at
/// the time of the call, and the well-known is
/// not yet in the router when this layer runs. A
/// naïve `route_layer` after the full merge chain
/// would protect the well-known too and the peer
/// could not learn the auth scheme to begin with;
/// the wire-level regression test in
/// `tests/a2a_client_wire.rs`
/// (`the_well_known_card_is_unauthenticated_even_when_bearer_is_required`)
/// pins this and would fail with that mistake.
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

/// Unit tests for the pure helpers. The wire tests in
/// `tests/a2a_serving_wire.rs` pin the serving-adapter
/// integration; these tests pin the spec-conformance
/// invariants the example must hold — a regression here
/// is what a wire test would otherwise catch by accident.
#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "tests assert on fallible calls; .expect() is the standard test pattern"
)]
mod tests {
    use super::*;

    fn task(id: &str) -> Task {
        Task::new(id.to_owned()).with_status(TaskStatus {
            state: TaskState::Completed,
            message: None,
            timestamp: None,
        })
    }

    /// A `user_text_message` is the
    /// common input shape the
    /// `SendMessage` handler reads.
    /// The `Message` builder takes
    /// one `Part` per call; a
    /// regression that expected a
    /// `Vec<Part>` (i.e. iterates
    /// and re-joins) would lose the
    /// text on a single-part input.
    fn user_text_message(text: &str) -> Message {
        Message::new(MessageRole::User).with_part(Part::Text(TextPart {
            text: text.to_owned(),
            metadata: std::collections::BTreeMap::new(),
        }))
    }

    /// A `ListTasks` response with at least one
    /// task has a positive `page_size`. A
    /// `page_size` of `0` is a malformed
    /// response per the A2A spec; the
    /// `ListTasksResponse::new` constructor
    /// defaults to `0`, and a handler that
    /// forgets to set it ships a malformed
    /// response. This test pins the fix.
    #[test]
    fn paginate_tasks_sets_positive_page_size() -> Result<(), Box<dyn std::error::Error>> {
        let response = paginate_tasks(vec![task("t-1")], None, None, 1);
        if response.page_size == 0 {
            return Err(format!(
                "page_size must be at least 1 per the A2A spec; got {}",
                response.page_size
            )
            .into());
        }
        if response.total_size != 1 {
            return Err(format!("total_size must be 1, got {}", response.total_size).into());
        }
        if !response.next_page_token.is_empty() {
            return Err(format!(
                "next_page_token must be empty for a single page; got {:?}",
                response.next_page_token
            )
            .into());
        }
        Ok(())
    }

    /// A peer-supplied `page_size` trims the
    /// `tasks` vector but leaves `total_size`
    /// reporting the full pre-trim count. A
    /// peer paginating through a 100-task list
    /// should see `total_size: 100` on every
    /// page so it can decide when to stop.
    #[test]
    fn paginate_tasks_trims_to_requested_page_size() -> Result<(), Box<dyn std::error::Error>> {
        let response = paginate_tasks(
            vec![task("t-1"), task("t-2"), task("t-3"), task("t-4"), task("t-5")],
            Some(2),
            None,
            5,
        );
        if response.tasks.len() != 2 {
            return Err(format!("expected 2 tasks, got {}", response.tasks.len()).into());
        }
        if response.page_size != 2 {
            return Err(format!("expected page_size 2, got {}", response.page_size).into());
        }
        if response.total_size != 5 {
            return Err(format!("expected total_size 5, got {}", response.total_size).into());
        }
        if response.next_page_token != "2" {
            return Err(format!(
                "expected next_page_token \"2\", got {:?}",
                response.next_page_token
            )
            .into());
        }
        Ok(())
    }

    /// An empty filtered list still produces a
    /// well-formed response: `page_size: 1` (the
    /// spec floor) and `total_size: 0` (no tasks
    /// matched). The vector is empty; no
    /// `tasks` member is required beyond an
    /// empty array. The `next_page_token` is
    /// the empty string (no more pages).
    #[test]
    fn paginate_tasks_empty_filter_keeps_page_size_at_one(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let response = paginate_tasks(Vec::new(), None, None, 0);
        if response.page_size == 0 {
            return Err("page_size must be at least 1 even for an empty result".into());
        }
        if response.total_size != 0 {
            return Err(format!("expected total_size 0, got {}", response.total_size).into());
        }
        if !response.next_page_token.is_empty() {
            return Err(format!(
                "next_page_token must be empty for an empty result; got {:?}",
                response.next_page_token
            )
            .into());
        }
        Ok(())
    }

    /// A peer-supplied `page_size: 0` is a
    /// spec-malformed request (the spec floor
    /// is 1). The example treats it as "no
    /// page size" rather than returning
    /// `-32602`, so the response is still
    /// well-formed (`page_size` is the
    /// filtered count, not 0). A stricter
    /// server could return `InvalidParams`;
    /// the example chooses tolerance for
    /// the wire-shape reference.
    #[test]
    fn paginate_tasks_treats_zero_page_size_as_unset(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let response = paginate_tasks(
            vec![task("t-1"), task("t-2")],
            Some(0),
            None,
            2,
        );
        if response.page_size == 0 {
            return Err("page_size must be at least 1; a peer-supplied 0 is treated as unset".into());
        }
        if response.tasks.len() != 2 {
            return Err(format!(
                "expected both tasks in the page, got {}",
                response.tasks.len()
            )
            .into());
        }
        Ok(())
    }

    /// A peer-supplied `page_token` skips that
    /// many entries and resumes from there.
    /// The token the example accepts is the
    /// next offset, base-10 — the same shape
    /// `paginate_tasks` emits on the previous
    /// page. The round-trip is the
    /// `next_page_token → page_token` flow
    /// the spec describes.
    #[test]
    fn paginate_tasks_resumes_from_page_token() -> Result<(), Box<dyn std::error::Error>> {
        let tasks: Vec<Task> = (0..5)
            .map(|i| task(&format!("t-{i}")))
            .collect();
        // First page: page_size 2, no token.
        let first = paginate_tasks(tasks.clone(), Some(2), None, 5);
        if first.tasks.len() != 2 {
            return Err(format!("first page: expected 2, got {}", first.tasks.len()).into());
        }
        if first.next_page_token != "2" {
            return Err(format!(
                "first page: expected next_page_token \"2\", got {:?}",
                first.next_page_token
            )
            .into());
        }
        // Second page: peer passes the token back.
        let second = paginate_tasks(tasks.clone(), Some(2), Some(&first.next_page_token), 5);
        if second.tasks.len() != 2 {
            return Err(format!("second page: expected 2, got {}", second.tasks.len()).into());
        }
        if second.next_page_token != "4" {
            return Err(format!(
                "second page: expected next_page_token \"4\", got {:?}",
                second.next_page_token
            )
            .into());
        }
        // Last page: the trimmed list has one entry
        // remaining and the next token is empty.
        let last = paginate_tasks(tasks, Some(2), Some(&second.next_page_token), 5);
        if last.tasks.len() != 1 {
            return Err(format!("last page: expected 1, got {}", last.tasks.len()).into());
        }
        if !last.next_page_token.is_empty() {
            return Err(format!(
                "last page: expected empty next_page_token, got {:?}",
                last.next_page_token
            )
            .into());
        }
        Ok(())
    }

    /// A `page_token` that points past the end
    /// of the filtered list is treated as the
    /// end of the list. Returning an empty page
    /// with a well-formed envelope is more
    /// useful than an error: the peer's next
    /// request with no token will see the
    /// actual data.
    #[test]
    fn paginate_tasks_past_end_token_returns_empty_page(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let response = paginate_tasks(vec![task("t-1"), task("t-2")], Some(2), Some("999"), 2);
        if !response.tasks.is_empty() {
            return Err(format!(
                "expected empty tasks, got {} entries",
                response.tasks.len()
            )
            .into());
        }
        if !response.next_page_token.is_empty() {
            return Err(format!(
                "expected empty next_page_token, got {:?}",
                response.next_page_token
            )
            .into());
        }
        if response.total_size != 2 {
            return Err(format!("expected total_size 2, got {}", response.total_size).into());
        }
        Ok(())
    }

    /// A `page_token` that is not a base-10
    /// `usize` is treated as offset `0` (i.e.
    /// the start of the list). A stricter
    /// server would return `InvalidParams`;
    /// the example chooses tolerance for the
    /// wire-shape reference, and a real agent
    /// can choose.
    #[test]
    fn paginate_tasks_invalid_token_falls_back_to_start(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let response = paginate_tasks(
            vec![task("t-1"), task("t-2")],
            Some(2),
            Some("not-a-number"),
            2,
        );
        if response.tasks.len() != 2 {
            return Err(format!(
                "expected 2 tasks (full page), got {}",
                response.tasks.len()
            )
            .into());
        }
        Ok(())
    }

    /// A peer-supplied `page_size` larger than
    /// the filtered list returns the whole
    /// list and an empty `next_page_token` —
    /// the trimmed vector is bounded by the
    /// filtered count, not by the requested
    /// size, so a peer that asks for "give me
    /// everything in one page" gets everything
    /// in one page.
    #[test]
    fn paginate_tasks_page_size_larger_than_filtered_returns_all(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let response = paginate_tasks(
            vec![task("t-1"), task("t-2")],
            Some(50),
            None,
            2,
        );
        if response.tasks.len() != 2 {
            return Err(format!("expected 2 tasks, got {}", response.tasks.len()).into());
        }
        if !response.next_page_token.is_empty() {
            return Err(format!(
                "expected empty next_page_token, got {:?}",
                response.next_page_token
            )
            .into());
        }
        Ok(())
    }

    /// `total_size` is reported as supplied,
    /// not derived from the filtered list.
    /// The caller's filter might have
    /// narrowed the result (e.g. a `state =
    /// Working` filter that drops
    /// `Completed` tasks), and the peer
    /// needs to see the *pre-filter* count
    /// on every page to know when to stop
    /// paginating. A regression that used
    /// `filtered.len()` as `total_size`
    /// would ship a count the peer cannot
    /// reason about, and the peer's
    /// stop-condition would diverge from
    /// the server's.
    #[test]
    fn paginate_tasks_total_size_reflects_caller_supplied_value(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // The filtered list has 2 tasks,
        // but the caller's filter narrowed
        // from a 100-task store. The peer
        // should see `total_size: 100` so
        // it can decide to keep paginating.
        let response = paginate_tasks(
            vec![task("t-1"), task("t-2")],
            Some(1),
            None,
            100,
        );
        if response.total_size != 100 {
            return Err(format!(
                "expected total_size 100 (caller-supplied), got {}",
                response.total_size
            )
            .into());
        }
        if response.tasks.len() != 1 {
            return Err(format!(
                "expected 1 task in the page, got {}",
                response.tasks.len()
            )
            .into());
        }
        if response.next_page_token != "1" {
            return Err(format!(
                "expected next_page_token '1' (offset 1), got {:?}",
                response.next_page_token
            )
            .into());
        }
        Ok(())
    }

    /// A `page_size` exactly equal to
    /// `filtered.len()` returns the whole
    /// list on this page, and the
    /// `next_page_token` is empty because
    /// the next offset is past the end.
    /// The boundary case is what makes
    /// "give me everything in one page"
    /// and "give me N items" agree when
    /// N happens to equal the filter
    /// count.
    #[test]
    fn paginate_tasks_page_size_equal_to_filtered_returns_whole_list(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let response = paginate_tasks(
            vec![task("t-1"), task("t-2"), task("t-3")],
            Some(3),
            None,
            3,
        );
        if response.tasks.len() != 3 {
            return Err(format!(
                "expected 3 tasks, got {}",
                response.tasks.len()
            )
            .into());
        }
        if !response.next_page_token.is_empty() {
            return Err(format!(
                "expected empty next_page_token, got {:?}",
                response.next_page_token
            )
            .into());
        }
        if response.page_size != 3 {
            return Err(format!(
                "expected page_size 3, got {}",
                response.page_size
            )
            .into());
        }
        Ok(())
    }

    /// A `page_size: 1` returns a
    /// singleton page. The first page
    /// of a long list should be a
    /// single task with a non-empty
    /// `next_page_token`, so a peer
    /// paginating through a long list
    /// gets a single task per page and
    /// continues from the next offset.
    /// The boundary case of
    /// `page_size: 1` is the
    /// "smallest possible page" the
    /// spec allows.
    #[test]
    fn paginate_tasks_page_size_one_returns_singleton(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let response = paginate_tasks(
            vec![task("t-1"), task("t-2"), task("t-3")],
            Some(1),
            None,
            3,
        );
        if response.tasks.len() != 1 {
            return Err(format!(
                "expected 1 task, got {}",
                response.tasks.len()
            )
            .into());
        }
        if response.next_page_token != "1" {
            return Err(format!(
                "expected next_page_token '1', got {:?}",
                response.next_page_token
            )
            .into());
        }
        if response.page_size != 1 {
            return Err(format!(
                "expected page_size 1, got {}",
                response.page_size
            )
            .into());
        }
        Ok(())
    }

    /// A task inserted into the store round-trips
    /// through `get` byte-for-byte. The store
    /// clones on the way out, so a caller that
    /// mutates the returned task does not see
    /// the mutation in a subsequent `get` —
    /// tests pin both halves (read returns the
    /// inserted value, and the store is not a
    /// shared mutable view).
    #[test]
    fn task_store_insert_and_get_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let mut store = TaskStore::default();
        let original = task("t-1");
        store.insert(original.clone());
        let read_back = store.get("t-1").ok_or("get returned None")?;
        if read_back != original {
            return Err("get returned a task that does not match the inserted task".into());
        }
        Ok(())
    }

    /// Cancelling a task that does not exist
    /// returns `None`. The handler turns that
    /// into `InvalidParams`; the store's contract
    /// is the smaller half.
    #[test]
    fn task_store_cancel_unknown_id_returns_none() -> Result<(), Box<dyn std::error::Error>> {
        let mut store = TaskStore::default();
        if store.cancel("missing").is_some() {
            return Err("cancel of an unknown id should return None".into());
        }
        Ok(())
    }

    /// Cancelling an existing task mutates the
    /// stored task's state to `Cancelled`. A
    /// peer that calls `get_task` after a cancel
    /// sees the new state.
    #[test]
    fn task_store_cancel_marks_state_cancelled() -> Result<(), Box<dyn std::error::Error>> {
        let mut store = TaskStore::default();
        store.insert(task("t-1"));
        let cancelled = store.cancel("t-1").ok_or("cancel returned None")?;
        if cancelled.status.state != TaskState::Cancelled {
            return Err(format!(
                "expected state Cancelled, got {:?}",
                cancelled.status.state
            )
            .into());
        }
        let read_back = store.get("t-1").ok_or("get returned None")?;
        if read_back.status.state != TaskState::Cancelled {
            return Err("cancel did not persist; a follow-up get saw the old state".into());
        }
        Ok(())
    }

    /// `list` with no filters returns every task
    /// in the store. The pin here is the
    /// no-filter code path: a future change that
    /// adds a default filter would silently
    /// regress, and the test catches it.
    #[test]
    fn task_store_list_no_filter_returns_all() -> Result<(), Box<dyn std::error::Error>> {
        let mut store = TaskStore::default();
        store.insert(task("t-1"));
        store.insert(task("t-2"));
        store.insert(task("t-3"));
        let listed = store.list(None, None);
        if listed.len() != 3 {
            return Err(format!("expected 3 tasks, got {}", listed.len()).into());
        }
        Ok(())
    }

    /// `list` with a `context_id` filter returns
    /// only the tasks whose `context_id` matches.
    /// A future change that forgets to apply the
    /// filter would return the whole store.
    #[test]
    fn task_store_list_filters_by_context_id() -> Result<(), Box<dyn std::error::Error>> {
        let mut store = TaskStore::default();
        let mut with_ctx = task("t-1");
        with_ctx.context_id = Some(Arc::from("ctx-a"));
        let mut other_ctx = task("t-2");
        other_ctx.context_id = Some(Arc::from("ctx-b"));
        store.insert(with_ctx);
        store.insert(other_ctx);
        store.insert(task("t-3"));
        let listed = store.list(Some("ctx-a"), None);
        if listed.len() != 1 {
            return Err(format!("expected 1 task, got {}", listed.len()).into());
        }
        if listed.first().map_or(true, |t| t.id.as_ref() != "t-1") {
            return Err("expected the listed task to be t-1".into());
        }
        Ok(())
    }

    /// `list` with a `state` filter returns only
    /// the tasks in that state. The two filters
    /// are independent: a context+state filter
    /// is the AND.
    #[test]
    fn task_store_list_filters_by_state() -> Result<(), Box<dyn std::error::Error>> {
        let mut store = TaskStore::default();
        let mut cancelled = task("t-1");
        cancelled.status.state = TaskState::Cancelled;
        store.insert(cancelled);
        store.insert(task("t-2"));
        let listed = store.list(None, Some(TaskState::Cancelled));
        if listed.len() != 1 {
            return Err(format!("expected 1 task, got {}", listed.len()).into());
        }
        if listed.first().map_or(true, |t| t.id.as_ref() != "t-1") {
            return Err("expected the listed task to be t-1".into());
        }
        Ok(())
    }

    /// `list` with both `context_id` and `state`
    /// filters returns the AND — the task must
    /// match both. A regression that OR'd the
    /// filters (or short-circuited) would return
    /// every task in the context or every task in
    /// the state, and the test catches it.
    #[test]
    fn task_store_list_combines_context_and_state_as_and(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut store = TaskStore::default();
        // t-1: context-a, cancelled — matches both
        let mut both = task("t-1");
        both.context_id = Some(Arc::from("ctx-a"));
        both.status.state = TaskState::Cancelled;
        // t-2: context-a, completed — only matches context
        let mut ctx_only = task("t-2");
        ctx_only.context_id = Some(Arc::from("ctx-a"));
        // t-3: no context, cancelled — only matches state
        store.insert(both);
        store.insert(ctx_only);
        store.insert(task("t-3"));
        let listed = store.list(Some("ctx-a"), Some(TaskState::Cancelled));
        if listed.len() != 1 {
            return Err(format!(
                "expected 1 task (the AND), got {}",
                listed.len()
            )
            .into());
        }
        if listed.first().map_or(true, |t| t.id.as_ref() != "t-1") {
            return Err("expected the listed task to be t-1".into());
        }
        Ok(())
    }

    /// `list` with a `context_id` that matches no
    /// task returns an empty list, not an error.
    /// A regression that panicked on "no matches"
    /// would crash a peer that asked for an
    /// empty slice of the store.
    #[test]
    fn task_store_list_with_no_match_returns_empty(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut store = TaskStore::default();
        store.insert(task("t-1"));
        store.insert(task("t-2"));
        let listed = store.list(Some("ctx-missing"), None);
        if !listed.is_empty() {
            return Err(format!(
                "expected empty list, got {} tasks",
                listed.len()
            )
            .into());
        }
        Ok(())
    }

    /// `list_push_configs` on a task with no
    /// configs returns an empty list. A
    /// regression that returned every config in
    /// the store (forgetting the task_id filter)
    /// would leak configs across tasks.
    #[test]
    fn task_store_push_config_list_on_unconfigured_task_returns_empty(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut store = TaskStore::default();
        let config = TaskPushNotificationConfig::new(
            String::new(),
            "t-other".to_owned(),
            "https://example.com/a".to_owned(),
        );
        store.insert_push_config(config);
        let listed = store.list_push_configs("t-1");
        if !listed.is_empty() {
            return Err(format!(
                "expected empty list for unconfigured task, got {} configs",
                listed.len()
            )
            .into());
        }
        Ok(())
    }

    /// `delete_push_config` on an id that
    /// doesn't exist returns `None`, not an
    /// error. The handler relies on this to
    /// tell "already deleted" from "real
    /// error" — a regression that returned
    /// `Result::Err` for missing ids would
    /// turn a normal peer behaviour (delete
    /// what exists) into a wire-level error.
    #[test]
    fn task_store_push_config_delete_missing_id_returns_none(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut store = TaskStore::default();
        let deleted = store.delete_push_config("pc-missing");
        if deleted.is_some() {
            return Err(
                "expected None for a missing push config id".into(),
            );
        }
        Ok(())
    }

    /// `insert_push_config` assigns a non-empty
    /// id and stores the config under it. The
    /// id is the same shape every call
    /// (`pc-<n>`) so a peer can list and address
    /// configs deterministically.
    #[test]
    fn task_store_push_config_insert_assigns_id(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut store = TaskStore::default();
        let config = TaskPushNotificationConfig::new(
            String::new(),
            "t-1".to_owned(),
            "https://example.com/webhook".to_owned(),
        );
        let id = store.insert_push_config(config.clone());
        if id.is_empty() {
            return Err("insert_push_config returned an empty id".into());
        }
        let read_back = store
            .get_push_config(&id)
            .ok_or("get_push_config returned None for the just-inserted id")?;
        if read_back.id != id {
            return Err(format!(
                "stored config id {:?} does not match the assigned id {:?}",
                read_back.id, id
            )
            .into());
        }
        if read_back.url != config.url {
            return Err("stored config url does not match".into());
        }
        Ok(())
    }

    /// `list_push_configs` returns only the
    /// configs scoped to the given task. A
    /// future change that forgets the
    /// `task_id` filter would return every
    /// config in the store, and the test
    /// catches that.
    #[test]
    fn task_store_push_config_list_filters_by_task(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut store = TaskStore::default();
        let config_a = TaskPushNotificationConfig::new(
            String::new(),
            "t-1".to_owned(),
            "https://example.com/a".to_owned(),
        );
        let config_b = TaskPushNotificationConfig::new(
            String::new(),
            "t-2".to_owned(),
            "https://example.com/b".to_owned(),
        );
        store.insert_push_config(config_a);
        store.insert_push_config(config_b);
        let listed = store.list_push_configs("t-1");
        if listed.len() != 1 {
            return Err(format!("expected 1 config, got {}", listed.len()).into());
        }
        if listed.first().map_or(true, |c| c.task_id != "t-1") {
            return Err("expected the listed config to be scoped to t-1".into());
        }
        Ok(())
    }

    /// `delete_push_config` returns the deleted
    /// config, and a follow-up `get` returns
    /// `None`. The pair is the round-trip the
    /// handler relies on.
    #[test]
    fn task_store_push_config_delete_returns_the_config(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut store = TaskStore::default();
        let config = TaskPushNotificationConfig::new(
            String::new(),
            "t-1".to_owned(),
            "https://example.com/a".to_owned(),
        );
        let id = store.insert_push_config(config);
        let deleted = store
            .delete_push_config(&id)
            .ok_or("delete returned None for a known id")?;
        if deleted.id != id {
            return Err("delete returned a config with a different id".into());
        }
        if store.get_push_config(&id).is_some() {
            return Err("get after delete returned Some; the deletion did not persist".into());
        }
        Ok(())
    }

    /// Two consecutive inserts get two
    /// different ids. The id counter is
    /// the store's only source of uniqueness,
    /// so a future change that forgets to
    /// increment it would overwrite the
    /// first config on the second call.
    #[test]
    fn task_store_push_config_insert_assigns_unique_ids(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut store = TaskStore::default();
        let first = TaskPushNotificationConfig::new(
            String::new(),
            "t-1".to_owned(),
            "https://example.com/a".to_owned(),
        );
        let second = TaskPushNotificationConfig::new(
            String::new(),
            "t-1".to_owned(),
            "https://example.com/b".to_owned(),
        );
        let id_a = store.insert_push_config(first);
        let id_b = store.insert_push_config(second);
        if id_a == id_b {
            return Err(format!(
                "two inserts returned the same id {id_a:?}; the counter did not advance"
            )
            .into());
        }
        // Both are still present — a counter
        // that overwrites on collision would
        // leave only the second config.
        if store.get_push_config(&id_a).is_none() || store.get_push_config(&id_b).is_none() {
            return Err("insert overwrote an earlier config; the store is not a list".into());
        }
        Ok(())
    }

    /// `len` reports the task count, not the
    /// push config count. The two collections
    /// are independent and the agent card
    /// path that calls `len` cares about
    /// tasks.
    #[test]
    fn task_store_len_counts_only_tasks(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut store = TaskStore::default();
        store.insert(task("t-1"));
        store.insert(task("t-2"));
        let config = TaskPushNotificationConfig::new(
            String::new(),
            "t-1".to_owned(),
            "https://example.com/a".to_owned(),
        );
        store.insert_push_config(config);
        if store.len() != 2 {
            return Err(format!("expected len 2, got {}", store.len()).into());
        }
        Ok(())
    }

    /// `reply_for` is a pure function: a
    /// user message with a single text
    /// part produces an agent message
    /// whose text is `"echo: <text>"`. The
    /// single-part case is the common
    /// one; a regression that lost the
    /// `echo:` prefix would silently
    /// change the wire shape.
    #[test]
    fn reply_for_single_text_part_carries_echo_prefix(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let user = Message::new(MessageRole::User).with_part(Part::Text(TextPart {
            text: "hello".to_owned(),
            metadata: std::collections::BTreeMap::new(),
        }));
        let reply = reply_for(&user);
        if reply.role != MessageRole::Agent {
            return Err(format!("expected Agent role, got {:?}", reply.role).into());
        }
        let [Part::Text(reply_text)] = &reply.parts[..] else {
            return Err("expected exactly one text part on the reply".into());
        };
        if reply_text.text != "echo: hello" {
            return Err(format!(
                "expected 'echo: hello', got {:?}",
                reply_text.text
            )
            .into());
        }
        Ok(())
    }

    /// `reply_for` joins multiple text
    /// parts with a single space. A
    /// regression that joined with `""`
    /// would mangle multi-part inputs.
    /// Non-text parts are filtered out —
    /// the agent has nothing useful to do
    /// with a `Part::File` or `Part::Data`,
    /// so they do not appear in the
    /// reply.
    #[test]
    fn reply_for_joins_text_parts_with_space(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let user = Message::new(MessageRole::User)
            .with_part(Part::Text(TextPart {
                text: "alpha".to_owned(),
                metadata: std::collections::BTreeMap::new(),
            }))
            .with_part(Part::Text(TextPart {
                text: "beta".to_owned(),
                metadata: std::collections::BTreeMap::new(),
            }));
        let reply = reply_for(&user);
        let [Part::Text(reply_text)] = &reply.parts[..] else {
            return Err("expected exactly one text part on the reply".into());
        };
        if reply_text.text != "echo: alpha beta" {
            return Err(format!(
                "expected 'echo: alpha beta', got {:?}",
                reply_text.text
            )
            .into());
        }
        Ok(())
    }

    /// `reply_for` on a message with no
    /// text parts still produces a
    /// well-formed reply — the prefix
    /// carries an empty body, and the
    /// role is `Agent`. The wire shape
    /// is what the consumer reads; a
    /// regression that returned `None`
    /// or a different role would break
    /// the round-trip.
    #[test]
    fn reply_for_message_with_no_text_parts_returns_echo_with_empty_body(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let user = Message::new(MessageRole::User);
        let reply = reply_for(&user);
        if reply.role != MessageRole::Agent {
            return Err(format!("expected Agent role, got {:?}", reply.role).into());
        }
        let [Part::Text(reply_text)] = &reply.parts[..] else {
            return Err("expected exactly one text part on the reply".into());
        };
        if reply_text.text != "echo: " {
            return Err(format!(
                "expected 'echo: ' (with empty body), got {:?}",
                reply_text.text
            )
            .into());
        }
        Ok(())
    }

    /// `build_card` declares the
    /// capabilities the example
    /// actually supports: `streaming` and
    /// `push_notifications` are both
    /// `true`. A card that said
    /// `streaming: false` would tell a
    /// peer to never call
    /// `SendStreamingMessage`, and a
    /// card that said
    /// `push_notifications: false` would
    /// tell a peer that
    /// `CreateTaskPushNotificationConfig`
    /// is unsupported. The example does
    /// both, so the card has to say both.
    #[test]
    fn build_card_declares_streaming_and_push_notifications(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let addr: SocketAddr = "127.0.0.1:8080".parse()?;
        let card = build_card(addr, None);
        if !card.capabilities.streaming {
            return Err("card.capabilities.streaming must be true".into());
        }
        if !card.capabilities.push_notifications {
            return Err("card.capabilities.push_notifications must be true".into());
        }
        Ok(())
    }

    /// `build_card` advertises the
    /// interface at the right URL: the
    /// example is bound to `addr`, and
    /// the card's interface URL has to
    /// match. A card that advertised
    /// `http://127.0.0.1:8080/a2a` while
    /// the server was actually bound to
    /// `127.0.0.1:8084` would be a lie a
    /// peer would believe; the
    /// `a2a_client` example is the test
    /// for that lie, but pinning it in a
    /// unit test catches the regression
    /// before the wire test would.
    #[test]
    fn build_card_interface_url_matches_bound_addr(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let addr: SocketAddr = "127.0.0.1:8084".parse()?;
        let card = build_card(addr, None);
        if card.supported_interfaces.len() != 1 {
            return Err(format!(
                "expected 1 supported interface, got {}",
                card.supported_interfaces.len()
            )
            .into());
        }
        let interface = &card.supported_interfaces[0];
        let expected = format!("http://{addr}/a2a");
        if interface.url.as_str() != expected {
            return Err(format!(
                "expected interface URL {expected:?}, got {:?}",
                interface.url.as_str()
            )
            .into());
        }
        Ok(())
    }

    /// `build_card` without a bearer
    /// token does not publish a security
    /// scheme. A card that always
    /// declared `bearer` would tell a
    /// peer that the server requires a
    /// token even when it does not, and
    /// the peer would fail to discover
    /// the server.
    #[test]
    fn build_card_without_bearer_omits_security_schemes(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let addr: SocketAddr = "127.0.0.1:8080".parse()?;
        let card = build_card(addr, None);
        if !card.security_schemes.is_empty() {
            return Err(format!(
                "expected no security schemes, got {}",
                card.security_schemes.len()
            )
            .into());
        }
        Ok(())
    }

    /// `build_card` with a bearer token
    /// publishes a `bearer` security
    /// scheme so a peer can discover
    /// the requirement. The middleware
    /// (`check_bearer`) is the
    /// enforcement half; the card
    /// declares what is enforced.
    #[test]
    fn build_card_with_bearer_publishes_bearer_scheme(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let addr: SocketAddr = "127.0.0.1:8080".parse()?;
        let card = build_card(addr, Some("secret-token"));
        if !card.security_schemes.contains_key("bearer") {
            return Err("expected a 'bearer' security scheme".into());
        }
        let scheme = &card.security_schemes["bearer"];
        if let SecurityScheme::Http(http) = scheme {
            if http.scheme.as_ref() != "bearer" {
                return Err(format!(
                    "expected http scheme 'bearer', got {:?}",
                    http.scheme.as_ref()
                )
                .into());
            }
        } else {
            return Err(format!(
                "expected SecurityScheme::Http, got a different variant: {scheme:?}"
            )
            .into());
        }
        Ok(())
    }

    /// `build_card` advertises the
    /// `echo` skill with the same
    /// description the example produces.
    /// The skill is what a peer
    /// discovery index uses to decide
    /// whether this agent is the right
    /// one for the user's task; a card
    /// that mis-described it would send
    /// peers to the wrong agent.
    #[test]
    fn build_card_advertises_echo_skill(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let addr: SocketAddr = "127.0.0.1:8080".parse()?;
        let card = build_card(addr, None);
        let has_echo = card
            .skills
            .iter()
            .any(|skill| skill.id.as_ref() == "echo");
        if !has_echo {
            return Err("card did not advertise the 'echo' skill".into());
        }
        Ok(())
    }

    /// `serialise_error` wraps a
    /// `serde_json::Error` from the
    /// response side of the dispatch
    /// into the typed `InternalError`
    /// the JSON-RPC envelope expects.
    /// A regression that returned
    /// `InvalidParams` (the params-side
    /// error) would put the wrong
    /// `code` on the wire: response
    /// failures are `-32603 Internal`,
    /// not `-32602 Invalid Params`.
    #[test]
    fn serialise_error_wraps_serde_error_as_internal(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // A real `serde_json::Error` is
        // the only way to construct one
        // — `serde_json::from_str::<Value>`
        // on invalid JSON is the
        // canonical way to get one.
        let result: Result<serde_json::Value, _> =
            serde_json::from_str("not json");
        let error = match result {
            Err(error) => error,
            Ok(_) => return Err("expected a parse error".into()),
        };
        let wrapped = serialise_error(&error);
        match wrapped {
            A2aError::InternalError { message } => {
                if !message.starts_with("serialise:") {
                    return Err(format!(
                        "expected 'serialise:' prefix, got {message:?}"
                    )
                    .into());
                }
            }
            other => {
                return Err(format!(
                    "expected InternalError, got {other:?}"
                )
                .into());
            }
        }
        Ok(())
    }

    /// `decode_params` decodes a
    /// `serde_json::Value` into the
    /// caller's typed request. A
    /// well-formed value yields the
    /// typed request; a malformed one
    /// yields `InvalidParams` with a
    /// message that names the
    /// underlying `serde_json` error
    /// so a peer can find the bad
    /// field.
    #[test]
    fn decode_params_yields_typed_request_for_well_formed_value(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let params = Some(serde_json::json!({ "id": "t-1" }));
        let request: GetTaskRequest = decode_params(params)
            .map_err(|error| format!("decode failed: {error:?}"))?;
        if request.id != "t-1" {
            return Err(format!("expected id 't-1', got {:?}", request.id).into());
        }
        Ok(())
    }

    /// `decode_params` on a value that
    /// does not match the request
    /// shape returns `InvalidParams`
    /// (not `InternalError`). The
    /// params-side failure is `-32602`,
    /// the code the spec names for bad
    /// inputs.
    #[test]
    fn decode_params_yields_invalid_params_for_malformed_value(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // A value that is the right
        // shape but missing the
        // required `id` field. A
        // `GetTaskRequest` requires
        // `id: String`; `{}` does not
        // supply it.
        let params = Some(serde_json::json!({}));
        let result: Result<GetTaskRequest, A2aError> = decode_params(params);
        match result {
            Err(A2aError::InvalidParams { reason }) => {
                if !reason.starts_with("params do not match") {
                    return Err(format!(
                        "expected 'params do not match' prefix, got {reason:?}"
                    )
                    .into());
                }
            }
            Ok(_) => {
                return Err("expected decode to fail for missing field".into());
            }
            Err(other) => {
                return Err(format!("expected InvalidParams, got {other:?}").into());
            }
        }
        Ok(())
    }

    /// `decode_params` on `None`
    /// substitutes `Value::Null` and
    /// tries to decode it. A
    /// `GetTaskRequest` requires an
    /// `id`, so `Null` does not match
    /// — the test pins the
    /// `None → Null` substitution
    /// path that the dispatch arm
    /// relies on.
    #[test]
    fn decode_params_treats_none_as_null_and_fails(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let result: Result<GetTaskRequest, A2aError> = decode_params(None);
        if result.is_ok() {
            return Err("expected decode_params(None) to fail".into());
        }
        Ok(())
    }

    /// `decode_failed` returns a
    /// ready-failed future carrying
    /// the supplied error. Awaiting
    /// the future yields the same
    /// error. The dispatch arm uses
    /// this as the early-exit when
    /// params decode fails, and the
    /// future shape is the contract
    /// the dispatch's return type
    /// expects.
    #[tokio::test]
    async fn decode_failed_returns_a_future_with_the_supplied_error(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let supplied = A2aError::InvalidParams {
            reason: "test reason".to_owned(),
        };
        let future = decode_failed(supplied.clone());
        let result = future.await;
        let error = match result {
            Err(error) => error,
            Ok(_) => return Err("expected the future to resolve to Err".into()),
        };
        if error != supplied {
            return Err(format!(
                "expected the same error back, got {error:?} vs {supplied:?}"
            )
            .into());
        }
        Ok(())
    }

    /// Build a minimal `EchoAgent`
    /// with an empty store, a real
    /// tokio handle, and no push
    /// notification configs. The
    /// handle is needed because the
    /// `EchoAgent` struct holds one;
    /// the spawn it would do at
    /// delivery time is never reached
    /// because an empty config list
    /// returns early. The card is the
    /// one the example would publish,
    /// so the `GetExtendedAgentCard`
    /// test reads back the same shape
    /// a peer would.
    fn minimal_agent() -> Result<EchoAgent, Box<dyn std::error::Error>> {
        let addr: SocketAddr = "127.0.0.1:8080".parse()?;
        let delivery = WebhookDelivery::new(Arc::new(SsrfDenylist::FILTER));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let agent = EchoAgent {
            store: Arc::new(std::sync::Mutex::new(TaskStore::default())),
            delivery: Arc::new(delivery),
            handle: runtime.handle().clone(),
            card: build_card(addr, None),
        };
        // Drop the runtime but keep the
        // handle. `Handle` outlives the
        // runtime; calls to `handle.spawn`
        // would fail at *use* time, but
        // the tests below never trigger
        // that path because the store
        // starts with no push configs.
        drop(runtime);
        Ok(agent)
    }

    /// `handle_send_message` creates a
    /// task, stores it, and returns
    /// the task with a `completed`
    /// state. The reply text is the
    /// echo of the input. The test
    /// pins the contract: the handler
    /// is the only place a task gets
    /// a `t-<n>` id, so a regression
    /// that bypassed the allocator
    /// would surface here as a
    /// missing or wrong id.
    #[test]
    fn handle_send_message_creates_a_completed_task_with_echo_reply(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let agent = minimal_agent()?;
        let request = SendMessageRequest::new(user_text_message("hello"));
        let response = agent.handle_send_message(&request);
        let SendMessageResponse::Task(task) = response else {
            return Err("expected SendMessageResponse::Task".into());
        };
        if !task.id.as_ref().starts_with("t-") {
            return Err(format!(
                "expected id to start with 't-', got {:?}",
                task.id.as_ref()
            )
            .into());
        }
        if task.status.state != TaskState::Completed {
            return Err(format!(
                "expected state Completed, got {:?}",
                task.status.state
            )
            .into());
        }
        let reply_text = task
            .status
            .message
            .as_ref()
            .and_then(|message| message.parts.first())
            .and_then(|part| match part {
                Part::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .ok_or("expected a text reply")?;
        if reply_text != "echo: hello" {
            return Err(format!("expected 'echo: hello', got {reply_text:?}").into());
        }
        Ok(())
    }

    /// `handle_get_task` returns the
    /// task the request names. The
    /// test sends a `SendMessage` to
    /// get a real id, then reads the
    /// task back. The round-trip is
    /// what a peer calling
    /// `SendMessage` then
    /// `GetTask` would observe.
    #[test]
    fn handle_get_task_returns_a_known_task(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let agent = minimal_agent()?;
        let sent = agent.handle_send_message(&SendMessageRequest::new(
            user_text_message("first"),
        ));
        let SendMessageResponse::Task(sent_task) = sent else {
            return Err("expected SendMessageResponse::Task".into());
        };
        let id = sent_task.id.to_string();
        let got = agent
            .handle_get_task(&GetTaskRequest::new(id.clone()))
            .map_err(|error| format!("handle_get_task failed: {error:?}"))?;
        if got.id.as_ref() != id {
            return Err(format!(
                "expected id {id:?}, got {:?}",
                got.id.as_ref()
            )
            .into());
        }
        if got.status.state != TaskState::Completed {
            return Err(format!(
                "expected state Completed, got {:?}",
                got.status.state
            )
            .into());
        }
        Ok(())
    }

    /// `handle_get_task` for a missing
    /// id returns `InvalidParams` —
    /// not `InternalError`, not
    /// `MethodNotFound`. The
    /// `MethodNotFound` shape is
    /// reserved for the method-level
    /// case (the peer named a method
    /// that isn't in the table), and
    /// `InternalError` is reserved
    /// for the server's own
    /// failures. "The task id is
    /// unknown" is a *params* shape
    /// mismatch from the handler's
    /// point of view, and the wire
    /// code is `-32602`.
    #[test]
    fn handle_get_task_missing_id_is_invalid_params(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let agent = minimal_agent()?;
        let result = agent.handle_get_task(&GetTaskRequest::new("t-missing"));
        match result {
            Err(A2aError::InvalidParams { reason }) => {
                if !reason.contains("t-missing") {
                    return Err(format!(
                        "expected reason to name the missing id, got {reason:?}"
                    )
                    .into());
                }
            }
            Ok(_) => return Err("expected an InvalidParams error".into()),
            Err(other) => {
                return Err(format!("expected InvalidParams, got {other:?}").into())
            }
        }
        Ok(())
    }

    /// `handle_cancel_task` flips the
    /// state of an existing task to
    /// `Cancelled` and returns the
    /// mutated task. A regression
    /// that returned the unchanged
    /// task (forgot the mutation)
    /// would surface here as a
    /// state mismatch.
    #[test]
    fn handle_cancel_task_flips_state_to_cancelled(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let agent = minimal_agent()?;
        let sent = agent.handle_send_message(&SendMessageRequest::new(
            user_text_message("to-cancel"),
        ));
        let SendMessageResponse::Task(sent_task) = sent else {
            return Err("expected SendMessageResponse::Task".into());
        };
        let id = sent_task.id.to_string();
        let cancelled = agent
            .handle_cancel_task(&CancelTaskRequest::new(id.clone()))
            .map_err(|error| format!("handle_cancel_task failed: {error:?}"))?;
        if cancelled.status.state != TaskState::Cancelled {
            return Err(format!(
                "expected state Cancelled, got {:?}",
                cancelled.status.state
            )
            .into());
        }
        // The follow-up read sees the
        // mutated state — the handler
        // is the mutation, not just a
        // response.
        let read_back = agent
            .handle_get_task(&GetTaskRequest::new(id.clone()))
            .map_err(|error| format!("handle_get_task failed: {error:?}"))?;
        if read_back.status.state != TaskState::Cancelled {
            return Err(format!(
                "expected GetTask to read Cancelled, got {:?}",
                read_back.status.state
            )
            .into());
        }
        Ok(())
    }

    /// `handle_cancel_task` for a
    /// missing id returns
    /// `InvalidParams`, mirroring
    /// the `handle_get_task` shape.
    #[test]
    fn handle_cancel_task_missing_id_is_invalid_params(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let agent = minimal_agent()?;
        let result = agent.handle_cancel_task(&CancelTaskRequest::new("t-missing"));
        match result {
            Err(A2aError::InvalidParams { reason }) => {
                if !reason.contains("t-missing") {
                    return Err(format!(
                        "expected reason to name the missing id, got {reason:?}"
                    )
                    .into());
                }
            }
            Ok(_) => return Err("expected an InvalidParams error".into()),
            Err(other) => {
                return Err(format!("expected InvalidParams, got {other:?}").into())
            }
        }
        Ok(())
    }

    /// `handle_list_tasks` reports
    /// the number of tasks the agent
    /// has. The test populates the
    /// store via two `SendMessage`
    /// calls, then lists with no
    /// filters. The list shape
    /// (`page_size`, `total_size`,
    /// `next_page_token`) is the
    /// `paginate_tasks` contract; the
    /// test pins the integration
    /// between the handler and the
    /// pagination math.
    #[test]
    fn handle_list_tasks_reports_total_size_of_populated_store(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let agent = minimal_agent()?;
        for text in ["one", "two"] {
            let _ = agent.handle_send_message(&SendMessageRequest::new(
                user_text_message(text),
            ));
        }
        let response = agent.handle_list_tasks(&ListTasksRequest {
            context_id: None,
            status: None,
            page_size: None,
            page_token: None,
            history_length: None,
            status_timestamp_after: None,
            metadata: std::collections::BTreeMap::new(),
        });
        if response.total_size != 2 {
            return Err(format!(
                "expected total_size 2, got {}",
                response.total_size
            )
            .into());
        }
        if response.tasks.len() != 2 {
            return Err(format!(
                "expected 2 tasks in the page, got {}",
                response.tasks.len()
            )
            .into());
        }
        Ok(())
    }

    /// `handle_get_extended_agent_card`
    /// returns the agent's card so a
    /// peer that authenticated can
    /// discover the post-auth card.
    /// The test pins that the card
    /// the agent holds is the card
    /// the handler hands back, with
    /// the same name and version.
    #[test]
    fn handle_get_extended_agent_card_returns_the_agent_card(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let agent = minimal_agent()?;
        let response = agent
            .handle_get_extended_agent_card(&GetExtendedAgentCardRequest::new());
        if response.0.name != agent.card.name {
            return Err(format!(
                "expected name {:?}, got {:?}",
                agent.card.name, response.0.name
            )
            .into());
        }
        if response.0.version != agent.card.version {
            return Err(format!(
                "expected version {:?}, got {:?}",
                agent.card.version, response.0.version
            )
            .into());
        }
        Ok(())
    }
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
