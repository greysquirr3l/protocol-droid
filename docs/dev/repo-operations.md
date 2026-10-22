# Repository operations

How to maintain this repository: what protects `main`, how a release gets made
and published, and how the documentation site is deployed.

None of this belongs in the [README](../../README.md), which is for people
_consuming_ the crates. It lives here because it is about running the
repository, not using the library, and it was the larger half of a README that
had grown into a maintainer's manual.

Contents:

- [Repository operations](#repository-operations)
  - [Git hooks](#git-hooks)
  - [Branch protection](#branch-protection)
    - [The nine status checks](#the-nine-status-checks)
  - [Releasing](#releasing)
  - [The docs site](#the-docs-site)

## Git hooks

Install them once per clone:

```bash
./scripts/install-hooks.sh
```

The hooks are tracked in [`scripts/`](../../scripts), so every machine runs the
same checks. The installer copies them into `.git/hooks/` and leaves any hook
that is not in that directory alone.

### pre-push

Runs the two CI checks that are cheap enough to run on every push:

- `cargo fmt --all --check` — the same command the [`fmt`](#the-nine-status-checks) job runs.
- `cargo audit` — over every committed `Cargo.lock`.

It takes about four seconds. The rest of CI takes minutes, and a hook that
takes ten minutes is a hook people disable.

Two decisions worth knowing about:

**Waivers come from [`deny.toml`](../../deny.toml).** `cargo audit` has no
config file, so the hook reads the `id` fields out of `[advisories].ignore` in
`deny.toml` and passes them as `--ignore`. There is one list, not two. If you
add a waiver to `deny.toml`, the hook honours it immediately; if you only added
it to a copy somewhere, CI and your pre-push would disagree.

**It audits all four lockfiles, not just the workspace one.** The interop and
fuzz crates are excluded from the workspace, so the `supply chain` job's
`cargo deny` never sees their dependency graphs. A committed `Cargo.lock` is a
dependency graph someone will build, so it gets the same scan.

A failed advisory scan is a hard block, including the case where the scan could
not run at all. `cargo audit` exits 1 for a vulnerability *and* for a failure to
fetch the advisory database, and the hook tells the two apart by whether the
report is empty on stdout — a finding prints its report, a fetch failure prints
nothing. A scan that did not run is not a scan that passed.

If `cargo-audit` is not installed the hook says so and skips that half rather
than failing; CI still catches it. Install it with
`cargo install cargo-audit --locked`.

`--no-verify` bypasses the hook. That is a guard against a habit, not a
security boundary — CI runs the same checks and does not honour it.

### Why pre-push and not pre-commit

Pre-commit runs before the commit exists, so a `cargo fmt` that rewrites files
cannot run there without silently amending what the author staged. Pre-push
runs after the commit exists, so the check reports rather than rewrites, and
fixing it is an explicit `cargo fmt --all && git commit --amend`.

## Branch protection

`main` is protected by [`.github/branch-protection.json`](../../.github/branch-protection.json),
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

The chain is:

```
CI (on main, green) → auto-tag.yml → tag → release.yml → crates.io
```

### What each run state means

Most green pushes to `main` are **not** releases, so the states below come up
often. None of the first three is a failure.

| What you see | What it means | What to do |
| --- | --- | --- |
| Auto Tag: `tag v0.4.2 already exists; nothing to do` | The workspace version is `0.4.2` and `v0.4.2` is already tagged on an earlier commit. Correct — this push is not a release. | Nothing. To release, bump the version. |
| Release: `No tag points at <sha>; nothing to release` | The consequence of the above, one workflow later. Correct, and the run is green. | Nothing. |
| Release: `Resolved release tag: v0.4.2` | A tag was found and the release is starting. | Wait, then approve the gate. |
| Release: `publish` sitting in **waiting** | Binaries and packaging verification are done. This is the approval gate, and it is the only place a human is required. | **Approve and deploy** on the run page. |
| crates.io shows the version, but no GitHub Release yet | `release.yml` creates the Release after the upload. A published crate is the real signal. | Nothing. |

**The only trigger is a version bump.** Auto Tag creates `v<workspace version>`
only when that tag does not already exist, so re-tagging cannot produce a
release, and re-running `release.yml` cannot either — a tag already on an
earlier commit means the version has to change.

```bash
# the one edit that starts a release
grep -n '^version' Cargo.toml
```

Then: bump → open a PR → merge → CI green → Auto Tag tags → Release runs →
approve once.

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

Approve it from the run's page — **Approve and deploy**. Do not expect `gh` to
do it. A default `gh auth login` yields an OAuth App token (`gho_`), and every
deployment-review route answers `404 Not Found` with one, on both accounts,
including as a repository admin. That is not a permissions problem and no scope
fixes it:

```console
$ gh auth refresh -h github.com -s deployments
The scopes requested are invalid: deployments. (invalid_scope)
```

`gh auth refresh` can only add scopes the gh OAuth App is registered for, and
`deployments` is not one of them. Two accounts, admin and not, all four
approval routes (`POST`/`GET /deployments/{id}/reviews`,
`GET`/`POST /actions/runs/{id}/pending_deployment_reviews`) — every one 404. An
earlier version of this note claimed `repo` was insufficient and pointed at
`gh auth refresh -s deployments`. Both halves of that were wrong: the scopes
are not the lever, and that command does not exist.

Driving the approval from a script would mean authenticating with a token that
has Deployments write access — a classic PAT (`gh auth login --with-token`) or
a fine-grained PAT with `Deployments: write` (`GH_TOKEN`). That is untested
here; the UI path is one click and it works.

A tag is created with `GITHUB_TOKEN`, which deliberately does **not** trigger
other workflows' `on: push` filters — which is why `release.yml` listens for
`workflow_run` rather than `push: tags`. A manual `workflow_dispatch` naming a
tag re-runs a release the trigger missed.

### When CI is red for no code reason

Check the annotation before reading any logs:

> The job was not started because recent account payments have failed or your
> spending limit needs to be increased

That is an Actions **billing** refusal. Every job dies, the run reports failure,
and nothing was ever compiled — so it reads as a wall of breakage when it is
one billing page. Six PRs sat red this way for eight days. The fix is the
billing page, not the code; a closed-and-reopened PR re-runs it.

## The docs site

The `site/` directory is an [Astro](https://astro.build/) + [Starlight](https://starlight.astro.build/)
project. Develop with `pnpm dev`; build with `pnpm build`; deploy with
`pnpm run deploy` (the deploy script lives in `site/scripts/deploy-docs.sh`
and is described in [`site/README.md`](../../site/README.md)). The deploy script
verifies the live URL after pushing, and prints a fix-it line if the source
repo's Pages config is shadowing the user-pages URL — the trap that bit
the first deploy.
