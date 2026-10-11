# Write an A2A server

An A2A server is a struct that implements `A2aRequestHandler`,
plus two axum routers — one for the well-known `AgentCard`, one
for the JSON-RPC method table. There is no macro; the serving
adapter is small enough that a hand-rolled server is shorter
than the macro expansion would be.

## The reference server

The
[`examples/a2a_server.rs`](../../crates/protocol-droid/examples/a2a_server.rs)
example is a complete A2A server: an in-memory task store, an
agent that implements `SendMessage`, `GetTask`, `CancelTask`,
and `ListTasks`, and the well-known `AgentCard` served at
`/.well-known/agent-card.json`. Run it:

```text
cargo run -p protocol-droid --example a2a_server --features a2a,http
```

A one-shot probe is `curl`:

```text
curl http://127.0.0.1:8080/.well-known/agent-card.json

curl -X POST http://127.0.0.1:8080/a2a \
  -H "content-type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"SendMessage",
       "params":{"message":{"role":"user",
                            "parts":[{"kind":"text","text":"hello"}]}}}'

curl -X POST http://127.0.0.1:8080/a2a \
  -H "content-type: application/json" \
  -d '{"jsonrpc":"2.0","id":2,"method":"GetTask","params":{"id":"t-1"}}'

curl -X POST http://127.0.0.1:8080/a2a \
  -H "content-type: application/json" \
  -d '{"jsonrpc":"2.0","id":3,"method":"ListTasks","params":{}}'

curl -X POST http://127.0.0.1:8080/a2a \
  -H "content-type: application/json" \
  -d '{"jsonrpc":"2.0","id":4,"method":"CancelTask","params":{"id":"t-1"}}'
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

Inside `dispatch`, the cleanest pattern is one async helper
per implemented method. The example does this with
`handle_send_message`, `handle_get_task`, `handle_cancel_task`,
and `handle_list_tasks`, each one taking the typed request and
returning the typed response. The handler trait's lifetimes
prevent the `dispatch` method from forwarding the per-method
typed wrappers on `A2aRequestHandlerExt` directly — the trait
borrows `&self` and `&context` with independent lifetimes, but
the extension trait's wrappers unify them — so the typed
wrappers are for external callers and the inner async helpers
are for `dispatch`.

For a more typed external shape, `A2aRequestHandlerExt`
provides per-method wrappers (`send_message`, `get_task`,
`cancel_task`, `list_tasks`, `get_extended_agent_card`,
plus the four push-notification methods). Each one parses
`params` for you and returns the typed response value. The
blanket impl on every `A2aRequestHandler` is automatic.

## Streaming

For `SendStreamingMessage` and `SubscribeToTask`, the handler
is `A2aStreamingRequestHandler`, which returns a `Stream<Item
= Result<StreamingEvent, A2aError>>` instead of a single
`Future`. The serving adapter is `serve_streaming`, which
emits Server-Sent Events on the response stream. See
`adapters::primary::a2a::streaming_typed_dispatch` for the
typed wrappers.

## Task lifecycle

The example agent stores its tasks in an in-memory
`HashMap` behind a `Mutex`. A real agent would put a database
here, or a queue fronting a worker pool. The store's shape —
keyed by `task id` with optional filters on context and state
— is the same either way.

`SendMessage` returns a `Task` in `completed` state with the
reply attached to the status message. `GetTask` reads the
task back. `CancelTask` mutates the task's status to
`cancelled`. `ListTasks` enumerates the store, optionally
filtered by context id and state.
