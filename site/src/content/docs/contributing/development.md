---
title: Development
description: How to set up the workspace, the lint and test commands that gate commits, and the layout a new crate or new spec revision will live in.
---

The repository is a Cargo workspace with five crates. The full layout
is in the README, but the day-to-day commands a contributor will run
are below.

## Toolchain

Rust stable, pinned. The MSRV and the pinned toolchain are both
`1.98.1` — see
[`rust-toolchain.toml`](https://github.com/greysquirr3l/protocol-droid/blob/main/rust-toolchain.toml)
in the repo root. A bare `cargo` invocation will resolve to that
channel because of the file.

```bash
cargo build --workspace --all-features
cargo test  --workspace --all-features
cargo l                                                   # the full clippy profile
cargo deny --all-features check
```

`cargo l` is an alias for the strict lint profile. It is the one that
gates commits; a bare `cargo build` is also warning-free because
`.cargo/config.toml` sets `build.warnings = "deny"`.

## Layout

| Crate                          | Role                                                                          |
| ------------------------------ | ----------------------------------------------------------------------------- |
| `crates/protocol-droid-schema` | Vendored official schemas, the build-time generator, the wire types it emits |
| `crates/protocol-droid`        | The library: IR, codecs, session engine, transports                           |
| `crates/protocol-droid-macros` | Derive macros. Proc-macro crates must be separate                             |
| `crates/protocol-droid-cli`    | The `protocol-droid` binary                                                   |
| `xtask`                        | Developer tooling. Not published                                              |

Keep the crate count minimal. New functionality that does not need
its own crate belongs in the closest existing one. A new crate is
only justified when the cost of pulling something out (a tighter
`Cargo.toml`, a smaller rebuild graph, a clearer ownership boundary)
exceeds the cost of keeping it.

## Hexagonal layout

Every crate uses the collapsed hexagonal / DDD-lite layout:
`src/{domain,application,adapters/{primary,secondary}}`. There is
**no `ports` module**.

- `domain/` holds value objects, aggregates, and pure functions
  only — no traits, no I/O, no `tokio`, no `serde_json::Value` in
  domain signatures.
- `application/` has one module per use case. Each handler
  declares its own narrow port trait inline (1–3 methods).
  Composed via supertraits only when genuinely needed.
- Port traits belong to the **consumer** — declared in the
  handler / use-case module inside `application/`, never in an
  adapter crate and never in a shared `ports` module. A port
  trait returns DOMAIN error types, not infrastructure ones.
- `adapters/primary/` is the driving side: server transports, CLI.
  `adapters/secondary/` is the driven side: client transports,
  upstream MCP peers, clocks, RNG, filesystem.

The transaction / resource scope stays inside the adapter. The live
analogue is the `ArcSwap` snapshot: a handler REQUESTS a publish, it
never mutates a snapshot in place, and the swap is a single atomic
operation.

## Tests

The test suite is `cargo test --workspace --all-features`. The
workspace currently runs **2,015 tests** across the crate graph; CI
runs the same command on three OSes (ubuntu-latest,
macos-latest, windows-latest) and in two feature modes
(all-features, no-default-features), so a new test that only
passes on one of the six cells is a CI failure waiting to happen.

The conformance suite is a separate binary:

```bash
cargo run -p protocol-droid-cli -- conformance self --spec-version 2026-07-28
cargo run -p protocol-droid-cli -- conformance self --spec-version 2026-07-28 --side client
cargo run -p protocol-droid-cli -- conformance server --url http://127.0.0.1:8080/mcp --spec-version 2025-11-25
```

The first mode runs the suite against fixtures in the repo. The
second mode runs it against a live server. Both emit a JSON report
on stdout and a human summary on stderr.

## CI / branch protection

`main` is protected by
[`.github/branch-protection.json`](https://github.com/greysquirr3l/protocol-droid/blob/main/.github/branch-protection.json),
which follows the OpenSSF Baseline branch-protection controls.
Re-apply it with the helper script:

```bash
./scripts/apply-branch-protection.sh          # defaults to the current repo
./scripts/apply-branch-protection.sh owner/repo
```

There is no sync _from_ GitHub. Editing protection in the settings
UI works, but the next run of that script reverts it, so changes
belong in the file.

The nine status checks CI actually reports are listed in the
README; they are not the job ids in `ci.yml` — the test job is a
3 × 2 matrix, so it emits six separate checks, and naming a single
literal `test` context would leave five of them ungated while the
branch looked protected.

## Adding a new spec revision

The full process is in
[Spec refresh process](/protocol-droid/guides/spec-refresh/). The
short form: vendor the schema, run codegen, add a row to the
`FeatureProfile` matrix with a citation, re-record the
`feature_matrix_golden` snapshot, run the conformance suite
against the new revision's reference server, and add a row to
`tests/negotiation.rs` that exercises the new revision on both
sides.

## Adding a new spec protocol (MCP ↔ A2A ↔ ACP)

The protocol-neutral core stays neutral. A new protocol gets a
submodule at each layer (`mcp/`, `a2a/`, `acp/`) that holds the
protocol-specific wire types, codecs, and handlers. The JSON-RPC
envelope, the session engine, and the transports do not change.
The cross-protocol bridge is the protocol pair
configuration on top of the existing `Bridge` type.
