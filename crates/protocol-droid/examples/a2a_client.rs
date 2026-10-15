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
//! The example exercises ten of the eleven A2A v1.0
//! methods — `SendMessage`, `SendStreamingMessage`,
//! `GetTask`, `SubscribeToTask`, `ListTasks`,
//! `CancelTask`, `CreateTaskPushNotificationConfig`,
//! `GetTaskPushNotificationConfig`,
//! `ListTaskPushNotificationConfigs`,
//! `DeleteTaskPushNotificationConfig`. The eleventh,
//! `GetExtendedAgentCard`, is auth-gated; a real
//! consumer would fetch the well-known card with the
//! same auth header.
//!
//! The streaming methods need a Server-Sent Events
//! parser on top of the JSON-RPC envelope; `sse_post`
//! is the line reader. The push-notification config
//! methods share the JSON-RPC envelope with the
//! request/response methods; the example uses
//! `http://1.2.3.4:9999/wh` as the webhook URL because
//! the SSRF denylist on the example server accepts it
//! (it is not on the deny list) but the network has no
//! route to that address, so the delivery silently
//! fails. The example is exercising the config CRUD,
//! not the delivery.

use std::error::Error;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use protocol_droid::adapters::secondary::a2a::A2aMethod;
use protocol_droid::domain::a2a::agent_card::AgentCard;
use protocol_droid::domain::a2a::agent_card::JSONRPC;
use protocol_droid::domain::a2a::list_tasks::{ListTasksRequest, ListTasksResponse};
use protocol_droid::domain::a2a::message::{Message, MessageRole, Part, TextPart};
use protocol_droid::domain::a2a::push_notification::TaskPushNotificationConfig;
use protocol_droid::domain::a2a::push_notification_methods::{
    CreateTaskPushNotificationConfigRequest, CreateTaskPushNotificationConfigResponse,
    DeleteTaskPushNotificationConfigRequest, DeleteTaskPushNotificationConfigResponse,
    GetTaskPushNotificationConfigRequest, GetTaskPushNotificationConfigResponse,
    ListTaskPushNotificationConfigsRequest, ListTaskPushNotificationConfigsResponse,
};
use protocol_droid::domain::a2a::send_message::{SendMessageRequest, SendMessageResponse};
use protocol_droid::domain::a2a::streaming_events::StreamingEvent;
use protocol_droid::domain::a2a::task::TaskState;
use protocol_droid::domain::a2a::task_lifecycle::{
    CancelTaskRequest, CancelTaskResponse, GetTaskRequest, GetTaskResponse,
};
use reqwest::Client;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let peer = std::env::var("A2A_PEER").unwrap_or_else(|_| "http://127.0.0.1:8080".to_owned());

    let client = Client::builder().timeout(Duration::from_secs(10)).build()?;

    let card: AgentCard =
        fetch_json(&client, &format!("{peer}/.well-known/agent-card.json")).await?;
    eprintln!("peer: {} v{}", card.name, card.version);
    eprintln!(
        "capabilities: streaming={} pushNotifications={} extendedAgentCard={}",
        card.capabilities.streaming,
        card.capabilities.push_notifications,
        card.capabilities.extended_agent_card,
    );

    let jsonrpc_url = card
        .supported_interfaces
        .iter()
        .find(|i| i.protocol_binding.as_str() == JSONRPC.as_str())
        .ok_or("peer advertises no JSON-RPC interface")?
        .url
        .clone();
    let stream_url = jsonrpc_url.as_str().replace("/a2a", "/a2a/stream");

    let task_id = send_message(&client, jsonrpc_url.as_str(), "hello from a2a_client").await?;
    eprintln!("SendMessage -> task id {task_id}");

    get_task(&client, jsonrpc_url.as_str(), &task_id).await?;
    list_tasks(&client, jsonrpc_url.as_str()).await?;
    cancel_task(&client, jsonrpc_url.as_str(), &task_id).await?;

    send_streaming_message(&client, &stream_url, "hello via streaming").await?;
    subscribe_to_task(&client, &stream_url, &task_id).await?;

    let pc_id = create_push_notification_config(
        &client,
        jsonrpc_url.as_str(),
        &task_id,
        "http://1.2.3.4:9999/wh",
        "demo-token",
    )
    .await?;
    get_push_notification_config(&client, jsonrpc_url.as_str(), &task_id, &pc_id).await?;
    list_push_notification_configs(&client, jsonrpc_url.as_str(), &task_id).await?;
    delete_push_notification_config(&client, jsonrpc_url.as_str(), &task_id, &pc_id).await?;

    Ok(())
}

/// `SendMessage` with a single text part. Returns the
/// new task's id, which is what every other method
/// here keys off of.
async fn send_message(client: &Client, url: &str, text: &str) -> Result<String, Box<dyn Error>> {
    let message = Message::new(MessageRole::User).with_part(Part::Text(TextPart {
        text: text.to_owned(),
        metadata: std::collections::BTreeMap::new(),
    }));
    let request = SendMessageRequest {
        message,
        configuration: None,
        metadata: std::collections::BTreeMap::new(),
    };
    let value = serde_json::to_value(&request)?;
    let result: Value = json_rpc(client, url, A2aMethod::SendMessage, value).await?;
    let response: SendMessageResponse = serde_json::from_value(result)?;
    match response {
        SendMessageResponse::Task(task) => Ok(task.id.to_string()),
        SendMessageResponse::Message(_) => Err("peer returned a Message instead of a Task".into()),
    }
}

/// `GetTask`. Prints the response's `status.state`.
async fn get_task(client: &Client, url: &str, task_id: &str) -> Result<(), Box<dyn Error>> {
    let request = GetTaskRequest {
        id: task_id.to_owned(),
        history_length: None,
        metadata: std::collections::BTreeMap::new(),
    };
    let value = serde_json::to_value(&request)?;
    let result: Value = json_rpc(client, url, A2aMethod::GetTask, value).await?;
    let _response: GetTaskResponse = serde_json::from_value(result.clone())?;
    eprintln!("GetTask({task_id}) -> {}", get_status(&result));
    Ok(())
}

/// `ListTasks`. The example leaves every filter empty
/// so the response is the full list.
async fn list_tasks(client: &Client, url: &str) -> Result<(), Box<dyn Error>> {
    let request = ListTasksRequest {
        context_id: None,
        status: None,
        page_size: None,
        page_token: None,
        status_timestamp_after: None,
        history_length: None,
        metadata: std::collections::BTreeMap::new(),
    };
    let value = serde_json::to_value(&request)?;
    let result: Value = json_rpc(client, url, A2aMethod::ListTasks, value).await?;
    let response: ListTasksResponse = serde_json::from_value(result)?;
    eprintln!("ListTasks -> {} task(s)", response.tasks.len());
    Ok(())
}

/// `CancelTask`. The example's task is already in
/// `completed` state, so the cancel is a no-op on
/// state but still returns the task.
async fn cancel_task(client: &Client, url: &str, task_id: &str) -> Result<(), Box<dyn Error>> {
    let request = CancelTaskRequest {
        id: task_id.to_owned(),
        metadata: std::collections::BTreeMap::new(),
    };
    let value = serde_json::to_value(&request)?;
    let result: Value = json_rpc(client, url, A2aMethod::CancelTask, value).await?;
    let _response: CancelTaskResponse = serde_json::from_value(result.clone())?;
    eprintln!("CancelTask({task_id}) -> {}", get_status(&result));
    Ok(())
}

/// `CreateTaskPushNotificationConfig`. The example
/// uses `http://1.2.3.4:9999/wh` because the SSRF
/// denylist on the example server rejects loopback
/// URLs (so the test would be vacuous if it pointed
/// at the peer) and 1.2.3.4 is not on the deny list
/// (so the config-time check accepts it). The
/// delivery to 1.2.3.4 will silently fail because
/// the network has no route to that address; the
/// point of the example is the config CRUD, not the
/// delivery. Returns the config id assigned by the
/// server.
async fn create_push_notification_config(
    client: &Client,
    url: &str,
    task_id: &str,
    webhook_url: &str,
    token: &str,
) -> Result<String, Box<dyn Error>> {
    let config = TaskPushNotificationConfig {
        id: "placeholder".to_owned(),
        task_id: task_id.to_owned(),
        url: webhook_url.to_owned(),
        token: Some(token.to_owned()),
        authentication: None,
        metadata: std::collections::BTreeMap::new(),
    };
    let request = CreateTaskPushNotificationConfigRequest::from(config);
    let value = serde_json::to_value(&request)?;
    let result: Value = json_rpc(
        client,
        url,
        A2aMethod::CreateTaskPushNotificationConfig,
        value,
    )
    .await?;
    let response: CreateTaskPushNotificationConfigResponse = serde_json::from_value(result)?;
    let config_id = response.0.id;
    eprintln!("CreateTaskPushNotificationConfig({task_id}) -> {config_id}");
    Ok(config_id)
}

/// `GetTaskPushNotificationConfig`. Reads back the
/// config created above. The URL field round-trips
/// byte-for-byte, which is the wire contract the
/// library's serving adapter promises.
async fn get_push_notification_config(
    client: &Client,
    url: &str,
    task_id: &str,
    config_id: &str,
) -> Result<(), Box<dyn Error>> {
    let request =
        GetTaskPushNotificationConfigRequest::new(config_id.to_owned(), task_id.to_owned());
    let value = serde_json::to_value(&request)?;
    let result: Value =
        json_rpc(client, url, A2aMethod::GetTaskPushNotificationConfig, value).await?;
    let _response: GetTaskPushNotificationConfigResponse = serde_json::from_value(result)?;
    eprintln!("GetTaskPushNotificationConfig({task_id}, {config_id}) -> ok");
    Ok(())
}

/// `ListTaskPushNotificationConfigs`. After the
/// create above, the list has one entry; the
/// example does not page.
async fn list_push_notification_configs(
    client: &Client,
    url: &str,
    task_id: &str,
) -> Result<(), Box<dyn Error>> {
    let request = ListTaskPushNotificationConfigsRequest {
        task_id: task_id.to_owned(),
        page_size: None,
        page_token: None,
    };
    let value = serde_json::to_value(&request)?;
    let result: Value = json_rpc(
        client,
        url,
        A2aMethod::ListTaskPushNotificationConfigs,
        value,
    )
    .await?;
    let response: ListTaskPushNotificationConfigsResponse = serde_json::from_value(result)?;
    eprintln!(
        "ListTaskPushNotificationConfigs({task_id}) -> {} config(s)",
        response.configs.len()
    );
    Ok(())
}

/// `DeleteTaskPushNotificationConfig`. The server
/// returns an empty response body; the example
/// just confirms the call succeeded.
async fn delete_push_notification_config(
    client: &Client,
    url: &str,
    task_id: &str,
    config_id: &str,
) -> Result<(), Box<dyn Error>> {
    let request =
        DeleteTaskPushNotificationConfigRequest::new(config_id.to_owned(), task_id.to_owned());
    let value = serde_json::to_value(&request)?;
    let result: Value = json_rpc(
        client,
        url,
        A2aMethod::DeleteTaskPushNotificationConfig,
        value,
    )
    .await?;
    let _response: DeleteTaskPushNotificationConfigResponse = serde_json::from_value(result)?;
    eprintln!("DeleteTaskPushNotificationConfig({task_id}, {config_id}) -> ok");
    Ok(())
}

/// `SendStreamingMessage` over SSE. POSTs to the
/// streaming endpoint, reads the response body as a
/// Server-Sent Events stream, and prints the
/// `state` of every status event. The example agent
/// emits a `working` event followed by a `completed`
/// event.
async fn send_streaming_message(
    client: &Client,
    url: &str,
    text: &str,
) -> Result<(), Box<dyn Error>> {
    let message = Message::new(MessageRole::User).with_part(Part::Text(TextPart {
        text: text.to_owned(),
        metadata: std::collections::BTreeMap::new(),
    }));
    let request = SendMessageRequest {
        message,
        configuration: None,
        metadata: std::collections::BTreeMap::new(),
    };
    let value = serde_json::to_value(&request)?;
    let events: Vec<StreamingEvent> =
        sse_post(client, url, A2aMethod::SendStreamingMessage, value).await?;
    let names = events
        .iter()
        .map(|e| match e {
            StreamingEvent::Status(s) => format!(
                "{} ({})",
                state_name(s.status.state),
                if s.is_final { "final" } else { "more" }
            ),
            StreamingEvent::Artifact(_) => "artifact".to_owned(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    eprintln!("SendStreamingMessage -> {names}");
    Ok(())
}

/// `SubscribeToTask` over SSE. The same wire format as
/// `SendStreamingMessage`, but the request is a
/// `GetTaskRequest` and the response is a single
/// final event with the task's current state. The
/// example's task is already cancelled (from
/// `cancel_task`), so the event carries that state.
async fn subscribe_to_task(
    client: &Client,
    url: &str,
    task_id: &str,
) -> Result<(), Box<dyn Error>> {
    let request = GetTaskRequest {
        id: task_id.to_owned(),
        history_length: None,
        metadata: std::collections::BTreeMap::new(),
    };
    let value = serde_json::to_value(&request)?;
    let events: Vec<StreamingEvent> =
        sse_post(client, url, A2aMethod::SubscribeToTask, value).await?;
    if let Some(StreamingEvent::Status(s)) = events.first() {
        eprintln!(
            "SubscribeToTask({task_id}) -> {} (final={})",
            state_name(s.status.state),
            s.is_final
        );
    } else {
        eprintln!("SubscribeToTask({task_id}) -> (no events)");
    }
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

/// Sends one JSON-RPC 2.0 request to a streaming
/// endpoint and reads the Server-Sent Events response
/// body. Each event is `data: <json-rpc-response>\n\n`
/// per the A2A streaming serve adapter; this helper
/// splits the body on blank lines, takes the `data:`
/// prefix off, parses the JSON-RPC envelope, and
/// collects the `result` field into a vector of typed
/// values. The whole body is read at once because the
/// example's streams are short (one or two events); a
/// real consumer that processes events as they arrive
/// would use `response.bytes_stream()` and a line
/// buffer instead. The shape is the same — the
/// library's streaming serve produces one
/// JSON-RPC envelope per `data:` line.
async fn sse_post<T: DeserializeOwned>(
    client: &Client,
    url: &str,
    method: A2aMethod,
    params: Value,
) -> Result<Vec<T>, Box<dyn Error>> {
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
        .header(reqwest::header::ACCEPT, "text/event-stream")
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let mut events = Vec::new();
    for block in body.split("\n\n") {
        for line in block.lines() {
            let Some(data) = line.strip_prefix("data: ") else {
                continue;
            };
            let envelope: Value = serde_json::from_str(data)?;
            if let Some(error) = envelope.get("error") {
                return Err(format!("JSON-RPC error: {error}").into());
            }
            let Some(result) = envelope.get("result") else {
                continue;
            };
            events.push(serde_json::from_value(result.clone())?);
        }
    }
    Ok(events)
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

/// Renders a `TaskState` to its wire form (`working`,
/// `completed`, `cancelled`, ...). The library's
/// `TaskState` derives `Serialize` with
/// `rename_all = "snake_case"`, so the same shape that
/// appears in the JSON-RPC envelope is what the
/// helper produces. A real consumer that wanted a
/// user-facing label would map the wire form to a
/// translated string; for the example, the wire form
/// is enough.
fn state_name(state: TaskState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|v| v.as_str().map(std::borrow::ToOwned::to_owned))
        .unwrap_or_else(|| "(unknown)".to_owned())
}

/// GETs a URL and parses the response body as JSON into
/// a typed value. Reads the body as text first because
/// the workspace's `reqwest` is configured without the
/// `json` feature; a `text` round-trip plus a
/// `serde_json::from_str` is the smallest dependency
/// shape the example can use.
async fn fetch_json<T: DeserializeOwned>(client: &Client, url: &str) -> Result<T, Box<dyn Error>> {
    let body = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    Ok(serde_json::from_str(&body)?)
}
