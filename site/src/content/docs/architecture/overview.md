---
title: Overview
description: Hexagonal layout, the protocol-neutral core, and the three rules that govern every layer the code touches.
---

`protocol-droid` is built hexagonal-collapsed, with a domain layer that
holds no I/O dependencies, application handlers that declare the port
traits they consume, and adapters that implement those traits and live
on the outside. The shape is what makes "speak every MCP revision plus
A* protocols through one model" tractable.

## Layer model

The collapsed layout is `src/{domain,application,adapters/{primary,secondary}}`:

| Layer            | Holds                                              | Dependencies |
| ---------------- | -------------------------------------------------- | ------------ |
| `domain/`        | Value objects, aggregates, pure functions          | none         |
| `application/`   | One module per use case; each declares its port    | `domain`     |
| `adapters/primary/`   | Driving adapters (servers, CLI)                | `application`, `domain` |
| `adapters/secondary/` | Driven adapters (clients, clocks, filesystem) | `application`, `domain` |

The arrow points inward: adapters depend on the layers they serve;
domain depends on nothing in the workspace. A consumer who needs to
swap an adapter for a test fake only writes the trait, which lives
in `application/` next to the handler that calls it.

## The protocol-neutral core

The JSON-RPC envelope, the session engine, and every transport live
in protocol-agnostic modules (`domain/jsonrpc`, `application/session`,
`adapters/_/transport`). Nothing in those paths imports an MCP type.
MCP-specific code sits in `mcp/` submodules within each layer; A2A
sits in `a2a/`, and ACP — when it lands — sits in `acp/`.

:::note
This split is what lets a future protocol drop in as a
feature-gated module, not as a refactor. The JSON-RPC
envelope, the session bind, the `Dispatcher` trait, and
every transport already know nothing about MCP.
:::

## Three rules

1. **Domain has no traits, no I/O, no third-party error types.**
   A handler that needs to log, sleep, or read a clock takes the
   port; the adapter is the only place `tracing`, `tokio`, or
   `reqwest` appears. Domain functions are pure.
2. **The port trait returns a DOMAIN error.** An axum handler that
   bubbles `axum::Error` into `application/` is a bug, not a
   convenience. The boundary is where the third-party type
   disappears.
3. **All schema revisions are always compiled.** They are data,
   not features. A `--no-default-features` build drops transports
   and OAuth, never a spec revision.

## Negotiation, the IR, and the codecs

A request arrives, gets routed by the single-pass envelope router
(`jsonrpc`, `id`, `method` lifted from a borrowed `&RawValue`),
negotiated against the peer's capabilities, and lifted into the IR.
The IR is a *superset* of every revision's surface; the codec is the
only thing that knows the wire shape of one revision, and the only
thing that needs to. The application code speaks the IR; the wire
version is whatever the negotiation step chose.

[Why one IR](/protocol-droid/guides/why-one-ir/) walks through the
claim with a concrete `tools/call` example. [FeatureProfile and
gating](/protocol-droid/guides/feature-profile/) walks through the
runtime feature model that sits on top of it.

## The bridge

The bridge is the only piece of the library that knows the wire
shape of two revisions at once. A `2024-11-05` client's request
arrives, gets lifted into the IR, and gets lowered to a `2026-07-28`
server's wire. The reverse direction works the same way. The
consumer never has to think about the wire version the peer speaks;
they drive the library through a single API and the bridge
translates.

[Bridging two peers](/protocol-droid/guides/bridge/) walks through
the bridge surface, including the A*-protocol cross-bridge
configuration.

## What's in the codebase

| Crate                          | Role                                                                          |
| ------------------------------ | ----------------------------------------------------------------------------- |
| `protocol-droid-schema`        | Vendored official schemas, build-time generator, per-revision wire types      |
| `protocol-droid`               | The library: IR, codecs, session engine, transports                           |
| `protocol-droid-macros`        | Derive macros. Proc-macro crates must be separate                             |
| `protocol-droid-cli`           | The `protocol-droid` binary: bridge, probe, inspect, record, replay, conformance |
| `xtask`                        | Developer tooling. Not published                                              |

The full architectural write-up — including the conformance-suite
verification, the differential interop against `rmcp` and the
TypeScript SDK, the security audit, and the fuzz harness — is in
[`docs/ARCHITECTURE.md`](https://github.com/greysquirr3l/protocol-droid/blob/main/docs/ARCHITECTURE.md)
in the repository.
