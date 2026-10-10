# Introduction

`protocol-droid` is a Rust Model Context Protocol toolkit. It speaks every
published MCP spec revision — `2024-11-05`, `2025-03-26`, `2025-06-18`,
`2025-11-25`, `2026-07-28`, plus the `draft` revision — all in a single
binary, all at once. A consumer never picks a version at compile time.

Three ideas set it apart from `rmcp` / `rust-mcp-sdk` / `turbomcp`:

1. **One canonical internal model, many wire codecs.** Application code
   speaks a version-agnostic superset IR. Per-revision codecs lift wire
   messages into the IR and lower them back to the wire for whichever
   revision was negotiated with the peer. Schema types for every revision
   coexist; no feature is mutually exclusive with another.

2. **Runtime feature gating driven by what the peer actually negotiated.**
   A pure domain function turns (protocol version × client capabilities ×
   server capabilities × transport) into a `FeatureProfile`. Every
   feature-specific code path consults the profile before it runs.
   Unavailable features fail with a typed error, or degrade under an
   explicit, opt-in `DegradePolicy`. Nothing is silently dropped.

3. **Every transport the spec has ever defined.** stdio, the legacy
   HTTP+SSE binding (`2024-11-05`), Streamable HTTP (stateful), and the
   `2026-07-28` stateless lifecycle. A CLI on top makes it a Swiss-army
   knife: a version-translating bridge (old client ↔ new server and
   back), a server prober/fingerprinter, an interactive inspector,
   record/replay, and a conformance runner.

## Goals, in priority order

1. **ACCURATE** — spec-exact, conformance-suite verified on every
   revision.
2. **FAST** — single-pass envelope routing, lazy params decoding,
   benchmark-gated CI.
3. **DYNAMIC** — tools/resources/prompts registered and removed at
   runtime, with `list_changed` emitted only when negotiated.
4. **REACHABLE** — a consumer's entire path is a few obvious calls;
   the layers stay an implementation detail behind them.

## What is in this guide

The chapters in this guide walk through the concepts the code is built
on, and the practical how-tos for a consumer adding `protocol-droid` to
their project.

- [Why one IR](./why-one-ir.md) — the central claim, and the shape
  that makes it true.
- [FeatureProfile and gating](./feature-profile.md) — the runtime
  feature model, and how a `DegradePolicy` opts in to lossy
  downgrades.
- [Write a server](./write-a-server.md) — the consumer-side entry
  point: register a tool, register a resource, serve over stdio or
  HTTP.
- [Write a client](./write-a-client.md) — connect to a server,
  negotiate, call a tool, read a resource.
- [Bridge two peers](./bridge.md) — translate a `2024-11-05` client's
  request into a `2026-07-28` server's response, in either direction.
- [Security model](./security.md) — what the library does, what it
  does not, and the line a consumer crosses when they wire their
  own auth.
- [Spec refresh process](./spec-refresh.md) — how a new revision
  becomes a row in the matrix.
- [Release process](./release.md) — the auto-tag + verify + publish
  chain.
