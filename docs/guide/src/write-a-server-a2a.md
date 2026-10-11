# Write an A2A server

An A2A server is a struct that implements `A2aRequestHandler`,
plus two or three axum routers — one for the well-known
`AgentCard`, one for the JSON-RPC method table, one for
the streaming SSE endpoint, and a `/health` probe for
container orchestrators. There is no macro; the serving
adapter is small enough that a hand-rolled server is shorter
than the macro expansion would be.

## The reference server

The
[`examples/a2a_server.rs`](../../crates/protocol-droid/examples/a2a_server.rs)
example is a complete A2A server: an in-memory task store, an
agent that implements all eleven A2A v1.0 methods over
JSON-RPC, and the two streaming methods over Server-Sent
Events. The well-known `AgentCard` is served at
`/.well-known/agent-card.json`; a liveness probe is served
at `/health`. Run it:

```text
cargo run -p protocol-droid --example a2a_server --features a2a,http
```

Override the bind address with `A2A_ADDR=0.0.0.0:9000 cargo run ...`.
A bad `A2A_ADDR` value is reported on stderr and the default
(`127.0.0.1:8080`) is used. The example does not install a
SIGINT handler; ctrl-c kills the process. A real agent would
wire `with_graceful_shutdown` to `tokio::signal::ctrl_c` (and
SIGTERM on Unix) so in-flight SSE streams and JSON-RPC
requests drain before the process exits — the example skips
that to keep the `tokio` feature set minimal.

A one-shot probe is `curl`:

```text
curl http://127.0.0.1:8080/.well-known/agent-card.json

# JSON-RPC request/response
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

# Streaming SSE
curl -N -X POST http://127.0.0.1:8080/a2a/stream \
  -H "content-type: application/json" \
  -d '{"jsonrpc":"2.0","id":5,"method":"SendStreamingMessage",
       "params":{"message":{"role":"user",
                            "parts":[{"kind":"text","text":"hello"}]}}}'

curl -N -X POST http://127.0.0.1:8080/a2a/stream \
  -H "content-type: application/json" \
  -d '{"jsonrpc":"2.0","id":6,"method":"SubscribeToTask","params":{"id":"t-1"}}'

# Push notification config CRUD
curl -X POST http://127.0.0.1:8080/a2a \
  -H "content-type: application/json" \
  -d '{"jsonrpc":"2.0","id":7,"method":"CreateTaskPushNotificationConfig",
       "params":{"id":"placeholder","taskId":"t-1",
                 "url":"https://example.com/webhook","token":"tok"}}'

curl -X POST http://127.0.0.1:8080/a2a \
  -H "content-type: application/json" \
  -d '{"jsonrpc":"2.0","id":8,"method":"GetTaskPushNotificationConfig",
       "params":{"id":"pc-1","taskId":"t-1"}}'

curl -X POST http://127.0.0.1:8080/a2a \
  -H "content-type: application/json" \
  -d '{"jsonrpc":"2.0","id":9,"method":"ListTaskPushNotificationConfigs",
       "params":{"taskId":"t-1"}}'

curl -X POST http://127.0.0.1:8080/a2a \
  -H "content-type: application/json" \
  -d '{"jsonrpc":"2.0","id":10,"method":"DeleteTaskPushNotificationConfig",
       "params":{"id":"pc-1","taskId":"t-1"}}'

# Extended agent card (returns the same card, no extra features)
curl -X POST http://127.0.0.1:8080/a2a \
  -H "content-type: application/json" \
  -d '{"jsonrpc":"2.0","id":11,"method":"GetExtendedAgentCard","params":{}}'
```

## The routes, and why each

The well-known route is the A2A §8 discovery mechanism. A peer
fetches the `AgentCard` from
`/.well-known/agent-card.json`, learns the methods the server
supports, and then talks JSON-RPC to the path the card names.
A server that only serves JSON-RPC is reachable, but not
discoverable; a server that only serves the card is not
reachable at all.

The JSON-RPC route serves the request/response methods. The
example covers all eleven: `SendMessage`, `GetTask`,
`CancelTask`, `ListTasks`, `GetExtendedAgentCard`, and the
four `*TaskPushNotificationConfig` methods for webhook
registration. The streaming route serves
`SendStreamingMessage` and `SubscribeToTask` as Server-Sent
Events.

All three routes are served by merging routers:

```text
serve_well_known(card)
    .merge(serve_jsonrpc("/a2a", request_handler, A2aVersion::V1_0))
    .merge(serve_streaming("/a2a/stream", streaming_handler, A2aVersion::V1_0))
```

The merge happens at the axum level, so a single
`axum::serve(listener, router)` call serves all of them. The
same agent struct can implement both `A2aRequestHandler` and
`A2aStreamingRequestHandler` so the two paths share the same
task store.

## The agent card, and what it must claim

The card's `capabilities` flags are a contract. A peer that
fetches the well-known path reads `streaming`,
`pushNotifications`, `extendedAgentCard`, and `extensions`
to decide which methods to call. A card that says
`streaming: false` tells the peer *do not call
`SendStreamingMessage`*; a card that says
`pushNotifications: false` tells it *do not call
`CreateTaskPushNotificationConfig`*. The example's card
sets `streaming: true` and `pushNotifications: true`
because the example actually serves those methods on
`/a2a/stream` and delivers to webhooks it has registered.
A card that contradicts the methods the agent serves is
not "conservative" — it is wrong, and a peer that follows
it will silently miss features the agent offers.

`extendedAgentCard` is the one capability that, when true,
also requires authentication. The spec says a card with
`extendedAgentCard: true` must gate `GetExtendedAgentCard`
behind authentication, and the card may also carry a
`securitySchemes` field that names the schemes the agent
accepts. The example's card sets `extendedAgentCard:
false` because the example has no extra capabilities to
return; a real agent with skills behind auth would set
it to `true` and add the schemes.

## The handler traits

`A2aRequestHandler` has one method: `dispatch(method, params,
context) -> Future<Output = Result<Value, A2aError>>`. The
serving adapter decodes the envelope, looks the method up in
the table, and calls `dispatch` with the matched method and
the raw params. The handler is free to do anything that
returns a `serde_json::Value` that matches the method's
response shape on the wire.

`A2aStreamingRequestHandler` has one method:
`dispatch_stream(method, params, context) -> Stream<Item =
Result<StreamingEvent, A2aError>>`. The serving adapter wraps
each event in a JSON-RPC response and emits it as an SSE
event. The stream ends when the handler's stream returns
`None`, or when the client disconnects.

Inside `dispatch`, the cleanest pattern is one helper per
implemented method. The example does this with
`handle_send_message`, `handle_get_task`, `handle_cancel_task`,
and `handle_list_tasks`, each one taking the typed request by
reference and returning the typed response. The handler trait's
lifetimes prevent the `dispatch` method from forwarding to the
per-method typed wrappers on `A2aRequestHandlerExt` directly —
the trait borrows `&self` and `&context` with independent
lifetimes, but the extension trait's wrappers unify them — so
the typed wrappers are for external callers and the inner
helpers are for `dispatch`.

For a more typed external shape, `A2aRequestHandlerExt`
provides per-method wrappers (`send_message`, `get_task`,
`cancel_task`, `list_tasks`, `get_extended_agent_card`,
plus the four push-notification methods). Each one parses
`params` for you and returns the typed response value. The
blanket impl on every `A2aRequestHandler` is automatic. The
streaming counterpart is `A2aStreamingRequestHandlerExt`, with
`send_streaming_message` and `subscribe_to_task`.

## Streaming and async

`dispatch_stream` returns a `Stream`, not a `Future`, so the
handler cannot `.await` directly. The example's streaming
helpers do all their work before the stream is returned: the
task id is allocated, the task is inserted into the store,
and the two events are pre-built into a `Vec`, then
`Box::pin(stream::iter(events))` produces the stream. A real
agent that does long-running work between events would return
a stream that yields each event as the work progresses (for
example, an `async_stream::stream!` macro or a channel-fed
`ReceiverStream`).

The example uses `std::sync::Mutex` rather than
`tokio::sync::Mutex` for the task store. The lock is held for
microseconds, so the synchronous lock is fine — and it
removes the runtime-in-runtime trap that `block_on` inside a
streaming handler would create.

## Task lifecycle

The example agent stores its tasks in an in-memory
`HashMap` behind a `Mutex`. A real agent would put a database
here, or a queue fronting a worker pool. The store's shape —
keyed by `task id` with optional filters on context and state
— is the same either way.

`SendMessage` returns a `Task` in `completed` state with the
reply attached to the status message. `SendStreamingMessage`
emits a `working`-then-`completed` pair on the SSE stream
and inserts the task into the store before the first event
so a concurrent `GetTask` from another peer can see it.
`SubscribeToTask` re-reads the task from the store and emits
a single final event with its current state. `GetTask` reads
the task back. `CancelTask` mutates the task's status to
`cancelled`. `ListTasks` enumerates the store, optionally
filtered by context id and state.

The four `*TaskPushNotificationConfig` methods let a peer
register a webhook URL for a task, fetch it back, list the
configs attached to a task, and remove it. The example
validates the URL through the SSRF denylist at config time
and spawns a delivery task on the runtime handle for every
config registered against a task. The delivery runs in the
background; the handler returns immediately. The library
ships `SsrfDenylist` (an `OWASP`-cited IP-range denylist
covering loopback, link-local, multicast, and the metadata
endpoints) and `WebhookDelivery` (a `reqwest`-backed POST
with re-validation at delivery time so a DNS-rebind attack
is caught). `GetExtendedAgentCard` returns the same card
the well-known path serves, since the example has no
extended capabilities — a real agent would gate this on
authentication and return additional skills.
