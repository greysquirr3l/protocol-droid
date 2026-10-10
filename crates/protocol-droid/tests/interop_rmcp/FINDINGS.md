# T22 — Differential interop findings

Findings from running `cargo test --manifest-path
crates/protocol-droid/tests/interop_rmcp/Cargo.toml` against
`rmcp 3.5.1` (pinned).

Each finding has a *leg* (the direction of the cross-test), a
*repro* (the input that surfaces it), and a *claim* (which SDK
is right, with a spec citation). Where the claim is
unresolved, the entry is filed under *open* and a TODO names
the person responsible for closing it.

## Closed

### `rmcp 3.5.1` negotiates down to `2025-11-25` when asked for `2026-07-28`

- **Leg:** protocol-droid client → rmcp server (`pd_client_to_rmcp_server_matrix`)
- **Repro:** `initialize` with `protocolVersion: 2026-07-28`
- **Claim:** `rmcp` is right, and the matrix is right to keep
  `2026-07-28` out of `REVISIONS`. rmcp's `ProtocolVersion` table
  lists `2025-11-25` as the latest revision it implements;
  protocol-droid's `REVISIONS` table is the *intersection* of
  what both libraries implement, and that intersection stops at
  `2025-11-25` for now.
- **Resolution:** none required; the matrix asserts the
  negotiated version is in the list, and the test passes when
  rmcp negotiates to `2025-11-25`. Recorded for posterity so a
  future bump of `rmcp` to support `2026-07-28` is the signal
  to add the new revision to the matrix.

### protocol-droid stdio_server refuses `initialize` with `2026-07-28`

- **Leg:** rmcp client → protocol-droid server (`rmcp_client_to_pd_server_matrix`)
- **Repro:** rmcp's default `()` client picks the newest version
  it knows, which is `2026-07-28`, and protocol-droid's
  `stdio_server` example answers with `-32601 no such method`
  because `2026-07-28`'s lifecycle (SEP-2575) removed
  `initialize` and the example only implements the pre-`2026`
  shape.
- **Claim:** the example is right to refuse — the spec is
  correct. A client asking for `2026-07-28` should be sending a
  `discover` request, not an `initialize`, and the example does
  not implement `discover`. The cross-test's matrix is the
  right shape: it forces the rmcp client to explicitly opt
  into a revision both libraries agree on (`2025-11-25`).
- **Resolution:** the test uses
  `ClientConfig::new(...).with_protocol_version(2025-11-25)`
  to pin rmcp's offer, and the test passes.

## Open

_None at the pin._
