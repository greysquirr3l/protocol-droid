//! The UTCP use cases: publish a manual, and plan a call.
//!
//! # Where UTCP sits in this crate
//!
//! The crate's core is a canonical IR with per-revision codecs on either
//! side of it, and the cross-protocol bridge composes two codecs into a
//! translation between two dialects. UTCP is not a dialect: it has no
//! envelope, no session, and no revision to negotiate. It is a *document* —
//! a list of tools and how to call them — and the calls it describes are
//! made in the tool's own protocol.
//!
//! So the two use cases here are not "handle a UTCP message". They are:
//!
//! - [`publish()`] — turn this server's tools into a manual a UTCP client can
//!   read.
//! - [`call`](mod@call) — turn a manual's tool and a set of arguments into a concrete
//!   call, in the tool's own protocol.
//!
//! # Why the planning is here and the doing is in an adapter
//!
//! [`call::plan_call`] decides *what* the request is: which URL, which
//! headers, which body, which credential. That decision is a pure function
//! of the tool, the arguments, and the variables, so it is testable without
//! a socket and it is the same decision every adapter makes. The adapter
//! then does the I/O — and the I/O is where the transport differences live:
//! an HTTP request is a request, an MCP call is a session and a
//! `tools/call`.
//!
//! # The three directions
//!
//! | Direction | What it is | Where it lives |
//! |---|---|---|
//! | Read a manual | fetch a provider's document | `adapters/secondary/utcp` |
//! | Publish a manual | serve this server's tools | [`publish()`], `adapters/primary/utcp` |
//! | Call a tool | make the call a template describes | [`call`](mod@call), `adapters/secondary/utcp` |

pub mod call;
pub mod publish;
pub mod register;

pub use call::{CallError, HttpAuth, HttpCall, McpCall, ToolCall, plan_call};
pub use publish::{PublishCommand, publish};
pub use register::{RefusalReason, RefusedTool, Registration, register};
