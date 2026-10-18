# Conformance gaps

The official `@modelcontextprotocol/conformance` suite runs its
server-scenarios against
[`crates/protocol-droid/examples/conformance_reference_server.rs`](../crates/protocol-droid/examples/conformance_reference_server.rs)
through `protocol-droid conformance self`. Suite `0.2.0-alpha.12` is pinned.

## Where it stands

Measured, not projected. Every row below is the output of:

```text
cargo run -p protocol-droid-cli --all-features -- \
    conformance self --spec-version <rev> --suite all --port 0
```

| Revision       | Passed | Failed | Total |
| -------------- | -----: | -----: | ----: |
| `2025-03-26`   |      — |      — |     — |
| `2025-06-18`   |     55 |      0 |    55 |
| `2025-11-25`   |     81 |      0 |    81 |
| `2026-07-28`   |     11 |    108 |   119 |

`2025-03-26` is absent because the pinned suite ships no `2025-03-26`
scenario set. That exclusion is recorded in
[`crates/protocol-droid-cli/conformance/ALLOWLIST.md`](../crates/protocol-droid-cli/conformance/ALLOWLIST.md)
rather than left here to be re-discovered.

The first two revisions are green. Everything below is about `2026-07-28`,
and the reason is one lifecycle change rather than a hundred.

## `2026-07-28`: the stateless lifecycle

`2026-07-28` replaces the session-required Streamable HTTP lifecycle with a
stateless one. A client sends no `Mcp-Session-Id`, and it does not keep a GET
stream open to receive server-initiated messages. The lifecycle this crate
serves is session-required: a request without a session header is refused
with `-32600 missing_session`, and because that is the *first* thing every
scenario does after `initialize`, the failure is total rather than partial.

```text
Error: Failed: this request requires an Mcp-Session-Id header
```

That single refusal accounts for almost all 108. It is one gap with one
cause, not a list.

## `2026-07-28`: SEP-2575

Separate from the lifecycle, and independent of it — these fail on a server
that *is* stateless too:

- `server/discover` is not implemented. The suite requires it, requires a
  server that supports prompts to declare `prompts` in its `DiscoverResult`,
  and requires the declared `capabilities` to match what the server actually
  honours on real RPC calls.
- `_meta` on a stateless request. SEP-2575 carries
  `io.modelcontextprotocol/protocolVersion` and
  `io.modelcontextprotocol/clientCapabilities` there. This crate reads the
  protocol version from the `initialize` handshake, which a stateless peer
  does not have.
- `sep-2575-http-server-error-jsonrpc-id` — error responses must carry the
  request's JSON-RPC id. This crate answers a pre-routing refusal with
  `id: null`, because the refusal happens before the id has been read off
  the envelope.

The third is the one to fix first regardless of the other two: a transport
that cannot echo an id it was handed has a defect that no revision gate
reaches.

## What is *not* a gap

Worth recording, because this file spent most of its life claiming the
opposite.

The failures recorded here between 2026-10-16 and 2026-10-18 were
**library** gaps wearing a fixture's clothes. Each one made a whole class of
scenarios fail for a reason that had nothing to do with the scenario:

| Symptom | Cause |
| ------- | ----- |
| `2025-11-25` 41/64 | The transport answered a negotiated request with its own revision, so a `2025-11-25` peer read a `2025-06-18` reply. |
| Every version, all HTTP | A loopback peer was refused by the DNS-rebinding check as `Host: 127.0.0.1` with no `Origin` at all. |
| `dns-rebinding-protection` | `Host` was never checked, only `Origin` — and a browser always sends `Origin`, so the check had no real user. |
| `2025-06-18` tools pass, then break | The default `Streaming::Json` was honoured on `initialize` and ignored on every request after it, so replies came back as `text/event-stream`. |
| `2025-11-25` elicitation | A tool that elicits under `Streaming::Json` waited forever on a ring nobody drains. It now fails fast, naming the setting. |
| `2025-11-25`, 3 scenarios | The reference server had no `test_elicitation_sep1034_defaults`, `test_elicitation_sep1330_enums`, or `json_schema_2020_12_tool`. |

The lesson is the one this file used to get wrong: the suite reports a
scenario as a single `FAILURE`, and the reason behind it is almost never in
the scenario's own text. Every row above was found by reading the failure
*message*, not the scenario.

## How to reproduce a single failure

```text
cargo run -p protocol-droid-cli --all-features -- \
    conformance self --spec-version 2026-07-28 --suite all --port 0 \
    --scenario tools-list
```

The suite prints the failing check, its error message, and the scenario's
server-implementation requirements. That last block is the contract the
fixture is measured against; everything above it is a diagnosis.
