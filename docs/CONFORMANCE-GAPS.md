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
| `2026-07-28`   |     81 |     38 |   119 |

`2025-03-26` is absent because the pinned suite ships no `2025-03-26`
scenario set. That exclusion is recorded in
[`crates/protocol-droid-cli/conformance/ALLOWLIST.md`](../crates/protocol-droid-cli/conformance/ALLOWLIST.md)
rather than left here to be re-discovered.

The first two revisions are green. Everything below is about `2026-07-28`,
and the reason is one lifecycle change rather than a hundred.

## `2026-07-28`: the stateless lifecycle

`2026-07-28` replaces the session-required Streamable HTTP lifecycle with a
stateless one. A client sends no `Mcp-Session-Id`, and every request carries
its own revision and its own capabilities in `params._meta` under
SEP-2575. This crate now serves that: `Unsessioned::Stateless` existed in the
router with no producer, and the handler now produces it when a body names
`io.modelcontextprotocol/protocolVersion`.

The peer for such a request is built from that `_meta` alone —
`HttpHandler::peer_from_meta` — because SEP-2575 says a server MUST NOT infer
capabilities from prior requests. Reading them off a session would be exactly
the inference the SEP closes.

## `2026-07-28`: SEP-2575

- `server/discover` is implemented, with the five members the schema
  requires (`resultType`, `supportedVersions`, `ttlMs`, `cacheScope`,
  `capabilities`) and defined only on the revisions that have it. The
  advertised capabilities are the profile's own advertisement, so a client
  that discovers `prompts` cannot find `prompts/get` refused afterwards.
- `-32022 UnsupportedProtocolVersionError` is produced for a revision this
  build does not speak, with `data.supported` listing every revision it does
  — newest first, which is the order a retrying client wants.
- A `_meta` missing `protocolVersion` or `clientCapabilities` is `-32602`,
  distinguished from a named-but-unknown version, which is `-32022`.

## `2026-07-28`: SEP-2549 result envelope

Every result at this revision requires `resultType`, and the five cacheable
results (four listings and `resources/read`) additionally require `ttlMs` and
`cacheScope`. This was the single largest remaining cause: without it every
list and read answer was schema-invalid, and the suite's client aborts on the
first invalid message, so one missing member took out whole scenarios.

`Dispatcher::dispatch` stamps it once on the way out rather than in each of
the ten result-producing methods, and `result_envelope_defined_in` keeps it
off the four revisions whose schemas have no such member.

## Still failing

| Scenario | Passing | Failing | Why |
| -------- | ------: | ------: | --- |
| `server-stateless` | 8 | 21 | `subscriptions/listen` is unimplemented; the suite waits on a stream that never streams. Also wants `-32020` header-mismatch, `-32021` missing-capability, and `404` on removed legacy methods. |
| `http-custom-header-server-validation` | 1 | 5 | `Mcp-Method` / `Mcp-Name` are not enforced against the body at this revision. |
| `tools-call-with-progress` | 1 | 1 | Progress notifications are not delivered on a stateless request. |
| `input-required-result-*` (10) | 1–3 each | 1 each | Elicitation is not modelled as `InputRequiredResult` at this revision. |

Three scenarios — `resources-list`, `resources-read-text` and
`http-header-validation` — abort in the suite's client rather than reporting a
denominator, so their checks are counted as failures here without a measured
split.

### Carried-over

- **Error responses do not always carry the request's JSON-RPC id.** A
  pre-routing refusal answers `id: null`, because the refusal happens before
  the id is read off the envelope. This is the one item here worth fixing
  regardless of revision, and it is independent of everything above: a
  transport that cannot echo an id it was handed has a defect no revision gate
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
