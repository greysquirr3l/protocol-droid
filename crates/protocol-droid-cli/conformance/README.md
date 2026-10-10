# Conformance suite pin

The official MCP conformance suite (`@modelcontextprotocol/conformance`) is
pinned to the version recorded in `PINNED_VERSION`. A deliberate upgrade is
required to move forward — the version is the input to `npx
@modelcontextprotocol/conformance@$(cat PINNED_VERSION) …`, so a bump here
is what a release notes entry explains.

## Why a pin rather than `latest`

`npx @modelcontextprotocol/conformance@latest` would run whatever the npm
registry serves at invocation time. A green CI run on Monday and a red run on
Tuesday, with no code change in this crate, is the failure mode a pin
prevents. The suite is at `0.2.x-alpha`, which is pre-stable — silently
tracking that channel in CI is a CI flake generator, not a feature.

## What the pinned version does

The suite ships scenarios under `src/scenarios/{client,server}/`. The
runner takes `--spec-version <v>` (one of `2024-11-05`, `2025-03-26`,
`2025-06-18`, `2025-11-25`, `2026-07-28`, `draft`) and decides
stateful-vs-stateless lifecycle from it. The fixture clients and servers this
crate ships are written against that wire.

## Known-suite-bugs allow-list

When a scenario fails because the suite is wrong (and we have a checked link
to the upstream issue), the failure is documented in
`ALLOWLIST.md` next to this file rather than suppressed silently. The CI
job reads that file and treats listed scenarios as expected-to-fail with a
typed reason, so a real regression still fails the run.

## Refresh protocol

1. Bump `PINNED_VERSION`.
2. Run `protocol-droid conformance self --spec-version <v>` for every
   revision the suite supports locally.
3. Move any genuinely-suite-bug failures from `ALLOWLIST.md` into
   resolved-as-fixed or update the allow-list entry.
4. Commit the bump, the new allow-list, and the regenerated fixtures
   (if scenarios changed) in one commit.

## Coverage

The matrix the CI workflow exercises is the cross product of
`PINNED_VERSION` × spec-revision. As of the `0.2.0-alpha.12` pin, the
suite supports these spec revisions for server scenarios:

- `2025-03-26`
- `2025-06-18`
- `2025-11-25`
- `2026-07-28` (and `draft` as an alias)

The library still ships full codec coverage for `2024-11-05` and earlier
revisions; what the conformance matrix does not exercise is the suite's
own `2024-11-05` scenario set, which the `0.2.0-alpha` series does not
ship. The `ALLOWLIST.md` entry named for `2024-11-05` is the explicit
record of that gap, so a future pin that re-introduces
`2024-11-05` scenarios is the trigger to remove the entry.
