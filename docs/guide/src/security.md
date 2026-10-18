# Security model

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
