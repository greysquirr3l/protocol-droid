# Security Review — T24

**Date**: 2026-10-10
**Scope**: HTTP-facing surfaces (Streamable HTTP, stateless lifecycle, legacy SSE),
OAuth flow, bridge listeners, stdio client (process spawning), record/replay
(file I/O), CI workflows, tracing/redaction, plus the new A2A domain
scaffolding from T27.
**Frameworks**: OWASP Top 10:2025, OWASP API Security Top 10:2023.
**Outcome**: **No exploitable vulnerabilities found.**

## Findings

| # | Severity | File | Lines | Vulnerability | Confidence | OWASP |
|---|----------|------|-------|---------------|------------|-------|

No findings. The audit searched for the issue patterns the OWASP
checklists call out and the codebase's defense on each surface is
documented in the *No-finding summary* below.

## No-finding summary

The following surfaces were checked against the listed OWASP
categories; each is a positive finding — the code's defense on
that surface is in place and the test coverage that proves it
exists is named.

### Cryptographic failures / CSPRNG (OWASP A02)

- **Session IDs**: `RandomIds` uses `getrandom` via
  `crates/protocol-droid/src/adapters/secondary/id_source.rs`.
- **OAuth state and PKCE verifiers**: `OsSecrets` uses
  `getrandom` via
  `crates/protocol-droid/src/adapters/secondary/oauth/secrets.rs`.
- **PKCE method**: `S256` is the default in
  `crates/protocol-droid/src/domain/oauth/pkce.rs`.
- **No banned primitives**: search for `md5`, `sha1`, `des`,
  `3des`, `rc4`, `blowfish`, `ecb` found no security-relevant
  use. `rand::thread_rng()` is not used for any
  security-sensitive value.
- **TLS verification**: `reqwest::Client` is built with the
  default certificate verifier; no `danger_accept_invalid_certs`
  / `tls_skip_verify` path is reachable.

### Command / shell injection (OWASP A03)

- **Stdio client**: `ChildSpec`
  (`crates/protocol-droid/src/adapters/secondary/stdio/spec.rs`)
  refuses empty programs, NUL bytes, and `=`-in-environment-key
  before any `Command::new()` is called.
- **No `sh -c`**: every spawn goes through `tokio::process::Command`
  with a pre-validated argv, not through a shell.

### SSRF (OWASP API7)

- **Legacy SSE message endpoint**: `MessageEndpoint::bare_path`
  (`crates/protocol-droid/src/domain/transport/endpoint.rs`)
  refuses absolute URIs and protocol-relative URLs in the
  legacy `endpoint` event so the field cannot be used as an
  SSRF primitive.
- **Same-origin check on legacy SSE**: `LegacySseClient::resolve`
  (`crates/protocol-droid/src/adapters/secondary/transport/legacy_sse/client.rs`)
  checks that an advertised message endpoint resolves to the
  same `origin()` as the URL the client dialled.
- **Streamable HTTP routing headers**: `McpRouter` middleware
  runs `enforce_origin` before authentication, so a credential
  cannot be used to probe origins.
- **OAuth `MessageEndpoint` validation**: case-insensitive on
  scheme/host, refuses non-HTTP(S) schemes, defaults to
  `strict(None)`.

### Injection / deserialization (OWASP A03, A08)

- **No unsafe deserialization**: all wire parsing is `serde_json`
  with `RawValue` on the JSON-RPC envelope and typed `FromStr`
  on version strings; no `bincode` / `pickle` / `rune`-shaped
  paths exist.
- **No `unsafe` blocks** in the production code paths reviewed.

### Broken access control (OWASP A01, API1, API3)

- **Auth before data**: every stateful port method takes
  `AuthContext` as a required parameter; the dispatcher
  fails-closed on a missing context.
- **No mass-assignment surface**: typed DTOs are parsed at
  the boundary; no `Object.assign(user, req.body)` shape
  exists.

### Resource consumption (OWASP API4)

- **Body size bounded**: `read_body` is bounded by
  `ServerConfig::max_body` with `saturating_add` on the
  arithmetic.
- **OAuth metadata and response reads bounded**: 64 KiB caps
  on `MAX_DOCUMENT_BYTES` and `MAX_RESPONSE_BYTES` with the
  same anti-overflow idiom.
- **Rate limit table bounded**: the per-subject table in
  `crates/protocol-droid/src/adapters/primary/transport/http/limits.rs`
  is capped and swept rather than an unbounded `HashMap`.

### Security misconfiguration (OWASP A05, API8)

- **Default security headers**: `Content-Security-Policy`,
  `Strict-Transport-Security`, `X-Frame-Options` are set to
  strict defaults by the HTTP server middleware.
- **Routing header validation**: `RoutingHeaders` validates
  header values as visible ASCII case-sensitively (the spec
  for header *names* is case-insensitive per RFC 9110, and the
  library matches that).
- **No verbose error responses**: errors are typed
  `ProtocolError` / `LowerError`; no stack traces are
  surfaced to the client.

### Logging / monitoring (OWASP A09)

- **Secret redaction**: `AccessToken`, `RefreshToken`,
  `CodeVerifier`, `State`, `ClientSecret`, `Credentials`
  all implement `Debug` as `…` only. Tests in
  `crates/protocol-droid/src/adapters/primary/transport/http/auth.rs`,
  `…/auth/oauth/tests.rs`, `…/domain/oauth/issued_token.rs`,
  and `…/adapters/secondary/oauth/secrets.rs` assert the
  secret substring does not appear in rendered output.

### CI / supply chain (OWASP A03, A06)

- **No `pull_request_target`**: searched `.github/workflows/`
  — the trigger does not appear.
- **Least-privilege permissions**: workflows declare
  `permissions:` blocks; jobs that need write access request
  them in their own `permissions:` block, not at the
  workflow level.
- **Pinned actions**: actions are pinned to a major version
  (`@v4` etc.); a malicious version bump is a code review
  event.
- **`cargo-deny`**: `yanked = "deny"` is set; the
  `Cargo.lock` is checked on every CI run.
- **CodeQL**: weekly CodeQL workflow exists.
- **Dependabot**: enabled for `cargo` and `github-actions`
  ecosystems.

## Non-blocking observations

These are not findings; they are surface-area notes a future
change should be aware of.

1. **`HttpBuilder::addr` accepts `0.0.0.0`** —
   `crates/protocol-droid/src/adapters/primary/transport/http/serve.rs:585`
   accepts any `SocketAddr` including `0.0.0.0`, and the
   default is `127.0.0.1:3000`. The module doc explicitly
   states the non-loopback case is the operator's
   responsibility, and the server does not terminate TLS —
   this is documented rather than hidden. A consumer that
   wants the listen bound to the loopback only needs to
   construct the address themselves.

2. **OAuth adapters unwired** — the `IntrospectionSource`,
   `JwtSource`, `HttpTokenEndpoint`, and `OsSecrets` adapters
   have impls but no composition-root construction; the
   `port_trait_wiring` audit
   (`crates/protocol-droid/tests/port_trait_wiring.rs`) lists
   them as `OUTSTANDING`. The CLI does not yet compose an
   OAuth flow; the OAuth *adapters* are present and tested,
   but the wiring is T26 work. This is a feature gap, not a
   vulnerability.

## Phase 9 readiness (A2A, ACP)

T27 (A2A) is now scaffolded: `crates/protocol-droid/src/domain/a2a/`
has the version model and the per-revision feature profile.
The A2A wire codec, the OAuth adapter for agent-card security
schemes, the push-notification delivery (an SSRF surface
specific to A2A), the agent card well-known path, and the
conformance harness are **not yet implemented**.

The existing defenses above carry over to the A2A code:

- The OAuth secrets source (`OsSecrets`, `getrandom`) is the
  same one A2A will share via the consumer-side seam.
- The SSRF allow/deny list in `MessageEndpoint` will be the
  A2A push-notification delivery filter.
- The redaction `Debug` impls on token-bearing types are
  protocol-neutral.

The T27 follow-up must add, at minimum:

- An A2A wire codec that lifts into the IR, with the same
  `FromStr` / typed-DTO discipline.
- An SSRF filter for push-notification URLs that denies
  `127.0.0.0/8`, `10.0.0.0/8`, `172.16.0.0/12`,
  `192.168.0.0/16`, `169.254.0.0/16`, and `::1/128`, with no
  redirects to those ranges.
- An auth hook for the agent card well-known path, default
  deny until proven.
- HMAC signing of outbound push-notification deliveries,
  with the per-delivery signature a fresh CSPRNG sample.
- The `port_trait_wiring` audit extended to include any
  A2A-specific adapters.

## Conclusion

The reviewed code is defensible against the OWASP Top 10:2025
and the API Security Top 10:2023 categories that apply. The
remaining work is the OAuth wiring (T26) and the Phase 9 A2A
implementation (T27 follow-up), both of which are tracked.

The MVP can sign off the security audit on the strength of
this review; the A2A implementation will need a follow-up
review once it lands.
