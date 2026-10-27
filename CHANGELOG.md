# Changelog

All notable changes to `protocol-droid` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Every entry cites the commit that made the change, so a claim here can be
checked against the code rather than taken on the changelog's word.

## [Unreleased]

### Fixed

- **A session reported the transport's ceiling instead of the revision it
  negotiated.** (`crates/protocol-droid/src/application/server/dispatch/mod.rs`,
  `crates/protocol-droid/src/application/server/context.rs`) `PeerSession::negotiated`
  is documented as "the revision this session actually settled on", and it
  answered with the ceiling — the newest revision the build could speak —
  whatever the peer asked for. The handshake narrowed the session's *profile*
  and left its *revision* alone, so the version that `negotiate` had already
  resolved was used to build the reply and then dropped.

  A handler could not learn which revision it was speaking, so a consumer
  that wanted to report its own version had to report a range of revisions
  rather than the one in use. The gate's refusal named the ceiling too, and
  the refusal exists precisely to say what was negotiated: "elicitation is
  unavailable" is an answer nobody can act on without knowing what *was*
  agreed.

  The fix narrows the revision with the profile, in the same place the
  profile was already narrowed. The two move together because the profile is
  resolved *for* the revision: the profile says which features exist, and the
  revision says how to spell them, so a session that knew one but not the
  other refused the features the older revision does not have and still
  answered in the newer revision's shape for everything else.

  The shape is not a detail. A tool result is lowered for the session's
  revision, and each revision has its own codec, so a `2024-11-05` peer was
  sent results spelled for `2025-11-25` — a `structuredContent` its schema
  has no field for, and content blocks such as `audio` that its schema has
  no type for at all. A peer that asked for an older revision could be sent
  something it cannot parse.

  Two paths were already right and are unchanged: the stateless lifecycle
  builds a peer per request from the revision the request names, and legacy
  HTTP+SSE negotiates the one revision its transport defines.

- **A client that sent the `MCP-Protocol-Version` header it had negotiated
  was refused.** (`crates/protocol-droid/src/adapters/primary/transport/http/serve/`)
  The header is an assertion, not a fallback: a present
  `MCP-Protocol-Version` that disagrees with the negotiated revision is a
  protocol error, and it is rejected rather than reconciled. But the
  comparison was against the session's ceiling rather than the revision it
  settled on, so a client that offered `2024-11-05` and sent
  `MCP-Protocol-Version: 2024-11-05` was answered `400`, while a client that
  sent the ceiling was accepted. Both follow from the same root cause.

## [0.6.1] — 2026-10-26

A patch, and it is a patch because nothing here changes an API. The
dependency bumps are the routine kind, and the two fixes are fixes to the
tests themselves rather than to the library. 0.6.0 is a separate release
because it has been published — the tag exists and the crates are on
crates.io — so the two are not folded together.

### Fixed

- **The `a2a_js_interop` test could fail for a reason that had nothing to
  do with what it measures.** (`crates/protocol-droid/tests/a2a_js_interop.rs`,
  `crates/protocol-droid/tests/fixtures/a2a_reference_server.mjs`) The test
  reserved a port by binding `127.0.0.1:0`, read the number, dropped the
  listener, and handed the number to the Node reference server. Between the
  drop and the child's bind, another process could take the port; the child
  died with `EADDRINUSE` and the test reported it as "the reference server
  closed stderr before listening", which says nothing about the A2A wire
  format. It failed twice on `macos-latest` in CI, both times with correct
  code.

  The kernel now assigns the port: the child asks for port 0, and the bind
  that claims the port and the bind that reports it are the same syscall, so
  there is no window for anything to slip into. The reference server reports
  the port it was actually given rather than the one it asked for, and its
  agent card is built from that same bound port. The test is also faster —
  3.48 s to 0.17 s — because it no longer waits on a port it has to race
  for.

- **The `bench_gate` smoke test measured the machine as much as the
  decoder.** (`crates/protocol-droid/tests/bench_gate.rs`) It took one
  wall-clock sample and compared it to a threshold. A wall-clock sample
  measures the machine as much as the code: the same decoder measured
  4 300 ns per iteration on an idle machine and 59 563 ns on a busy one, a
  14× spread against a threshold of 15 100 ns. The failure said "the decoder
  got slower", which was false.

  The measurement is now the fastest of ten samples rather than a single
  one — the fastest sample is the one least disturbed by the OS preempting
  the test thread, and a real regression is present in every sample, so the
  minimum still catches it. Measured under a load average of 20, the fastest
  of ten samples was 4 640 ns while the median was 12 179 ns and the
  slowest was 22 079 ns: the minimum recovers the uncontended cost, the
  median does not.

  The threshold was wrong independently of the noise. It scaled the committed
  release-mode baseline by 15, a dev/release cost ratio guessed when the
  test was written and never revisited; the real ratio is about 21. A
  threshold computed from 15 is 15 100 ns, where the contract says 5× the
  baseline — which put the threshold *below* the cost of an uncontaminated
  decode on a loaded machine, and is why the test was tripping on noise.
  The 100 ns padding term is gone as well: a pad on top of "5× the
  baseline" means a decoder that is exactly 5× slower does not trip the
  gate, which is not what a 5× gate means.

  A second test now pins the contract, by asking the gate's own comparison
  whether a synthetic 5× slower decoder trips it and whether an
  uncontaminated one does not. Without it the constants could be loosened
  until the gate stopped catching anything, and nothing would say so — the
  failure mode of a gate that has quietly stopped gating is silence. The
  gate was checked against a real regression too: a slowdown injected into
  `decode` was caught at 25 615 ns against the 20 000 ns threshold, and
  reverted.

### Changed

- **Dependency bumps.** `tokio` 1.53.3 → 1.54.1, `arc-swap` 1.9.2 → 1.9.3,
  `jsonschema` 0.58.6 → 0.60.2 and `ureq` 3.4.2 → 3.5.0. The `jsonschema`
  bump crosses a minor version and the crate's API did move, but it is a
  dev-dependency — it validates generated values against the vendored
  `schema.json` and nothing else — so the change is confined to the test
  suite.

### Documentation

- **`docs/benchmarks.md` described a CI bench job that does not exist.**
  (`docs/benchmarks.md`) The document described a job that runs the divan
  benches and compares their medians against `baselines.json` with a 10 %
  threshold in either direction. There is no such job, and `baselines.json`
  is read by nothing but the documentation. The document now says what is
  actually there: the `bench_gate` smoke test is the automated performance
  gate, and the committed baselines are reference data for a human reading a
  bench run.

  The reason the divan benches are not in CI is the same reason the smoke
  test now reads the fastest of ten samples: a wall-clock measurement on a
  shared runner measures the runner. A 10 % threshold there is not a
  stricter gate, it is a noisier one. Wiring those benches into CI needs a
  runner that is not shared, and is its own piece of work.

## [0.6.0] — 2026-10-26

A minor, and the reason is that the `#[server]` attribute grew a field.

`#[server]`'s `into_server()` built the server with `Server::builder(name)`
and no version, so `serverInfo.version` reported the version of the
`protocol-droid` crate the consumer happened to be linked against — not the
consumer's own version. The escape hatch (`ServerBuilder::version`) existed,
but reaching it meant hand-writing the builder chain, which means giving up
the macro's registration. A server that merely links `protocol-droid` is not
`protocol-droid`, and a peer asking which version a server is asks about the
server.

### Added

- **`version = ...` on the `#[server]` attribute.** The field takes an
  **expression** rather than a string literal:

  ```rust
  #[protocol_droid::server(name = "greeter", version = env!("CARGO_PKG_VERSION"))]
  ```

  The `env!` is expanded in the *consuming* crate, because a proc-macro
  expansion is compiled in the crate that uses it. The version is therefore
  written once — there is no second copy to drift — and a consumer that
  bumps its own manifest has nothing else to update. A literal still works,
  for a server whose version is not in a manifest, and a non-string literal
  is refused with a message that names the fix rather than a trait-bound
  error about `Into<String>`.

  The cross-crate proof is a trybuild pass case
  (`crates/protocol-droid/tests/ui/pass/server_version.rs`). A test *inside*
  `protocol-droid` cannot make the claim, because there the consuming
  crate's version and the dependency's are the same string; a trybuild case
  is its own crate at version `0.0.0`, so the two are distinguishable. The
  case was checked against a mutation that hard-codes the dependency's
  version, and fails with the mismatch it is supposed to catch.

- **A test that fails if a committed lockfile stops being scanned.**
  (`crates/protocol-droid/tests/supply_chain_coverage.rs`) The failure mode
  is silent — a workflow edit that drops a step leaves a green CI run and a
  lockfile nobody scans. The test reads the workflow, works out which
  manifests are scanned, and fails if any committed lockfile is not among
  them.

### Changed

- **A server built by `#[server]` reports the version of the crate that
  wrote it.** With no `version` field, the default is now the consuming
  crate's own version rather than `protocol-droid`'s — the macro emits the
  same `env!` the explicit form would, so a server that names no version no
  longer claims to be the dependency.

  This is the one change here a consumer can notice without asking for it,
  so it is worth stating plainly: a peer that was told `0.5.3` is now told
  the consumer's own version. The old answer was wrong — it was a fact
  about the dependency rather than about the server — but a consumer
  matching on the old string will see a difference. That is why this is a
  minor and not a patch.

  A server built by hand through `Server::builder` is unaffected and still
  defaults to `protocol-droid`'s own version, which is right for a server
  that *is* this crate.

### Fixed

- **CI now scans every committed lockfile, not just the root one.** The
  `supply chain` job ran `cargo deny` against the root `Cargo.lock` only,
  while three lockfiles outside the workspace — `fuzz`, `interop_rmcp` and
  `interop_ts_sdk` — were excluded from the workspace and so from that
  scan. The gap was not theoretical: when `ctutils` was bumped to 0.4.5 for
  RUSTSEC-2026-0385, the root lockfile was updated and the interop
  fixture's was not, so the fixture kept the vulnerable version until the
  pre-push hook blocked the push.

  Each of the three new steps passes `--locked`. Without it, `cargo metadata`
  re-resolves and rewrites the lockfile, so the scan would report on a
  dependency set nobody committed. The cost is that a workspace version bump
  invalidates those lockfiles, which now fail the job instead of going
  stale; the release procedure records how to refresh them.

- **The pre-push hook no longer passes an `unsound` advisory.** `cargo audit`
  files an `unsound` advisory under `warnings` rather than `vulnerabilities`
  and exits zero for it by default, so the hook — which read only
  `vulnerabilities.count` — reported a lockfile carrying a use-after-free
  advisory as clean. The hook now passes `-D unsound -D yanked` and reads
  all three categories out of the report, matching the set `deny.toml`
  denies. A hook that passes where CI fails is worse than no hook.

  A yanked crate is reported separately from an advisory, because the
  remedies differ: an advisory can be waived in `deny.toml`, while a yanked
  crate has no advisory id to ignore and can only be fixed by updating the
  lockfile.

### Documentation

- **The "Write a server" guide described an API that does not exist.**
  (`site/src/content/docs/guides/write-a-server.md`) It showed
  `use protocol_droid::prelude::*`, a `#[server]` attribute on a *struct*,
  a `Dispatcher` *trait* a consumer implements, and a `Server::new(..)` /
  `serve(config)` pair — none of which is the crate's shape. The real API
  is `#[protocol_droid::server]` on an *impl block*, `Dispatcher` is a
  struct rather than a trait, and the entry points are
  `Server::builder(name)` and `serve_stdio` / `serve_http`. A reader
  following the guide would not have got a compiling program.

  The guide now also says what the attribute has to be attached to and
  why: an attribute macro only sees the item it is on, so a `#[server]`
  on a struct cannot see the `impl` beside it, and every tool in that
  `impl` would be invisible to it.

- **The quickstart imported a `prelude` module and described a runtime
  the library does not build.**
  (`site/src/content/docs/getting-started/quickstart.md`) There is no
  `protocol_droid::prelude`. The tip block also said `serve_stdio` builds
  a runtime; it is `async` and runs on whatever runtime the caller
  already has, which is what the `#[tokio::main]` in the same snippet is
  for.

## [0.5.3] — 2026-10-25

A patch for the `#[server]` macro, and a security fix that a patch is the
right shape for.

The macro fix is the reason this release exists: a consumer using
`#[server]` / `#[tool]` had to add `schemars` to their own `Cargo.toml` or
the generated code would not compile. The error pointed at the `#[server]`
attribute rather than at the macro, so the natural first reaction —
`use schemars;` — does not help.

The advisory is the reason the patch reaches the dependency graph at all.
`ctutils 0.4.3` has a non-constant-time mask generation, and it reaches
this crate through `hmac 0.13` → `digest 0.11.3` → `ctutils`. The fix is a
lockfile update, not an API change, so a patch is what it is.

### Changed

- **Dependency advisories on the repository are down from 28 to 1.** The
  remaining one is `esbuild` (low, `GHSA-g7r4-m6w7-qqqr`) in
  `site/pnpm-lock.yaml`. Dependabot reports the update as
  `security_update_not_possible` — latest resolvable `0.27.7`, lowest
  non-vulnerable `0.28.1` — but that is a resolution result rather than a
  proof that no fix exists, since `0.28.1` is published and vite's peer range
  admits it. A `pnpm.overrides` entry forcing `esbuild: 0.28.1` is the untried
  path. The advisory covers the `astro dev` server on Windows; the published
  site is static output and never runs it.

  [`docs/dev/repo-operations.md`](docs/dev/repo-operations.md#the-one-advisory-that-stays-open)
  records the analysis.

- **The documentation site is built in CI.** (`.github/workflows/site.yml`)
  Nothing built `site/` before, so a green PR on that directory said nothing
  about the site. Dependabot's astro 5 → 7 PR passed fourteen green Rust
  checks and did not build; the only reason it was caught before production
  was a local `pnpm build`.

  The new job installs with `--frozen-lockfile`, runs `astro check` (which
  the build does not do on its own), builds, and asserts the output has the
  expected shape. It is a required status check, so a broken site blocks the
  merge.

  Two details worth knowing. The workflow has no `paths:` filter, because a
  path-filtered workflow reports no check run when the filter does not match,
  and a required check that never reports blocks every merge; the filter is
  applied to the build steps instead, so the job always reports. And the node
  version is pinned to `22.23.4`, the floor astro 7 declares, rather than the
  newest release — proving the site builds on the oldest runtime it claims to
  support. `pnpm` is pinned the same way, through the `packageManager` field
  in `site/package.json`.

  Documented in
  [`docs/dev/repo-operations.md`](docs/dev/repo-operations.md#ci-builds-this-site).

### Fixed

- **The `#[server]` macro no longer needs the consumer to depend on
  `schemars`.** (`dbea59a3`, #63) A consumer of `protocol-droid` that used
  `#[server]` / `#[tool]` had to add `schemars` to its own `Cargo.toml` to
  make the generated code compile, and the error pointed at the `#[server]`
  attribute rather than at the macro.

  The generated argument struct derives `JsonSchema` through this crate's
  re-export, but the derive expands to unqualified `schemars::…` paths that
  resolve against the *consuming* crate's root. `schemars` 1.x honours the
  derive's `crate` attribute — it emits `use <path> as schemars;` inside the
  `const _` block it wraps the expansion in, shadowing the unresolved name
  for every path it expands to, including the `where`-clause bounds — so one
  attribute per generated struct fixes it.

  The regression test lives in `crates/protocol-droid-cli/tests/` because
  that crate is the workspace's one consumer of `protocol-droid` that does
  *not* depend on `schemars`. Every test inside `protocol-droid` is blind to
  this: that crate depends on `schemars` itself, so it is in the extern
  prelude of its own tests whatever the expansion does. The test also reads
  the generated `inputSchema` back out of the registry, so a redirect
  pointing at the wrong crate fails there rather than passing a compile
  check.

### Security

- **`ctutils` is fixed rather than waived.** (`RUSTSEC-2026-0385`, medium)
  `ctutils 0.4.3`'s `Choice` mask generation could compile to a branch on
  secret values, and `ctutils`' `u128` comparisons were not constant-time on
  32-bit targets. `ctutils 0.4.4` fixes it; this workspace takes `0.4.5`.

  This one is reachable, and it is worth being precise about where. The path
  is `protocol-droid` → `hmac 0.13` → `digest 0.11.3` → `ctutils`, and
  `ctutils` is pulled in by `digest`'s `mac` feature rather than by
  `hmac`'s own use of it — `digest/src/mac.rs` does `use ctutils::CtEq;` and
  calls `ct_eq(tag)` to compare a MAC tag. Our own HMAC comparison in
  `application/mcp/input_required.rs` is exactly that path:
  `StateKey::open` verifies a `requestState` MAC with `verify_slice`. A
  constant-time regression there is a real defect and not a theoretical one.

  0.5.2 shipped the vulnerable version in all four crates' `Cargo.lock`, and
  `protocol-droid-cli` publishes its lockfile, so the published binary
  carried it. A consumer who resolves fresh gets `0.4.5` from the caret
  requirement on `ctutils 0.4`; a consumer whose own lockfile still pins
  `0.4.3` needs `cargo update -p ctutils`.

- **All four committed lockfiles now carry the patched `ctutils` and the
  current `protocol-droid` version.** The three excluded from the workspace
  (`fuzz`, `interop_rmcp`, `interop_ts_sdk`) had drifted: the root lockfile
  moved to `ctutils 0.4.5` and the interop fixture's did not, so it kept the
  vulnerable `0.4.3` until the pre-push hook blocked the push. The other two
  were staler still, pinning `protocol-droid 0.1.0` against a workspace at
  0.5.2.

  All four are current now, and each records `ctutils 0.4.5`. The hook scans
  every committed lockfile, so this is the state it was already asking for;
  the drift was that nobody had run it since the advisory landed. The CI gap
  this exposes is recorded in `SECURITY.md` rather than closed here.

- **`RUSTSEC-2026-0387` on `crypto-bigint 0.5.5` is waived, with the
  argument written down.** (`deny.toml`) It is reached through
  `jsonwebtoken`'s `rust_crypto` backend → `p256`/`p384` → `ecdsa` →
  `elliptic-curve`, and there is no upgrade path: the fix sits behind
  `p256`/`p384 0.14`, which `jsonwebtoken 11.1.0`'s caret requirements
  (`^0.13.2` / `^0.13.0`) do not admit, and 11.1.0 is the latest release
  whose `master` carries the same pins.

  It is unreachable because the only path to `elliptic-curve` here is ECDSA
  *verification*. `ecdsa`'s `verify_prehashed` runs on the public key, the
  message hash and the signature's own `r` and `s` — no secret scalar
  reaches the affected operations. The signing path (`sign_prehashed`, which
  calls `k.invert()` on a secret scalar) is the reachable one and this crate
  has no path to it: `jsonwebtoken`'s `EllipticCurveKeyParameters` carries
  only `kty`, `crv`, `x` and `y`, so a JWK has no field in which to carry a
  private scalar.

  The waiver carries that argument in full, so the next person to read
  `deny.toml` does not have to re-derive it.

- **`enforce_admins` is on.** (`.github/branch-protection.json`) It was
  `false`, which meant a repository admin could push straight to `main` and
  bypass the pull-request requirement. Enabling Dependabot's alerts made this
  worth closing: an advisory is now reported against the repository, and the
  person who would fix it could also skip the review saying the fix is right.

  The release chain is unaffected — Auto Tag pushes a tag, and branch
  protection applies to branches, not tags.

- **`ip-address` in the interop fixture is fixed rather than suppressed.**
  (`GHSA-rxp7-6hxx-x8q5`, medium) Dependabot proposed this as #55 and it was
  closed in favour of an ignore rule, on the reasoning that a transitive bump
  churns a lockfile whose job is to hold the SDK at one version. That
  reasoning was wrong: the SDK's own declared range is `^10.2.0`, so 10.7.4
  was always permitted. The bump moves three lines, leaves
  `@modelcontextprotocol/sdk` at `1.32.1`, and takes the fixture's
  `npm audit` from one moderate to zero. The ignore rule is removed.

  `npm install` does not do this on its own — it honours the lockfile, so
  10.7.3 stayed installed until the lockfile was updated deliberately. The
  `dependabot.yml` entry for that directory is kept for exactly this
  reason: without it there is no schedule watching a lockfile that only
  changes by hand.

## [0.5.2] — 2026-10-22

A patch in name only for #50: fifty-seven files moved and not one line of the
public API changed. #53 is a real fix, and it is on the documentation site
rather than in the library.

### Added

- **A tracked pre-push hook.** (`fe2a8dc`) Install it with
  `./scripts/install-hooks.sh`. It runs `cargo fmt --all --check` — the same
  command the `fmt` job runs — and `cargo audit` over every committed
  `Cargo.lock`. The advisory waivers are read from `deny.toml`, so there is
  one list rather than two, and it honours a waiver the moment you add it.
  Documented in [`docs/dev/repo-operations.md`](docs/dev/repo-operations.md#git-hooks).

### Changed

- **Dependabot now covers the documentation site.** (`0cec69a`) The
  `npm_and_yarn` entry for `/site` was missing, so the site's dependencies were
  being updated on an undeclared cadence. Site bumps are grouped into one PR,
  because astro and `@astrojs/starlight` are version-coupled and have to be
  tested against each other.

- **Seven files had grown too large to read.** (`300fa16`..`cc579cf`) The MCP
  client, the Streamable HTTP serve module, the dispatcher, the OAuth
  registration module, the replay command and the server context were each split
  along the seams already implicit in them — wire types from peers, types from
  ports, routing from negotiation.

  No behaviour change, and the public surface is unchanged by design. The new
  submodules are private (`cc579cf`); the one type that genuinely moved,
  `DispatchError` from `server::builder` to `server::error`, is still
  re-exported from `builder` so both paths keep resolving. The seven files came
  to 9548 insertions and 8936 deletions, so this is a move rather than a
  rewrite.

  Two tests fail intermittently on `main` as well —
  `decode_is_within_bench_gate` and
  `the_childs_stderr_never_reaches_the_protocol_stream`. Both were measured
  against `main` and both are timing-sensitive rather than caused by this
  change. Neither is touched here.

### Fixed

- **The documentation site was 13 dependency advisories behind, including a
  critical remote code execution in astro.** (`929f14d4`) Astro `5.18.2 →
  7.4.2` clears GHSA-26w7-cxv4-gfx2 (RCE through AVIF image optimisation),
  both astro high-severity advisories and six of its own XSS-class ones;
  `sharp 0.33.5 → 0.35.5` clears three libvips/libheif/librsvg advisories.
  That is 13 of the 15 distinct advisories Dependabot found.

  Two remain, and neither is reachable from a build of a static site:
  GHSA-rj75-hqrm-r3gf (postcss-selector-parser, medium) and GHSA-g7r4-m6w7-qqqr
  (esbuild arbitrary file read, low — dev server only). Both are transitive
  build-time tooling with no fix available at the versions astro resolves.

  Astro had to move with `@astrojs/starlight 0.30 → 0.42` — starlight pins
  `astro: ^5.0.0`, and `pnpm install` does not enforce peer ranges, so the
  mismatch surfaced only at build time. Two Starlight config shapes changed
  with the major and are fixed in `site/astro.config.mjs`: autogenerated
  sidebar groups were removed in 0.39, and `social` became an array in 0.33.

  Dependabot's own astro PR ([#52](https://github.com/greysquirr3l/protocol-droid/pull/52))
  does not build for exactly this reason. It was closed rather than merged; the
  same applies to #51, which bumped `sharp` on its own.

### Security

- **Dependency alerts, secret scanning and push protection are on.**
  Dependabot vulnerability alerts, Dependabot security updates, secret scanning
  and push protection now cover this repository. Enabling the alerts surfaced
  **28 advisories against the documentation site** — 2 critical, 10 high —
  which is what prompted the site migration above. None were in the Rust
  workspace; `cargo deny` in CI had been reporting correctly all along.

  These are GitHub repository settings, so there is no commit to cite — they
  exist only in GitHub's database. They are now recorded in
  [`docs/dev/repo-operations.md`](docs/dev/repo-operations.md#security-settings),
  with the API call needed to re-apply each one.

## [0.5.1] — 2026-10-21

The other half of the Entra token fix. 0.5.0 bound `aud` correctly but still
refused a valid delegated token for carrying no scopes, because the claim it
carries them in is not the one being read.

### Fixed

- **Every delegated token from Microsoft Entra was refused as unscoped.**
  (`301df79`) The JWT source read the granted permissions from the `scope`
  claim alone. Entra's v2.0 delegated tokens carry them in `scp` — the same
  space-delimited list under Microsoft's name — and carry no `scope` claim at
  all, so such a token arrived with no permissions and `verify` refused it with
  `Refusal::Unscoped`. A server fronted by Entra therefore refused every valid
  token, silently, until its first real request. `scp` is now read alongside
  `scope`, and an issuer that sends both has both granted.

### Security

- **`webpki-roots` 1.0.9 → 1.0.10** (`24bf997`) — RUSTSEC-2026-0381. The
  bundled root store contained a distrusted emSign intermediate; 1.0.10 removes
  it. [`rustsec`](https://rustsec.org/advisories/RUSTSEC-2026-0381)
- **`rustls-webpki` 0.103.15 → 0.103.16** (`24bf997`) — RUSTSEC-2026-0382.
  Certificate verification may have accepted distrusted intermediates.
  [`rustsec`](https://rustsec.org/advisories/RUSTSEC-2026-0382)

Both are transitive, reached through `reqwest` on the server and client halves
and `ureq` in `xtask`, so this is a lockfile change with no manifest edit. Both
resolve within the 1.98 MSRV floor.

## [0.5.0] — 2026-10-20

A token's `aud` can now be bound independently of the RFC 9728 resource
identifier, and `aud` is no longer modelled as one. Closes
[#46](https://github.com/greysquirr3l/protocol-droid/issues/46).

### Fixed

- **Every token from Microsoft Entra was refused.** (`deb220d`) `verify`
  compared a token's `aud` only against `policy.resource`, with no way to
  configure anything else. Entra's v2.0 tokens carry the web API's *client ID* —
  a bare GUID — in `aud`, while RFC 9728 §3.1 requires the resource to be the
  HTTPS URL because the metadata path is derived from it. A server fronted by
  Entra therefore refused every valid token with `AudienceMismatch`, silently,
  until its first real request.
- **An `aud` that is not a URI was dropped before it could be compared.**
  (`deb220d`) `aud` was modelled as `ResourceUri`, which requires an http(s)
  scheme, so it could hold neither Entra form — not the bare GUID, and not
  `api://<guid>` either. `with_audience_string` then discarded any entry that
  did not parse, so such a token reached `verify` with an *empty* audience and
  was refused however the policy was configured, reporting a mismatch that
  blamed the token for a decode which had thrown the answer away. RFC 7519 §4.1.3
  defines `aud` as a `StringOrURI` and puts no URI requirement on it.

### Added

- **`TokenPolicy::with_expected_audience` / `with_expected_audiences`,** and the
  matching `OAuthAuthenticator` builders, naming audiences to accept *in
  addition to* the resource. Comparison stays exact equality per identifier — no
  host, prefix or case folding — so a sibling path and a second web API's GUID
  both still fail, and the resource stays accepted so configuring an audience
  never withdraws the RFC 8707 binding. Defaults to `None`, leaving existing
  deployments unchanged.
- **`Audience`** (`deb220d`) — one `aud` entry, held verbatim and never
  canonicalised. Exported from `application::oauth`.

### Changed

- **`TokenClaims::audience` returns `&[Audience]`,** not `&[ResourceUri]`, and
  `with_audience` accepts `impl Into<Audience>`. **This is a breaking change**
  and the reason for the minor bump: RFC 7519 §4.1.3 places no URI requirement
  on `aud`, so `ResourceUri` was the wrong type for it — it could not represent
  the identifiers authorization servers actually mint. `impl Into<Audience>`
  still accepts a `ResourceUri` through the `From` impl, so a caller that was
  passing one keeps compiling.
- **`with_audience_string` retains every entry,** including one that is not an
  absolute URI, rather than dropping it. Two tests asserted the old
  drop-behaviour and now assert retention; they are the ones that would have
  caught this.

## [0.4.3] — 2026-10-20

Documentation, release tooling and dev-dependencies. No library behaviour
changes, which is why this is a patch.

This release carries **everything since 0.4.1** in one entry. `v0.4.2` was
tagged and prepared but never published — the run sat at the approval gate and
was abandoned rather than forced — so there is no 0.4.2 on crates.io and there
never will be. The version was skipped, not backfilled.

### Fixed

- **The docs-site deploy script could report success while deploying nothing.**
  `rsync` compares size and mtime by default, so a rebuilt page of unchanged
  byte length whose mtime landed in the same second was judged identical,
  skipped, committed, pushed, and reported live — while the served tree still
  held the previous deploy. Now compared by content.
- **Previewing a deploy stopped the deploy.** `--no-push` committed and skipped
  only the push, leaving the pages clone ahead of its remote. The next real run
  saw a clean index, reported "nothing to deploy", and exited without pushing.
- **Live verification proved reachability, not freshness.** It polled for HTTP
  `200`, which a CDN still serving the previous deploy answers immediately, so
  it passed on the first attempt against a stale site. It now compares the
  served body against the local build.
- **A missing `package.json` aborted the deploy script silently.** `jq` exits 2
  on an unreadable file and `set -e` took the process with it before the
  validation errors could print.
- **The release chain reported "nothing to release" in a way that read like a
  failure.** Most green pushes to `main` are not releases, so it is the common
  case — but the message sat in a scrollback with no summary on the run page,
  and the obvious conclusion was that the release was broken. Both no-op paths
  now write a job summary and a run annotation saying what happened, why it is
  expected, and that bumping `[workspace.package] version` is the only trigger.
  The "tag created" path also states that the run will stop at the environment
  gate needing one approval, so the point where a human is required is visible
  before it is reached.

### Changed

- **The GitHub Actions move off Node.js 20.** `actions/checkout`,
  `actions/setup-node` and `actions/upload-artifact` v4 → v7,
  `github/codeql-action` v3 → v4, `softprops/action-gh-release` v2 → v3. Every
  run in this repository carried a Node 20 deprecation notice because the
  actions targeted a runtime GitHub now forces off. `node-version: "20"` in the
  interop workflow is deliberately unchanged — it is the LTS line
  `@modelcontextprotocol/sdk` targets, and pinning it is what makes a green run
  on Monday a green run on Tuesday.
- **The README no longer carries a "Project status" section.** Every claim in
  it changed on a release rather than on a change to what it described, so it
  was on a guaranteed-rotted schedule — it was already claiming 0.4.0 while
  0.4.1 shipped. The durable facts are covered by the install instructions, the
  CI status checks, and `docs/CONFORMANCE-GAPS.md`.
- **`Branch protection`, `Releasing` and `The docs site` moved to
  `docs/dev/repo-operations.md`.** They describe running the repository rather
  than using the crates, and had grown the README into a maintainer's manual.
- **Deleted `docs/guide/` and the root `book.toml`.** A second copy of the
  guide, built by no workflow, published by nothing, linked from nowhere, and
  already drifted from the site by dozens of lines. The site is now the only
  maintained document tree, and a test fails if a second one reappears.

### Added

- **`site/scripts/deploy-docs.test.sh`** — ten assertions covering the first
  two deploy fixes, against a throwaway git origin and a local HTTP server
  standing in for the CDN. The `rsync` case forces the size-and-mtime
  collision deliberately, so it fails deterministically if `--checksum` is
  removed rather than passing by luck.
- **`docs/dev/repo-operations.md` gained a table of what each release run state
  means**, and a note that a CI run failing with *"the job was not started
  because recent account payments have failed"* is an Actions billing refusal
  rather than a code failure. Four pull requests sat red for eight days on that
  alone, with `FAILURE` on every job while nothing had been compiled.

### Dependencies

- **proptest 1.11.0 → 1.12.0.** Development only, not a build or runtime
  dependency of the published crates. It introduces a second `rand_core` line
  (0.10 via `rand` 0.10, against the 0.6 the RustCrypto stack reaches through
  `jsonwebtoken` still pins), which is now a documented `cargo deny` skip entry
  alongside the other twelve.

## 0.4.2 — never published

`v0.4.2` was tagged and its release run built every binary and verified
packaging, but it stopped at the `release` environment gate and was
abandoned rather than forced. No 0.4.2 was ever uploaded, so crates.io
goes 0.4.1 → 0.4.3 with nothing at 0.4.2. The tag is still on the commit
it marked, because that commit is real history; only the publish never
happened.

Its contents are folded into 0.4.3 above. Nothing that was written for
0.4.2 was lost — it was superseded, not abandoned mid-flight.

## [0.4.1] — 2026-10-19

Documentation only. No library behaviour changes, which is why this is a patch.

### Fixed

- **The README's install instructions named 0.2.0.** Both dependency
  snippets — the default build and the lean one — said `protocol-droid = "0.2"`
  while 0.4.0 was the current release, so a reader who followed them installed
  a crate that predates the RFC 9728 document and the OAuth resource server.
- **The security guide's resource-server snippet was never compiled.** It was
  hand-written markdown that no CI job built. Writing it surfaced four import
  paths that do not exist; committing it as prose would have shipped all four.

### Added

- **`http` is named as a requirement in the README.** The default build is
  `serde` + `stdio`, so reaching for `ServerConfig` fails with
  ``cannot find `transport` in `primary` ``. The README now says so, with the
  error text quoted from a build against the published crate rather than
  recalled.
- **`examples/oauth_resource_server.rs`.** The resource-server assembly is now a
  compiled example, built by the existing `--all-targets --all-features` matrix,
  and the security guide is spliced from it.
- **`tests/published_docs_consistency.rs`.** Three checks on the documents a
  reader actually receives: every README install snippet names the current
  minor, every manifest feature has a row in the README's feature table
  (`serde` did not), and the site's security snippet still matches the example
  it was copied from. Each was verified to fail when the underlying drift is
  reintroduced.

`docs/guide/` is untouched and untested here: it is an mdBook tree that no
workflow builds, no deploy script publishes, and nothing links to, and it has
already drifted from the site by dozens of lines.

## [0.4.0] — 2026-10-19

A server can now require OAuth from public API alone, and publish the document
a client needs to find out how. Closes
[#33](https://github.com/greysquirr3l/protocol-droid/issues/33).

### Added

- **The RFC 9728 protected-resource metadata document is served.**
  (`e1767c9`) A client that receives a `401` reads the `WWW-Authenticate`
  challenge, follows it to the metadata document, and learns where to get a
  token. Until now the challenge could name a URL nothing was serving.
  `ResourceMetadata` gained the `Serialize` impl RFC 9728 §2 defines, with
  §3.2's omit-when-empty rule; `serve_protected_resource` returns a
  `GET`-only router for deployments that need the document on a different
  origin than the MCP endpoint.
- **The metadata document mounts itself.** (`31d82af`)
  `with_resource_metadata` mounts the route on the MCP router and points the
  `401` challenge at the same URL, both read from one value, so the two cannot
  drift apart. The route deliberately skips the authenticator — an escape route
  cannot require the credential it explains how to obtain — and still passes
  the same `Host` and rate-limit checks as every other route.

### Fixed

- **The default build did not compile.** (`0fd17f1`)
  `cargo check --workspace --all-targets` failed on default features because
  the `a2a_serving` bench imported `adapters::secondary::a2a` without
  `required-features = ["a2a"]`. CI missed it because it ran
  `cargo test --workspace`, which skips benches. The matrix now checks
  `--all-targets` per feature set.
- **Intra-doc links were broken and rustdoc was ungated.** (`ad7ce31`) Six
  unresolved links under lean features, and nothing in CI ran `cargo doc`, so
  they could not reach `main`. CI now gates on `--all-features`.
- **The `auth` module doc described T16 as future work.** (`7d8902d`) It had
  shipped; the doc still said it had not, which is how a reader concludes the
  resource-server half does not exist.

### Documented

- **Requiring a credential on a server.** (`db69cdc`, `2752b01`) The security
  guide documented the client OAuth flow in detail and the resource-server half
  not at all. It now covers `with_authenticator`, `with_policy`, the
  auto-mounted document, and the fail-closed contract that refuses to build a
  route table when an authenticator is wired without a policy. The snippet is
  compiled, not hand-written — an earlier draft cited four import paths that
  did not exist.
- **A chain test ties the `401` challenge to the served route.** (`db69cdc`)
  Both halves existed; nothing proved they named the same URL. The test fails
  on a wrong resource, a wrong path, and on config that names a document it
  does not serve.
- **An assembly test answers [#33](https://github.com/greysquirr3l/protocol-droid/issues/33)'s
  question.** (`2752b01`) The issue claimed a server could not require OAuth
  without assembling internals. It could, and can still — the test builds one
  from public API alone, which is what the claim needed.

### Previously unreleased fixes from `0.3.0`

- **The `0.3.0` binaries job could not produce a release.** (`ce3ed1d`) Three
  separate faults, each fatal on its own: the job had no `contents: write`, so
  it could not create the draft release it was uploading to; the musl targets
  had no C compiler for `ring`; and the Windows build ran under PowerShell,
  which cannot parse `set -euo pipefail`. `cargo --bin` was also given the
  target name with its `.exe`.
- **Two test assertions printed the very secrets they police.** (`6011212`) A
  panic message is a log line, and one test interpolated a full session id into
  its failure message while another named the offending credential in the
  assertion that exists to stop credentials reaching an error string — so the
  one circumstance in which the value reached a CI log was the one where the
  test caught it leaking. The second now reports which of the three went wrong
  without reprinting it; the first names the delta, which was the whole
  diagnosis anyway.
- **The docs site could not install.** (`9c79168`) pnpm 11 refuses to install
  while a dependency's build script is neither allowed nor denied, and both
  `esbuild` and `sharp` still held the placeholder value, which broke
  `pnpm build` and the deploy script. Both genuinely need their install step:
  `esbuild` fetches its platform binary, `sharp` its native module.

## [0.3.0] — 2026-10-19

Conformance is now green on every revision the pinned suite covers, on both
sides. The server matrix is 3/3, 55/55, 81/81 and 151/151 on `2025-03-26`,
`2025-06-18`, `2025-11-25` and `2026-07-28` (`2026-07-28` was 81/119). The
client matrix, newly run, is 2/2, 18/18 and 32/32 on `2025-06-18`,
`2025-11-25` and `2026-07-28`, OAuth included. Most of the gap was library
behaviour, not fixture work, and several of the fixes apply to every revision.

### Added

- **Runtime registration that announces itself.** (`927f9d0`) `Server::handle()` returns a
  clone-able `ServerHandle` that adds and removes tools, resources, templates
  and prompts while serving. Each change is published and then sent as
  `notifications/{tools,resources,prompts}/list_changed` to every session whose
  negotiated profile includes the matching `listChanged` — on stdio, on a
  Streamable HTTP session, and on a `2026-07-28` listen stream. Before this the
  server emitted no `list_changed` on any revision, and a tool added while
  serving was listed but could not be called.
- **`ServerHandle::notify_resource_updated`.** (`927f9d0`) `resources/subscribe` (and
  `resourceSubscriptions` on a listen stream) now record a real subscription,
  so the `resources.subscribe` capability the server advertises is honoured.
- **The `2026-07-28` stateless lifecycle over HTTP, in full** (`927f9d0`): per-request
  `_meta` validation, `MCP-Protocol-Version` / `Mcp-Method` / `Mcp-Name` and
  `Mcp-Param-*` checked against the body (`-32020`), `-32022` echoing the
  requested version, `-32021` with `requiredCapabilities`, `404` for removed and
  unknown methods, `subscriptions/listen`, and `serverInfo` in every result's
  `_meta`.
- **SEP-2322 multi round-trip requests.** (`927f9d0`) `InputRequired` builds an interim
  result; `RequestContext::input_response`, `request_state` and `seal_state`
  read and write the retry. `requestState` is HMAC-SHA256-sealed under a
  per-server key, so a tampered state is refused before any handler runs.
- **A stateless HTTP client.** (`927f9d0`) `StatelessHttpClient`, reached through
  `Client::connect_to` with an `http-stateless:` spec, sends the per-request
  `_meta` and headers and mirrors `x-mcp-header` arguments.
- **Sampling and elicitation over stdio.** (`927f9d0`) The serve loop keeps reading while
  a handler waits on the client, delivers the answer, and queues any request
  that arrives meanwhile so replies keep their order.
- **The `2026-07-28` lifecycle over stdio** (`a2cadea`): stateless requests,
  `subscriptions/listen` on the shared channel, cancellation of an in-flight
  request or a listen stream, and the same `-32022` / `-32021` data as HTTP.
- **OAuth client building blocks** (`a2cadea`): `WWW-Authenticate` parsing, discovery URL
  ordering, canonical resource URI, scope selection and step-up, metadata
  parsing with issuer validation, `client_secret_basic` / `_post` / public
  token authentication, and the `authorization_response_iss_parameter_supported`
  rule (an advertising server that omits `iss` is refused).
- **An OAuth 2.1 client that runs the whole flow.** (`687d90e`) `OAuthAuthorizer` handles a
  `401` or `403`: protected-resource metadata, authorization-server discovery,
  PKCE `S256` verification, client selection (held, pre-registered, metadata
  document, dynamic registration), scope selection and step-up, `resource`
  taken verbatim from the metadata, `iss` checks, and redemption. Pass it to
  `ConnectOptions::authorizing` and the Streamable HTTP and stateless clients
  retry with the token. It stops after three refusals. It passes every
  `auth/*` scenario of the client conformance suite on `2025-11-25` and
  `2026-07-28`.
- **Sign in from a browser.** (`dbde884`) `LoopbackBrowser` is a `UserAgent` that opens the
  authorization URL (web URLs only) and receives the redirect on a loopback port,
  with a timeout. `protocol-droid inspect <server> --oauth list` uses it; a client
  registered ahead of time is named by `PROTOCOL_DROID_OAUTH_CLIENT_ID` and
  `PROTOCOL_DROID_OAUTH_CLIENT_SECRET`, never by a flag.
- **`probe` recognises a legacy HTTP+SSE endpoint.** (`288ff83`) A URL that refuses every `POST`
  with a 4xx is retried as `sse:` (the 2025-03-26 backward-compatibility rule),
  and the result is reported only if that works.
- **Refresh tokens.** (`a89bb63`) `OAuthAuthorizer` keeps the refresh token a login returns and
  trades it for a new access token when the server says the token is no longer
  good, so an expired token no longer sends the user back through consent. A
  spent or revoked refresh token is forgotten and the full flow runs; a
  challenge for more scope skips the refresh. `TokenEndpoint::refresh` is a
  new defaulted method, so existing implementors still compile.
- **A `Server` over the legacy HTTP+SSE transport.** (`4f904b6`) `HttpBuilder::legacy_sse()`
  (or `legacy_sse_at(stream, message)`) mounts the `2024-11-05` routes beside
  Streamable HTTP on one listener. The same tools, resources and prompts answer
  on both, `list_changed` and progress go out on the session's stream, a tool
  can ask the client a question, and a client offering a later revision is
  answered with `2024-11-05`. `LegacyDispatcher::dispatch` is now async and
  carries per-session state (`LegacySession`, `LegacyDispatcher::open`).
- **The bridge forwards to a `2026-07-28` stateless upstream.** (`8598ad5`) `protocol-droid
  bridge --upstream http-stateless:http://host/mcp` lets a `2025-11-25` (or
  older) client use a stateless server: the upstream's revision is discovered
  once, and each forwarded request carries it and the bridge's capabilities in
  its own `_meta`. `TransportPeer` is the library piece, usable over any
  `ClientTransport`.
- **`conformance_reference_server --legacy-sse`** (`6332dce`) mounts the `2024-11-05` routes
  beside `/mcp`, as a runnable example of `HttpBuilder::legacy_sse()`; probe it
  with `protocol-droid probe sse:http://127.0.0.1:8080/sse`.
- **`Host` checking and rate limiting on the legacy routes.** (`4f904b6`) They had `Origin`
  and authentication but neither of these, so a rebound DNS name or a flood of
  `POST`s reached the dispatcher.
- **`a2a` implies `http`.** (`687d90e`) Building with `--features a2a` alone did not compile.
- **A comparative benchmark against `rmcp`.** (`dbf2cd6`) `interop_rmcp`'s `compare` binary
  drives both stdio servers with the same client; the two are within about 5 %
  of each other (`docs/benchmarks.md`).

### Changed

- **`LegacyDispatcher::dispatch` is async and carries per-session state.**
  (`4f904b6`) It gains an associated `Session` type and an `open` method, and the
  router's session store now holds a `LegacySession`. Anyone who implemented the
  trait must update; the crate's own implementors did.
- **A legacy HTTP+SSE session negotiates only `2024-11-05`.** (`4f904b6`) A client
  offering a later revision is answered with it, because it is the only revision
  that defines the transport.
- **The idempotency store keeps the last 4 096 finished results rather than
  refusing new work once full.** (`dbf2cd6`) A retry is replayed while it is within
  that window; an older one runs again. A store full of running calls still
  refuses. See *Fixed*.
- **Documentation.** (`67e076a`) The CLI reference is rewritten against the binary's real
  flags, and the guides cover OAuth sign-in, the legacy server, the stateless
  bridge and the security checks.

### Fixed

- **A server stopped answering `tools/call` after its 4 096th call.** (`dbf2cd6`) The
  idempotency store filled with finished results and refused every new request
  id from then on, so any long-lived session or server failed permanently. It
  now drops the oldest *finished* result to make room, never one still running.
- **The legacy HTTP+SSE client never matched a numeric reply id.** (`4f904b6`) It
  compared the awaited id as the string `"1"` against the number `1` on the
  wire, so any server that answered with a numeric id left every request waiting.
- **The bridge offered its upstream the oldest revision first** (`8598ad5`), so it
  negotiated `2024-11-05` with every server that still accepted it. It now offers
  the newest revision that has a handshake.
- **A finished tool result from a `2026-07-28` server could not reach an older
  client.** (`8598ad5`) `resultType: "complete"` was treated as a feature the older
  revision lacks. Older revisions say the same thing by omitting the field, so
  the bridge now forwards it; `input_required` is still refused.
- **`protocol-droid bridge --allow-downgrade` could not be used while serving.**
  (`8598ad5`) clap required `--check-result` beside it, so only the probe could
  receive the flag.
- **A `default-features = false` build warned about an unused import** in the
  client connector. (`687d90e`)

- **A numeric `progressToken` was echoed back as a string** (`927f9d0`) on every revision,
  so a client correlating by value never matched its progress.
- **stdio delivered no progress or log notifications.** (`927f9d0`) The peer's sink was a
  placeholder that refused every frame, and the progress token was never read.
- **`logging/setLevel` was accepted and ignored.** (`927f9d0`) It now filters; at
  `2026-07-28` a request without `logLevel` receives no log records.
- **Stateless requests shared one session id** (`927f9d0`), so two clients reusing a
  request id for the same tool were answered with each other's recorded result.
- **Listings wrote `"description": null`** (`927f9d0`) for an absent description, which no
  revision's schema admits.
- **Methods a revision removed were still answered** (`927f9d0`) (`ping`,
  `logging/setLevel` and `resources/(un)subscribe` at `2026-07-28`).
- **The HTTP client never sent `MCP-Protocol-Version`** (`927f9d0`) after negotiating
  `2025-06-18` or later.
- **`protocol-droid probe` reported `2026-07-28` as refused** (`927f9d0`) by servers that
  support it: its `server/discover` carried no `_meta`. A Streamable HTTP
  target's stateless revisions are now probed with the stateless client.
- **The Release workflow failed on every green push** that did not create a
  tag. It now reports "nothing to release" and skips (`d7fe867`).

## [0.2.0] — 2026-10-17

A minor bump, not a patch: `ServerBuilder::resource` and `::prompt` are renamed
to `resource_with` / `prompt_with`, and a bare `#[resource]` is now a compile
error. That breaks source for anyone who registered a listing-only resource,
so under semver it is `0.2.x` rather than `0.1.x`. All four crates ship at the
same version.

The five commits below are the whole of the delta since `v0.1.0`, and all of
them close gaps on the path a consuming project takes: what a tool can answer
with, what a server can serve, and who the server thinks is calling.

### Added

- **A `#[tool]` handler can answer with any content block** (`a359baf`). A
  handler whose return type is `ToolResult` now takes a path that lowers
  through the existing `WireToolResults` codec at the session's negotiated
  revision, rather than being shaped by the macro into a single text block.
  Images, audio blocks, and embedded resources become reachable from `#[tool]`
  for the first time. A block the negotiated revision cannot express is refused
  under `DegradePolicy::strict` rather than quietly dropped — audio is answered
  at `2025-03-26` and refused at `2024-11-05`.
- **`resources/read` and `prompts/get`** (`2fcb9d6`). Both methods were
  absent: the dispatcher answered seven methods and refused the rest, so a
  server could register resources, advertise them in `resources/list`, and then
  fail to hand one over. Registration is opt-in via `resource_with` alongside
  `resource`, and `prompt_with` alongside `prompt`.
  - `ResourceContents` and `GetPromptResult` land as pure IR with no
    serialisation of their own; assembly lives in `application/server/read.rs`
    because it depends on the negotiated revision.
  - A prompt's content block goes through the tool codec rather than a second
    copy of it, so `prompts/get` cannot drift from `tools/call` on a field a
    later revision adds.
  - Neither method is idempotency-keyed, unlike `tools/call`: a read has no side
    effect to protect, and a key would make two legitimate concurrent reads of
    one URI collide into `DuplicateInFlight`.

### Changed

- **A bare `#[resource]` or `#[prompt]` is now a compile error** (`51f78f7`).
  `ServerBuilder::resource` and `prompt` registered a listing with no way to
  serve it, and the dispatcher could not tell that apart from a URI the server
  never had — both answered `InvalidParams`. That breaks a promise on the wire
  rather than at the call site that built it. The listing-only methods are gone
  rather than deprecated:

  ```rust
  #[resource(reader = serve_it)]   // a reader must be named
  ```

  **Migrating from 0.1.x.** `ServerBuilder::resource` and `::prompt` become
  `resource_with` and `prompt_with`, taking the handler as a second argument:

  ```rust
  // 0.1.x
  Server::builder("app").build().resource(my_resource);
  // 0.2.0
  Server::builder("app").build().resource_with(my_resource, serve_it);
  ```

  The methods are renamed rather than the old pair deprecated, so the compiler
  names the replacement instead of leaving a listing that still does not serve.
  `resources/read` and `prompts/get` did not work in 0.1.x at all — the
  dispatcher answered `UnknownMethod` — so nothing that worked stops working.

  The reader is named rather than inferred from a convention: a convention works
  until a resource's contents are read by a helper three impls away, and then it
  registers the wrong method or silently registers nothing — both surface to a
  client as a resource that lists and will not open. Six compile-fail fixtures
  pin each error message.

### Fixed

- **The release workflow published crates in the wrong order.** The loop read
  `schema → protocol-droid → macros → cli`, but `protocol-droid` depends on
  `protocol-droid-macros`, so crates.io cannot resolve it until the macros
  crate is published. Publishing the macros *last* makes the library's own
  `cargo publish` fail with `no matching package named
  \`protocol-droid-macros\` found`. Both the dry-run and the real publish loop
  now order by the dependency graph: `schema → macros → protocol-droid → cli`.
  The same wrong order was documented in the release guide and is fixed there
  too. This would have failed at tag time, on the first real release, with
  nothing in CI to have caught it.
- **The library no longer hardcodes its sibling versions.** `protocol-droid`
  depended on `protocol-droid-macros` with a literal `version = "0.1.0"`, so
  bumping `[workspace.package]` produced a build failure rather than a version
  bump. The macro crate is now a proper `[workspace.dependencies]` entry and
  inherits, which means one list to bump instead of a pin that silently
  disagrees with it.

- **A handler now sees the authenticated principal on every transport**
  (`6a81010`, `2f91316`). `PeerSession` documented `with_auth` as the way a
  real principal arrives, and no transport in the tree ever called it — so a
  `#[tool]` handler's `ctx.peer().auth()` reported the anonymous default even
  when the request had authenticated successfully. This failed closed (anonymous
  is default-deny), but it left a consumer with no way to attribute an action to
  a caller on HTTP: no audit trail, no per-caller policy, no per-caller quota.
  - *Streamable HTTP* (`6a81010`): the principal is resolved per request and
    overlaid onto that request's own copy of the peer. The stored session keeps
    the anonymous default — session state outlives the credential that produced
    it, and a principal recorded there has the wrong lifetime for a token that
    can be refreshed, replaced, or withdrawn mid-session. The new
    `PeerSession::without_auth` makes that stored-peer shape explicit and is what
    a persisted handshake result stores.
  - *Legacy HTTP+SSE* (`2f91316`): `LegacyDispatcher::dispatch` now takes an
    `&AuthContext` as its first parameter, resolved per request. A legacy session
    is nothing but an event tail, so there is no session state for a principal
    to be pinned to anyway.
  - *stdio* (`2f91316`): `StdioBuilder::auth` lets the deployment declare the
    principal for the life of the connection. There is nothing on a pipe to
    authenticate, and the alternative was unreachable rather than merely
    awkward — a scope-gated tool is invisible to an anonymous session, so a
    stdio server registering a `Write`-scoped tool could never serve it.
- **`protocol-droid-cli` manifest parses again** (`2f91316`). It inherited
  `categories` from a `workspace.package` that does not define it. That is a
  hard parse failure, not a warning: any `cargo` command touching the workspace
  failed to load the manifest. Now `command-line-interface`, which exists as a
  crates.io slug, rather than `command-line-utilities`, which does not.

### Known gaps

- **Conformance matrix is below target on three revisions.** `tools-call-error`
  fails on `2025-06-18`, `2025-11-25`, and `2026-07-28`. The root-cause
  analysis and the six-step order of operations for closing the matrix are in
  [`docs/CONFORMANCE-GAPS.md`](docs/CONFORMANCE-GAPS.md); the short version is
  that the codecs and session engine round-trip every wire shape the suite
  checks, and the failing scenarios are asking the reference-server fixture for
  tools and prompts it does not yet register.

## [0.1.0]

Initial release. Workspace, vendored schemas at pinned commits, `ProtocolVersion`
and the feature catalogue, the canonical IR with six lift/lower codecs, the
single-pass JSON-RPC router and session engine, all four transports with both
server and client halves, OAuth 2.1, HTTP hardening, the A2A and ACP protocol
modules, and the `bridge` / `probe` / `inspect` / `record` / `replay` /
`conformance` CLI surface.