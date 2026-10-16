---
title: Quickstart
description: A minimal MCP server in nine lines, the smallest A2A server, and the one-call client that drives it.
---

This page is the shortest path from `cargo add` to a working server you
can drive from another MCP client. The full tutorials live in
[Write a server](/protocol-droid/guides/write-a-server/) and
[Write a client](/protocol-droid/guides/write-a-client/); the snippets
here are the front door.

## Hello, server

Add the crate, then drop this into `src/main.rs`:

```rust
use protocol_droid::prelude::*;

/// A greeter: one tool, no state.
struct Greeter;

#[protocol_droid::server(name = "greeter")]
impl Greeter {
    /// Greets someone by name.
    #[protocol_droid::tool]
    async fn greet(name: String) -> String {
        format!("hello, {name}")
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    Greeter.into_server()?.serve_stdio().await
}
```

`#[protocol_droid::server]` generates one method that matters — `into_server`. The marks inside it
(`#[tool]`, `#[resource]`, `#[prompt]`) register as the macro expands:
each parameter becomes a property in the tool's `inputSchema`, the
doc comment becomes the `description` a peer reads, and the function
body is wired to the JSON-RPC router without further plumbing. A
`#[tool]` handler is `async` even when it awaits nothing, because the
call the macro generates has to produce a future.

:::tip
The full `examples/stdio_server.rs` also enables the
`rt-multi-thread` feature on the `tokio` re-export the
crate carries, and uses `serve_stdio` to build a runtime
that matches the stdio transport's needs. The nine-line
form above works with a stock `#[tokio::main]`.
:::

## Hello, client

Connecting to the server you just built, with the one-call entry point:

```rust
use protocol_droid::adapters::secondary::stdio::ChildSpec;
use protocol_droid::application::client::Client;

let spec = ChildSpec::new("./target/debug/greeter")?;
let client = Client::connect_stdio(&spec).await?;
let tools = client.list_tools().await?;
```

`Client::connect_stdio` is the consumer-side entry point; the spawn,
the handshake, the session bind, and the capability exchange are all
hidden behind one call. The result `tools` is the typed list the
server registered.

## Hello, A2A server

For the A* protocols, the same shape but with the A2A route table
mounted on a single axum router. The minimal example is 180 lines and
the full reference is 200+; the entry point looks like this:

```rust
use std::sync::Arc;
use protocol_droid::adapters::primary::a2a::serve_agent::serve_a2a_agent;

let app = serve_a2a_agent(
    "/a2a",
    Arc::new(my_agent),     // implements A2aRequestHandler
    Arc::new(my_agent),     // implements A2aStreamingRequestHandler
    Arc::new(my_card),
);

let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await?;
axum::serve(listener, app).await?;
```

The streaming path is derived as `{jsonrpc_path}/stream` and the
well-known path is fixed at `/.well-known/agent-card.json` per A2A
v1.0 §8. The full walk-through is in
[Write an A2A server](/protocol-droid/guides/write-a-server-a2a/).

## Where to next

- The [architecture overview](/protocol-droid/architecture/overview/) for the
  one-IR, per-revision codec, runtime feature gating model.
- The [CLI reference](/protocol-droid/reference/cli/) for `bridge`, `probe`,
  `inspect`, `record`, `replay`, and `conformance`.
- The [security model](/protocol-droid/guides/security/) for the trust boundary
  between the library and the consumer.
