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

## What the library does not do

- **Token storage.** The `AuthorizationSecrets` port trait
  is the consumer-side seam. `OsSecrets` is one
  implementation (reads from the OS keyring); a consumer
  that wants a different source implements the trait.
- **Rate limiting.** The transport adapters expose the
  rate-limit knob; the policy is the consumer's.
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
