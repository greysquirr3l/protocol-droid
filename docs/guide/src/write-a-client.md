# Write a client

A `protocol-droid` client connects to an upstream MCP peer,
negotiates a protocol version, exchanges capabilities, and drives
the `tools/call` / `resources/read` / `prompts/get` flow. The
client-side adapters are `StdioClient` and `HttpClient`; both
implement the `BridgePeer` port trait and can be driven by
`Client::new(...)`.

## A 9-line minimal client

```text
use protocol_droid::adapters::secondary::stdio::{ChildSpec, ClientConfig, StdioClient};
use protocol_droid::application::client::Client;
use protocol_droid::adapters::secondary::diagnostics::Diagnostics;
use protocol_droid::domain::ProtocolVersion;

let spec = ChildSpec::new("upstream-mcp-server")?;
let config = ClientConfig::default();
let client = StdioClient::spawn(&spec, config, Diagnostics::stderr())?;

let mut session = Client::new(client)
    .initialize(ProtocolVersion::latest())
    .await?;

let reply = session
    .call_tool("add", serde_json::json!({"a": 2, "b": 3}))
    .await?;
```

The client is one `Client::new(...)` call away. The `Client`
type is the consumer-side entry point; the rest of the library's
internals are hidden behind it.

## What the client handles

- **Version negotiation.** `Client::initialize` sends the
  peer's preferred protocol version; the peer counters with
  what it supports; the codec the client uses is the one
  the negotiation chose.
- **Capability exchange.** The client's `CapabilitySet` is
  sent in `initialize`; the server's `CapabilitySet` is
  returned in the reply. The `FeatureProfile` is built
  from the four inputs (version × client × server ×
  transport) and gates every feature-specific path.
- **Idempotency.** A `tools/call` retry after a dropped
  Streamable HTTP connection replays its recorded response
  rather than running the tool twice. The `IdempotencyStore`
  is the boundary the handler can plug into; the order
  (check → execute → persist) is the contract.

## What the client does not do

- It does not implement your tool's logic. A `tools/call` is
  forwarded to the upstream server; the client is a *client*.
- It does not silently downgrade a feature. A tool result
  the peer cannot shape returns a typed `LowerError`, not a
  serde error.
- It does not reconnect silently. A transport that drops a
  frame surfaces a `TransportError`; the caller decides
  whether to retry.
