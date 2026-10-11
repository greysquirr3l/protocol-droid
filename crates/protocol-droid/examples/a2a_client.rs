//! A small client that drives an A2A peer.
//!
//! The mirror of `a2a_server.rs`: where the server example
//! shows what an A2A peer serves, this one shows what an
//! A2A peer consumes. There is no `A2aClient` port in the
//! library yet — adding one is T27 scope — so this example
//! uses `reqwest` directly. The pattern is small enough
//! that a hand-rolled client is shorter than the macro
//! expansion a future client port would need.
//!
//! Run it against the `a2a_server` example:
//!
//! ```text
//! # terminal 1
//! cargo run -p protocol-droid --example a2a_server --features a2a,http
//!
//! # terminal 2
//! cargo run -p protocol-droid --example a2a_client --features a2a,http
//! ```
//!
//! Override the peer with `A2A_PEER=http://other-host:9000`.
//! The peer must be reachable; the well-known card is the
//! first thing the client fetches.
//!
//! The example exercises four of the eleven A2A v1.0
//! methods — `SendMessage`, `GetTask`, `ListTasks`,
//! `CancelTask` — all over JSON-RPC. Streaming methods
//! (`SendStreamingMessage`, `SubscribeToTask`) need a
//! Server-Sent Events parser; the JSON-RPC envelope is
//! the same, so adding them is an SSE reader on top of
//! the same `json_rpc_envelope` shape.

use std::error::Error;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use protocol_droid::adapters::secondary::a2a::A2aMethod;
use protocol_droid::domain::a2a::agent_card::AgentCard;
use protocol_droid::domain::a2a::agent_card::JSONRPC;
use protocol_droid::domain::a2a::message::{Message, MessageRole, Part, TextPart};
use protocol_droid::domain::a2a::send_message::{SendMessageRequest, SendMessageResponse};
use protocol_droid::domain::a2a::task_lifecycle::{CancelTaskRequest, CancelTaskResponse, GetTaskRequest, GetTaskResponse};
use protocol_droid::domain::a2a::list_tasks::{ListTasksRequest, ListTasksResponse};
use reqwest::Client;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let peer = std::env::var("A2A_PEER").unwrap_or_else(|_| "http://127.0.0.1:8080".to_owned());

    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;

    // 1. Fetch the well-known card. The card tells us what
    //    the peer supports and where to reach it.
    let card_url = format!("{peer}/.well-known/agent-card.json");
    let card: AgentCard = fetch_json(&client, &card_url).await?;
    eprintln!("peer: {} v{}", card.name, card.version);
    eprintln!(
        "capabilities: streaming={} pushNotifications={} extendedAgentCard={}",
        card.capabilities.streaming,
        card.capabilities.push_notifications,
        card.capabilities.extended_agent_card,
    );

    // 2. Find the JSON-RPC interface. A peer may advertise
    //    multiple bindings (JSON-RPC, gRPC, HTTP+JSON); the
    //    A2aClient v1.0 line is JSON-RPC, and the example
    //    speaks that one.
    let jsonrpc_url = card
        .supported_interfaces
        .iter()
        .find(|i| i.protocol_binding.as_str() == JSONRPC.as_str())
        .ok_or("peer advertises no JSON-RPC interface")?
        .url
        .clone();

    // 3. SendMessage. Construct the request with the typed
    //    `Message` value object — the same one the server
    //    decodes back.
    let message = Message::new(MessageRole::User)
        .with_part(Part::Text(TextPart {
            text: "hello from a2a_client".to_owned(),
            metadata: std::collections::BTreeMap::new(),
        }));
    let send = SendMessageRequest {
        message,
        configuration: None,
        metadata: std::collections::BTreeMap::new(),
    };
    let send_value = serde_json::to_value(&send)?;
    let result: Value = json_rpc(&client, jsonrpc_url.as_str(), A2aMethod::SendMessage, send_value).await?;
    let task_response: SendMessageResponse = serde_json::from_value(result.clone())?;
    let task_id = match &task_response {
        SendMessageResponse::Task(task) => task.id.to_string(),
        SendMessageResponse::Message(_) => {
            return Err("peer returned a Message instead of a Task".into());
        }
    };
    eprintln!("SendMessage -> task id {task_id}");

    // 4. GetTask. Read the task back to confirm the
    //    server's view matches what we sent.
    let get = GetTaskRequest {
        id: task_id.clone(),
        history_length: None,
        metadata: std::collections::BTreeMap::new(),
    };
    let get_value = serde_json::to_value(&get)?;
    let result: Value = json_rpc(&client, jsonrpc_url.as_str(), A2aMethod::GetTask, get_value).await?;
    let _get_response: GetTaskResponse = serde_json::from_value(result.clone())?;
    eprintln!("GetTask({task_id}) -> {}", get_status(&result));

    // 5. ListTasks. A peer may filter by context id or
    //    state; the example leaves both empty so the
    //    response is the full list.
    let list = ListTasksRequest {
        context_id: None,
        status: None,
        page_size: None,
        page_token: None,
        status_timestamp_after: None,
        history_length: None,
        metadata: std::collections::BTreeMap::new(),
    };
    let list_value = serde_json::to_value(&list)?;
    let result: Value = json_rpc(&client, jsonrpc_url.as_str(), A2aMethod::ListTasks, list_value).await?;
    let list_response: ListTasksResponse = serde_json::from_value(result.clone())?;
    eprintln!("ListTasks -> {} task(s)", list_response.tasks.len());

    // 6. CancelTask. The example agent's task is already
    //    in `completed` state, so the cancel is a no-op
    //    on state but still returns the task.
    let cancel = CancelTaskRequest {
        id: task_id.clone(),
        metadata: std::collections::BTreeMap::new(),
    };
    let cancel_value = serde_json::to_value(&cancel)?;
    let result: Value = json_rpc(&client, jsonrpc_url.as_str(), A2aMethod::CancelTask, cancel_value).await?;
    let _cancel_response: CancelTaskResponse = serde_json::from_value(result.clone())?;
    eprintln!("CancelTask({task_id}) -> {}", get_status(&result));

    Ok(())
}

/// Sends one JSON-RPC 2.0 request and returns the
/// `result` field as a typed value. The id is generated
/// from a process-local counter so concurrent clients do
/// not collide on the same id. A `result` field is
/// always present on a successful response; an `error`
/// field is a typed `ErrorResponse` that this function
/// surfaces as a `Box<dyn Error>`.
async fn json_rpc<T: DeserializeOwned>(
    client: &Client,
    url: &str,
    method: A2aMethod,
    params: Value,
) -> Result<T, Box<dyn Error>> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let envelope = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method.as_str(),
        "params": params,
    });
    let body = client
        .post(url)
        .body(serde_json::to_vec(&envelope)?)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let response: Value = serde_json::from_str(&body)?;
    if let Some(error) = response.get("error") {
        return Err(format!("JSON-RPC error: {error}").into());
    }
    let Some(result) = response.get("result") else {
        return Err(format!("missing result in response: {response}").into());
    };
    Ok(serde_json::from_value(result.clone())?)
}

/// Reads `status.state` from a JSON-RPC `result` value
/// for a Task-shaped response. Used as a one-line
/// confirmation that the round trip carried the
/// expected state.
fn get_status(result: &Value) -> String {
    result
        .pointer("/status/state")
        .and_then(Value::as_str)
        .map_or_else(|| "(no status)".to_owned(), std::borrow::ToOwned::to_owned)
}

/// GETs a URL and parses the response body as JSON into
/// a typed value. Reads the body as text first because
/// the workspace's `reqwest` is configured without the
/// `json` feature; a `text` round-trip plus a
/// `serde_json::from_str` is the smallest dependency
/// shape the example can use.
async fn fetch_json<T: DeserializeOwned>(
    client: &Client,
    url: &str,
) -> Result<T, Box<dyn Error>> {
    let body = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    Ok(serde_json::from_str(&body)?)
}
