//! An MCP client in one runnable file.
//!
//! Spawns the
//! [`stdio_server`](protocol_droid::adapters::primary::stdio::serve_stdio)
//! example as a child process, connects to it with
//! [`Client::connect_stdio`](protocol_droid::application::client::Client::connect_stdio),
//! lists the tools it advertises, and calls one.
//!
//! # Why this exists
//!
//! The write-a-client.md guide shows a 9-line example
//! using the new [`Client::connect_stdio`] façade. This
//! file is the runnable counterpart — a consumer who
//! copies it and changes the `ChildSpec::new(...)`
//! program has a working MCP client pointed at any
//! stdio server they want to drive.
//!
//! # What it does
//!
//! 1. Spawns `cargo run -p protocol-droid --example
//!    stdio_server` as a child. The child's stdout is
//!    the protocol stream; its stderr is drained into
//!    `Diagnostics::stderr()` by the library's
//!    `StdioClient`.
//! 2. Connects with `Client::connect_stdio(&spec)`.
//!    That single call covers the four steps a
//!    consumer would otherwise write by hand:
//!    `StdioClient::spawn`, `handshake`, and
//!    `Client::from_session`.
//! 3. Lists the tools the server advertises. The
//!    `stdio_server` example has one tool, `shout`,
//!    so the result is a single entry.
//! 4. Calls `shout` with `text: "hello"`. The server
//!    uppercases it and reports the character count.
//!
//! # Running
//!
//! ```text
//! cargo run -p protocol-droid --example mcp_client
//! ```
//!
//! The child cargo process is killed when the client
//! returns; the example does not install a SIGINT
//! handler of its own, so ctrl-c also kills the child
//! through the standard parent/child relationship.

use protocol_droid::adapters::secondary::stdio::ChildSpec;
use protocol_droid::application::client::Client;
use protocol_droid::domain::ir::JsonObject;
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(run())
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let spec = server_spec()?;
    let client = Client::connect_stdio(&spec).await?;

    let tools = client.list_tools().await?;
    eprintln!("server advertises: {tools}", tools = tools.as_str());

    let args = JsonObject::from_value(json!({ "text": "hello" }))?;
    let reply = client.call_tool("shout", &args).await?;
    let reply_value: serde_json::Value = serde_json::from_str(reply.as_str())?;
    println!("shout(hello) -> {reply_value}");

    Ok(())
}

/// The `ChildSpec` for the `stdio_server` example.
///
/// `cargo run --example stdio_server` is what the
/// example's own docs tell the reader to run, so the
/// child is the same one. The `--example` flag and
/// the example name are the only arguments; cargo
/// handles the rest. The CARGO env var is set when
/// running through cargo, so the child picks up the
/// same toolchain.
fn server_spec() -> Result<ChildSpec, Box<dyn std::error::Error>> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    Ok(ChildSpec::new(cargo)?
        .with_arg("run")?
        .with_arg("--quiet")?
        .with_arg("-p")?
        .with_arg("protocol-droid")?
        .with_arg("--example")?
        .with_arg("stdio_server")?)
}
