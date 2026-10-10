# T22 — Differential interop findings (TypeScript SDK leg)

Findings from running `cargo test --manifest-path
crates/protocol-droid/tests/interop_ts_sdk/Cargo.toml` against
`@modelcontextprotocol/sdk@1.32.1` (pinned in
`fixtures/package.json`).

Each finding has a *leg* (the direction of the cross-test), a
*repro* (the input that surfaces it), and a *claim* (which SDK
is right, with a spec citation). Where the claim is
unresolved, the entry is filed under *open* and a TODO names
the person responsible for closing it.

## Closed

### TS SDK 1.32.1 covers the matrix except `2026-07-28`

- **Leg:** both (`pd_client_to_ts_server_matrix`,
  `ts_client_to_pd_server_matrix`)
- **Repro:** `node fixtures/server.mjs` answers
  `initialize` with `protocolVersion: 2025-11-25` (its
  `LATEST_PROTOCOL_VERSION`) regardless of the version the
  caller asked for. The matrix is therefore the
  intersection of what the TS SDK implements
  (`2025-11-25`, `2025-06-18`, `2025-03-26`,
  `2024-11-05`, plus the `2024-10-07` draft this project
  does not claim) and what `protocol-droid` implements
  (`2026-07-28`, `2025-11-25`, `2025-06-18`,
  `2025-03-26`, `2024-11-05`). The intersection is the
  four revisions the test matrix iterates over.
- **Claim:** both libraries are right. The TS SDK does
  not implement `2026-07-28`'s SEP-2575 stateless
  lifecycle yet, and `protocol-droid` does not claim
  the `2024-10-07` draft. The matrix leg the TS SDK
  does not cover is `2026-07-28`, and the rmcp interop
  crate (its own FINDINGS.md) records the equivalent
  rmcp-side gap. A bump in either library's
  supported set should bump the matrix in lockstep.
- **Resolution:** none required; the test matrix is
  sized to the intersection, and the cross-test's
  `initialize` assertion confirms the negotiated
  version is the one the matrix leg asked for.

### TS SDK client offers its `LATEST_PROTOCOL_VERSION` and the caller cannot override

- **Leg:** TS SDK client → protocol-droid server
  (`ts_client_to_pd_server_matrix`)
- **Repro:** `node fixtures/client.mjs` connects to
  `protocol-droid`'s `stdio_server` example. The TS SDK's
  `Client.connect()` always sends
  `protocolVersion: LATEST_PROTOCOL_VERSION` (currently
  `2025-11-25` at the pinned `1.32.1`); there is no
  public API to override the offered version.
- **Claim:** this is a TS SDK API gap, not a wire bug.
  A consumer that wants to force a specific revision
  has to fork `Client` or build their own envelope
  writer. The cross-test in this direction is
  therefore a *single* test (one round-trip at the
  SDK's offered version), not a per-revision matrix;
  the per-revision variation lives in the other leg
  (`pd_client_to_ts_server_matrix`), where the
  protocol-droid `StdioClient` is the one offering
  the version.
- **Resolution:** none required; the test is
  named `ts_client_to_pd_server_matrix` for symmetry
  with the rmcp crate's `rmcp_client_to_pd_server_matrix`
  but the `_matrix` suffix in this leg is a single
  iteration, not four. A TS SDK release that
  exposes the offered version would let the
  per-revision matrix live in this leg too, and the
  test would then iterate `REVISIONS` the same way
  the other leg does.

## Open

### TS-SDK-001 — Unknown tool answers with `-32603 InternalError`, not the spec's `-32601 MethodNotFound`

- **Leg:** protocol-droid client → TS SDK server
  (`pd_client_to_ts_server_matrix`)
- **Repro:** `tools/call` for a tool the server does
  not advertise (e.g. `definitely_not_a_real_tool`).
  The fixture's `server.mjs` handler throws
  `new Error("Unknown tool: ...")`, which the TS SDK
  converts to a JSON-RPC error with `code: -32603`
  (`InternalError`) and the message as `data`.
- **Claim:** the TS SDK is wrong, and the spec
  recommendation wins. The MCP `tools/call` spec
  says: "If the named tool does not exist, the
  server SHOULD return a JSON-RPC error with code
  -32601 (`MethodNotFound`)." The TS SDK
  treats any thrown `Error` as a generic
  `InternalError`, which is correct for *handler
  bugs* (a tool that throws during execution) and
  wrong for *tool-not-found* (a request the
  server itself should have rejected before
  dispatching). The handler should distinguish
  the two cases.
- **Cross-test impact:** none. The cross-test
  asserts on the positive path; a regression
  here would be a *spec* disagreement the
  conformance suite's
  `tools-call-unknown-tool` scenario already
  covers, not a wire-shape diff between the
  libraries. The cross-test deliberately does
  not assert on the negative path because the
  two SDKs would still agree on the bytes
  (both produce a JSON-RPC error), and a
  cross-test finding is "what does the wire
  look like," not "does the wire match the
  spec."
- **Action:** file an issue against
  [`modelcontextprotocol/typescript-sdk`](https://github.com/modelcontextprotocol/typescript-sdk)
  requesting the tool dispatcher reject
  unknown tool names with `McpError(ErrorCode.MethodNotFound, ...)`
  before falling through to the handler.
