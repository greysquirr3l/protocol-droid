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
//!
//! # Two ports, and why they are not one
//!
//! [`FrameSource`] is the inbound edge — a stream of whole frames — and it is
//! declared here because the loop that reads a frame and routes it is this
//! module's consumer. The outbound edge is
//! [`FrameSink`](crate::application::session::FrameSink), which sits with the
//! session engine that consumes it instead. Same direction of travel, different
//! consumer, different module: a trait that both read and wrote would make every
//! implementor answer for both, and a replay source only reads at all.

pub mod batch;
pub mod frame_source;
pub mod router;

pub use batch::{AllowBatching, BatchPolicy, DenyBatching, ProfileBatching};
pub use frame_source::{DynFrameSource, FrameSource, SourceError};
pub use router::{
    MethodTable, ReplyPayload, RouteError, Routed, RoutedFrame, RoutedMessage, route_frame,
    route_one,
};
