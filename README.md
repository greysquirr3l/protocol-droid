# protocol-droid

A Model Context Protocol toolkit that speaks every published spec revision —
2024-11-05, 2025-03-26, 2025-06-18, 2025-11-25 and 2026-07-28, plus `draft` — in
a single binary, all at once. It does not ask you to pick a version at compile
time.

This repository is at scaffold stage (task T01). The workspace, toolchain pin,
lint profile, CI and supply-chain policy below are real and enforced; the crates
themselves carry no protocol logic yet.

## Layout

```
Cargo.toml                      virtual workspace: members, shared metadata, lint profile
rust-toolchain.toml             the exact toolchain everything resolves to
.cargo/config.toml              the `cargo l` alias and `build.warnings = "deny"`
.cargo/audit.toml               cargo-audit config; deny.toml is authoritative
deny.toml                       supply-chain policy: advisories, licences, bans, sources
crates/
  protocol-droid-schema/        per-revision MCP wire types, generated from schema.json
  protocol-droid/               the library: canonical IR, codecs, session engine, transports
  protocol-droid-macros/        proc-macro crate (must be separate from the library)
  protocol-droid-cli/           the `protocol-droid` binary
```

Each of the three non-proc-macro crates uses the collapsed hexagonal layout:
`src/{domain,application,adapters/{primary,secondary}}`. There is deliberately
no `ports` module — a port trait is owned by the handler that consumes it and
lives beside that handler.

## Prerequisites

- `rustup`, with the pinned toolchain. The first build installs it automatically
  from `rust-toolchain.toml`; `rustc --version` must report `1.98.1`.
- `cargo-deny`, for the supply-chain check: `cargo install cargo-deny --locked`.

Nothing else. There is no code generation step, and no vendored specification
yet — that arrives with T02.

## Build, test, lint

```bash
cargo build --workspace --all-features   # plain `cargo build` also fails on warnings
cargo test  --workspace --all-features
cargo l                                   # the full clippy profile, no arguments needed
cargo deny --all-features check
```

`cargo l` is an alias defined in `.cargo/config.toml`. It carries the whole
profile — `clippy::all`, `pedantic`, `nursery`, `cargo`, `perf`, with
`unwrap_used`, `expect_used`, `panic`, `indexing_slicing`, `cast_ptr_alignment`
and `suspicious` denied outright. The same profile is mirrored under
`[workspace.lints]` so a bare `cargo clippy` is equally strict; change both when
a lint moves.

`[build] warnings = "deny"` makes a plain `cargo build` fail on a warning too, so
the strict job is not the only thing standing between a warning and `main`. It
applies to workspace members only — crates.io dependencies still build with
their lints capped, so an upstream warning cannot break our build.

### Feature extremes

```bash
cargo test --workspace --all-features
cargo test --workspace --no-default-features
```

Both are in CI. Every schema revision is always compiled; revisions are data, not
optional features. What the feature flags gate is the heavy optional
integration — each transport, OAuth, legacy SSE, and the CLI subcommand groups.
Those gates are introduced by the tasks that implement them (T13 to T16, T18 to
T21), not declared empty in advance.

## CI

`.github/workflows/ci.yml` runs on every push to `main` and every pull request:

| Job    | Runs on       | What it does                                     |
| ------ | ------------- | ------------------------------------------------ |
| `fmt`  | ubuntu        | `cargo fmt --all --check`                        |
| `lint` | ubuntu        | lockfile freshness, then `cargo l`               |
| `test` | 3 OS x 2 sets | `--all-features` and `--no-default-features`     |
| `deny` | ubuntu        | `cargo deny` advisories, licences, bans, sources |

The `test` matrix is ubuntu-latest, macos-latest and windows-latest crossed with
both feature extremes. CodeQL runs in its own workflow, and Dependabot tracks
both Cargo and GitHub Actions.

Every workflow declares an explicit `permissions:` block, narrowed again per job
where a job needs more than `contents: read`. No workflow uses
`pull_request_target`, which would run untrusted code with a write-scoped token.

## Release

The release chain is deliberately two workflows rather than one:

1. `auto-tag.yml` triggers on `workflow_run` for a **successful** CI run on
   `main`. It reads the workspace version out of `cargo metadata` and creates
   `v<version>`, annotated and pushed with `GITHUB_TOKEN`. If the tag already
   exists it does nothing, so re-running is a no-op.
2. `release.yml` triggers on `workflow_run` for `Auto Tag`. Its `resolve-tag`
   job derives the tag from the commit SHA, then `verify` dry-runs
   `cargo publish` for every crate in dependency order, and `publish` performs
   the real upload.

`workflow_run` is required rather than `on: push: tags`, because a tag pushed
with `GITHUB_TOKEN` deliberately does not trigger other workflows. The
`resolve-tag` job therefore looks the tag up from the SHA that the triggering
run reported, which works no matter what caused the run.

`publish` is skipped, with a warning, when `CARGO_REGISTRY_TOKEN` is not set, so
a fork or an unconfigured repository does not fail the chain. Publishing
requires a version bump commit: the tag comes from the committed workspace
version, so an already-published version is rejected by crates.io rather than
silently skipped.

## Conventions worth knowing before you change something

- Edition 2024, resolver 3, `rust-version = "1.98"`. The MSRV floor is only
  honoured by MSRV-aware resolution because it is actually set in
  `[workspace.package]` and inherited by every member.
- `Cargo.lock` is committed, because the workspace contains a binary. CI checks
  it is current.
- Ordered enums derive **both** `Ord` and `PartialOrd` from the same variant
  order, and carry a test proving the two agree. Since 1.98, `derive(PartialOrd)`
  has a fast path that assumes exactly that, and a mismatched pair returns wrong
  comparisons instead of failing to compile.
- No `lazy_static`; `LazyLock` or `LazyCell` only. No `#[allow(clippy::…)]`; use
  `#[expect(…, reason = "…")]`, which clears itself when it stops being needed.
