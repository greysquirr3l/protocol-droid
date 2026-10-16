---
title: CLI
description: Every subcommand the `protocol-droid` binary ships today, the flags they accept, and the contract they expose to an agent caller.
---

The `protocol-droid` binary is a thin adapter over the
`protocol-droid` library. It parses arguments into a typed
contract with `clap`, hands that contract to a use case, and maps
the resulting domain error into an exit code. It contains no
protocol logic of its own.

Every subcommand owes the same two things: a non-interactive
form that does one thing and exits, and machine-readable JSON on
stdout with the human summary on stderr. A REPL would require a
human at a terminal; an agent calling a tool should not have to
hold the whole result in its context window to use it.

## Top-level help

```bash
protocol-droid --help
protocol-droid --version
```

`--version` reports the crate version, a short git SHA (with a
`-dirty` suffix when the working tree had uncommitted changes at
build time), and the last commit's UTC date. The two env vars
that drive the version string are baked by `build.rs`; reading
them with `env!` keeps the format a compile-time constant.

A bare invocation prints help and exits non-zero. The subcommand
list is the source of truth — naming only what is built keeps
`--help` honest about what the binary can actually do.

## `bridge`

Validate and report a version-translating bridge between two
peers. Runs the same bridge the consumer path runs and reports
what version was negotiated, what the `FeatureProfile` is, and
what round-trip succeeded.

```bash
protocol-droid bridge \
  --listen stdio:. \
  --upstream http://backend:3000/mcp
```

`--json` puts the plan or the run report on stdout and the human
summary on stderr, so an agent parses the machine-readable half
and a person watches the other. The exit code is the bridge
result, not a logger's exit code.

## `inspect`

List and call primitives on a server, one-shot. Runs the same
connection the consumer path runs, then performs one `list`,
`schema`, or `call` and writes the answer as JSON on stdout.

```bash
protocol-droid inspect list      --stdio ./upstream-mcp-server
protocol-droid inspect schema   --stdio ./upstream-mcp-server --tool greet
protocol-droid inspect call     --stdio ./upstream-mcp-server --tool greet --arg name=world
```

The one-shot form is the scriptable form, and the scriptable
form is the one that exists. A REPL mode is an addition to that,
not a replacement.

## `probe`

Fingerprint what revisions an MCP server actually speaks. Tries
`initialize` (or `server/discover` on `2026-07-28`) at every
revision this build has wire types for, reports a version ×
capability matrix on stdout, and exits non-zero when nothing
was accepted.

```bash
protocol-droid probe --stdio ./upstream-mcp-server
protocol-droid probe --http http://backend:3000/mcp
```

The matrix is what tells you whether the server is a
`2024-11-05` server, a `2026-07-28` server, or something in
between. It is also what a `bridge` plan reads to decide which
codecs to use.

## `conformance`

Run the official MCP conformance suite against a server or
client. Three modes:

| Mode     | What it does                                                  |
| -------- | ------------------------------------------------------------- |
| `server` | Suite → your server URL                                       |
| `client` | Suite → your client subprocess                                |
| `self`   | Suite → this crate's own fixtures (always available)          |

```bash
protocol-droid conformance self
protocol-droid conformance server --url http://127.0.0.1:8080/mcp
protocol-droid conformance client --stdio ./my-client
```

The report is a JSON object on stdout with one row per test
case, the expected and observed wire form, and the verdict. A
non-zero exit is a failed row in the matrix.

## `replay`

Replay a captured session against a target revision, frame by
frame. Useful for "does the bridge still translate this old
session correctly?" and for re-running a regression that came
from a specific input.

```bash
protocol-droid replay --input session.jsonl --target 2026-07-28
```

The captured session is the `record` subcommand's output. The
frame-by-frame re-encode is the proof that a translation has not
drifted, not a re-test of the original session.

## `record`

Record bridged traffic as a redacted JSONL session log. Pairs
with `replay`. The redaction list mirrors the library's
tracing-layer redaction: tokens, Authorization headers, session
ids in full, and elicitation responses are stripped at write
time, not at read time.

```bash
protocol-droid record --bridge --listen stdio:. --upstream http://backend:3000/mcp --output session.jsonl
```

## A2A subcommands

The A2A surface is gated on `--features a2a` and is reachable
through the same binary. The two main entry points mirror the
A2A reference server:

- `protocol-droid a2a serve` — start a local A2A reference
  server (the example's `a2a_server.rs`, packaged as a
  subcommand).
- `protocol-droid a2a conformance` — drive the in-tree A2A
  v1.0 conformance vectors against a peer.

`a2a serve` and `a2a conformance` accept the same
`A2A_ADDR` / `A2A_PEER` / `A2A_BEARER_TOKEN` env vars the
examples do.

## Exit codes

| Code | Meaning                                                |
| ---- | ------------------------------------------------------ |
| `0`  | Success                                                |
| `1`  | Unbuildable / bad usage                                |
| `2`  | Configuration error (missing env, bad URL)             |
| `3`  | The peer refused the negotiation                       |
| `4`  | A bridge frame failed to lift or lower                 |
| `5`  | The tool call returned an error                        |
| `7`  | Conformance failure                                    |

The exact mapping is in
[`crates/protocol-droid-cli/src/exit.rs`](https://github.com/greysquirr3l/protocol-droid/blob/main/crates/protocol-droid-cli/src/exit.rs).
