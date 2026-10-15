//! An MCP client that connects to a peer over Streamable HTTP.
//!
//! The stdio counterpart is
//! [`mcp_client.rs`](protocol_droid::adapters::secondary::stdio::ClientConfig);
//! this file shows the **HTTP** counterpart — the same
//! `Client::list_tools` / `Client::call_tool` API, but reached
//! through `Client::connect_to(spec)` rather than
//! `Client::connect_stdio(spec)`.
//!
//! # Why this exists
//!
//! `Client::connect_to(spec)` takes a `&TransportSpec` parsed
//! from a `kind:target` string, where the `kind` is one of
//! `http://...` (Streamable HTTP), `stdio:...` (stdio, refused
//! here because that path is what `connect_stdio` is for), or
//! the legacy SSE form. The consumer's first question on the
//! HTTP path is "what does the parse look like, and what
//! comes back from `connect_to`?" — both are visible in this
//! file.
//!
//! # What it does
//!
//! 1. Parses the peer URL from the `MCP_PEER` env var
//!    (default `http://127.0.0.1:3000/mcp`).
//! 2. Connects with `Client::connect_to(&spec).await?`.
//!    That single call covers the four steps a consumer
//!    would otherwise write by hand: detect, open, do the
//!    `initialize` handshake, bind the session.
//! 3. Lists the tools the server advertises.
//! 4. Calls `shout` with `text: "hello"` and prints the
//!    result.
//!
//! # Running
//!
//! ```text
//! # terminal 1
//! cargo run -p protocol-droid --example http_server --features http,random-ids
//!
//! # terminal 2
//! cargo run -p protocol-droid --example http_client --features http
//! ```
//!
//! Override the peer with `MCP_PEER=http://other-host:9000/mcp`.

use protocol_droid::application::client::Client;
use protocol_droid::domain::TransportSpec;
use protocol_droid::domain::ir::JsonObject;
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run())
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let peer = std::env::var("MCP_PEER").unwrap_or_else(|_| "http://127.0.0.1:3000/mcp".to_owned());
    eprintln!("http_client: connecting to {peer}");

    let spec: TransportSpec = peer.parse()?;
    let client = Client::connect_to(&spec).await?;

    let tools = client.list_tools().await?;
    eprintln!("server advertises: {tools}", tools = tools.as_str());

    let args = JsonObject::from_value(json!({ "text": "hello" }))?;
    let reply = client.call_tool("shout", &args).await?;
    let reply_value: serde_json::Value = serde_json::from_str(reply.as_str())?;
    println!("shout(hello) -> {reply_value}");

    Ok(())
}
