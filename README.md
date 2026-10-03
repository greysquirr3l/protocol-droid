# protocol-droid

A Rust toolkit for the [Model Context Protocol](https://modelcontextprotocol.io)
that speaks **every** published spec revision — `2024-11-05`, `2025-03-26`,
`2025-06-18`, `2025-11-25`, `2026-07-28`, and the floating `draft` — in a
single binary, all at once. It does not ask you to pick a version at compile
time.

Three ideas set it apart:

1. **One canonical internal model, many wire codecs.** Application code speaks a
   version-agnostic superset IR. Per-revision codecs lift wire messages into the
   IR and lower IR back to the wire for whatever version was negotiated. Schema
   types for every revision coexist; nothing is mutually exclusive.

2. **Runtime feature gating driven by what the peer negotiated.** A pure domain
   function turns _(protocol version × client capabilities × server capabilities
   × transport)_ into a `FeatureProfile`. Every feature-specific path consults
   it. Unavailable features fail with a typed error, or degrade under an
   explicit opt-in policy. Nothing is silently dropped.

3. **Every transport the spec has ever defined.** stdio, legacy HTTP+SSE
   (`2024-11-05`), Streamable HTTP (stateful), and the `2026-07-28` stateless
   lifecycle. A CLI on top makes it a Swiss-army knife: a version-translating
   bridge, a server prober, an interactive inspector, record/replay, and a
   conformance runner.

## Status

Phases 1–2 are in progress. The specification schemas for every revision are
vendored and provenance-verified; the code generator, feature matrix, codecs,
and transports follow.

## Toolchain

Rust stable, pinned. The MSRV and the pinned toolchain are both `1.98.1` — see
[`rust-toolchain.toml`](rust-toolchain.toml).

```bash
cargo build --workspace --all-features
cargo test  --workspace --all-features
cargo l                                                   # the full clippy profile
cargo deny --all-features check
```

`cargo l` is an alias for the strict lint profile. It is the one that gates
commits; a bare `cargo build` is also warning-free because `.cargo/config.toml`
sets `build.warnings = "deny"`.

## Layout

| Crate                          | What it is                                                                    |
| ------------------------------ | ----------------------------------------------------------------------------- |
| `crates/protocol-droid-schema` | Vendored official schemas, the provenance manifest, and the verification gate |
| `crates/protocol-droid`        | The library: IR, codecs, session engine, transports                           |
| `crates/protocol-droid-macros` | Derive macros. Proc-macro crates must be separate                             |
| `crates/protocol-droid-cli`    | The `protocol-droid` binary                                                   |
| `xtask`                        | Developer tooling. Not published                                              |

## The vendored schemas

Every wire type is generated from the official `schema.json` / `schema.ts`, and
those files are pinned by commit and locked by SHA-256. A build refuses to
compile if the tree has drifted from the manifest, and no build ever touches the
network.

```bash
cargo xtask refresh-spec 2026-07-28     # re-fetch a tagged revision
cargo xtask refresh-spec draft --newest  # re-resolve main, repin, and re-fetch
```

Full details — layout, what each field is for, how to vendor a new revision —
are in [`crates/protocol-droid-schema/spec/README.md`](crates/protocol-droid-schema/spec/README.md).

## Architecture

Hexagonal, collapsed to `src/{domain,application,adapters/{primary,secondary}}`.
Domain code has no I/O dependencies and no traits; adapters implement port traits
declared by the application-layer handler that consumes them. Dependencies point
inward: adapters → ports ← domain.

Two rules are worth knowing before reading the code:

- **The protocol-neutral core stays neutral.** The JSON-RPC envelope, session
  tracking, and all transports know nothing about MCP. MCP lives in `mcp/`
  submodules within each layer. That is what lets a future protocol drop in
  without a refactor.
- **All schema revisions are always compiled.** They are data, not features. A
  `--no-default-features` build sheds transports and heavy integrations, never
  spec revisions.

## Releasing

The release chain is automated end to end, with one manual step that is not
visible in the code:

```
CI (on main, green) → auto-tag.yml → tag → release.yml → crates.io
```

A tag is created with `GITHUB_TOKEN`, which deliberately does **not** trigger
other workflows' `on: push` filters — so `release.yml` is triggered by
`workflow_run`, not by `push: tags`, and resolves the tag from the run's
`head_sha`.

> **Before the first release**, create a GitHub Environment named `release`
> (repository → Settings → Environments) and enable **Required reviewers** on
> it.
>
> `release.yml` declares `environment: release`, but nothing in this repository
> creates it. Without that manual step the key is a no-op and the publish job
> runs unattended — the exact thing it was added to prevent. There is also
> nothing to do if you are on a fork with no registry token: the job warns and
> exits 0 instead of failing the chain.

## Licence

MIT OR Apache-2.0.
