//! A2A's registration with the protocol-neutral JSON-RPC layer.
//!
//! One [`MethodTable`](crate::application::jsonrpc::MethodTable) implementation
//! and nothing else. The router has no idea this module exists; an MCP adapter
//! sits beside it in `adapters/secondary/mcp/`, and a future ACP adapter slots
//! in the same way. That parallelism is the property the MVP seam was
//! designed to give T27 for free.

pub mod client;
pub mod jsonrpc_peer;
pub mod method_table;
pub mod ssrf_denylist;
pub mod webhook_delivery;

pub use client::{A2aClient, A2aClientError};
pub use jsonrpc_peer::{A2aJsonRpcPeer, A2aPeerError};
pub use method_table::{A2aMethod, A2aMethodTable};
pub use ssrf_denylist::SsrfDenylist;
