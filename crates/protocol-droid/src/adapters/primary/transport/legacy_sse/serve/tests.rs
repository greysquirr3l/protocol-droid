//! A real `Server` over the legacy transport, driven by this crate's own
//! legacy client.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

use crate::adapters::primary::transport::http::{HttpBuilder, ServerConfig};
use crate::adapters::secondary::transport::legacy_sse::{
    ClientFrame, LegacySseClient, LegacySseConfig,
};
use crate::application::client::Client;
use crate::application::server::Server;
use crate::application::session::IdSource;
use crate::domain::ir::JsonObject;
use crate::domain::registry::Tool;
use crate::domain::session::SessionId;
use crate::domain::{ProtocolVersion, TransportKind, TransportSpec};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Counting(AtomicU64);

impl IdSource for Counting {
    fn next_session_id(&self) -> SessionId {
        SessionId::new(self.0.fetch_add(1, Ordering::SeqCst))
    }
}

fn echo_server() -> Result<Server, Box<dyn std::error::Error>> {
    Ok(Server::builder("legacy-peer")
        .tool(
            Tool::new(
                "echo",
                r#"{"type":"object","properties":{"text":{"type":"string"}}}"#,
            )?,
            Arc::new(|_ctx, args| {
                Box::pin(async move {
                    JsonObject::from_value(json!({
                        "content": [{ "type": "text", "text": args.as_str() }],
                    }))
                    .unwrap_or_default()
                })
            }),
        )
        .build())
}

/// Serves `server` on both transports and returns the address.
async fn serve(server: Server) -> Result<std::net::SocketAddr, Box<dyn std::error::Error>> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let builder = HttpBuilder::new(server)
        .config(ServerConfig::new()?)
        .ids(Arc::new(Counting(AtomicU64::new(1))))
        .legacy_sse();
    tokio::spawn(async move {
        let _ = builder.serve_on(listener).await;
    });
    Ok(addr)
}

#[tokio::test]
async fn a_legacy_client_handshakes_lists_and_calls() -> TestResult {
    let addr = serve(echo_server()?).await?;
    let spec = TransportSpec::new(TransportKind::LegacySse, format!("http://{addr}/sse"))?;
    let client = Client::connect_to(&spec).await?;
    assert_eq!(client.negotiated(), ProtocolVersion::V2024_11_05);

    let listed: Value = serde_json::from_str(client.list_tools().await?.as_str())?;
    assert_eq!(listed.pointer("/tools/0/name"), Some(&json!("echo")));

    let called = client
        .call_tool("echo", &JsonObject::new(r#"{"text":"hello"}"#)?)
        .await?;
    assert!(called.as_str().contains("hello"), "{}", called.as_str());
    Ok(())
}

#[tokio::test]
async fn streamable_http_is_still_served_beside_it() -> TestResult {
    let addr = serve(echo_server()?).await?;
    let spec = TransportSpec::new(TransportKind::StreamableHttp, format!("http://{addr}/mcp"))?;
    let client = Client::connect_to(&spec).await?;
    let listed: Value = serde_json::from_str(client.list_tools().await?.as_str())?;
    assert_eq!(listed.pointer("/tools/0/name"), Some(&json!("echo")));
    Ok(())
}

/// Reads frames until one carries `method`, within a few seconds.
async fn next_with_method(
    client: &LegacySseClient,
    method: &str,
) -> Result<Value, Box<dyn std::error::Error>> {
    let deadline = std::time::Duration::from_secs(5);
    tokio::time::timeout(deadline, async {
        loop {
            match client.next().await? {
                Some(ClientFrame::Message(frame))
                    if frame.get("method") == Some(&json!(method)) =>
                {
                    return Ok(frame);
                }
                Some(_) => {}
                None => return Err("the stream closed".into()),
            }
        }
    })
    .await
    .map_err(|_| format!("no `{method}` arrived"))?
}

async fn open(addr: std::net::SocketAddr) -> Result<LegacySseClient, Box<dyn std::error::Error>> {
    Ok(LegacySseClient::connect(LegacySseConfig::new(&format!("http://{addr}/sse"))?).await?)
}

#[tokio::test]
async fn a_tool_added_at_runtime_is_announced_on_the_stream() -> TestResult {
    let server = echo_server()?;
    let handle = server.handle();
    let addr = serve(server).await?;
    let client = open(addr).await?;
    client
        .send(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#)
        .await?;
    // The reply, then the notification that follows `initialized`.
    next_with_reply(&client).await?;
    client
        .send(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
        .await?;

    let _ = handle
        .add_tool(
            Tool::new("late", r#"{"type":"object"}"#)?,
            Arc::new(|_ctx, _args| Box::pin(async { JsonObject::default() })),
        )
        .await;
    next_with_method(&client, "notifications/tools/list_changed").await?;
    Ok(())
}

async fn next_with_reply(client: &LegacySseClient) -> Result<Value, Box<dyn std::error::Error>> {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match client.next().await? {
                Some(ClientFrame::Message(frame)) if frame.get("result").is_some() => {
                    return Ok(frame);
                }
                Some(_) => {}
                None => return Err("the stream closed".into()),
            }
        }
    })
    .await
    .map_err(|_| "no reply arrived")?
}

#[tokio::test]
async fn a_request_before_initialize_is_refused_on_the_stream() -> TestResult {
    let addr = serve(echo_server()?).await?;
    let client = open(addr).await?;
    client
        .send(br#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#)
        .await?;
    let refusal = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match client.next().await? {
                Some(ClientFrame::Message(frame)) if frame.get("error").is_some() => {
                    return Ok(frame);
                }
                Some(_) => {}
                None => {
                    return Err::<Value, Box<dyn std::error::Error>>("the stream closed".into());
                }
            }
        }
    })
    .await
    .map_err(|_| "no refusal arrived")??;
    assert_eq!(refusal.pointer("/error/code"), Some(&json!(-32600)));
    assert_eq!(refusal.get("id"), Some(&json!(7)));
    Ok(())
}

#[tokio::test]
async fn a_client_offering_a_later_revision_is_answered_with_2024_11_05() -> TestResult {
    let addr = serve(echo_server()?).await?;
    let client = open(addr).await?;
    client
        .send(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#)
        .await?;
    let reply = next_with_reply(&client).await?;
    assert_eq!(
        reply.pointer("/result/protocolVersion"),
        Some(&json!("2024-11-05"))
    );
    Ok(())
}

/// A tool that waits on the client must not hold the `POST` that started it:
/// the answer it waits for arrives on a second `POST`.
#[tokio::test]
async fn a_tool_can_ask_the_client_and_wait_for_the_answer() -> TestResult {
    let server = Server::builder("asks")
        .tool(
            Tool::new("ask", r#"{"type":"object"}"#)?,
            Arc::new(|ctx, _args| {
                Box::pin(async move {
                    let params = JsonObject::new(
                        r#"{"messages":[{"role":"user","content":{"type":"text","text":"hi"}}],"maxTokens":8}"#,
                    )
                    .unwrap_or_default();
                    let text = match ctx.sample(&params).await {
                        Ok(answer) => answer.as_str().to_owned(),
                        Err(error) => format!("refused: {error}"),
                    };
                    JsonObject::from_value(json!({ "content": [{ "type": "text", "text": text }] }))
                        .unwrap_or_default()
                })
            }),
        )
        .build();
    let addr = serve(server).await?;
    let client = open(addr).await?;
    client
        .send(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{"sampling":{}},"clientInfo":{"name":"t","version":"0"}}}"#)
        .await?;
    next_with_reply(&client).await?;
    client
        .send(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
        .await?;
    client
        .send(br#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"ask","arguments":{}}}"#)
        .await?;

    let question = next_with_method(&client, "sampling/createMessage").await?;
    let asked = question.get("id").cloned().ok_or("the request had no id")?;
    let answer = json!({
        "jsonrpc": "2.0",
        "id": asked,
        "result": { "role": "assistant", "content": { "type": "text", "text": "pong" }, "model": "m" },
    });
    client.send(answer.to_string().as_bytes()).await?;

    let reply = next_with_reply(&client).await?;
    assert_eq!(reply.get("id"), Some(&json!(2)));
    assert!(reply.to_string().contains("pong"), "{reply}");
    Ok(())
}
