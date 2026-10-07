//! The bridge use case.
//!
//! Two modules today. It is a directory because the next two things to land —
//! the replayable forwarding path and the lossless translation of
//! `ToolResult` across revisions — belong here rather than in a handler.

mod forward;
mod serve;

pub use forward::{Bridge, BridgeError, BridgePeer, Forwarded, SendFuture};
pub use serve::{BridgeLoop, LoopError, LoopReport};
