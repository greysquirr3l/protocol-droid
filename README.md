# protocol-droid

<p align="center">
  <img src="https://raw.githubusercontent.com/greysquirr3l/protocol-droid/main/assets/img/protocol-droid.svg" alt="protocol-droid" width="180">
</p>

[![crates.io](https://img.shields.io/crates/v/protocol-droid.svg)](https://crates.io/crates/protocol-droid)
[![docs.rs](https://img.shields.io/docsrs/protocol-droid.svg)](https://docs.rs/protocol-droid)
[![CI](https://github.com/greysquirr3l/protocol-droid/actions/workflows/ci.yml/badge.svg)](https://github.com/greysquirr3l/protocol-droid/actions/workflows/ci.yml)
[![Conformance](https://github.com/greysquirr3l/protocol-droid/actions/workflows/conformance.yml/badge.svg)](https://github.com/greysquirr3l/protocol-droid/actions/workflows/conformance.yml)
[![MSRV](https://img.shields.io/badge/MSRV-1.98-blue.svg)](rust-toolchain.toml)

**The MCP compatibility layer.** A Rust toolkit for the
[Model Context Protocol](https://modelcontextprotocol.io) that speaks **every**
published spec revision — `2024-11-05`, `2025-03-26`, `2025-06-18`,
`2025-11-25`, `2026-07-28`, and the floating `draft` — in a single binary, all
at once. It does not ask you to pick a version at compile time, and it does not
ask you to pick a version at deploy time either: one build serves the oldest
client and the newest one.

Most SDKs track the latest revision and leave everything behind it behind. This
one does the opposite. The `2026-07-28` specification now says that clients and
servers **MAY** support multiple protocol versions simultaneously, and it
defines Modern, Legacy and Dual-era peers whose compatibility matrix fails in
both directions with no fall-forward. A modern client talking to a Legacy server
is a supported deployment, not a bug — and somebody has to translate.

**📖 [Read the guide](https://greysquirr3l.github.io/protocol-droid/)** —
the live Astro + Starlight site at `greysquirr3l.github.io/protocol-droid/`
hosts the full guide (introduction, FeatureProfile, write-a-server,
write-a-client, bridge, security model, spec-refresh, release). The site
is built from `site/` in this repo and is updated on every `pnpm run deploy`.

**▶️ [Watch the walkthrough](https://youtu.be/p0hU6RN5pVM)** — *"protocol-droid:
Every MCP Spec Revision in One Rust Binary"*, a tour of the crate and the CLI.

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
   lifecycle — each with both a server and a client half.

On top of that library sits a command-line compatibility toolkit: a
version-translating `bridge`, a `probe` that fingerprints what a server
actually speaks, a one-shot `inspect`, `record`/`replay` with per-frame
diffing, and a `conformance` runner that drives the official suite.

## rmcp, and when to use which

[`rmcp`](https://github.com/modelcontextprotocol/rust-sdk) is the official Rust
SDK, and it is a good one. It is the right default when **you control both ends
of the connection** — you are writing the client and the server, both are
current, and nothing older is ever going to connect.

This project is for the case where **you do not control both ends**:

| | `rmcp` | `protocol-droid` |
| --- | --- | --- |
| Revisions it can negotiate | All five, `2024-11-05` through `2026-07-28` | All five, plus the floating `draft` as its own wire type set |
| Wire types per revision | One shared model, shaped at runtime | One generated type set per revision, from the vendored `schema.json` |
| Legacy HTTP+SSE transport (`2024-11-05`) | **Not provided** — a stated non-goal; its README says it will not add it back | Server and client, both halves |
| Translating between two peers' revisions | No | `protocol-droid bridge` |
| Probing what a server actually speaks | No | `protocol-droid probe` |
| Cross-revision record/replay with per-frame verdicts | No | `protocol-droid record` / `replay` |
| Command-line tooling | None | `bridge`, `probe`, `inspect`, `record`, `replay`, `conformance` |
| MSRV | 1.88 | 1.98 |

Those rows are not a scoreboard. `rmcp` is smaller because it does less, and
"less" is the right answer for the case it targets. The row that matters is the
one marked *not provided*: when a legacy `2024-11-05` server is the only thing
on the other end of the socket, `rmcp` cannot reach it at all, and this crate
can — as a client, as a server, or as a bridge between the two.

The second row matters more than it looks. One shared model means a message
that is legal in `2026-07-28` and illegal in `2024-11-05` has to be reshaped
by hand at every boundary, which is exactly where "the peer sent a field this
revision does not define" turns into a dropped field. Here each revision gets
its own types generated from its own vendored schema, and the members a type
does not name are preserved rather than discarded — which is what lets the
bridge forward a message losslessly instead of approximately.

If you want the sharpest version of that: **`rmcp` if you control both ends;
`protocol-droid` if you don't.**

Both are checked against each other in CI — a differential interop harness
drives both libraries' servers with the same client and asserts the wire
shapes agree. Details in
[`tests/interop_rmcp/FINDINGS.md`](crates/protocol-droid/tests/interop_rmcp/FINDINGS.md).

## Conformance

The official [`@modelcontextprotocol/conformance`](https://www.npmjs.com/package/@modelcontextprotocol/conformance)
suite runs against this crate in CI, against every revision it ships. Suite
`0.2.0-alpha.12` is pinned.

| Revision     | Passed | Failed | Total |
| ------------ | -----: | -----: | ----: |
| `2025-03-26` |      3 |      0 |     3 |
| `2025-06-18` |     55 |      0 |    55 |
| `2025-11-25` |     81 |      0 |    81 |
| `2026-07-28` |    151 |      0 |   151 |

The client side is covered too — `2025-06-18` 2/2, `2025-11-25` 18/18,
`2026-07-28` 32/32, covering every `auth/*` scenario the suite defines for
each revision. `2024-11-05` has no row because the pinned suite ships no
scenario set for it; the exclusion is recorded rather than assumed away.

Those numbers are measured, not projected, and the gap behind them was real
library bugs rather than fixture work. The `2026-07-28` revision went from
81/119 to 151/151 because every refusal was answering `200`, `list_changed`
was never emitted on any revision, every stateless request shared session id
`1` and leaked results between clients, and an absent description was written
as `"description": null`, which no schema admits. The full list, with where
each fix lives, is in
[`docs/CONFORMANCE-GAPS.md`](docs/CONFORMANCE-GAPS.md).

You can reproduce any of it:

```bash
cargo run -p protocol-droid-cli --all-features -- \
    conformance self --spec-version 2026-07-28 --suite all --port 0
```

## Installing

```toml
[dependencies]
protocol-droid = "0.7"
```

The default build is `serde` + `stdio`, which is enough to *serve* over stdio
and to speak every protocol revision. Everything else is a feature, because a
consumer that links this as a library should not pay for a server it never
starts:

| Feature         | What it turns on                                                       |
| --------------- | ---------------------------------------------------------------------- |
| `serde`         | `serde_json` as the envelope decoder. **On by default**, and always compiled anyway — see below |
| `stdio`         | The stdio transport, server and client. **On by default**               |
| `http`          | Streamable HTTP plus the `2026-07-28` stateless lifecycle              |
| `legacy-sse`    | The deprecated `2024-11-05` HTTP+SSE transport, client and server (`HttpBuilder::legacy_sse()`). Implies `http` |
| `oauth`         | The OAuth 2.1 resource server and PKCE client. Implies `http`          |
| `a2a`           | The A2A v1.0 server, client, and cross-protocol bridge. Experimental — see [A2A and ACP](#a2a-and-acp-experimental) |
| `acp-experimental` | Zed's ACP v1.25.0. Pre-1.0, so it carries no semver guarantee. Experimental and incomplete |
| `random-ids`    | CSPRNG session ids instead of a counter                                |

A `simd` envelope parser is described in the manifest but is not yet wired to a
feature — it exists so the two decoders can be benchmarked rather than argued
about, and it will not appear here until it does.

Serving over HTTP needs `http` named explicitly, because the default build is
stdio-only. Reaching for `ServerConfig` or `HttpBuilder` without it fails with
``cannot find `transport` in `primary` `` and a `found an item that was
configured out` note — the module is behind the feature, not missing:

```toml
protocol-droid = { version = "0.7", features = ["http"] }
```

Add `oauth` for the resource-server and PKCE client halves (it implies `http`),
and `random-ids` if you want CSPRNG session ids rather than a counter.

A lean build — the IR, every codec, every wire type, and no executor at all:

```toml
protocol-droid = { version = "0.7", default-features = false, features = ["serde"] }
```

`default-features = false` drops the executor, the transport adapters, and the
diagnostics channel with it. What it never drops is a spec revision: the schema
types for all six are data, not features, so `--no-default-features` is a
smaller binary and never a smaller protocol surface.

The CLI is a separate crate, so it installs on its own:

```bash
cargo install protocol-droid-cli
```

It versions in lockstep with the library rather than independently — the four
crates are published from one workspace and share a version, which is what
makes `cargo install protocol-droid-cli` pull a `protocol-droid` that matches
what its `bridge` subcommand was built against.

## Using it

Serving a server is a struct, an impl block, and one call:

```rust
/// A greeter: one tool, no state.
struct Greeter;

#[protocol_droid::server(name = "greeter")]
impl Greeter {
    /// Greets someone by name.
    #[protocol_droid::tool]
    async fn greet(name: String) -> String {
        format!("hello, {name}")
    }
}

Greeter.into_server()?.serve_stdio().await?
```

`#[protocol_droid::server]` goes on the impl block and generates the one method
that matters — `into_server`. The marks inside it (`#[tool]`, `#[resource]`,
`#[prompt]`) register as it goes: each parameter becomes a property in the tool's
`inputSchema`, which is why nothing above declares a schema, and the doc comment
becomes the `description` a peer reads. A `#[tool]` handler is `async` even when
it awaits nothing, because the call the macro generates has to produce a future.

The version a peer sees in `serverInfo` is the version of *your* crate, not of
`protocol-droid` — a server that merely links this crate is not this crate. To
say so explicitly, name it; the field takes an expression, so the version is
written once and cannot drift from your manifest:

```rust
#[protocol_droid::server(name = "greeter", version = env!("CARGO_PKG_VERSION"))]
```

Two lines are missing from the snippet above because they are not the library's
business: a `struct Greeter;` declaration, and the runtime. `serve_stdio` is
`async`, so `main` needs one — and the crate does not enable `rt-multi-thread`,
so it is built by hand rather than by `#[tokio::main]`. The full version is
[`examples/stdio_server.rs`](crates/protocol-droid/examples/stdio_server.rs),
and it compiles.

Bridging a client on one revision to a server on another:

```bash
protocol-droid bridge \
  --listen stdio:. \
  --upstream http://backend:3000/mcp
```

The upstream can also be a legacy HTTP+SSE server (`sse:http://host/sse`) or a
stateless `2026-07-28` one (`http-stateless:http://host/mcp`), so a
`2025-11-25` client can use a server that has no handshake at all.

A server that needs a login is reached with `--oauth`, which signs in through
the system browser and refreshes the token when it expires:

```bash
protocol-droid inspect https://mcp.example.com/mcp --oauth list
```

To serve the `2024-11-05` transport beside Streamable HTTP, add `.legacy_sse()`
to the `HttpBuilder`; the same tools answer on both.

Changing a server while it serves goes through a `ServerHandle`. Every change
is published and then announced as `notifications/*/list_changed` to the
sessions that negotiated it — on stdio, on a Streamable HTTP session, or on a
`2026-07-28` `subscriptions/listen` stream:

```rust
let server = Greeter.into_server()?;
let handle = server.handle();            // cheap to clone; store it anywhere
handle.add_tool(tool, handler).await;     // listed, callable, announced
handle.notify_resource_updated("file:///notes.md").await;
```

At `2026-07-28` a server cannot send a request of its own, so a tool that needs
the user answers with an `InputRequiredResult` and reads the reply on the
client's retry. `requestState` is sealed by the library, so a client that edits
it is refused before the handler runs:

```rust
if let Some(answer) = ctx.input_response("name") {
    return text(&format!("hello, {}", answer.as_str()));
}
InputRequired::new()
    .elicit("name", &ask)
    .with_state(ctx.seal_state("round-1")?)
    .into_result()
```

The full worked set — every SEP-2322 shape the conformance suite exercises — is
in [`examples/conformance_reference_server/stateless.rs`](crates/protocol-droid/examples/conformance_reference_server/stateless.rs).

`--json` puts the plan or the run report on stdout and the human summary on
stderr, so an agent parses the machine-readable half and a person watches the
other. `cargo l` is the lint profile; `cargo test --workspace --all-features` is
the suite.

## The generated wire types

`protocol-droid-schema` turns each vendored `schema.json` into serde types
during `build.rs`, and the `wire` module that `src/lib.rs` declares re-exports
the result as one module per revision:

```rust
use protocol_droid_schema::wire::{v2024_11_05, v2026_07_28};

// The same name, two revisions, two different shapes — both live in one build.
let old: v2024_11_05::CallToolResult =
    serde_json::from_str(r#"{"content": [], "isError": true}"#)?;
let new: v2026_07_28::CallToolResult =
    serde_json::from_str(r#"{"content": [], "resultType": "complete"}"#)?;

assert_eq!(old.is_error, Some(true));
assert!(!old.extra.contains_key("resultType"));   // 2024-11-05 has no such member
```

`resultType` is required in `2026-07-28` and does not exist at all in
`2024-11-05`, and `content` is a different Rust type in each. That is the whole
point: the revisions coexist as distinct types, and deciding which one a message
belongs to is a runtime question answered by negotiation, not a compile-time one
answered by a cargo feature.

> **One upstream oddity worth knowing about.** The vendored
> `2025-11-25/schema.ts` declares
> `LATEST_PROTOCOL_VERSION = "DRAFT-2025-v3"` while its own tag is `2025-11-25`
> and every other revision's `schema.ts` carries its own date. It looks like an
> unreleased-build leftover in a tagged revision.
> [`ProtocolVersion`](crates/protocol-droid/src/domain/protocol_version.rs)
> accepts `2025-11-25` and rejects `DRAFT-2025-v3`: the tag is what the
> specification documents and what peers negotiate on, and accepting a string
> the specification does not define would mean announcing a version we cannot
> speak. The finding is recorded in PROGRESS.md rather than papered over.

Three properties of the generated code are load-bearing, and each has a test:

- **Members the type does not name are preserved.** Every object type carries a
  `#[serde(flatten)] extra: Map<String, Value>`, so a field the peer sent that
  this revision does not define survives a decode/encode cycle — including a
  `resultType` from a newer server arriving at a `2024-11-05` client. That is
  what makes the version-translating bridge lossless rather than
  lossy-but-tidy.
- **Absence is absence, not `null`.** An optional member is
  `Option<T>` with `skip_serializing_if = "Option::is_none"`, so a decode/encode
  cycle does not invent members the peer never sent.
- **A value that round-trips still satisfies the schema.** The property suite
  generates arbitrary values that validate against the vendored `schema.json`,
  decodes them into the generated type, and asserts the re-encoded value is
  byte-identical to the original and validates again. Dropping either serde
  attribute above makes it fail.

The generator lives in `crates/protocol-droid-schema/src/generator/`. Nothing is
checked in: `build.rs` verifies the vendored SHA-256 digests first and only then
regenerates, so drift from the vendored bytes is a build failure rather than a
diff someone has to notice.

## A2A and ACP (experimental)

The same internal model carries two more protocols. **Both are opt-in Cargo
features, and both are experimental** — a consumer that only wants MCP never
pays for them, and neither is covered by a semver guarantee.

- **A2A v1.0** (`a2a`) — the server, the client, and the cross-protocol bridge
  that translates between an MCP client and an A2A agent. The bridge is a flag
  on the bridge you already have:

  ```bash
  protocol-droid bridge --listen stdio:. --peer http://agent:9000
  ```

  A2A has its own conformance runner and a differential interop test against
  the reference implementation. The `a2a` feature belongs to the **CLI** crate
  as well as the library, so `cargo install protocol-droid-cli --features a2a`
  is what puts `--peer` on the binary.

- **ACP v1.25.0** (`acp-experimental`) — Zed's Agent Client Protocol. The
  feature is named `acp-experimental` rather than `acp` because the
  specification is pre-1.0 and explicitly outside the semver guarantees this
  project makes. The module is incomplete: the versioned types, the method
  table, the feature profile and the stdio peer are in place, and the
  remaining work is tracked in [`PROGRESS.md`](PROGRESS.md).

These are the least stable part of the project by design. Each one is a
specification that moves under us, and MCP — where the compatibility problem is
sharpest — is what this crate is for.

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

> **On the MSRV.** `1.98` is higher than the *code* needs: the workspace
> compiles clean on `1.97.1` with every feature and every target enabled, and
> no dependency in the graph declares a higher floor. The floor is `1.98`
> because the *lint profile* is.
>
> `cargo l` denies warnings, and the lint set moves between those two
> toolchains in both directions. `clippy::unused_async_trait_impl` does not
> exist before `1.98`; `duration_suboptimal_units` fires on `1.97` and not on
> `1.98`; `let_underscore_future` is denied by `clippy::suspicious` on `1.97`;
> and `dead_code_pub_in_binary` — unknown on `1.96`, known from `1.97` — is
> promoted to library crates in `1.98`, which is what the crate's own
> `#![allow]` and `#[expect]` attributes are written against. Pinning the
> floor at `1.98` means the strict profile that gates every commit runs
> against the same toolchain the crate claims to support.
>
> Lowering it is possible and is on the list of things to revisit, but it is
> not a one-line change to `rust-version` — it is reconciling a lint profile
> across two toolchains, and it should be done deliberately.

## Layout

| Crate                          | What it is                                                                       |
| ------------------------------ | -------------------------------------------------------------------------------- |
| `crates/protocol-droid-schema` | Vendored official schemas, the build-time generator, and the wire types it emits |
| `crates/protocol-droid`        | The library: IR, codecs, session engine, transports                              |
| `crates/protocol-droid-macros` | Derive macros. Proc-macro crates must be separate                                |
| `crates/protocol-droid-cli`    | The `protocol-droid` binary                                                      |
| `xtask`                        | Developer tooling. Not published                                                 |

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

Full treatment, with diagrams, in [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

Hexagonal, collapsed to `src/{domain,application,adapters/{primary,secondary}}`.
Domain code has no I/O dependencies and no traits; adapters implement port traits
declared by the application-layer handler that consumes them. Dependencies point
inward: adapters → ports ← domain.

Three rules are worth knowing before reading the code:

- **The protocol-neutral core stays neutral.** The JSON-RPC envelope, session
  tracking, and all transports know nothing about MCP. MCP lives in `mcp/`
  submodules within each layer. That is what lets a future protocol drop in
  without a refactor.
- **All schema revisions are always compiled.** They are data, not features. A
  `--no-default-features` build sheds transports and heavy integrations, never
  spec revisions.
- **There is no `ports` module, and that is deliberate.** Port traits belong to
  the consumer, so each handler declares its own beside the code that calls it.
  A shared ports module is how a method added for one use case ends up on every
  other one's interface.

## Maintaining this repository

The git hooks, the GitHub-side security settings, branch protection, the
release chain and the docs-site deploy are documented in
[`docs/dev/repo-operations.md`](docs/dev/repo-operations.md). They are about
running the repository rather than using the crates, and they belonged here
only until the README had grown into a maintainer's manual.

Two of them are easy to trip over without knowing they exist:

- **The docs site builds in CI, but not quickly.** `site.yml` builds it on any
  PR that touches `site/`. Running `pnpm build` locally first is still faster
  than waiting for the run.
- **The security settings live in GitHub's database**, not in the tree, so
  there is no commit that turns them on and no way to see them in a diff.
  That document records which are on and how to re-apply each one.

## Licence

MIT OR Apache-2.0.
