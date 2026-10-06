//! The bridge use case.
//!
//! One module today. It is a directory because the next two things to land —
//! the replayable forwarding path and the lossless translation of
//! `ToolResult` across revisions — belong here rather than in a handler.

mod forward;

pub use forward::{Bridge, BridgeError, BridgePeer, Forwarded};
