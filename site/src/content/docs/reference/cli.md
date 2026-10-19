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

## Transport specs

Anywhere a command takes a server, it is written `kind:target`, or just a
URL for `http`:

| Spec                                  | Meaning                                              |
| ------------------------------------- | ---------------------------------------------------- |
| `http://host/mcp`                     | Streamable HTTP                                      |
| `sse:http://host/sse`                 | Legacy HTTP+SSE (`2024-11-05`)                       |
| `http-stateless:http://host/mcp`      | The `2026-07-28` stateless lifecycle                 |
| `stdio:./name`                        | This process's own stdio (a bridge's listen side)    |

A stdio *upstream* needs a program to run, which `kind:target` has no room
for, so spawn the server yourself and bridge to its HTTP endpoint.

## `bridge`

Validate and report a version-translating bridge between two
peers, or run one. The listen side is stdio or an `http` address; the
upstream is Streamable HTTP, legacy HTTP+SSE, or a stateless
`2026-07-28` server.

```bash
protocol-droid bridge --listen stdio:. --upstream http://backend:3000/mcp
protocol-droid bridge --listen stdio:. --upstream http-stateless:http://backend:3000/mcp
protocol-droid bridge --listen http://127.0.0.1:8080/mcp --upstream sse:http://legacy:3000/sse
```

| Flag                       | What it does                                                           |
| -------------------------- | ---------------------------------------------------------------------- |
| `--listen <SPEC>`          | Where clients connect                                                  |
| `--upstream <SPEC>`        | Where the bridge forwards to                                           |
| `--peer <URL>`             | Bridge to an A2A agent instead (needs the `a2a` feature)               |
| `--allow-downgrade`        | Allow the downgrades the specification defines a fallback for          |
| `--check-result <FILE>`    | Report what the bridge would do with one reply, and exit               |
| `--client-version`, `--server-version` | The two revisions `--check-result` judges between          |
| `--json`                   | The run report as JSON on stdout, the summary on stderr                |

The bridge offers its upstream the newest revision it speaks. A finished
tool result from a newer server reaches an older client; a result that
needs `input_required` does not, and no flag changes that. See
[Bridge two peers](/protocol-droid/guides/bridge/).

## `inspect`

List and call primitives on a server, one-shot. Runs the same
connection the consumer path runs, then performs one `list`,
`schema`, or `call` and writes the answer as JSON on stdout.

```bash
protocol-droid inspect http://backend:3000/mcp list
protocol-droid inspect http://backend:3000/mcp schema greet
protocol-droid inspect http://backend:3000/mcp call greet --args '{"name":"world"}'
protocol-droid inspect https://mcp.example.com/mcp --oauth list
```

`--args` takes a JSON literal, `@path` to read a file, or `-` for stdin.
`--json` puts the machine-readable answer on stdout and the summary on stderr.

`--oauth` signs in with OAuth 2.1 if the server answers `401`: it opens the
system browser and receives the redirect on a loopback port, so a person has
to be present. A client registered ahead of time is named by
`PROTOCOL_DROID_OAUTH_CLIENT_ID` and, if it has one,
`PROTOCOL_DROID_OAUTH_CLIENT_SECRET`; these are environment variables on
purpose, so a secret stays out of shell history. Without the flag a `401` is an
error and nothing is opened. A tool that elicits or samples has no prompt in
one-shot mode, and fails with an error naming the callback.

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
protocol-droid probe http://backend:3000/mcp
protocol-droid probe sse:http://legacy:3000/sse --json
protocol-droid probe http://backend:3000/mcp --versions 2025-11-25,2026-07-28
```

A URL that refuses every `POST` with a 4xx is retried as a legacy
`sse:` endpoint (the backward-compatibility rule from the `2025-03-26`
transport page), and the result is shown only if that works. A legacy
server answers every offer with `2024-11-05`, so the later rows read as
refused, which is accurate.

## `conformance`

Run the official MCP conformance suite against a server or
client. Three modes:

| Mode     | What it does                                                  |
| -------- | ------------------------------------------------------------- |
| `server` | Suite → a server you name with `--url`                        |
| `client` | Suite → a client program you name with `--command`            |
| `self`   | Suite → this crate's own fixture server and client            |

```bash
protocol-droid conformance self --spec-version 2026-07-28
protocol-droid conformance self --spec-version 2025-11-25 --side client
protocol-droid conformance self --spec-version 2026-07-28 --side client --scenario auth/scope-step-up
protocol-droid conformance server --url http://127.0.0.1:8080/mcp --spec-version 2025-11-25
protocol-droid conformance client --command ./my-client --spec-version 2026-07-28
```

`--spec-version` is required. `--side server|client|both` (for `self`)
picks the half to run (naming a `--scenario` runs the server half unless you
say `--side client`), and `--scenario` runs one. The client half covers
the OAuth (`auth/*`) scenarios as well as `initialize`, `tools_call`,
request metadata, the `x-mcp-header` scenarios and `sse-retry`. `--json`
puts the report on stdout. The current results are in
[`CONFORMANCE-GAPS.md`](https://github.com/greysquirr3l/protocol-droid/blob/main/docs/CONFORMANCE-GAPS.md).

## `replay`

Replay a captured session against a target revision, frame by
frame. Useful for "does the bridge still translate this old
session correctly?" and for re-running a regression that came
from a specific input.

```bash
protocol-droid replay session.jsonl --target 2026-07-28
protocol-droid replay session.jsonl --target 2025-11-25 --explain
```

The captured session is the `record` subcommand's output. `--explain`
adds a per-frame summary on stderr. `--diff <SESSION>` compares two captures
line by line. `--as-server` stands up a stdio server that answers from the
recording, and `--as-client` drives a live stdio server (named after `--`)
with the recorded requests and diffs its replies against the recording.

## `record`

Record bridged traffic as a redacted JSONL session log. Pairs
with `replay`. The redaction list mirrors the library's
tracing-layer redaction: tokens, Authorization headers, session
ids in full, and elicitation responses are stripped at write
time, not at read time.

```bash
protocol-droid record --listen stdio:. --upstream http://backend:3000/mcp --out session.jsonl
```

## A2A subcommands

The A2A surface is gated on `--features a2a` and is reachable
through the same binary. The two main entry points mirror the
A2A reference server:

- `protocol-droid a2a-conformance <vectors> --server <url>` — drive the in-tree
  A2A v1.0 conformance vectors against a peer.
- `protocol-droid probe --a2a <url>` — fingerprint an A2A agent from its
  well-known card.
- `protocol-droid bridge --peer <url>` — bridge an MCP client to an A2A agent.

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
