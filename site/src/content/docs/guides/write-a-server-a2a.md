---
title: Write an A2A server
description: A 180-line minimal A2A server, the reference server with task state, and the one-liner façade for the common case.
---

An A2A server is a struct that implements `A2aRequestHandler`,
plus two or three axum routers — one for the well-known
`AgentCard`, one for the JSON-RPC method table, one for
the streaming SSE endpoint, and a `/health` probe for
container orchestrators. There is no macro; the serving
adapter is small enough that a hand-rolled server is shorter
than the macro expansion would be.

## The smallest server

The
[`examples/a2a_minimal_server.rs`](https://github.com/greysquirr3l/protocol-droid/blob/main/crates/protocol-droid/examples/a2a_minimal_server.rs)
example is a 180-line A2A server that uses the
[`serve_a2a_agent`](https://github.com/greysquirr3l/protocol-droid/blob/main/crates/protocol-droid/src/adapters/primary/a2a/serve_agent.rs)
façade. The handler is a stub — every method
returns `-32601 Method not implemented` — and
there is no task store, no delivery port, and
no health probe. The point is the front door:
one function call mounts the well-known
path, the JSON-RPC path, and the streaming
SSE path on one router. Run it:

```bash
cargo run -p protocol-droid --example a2a_minimal_server --features a2a,http
```

A real consumer starts here, fills in the
methods they need, and adds task state and
delivery when the use case demands it. The
minimal example's `Echo` is the
`A2aRequestHandler + A2aStreamingRequestHandler`
impl; the rest of the work is per-method
business logic and the infrastructure
choices the consumer is making anyway.

## The reference server

The
[`examples/a2a_server.rs`](https://github.com/greysquirr3l/protocol-droid/blob/main/crates/protocol-droid/examples/a2a_server.rs)
example is a complete A2A server: an in-memory task store, an
agent that implements all eleven A2A v1.0 methods over
JSON-RPC, and the two streaming methods over Server-Sent
Events. The well-known `AgentCard` is served at
`/.well-known/agent-card.json`; a liveness probe is served
at `/health`. Run it:

```bash
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

## The convenience façade

The example assembles the three serving
adapters (`serve_well_known`, `serve_jsonrpc`,
`serve_streaming`) by hand, because the
example is the layer-by-layer reference. A
consumer who wants a one-liner can use
[`serve_a2a_agent`](https://github.com/greysquirr3l/protocol-droid/blob/main/crates/protocol-droid/src/adapters/primary/a2a/serve_agent.rs),
which mounts the same three routes on one
router:

```rust
use std::sync::Arc;
use protocol_droid::adapters::primary::a2a::serve_agent::serve_a2a_agent;

let app = serve_a2a_agent(
    "/a2a",
    Arc::new(my_agent),     // implements A2aRequestHandler
    Arc::new(my_agent),     // implements A2aStreamingRequestHandler
    Arc::new(my_card),
);
```

The streaming path is derived as
`{jsonrpc_path}/stream`. The well-known path
is fixed at `/.well-known/agent-card.json`
per A2A v1.0 §8. The example uses the
layer-by-layer form so the route shape is
visible at a glance; the façade is what a
consumer building a real product would
reach for.

## The matching client

Two examples are the mirror of `a2a_server.rs`. The
library-side `A2aClient` port is the recommended way to
consume an A2A peer; the raw-reqwest example remains as
a low-level reference.

### The typed client

[`examples/a2a_typed_client.rs`](https://github.com/greysquirr3l/protocol-droid/blob/main/crates/protocol-droid/examples/a2a_typed_client.rs)
is the front door: `A2aClient::new(peer)?`, typed
request/response types, no envelope munging. The
example is now 202 lines and drives all eleven
A2A v1.0 methods — `card`, `send_message`,
`get_task`, `list_tasks`, `cancel_task`,
`send_streaming_message`, the four
push-notification-config CRUD calls, and
`get_extended_agent_card` (the last is only
called when `A2A_BEARER_TOKEN` is set, since
the extended read is auth-gated; without a
token, the example prints a one-line "skipped"
message). `subscribe_to_task` is intentionally
not exercised because the typed client treats
the streaming path as collect-then-iterate,
and the example already exercises the
streaming collection shape. Run it:

```bash
# terminal 1
cargo run -p protocol-droid --example a2a_server --features a2a,http

# terminal 2
cargo run -p protocol-droid --example a2a_typed_client --features a2a,http
```

Override the peer with `A2A_PEER=http://other-host:9000`.
The example calls `card()` first to discover the
peer, then exercises every request/response
method the example agent supports, plus the
streaming `send_streaming_message` round-trip
that drives the SSE path end-to-end.

For a peer that requires authentication, set
`A2A_BEARER_TOKEN=<token>` on both sides. The
`a2a_typed_client` reads the env var and attaches
`Authorization: Bearer <token>` to every request via
`A2aClient::with_bearer_token(token)`; the
`a2a_server` reads the same env var, declares a
`bearer` `SecurityScheme::Http` on the card, and
runs the JSON-RPC and streaming routes through an
axum `from_fn_with_state` middleware that 401s on
a missing or wrong token. The well-known card is
unauthenticated; the extended card requires the
token.

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

```rust
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
also requires a route through authentication before the
extended card is served. The example gates that read on
`A2A_BEARER_TOKEN` and returns the same card from both the
well-known path and the extended read, since the example
ships no extended capabilities.

## The task store

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
