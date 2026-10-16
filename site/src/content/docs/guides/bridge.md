---
title: Bridge two peers
description: How the version-translating bridge lets a legacy client talk to a modern server, and vice versa, without the application knowing.
---

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

```rust
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

:::note
The same bridge surface is also the entry point for the
cross-protocol bridge between MCP and the A* protocols. The
internal model is one; the wire is just different.
:::

## What the bridge is not

The bridge is not a proxy that improves the wire. A `2024-11-05`
client that sends a `JsonObject` argument to a `2026-07-28`
server's typed `tools/call` parameter will fail the lift, and
the bridge surfaces the typed `LowerError` to the client. The
bridge does not paper over a wire mismatch; it surfaces it.
