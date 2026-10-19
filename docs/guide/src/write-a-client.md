# Write a client

A `protocol-droid` client connects to an upstream MCP peer,
negotiates a protocol version, exchanges capabilities, and drives
the `tools/call` / `resources/read` / `prompts/get` flow. The
client-side adapters are `StdioClient` and `HttpClient`; both
implement the `BridgePeer` port trait and can be driven by
`Client::new(...)`.

## A 9-line minimal client

```text
use protocol_droid::adapters::secondary::stdio::ChildSpec;
use protocol_droid::application::client::Client;

let spec = ChildSpec::new("upstream-mcp-server")?;
let client = Client::connect_stdio(&spec).await?;
let tools = client.list_tools().await?;
```

`Client::connect_stdio` is the consumer-side entry point; the
rest of the library's internals (the spawn, the handshake, the
session bind) are hidden behind one call. The four steps a
consumer would otherwise write by hand — `StdioClient::spawn`,
`handshake`, `Client::from_session` — are folded into
`connect_stdio`. A consumer who needs to control any of those
(specific protocol versions, a custom diagnostics channel,
shorter timeouts) uses the four steps directly; both paths are
public and `connect_stdio` is the easy one.

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

## Signing in to an OAuth-protected server

When a server answers `401` or `403`, an `OAuthAuthorizer` obtains
a token and the request is retried. It follows the authorization
page of the spec: it reads the protected resource metadata (from the
challenge, else the well-known location), checks that it describes the
server being called, finds the authorization server, and refuses one
that does not advertise PKCE with `S256`. It then picks a client in
priority order (one it already holds, pre-registered credentials, a
client metadata document, dynamic registration), asks for the scope the
challenge names, and sends the `resource` exactly as the metadata
published it. Responses are checked against RFC 9207's `iss`
rules. After three refusals it stops.

```rust
use std::sync::Arc;
use protocol_droid::adapters::secondary::oauth::LoopbackBrowser;
use protocol_droid::adapters::secondary::oauth::environment::HttpOAuthEnvironment;
use protocol_droid::adapters::secondary::oauth::secrets::OsSecrets;
use protocol_droid::application::client::handler::ConnectOptions;
use protocol_droid::application::oauth::authorizer::{OAuthAuthorizer, OAuthConfig};
use protocol_droid::domain::ProtocolVersion;

let browser = LoopbackBrowser::bind().await?;
let config = OAuthConfig::new("https://mcp.example.com/mcp", ProtocolVersion::latest_known())?
    .redirect_to(browser.redirect_uri())
    .wanting_refresh_tokens();
let authorizer = OAuthAuthorizer::new(config, HttpOAuthEnvironment::new(Arc::new(browser))?, OsSecrets);

let client = Client::connect_with(&spec, &versions, ConnectOptions::new().authorizing(authorizer)).await?;
```

`LoopbackBrowser` opens the system browser and receives the redirect on
`127.0.0.1`; it gives up after five minutes by default.
`FollowsRedirect` is the headless alternative, for an authorization
server that approves without asking (tests and CI). A server
you register with ahead of time is described with
`OAuthConfig::with_pre_registered`.

When a token expires the authorizer trades its refresh token for a new
one, without asking the user again. A refused refresh token is
forgotten and the whole flow runs; a challenge for more scope always
goes back to the user, and the new request asks for the union of what
was granted and what is now needed.

From the command line, `protocol-droid inspect <server> --oauth list` does the same.

## Stateless servers

A `2026-07-28` server is reached with an `http-stateless:` spec.
`Client::connect_to` discovers the revision (renegotiating on `-32022`),
sends the revision and capabilities in every request's `_meta`, and
mirrors tool arguments annotated `x-mcp-header` into
`Mcp-Param-*` headers. A tool whose annotations are invalid is dropped
from the listing, as the specification asks.

## What the client does not do

- It does not implement your tool's logic. A `tools/call` is
  forwarded to the upstream server; the client is a *client*.
- It does not silently downgrade a feature. A tool result
  the peer cannot shape returns a typed `LowerError`, not a
  serde error.
- It does not reconnect silently. A transport that drops a
  frame surfaces a `TransportError`; the caller decides
  whether to retry.
