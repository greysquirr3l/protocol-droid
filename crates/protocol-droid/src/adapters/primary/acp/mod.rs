//! Primary adapters for the ACP protocol.
//!
//! # What lives here
//!
//! [`serve`] drives the agent side of the ACP loop: the
//! editor's requests come in, the consumer's handler
//! answers, the reply goes back. ACP is a stdio
//! protocol, so this is a driving adapter in the same
//! sense the MCP server transports are — it owns the
//! loop, the peer is the thing it drives.
//!
//! # Where the peer lives
//!
//! The peer itself is a *secondary* adapter
//! (`adapters::secondary::acp`): it is the transport,
//! and it is what the primary adapter drives. The split
//! follows the same shape as MCP's, where the stdio
//! client is secondary and the server transports are
//! primary.

pub mod serve;
#[cfg(test)]
mod tests;

pub use serve::{
    serve_acp_agent, serve_acp_agent_with_context, serve_acp_agent_with_version,
    AcpServeError,
};
