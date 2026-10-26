---
title: Write a server
description: A minimal MCP server in nine lines, what the macro expands to, and the manual path for everything the macro does not cover.
---

A `protocol-droid` server is a `Server` — a registry of tools, resources
and prompts, plus the machinery that answers a peer's requests against
it. The library ships two transports — stdio and Streamable HTTP — and a
`#[server]` macro that writes the boilerplate for the common case.

## The minimal server

The `examples/stdio_server.rs` example is the smallest server that is
also a complete one. It registers a tool, a resource and a prompt, and
serves over stdio:

```rust
struct Greeter;

#[protocol_droid::server(name = "greeter")]
impl Greeter {
    /// Greets someone by name.
    #[protocol_droid::tool]
    async fn greet(name: String) -> Result<String, String> {
        Ok(format!("hello, {name}"))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    Greeter.into_server()?.serve_stdio().await?;
    Ok(())
}
```

That is the consumer-facing API. `#[protocol_droid::server]` generates
one method that matters — `into_server` — and the marks inside it
(`#[tool]`, `#[resource]`, `#[prompt]`) register as the macro expands:
each parameter becomes a property in the tool's `inputSchema`, the doc
comment becomes the `description` a peer reads, and the function body is
wired to the JSON-RPC router without further plumbing. A `#[tool]`
handler is `async` even when it awaits nothing, because the call the
macro generates has to produce a future.

:::note
`#[server]` goes on an **impl block**, not on the struct. An attribute
macro only ever sees the item it is attached to, so a `#[server]` on a
struct would not see the `impl` beside it, and every tool in that impl
would be invisible to it. Putting it on the `impl` puts the macro where
the methods are, which is the only place the tools can be read from.
:::

## A fuller picture

`#[server]` covers the common case. The non-macro path builds the same
`Server` by hand, and the difference is mostly that the schema is
written out rather than derived:

```rust
use std::sync::Arc;

use protocol_droid::application::server::Server;
use protocol_droid::domain::ir::JsonObject;

let server = Server::builder("greeter")
    .simple_tool(
        "greet",
        r#"{"type":"object","properties":{"name":{"type":"string"}}}"#,
        Arc::new(|_ctx, args| {
            Box::pin(async move {
                // A handler answers with the tool's result object, not with a
                // `Result`: MCP reports a tool failure as a successful call
                // carrying `isError: true`, so "the tool failed" is something
                // the handler *returns* rather than propagates.
                let name = args
                    .get("name")
                    .and_then(|value| value.as_str())
                    .unwrap_or("world");
                JsonObject::from_value(serde_json::json!({
                    "content": [{ "type": "text", "text": format!("hello, {name}") }],
                }))
                .unwrap_or_default()
            })
        }),
    )
    .expect("the schema above is valid")
    .build();

server.serve_stdio().await?;
```

The two paths converge on the same `Server`. The macro is sugar; the
`Server` is the actual surface.

`Server::builder(name)` takes the name a peer sees in `serverInfo`. The
version beside it comes from the crate that wrote the `#[server]`
attribute, so a server that merely *links* `protocol-droid` reports its
own version rather than the dependency's. Override it with the
`version` field:

```rust
#[protocol_droid::server(name = "greeter", version = env!("CARGO_PKG_VERSION"))]
impl Greeter { /* ... */ }
```

## Transports: stdio and Streamable HTTP

The stdio path is `serve_stdio`. The Streamable HTTP path is
`serve_http`, or `HttpBuilder` when the listener address and the
authentication and origin policy matter:

```rust
use protocol_droid::adapters::primary::transport::http::HttpBuilder;

let report = HttpBuilder::new(server).addr(addr).serve().await?;
```

Both take the server and return a report. The revision comes from the
peer's own `initialize` — the server does not pick one at startup — and
every request after the handshake is gated on the revision that peer
negotiated.

### Legacy HTTP+SSE

The `2024-11-05` transport mounts beside Streamable HTTP on the same
listener:

```rust
let report = HttpBuilder::new(server).addr(addr).legacy_sse().serve().await?;
```

A client opens a stream with `GET /sse`, is told where to `POST` by the
`endpoint` event, and speaks `2024-11-05` over the pair (use
`legacy_sse_at` to move the paths). The same tools, resources and
prompts answer on both transports. `list_changed`, progress and log
records go out on the session's stream, and a tool can ask the client a
question and wait for the answer. A client that offers a later revision
is answered with `2024-11-05`, the only revision that defines the
transport. The authenticator, `Origin` policy, `Host` check and rate
limit set on the builder apply to both transports.
`conformance_reference_server --legacy-sse` runs this.

## Registering and removing primitives while serving

`Server::handle()` returns a `ServerHandle`, which is the live view of
the registry: `is_listed`, `subscribe`, `watch_resource` and
`unwatch_resource` read what is there and who is watching. A tool
registered or removed while the server is running is announced with
`list_changed` to the sessions that negotiated it, and only those.

## How long a retry is remembered

A `tools/call` retried after a dropped connection replays its recorded
answer rather than running the tool again. The server keeps the last
4 096 completed results; once full it drops the oldest finished one,
never a call still running. A retry of something older than that window
runs again.

## What the server does not do

- It does not start a tokio runtime. `serve_stdio` and `serve_http` are
  `async`, so they run on whatever runtime you already have; a
  `#[tokio::main]` is the usual way to have one.
- It does not log tokens, Authorization headers, or session ids
  in full. The tracing layer's redaction list is the
  authoritative form.
- It does not silently drop a notification the peer cannot
  receive. A `list_changed` notification the peer did not
  advertise is gated by the `FeatureProfile` and the call site
  decides what to do (`DegradePolicy::Skip` if the caller
  opted in, else a typed error).
