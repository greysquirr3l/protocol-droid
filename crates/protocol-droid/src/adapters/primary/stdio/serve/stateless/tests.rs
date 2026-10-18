//! The stateless lifecycle and cancellation on stdio, over a pair of pipes.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

use crate::adapters::primary::stdio::serve::ServeLoop;
use crate::adapters::primary::stdio::{Diagnostics, StdioConfig};
use crate::application::jsonrpc::AllowBatching;
use crate::application::server::Dispatcher;
use crate::application::server::{
    DynExchange, ExchangeError, PeerSession, Server, ServerHandle, ToolHandler,
};
use crate::application::session::{FrameSink, SinkError};
use crate::domain::ir::JsonObject;
use crate::domain::registry::Tool;
use crate::domain::session::SessionId;
use crate::domain::{CapabilitySet, Negotiation, ProtocolVersion, TransportKind, resolve};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Unused;

impl FrameSink for Unused {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "FrameSink is a native async fn trait; the signature is not ours to change"
    )]
    async fn send(&self, _frame: &str) -> Result<(), SinkError> {
        Err(SinkError)
    }
}

struct Refuses;

impl DynExchange for Refuses {
    fn exchange_boxed<'a>(
        &'a self,
        _method: &'a str,
        _params: &'a JsonObject,
    ) -> core::pin::Pin<Box<dyn Future<Output = Result<JsonObject, ExchangeError>> + Send + 'a>>
    {
        Box::pin(async { Err(ExchangeError::new("unused")) })
    }
}

/// Waits until its request is cancelled, then answers anyway — the answer the
/// loop must not send.
fn waits_for_cancel() -> ToolHandler {
    Arc::new(|ctx, _args| {
        Box::pin(async move {
            for _ in 0..200 {
                if ctx.cancellation().is_cancelled() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            JsonObject::default()
        })
    })
}

struct Pipes {
    writer: DuplexStream,
    reader: BufReader<DuplexStream>,
    handle: ServerHandle,
}

impl Pipes {
    async fn send(&mut self, value: Value) -> TestResult {
        self.writer
            .write_all(format!("{value}\n").as_bytes())
            .await?;
        Ok(())
    }

    async fn read(&mut self) -> Result<Value, Box<dyn std::error::Error>> {
        let mut text = String::new();
        tokio::time::timeout(Duration::from_secs(5), self.reader.read_line(&mut text)).await??;
        Ok(serde_json::from_str(text.trim())?)
    }
}

fn start() -> Result<Pipes, Box<dyn std::error::Error>> {
    let (client_out, server_in) = tokio::io::duplex(64 * 1024);
    let (server_out, client_in) = tokio::io::duplex(64 * 1024);
    let server = Server::builder("stdio-stateless")
        .tool(
            Tool::new("slow", r#"{"type":"object"}"#)?,
            waits_for_cancel(),
        )
        .build();
    let handle = server.handle();
    let version = ProtocolVersion::V2025_11_25;
    let peer = PeerSession::new(
        version,
        resolve(Negotiation {
            version,
            client_caps: CapabilitySet::empty(),
            server_caps: CapabilitySet::all_for(version),
            transport: TransportKind::Stdio,
        }),
        SessionId::new(1),
        TransportKind::Stdio,
        Arc::new(Refuses),
        Arc::new(Unused),
    );
    let mut serve_loop = ServeLoop::new(
        server_in,
        server_out,
        Diagnostics::new(tokio::io::sink()),
        StdioConfig::default(),
        Arc::new(Dispatcher::new(Arc::new(server))),
        peer,
        AllowBatching,
    );
    tokio::spawn(async move { serve_loop.serve().await });
    Ok(Pipes {
        writer: client_out,
        reader: BufReader::new(client_in),
        handle,
    })
}

fn meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {},
    })
}

#[tokio::test]
async fn discover_needs_no_handshake() -> TestResult {
    let mut pipes = start()?;
    pipes
        .send(json!({ "jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": { "_meta": meta() } }))
        .await?;
    let answer = pipes.read().await?;
    assert!(
        answer.pointer("/result/supportedVersions").is_some(),
        "{answer}"
    );
    assert!(
        answer
            .pointer("/result/_meta/io.modelcontextprotocol~1serverInfo")
            .is_some()
    );
    Ok(())
}

#[tokio::test]
async fn refusals_carry_the_revision_codes() -> TestResult {
    let mut pipes = start()?;
    pipes
        .send(
            json!({ "jsonrpc": "2.0", "id": 2, "method": "server/discover",
            "params": { "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28" } } }),
        )
        .await?;
    assert_eq!(
        pipes.read().await?.pointer("/error/code"),
        Some(&json!(-32602))
    );

    pipes
        .send(
            json!({ "jsonrpc": "2.0", "id": 3, "method": "server/discover",
            "params": { "_meta": { "io.modelcontextprotocol/protocolVersion": "v999.0.0",
                "io.modelcontextprotocol/clientCapabilities": {} } } }),
        )
        .await?;
    let unsupported = pipes.read().await?;
    assert_eq!(unsupported.pointer("/error/code"), Some(&json!(-32022)));
    assert_eq!(
        unsupported.pointer("/error/data/requested"),
        Some(&json!("v999.0.0"))
    );

    pipes
        .send(json!({ "jsonrpc": "2.0", "id": 4, "method": "ping", "params": { "_meta": meta() } }))
        .await?;
    assert_eq!(
        pipes.read().await?.pointer("/error/code"),
        Some(&json!(-32601))
    );
    Ok(())
}

#[tokio::test]
async fn a_listen_stream_is_acknowledged_tagged_and_cancelled() -> TestResult {
    let mut pipes = start()?;
    pipes
        .send(
            json!({ "jsonrpc": "2.0", "id": "watch", "method": "subscriptions/listen",
            "params": { "_meta": meta(), "notifications": { "toolsListChanged": true } } }),
        )
        .await?;
    let ack = pipes.read().await?;
    assert_eq!(
        ack.get("method"),
        Some(&json!("notifications/subscriptions/acknowledged"))
    );
    assert_eq!(
        ack.pointer("/params/_meta/io.modelcontextprotocol~1subscriptionId"),
        Some(&json!("watch"))
    );

    let handle = pipes.handle.clone();
    let _ = handle
        .add_tool(
            Tool::new("late", r#"{"type":"object"}"#)?,
            waits_for_cancel(),
        )
        .await;
    let change = pipes.read().await?;
    assert_eq!(
        change.get("method"),
        Some(&json!("notifications/tools/list_changed"))
    );
    assert_eq!(
        change.pointer("/params/_meta/io.modelcontextprotocol~1subscriptionId"),
        Some(&json!("watch"))
    );

    pipes
        .send(json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": "watch" } }))
        .await?;
    // A request after the cancellation is answered; once it is, the cancel has
    // been processed and no later change may arrive on the dead stream.
    pipes
        .send(json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/list", "params": { "_meta": meta() } }))
        .await?;
    assert_eq!(pipes.read().await?.get("id"), Some(&json!(5)));
    let _ = handle.remove_tool("late").await;
    assert_eq!(handle.subscriber_count(), 0);
    Ok(())
}

#[tokio::test]
async fn a_cancelled_request_is_not_answered() -> TestResult {
    let mut pipes = start()?;
    pipes
        .send(json!({ "jsonrpc": "2.0", "id": 6, "method": "tools/call",
            "params": { "_meta": meta(), "name": "slow", "arguments": {} } }))
        .await?;
    pipes
        .send(json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": 6 } }))
        .await?;
    pipes
        .send(json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/list", "params": { "_meta": meta() } }))
        .await?;
    let next = pipes.read().await?;
    assert_eq!(
        next.get("id"),
        Some(&json!(7)),
        "the cancelled call was answered: {next}"
    );
    Ok(())
}
