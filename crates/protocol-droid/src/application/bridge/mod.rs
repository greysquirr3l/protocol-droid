//! The bridge use case.
//!
//! Three modules today. It is a directory because the remaining piece — the
//! lossless translation of `ToolResult` across revisions — needs its own file
//! rather than another arm in the forwarding path.

mod content;
mod forward;
mod serve;
mod unsolicited;

pub use content::{ToolResultCodec, Translated, Translator};
pub use forward::{Bridge, BridgeError, BridgePeer, Forwarded, SendFuture};
pub use serve::{BridgeLoop, LoopError, LoopReport};
pub use unsolicited::Unsolicited;
