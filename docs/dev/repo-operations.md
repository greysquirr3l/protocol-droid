# Repository operations

How to maintain this repository: what protects `main`, how a release gets made
and published, and how the documentation site is deployed.

None of this belongs in the [README](../../README.md), which is for people
_consuming_ the crates. It lives here because it is about running the
repository, not using the library, and it was the larger half of a README that
had grown into a maintainer's manual.

Contents:

- [Repository operations](#repository-operations)
  - [Branch protection](#branch-protection)
    - [The nine status checks](#the-nine-status-checks)
  - [Releasing](#releasing)
  - [The docs site](#the-docs-site)

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

## The docs site

The `site/` directory is an [Astro](https://astro.build/) + [Starlight](https://starlight.astro.build/)
project. Develop with `pnpm dev`; build with `pnpm build`; deploy with
`pnpm run deploy` (the deploy script lives in `site/scripts/deploy-docs.sh`
and is described in [`site/README.md`](../../site/README.md)). The deploy script
verifies the live URL after pushing, and prints a fix-it line if the source
repo's Pages config is shadowing the user-pages URL — the trap that bit
the first deploy.
