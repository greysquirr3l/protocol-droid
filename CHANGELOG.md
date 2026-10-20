# Changelog

All notable changes to `protocol-droid` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Every entry cites the commit that made the change, so a claim here can be
checked against the code rather than taken on the changelog's word.

## [Unreleased]

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