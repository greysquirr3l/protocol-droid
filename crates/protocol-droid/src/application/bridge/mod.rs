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

pub use content::{ToolResultCodec, Translated, Translator};
pub use downstream::{Downstream, DownstreamConfig, DownstreamError, Identity};
pub use forward::{Bridge, BridgeError, BridgePeer, Forwarded, SendFuture};
pub use serve::{BridgeLoop, LoopError, LoopReport, Wiring};
pub use unsolicited::Unsolicited;
pub use upstream::{BridgeClientError, OpenedUpstream, Upstream, UpstreamError, run_client};

/// Settles the downstream leg of a handshake.
///
/// Re-exported beside the rest so the two halves of a bridge's negotiation are
/// reachable together: this needs an upstream [`Agreement`](crate::domain::Agreement),
/// which is what the client half of the connection produced.
pub use downstream::negotiate as negotiate_downstream;
