//! Drives an A2A peer through the typed `A2aClient` port.
//!
//! The matching raw-reqwest example is
//! [`a2a_client.rs`](https://github.com/greysquirr3l/protocol-droid/blob/main/crates/protocol-droid/examples/a2a_client.rs):
//! that file is five hundred lines of envelope
//! munging, SSE parsing, and an id counter. This
//! file is the typed alternative — `A2aClient`
//! takes the request types the library defines
//! and returns the response types the library
//! parses, so a consumer matches on real A2A
//! types, not on `serde_json::Value`.
//!
//! Run with the [`a2a_server`](https://github.com/greysquirr3l/protocol-droid/blob/main/crates/protocol-droid/examples/a2a_server.rs)
//! example in another terminal:
//!
//! ```text
//! $ cargo run -p protocol-droid --example a2a_server
//! a2a_server: listening on 127.0.0.1:8080
//! a2a_server: well-known at http://127.0.0.1:8080/.well-known/agent-card.json
//! a2a_server: JSON-RPC at http://127.0.0.1:8080/a2a
//! a2a_server: streaming at http://127.0.0.1:8080/a2a/stream
//! $ cargo run -p protocol-droid --example a2a_typed_client
//! ```
//!
//! Set `A2A_BEARER_TOKEN=secret` on both sides to exercise
//! the bearer-token path:
//!
//! ```text
//! $ A2A_BEARER_TOKEN=secret cargo run -p protocol-droid --example a2a_server
//! $ A2A_BEARER_TOKEN=secret cargo run -p protocol-droid --example a2a_typed_client
//! ```

use std::error::Error;

use protocol_droid::adapters::secondary::a2a::A2aClient;
use protocol_droid::domain::a2a::message::{Message, MessageRole, Part, TextPart};
use protocol_droid::domain::a2a::push_notification::TaskPushNotificationConfig;
use protocol_droid::domain::a2a::push_notification_methods::{
    CreateTaskPushNotificationConfigRequest, DeleteTaskPushNotificationConfigRequest,
    GetTaskPushNotificationConfigRequest, ListTaskPushNotificationConfigsRequest,
};
use protocol_droid::domain::a2a::send_message::{SendMessageRequest, SendMessageResponse};
use protocol_droid::domain::a2a::streaming_events::StreamingEvent;
use protocol_droid::domain::a2a::task_lifecycle::{CancelTaskRequest, GetTaskRequest};
use protocol_droid::domain::a2a::list_tasks::ListTasksRequest;

const PEER: &str = "http://127.0.0.1:8080";

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    // The peer is started with `A2A_BEARER_TOKEN=secret`
    // when an auth demonstration is wanted. The
    // client reads the same env var so the two
    // sides match; a peer that does not require
    // auth ignores the header and the request
    // still works.
    let client = match std::env::var("A2A_BEARER_TOKEN")
        .ok()
        .filter(|value| !value.is_empty())
    {
        Some(token) => A2aClient::new(PEER)?.with_bearer_token(token),
        None => A2aClient::new(PEER)?,
    };

    // `card` returns the agent's `AgentCard`. The
    // library does not cache the card; a consumer
    // that wants the card once calls it once.
    let card = client.card().await?;
    eprintln!("discovered {} v{}", card.name, card.version);

    // `send_message` takes the request type and
    // returns the typed response. The example
    // agent replies with a `Task` whose state
    // is `Completed`.
    let message = Message::new(MessageRole::User).with_part(Part::Text(TextPart {
        text: "hello from a2a_typed_client".to_owned(),
        metadata: std::collections::BTreeMap::new(),
    }));
    let request = SendMessageRequest::new(message);
    let response = client.send_message(request).await?;
    let task_id = match &response {
        SendMessageResponse::Task(task) => task.id.to_string(),
        SendMessageResponse::Message(_) => {
            return Err("peer returned a Message, expected a Task".into());
        }
    };
    eprintln!("SendMessage -> task {task_id}");

    // `get_task`, `cancel_task`, `list_tasks` are
    // the same one-line shape: typed request in,
    // typed response out.
    let get = client.get_task(GetTaskRequest::new(task_id.clone())).await?;
    eprintln!(
        "GetTask({task_id}) -> state {:?}",
        get.0.status.state
    );

    let listed = client
        .list_tasks(ListTasksRequest::default())
        .await?;
    eprintln!("ListTasks -> {} tasks", listed.tasks.len());

    let cancelled = client
        .cancel_task(CancelTaskRequest::new(task_id.clone()))
        .await?;
    eprintln!(
        "CancelTask({task_id}) -> state {:?}",
        cancelled.0.status.state
    );

    // `send_streaming_message` is the streaming
    // path. The library returns a `Vec` of typed
    // events; a consumer that wants events one
    // at a time as they arrive wraps the call.
    let message = Message::new(MessageRole::User).with_part(Part::Text(TextPart {
        text: "hello via streaming".to_owned(),
        metadata: std::collections::BTreeMap::new(),
    }));
    let request = SendMessageRequest::new(message);
    let events = client.send_streaming_message(request).await?;
    eprintln!("SendStreamingMessage -> {} events", events.len());
    for (i, event) in events.iter().enumerate() {
        let StreamingEvent::Status(status) = event else {
            eprintln!("  [{i}] artifact event");
            continue;
        };
        eprintln!("  [{i}] status {:?} (final = {})", status.status.state, status.is_final);
    }

    // The four push-notification-config methods
    // round-trip a `TaskPushNotificationConfig`.
    let config = TaskPushNotificationConfig {
        id: "placeholder".to_owned(),
        task_id: "t-1".to_owned(),
        // The URL is on the SSRF denylist of the
        // example server's webhook adapter; the
        // example is about the config CRUD, not
        // the delivery.
        url: "http://1.2.3.4:9999/wh".to_owned(),
        token: Some("token".to_owned()),
        authentication: None,
        metadata: std::collections::BTreeMap::new(),
    };
    let created = client
        .create_push_notification_config(CreateTaskPushNotificationConfigRequest::from(config))
        .await?;
    eprintln!("CreateTaskPushNotificationConfig -> {}", created.0.id);

    let got = client
        .get_push_notification_config(GetTaskPushNotificationConfigRequest::new(
            created.0.id.clone(),
            "t-1".to_owned(),
        ))
        .await?;
    eprintln!("GetTaskPushNotificationConfig -> ok");

    let listed = client
        .list_push_notification_configs(ListTaskPushNotificationConfigsRequest {
            task_id: "t-1".to_owned(),
            page_size: None,
            page_token: None,
        })
        .await?;
    eprintln!(
        "ListTaskPushNotificationConfigs -> {} configs",
        listed.configs.len()
    );

    client
        .delete_push_notification_config(DeleteTaskPushNotificationConfigRequest::new(
            got.0.id,
            "t-1".to_owned(),
        ))
        .await?;
    eprintln!("DeleteTaskPushNotificationConfig -> ok");

    Ok(())
}
