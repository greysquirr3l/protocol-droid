# Security Policy

## Supported versions

`protocol-droid` is published as four crates (`protocol-droid-schema`,
`protocol-droid`, `protocol-droid-macros`, `protocol-droid-cli`) on
crates.io. Security advisories are issued against the latest released
minor version; the previous minor receives advisories for **30 days**
after a new minor ships. The `main` branch is built and tested on
every push; advisories against `main` only are evaluated case by case
because the work to backport a fix is small but a backport is not
guaranteed to be free of merge conflicts on the older minor.

| Crate                    | Latest stable | Previous minor |
| ------------------------ | ------------- | -------------- |
| `protocol-droid-schema`  | `0.6`         | `0.5`          |
| `protocol-droid`         | `0.6`         | `0.5`          |
| `protocol-droid-macros`  | `0.6`         | `0.5`          |
| `protocol-droid-cli`     | `0.6`         | `0.5`          |

All four share one version — every member inherits `version` from
`[workspace.package]`, so there is no series in which the crates
disagree. The 0.x line means the semver contract is not yet stable, not
that the versions are unmaintained.

Published on crates.io: 0.3.0, 0.4.0, 0.4.1, 0.4.3, 0.5.0, 0.5.1 and
newer. 0.3.x and everything before it are past the 30-day window that
follows a minor and are not maintained.

The MSRV (1.98.1) and the supported Rust toolchain are pinned in
[`rust-toolchain.toml`](https://github.com/greysquirr3l/protocol-droid/blob/main/rust-toolchain.toml)
and in `[workspace.package].rust-version`; advisories on the
compilation surface (not the wire surface) are evaluated against the
MSRV, not the latest stable.

## Reporting a vulnerability

**Use [GitHub's private vulnerability reporting](https://github.com/greysquirr3l/protocol-droid/security/advisories/new)**
for the initial report. That channel encrypts the message in transit,
restricts the audience to the maintainer, and gives a unique URL to
track the conversation.

Please include:

1. The version(s) of the four crates that are affected, pinned
   exactly (e.g. `protocol-droid = "0.1.3"`).
2. A reproducer — a server fixture, a wire capture, an upstream peer
   that triggers the issue, or a unit-test-shaped snippet. The
   reproducer does not have to be public; the test that lands as the
   fix is enough.
3. The impact, in the words of the threat model below. "I can read
   other users' files" is more useful than "path traversal"; "the
   `X-Remote-User` header is trusted without validation" is more
   useful than "missing auth check".
4. Whether the report is embargoed. Default is no embargo; if a CVE
   or coordinated disclosure matters, name the date and the
   recipients.

Reports that arrive out of band — by email, by issue, by DM — are
accepted but the response is slower. A report of an active exploit
should also carry a phone number in the first message; an
"actively exploited" report that is only on a tracker is read in
working-hours order, not in incident order.

## What is in scope

Anything in the four crates' source that affects a consumer's trust
boundary, the wire surface, or a peer of the library:

- **JSON-RPC envelope routing** — wrong id routing, wrong method
  dispatch, frame framing errors that let a peer smuggle extra
  bytes through a `&RawValue` parse.
- **Wire codec behaviour** — a `tools/call` argument that the
  application refused but the codec accepted, or a result the
  application trusted but the codec lost a field on. The codec
  never silently drops a field; if it appears to, that is a bug.
- **Capability gating** — a `FeatureProfile::is_enabled` answer
  that lets a path run when the negotiated capabilities do not
  cover it. A `list_changed` notification sent to a peer that
  did not advertise `list_changed` is always a bug.
- **Token handling** — redaction failures at the tracing layer
  (the test is `tests/secret_logging.rs`), bearer-token leaks
  in error paths, PKCE verifier leakage, OAuth state
  non-entropy.
- **HTTP transport hardening** — `Origin` allowlist bypass,
  DNS-rebinding via `Host`-header mismatch, request-deadline
  or concurrency-ceiling bypass, missing or wrong rate-limit
  knob.
- **Filesystem-backed resources** — path canonicalisation
  failures, a `..` traversal that escapes the allowed root,
  symlink handling that lets a resource read outside its
  declared scope.
- **Fuzz harness regressions** — the harness in
  `crates/protocol-droid/fuzz/` runs nightly; a found input
  is itself a reproducer and is in scope.
- **Build / supply chain** — a `cargo deny` finding that
  bypasses the CI gate, a vendored schema that drifted from
  the manifest, a transitive dependency that drops a security
  patch.

## How a vulnerability gets found

A report is the best route and not the only one. Three things run without
anyone looking:

| Where | What it checks | Coverage |
| --- | --- | --- |
| `supply chain` CI job | `cargo deny` against a freshly fetched advisory database | all four committed lockfiles |
| Dependabot alerts | advisory database against every manifest | root, `site/`, and the interop fixture |
| Secret scanning | credential patterns across the history and every push | whole repository |
| `pre-push` hook | `cargo audit` over every committed lockfile | all four, including the three outside the workspace |

Two gaps are worth stating plainly rather than leaving to be discovered:

**Every committed lockfile now gets a CI-side advisory watch.** Until the
`supply chain` job was extended, it scanned the root lockfile only.
`crates/protocol-droid/fuzz/`, `crates/protocol-droid/tests/interop_rmcp/`
and `crates/protocol-droid/tests/interop_ts_sdk/` are excluded from the
workspace, so `cargo deny` never saw their dependency graphs; the pre-push
hook covered them locally, which is a mitigation, not equivalent coverage.

The job now scans all four, one step per lockfile, and each of those
steps passes `--locked`. Without `--locked`, `cargo metadata` re-resolves
the graph and rewrites the lockfile on disk, so the scan would report on
a dependency set nobody committed — a different check, not a stricter
one. With `--locked`, the scan reports on the lockfile as committed, and
fails loudly when that lockfile is stale.

That gap was not theoretical. When `ctutils` was bumped to 0.4.5 for
RUSTSEC-2026-0385, the root `Cargo.lock` was updated and the interop
fixture's was not, so the fixture kept `ctutils 0.4.3` until the pre-push
hook blocked the push. The hook was the only thing that saw it, and the
lockfile it caught was stale on more than the advisory: it still pinned
`protocol-droid 0.1.0` against a workspace at 0.5.2.

The `interop_ts_sdk` fixture is the one case where a manifest *is* watched:
Dependabot's security updates reach it, and it is declared in
`dependabot.yml` so a transitive advisory gets a bump proposal rather than
sitting unnoticed. Note that `npm install` honours the committed lockfile,
so it will not self-heal — the lockfile has to be updated on purpose. The
other two are Rust, and are now covered by CI as well as by the hook.

**The site is built in CI, but the peer-range gap is still worth knowing.**
The `site` job installs with `--frozen-lockfile`, runs `astro check` and
`astro build`, and asserts the output has the expected shape, so a change
under `site/` that does not build fails the PR rather than passing every
check. It was not always so: Dependabot's astro 5 → 7 PR passed fourteen green
Rust checks without building, because starlight 0.30 pins `astro: ^5.0.0` and
`pnpm install` does not enforce peer ranges — the failure appears only at
build time. `pnpm install --frozen-lockfile` does not change that; it
enforces the *lockfile*, not the peer ranges.

`deny.toml` is the single list of accepted advisories, and it is the same
list the `pre-push` hook reads. A waiver belongs there with a reason saying
why the advisory is unreachable from this code; there is no second place to
record one.

## What is out of scope

- **The consumer's deployment.** A consumer that disables TLS
  verification on its own `reqwest` client, that does not set
  `Content-Security-Policy` on its own HTTP server, that picks
  a token-storage adapter that is not `OsSecrets` and stores
  the token in a world-readable file — that is the consumer's
  deployment, not this library.
- **A2A peers that are not under the consumer's control.** A
  push-notification webhook URL that resolves to a private
  network is refused by the SSRF denylist at config time, and
  re-validated at delivery time. A URL that resolves to a
  public network the consumer did not intend to send to is a
  consumer-configuration problem.
- **A peer that has compromised the consumer's process.** The
  library's parsers are fuzzed against malicious input; a peer
  that already has code execution on the consumer's host is
  not in the threat model.
- **MCP revisions older than the lowest vendored one.** The
  library does not claim to speak revisions it does not have
  wire types for. A consumer who needs revision `X` and
  `X` is not in `crates/protocol-droid-schema/spec/` is not
  in scope.
- **The CLI's own configuration file format.** The CLI is a
  thin adapter; the configuration it parses is the consumer's
  to validate, and a bad config is the consumer's error, not
  a security issue.

## Response timeline

| Step                                        | Window          |
| ------------------------------------------- | --------------- |
| Initial acknowledgement                     | 7 days          |
| Triage and impact assessment                | 30 days         |
| Patch landed (or status update)             | 60 days         |
| Public advisory and CVE request (if needed) | 90 days         |
| Embargoed release at the consumer's request | by request      |

A "status update" in the 60-day row is a maintainer message in
the original advisory thread explaining what is blocking and
what the next step is — a patch that needs a contract change,
a reproducer that does not reproduce, a CVE that is still in
the CNA queue. The status update is what prevents a silent
60-day pause.

## Security model

The library is **default deny**:

- An `AuthContext` is a required parameter on every stateful
  port method. There is no path through the library that does
  not carry one.
- Authorization predicates are pure domain functions. The
  predicate that says "this user can see this resource" lives
  in `domain/`, not in an adapter; the adapter hands it the
  data and applies the answer.
- TLS verification is on by default. A `reqwest` client with
  certificate verification disabled is not constructed; the
  library has no path to do so.
- Path canonicalisation refuses a `..` traversal that escapes
  the allowed root with a typed error, not a panic.
- Tokens, Authorization headers, session ids in full, and
  elicitation responses are redacted at the tracing-layer
  boundary. The test that proves the redaction list is
  complete is `tests/secret_logging.rs`.

The full design, including what the library does *not* do
(token storage, rate limiting, CORS) and what the consumer is
responsible for, is in the
[Security model guide](https://greysquirr3l.github.io/protocol-droid/guides/security/)
on the docs site. The audit that backs the no-finding summary
in this policy is in
[`SECURITY-REVIEW.md`](https://github.com/greysquirr3l/protocol-droid/blob/main/SECURITY-REVIEW.md).

## Coordinated disclosure

A 90-day window is the default. A reporter who needs more time
(an embargoed CVE, a downstream rollout that takes longer than
the patch) names the date in the first message; the maintainer
confirms or counters. An embargo that runs past 180 days is
reviewed at 180 days; a longer embargo is honoured only if the
reporter and the maintainer agree in writing.

A patch lands **before** the advisory is public, and the
advisory text is shared with the reporter for review at least
48 hours before publication. The patch is credited in the
advisory, in the release notes, and in the git history; the
reporter's preferred attribution (name, handle, or anonymous)
is honoured.

## Past advisories

None. The repository's first release was tagged after the
SECURITY-REVIEW audit in October 2026, and no vulnerability
has been reported against a tagged release.

## Dependency advisories in the published graph

A dependency advisory is not an advisory against this crate, but it is
still something a consumer of a published version inherits, so it is
recorded here.

**RUSTSEC-2026-0385 — `ctutils` — affected every published version up
to and including 0.5.2. Fixed in 0.5.3.**

`ctutils 0.4.3`'s `Choice` mask generation could compile to a branch
on secret values, and its `u128` comparisons were not constant-time on
32-bit targets. Upstream fixed it in `ctutils 0.4.4`.

The path into this crate is `protocol-droid` → `hmac 0.13` → `digest
0.11.3` → `ctutils`, and `ctutils` is pulled in by `digest`'s `mac`
feature rather than by `hmac`'s own use of it: `digest/src/mac.rs` does
`use ctutils::CtEq;` and calls `ct_eq(tag)` to compare a MAC tag. The
reachable path is this crate's own HMAC comparison in
`application/mcp/input_required.rs`, where `StateKey::open` verifies a
`requestState` MAC with `verify_slice`. That is a real comparison over a
value derived from a server-held key, so the regression is a genuine
defect and not a theoretical one.

What a consumer needs to know:

- **A library consumer** resolves their own dependency graph, so a
  fresh resolve picks up a patched `ctutils` on its own. A consumer
  whose own lockfile still pins `ctutils 0.4.3` should run
  `cargo update -p ctutils`; the requirement is `^0.4`, so the patched
  version is already permitted.
- **A binary consumer** of `protocol-droid-cli` is the case that needs
  attention. A binary crate publishes its `Cargo.lock`, and `cargo
  install` honours it, so `cargo install protocol-droid-cli --version
  0.5.2` installed a binary built against `ctutils 0.4.3`. 0.5.3 ships
  the patched version, as does every release since.

The advisory was published on 2026-10-23, after 0.5.2 was tagged on
2026-10-22, so the fix could not have shipped in 0.5.2.

**RUSTSEC-2026-0387 — `crypto-bigint` — waived, not fixed.**

`crypto-bigint 0.5.5` has a constant-time regression in its `u128`
operations. It is reached through `jsonwebtoken`'s `rust_crypto`
backend → `p256`/`p384` → `ecdsa` → `elliptic-curve`, and no upgrade
path exists: the fix sits behind `p256`/`p384 0.14`, which
`jsonwebtoken 11.1.0`'s caret requirements do not admit.

It is unreachable from this code. The only path to `elliptic-curve` is
ECDSA *verification*, which runs on the public key, the message hash
and the signature's own `r` and `s` — no secret scalar reaches the
affected operations. The signing path is the reachable one and this
crate has no path to it: `jsonwebtoken`'s `EllipticCurveKeyParameters`
carries only `kty`, `crv`, `x` and `y`, so a JWK has no field in which
to carry a private scalar.

The waiver and its full reasoning live in
[`deny.toml`](https://github.com/greysquirr3l/protocol-droid/blob/main/deny.toml),
which is the single list of accepted advisories. It should be re-read
whenever `jsonwebtoken` or `rsa` changes, not waited on a schedule.

## Acknowledgements

The threat model and the no-finding summary draw on the
[OWASP Top 10:2025](https://owasp.org/Top10/2025/) and
[OWASP API Security Top 10:2023](https://owasp.org/API-Security/editions/2023/en/0x11-t10/)
checklists. The MCP-spec-specific surface is read against the
official [Model Context Protocol specification](https://modelcontextprotocol.io/)
and the [conformance suite](https://github.com/modelcontextprotocol/conformance).

---

*This policy is reviewed at every minor release and at every
change to the wire-codec surface. A change to the supported-
versions table or to the response-timeline rows is a release
note, not a docs-only change.*
