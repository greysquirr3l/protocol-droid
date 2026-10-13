//! The bridge use case.
//!
//! Five modules, each one part of a bridge that would otherwise be one
//! oversized file: the lossless translation of `ToolResult` across revisions,
//! the downstream handshake a bridge owes the client in front of it, the
//! forwarding path itself, the loop that drives it, and the out-of-band frames
//! neither side asked for.

mod content;
mod downstream;
mod forward;
mod serve;
mod unsolicited;
mod upstream;

#[cfg(feature = "a2a")]
mod a2a_mcp_codec;
#[cfg(feature = "a2a")]
mod cross_protocol;

pub use content::{ToolResultCodec, Translated, Translator};
pub use downstream::{Downstream, DownstreamConfig, DownstreamError, Identity};
pub use forward::{Bridge, BridgeError, BridgePeer, Forwarded, SendFuture};
pub use serve::{BridgeLoop, LoopError, LoopReport, Wiring};
pub use unsolicited::Unsolicited;
pub use upstream::{BridgeClientError, OpenedUpstream, Upstream, UpstreamError, run_client};

#[cfg(feature = "a2a")]
pub use a2a_mcp_codec::A2aMcpCodec;
#[cfg(feature = "a2a")]
pub use cross_protocol::{
    CrossProtocolCodec, CrossProtocolError, ProtocolSide, RequestCodecReport, RequestCodecStep,
};

/// Settles the downstream leg of a handshake.
///
/// Re-exported beside the rest so the two halves of a bridge's negotiation are
/// reachable together: this needs an upstream [`Agreement`](crate::domain::Agreement),
/// which is what the client half of the connection produced.
pub use downstream::negotiate as negotiate_downstream;

#[cfg(feature = "a2a")]
pub use crate::domain::bridge::a2a_mcp::{
    a2a_message_to_mcp_content, a2a_message_to_mcp_text, a2a_skill_to_mcp_tool,
    mcp_content_to_a2a_part, CrossProtocolReport, Direction, Protocol, Step, StepKind,
};
