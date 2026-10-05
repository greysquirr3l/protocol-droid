//! The parts of MCP that exist as wire headers rather than as IR.
//!
//! # Why this module exists at all
//!
//! Everything the application layer exchanges is a version-agnostic IR message,
//! and the JSON-RPC envelope around it. A Streamable HTTP request is neither: an
//! HTTP request is addressed by a set of headers, and three of them carry MCP
//! meaning that the IR cannot express — `Mcp-Session-Id` names the session the
//! request belongs to, and `Mcp-Method`/`Mcp-Name` name the operation and the
//! target before a body is even parsed. Those facts are MCP's, not the IR's and
//! not the HTTP library's, so they are value objects here with the rules about
//! what a legal value looks like attached to the type.
//!
//! This is the first `domain/mcp` module. It exists because the project rule is
//! that MCP-specific code lives under an `mcp` submodule in each layer while the
//! protocol-neutral core stays free of MCP types, and putting a header value in
//! `domain/ir` would have made the IR depend on a transport.
//!
//! # What is deliberately not here
//!
//! Nothing that reads or writes a header. These types know the *grammar* of a
//! value and nothing about HTTP: no `http::HeaderValue`, no request, no status
//! code. The axum adapter in the next stage owns that, and it converts these
//! values at the boundary. A value object that borrowed the HTTP crate's types
//! would be untestable without an HTTP runtime, and this stage has no HTTP
//! dependency at all by design.

mod routing_headers;
mod session_id;

pub mod authorization;
pub mod resource_metadata;

pub use authorization::{AuthorizationModel, ClientRegistration, authorization_model};
pub use resource_metadata::{
    BearerMethod, ResourceMetadata, ResourceUri, ResourceUriError, WELL_KNOWN,
};
pub use routing_headers::{
    MCP_METHOD_HEADER, MCP_NAME_HEADER, RoutingHeaderError, RoutingHeaders, RoutingName,
    RoutingTarget,
};
pub use session_id::{McpSessionId, McpSessionIdError, VISIBLE_ASCII_RANGE};
