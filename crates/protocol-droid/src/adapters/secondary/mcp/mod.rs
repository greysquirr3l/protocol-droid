//! MCP's registration with the protocol-neutral JSON-RPC layer.
//!
//! One [`MethodTable`](crate::application::jsonrpc::MethodTable) implementation
//! and nothing else. The router has no idea this module exists; a future A2A
//! adapter sits beside it rather than inside it, which is the property that
//! makes T27 and T29 additions rather than a fork.

pub mod method_table;

pub use method_table::{McpMethod, McpMethodTable};
