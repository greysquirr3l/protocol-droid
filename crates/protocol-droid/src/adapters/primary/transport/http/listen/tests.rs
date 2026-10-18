//! `subscriptions/listen`, over the real router.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use futures_util::StreamExt as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;

use crate::adapters::primary::transport::http::config::{ServerConfig, Streaming};
use crate::adapters::primary::transport::http::serve::test_routes;
use crate::application::server::{Server, ServerHandle};
use crate::domain::ProtocolVersion;
use crate::domain::ir::JsonObject;
use crate::domain::registry::{Prompt, Tool};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn served() -> Result<(axum::Router, ServerHandle), Box<dyn std::error::Error>> {
    let server = Server::builder("listen").build();
    let handle = server.handle();
    let config = ServerConfig::new()?.with_streaming(Streaming::EventStream);
    let router = test_routes(server, ProtocolVersion::V2025_11_25, Some(config), None)?;
    Ok((router, handle))
}

fn listen(id: i64, filter: &Value) -> Request<Body> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "subscriptions/listen",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {},
            },
            "notifications": filter,
        },
    });
    Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "localhost")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2026-07-28")
        .header("mcp-method", "subscriptions/listen")
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| Request::new(Body::empty()))
}

/// Reads frames off a live stream until `count` arrive or the wait runs out.
async fn frames(
    stream: &mut (impl futures_util::Stream<Item = Result<axum::body::Bytes, axum::Error>> + Unpin),
    count: usize,
) -> Vec<Value> {
    let mut found = Vec::new();
    let mut pending = String::new();
    while found.len() < count {
        let Ok(Some(Ok(chunk))) = tokio::time::timeout(Duration::from_secs(2), stream.next()).await
        else {
            break;
        };
        pending.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(end) = pending.find("\n\n") {
            let event: String = pending.drain(..end + 2).collect();
            for line in event.lines() {
                if let Some(data) = line.strip_prefix("data:")
                    && let Ok(value) = serde_json::from_str(data.trim())
                {
                    found.push(value);
                }
            }
        }
    }
    found
}

fn noop() -> crate::application::server::ToolHandler {
    Arc::new(|_ctx, _args| Box::pin(async { JsonObject::default() }))
}

#[tokio::test]
async fn the_acknowledgement_comes_first_and_carries_the_request_id() -> TestResult {
    let (router, handle) = served()?;
    let response = router.oneshot(listen(77, &json!({ "toolsListChanged": true }))).await?;
    let mut stream = response.into_body().into_data_stream();
    let first = frames(&mut stream, 1).await;
    let ack = first.first().ok_or("no acknowledgement")?;
    assert_eq!(
        ack.get("method"),
        Some(&json!("notifications/subscriptions/acknowledged"))
    );
    assert_eq!(
        ack.pointer("/params/_meta/io.modelcontextprotocol~1subscriptionId"),
        Some(&json!(77)),
        "the subscription id is the request's own id"
    );
    assert_eq!(
        ack.pointer("/params/notifications"),
        Some(&json!({ "toolsListChanged": true }))
    );

    let _ = handle
        .add_tool(Tool::new("late", r#"{"type":"object"}"#)?, noop())
        .await;
    let next = frames(&mut stream, 1).await;
    let change = next.first().ok_or("no change notification")?;
    assert_eq!(change.get("method"), Some(&json!("notifications/tools/list_changed")));
    assert_eq!(
        change.pointer("/params/_meta/io.modelcontextprotocol~1subscriptionId"),
        Some(&json!(77))
    );
    Ok(())
}

#[tokio::test]
async fn a_stream_filtered_to_prompts_hears_nothing_about_tools() -> TestResult {
    let (router, handle) = served()?;
    let response = router
        .oneshot(listen(5, &json!({ "promptsListChanged": true })))
        .await?;
    let mut stream = response.into_body().into_data_stream();
    let _ack = frames(&mut stream, 1).await;

    let _ = handle
        .add_tool(Tool::new("t", r#"{"type":"object"}"#)?, noop())
        .await;
    let _ = handle
        .add_prompt(
            Prompt::new("p"),
            Arc::new(|_ctx, _args| Box::pin(async { Ok(JsonObject::default()) })),
        )
        .await;
    let next = frames(&mut stream, 1).await;
    let change = next.first().ok_or("no change notification")?;
    assert_eq!(
        change.get("method"),
        Some(&json!("notifications/prompts/list_changed")),
        "the first frame after the ack is the prompt change, not the tool change"
    );
    Ok(())
}

#[tokio::test]
async fn an_unsupported_filter_member_is_not_acknowledged() -> TestResult {
    let (router, _handle) = served()?;
    let response = router
        .oneshot(listen(
            6,
            &json!({ "toolsListChanged": true, "resourceSubscriptions": ["test://x"] }),
        ))
        .await?;
    let mut stream = response.into_body().into_data_stream();
    let ack = frames(&mut stream, 1).await;
    let honoured = ack
        .first()
        .and_then(|ack| ack.pointer("/params/notifications"))
        .ok_or("no acknowledgement")?;
    assert!(honoured.get("resourceSubscriptions").is_none(), "{honoured}");
    Ok(())
}

#[tokio::test]
async fn dropping_the_stream_ends_the_subscription() -> TestResult {
    let (router, handle) = served()?;
    let response = router.oneshot(listen(8, &json!({ "toolsListChanged": true }))).await?;
    assert_eq!(handle.subscriber_count(), 1);
    drop(response);
    assert_eq!(handle.subscriber_count(), 0);
    Ok(())
}
