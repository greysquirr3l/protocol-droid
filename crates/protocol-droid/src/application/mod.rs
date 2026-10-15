//! Application layer: one module per use case.
//!
//! Each handler declares the narrow port trait it consumes, inline, in its own
//! module. A handler returns domain error types, never a third-party one:
//! adapters map their own failures into domain errors at the boundary, so no
//! infrastructure error type ever appears in a signature here.
//!
//! Populated by T06 (negotiation), T08 (codecs), T09 (JSON-RPC routing), T10
//! (session engine), T11 (dynamic registry), T12 (server and client APIs) and
//! T14 (the idempotency store a retried `tools/call` is deduplicated through).

// T27. The A2A use cases: the URL-filter port, the push-notification
// delivery port, and the per-revision codecs the wire layer feeds
// into. Gated on the `a2a` feature because the dependency footprint
// (the `url` crate) is independent of MCP and a consumer that never
// speaks A2A should not pay it.
#[cfg(feature = "a2a")]
pub mod a2a;

// T29. The ACP use cases: the request-handler port
// for the agent side, plus the typed per-method
// wrappers. Gated on `acp-experimental` because the
// spec is pre-1.0 and the dependency surface is
// independent of MCP — a consumer that never speaks
// ACP pays no cost.
#[cfg(feature = "acp-experimental")]
pub mod acp;
pub mod bridge;
pub mod client;
pub mod idempotency;
pub mod jsonrpc;
pub mod mcp;
pub mod oauth;
pub mod registry;
pub mod server;
pub mod session;
