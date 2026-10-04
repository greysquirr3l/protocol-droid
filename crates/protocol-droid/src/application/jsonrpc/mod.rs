//! Routing a decoded JSON-RPC message to a method, without naming one.
//!
//! # The seam
//!
//! [`MethodTable`] is the boundary between "this is a well-formed JSON-RPC
//! message naming `tools/call`" and "run the tool handler". The first half is
//! finished here, for every protocol at once, because it is the same work
//! whatever the method is. The second half belongs to a protocol module and is
//! T12's.
//!
//! Nothing in this module names an MCP method. A2A and ACP register their own
//! tables under `adapters` the same way MCP does, which is the property the
//! project's protocol-neutral rule asks for and the reason a later task can add
//! a protocol without a refactor.

pub mod batch;
pub mod router;

pub use batch::{AllowBatching, BatchPolicy, DenyBatching, ProfileBatching};
pub use router::{
    MethodTable, ReplyPayload, RouteError, Routed, RoutedFrame, RoutedMessage, route_frame,
    route_one,
};
