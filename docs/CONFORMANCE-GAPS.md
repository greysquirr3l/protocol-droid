# Conformance gaps

The official `@modelcontextprotocol/conformance` suite runs its
server-scenarios against
[`crates/protocol-droid/examples/conformance_reference_server/`](../crates/protocol-droid/examples/conformance_reference_server/main.rs)
through `protocol-droid conformance self`. Suite `0.2.0-alpha.12` is pinned.

## Where it stands

Measured, not projected. Every row is the output of:

```text
cargo run -p protocol-droid-cli --all-features -- \
    conformance self --spec-version <rev> --suite all --port 0
```

| Revision     | Passed | Failed | Total |
| ------------ | -----: | -----: | ----: |
| `2025-03-26` |      3 |      0 |     3 |
| `2025-06-18` |     55 |      0 |    55 |
| `2025-11-25` |     81 |      0 |    81 |
| `2026-07-28` |    151 |      0 |   151 |

`--suite all` runs the scenarios the suite marks `pending` or `not_scored` as
well as the scored ones, so the `2026-07-28` total includes
`json-schema-2020-12`, `http-header-validation` and
`http-custom-header-server-validation`, which the frozen requirements file
does not count. All three pass.

`2024-11-05` has no row: the pinned suite ships no scenario set for it. That
is recorded in
[`ALLOWLIST.md`](../crates/protocol-droid-cli/conformance/ALLOWLIST.md).

## What closed the `2026-07-28` gap

The revision went from 81/119 to 151/151. Almost none of it was fixture work.
The library was missing behaviour the revision requires, and the fixture was
the first client to ask for it.

| Area | What was wrong | Where it lives now |
| ---- | -------------- | ------------------ |
| HTTP status codes | Every refusal answered `200`. SEP-2575 gives an unknown or removed method `404`, and a bad `_meta`, an unsupported version, a header mismatch and a missing client capability `400`. | `adapters/primary/transport/http/stateless.rs` |
| Removed methods | `ping`, `logging/setLevel` and `resources/(un)subscribe` were still answered at `2026-07-28`. | `domain/mcp/client_methods.rs`, a per-revision method table read off each schema's `ClientRequest` union |
| Request headers | `MCP-Protocol-Version`, `Mcp-Method` and `Mcp-Name` were never compared with the body. They are required, and a mismatch is `-32020`. | `stateless.rs` |
| `Mcp-Param-*` | `x-mcp-header` tool arguments were never checked. | `application/mcp/param_headers.rs` |
| Unsupported versions | A version the build could not parse was answered `-32602` instead of `-32022` echoing what was asked. | `stateless.rs` |
| Missing capabilities | Nothing produced `-32021`. | The dispatcher's `InputRequiredResult` check |
| `subscriptions/listen` | Not implemented. | `adapters/primary/transport/http/listen.rs` |
| `list_changed` | The server never emitted one on **any** revision. The fan-out existed and nothing called it. | `application/server/live.rs` |
| Runtime registration | A tool added while serving was listed and could not be called. | `ServerHandle`, `live.rs` |
| SEP-2322 | `InputRequiredResult`, `inputResponses` and `requestState` were not modelled. | `application/mcp/input_required.rs`, with `requestState` HMAC-sealed by the library |
| Logging | `notifications/message` ignored the per-request `logLevel`, and `logging/setLevel` was ignored on every earlier revision too. | `application/server/log_gate.rs` |
| Progress | Stateless requests had no stream to put progress on, and a numeric `progressToken` came back as a string on every revision. | `stateless.rs`, `context.rs` |
| Listings | An absent description was written as `"description": null`, which no schema admits. The `2026-07-28` harness validates every message and aborted the listing scenarios over it. | `dispatch.rs` |
| Isolation | Every stateless request shared session id `1`, so two clients calling the same tool with the same request id got each other's recorded result. | `stateless.rs` |

## Known limitations

None of these is measured by the suite. They're listed because they are real.

- **The bridge cannot forward to a stateless upstream.** `protocol-droid
  bridge --upstream http-stateless:…` refuses with a message saying so.
  Translating a session's handshake into per-request `_meta` is translation
  work the bridge does not do yet. The library's own client *does* speak the
  stateless lifecycle (`StatelessHttpClient`, and `Client::connect_to` with an
  `http-stateless:` spec).
- **No stateless lifecycle over stdio.** A `2026-07-28` peer on stdio is
  answered only after an `initialize`, and `2026-07-28` has none. The HTTP
  transport serves the stateless lifecycle in full.
- **A `Server` cannot be served over the legacy 2024-11-05 HTTP+SSE
  transport.** `LegacyDispatcher` is synchronous and nothing adapts a `Server`
  to it, so a legacy-SSE endpoint needs a hand-written dispatcher. The
  transport has been deprecated since `2025-03-26`; the Streamable HTTP server
  answers 2024-11-05 clients that speak Streamable HTTP.
- **The client-side scenarios are not run.** The matrix is server scenarios
  only. A reference client fixture is the open half of T21.

## How to reproduce a single scenario

```text
cargo run -p protocol-droid-cli --all-features -- \
    conformance self --spec-version 2026-07-28 --suite all --port 0 \
    --scenario server-stateless
```

The suite prints each check, its error message, and the scenario's
server-implementation requirements. Read the failure *message*, not the
scenario name: the reason a scenario fails is almost never in its title.
