# Write an A2A server

An A2A server is a struct that implements `A2aRequestHandler`,
plus two axum routers — one for the well-known `AgentCard`, one
for the JSON-RPC method table. There is no macro; the serving
adapter is small enough that a hand-rolled server is shorter
than the macro expansion would be.

## The 60-line minimal server

The
[`examples/a2a_server.rs`](../../crates/protocol-droid/examples/a2a_server.rs)
example is the smallest server that is also a complete one.
It implements `SendMessage` and `GetTask`, serves the
`AgentCard` on the well-known path, and runs on
`127.0.0.1:8080`:

```text
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use protocol_droid::adapters::primary::a2a::jsonrpc_serve::serve_jsonrpc;
use protocol_droid::adapters::primary::a2a::well_known_serve::serve_well_known;
use protocol_droid::adapters::secondary::a2a::A2aMethod;
use protocol_droid::application::a2a::{A2aError, A2aRequestContext, A2aRequestHandler};
use protocol_droid::domain::a2a::A2aVersion;
use protocol_droid::domain::a2a::agent_card::{
    AgentCapabilities, AgentCard, AgentCardBuilder, AgentInterface, AgentSkill, JSONRPC,
};
use protocol_droid::domain::a2a::message::{Message, MessageRole, Part, TextPart};
use protocol_droid::domain::a2a::send_message::SendMessageResponse;
use protocol_droid::domain::a2a::task::{Task, TaskState, TaskStatus};

struct EchoAgent;

impl A2aRequestHandler for EchoAgent {
    fn dispatch(
        &self,
        method: A2aMethod,
        _params: Option<serde_json::Value>,
        _context: &A2aRequestContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<serde_json::Value, A2aError>> + Send + '_>> {
        Box::pin(async move { /* … */ Ok(serde_json::Value::Null) })
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let card = Arc::new(
        AgentCardBuilder::new()
            .with_name("Echo Agent")
            .with_description("Echoes messages back.")
            .with_version("0.1.0")
            .with_interface(AgentInterface {
                url: url::Url::parse("http://127.0.0.1:8080/a2a")?,
                protocol_binding: JSONRPC.clone(),
                protocol_version: A2aVersion::V1_0,
                tenant: None,
            })
            .with_capabilities(AgentCapabilities::default())
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .with_skill(AgentSkill {
                id: "echo".into(),
                name: "Echo".into(),
                description: "Echoes the user's message.".into(),
                tags: Vec::new(),
            })
            .build()?,
    );
    let handler: Arc<dyn A2aRequestHandler> = Arc::new(EchoAgent);
    let router = serve_well_known(card).merge(serve_jsonrpc("/a2a", handler, A2aVersion::V1_0));

    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:8080".parse::<SocketAddr>()?).await?;
        axum::serve(listener, router).await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}
```

Run it:

```text
cargo run -p protocol-droid --example a2a_server --features a2a,http
```

A one-shot probe is `curl`:

```text
curl http://127.0.0.1:8080/.well-known/agent-card.json

curl -X POST http://127.0.0.1:8080/a2a \
  -H "content-type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"SendMessage",
       "params":{"message":{"role":"user","messageId":"m-1",
                            "parts":[{"kind":"text","text":"hello"}]}}}'
```

## The two routes, and why both

The well-known route is the A2A §8 discovery mechanism. A peer
fetches the `AgentCard` from
`/.well-known/agent-card.json`, learns the methods the server
supports, and then talks JSON-RPC to the path the card names.
A server that only serves JSON-RPC is reachable, but not
discoverable; a server that only serves the card is not
reachable at all.

Both routes are served by merging two routers:

```text
serve_well_known(card).merge(serve_jsonrpc("/a2a", handler, A2aVersion::V1_0))
```

The merge happens at the axum level, so a single
`axum::serve(listener, router)` call serves both.

## The handler trait

`A2aRequestHandler` has one method: `dispatch(method, params,
context) -> Future<Output = Result<Value, A2aError>>`. The
serving adapter decodes the envelope, looks the method up in
the table, and calls `dispatch` with the matched method and
the raw params. The handler is free to do anything that
returns a `serde_json::Value` that matches the method's
response shape on the wire.

For a more typed shape, `A2aRequestHandlerExt` provides
per-method wrappers (`send_message`, `get_task`,
`cancel_task`, `list_tasks`, `get_extended_agent_card`,
plus the four push-notification methods). Each one parses
`params` for you and returns the typed response value.

## Streaming

For `SendStreamingMessage` and `SubscribeToTask`, the handler
is `A2aStreamingRequestHandler`, which returns a `Stream<Item
= Result<StreamingEvent, A2aError>>` instead of a single
`Future`. The serving adapter is `serve_streaming`, which
emits Server-Sent Events on the response stream. See
`adapters::primary::a2a::streaming_typed_dispatch` for the
typed wrappers.
