//! This crate's stateless client against this crate's server, over a socket.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::adapters::primary::transport::http::{HttpBuilder, ServerConfig};
use crate::application::client::Client;
use crate::application::server::Server;
use crate::domain::ir::JsonObject;
use crate::domain::registry::Tool;
use crate::domain::{ProtocolVersion, TransportKind, TransportSpec};

type TestResult = Result<(), Box<dyn std::error::Error>>;

async fn serve(server: Server) -> Result<String, Box<dyn std::error::Error>> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let builder = HttpBuilder::new(server).config(ServerConfig::new()?);
    tokio::spawn(async move {
        let _ = builder.serve_on(listener).await;
    });
    Ok(format!("http://{addr}/mcp"))
}

fn region_tool() -> Result<Tool, crate::domain::ir::JsonObjectError> {
    Tool::new(
        "regional",
        r#"{"type":"object","properties":{"region":{"type":"string","x-mcp-header":"Region"}},"required":["region"]}"#,
    )
}

#[tokio::test]
async fn a_client_negotiates_statelessly_and_mirrors_param_headers() -> TestResult {
    let server = Server::builder("stateless-peer")
        .tool(
            region_tool()?,
            Arc::new(|_ctx, args| {
                Box::pin(async move {
                    JsonObject::from_value(json!({
                        "content": [{ "type": "text", "text": args.as_str() }],
                    }))
                    .unwrap_or_default()
                })
            }),
        )
        .build();
    let url = serve(server).await?;
    let client = Client::connect_to(&TransportSpec::new(TransportKind::StatelessHttp, url)?).await?;
    assert_eq!(client.negotiated(), ProtocolVersion::V2026_07_28);

    let listed: Value = serde_json::from_str(client.list_tools().await?.as_str())?;
    assert_eq!(listed.pointer("/tools/0/name"), Some(&json!("regional")));

    // The server refuses a `tools/call` whose `Mcp-Param-Region` is missing or
    // disagrees with the body, so a result here proves the client sent it —
    // and sent it Base64-encoded, because the value is not plain ASCII.
    let called = client
        .call_tool("regional", &JsonObject::new(r#"{"region":"Zürich"}"#)?)
        .await?;
    assert!(called.as_str().contains('Z'), "{}", called.as_str());
    Ok(())
}

#[tokio::test]
async fn a_tool_with_an_invalid_annotation_is_dropped_from_the_listing() -> TestResult {
    let invalid = Tool::new(
        "bad",
        r#"{"type":"object","properties":{"ratio":{"type":"number","x-mcp-header":"Ratio"}}}"#,
    )?;
    let server = Server::builder("stateless-peer")
        .tool(region_tool()?, Arc::new(|_ctx, _args| Box::pin(async { JsonObject::default() })))
        .tool(invalid, Arc::new(|_ctx, _args| Box::pin(async { JsonObject::default() })))
        .build();
    let url = serve(server).await?;
    let client = Client::connect_to(&TransportSpec::new(TransportKind::StatelessHttp, url)?).await?;
    let listed: Value = serde_json::from_str(client.list_tools().await?.as_str())?;
    let names: Vec<&str> = listed
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect();
    assert_eq!(names, ["regional"]);
    Ok(())
}
