//! Driven adapters: the client transports and the injected side-effect ports.
//!
//! Populated by T13 (stdio, including child-process spawning), T14 and T15
//! (client-side HTTP transports) and T10 (the `Clock` and `IdSource` ports the
//! domain depends on but must never call globally).

pub mod codec;
pub mod idempotency;
// T16. A session id source that does not enumerate. `IdSource` has been a port
// with no production implementation since T10 — every test writes its own
// `fetch_add`, which is the obvious thing to copy into production by accident.
// Gated on its own feature because the only reason to link an entropy source is
// to ask it for unpredictable session ids.
#[cfg(feature = "random-ids")]
pub mod id_source;
pub mod mcp;
// T16. The OAuth token sources: RFC 7662 introspection and JWT/JWKS. Gated on
// `oauth` rather than on `http` even though both open a socket, because the
// JWS signature check is a second reason on top of that one — and a consumer
// that wants an authorization gate over a socket it already has still should
// not be made to link a JOSE implementation to get it.
#[cfg(feature = "oauth")]
pub mod oauth;

#[cfg(feature = "http")]
pub mod transport;

// T13. The client half of the stdio transport: spawning a child server, killing
// it on drop, and keeping its stderr off the protocol stream. Gated with the
// server half because it is the same dependency — a tokio runtime and pipes —
// and a build that speaks stdio on one side and not the other has no use.
#[cfg(feature = "stdio")]
pub mod stdio;
// T27. The A2A (Agent2Agent) JSON-RPC method table. Sits beside the MCP table
// rather than inside it — the protocol-neutral core in `application/jsonrpc`
// is what routes on the table, and a future ACP table slots in the same way.
// Gated on its own feature because the dependency footprint is independent
// of MCP and a consumer that never speaks A2A should not pay it.
#[cfg(feature = "a2a")]
pub mod a2a;
