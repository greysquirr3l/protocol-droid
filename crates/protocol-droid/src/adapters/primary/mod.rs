//! Driving adapters: the server transports.
//!
//! Populated by T13 (stdio), T14 (Streamable HTTP) and T15 (legacy HTTP+SSE).
//!
//! These are the transports the project has to run on: stdio, legacy HTTP+SSE as
//! defined for 2024-11-05, stateful Streamable HTTP, and the stateless
//! lifecycle from 2026-07-28. Each is feature-gated so a
//! `--no-default-features` build stays a lean core.

#[cfg(all(feature = "a2a", feature = "http"))]
pub mod a2a;

// T29. The ACP agent side: serving a consumer's handler
// over the stdio transport. ACP is a stdio protocol, so
// the agent loop is a driving adapter here while the peer
// stays secondary — the same split MCP uses.
#[cfg(feature = "acp-experimental")]
pub mod acp;

#[cfg(feature = "http")]
pub mod transport;

#[cfg(feature = "stdio")]
pub mod stdio;

// T31. The UTCP server side: publishing this server's tools as a manual at
// `GET /utcp`. Its own router, because a manual is a document and not a
// session, and a consumer mounts it beside the MCP endpoint with one
// `merge`.
#[cfg(feature = "utcp")]
pub mod utcp;
