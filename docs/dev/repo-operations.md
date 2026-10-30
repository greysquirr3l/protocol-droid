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
  - [Security settings](#security-settings)
    - [A dependency advisory is not the whole story](#a-dependency-advisory-is-not-the-whole-story)
    - [The one advisory that stays open](#the-one-advisory-that-stays-open)
  - [Branch protection](#branch-protection)
    - [The ten status checks](#the-ten-status-checks)
    - [The OSSF Scorecard posture](#the-ossf-scorecard-posture)
    - [The two-account review pattern](#the-two-account-review-pattern)
  - [Releasing](#releasing)
  - [The docs site](#the-docs-site)
    - [CI builds this site](#ci-builds-this-site)
    - [Dependency updates](#dependency-updates)
      - [The fixture entry exists because npm install will not fix it](#the-fixture-entry-exists-because-npm-install-will-not-fix-it)

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

- `cargo fmt --all --check` — the same command the [`fmt`](#the-ten-status-checks) job runs.
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
fuzz crates are excluded from the workspace, so the root `cargo deny` never
sees their dependency graphs. A committed `Cargo.lock` is a dependency graph
someone will build, so it gets the same scan.

**It denies `unsound` and `yanked`, not just plain vulnerabilities.**
`cargo audit` files an `unsound` advisory under `warnings` rather than
`vulnerabilities`, and exits zero for it by default — so a hook that read
only `vulnerabilities.count` reported a lockfile carrying a use-after-free
advisory as clean. The hook passes `-D unsound -D yanked` and reads all
three categories out of the report, which is the same set `deny.toml`
denies. A hook that passes where CI fails is worse than no hook: it hands
out a clean bill of health on a graph CI would reject. `unmaintained` is
deliberately *not* denied, because `deny.toml` sets
`unmaintained = "workspace"`.

A yanked crate is reported separately from an advisory, because the two
have different remedies. An advisory can be waived in `deny.toml`; a
yanked crate has no advisory id to ignore, so the only fix is to update
the lockfile.

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

## Security settings

These live in GitHub's database, not in the tree. There is no apply script for
them the way there is for branch protection, so nothing reverts them — but
nothing records them either, which means the next person inherits four
settings they did not set and cannot account for. This section is that record.

| Setting | State | What it does |
| ------- | ----- | ------------ |
| Dependabot vulnerability alerts | on | Raises an alert when a published advisory hits a dependency already in the tree |
| Dependabot security updates | on | Opens the fix PR itself, grouped and prefixed `chore(deps)` |
| Secret scanning | on | Scans the history for credentials, public repo so free |
| Push protection | on | **Blocks a push** containing a recognised secret |

Turn one on with `PATCH /repos/{owner}/{repo}` — the documented
`PUT /secret-scanning/secret-scanning` and `PUT /secret-scanning/push-protection`
endpoints return 404 on this account even though the features work:

```bash
gh api --method PATCH repos/greysquirr3l/protocol-droid \
  -f 'security_and_analysis[secret_scanning_push_protection][status]=enabled' \
  --jq .security_and_analysis.secret_scanning_push_protection.status
```

Two notes on the cost, since both turned out to be non-issues and both are
worth checking rather than assuming:

**These do not spend AI credits.** Dependabot resolves versions with the
ecosystem's own solver; no model is involved. The one part of the pipeline that
would use credits is Copilot code review on the resulting PRs, and it is not
enabled on this repository.

**Actions minutes are free here.** The repo is public, so the ~20 CI jobs that
run on each fix PR cost nothing. On a private repo this would be the real
expense line, and worth re-checking before enabling anything else.

Still off by choice: `secret_scanning_validity_checks` (only matters for
fine-grained PATs and SSH keys) and `secret_scanning_non_provider_patterns`
(deprecated; it stays disabled on its own and is not worth forcing).

### A dependency advisory is not the whole story

These settings read manifests. CI's `supply chain` job reads the root
`Cargo.lock`. Neither sees the three lockfiles outside the workspace:

- `crates/protocol-droid/fuzz/Cargo.lock`
- `crates/protocol-droid/tests/interop_rmcp/Cargo.lock`
- `crates/protocol-droid/tests/interop_ts_sdk/Cargo.lock`

The pre-push hook covers them locally, which is better than nothing but is not
a substitute for CI. Same for `site/pnpm-lock.yaml`, which no security setting
covered until Dependabot was pointed at it — see
[The docs site](#the-docs-site).

### The one advisory that stays open

`GHSA-g7r4-m6w7-qqqr` — esbuild, low severity, arbitrary file read through
the dev server on Windows. It is the only advisory left open on this
repository.

Dependabot's own report says the update is `security_update_not_possible`:
latest resolvable `0.27.7`, lowest non-vulnerable `0.28.1`, and no version
satisfies both. The lockfile resolves `esbuild@0.27.7`.

**That is a resolution result, not a proof that no fix exists.** `0.28.1` is
published, and the peer range that reaches it is wide enough — vite 8.3.5
declares `esbuild: ^0.27.0 || ^0.28.0`. The narrower constraint is the
`(esbuild@0.27.7)` peer suffix astro 7.4.2 carries in the lockfile, which
pnpm resolved from the version present when the lockfile was written. A
`pnpm.overrides` entry forcing `esbuild: 0.28.1` is the untried path, and it
is the thing to reach for before deciding this one is acceptable.

The advisory is genuinely low-impact for this repository. It is in
`site/pnpm-lock.yaml`, and esbuild is a build-time dependency of the Astro
dev server, not of the static output. The published site is static HTML
built by `astro build`; the advisory describes the `astro dev` server, which
never runs in CI or in the deploy to `greysquirr3l.github.io`. It would
matter to a contributor running `pnpm dev` on Windows.

Left open on purpose, and re-check when astro next moves — a new astro major
may resolve esbuild to a patched version on its own.

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
| Status checks           | 10, strict  | See below                                 |
| **`enforce_admins`**    | **`true`**  | **Deviation — see below**                 |

> **Deviation: an admin-only lockout.** `enforce_admins` is `true`, which closes
> the baseline finding that an admin could push straight to `main`. The cost is
> that the rules now bind everyone, including the one person who can otherwise
> change them: if the sole admin loses the ability to approve their own PRs,
> the repository is stuck. The review requirement stays at 1 and the approver
> is the repo owner, so a normal release cycle is unaffected — it takes a
> repository with no second approver *and* an admin needing to merge their own
> work to reach that state.
>
> **Applying this needs the `admin` permission, not just `push`.** A
> collaborator with push access gets a 404 from `apply-branch-protection.sh`
> rather than a 403, because the API refuses to reveal branch protection to a
> non-admin. If the script appears to do nothing, check identity with
> `gh api user --jq .login` before assuming it succeeded.

### The ten status checks

They are the names CI actually _reports_, not the job ids in `ci.yml`. The test
job is a 3 × 2 matrix, so it emits six separate checks; naming a single literal
`test` context would leave five of them ungated while the branch looked
protected.

```
fmt   clippy   supply chain   site
test (ubuntu-latest,  all-features)          test (ubuntu-latest,  no-default-features)
test (macos-latest,  all-features)          test (macos-latest,  no-default-features)
test (windows-latest, all-features)         test (windows-latest, no-default-features)
```

Renaming a CI job means updating this file in the same commit, or `main` becomes
unmergeable on a context that will never report.

`site` comes from a separate workflow, `site.yml`, not from `ci.yml`. It is
listed here because branch protection does not care which workflow reports a
context — only the name.

### The OSSF Scorecard posture

The [OpenSSF Scorecard](https://scorecard.dev/viewer/?uri=github.com/greysquirr3l/protocol-droid)
is the closest thing to an outside audit this repository has, and the score it
reports is mostly a consequence of the settings above rather than of anything
in the tree. There is no `scorecard.yml` workflow in `.github/workflows/`, so
nothing here is currently *reported* by the service — the scores below are
what the service would compute if the workflow were added, checked by hand
against the API on 2026-10-30.

| Check | State | Notes |
| ----- | ----- | ----- |
| Branch-Protection | pass | 10 required contexts, strict, `enforce_admins` on, 1 approving review, conversation resolution required, no force-push |
| CI-Tests | pass | `ci.yml` runs the full matrix on every PR, plus conformance, interop and CodeQL |
| Code-Review | **partial** | Every merged PR carries an approving review, but none carries an **inline** review comment — and the Scorecard's `Code-Review` check looks for the latter. See below |
| Signed-Releases | **partial** | Every tag is annotated, but none is GPG- or Sigstore-signed. See below |
| Token-Permissions | pass | Every workflow declares a narrow `permissions:` block |
| Dependency-Update-Tool | pass | Dependabot is on and opened five update PRs; their content was landed on `release/0.6.1` and the PRs were closed rather than merged, to avoid duplicating the commits |
| Security-Policy | pass | `SECURITY.md` exists and names the supported versions |
| Pinned-Dependencies | **partial** | Rust dependencies are pinned by `Cargo.lock`, but the Actions are pinned to a **tag**, not a SHA. See below |
| SAST | pass | CodeQL runs on every push |
| Fuzzing | pass | `cargo-fuzz` targets are in the tree and fuzz is a CI job |
| Packaging | pass | Published to crates.io with `cargo publish` |
| Maintained | **partial** | One maintainer, though the commit history is active: 420 commits in the last 90 days |
| Contributors | partial | One human contributor, under three email identities, plus `dependabot[bot]` |

Four of those are worth being honest about rather than leaving to a badge.

**The reviews are approvals, not review comments.** Every merged pull request
carries an approving review from the other account, and the branch protection
requires one — but none of them carries an **inline** comment on the diff.
The Scorecard's `Code-Review` check looks for review comments specifically,
because an approval with no comment says that somebody looked, not what they
thought. The `Pull-Request-Review` check passes on the same evidence. This
is a property of the workflow rather than a policy: the reviews are written
to be substantive in the body, and the Scorecard reads the other place.

**The tags are not signed.** `auto-tag.yml` creates an annotated tag and
`release.yml` creates the GitHub Release, but neither signs them — the tag
object carries a `tagger` and a message and no signature block. The
`Signed-Releases` check looks for a signature, so it scores partial or worse
depending on how many releases it samples. Fixing it means giving the
release workflow a signing key, which is a credential in a GitHub secret and
therefore a decision with its own risk: a key that can sign releases is a key
that can be used to impersonate one. It is not a change to make casually.

**The Actions are pinned to a tag, not to a commit SHA.** Every workflow
uses `actions/checkout@v7`, `actions/setup-node@v7`,
`actions/upload-artifact@v7`, `softprops/action-gh-release@v3` and
`dtolnay/rust-toolchain@master`. A tag is a mutable ref: whoever owns the
action can move it, and a workflow that resolves `v7` today can resolve
different code tomorrow without any change in this repository. The
Scorecard's `Pinned-Dependencies` check wants a full commit SHA for exactly
this reason. The fix is mechanical — replace each tag with the SHA it
currently points at and add the version as a trailing comment — but it means
re-pinning on every action bump, and the bump becomes a commit that has to
be made by hand rather than by Dependabot. It is a real trade, not an
oversight.

**`Maintained` will decay on its own.** The scorecard looks at commit
frequency over a trailing window, and it penalises a repository that has not
been touched recently. That is not something to fix in the repository; it is
something to fix by continuing to work on it.

Nothing here requires a workflow to be added to see — but adding
`ossf/scorecard-action` would turn these from a claim into a measurement,
and would surface the four partial rows on the pull request rather than in a
document that nobody re-reads.

### The two-account review pattern

`main` requires one approving review and `enforce_admins` is on, so **a PR
cannot be approved by its own author**. This repository has two GitHub
accounts — `greysquirr3l` and `sosh-ncampbell` — and the pattern is that
each approves the other's PRs. PR #78 was authored by `greysquirr3l` and
approved by `sosh-ncampbell`; PR #79 was the other way round.

Switch accounts with `gh auth switch --user <login>`, and switch back
afterwards — the active account is what determines the PR's author and who
can approve it.

There is a second, separate gate: the `Entire Gates` check, which reads
Entire's own trail approvals rather than GitHub's reviews. It is **not** one
of the ten required contexts, so it does not block a merge, and it does not
re-run when the approval lands — it is a snapshot, and it will keep reporting
"0 of 1 required approvals" against a trail that has one. Clear it with:

```bash
entire trail approve <trail> --project gh/<owner> --branch <branch>
```

### The two-account review pattern

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

**A version bump also invalidates the three lockfiles outside the
workspace.** `fuzz`, `interop_rmcp` and `interop_ts_sdk` are each their own
workspace, and each pins `protocol-droid` by version. Bump the workspace and
those lockfiles no longer match, which the `supply chain` job reports
because its scan passes `--locked`. The fix is to refresh them:

```bash
# the root lockfile
cargo update -p protocol-droid

# and the three outside the workspace
for crate in crates/protocol-droid/fuzz \
             crates/protocol-droid/tests/interop_rmcp \
             crates/protocol-droid/tests/interop_ts_sdk; do
  cargo update --manifest-path "$crate/Cargo.toml" -p protocol-droid
done
```

Leaving them stale is worse than an inconvenience: a stale fixture lockfile
is a dependency graph nobody scans, which is how the fixture kept
`ctutils 0.4.3` (RUSTSEC-2026-0385) while the root lockfile had already
moved to 0.4.5. The `supply chain` job failing on `--locked` is the
mechanism that keeps the four lockfiles honest.

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

### CI builds this site

`site.yml` builds the site on every PR and on every push to `main`. It runs
`pnpm install --frozen-lockfile`, `astro check` and `astro build`, then
asserts the output has the expected shape.

This was not always true, and the gap was real: Dependabot's astro 5 → 7 PR
passed fourteen green checks and did not build, because starlight 0.30 pins
`astro: ^5.0.0` and `pnpm install` does not enforce peer ranges. The only
reason it was caught before the docs site broke in production was a local
`pnpm build`.

Two things about how the job is put together are worth knowing before changing
it.

**The workflow has no `paths:` filter.** A path-filtered workflow does not run
at all when the filter does not match, so it reports no check run — and a
required status check that never reports blocks the merge forever. The filter
is applied to the build *steps* instead, so the job always runs and always
reports. A PR that touches no site file skips the build and goes green in
seconds; a PR that does touch `site/` builds it.

**The node version is pinned to the floor, not the latest.** `22.23.4` is what
astro 7 declares as its minimum (`engines.node >= 22.23.4`). Pinning the
newest release would prove the site builds on a runtime nobody is forced to
use. The floor is the one a contributor on an older LTS will have.

`pnpm` is pinned the same way, through the `packageManager` field in
`site/package.json`, and corepack reads it. Without that field corepack picks
a default, and the lockfile can end up written by one pnpm major and read by
another.

**Running `pnpm build` locally before merging is still worth doing.** It is
faster than waiting for CI, and it is the only way to see the site.

### Dependency updates

[`.github/dependabot.yml`](../../.github/dependabot.yml) has four entries:
`cargo` for the workspace, `github-actions`, `npm_and_yarn` for `/site`, and
`npm_and_yarn` for the interop fixture. The Node ones are declared under
GitHub's name for the ecosystem generally — there is no npm or Yarn in this
project — and it is that ecosystem which reads `site/pnpm-lock.yaml`.

Site bumps are grouped into a single PR, for the same reason action bumps are:
`@astrojs/starlight` is version-coupled to `astro`, so moving one without the
other produces a site that does not build. **Astro, starlight and `@astrojs/mdx`
have to move together.** A starlight major also breaks two config shapes —
autogenerated sidebar groups were removed in 0.39, and `social` became an
array in 0.33 — and both failures surface only at build time.

### The fixture entry exists because `npm install` will not fix it

`crates/protocol-droid/tests/interop_ts_sdk/fixtures/package.json` is a
checked-in manifest that `cargo test` spawns with `npm install`, pinning
`@modelcontextprotocol/sdk` at exactly one version so the interop test tests
that version.

It was never declared in `dependabot.yml`, and that was the problem:
**Dependabot's security updates reach an undeclared directory, but its ignore
rules do not.** So `GHSA-rxp7-6hxx-x8q5` arrived as PR #55 with nowhere to
say "not this one".

The obvious response — ignore it, because the SDK pin matters more — was
tried and was wrong. The SDK's own declared range for the affected
transitive dependency is `^10.2.0`, so the fix was always permitted. The
bump moves three lines of lockfile, leaves the SDK at `1.32.1`, and takes the
fixture's `npm audit` from one moderate to zero.

The real reason a lockfile needs watching is that `npm install` honours it.
Run `npm install --package-lock-only` against the fixture and the diff is
**empty**: the vulnerable version stays installed until somebody updates the
lockfile on purpose. A lockfile that only changes by hand is a lockfile that
silently keeps a known-vulnerable transitive version, so the entry is kept
for the schedule rather than for the ignore.

The general rule, and the one worth carrying: an ignore rule cannot be
applied to a directory that is not declared, and a security update will not
respect a directory that is. But an ignore is the wrong tool for a
transitive dependency whose fix is inside its declared range — that is a
bump, and the bump is cheaper than the argument.
