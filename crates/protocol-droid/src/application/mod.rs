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

pub mod client;
pub mod idempotency;
pub mod jsonrpc;
pub mod oauth;
pub mod registry;
pub mod server;
pub mod session;
