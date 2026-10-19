# protocol-droid

<p align="center">
  <img src="https://raw.githubusercontent.com/greysquirr3l/protocol-droid/main/assets/img/protocol-droid.svg" alt="protocol-droid" width="180">
</p>

[![crates.io](https://img.shields.io/crates/v/protocol-droid.svg)](https://crates.io/crates/protocol-droid)
[![docs.rs](https://img.shields.io/docsrs/protocol-droid.svg)](https://docs.rs/protocol-droid)
[![CI](https://github.com/greysquirr3l/protocol-droid/actions/workflows/ci.yml/badge.svg)](https://github.com/greysquirr3l/protocol-droid/actions/workflows/ci.yml)
[![Conformance](https://github.com/greysquirr3l/protocol-droid/actions/workflows/conformance.yml/badge.svg)](https://github.com/greysquirr3l/protocol-droid/actions/workflows/conformance.yml)
[![MSRV](https://img.shields.io/badge/MSRV-1.98-blue.svg)](rust-toolchain.toml)

A Rust toolkit for the [Model Context Protocol](https://modelcontextprotocol.io)
that speaks **every** published spec revision — `2024-11-05`, `2025-03-26`,
`2025-06-18`, `2025-11-25`, `2026-07-28`, and the floating `draft` — in a
single binary, all at once. It does not ask you to pick a version at compile
time. The A\* protocols ride the same internal model — A2A v1.0 and ACP
v1.25.0 are vendored alongside the MCP schemas, the cross-protocol
bridge (`protocol-droid bridge_a2a`) translates between MCP and A2A
without a consumer having to re-implement their application, and a
consumer that only wants MCP never pays for the A\* codecs (`a2a` and
`acp-experimental` are opt-in Cargo features).

**📖 [Read the guide](https://greysquirr3l.github.io/protocol-droid/)** —
the live Astro + Starlight site at `greysquirr3l.github.io/protocol-droid/`
hosts the full guide (introduction, FeatureProfile, write-a-server,
write-a-client, bridge, security model, spec-refresh, release). The site
is built from `site/` in this repo and is updated on every `pnpm run deploy`.

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
   lifecycle — each with both a server and a client half. The CLI on top is
   being built to a Swiss-army knife; today it has `bridge`, `probe`,
   `inspect`, `record`, `replay`, and `conformance`.

## Installing

```toml
[dependencies]
protocol-droid = "0.2"
```

The default build is `serde` + `stdio`, which is enough to *serve* over stdio
and to speak every protocol revision. Everything else is a feature, because a
consumer that links this as a library should not pay for a server it never
starts:

| Feature         | What it turns on                                                       |
| --------------- | ---------------------------------------------------------------------- |
| `stdio`         | The stdio transport, server and client. **On by default**               |
| `http`          | Streamable HTTP plus the `2026-07-28` stateless lifecycle              |
| `legacy-sse`    | The deprecated `2024-11-05` HTTP+SSE transport, client and server (`HttpBuilder::legacy_sse()`). Implies `http` |
| `oauth`         | The OAuth 2.1 resource server and PKCE client. Implies `http`          |
| `a2a`           | The A2A v1.0 server, client, and cross-protocol bridge                  |
| `acp-experimental` | Zed's ACP v1.25.0. Pre-1.0, so it carries no semver guarantee         |
| `random-ids`    | CSPRNG session ids instead of a counter                                |

A `simd` envelope parser is described in the manifest but is not yet wired to a
feature — it exists so the two decoders can be benchmarked rather than argued
about, and it will not appear here until it does.

A lean build — the IR, every codec, every wire type, and no executor at all:

```toml
protocol-droid = { version = "0.2", default-features = false, features = ["serde"] }
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
during `build.rs`, and `src/wire.rs` re-exports the result as one module per
revision:

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

## Branch protection

`main` is protected by [`.github/branch-protection.json`](.github/branch-protection.json),
which follows the OpenSSF Baseline branch-protection controls. Re-apply it with:

```bash
./scripts/apply-branch-protection.sh          # defaults to the current repo
./scripts/apply-branch-protection.sh owner/repo
```

There is no sync _from_ GitHub. Editing protection in the settings UI works,
but the next run of that script reverts it, so changes belong in the file.

| Setting                 | Value       | Why                                       |
| ----------------------- | ----------- | ----------------------------------------- |
| Required PR reviews     | 1           | No direct commits to `main`               |
| Stale reviews dismissed | yes         | Approval does not survive new pushes      |
| Conversation resolution | required    | Review threads get resolved, not ignored  |
| Force pushes            | blocked     |                                           |
| Branch deletion         | blocked     | Deleting the primary branch is deliberate |
| Status checks           | 9, strict   | See below                                 |
| **`enforce_admins`**    | **`false`** | **Deviation — see below**                 |

> **Deviation: admins can bypass.** `enforce_admins` is `false`, so a repository
> admin can push straight to `main` and bypass every rule above. That does not
> meet the baseline, which treats admin bypass as a finding. It is on for now
> because this is a solo repository and an admin-only lockout is worse than the
> gap. Flipping it to `true` is a one-word change in the payload.

### The nine status checks

They are the names CI actually _reports_, not the job ids in `ci.yml`. The test
job is a 3 × 2 matrix, so it emits six separate checks; naming a single literal
`test` context would leave five of them ungated while the branch looked
protected.

```
fmt   clippy   supply chain
test (ubuntu-latest,  all-features)          test (ubuntu-latest,  no-default-features)
test (macos-latest,  all-features)          test (macos-latest,  no-default-features)
test (windows-latest, all-features)         test (windows-latest, no-default-features)
```

Renaming a CI job means updating this file in the same commit, or `main` becomes
unmergeable on a context that will never report.

## Project status

The crates are at **0.2.0** and not yet on crates.io. All four version
together, so `protocol-droid = "0.2"` pulls matching `protocol-droid-schema`,
`protocol-droid-macros` and `protocol-droid-cli`.

**Conformance is green on every revision the pinned suite covers** — 3/3 on
`2025-03-26`, 55/55 on `2025-06-18`, 81/81 on `2025-11-25` and 151/151 on
`2026-07-28`, including the scenarios the suite does not yet score. The
`2026-07-28` stateless lifecycle is served in full over HTTP: per-request
`_meta`, `server/discover`, `subscriptions/listen`, SEP-2322 multi round-trip
requests, and the request-metadata headers. The measurements, what closed them,
and the known limitations are in
[`docs/CONFORMANCE-GAPS.md`](docs/CONFORMANCE-GAPS.md).

Preflight — `cargo build`, `cargo test --workspace --all-features`, `cargo l`
and `cargo deny --all-features check` — is the gate on every commit.

## Releasing

The chain is:

```
CI (on main, green) → auto-tag.yml → tag → release.yml → crates.io
```

`auto-tag.yml` tags `v<workspace version>` when CI is green on a push to
`main` and that tag does not exist yet. `release.yml` runs on Auto Tag's
completion and resolves the tag from the commit; a green push that did not
create a tag is reported as "nothing to release" and every downstream job is
skipped, so bumping `[workspace.package] version` is what makes a release.

The upload sits behind the `release` GitHub environment, which must carry
required reviewers. An environment with no protection rules is a label, not a
gate — check it before trusting a green run:

```bash
gh api repos/greysquirr3l/protocol-droid/environments/release --jq '.protection_rules'
```

A tag is created with `GITHUB_TOKEN`, which deliberately does **not** trigger
other workflows' `on: push` filters — which is why `release.yml` listens for
`workflow_run` rather than `push: tags`. A manual `workflow_dispatch` naming a
tag re-runs a release the trigger missed.

## The docs site

The `site/` directory is an [Astro](https://astro.build/) + [Starlight](https://starlight.astro.build/)
project. Develop with `pnpm dev`; build with `pnpm build`; deploy with
`pnpm run deploy` (the deploy script lives in `site/scripts/deploy-docs.sh`
and is described in [`site/README.md`](site/README.md)). The deploy script
verifies the live URL after pushing, and prints a fix-it line if the source
repo's Pages config is shadowing the user-pages URL — the trap that bit
the first deploy.

## Licence

MIT OR Apache-2.0.
