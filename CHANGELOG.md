# Changelog

All notable changes to `protocol-droid` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Every entry cites the commit that made the change, so a claim here can be
checked against the code rather than taken on the changelog's word.

## [Unreleased]

Conformance is now green on every revision the pinned suite covers:
`2026-07-28` went from 81/119 to 151/151, with `2025-03-26`, `2025-06-18` and
`2025-11-25` unchanged at 3/3, 55/55 and 81/81. Most of the gap was library
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

- **The `2026-07-28` lifecycle over stdio**: stateless requests,
  `subscriptions/listen` on the shared channel, cancellation of an in-flight
  request or a listen stream, and the same `-32022` / `-32021` data as HTTP.
- **OAuth client building blocks**: `WWW-Authenticate` parsing, discovery URL
  ordering, canonical resource URI, scope selection and step-up, metadata
  parsing with issuer validation, `client_secret_basic` / `_post` / public
  token authentication, and the `authorization_response_iss_parameter_supported`
  rule (an advertising server that omits `iss` is refused).

### Fixed

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