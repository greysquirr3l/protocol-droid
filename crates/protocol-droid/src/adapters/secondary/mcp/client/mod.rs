//! The MCP client: the wire shapes, and the peer that carries them.
//!
//! Two files, because they are two things. [`types`] is the vocabulary —
//! the `initialize` request, the capability blocks, the tool call shapes —
//! transcribed from the vendored schema and saying nothing about transport.
//! [`peer`] is the transport: a child process, the reader and writer tasks,
//! and the pending-reply table that matches a reply to its request.
//!
//! The peer speaks newline-delimited JSON-RPC 2.0 in both directions, owns
//! the child process, and kills it on drop.

pub mod peer;
pub mod types;

// Re-exported flat so that every path that resolved before the split still
// resolves. The seven capability blocks in particular are only used inside
// `types` today, but they were reachable as `mcp::client::RootsCapability` and
// a downstream consumer naming that path should not have to learn that the
// file moved.
pub use peer::{McpPeerError, McpStdioPeer, SpawnSpec};
pub use types::{
    CallToolRequest, CallToolResult, ClientCapabilities, ContentBlock, Implementation,
    InitializeRequest, InitializeResult, ListToolsResult, LoggingCapability,
    PromptsCapability, ResourcesCapability, RootsCapability, SamplingCapability, ServerCapabilities,
    Tool, ToolsCapability, default_client_capabilities, initialize_request_template,
};
