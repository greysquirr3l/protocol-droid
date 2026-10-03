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

Phases 1–3 are in progress. The specification schemas for every revision are
vendored and provenance-verified, and the per-revision wire types are generated
from them at build time — `protocol_droid_schema::wire::v2024_11_05` through
`wire::draft` all compile together, with no version selection at build time.
The feature matrix, version codecs, and transports follow.

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

| Crate                          | What it is                                                                      |
| ------------------------------ | ------------------------------------------------------------------------------- |
| `crates/protocol-droid-schema` | Vendored official schemas, the build-time generator, and the wire types it emits |
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

## Licence

MIT OR Apache-2.0.
