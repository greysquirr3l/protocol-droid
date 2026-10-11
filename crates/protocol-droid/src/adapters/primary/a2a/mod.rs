//! Primary adapters for the A2A protocol.
//!
//! The A2A spec's surface on the server side is a
//! well-known path (the agent card) and a JSON-RPC
//! endpoint (the methods), plus a streaming
//! variant of two of the methods. This module
//! is the driving adapter for all three:
//!
//! - [`well_known_serve`] serves the agent card
//!   on the well-known path.
//! - [`jsonrpc_serve`] serves the regular
//!   (request/response) JSON-RPC endpoint.
//! - [`streaming_serve`] serves the streaming
//!   (SSE) JSON-RPC endpoint.
//!
//! Feature-gated on `a2a` (the value objects) AND
//! `http` (the axum dep). A consumer who turns on
//! `a2a` but not `http` gets the value objects and
//! the JSON-RPC method table but not the HTTP
//! serving — they can implement their own transport
//! without paying the axum cost.
#[cfg(all(feature = "a2a", feature = "http"))]
pub mod jsonrpc_serve;
#[cfg(all(feature = "a2a", feature = "http"))]
pub mod streaming_serve;
#[cfg(all(feature = "a2a", feature = "http"))]
pub mod well_known_serve;
