# Bridge two peers

The bridge is a version-translating forwarder: a `2024-11-05`
client's request arrives, gets lifted into the IR, and gets
lowered to a `2026-07-28` server's wire. The reverse direction
works the same way. The bridge is the only piece of the library
that knows the wire shape of two revisions at once, and the
only one that needs to.

## When to use the bridge

- A legacy client cannot connect to a modern server directly.
  The bridge accepts the legacy transport, decodes the legacy
  wire, and re-encodes to the modern wire.
- A modern client wants to talk to a legacy server for
  compatibility testing. The reverse direction.
- A diagnostic session needs to inspect the wire form of a
  peer. The bridge's `--log` mode records every frame in
  both directions.

## A minimal bridge

```text
use protocol_droid::application::bridge::Bridge;
use protocol_droid::adapters::secondary::stdio::ChildSpec;
use protocol_droid::domain::HttpTarget;
use protocol_droid::adapters::primary::stdio::StdioListener;

let bridge = Bridge::new(
    ChildSpec::new("upstream-mcp-server")?,
    HttpTarget::new("http://backend:3000/mcp")?,
)
.serve(StdioListener)
.await?;
```

The `Bridge` type is the entry point. It accepts a `ChildSpec`
on the listening side and a transport spec on the upstream side,
and serves over the listening transport.

## Bridging to a stateless `2026-07-28` server

A `2026-07-28` server has no handshake and no session: every request
names its revision and the client's capabilities in `params._meta`.
The bridge hides that from an older client. It discovers the
server's revision once, then carries every forwarded request with
that revision and the bridge's own capabilities in its `_meta`.

```bash
protocol-droid bridge \
  --listen stdio:. \
  --upstream http-stateless:http://backend:3000/mcp
```

The target after `http-stateless:` is the server's URL. In code,
`TransportPeer` adapts any `ClientTransport` to the bridge's peer
port, and writes each answer back under the id the downstream
client chose.

## What the bridge offers, and what it refuses

The bridge offers its upstream the newest revision it speaks that
has an `initialize` handshake, and the upstream picks the highest it
supports. A tool result from a newer server reaches an older client
when nothing is lost: a finished result (`resultType: "complete"`)
means the same as a result with no `resultType`, so it is forwarded.
A result that asks the client to do something the older revision
cannot express (`input_required`) is refused with `-32601`, and no
policy changes that.

Other losses are refused by default and allowed on request:

```bash
protocol-droid bridge --listen stdio:. --upstream http://backend:3000/mcp --allow-downgrade
```

`--allow-downgrade` authorises only the downgrades the specification
defines a fallback for. The same flag works with
`--check-result <FILE>`, which reports what the bridge would do with
one reply without opening a socket.

## What the bridge is not

The bridge is not a proxy that improves the wire. A `2024-11-05`
client that sends a `JsonObject` argument to a `2026-07-28`
server's typed `tools/call` parameter will fail the lift, and
the bridge surfaces the typed `LowerError` to the client. The
bridge does not paper over a wire mismatch; it surfaces it.
