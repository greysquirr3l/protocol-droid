//! The protocol-neutral JSON-RPC layer.
//!
//! # What belongs here
//!
//! JSON-RPC 2.0 has no knowledge of MCP, and neither does anything in this
//! module. The envelope, the id, the error codes, and the decode step are the
//! same for an MCP session, an A2A session, and an ACP session — which is the
//! property that lets a later task add a protocol as a feature-gated module
//! rather than a fork of the transport layer.
//!
//! The converse boundary is enforced by the module layout rather than by
//! discipline: the method table that says `tools/call` exists lives in
//! [`crate::application::jsonrpc`] as a trait, and its MCP implementation lives
//! under `adapters`. Nothing here names a method.
//!
//! # The two views of a message
//!
//! | | Owned | Borrowed |
//! | --- | --- | --- |
//! | module | [`crate::domain::ir`] | this one |
//! | holds | [`crate::domain::ir::IrMessage`] | [`Frame`] |
//! | costs | an allocation per string id | none |
//! | used when | a message has to be stored, logged, or replayed | a message is being routed |
//!
//! They are not two versions of the same thing and neither replaces the other.
//! The borrowed view is what the hot path uses, because routing a frame should
//! not allocate; the owned view is what the session engine and the transports
//! use, because those keep messages past the life of the read buffer.

pub mod envelope;

pub use envelope::{
    DecodeError, Envelope, Frame, MessageKind, Method, Outcome, WireError, WireId, WireIdError,
    decode,
};

/// Re-exported so this module is the front door for the JSON-RPC concept.
///
/// [`crate::domain::RequestId`] and [`crate::domain::ir::IrErrorCode`] predate
/// this module and are used by the IR, so they are not moved here — a caller
/// that thinks of "JSON-RPC" should find the id and the codes through the
/// module that owns the concept, and a caller that is holding an `IrRequest`
/// should not have to learn a second path to the same type.
pub use crate::domain::RequestId;
pub use crate::domain::ir::IrErrorCode;
