# Write a server

A `protocol-droid` server is a struct that implements the
`Dispatcher` trait, plus a transport that knows how to deliver a
JSON-RPC frame to it. The library ships two transports — stdio and
Streamable HTTP — and a `#[server]` macro that writes the
boilerplate for the common case.

## The 9-line minimal server

The `examples/stdio_server.rs` example is the smallest server
that is also a complete one. It registers one tool, one
resource, and one prompt, and serves over stdio:

```text
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

```text
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
