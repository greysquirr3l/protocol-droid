---
title: Security model
description: What protocol-droid does to keep a peer on the safe side of the trust boundary, what it does not, and where the consumer's responsibility begins.
---

The library's security model is *default deny*. The auth context
is a required parameter on every stateful port method, and the
authorization predicates are pure domain functions.

## What the library does

- **Token redaction at the tracing boundary.** Tokens,
  Authorization headers, session ids in full, and elicitation
  responses are redacted by the tracing layer. The redaction
  is tested in `tests/secret_logging.rs`.
- **TLS verification on by default.** A `reqwest` client that
  disables certificate verification is not constructed; the
  library has no path to do so.
- **Path canonicalisation for filesystem-backed resources.** A
  resource whose path traverses outside the allowed root is
  refused with a typed error.
- **Fail-secure on auth.** A `?` operator on a failed auth
  check is the default; an `?`-skipped check is not in the
  library.

## Protecting a server, and signing in to one

Both HTTP transports a server offers (Streamable HTTP and the legacy
HTTP+SSE routes) check the `Origin` header and the `Host` header
(the second catches a rebound DNS name, since `Origin` is absent from
every non-browser client), count requests against a per-subject rate
limit before authenticating, and put the same security headers on every
response, including the ones no handler wrote. A request with no
`Host` is refused.

### Requiring a credential on a server

Requiring OAuth is two builders, and the second one is the part that
is easy to leave off. An authenticator answers "is this token valid?";
a policy answers "does this endpoint need one?". The library treats an
authenticator without a policy as the silent fail-open it is, and
`into_router` refuses to build the route table rather than serve the
endpoint unprotected:

<!--
Mirrored from `crates/protocol-droid/examples/oauth_resource_server.rs`, which
is compiled by CI on every commit. Starlight does not expand mdbook's
`{{#include}}`, so this copy is duplicated — if the snippet and the example ever
disagree, the example is the one that is correct.
-->

```rust
use jsonwebtoken::Algorithm;
use protocol_droid::adapters::primary::transport::http::{
    AuthPolicy, HttpBuilder, OAuthAuthenticator, ServerConfig, SystemClock,
};
use protocol_droid::adapters::secondary::oauth::jwt::JwtSource;
use protocol_droid::application::oauth::NoScopes;
use protocol_droid::application::server::Server;
use protocol_droid::domain::ProtocolVersion;
use protocol_droid::domain::mcp::{ResourceMetadata, ResourceUri};
use protocol_droid::domain::registry::auth::{Scope, ScopeSet};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Where tokens presented to this server are verified against. A real
    // deployment points this at its own JWKS; `IntrospectionSource` is the
    // opaque-token alternative.
    let source = JwtSource::new(
        Arc::from("https://login.example.com/.well-known/jwks.json"),
        vec![Algorithm::RS256],
    );

    // RFC 9728's protected-resource metadata: which resource this is, and who
    // may issue tokens for it. One value serves both purposes below, so the
    // document that is served and the challenge that points at it cannot name
    // different URLs.
    let metadata = ResourceMetadata::new(ResourceUri::new("https://mcp.example/mcp")?)
        .with_authorization_server("https://login.example.com")
        .with_scope("mcp:read");

    // Read before the metadata moves into the config. This is the exact string
    // a `401` carries in `WWW-Authenticate`, verbatim.
    match metadata.challenge() {
        Ok(challenge) => println!("a 401 would carry: {challenge}"),
        Err(error) => println!("no challenge: {error}"),
    }

    // Half one: what validates a token presented *to* this server. The audience
    // is bound to `metadata`, which is what stops a token minted for a
    // different resource being replayed here.
    let authenticator = Arc::new(OAuthAuthenticator::new(
        source,
        metadata.clone(),
        ProtocolVersion::V2025_06_18,
        NoScopes,
        SystemClock,
    ));

    // Half two: what says the endpoint needs one at all. These are separate
    // because "nothing is configured yet" and "this endpoint is deliberately
    // public" are different states, and conflating them is how a default
    // becomes a decision nobody made.
    //
    // Wiring an authenticator without a policy is the silent fail-open it looks
    // like, so `into_router` refuses to build the route table instead of
    // serving the endpoint unprotected.
    let policy = AuthPolicy::Require(ScopeSet::empty().with(Scope::Read));

    let config = ServerConfig::new()?.with_resource_metadata(metadata);

    // `Server` is what a `#[server]` declaration produces through
    // `.into_server()`; building one directly here keeps the example runnable
    // without a full tool declaration.
    let server = Server::builder("mcp").build();

    // `.serve().await` binds the address and starts listening. This example
    // stops short of that so it exits and prints instead of hanging.
    let _builder = HttpBuilder::new(server)
        .config(config)
        .with_authenticator(authenticator)
        .with_policy(policy);
```

Two details in that snippet are easy to trip over. `metadata` is cloned
rather than moved, because the authenticator needs the audience and the
config needs the publication URL, and it is deliberately not `Copy` — it
owns an `Arc<str>`. And `Algorithm` comes from `jsonwebtoken`, which is
a dependency of the OAuth feature rather than a type this crate
re-exports.

`with_resource_metadata` mounts the document on the MCP router and
points the `401` challenge at that same URL, both read from one value, so
the challenge cannot name a document that is not being served. The
document route deliberately skips the authenticator — the escape route
cannot require the credential it explains how to obtain — but it still
passes the same `Host` and rate-limit checks as every other route.

A single-issuer deployment should also name the issuer, which is off by
default because RFC 9728 publishes a *list* of trusted authorization
servers:

```rust
let authenticator = authenticator.with_expected_issuer("https://login.example.com");
```

On the client side, the OAuth flow:

- refuses a server whose metadata describes a different resource,
- refuses an authorization server that does not advertise PKCE `S256`,
- sends `resource` as the metadata published it and checks `iss` on the
  response,
- never takes a client id from one authorization server to another,
- only opens web URLs in the browser, and receives the redirect on
  `127.0.0.1`,
- gives up after three refusals instead of asking a person forever.

A client registered ahead of time is named through
`PROTOCOL_DROID_OAUTH_CLIENT_ID` and `PROTOCOL_DROID_OAUTH_CLIENT_SECRET`
in the CLI, never a flag, so a secret does not reach shell history or a
process listing.

## What the library does not do

- **Token storage.** The `AuthorizationSecrets` port trait
  is the consumer-side seam. `OsSecrets` is one
  implementation (reads from the OS keyring); a consumer
  that wants a different source implements the trait.
- **Choosing the rate limit.** Both HTTP transports enforce one; the
  allowance is the consumer's to set.
- **CORS.** The Streamable HTTP server's CORS configuration
  is a transport-level concern; the library does not
  second-guess it.

## What the consumer is responsible for

- Choosing the `TokenSource` and `TokenEndpoint` adapters
  that match their deployment.
- Setting `Content-Security-Policy`, `Strict-Transport-Security`,
  and `X-Frame-Options` on the HTTP server they wrap around
  the library.
- Wiring the rate limit and request size limit at the
  transport level.

## Threat model

The library is designed against a peer that:

- may send malformed JSON-RPC frames,
- may send a method name the server does not register,
- may send a capability the server did not negotiate,
- may retry an in-flight request id,
- may cancel an unknown or completed request.

The library is *not* designed against a peer that has
compromised the consumer's process. The library's input
parsers are tested against malicious inputs (the fuzz
harness in `crates/protocol-droid/fuzz/` runs nightly), but
a compromised process is the consumer's problem.

:::tip
Read the [architecture overview](/protocol-droid/architecture/overview/) for how the
`AuthContext` requirement and the port-trait returns-DOMAIN-error rule fit
together. The split between "library does" and "consumer does" is the line
that makes the `AuthorizationSecrets` trait a seam, not a check.
:::
