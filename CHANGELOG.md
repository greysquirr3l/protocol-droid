# Changelog

All notable changes to `protocol-droid` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Every entry cites the commit that made the change, so a claim here can be
checked against the code rather than taken on the changelog's word.

## [Unreleased]

Five commits past the `v0.1.0` tag. All five close gaps on the path a
consuming project takes: what a tool can answer with, what a server can serve,
and who the server thinks is calling.

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

  The reader is named rather than inferred from a convention: a convention works
  until a resource's contents are read by a helper three impls away, and then it
  registers the wrong method or silently registers nothing — both surface to a
  client as a resource that lists and will not open. Six compile-fail fixtures
  pin each error message.

### Fixed

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