//! A tool that asks the peer a question, served over a real pair of pipes.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::adapters::primary::stdio::{Diagnostics, ServeLoop, StdioConfig};
use crate::application::jsonrpc::AllowBatching;
use crate::application::server::{Dispatcher, PeerSession, Server, ToolHandler};
use crate::application::session::FrameSink;
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
    async fn send(&self, _frame: &str) -> Result<(), crate::application::session::SinkError> {
        Err(crate::application::session::SinkError)
    }
}

struct NoCallbacks;

impl crate::application::server::DynExchange for NoCallbacks {
    fn exchange_boxed<'a>(
        &'a self,
        _method: &'a str,
        _params: &'a JsonObject,
    ) -> core::pin::Pin<
        Box<
            dyn Future<Output = Result<JsonObject, crate::application::server::ExchangeError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async { Err(crate::application::server::ExchangeError::new("unused")) })
    }
}

fn asking_tool() -> ToolHandler {
    Arc::new(|ctx, _args| {
        Box::pin(async move {
            let ask = JsonObject::new(r#"{"message":"Name?","requestedSchema":{"type":"object"}}"#)
                .unwrap_or_default();
            let said = match ctx.elicit(&ask).await {
                Ok(answer) => answer.as_str().to_owned(),
                Err(error) => format!("failed: {error}"),
            };
            JsonObject::from_value(json!({ "content": [{ "type": "text", "text": said }] }))
                .unwrap_or_default()
        })
    })
}

async fn line(
    reader: &mut (impl AsyncBufReadExt + Unpin),
) -> Result<Value, Box<dyn std::error::Error>> {
    let mut text = String::new();
    tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut text)).await??;
    Ok(serde_json::from_str(text.trim())?)
}

#[tokio::test]
async fn a_tool_can_elicit_over_stdio_and_replies_keep_their_order() -> TestResult {
    let (client_out, server_in) = tokio::io::duplex(64 * 1024);
    let (server_out, client_in) = tokio::io::duplex(64 * 1024);
    let server = Server::builder("stdio-exchange")
        .tool(Tool::new("ask", r#"{"type":"object"}"#)?, asking_tool())
        .build();
    let version = ProtocolVersion::V2025_06_18;
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
        Arc::new(NoCallbacks),
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
    let serving = tokio::spawn(async move { serve_loop.serve().await });

    let mut writer = client_out;
    let mut reader = BufReader::new(client_in);
    let send = |value: Value| format!("{value}\n");

    writer
        .write_all(
            send(json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": { "elicitation": {} },
                    "clientInfo": { "name": "test", "version": "1" },
                },
            }))
            .as_bytes(),
        )
        .await?;
    let initialized = line(&mut reader).await?;
    assert_eq!(initialized.get("id"), Some(&json!(1)));

    writer
        .write_all(
            send(json!({
                "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": { "name": "ask", "arguments": {} },
            }))
            .as_bytes(),
        )
        .await?;
    let question = line(&mut reader).await?;
    assert_eq!(question.get("method"), Some(&json!("elicitation/create")));
    let asked = question
        .get("id")
        .cloned()
        .ok_or("the question has no id")?;

    // A request sent while the tool waits is queued behind it, not answered first.
    writer
        .write_all(send(json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" })).as_bytes())
        .await?;
    writer
        .write_all(
            send(json!({
                "jsonrpc": "2.0", "id": asked,
                "result": { "action": "accept", "content": { "name": "Ada" } },
            }))
            .as_bytes(),
        )
        .await?;

    let answered = line(&mut reader).await?;
    assert_eq!(answered.get("id"), Some(&json!(2)), "{answered}");
    let text = answered
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .ok_or("no text")?;
    assert!(text.contains("Ada"), "{text}");
    let listed = line(&mut reader).await?;
    assert_eq!(listed.get("id"), Some(&json!(3)));

    drop(writer);
    let report = tokio::time::timeout(Duration::from_secs(5), serving).await??;
    assert!(report.is_ok(), "{report:?}");
    Ok(())
}
