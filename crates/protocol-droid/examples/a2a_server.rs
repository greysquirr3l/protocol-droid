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
//!
//! The agent below is a real in-memory task store. `SendMessage`
//! creates a task in the store and returns it, `GetTask` reads
//! from the store, `CancelTask` mutates the store, and `ListTasks`
//! enumerates the store. The agent's "work" is a one-line echo
//! because the point of an example is the wiring, not the work.

#![allow(
    clippy::expect_used,
    reason = "examples assert on fallible calls with named failure messages; a panic in main is the right shape for an example"
)]

use std::collections::HashMap;
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
use protocol_droid::domain::a2a::send_message::{SendMessageRequest, SendMessageResponse};
use protocol_droid::domain::a2a::task::{Task, TaskState, TaskStatus};
use protocol_droid::domain::a2a::task_lifecycle::{
    CancelTaskRequest, CancelTaskResponse, GetTaskRequest, GetTaskResponse,
};
use protocol_droid::domain::a2a::list_tasks::{ListTasksRequest, ListTasksResponse};
use tokio::sync::Mutex;

/// The in-memory task store. A real agent would put a database
/// here, or a queue fronting a worker pool. The shape — keyed
/// by `task id` with optional filters on context and state — is
/// the same either way.
#[derive(Default)]
struct TaskStore {
    next_id: u64,
    tasks: HashMap<String, Task>,
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
}

/// The agent. Holds the task store behind an `Arc<Mutex>` so the
/// JSON-RPC dispatcher can move the shared state into each
/// request's handler.
struct EchoAgent {
    store: Arc<Mutex<TaskStore>>,
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
                let request: SendMessageRequest = match decode_params(params) {
                    Ok(r) => r,
                    Err(error) => return Box::pin(std::future::ready(Err(error))),
                };
                Box::pin(async move {
                    let value = self.handle_send_message(request).await?;
                    serde_json::to_value(value).map_err(|error| A2aError::InternalError {
                        message: format!("serialise: {error}"),
                    })
                })
            }
            A2aMethod::GetTask => {
                let request: GetTaskRequest = match decode_params(params) {
                    Ok(r) => r,
                    Err(error) => return Box::pin(std::future::ready(Err(error))),
                };
                Box::pin(async move {
                    let value = self.handle_get_task(request).await?;
                    serde_json::to_value(value).map_err(|error| A2aError::InternalError {
                        message: format!("serialise: {error}"),
                    })
                })
            }
            A2aMethod::CancelTask => {
                let request: CancelTaskRequest = match decode_params(params) {
                    Ok(r) => r,
                    Err(error) => return Box::pin(std::future::ready(Err(error))),
                };
                Box::pin(async move {
                    let value = self.handle_cancel_task(request).await?;
                    serde_json::to_value(value).map_err(|error| A2aError::InternalError {
                        message: format!("serialise: {error}"),
                    })
                })
            }
            A2aMethod::ListTasks => {
                let request: ListTasksRequest = match decode_params(params) {
                    Ok(r) => r,
                    Err(error) => return Box::pin(std::future::ready(Err(error))),
                };
                Box::pin(async move {
                    let value = self.handle_list_tasks(request).await?;
                    serde_json::to_value(value).map_err(|error| A2aError::InternalError {
                        message: format!("serialise: {error}"),
                    })
                })
            }
            _other => Box::pin(std::future::ready(Err(A2aError::MethodNotFound))),
        }
    }
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
    async fn handle_send_message(
        &self,
        request: SendMessageRequest,
    ) -> Result<SendMessageResponse, A2aError> {
        let reply = reply_for(&request.message);
        let next_id = {
            let mut store = self.store.lock().await;
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
        self.store.lock().await.insert(stored);
        Ok(SendMessageResponse::Task(task))
    }

    async fn handle_get_task(&self, request: GetTaskRequest) -> Result<GetTaskResponse, A2aError> {
        self.store
            .lock()
            .await
            .get(&request.id)
            .map(GetTaskResponse::from)
            .ok_or_else(|| A2aError::InvalidParams {
                reason: format!("no task with id '{}'", request.id),
            })
    }

    async fn handle_cancel_task(
        &self,
        request: CancelTaskRequest,
    ) -> Result<CancelTaskResponse, A2aError> {
        self.store
            .lock()
            .await
            .cancel(&request.id)
            .map(CancelTaskResponse::from)
            .ok_or_else(|| A2aError::InvalidParams {
                reason: format!("no task with id '{}'", request.id),
            })
    }

    async fn handle_list_tasks(
        &self,
        request: ListTasksRequest,
    ) -> Result<ListTasksResponse, A2aError> {
        let tasks = self
            .store
            .lock()
            .await
            .list(request.context_id.as_deref(), request.status);
        Ok(ListTasksResponse::new(tasks))
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

/// Builds the agent card. The well-known path serves this exact value.
fn build_card() -> Arc<AgentCard> {
    Arc::new(
        AgentCardBuilder::new()
            .with_name("Echo Agent")
            .with_description("An A2A example that echoes messages back as completed tasks.")
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
                description: "Echoes the user's message back as a completed task.".into(),
                tags: Vec::new(),
            })
            .build()
            .expect("card is built from valid inputs"),
    )
}

/// Mounts the well-known path and the JSON-RPC path on one router.
fn build_router() -> Router {
    let card = build_card();
    let agent = EchoAgent {
        store: Arc::new(Mutex::new(TaskStore::default())),
    };
    let handler: Arc<dyn A2aRequestHandler> = Arc::new(agent);
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
