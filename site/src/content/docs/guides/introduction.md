---
title: Introduction
description: protocol-droid speaks every published MCP spec revision at once, plus A2A and ACP, through a single canonical internal model.
---

`protocol-droid` is a Rust Model Context Protocol toolkit. It speaks every
published MCP spec revision — `2024-11-05`, `2025-03-26`, `2025-06-18`,
`2025-11-25`, `2026-07-28`, plus the `draft` revision — all in a single
binary, all at once. A consumer never picks a version at compile time.

Most SDKs track the latest revision and leave everything behind it behind.
This one does the opposite: one build serves the oldest client and the newest
one. The `2026-07-28` specification now says that clients and servers **MAY**
support multiple protocol versions simultaneously, and it defines Modern,
Legacy and Dual-era peers whose compatibility matrix fails in both directions
with no fall-forward. A modern client talking to a Legacy server is a
supported deployment, not a bug — and somebody has to translate.

The A* protocols (A2A, ACP) ride the same canonical model: they get their own
vendored schemas and codecs, so the bridge translates between protocols without
a consumer having to re-implement their application. Both are opt-in Cargo
features and both are experimental — see
[A2A and ACP](/protocol-droid/guides/introduction/#a2a-and-acp-experimental).

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
   `2026-07-28` stateless lifecycle. A CLI on top turns the library into a
   compatibility toolkit: a version-translating bridge (old client ↔ new
   server and back), a server prober/fingerprinter, an interactive
   inspector, record/replay, and a conformance runner.

## rmcp, and when to use which

[`rmcp`](https://github.com/modelcontextprotocol/rust-sdk) is the official Rust
SDK, and it is a good one. It is the right default when **you control both
ends of the connection** — you are writing the client and the server, both
are current, and nothing older is ever going to connect.

This project is for the case where **you do not control both ends**:

| | `rmcp` | `protocol-droid` |
| --- | --- | --- |
| Revisions it can negotiate | All five, `2024-11-05` through `2026-07-28` | All five, plus the floating `draft` as its own wire type set |
| Wire types per revision | One shared model, shaped at runtime | One generated type set per revision, from the vendored `schema.json` |
| Legacy HTTP+SSE transport (`2024-11-05`) | **Not provided** — a stated non-goal; its README says it will not add it back | Server and client, both halves |
| Translating between two peers' revisions | No | `protocol-droid bridge` |
| Probing what a server actually speaks | No | `protocol-droid probe` |
| Cross-revision record/replay with per-frame verdicts | No | `protocol-droid record` / `replay` |
| Command-line tooling | None | `bridge`, `probe`, `inspect`, `record`, `replay`, `conformance` |
| MSRV | 1.88 | 1.98 |

Those rows are not a scoreboard. `rmcp` is smaller because it does less, and
"less" is the right answer for the case it targets. The row that matters is the
one marked *not provided*: when a legacy `2024-11-05` server is the only thing
on the other end of the socket, `rmcp` cannot reach it at all, and this crate
can — as a client, as a server, or as a bridge between the two.

The second row matters more than it looks. One shared model means a message
that is legal in `2026-07-28` and illegal in `2024-11-05` has to be reshaped
by hand at every boundary, which is exactly where "the peer sent a field this
revision does not define" turns into a dropped field. Here each revision gets
its own types generated from its own vendored schema, and the members a type
does not name are preserved rather than discarded — which is what lets the
bridge forward a message losslessly instead of approximately.

If you want the sharpest version of that: **`rmcp` if you control both ends;
`protocol-droid` if you don't.**

## Conformance

The official `@modelcontextprotocol/conformance` suite runs against this crate
in CI, against every revision it ships. Suite `0.2.0-alpha.12` is pinned.

| Revision     | Passed | Failed | Total |
| ------------ | -----: | -----: | ----: |
| `2025-03-26` |      3 |      0 |     3 |
| `2025-06-18` |     55 |      0 |    55 |
| `2025-11-25` |     81 |      0 |    81 |
| `2026-07-28` |    151 |      0 |   151 |

The client side is covered too — `2025-06-18` 2/2, `2025-11-25` 18/18,
`2026-07-28` 32/32, covering every `auth/*` scenario the suite defines for
each revision. `2024-11-05` has no row because the pinned suite ships no
scenario set for it; the exclusion is recorded rather than assumed away.

Those numbers are measured, not projected, and the gap behind them was real
library bugs rather than fixture work. The full list, with where each fix
lives, is in
[`docs/CONFORMANCE-GAPS.md`](https://github.com/greysquirr3l/protocol-droid/blob/main/docs/CONFORMANCE-GAPS.md).

## A2A and ACP (experimental)

- **A2A v1.0** (`a2a`) — the server, the client, and the cross-protocol bridge
  that translates between an MCP client and an A2A agent. A2A has its own
  conformance runner and a differential interop test against the reference
  implementation.
- **ACP v1.25.0** (`acp-experimental`) — Zed's Agent Client Protocol. The
  feature is named `acp-experimental` rather than `acp` because the
  specification is pre-1.0 and explicitly outside the semver guarantees this
  project makes. The module is incomplete; the remaining work is tracked in
  [`PROGRESS.md`](https://github.com/greysquirr3l/protocol-droid/blob/main/PROGRESS.md).

These are the least stable part of the project by design. Each one is a
specification that moves under us, and MCP — where the compatibility problem is
sharpest — is what this crate is for.

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

- [Why one IR](/protocol-droid/guides/why-one-ir/) — the central claim, and the shape
  that makes it true.
- [FeatureProfile and gating](/protocol-droid/guides/feature-profile/) — the runtime
  feature model, and how a `DegradePolicy` opts in to lossy
  downgrades.
- [Write a server](/protocol-droid/guides/write-a-server/) — the consumer-side entry
  point: register a tool, register a resource, serve over stdio or
  HTTP.
- [Write a client](/protocol-droid/guides/write-a-client/) — connect to a server,
  negotiate, call a tool, read a resource.
- [Bridge two peers](/protocol-droid/guides/bridge/) — translate a `2024-11-05` client's
  request into a `2026-07-28` server's response, in either direction.
- [Security model](/protocol-droid/guides/security/) — what the library does, what it
  does not, and the line a consumer crosses when they wire their
  own auth.
- [Spec refresh process](/protocol-droid/guides/spec-refresh/) — how a new revision
  becomes a row in the matrix.
- [Release process](/protocol-droid/guides/release/) — the auto-tag + verify + publish
  chain.
