---
title: Write a server
description: A nine-line minimal server, what the macro expands to, and the manual path for everything the macro does not cover.
---

A `protocol-droid` server is a struct that implements the
`Dispatcher` trait, plus a transport that knows how to deliver a
JSON-RPC frame to it. The library ships two transports — stdio and
Streamable HTTP — and a `#[server]` macro that writes the
boilerplate for the common case.

## The nine-line minimal server

The `examples/stdio_server.rs` example is the smallest server
that is also a complete one. It registers one tool, one
resource, and one prompt, and serves over stdio:

```rust
use protocol_droid::prelude::*;

#[server]
struct Greeter {
    #[tool("Return a greeting for the given name")]
    fn greet(&self, name: String) -> String {
        format!("hello, {name}")
    }

    #[resource("greeting://{name}")]
    fn greeting(&self, name: String) -> String {
        format!("hello, {name}")
    }
}

fn main() -> std::io::Result<()> {
    Greeter::new().serve_stdio()
}
```

That is the consumer-facing API. The macro expands to a struct
that implements `Dispatcher`, a `serve_stdio` method that wires
the stdio transport, and the rustdoc the macro generates is the
contract the rest of the server implements against.

## A fuller picture

`#[server]` covers the common case. The non-macro path is one
struct per use case plus an `impl Dispatcher` block:

```rust
use protocol_droid::application::jsonrpc::Dispatcher;
use protocol_droid::application::server::Server;
use protocol_droid::adapters::secondary::stdio::StdioConfig;
use protocol_droid::adapters::secondary::id_source::RandomIds;

struct Greeter;

impl Dispatcher for Greeter {
    // ...
}

fn main() -> anyhow::Result<()> {
    Server::new(Greeter)
        .with_id_source(RandomIds::new()?)
        .serve(StdioConfig::default())
}
```

The two paths converge on the same `Server::serve` entry point.
The macro is sugar; the `Server` is the actual surface.

## Transports: stdio and Streamable HTTP

The `stdio` transport drives a child process's standard
streams. The library also ships a Streamable HTTP transport
— same `Server` and same `#[tool]` macros, different
serving adapter. The
[`examples/http_server.rs`](https://github.com/greysquirr3l/protocol-droid/blob/main/crates/protocol-droid/examples/http_server.rs)
example is the counterpart to `stdio_server.rs`:

```rust
use protocol_droid::adapters::primary::transport::http::HttpBuilder;

let server = Greeter.into_server()?;
let report = HttpBuilder::new(server).addr(addr).serve().await?;
```

`HttpBuilder` mirrors `StdioBuilder`: same protocol, different
delivery mechanism. Both share the four pieces a serving
adapter needs (a `Server`, a `Dispatcher`, a `PeerSession`,
a frame loop); neither asks the caller to name a protocol
version. A consumer who needs the full feature set the
conformance suite reads copies
[`conformance_reference_server.rs`](https://github.com/greysquirr3l/protocol-droid/blob/main/crates/protocol-droid/examples/conformance_reference_server.rs)
instead — the same `HttpBuilder`, every feature wired.

## Serving the legacy `2024-11-05` transport too

`HttpBuilder::legacy_sse()` mounts the old HTTP+SSE routes beside
Streamable HTTP on the same listener:

```rust
let report = HttpBuilder::new(server).addr(addr).legacy_sse().serve().await?;
```

A client opens a stream with `GET /sse`, is told where to `POST` by the
`endpoint` event, and speaks `2024-11-05` over the pair (use
`legacy_sse_at(stream, message)` to move the paths). The same tools,
resources and prompts answer on both transports. `list_changed`,
progress and log records go out on the session's stream, and a tool
can ask the client a question and wait for the answer. A client that
offers a later revision is answered with `2024-11-05`, the only
revision that defines the transport. The authenticator, `Origin`
policy, `Host` check and rate limit set on the builder apply to both
transports. `conformance_reference_server --legacy-sse` runs this.

## Registering and removing primitives while serving

`Server::handle()` returns a `ServerHandle`. `add_tool`, `remove_tool`,
and the matching calls for resources, templates and prompts change what
the server lists while it is running, and each change is announced with
`list_changed` to the sessions that negotiated it, and only those.

## How long a retry is remembered

A `tools/call` retried after a dropped connection replays its recorded
answer rather than running the tool again. The server keeps the last
4 096 completed results; once full it drops the oldest finished one,
never a call still running. A retry of something older than that window
runs again.

## What the server does not do

- It does not start a tokio runtime; `serve_stdio` builds one
  for you, and `serve` takes a transport config that has its
  own.
- It does not log tokens, Authorization headers, or session ids
  in full. The tracing layer's redaction list is the
  authoritative form.
- It does not silently drop a notification the peer cannot
  receive. A `list_changed` notification the peer did not
  advertise is gated by the `FeatureProfile` and the call site
  decides what to do (`DegradePolicy::Skip` if the caller
  opted in, else a typed error).
