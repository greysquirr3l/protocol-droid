use std::sync::{Arc, Mutex, PoisonError};

use super::{ListKind, ServerHandle};
use crate::application::server::Server;
use crate::application::server::builder::{PromptHandler, ResourceHandler, ToolHandler};
use crate::application::session::{DynFrameSink, FrameSink, SinkError};
use crate::domain::ir::JsonObject;
use crate::domain::registry::{Prompt, PrimitiveKind, Resource, ResourceTemplate, Tool};
use crate::domain::{Feature, FeatureProfile};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Records every frame it is handed, in order.
#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);

impl Recorder {
    fn frames(&self) -> Vec<String> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl FrameSink for Recorder {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "FrameSink is a native async fn trait; the signature is not ours to change"
    )]
    async fn send(&self, frame: &str) -> Result<(), SinkError> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(frame.to_owned());
        Ok(())
    }
}

fn noop_tool() -> ToolHandler {
    Arc::new(|_ctx, _args| Box::pin(async { JsonObject::default() }))
}

fn noop_reader() -> ResourceHandler {
    Arc::new(|_ctx, _uri| Box::pin(async { Ok(JsonObject::default()) }))
}

fn noop_getter() -> PromptHandler {
    Arc::new(|_ctx, _args| Box::pin(async { Ok(JsonObject::default()) }))
}

fn everything() -> FeatureProfile {
    FeatureProfile::empty()
        .with(Feature::ToolsListChanged)
        .with(Feature::ResourcesListChanged)
        .with(Feature::PromptsListChanged)
}

fn sink() -> (Arc<Recorder>, Arc<dyn DynFrameSink>) {
    let recorder = Arc::new(Recorder::default());
    let dynamic: Arc<dyn DynFrameSink> = Arc::clone(&recorder) as Arc<dyn DynFrameSink>;
    (recorder, dynamic)
}

fn handle() -> (Server, ServerHandle) {
    let server = Server::builder("live").build();
    let handle = server.handle();
    (server, handle)
}

#[tokio::test]
async fn a_tool_added_while_serving_is_listed_and_callable() -> TestResult {
    let (server, handle) = handle();
    let _ = handle
        .add_tool(Tool::new("late", r#"{"type":"object"}"#)?, noop_tool())
        .await;
    assert_eq!(server.tool_count(), 1);
    assert!(server.handler("late").is_some());
    assert_eq!(server.registry().snapshot().len(PrimitiveKind::Tool), 1);
    Ok(())
}

#[tokio::test]
async fn a_removed_tool_is_neither_listed_nor_callable() -> TestResult {
    let (server, handle) = handle();
    let _ = handle
        .add_tool(Tool::new("gone", r#"{"type":"object"}"#)?, noop_tool())
        .await;
    let _ = handle.remove_tool("gone").await;
    assert!(server.handler("gone").is_none());
    assert_eq!(server.registry().snapshot().len(PrimitiveKind::Tool), 0);
    Ok(())
}

#[tokio::test]
async fn re_adding_a_tool_replaces_its_handler_rather_than_duplicating_it() -> TestResult {
    let (server, handle) = handle();
    for _ in 0..3 {
        let _ = handle
            .add_tool(Tool::new("twice", r#"{"type":"object"}"#)?, noop_tool())
            .await;
    }
    assert_eq!(server.tool_count(), 1);
    Ok(())
}

#[tokio::test]
async fn a_subscribed_session_is_told_once_per_change() -> TestResult {
    let (_server, handle) = handle();
    let (recorder, dynamic) = sink();
    let _guard = handle.subscribe(&dynamic, everything(), &ListKind::ALL, None);

    let fanout = handle
        .add_tool(Tool::new("t", r#"{"type":"object"}"#)?, noop_tool())
        .await;
    assert_eq!(fanout.sent, 1);
    assert_eq!(
        recorder.frames(),
        [r#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#]
    );
    Ok(())
}

#[tokio::test]
async fn removing_what_was_never_there_announces_nothing() -> TestResult {
    let (_server, handle) = handle();
    let (recorder, dynamic) = sink();
    let _guard = handle.subscribe(&dynamic, everything(), &ListKind::ALL, None);
    let fanout = handle.remove_prompt("absent").await;
    assert!(fanout.is_silent());
    assert!(recorder.frames().is_empty());
    Ok(())
}

#[tokio::test]
async fn a_session_without_the_capability_is_not_told() -> TestResult {
    let (_server, handle) = handle();
    let (recorder, dynamic) = sink();
    let _guard = handle.subscribe(&dynamic, FeatureProfile::empty(), &ListKind::ALL, None);
    let fanout = handle
        .add_tool(Tool::new("t", r#"{"type":"object"}"#)?, noop_tool())
        .await;
    assert_eq!(fanout.gated, 1);
    assert!(recorder.frames().is_empty());
    Ok(())
}

#[tokio::test]
async fn a_session_is_told_only_about_the_kinds_it_asked_for() -> TestResult {
    let (_server, handle) = handle();
    let (recorder, dynamic) = sink();
    let _guard = handle.subscribe(&dynamic, everything(), &[ListKind::Prompts], None);
    let _ = handle
        .add_tool(Tool::new("t", r#"{"type":"object"}"#)?, noop_tool())
        .await;
    assert!(recorder.frames().is_empty(), "a tool change leaked");
    let _ = handle.add_prompt(Prompt::new("p"), noop_getter()).await;
    assert_eq!(recorder.frames().len(), 1);
    Ok(())
}

#[tokio::test]
async fn templates_are_announced_as_a_resource_list_change() -> TestResult {
    let (_server, handle) = handle();
    let (recorder, dynamic) = sink();
    let _guard = handle.subscribe(&dynamic, everything(), &[ListKind::Resources], None);
    let _ = handle
        .add_resource_template(ResourceTemplate::new("file:///{path}", "files"))
        .await;
    let _ = handle
        .add_resource(Resource::new("file:///a", "a"), noop_reader())
        .await;
    assert_eq!(
        recorder.frames(),
        [
            r#"{"jsonrpc":"2.0","method":"notifications/resources/list_changed"}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/resources/list_changed"}"#,
        ]
    );
    Ok(())
}

#[tokio::test]
async fn dropping_the_subscription_stops_the_notifications() -> TestResult {
    let (_server, handle) = handle();
    let (recorder, dynamic) = sink();
    let guard = handle.subscribe(&dynamic, everything(), &ListKind::ALL, None);
    assert_eq!(handle.subscriber_count(), 3);
    drop(guard);
    assert_eq!(handle.subscriber_count(), 0);
    let _ = handle
        .add_tool(Tool::new("t", r#"{"type":"object"}"#)?, noop_tool())
        .await;
    assert!(recorder.frames().is_empty());
    Ok(())
}

#[tokio::test]
async fn meta_is_carried_in_the_notification_params() -> TestResult {
    let (_server, handle) = handle();
    let (recorder, dynamic) = sink();
    let meta = serde_json::json!({ "io.modelcontextprotocol/subscriptionId": 7 });
    let _guard = handle.subscribe(&dynamic, everything(), &[ListKind::Tools], Some(&meta));
    let _ = handle
        .add_tool(Tool::new("t", r#"{"type":"object"}"#)?, noop_tool())
        .await;
    let frames = recorder.frames();
    let first = frames.first().ok_or("no frame")?;
    let parsed: serde_json::Value = serde_json::from_str(first)?;
    assert_eq!(
        parsed.pointer("/params/_meta/io.modelcontextprotocol~1subscriptionId"),
        Some(&serde_json::json!(7))
    );
    Ok(())
}
