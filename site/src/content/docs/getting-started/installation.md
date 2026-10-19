---
title: Installation
description: How to add protocol-droid to a Cargo project, what the feature flags gate, and the toolchain pin the crate requires.
---

Add the library with `cargo add`. The crate is published as
`protocol-droid` on crates.io, and the macro crate `protocol-droid-macros`
is a transitive dependency that `cargo` resolves automatically.

```bash
cargo add protocol-droid
```

The crate compiles on stable Rust 1.98.1 — see
[`rust-toolchain.toml`](https://github.com/greysquirr3l/protocol-droid/blob/main/rust-toolchain.toml)
for the exact pin. A `cargo` invocation that picks a different toolchain
will be auto-redirected by the file, so a clone of the repo is reproducible
end-to-end.

## Feature flags

The default build is the domain, the IR, every codec, and the stdio
transport. Everything else is additive and does not change behaviour on the
default path:

| Feature            | What it adds                                                                     |
| ------------------ | -------------------------------------------------------------------------------- |
| `stdio`            | The stdio transport, server and client. **On by default**                        |
| `http`             | Streamable HTTP and the `2026-07-28` stateless lifecycle, client and server      |
| `legacy-sse`       | The deprecated `2024-11-05` HTTP+SSE transport, client and server. Implies `http` |
| `oauth`            | The OAuth 2.1 resource server and client (PKCE, discovery, registration, refresh, browser sign-in). Implies `http` |
| `random-ids`       | CSPRNG session ids instead of a counter. An HTTP server needs ids from somewhere |
| `a2a`              | The A2A v1.0 server, client, and cross-protocol bridge. Implies `http`           |
| `acp-experimental` | Zed's Agent Client Protocol v1.25.0. Pre-1.0, no semver guarantee                |

The CLI is a separate crate (`protocol-droid-cli`); its `bridge` feature, on by
default, pulls in `http`, `stdio`, `legacy-sse`, `random-ids` and `oauth`.

All schema revisions are *always* compiled. They are data, not optional
features — a `--no-default-features` build still speaks every revision,
it just does not bring transports or OAuth along with it.

## Toolchain

```bash
cargo build --workspace --all-features
cargo test  --workspace --all-features
cargo l                                                   # the full clippy profile
cargo deny --all-features check
```

`cargo l` is an alias for the strict lint profile. It is the one that
gates commits; a bare `cargo build` is also warning-free because
`.cargo/config.toml` sets `build.warnings = "deny"`. Full tooling
guidance lives in the [Development guide](/protocol-droid/contributing/development/).

## Vendored schemas

Every wire type is generated from the official `schema.json` / `schema.ts`,
and those files are pinned by commit and locked by SHA-256. A build
refuses to compile if the tree has drifted from the manifest, and no
build ever touches the network.

To re-fetch a schema after a new MCP release:

```bash
cargo xtask refresh-spec 2026-07-28      # re-fetch a tagged revision
cargo xtask refresh-spec draft --newest   # re-resolve main, repin, and re-fetch
```

Full details — layout, what each field is for, how to vendor a new
revision — are in
[`crates/protocol-droid-schema/spec/README.md`](https://github.com/greysquirr3l/protocol-droid/blob/main/crates/protocol-droid-schema/spec/README.md).
