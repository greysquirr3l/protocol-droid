# protocol-droid

<p align="center">
  <img src="https://raw.githubusercontent.com/greysquirr3l/protocol-droid/main/assets/img/protocol-droid.svg" alt="protocol-droid" width="180">
</p>

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
   `inspect`, `record`, `replay`, and `conformance` (see [Status](#status)).

## Status

Twenty-six of twenty-nine tasks are complete, with three in flight.
Preflight is green at **2,343 tests** across the workspace
(`cargo test --workspace --all-features`).

| Done        | What it built                                                                                           |
| ----------- | ------------------------------------------------------------------------------------------------------- |
| T01–T03     | Workspace, vendored schemas at pinned commits, per-revision wire types generated at build time          |
| T04–T06     | `ProtocolVersion`, the 37-feature catalogue, `FeatureProfile` resolution, negotiation as pure functions |
| T07–T08     | The canonical IR, and six lift/lower codecs with `DegradePolicy` and `TranslationReport`                |
| T09–T12     | Single-pass JSON-RPC router, session engine, lock-free dynamic registry, `#[server]`/`#[tool]` macros   |
| T13–T15     | stdio, Streamable HTTP, stateless HTTP, and legacy HTTP+SSE — server **and** client halves              |
| T16         | OAuth 2.1: resource server, PKCE client, RFC 7591 + CIMD registration, RFC 7662 introspection, JWT/JWKS |
| T17         | HTTP hardening: `Origin`/DNS-rebinding, rate limiting, request deadline, concurrency ceiling            |
| T18, T21, T22, T24, T25 | The version-translating `bridge` serving loop; the official conformance suite (`conformance self/server/client`); differential interop vs `rmcp` and the TypeScript SDK across four revisions; security audit (no exploitable findings, `SECURITY-REVIEW.md` committed); wiring stub audit |
| T19         | `probe` and `inspect` — the operator-side fingerprinter and the typed JSON-RPC inspector              |
| T20         | `record` and `replay` — capture a session's frames to a `.jsonl` and replay them with version downgrade |
| T27         | A2A v1.0 — full server + client + `a2a_*_wire` end-to-end tests against a vanilla-Node reference         |
| T28         | `bridge_a2a` — the MCP ↔ A2A cross-protocol bridge, sharing the same `bridge` loop the MCP bridge uses  |
| T29         | ACP v1.25.0 — typed `AcpStdioPeer` plus a vanilla-Node reference agent for interop coverage            |

**In progress.** T23 (perf + fuzz baselines committed; the
comparative bench against `rmcp` is the one open piece), T26
(docs site **live** at [greysquirr3l.github.io/protocol-droid](https://greysquirr3l.github.io/protocol-droid/);
the automated crates.io publish stays deliberately disabled so
a human reviews the push), and the conformance work — the
suite runs end-to-end now, but the per-scenario pass rate is
below the MCP target on three revisions (`2025-06-18`,
`2025-11-25`, `2026-07-28`), and closing the gaps is the last
gate before MVP sign-off. The per-scenario breakdown is in
the `Conformance` workflow logs.

**Not started.** Nothing the strategy document named. Anything that
lands after MVP sign-off (post-MVP A2A work, additional cross-protocol
bridges, third-party protocol modules) is a separate workstream, not
a deferred Phase 1 task.

The three ideas above describe the finished design. Where the code does not yet
match the design, the architecture document says so rather than rounding up.

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

## Releasing

> **Automatic release is currently disabled.** `release.yml` has no automatic
> trigger on purpose. The `publish` job uploads four crates to crates.io, and the
> review gate meant to sit in front of it does not exist: the `release`
> environment is created but carries no protection rules, because GitHub does
> not offer required reviewers for private repositories on the free plan. The
> automatic trigger was removed rather than left armed behind a gate that does
> not bite.
>
> To re-enable, see the comment at the top of
> [release.yml](.github/workflows/release.yml) — it takes two changes, and the
> environment rules are the one that is not code.

The chain, once re-enabled, is:

```
CI (on main, green) → auto-tag.yml → tag → release.yml → crates.io
```

A tag is created with `GITHUB_TOKEN`, which deliberately does **not** trigger
other workflows' `on: push` filters — so `release.yml` is triggered by
`workflow_run`, not by `push: tags`, and resolves the tag from the run's
`head_sha`.

There is also nothing to do if you are on a fork with no registry token: the
publish job warns and exits 0 instead of failing the chain.

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
