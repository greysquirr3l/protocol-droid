//! The bridge use case.
//!
//! Three modules today. It is a directory because the remaining piece — the
//! lossless translation of `ToolResult` across revisions — needs its own file
//! rather than another arm in the forwarding path.

mod content;
mod downstream;
mod forward;
mod serve;
mod unsolicited;

pub use content::{ToolResultCodec, Translated, Translator};
pub use downstream::{Downstream, DownstreamConfig, DownstreamError, Identity};
pub use forward::{Bridge, BridgeError, BridgePeer, Forwarded, SendFuture};
pub use serve::{BridgeLoop, LoopError, LoopReport};
pub use unsolicited::Unsolicited;

/// Settles the downstream leg of a handshake.
///
/// Named here so the two halves of a bridge's negotiation are reachable
/// together: [`negotiate`] needs an upstream [`Agreement`](crate::domain::Agreement),
/// which is what the client half of the connection produced.
pub use downstream::negotiate as negotiate_downstream;
