# Release process

A release is a tag, a verified dry-run, a publish to
crates.io, and a binary matrix. The chain is automated;
the human gate is the `release` GitHub Environment.

## The chain

```text
push to main
  → CI (`.github/workflows/ci.yml`)
  → Auto Tag (`.github/workflows/auto-tag.yml`)
  → Release (`.github/workflows/release.yml`)
    → resolve-tag
    → verify packaging
    → publish (gated on the `release` environment)
    → binaries (matrix on three OSes)
```

A green `CI` run on `main` triggers `Auto Tag`, which reads
the workspace's `version` from `Cargo.toml` and creates a
tag of the form `v<version>`. A second green CI run on
the tag (or a manual dispatch) triggers `Release`.

## The `release` environment

The `publish` job is gated on a GitHub Environment named
`release`. The environment does not exist by default; it
must be created in the repository's settings with required
reviewers enabled. Until that is done, the `publish` job
runs unattended, which is the failure mode the gate
prevents.

## What `verify packaging` does

`verify packaging` runs `cargo publish --dry-run` for
each crate in dependency order:

```text
protocol-droid-schema → protocol-droid → protocol-droid-macros → protocol-droid-cli
```

A failed dry-run is a failed release: a manifest typo, a
missing path dep, a `rust-version` that is not the
intended floor all surface here, before anything is
uploaded.

## The binaries matrix

The `binaries` job builds the CLI for six targets:

| OS | Target | Label |
| --- | --- | --- |
| Linux | `x86_64-unknown-linux-gnu` | `linux-x86_64` |
| Linux | `x86_64-unknown-linux-musl` | `linux-x86_64-musl` |
| Linux | `aarch64-unknown-linux-musl` | `linux-aarch64-musl` |
| macOS | `x86_64-apple-darwin` | `macos-x86_64` |
| macOS | `aarch64-apple-darwin` | `macos-aarch64` |
| Windows | `x86_64-pc-windows-msvc` | `windows-x86_64` |

The matrix mirrors the CI test matrix, so a target CI did
not exercise is not released. A maintainer-edited release
body and the per-OS asset list are what the GitHub
Release UI shows; the `binaries` job uploads the assets
as `draft: true` so a maintainer reviews the list before
the release is published.
