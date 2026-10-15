//! `AcpReply` — the raw JSON-RPC 2.0 reply shape.
//!
//! # Why the peer carries a raw reply, not a typed one
//!
//! The peer is the transport layer. It does not
//! know that the wire's `result` field for an
//! `initialize` call is a typed
//! [`InitializeResponse`](crate::domain::acp::initialize::InitializeResponse);
//! the consumer is the one that deserialises the
//! raw `serde_json::Value` into the concrete type
//! the method returns. This is the same shape the
//! protocol-neutral core's
//! [`Envelope`](crate::domain::jsonrpc::envelope::Envelope)
//! has: the wire is bytes, the consumer is the
//! decoder.
//!
//! The peer's job is to *return* the value the
//! agent sent. The consumer decodes.

use serde::Deserialize;

/// A JSON-RPC 2.0 reply: the agent's answer to a
/// request.
///
/// The wire form has either a `result` (success)
/// or an `error` (failure), never both, never
/// neither. The typed form is the same: a
/// `Result<AcpReply, AcpPeerError>` from the
/// peer's `send_request` already encodes the
/// success/failure split. The peer's
/// [`JsonRpc`](super::error::AcpPeerError::JsonRpc)
/// variant is the failure case, so the
/// `AcpReply` value the consumer receives is
/// always the success case.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcpReply {
    /// The reply's JSON-RPC 2.0 protocol field.
    /// Always `"2.0"`. Carried through the
    /// decoder as a sanity check on the agent's
    /// framing; the peer does not act on it.
    pub jsonrpc: String,
    /// The id the request carried. The peer
    /// uses the id to correlate; the consumer
    /// does not need to look at it.
    pub id: serde_json::Value,
    /// The success result. The shape depends
    /// on the method; the peer does not
    /// decode it.
    pub result: serde_json::Value,
}
