//! The stdio **client**: a child MCP server, spawned and supervised.
//!
//! # What this is
//!
//! [`primary::stdio`](crate::adapters::primary::stdio) is the byte layer of a
//! stdio conversation, and it is deliberately agnostic about where the bytes
//! come from: [`StdioSource<R>`] and [`StdioSink<W>`] are generic over
//! `AsyncRead` and `AsyncWrite` precisely so that a process's pipes can be
//! dropped into them. This module supplies what the byte layer was missing —
//! the other end of the conversation. [`StdioClient`] starts a child server,
//! hands its stdout to the source and its stdin to the sink, drains its stderr
//! somewhere that is not the protocol, and takes the process down with it.
//!
//! There is no second framing and no second encoder here, and that is the
//! whole argument for reusing those two types rather than writing a client
//! transport of one's own. Framing is where the security properties live — the
//! ceiling, the single delimiter, the decision that a frame the peer wrote and
//! did not terminate is dropped rather than reported — and a second
//! implementation would be a second set of those properties to get right. The
//! request and reply *documents* in [`request`] are the exception, and they are
//! documents rather than framing: the shapes [`reply`](crate::adapters::primary::stdio::reply)
//! writes and this reads are one wire format with one serialiser, not two.
//!
//! # The two things that are easy to get wrong
//!
//! **Kill-on-drop is opt-in.** `tokio::process::Child` does *not* kill the
//! process it is dropped with. Without an explicit `kill_on_drop(true)`, a
//! client that returns an error half way through a handshake leaves a server
//! running with nobody reading its stdout — an orphan holding a pipe, holding
//! whatever the user asked it to do. [`child::spawn`] is where the flag is set,
//! and it is set on the one path that can spawn a child.
//!
//! **The child's stderr is not the protocol.** [`Diagnostics`] is bound to
//! stderr and has no constructor that can be pointed at stdout, for the same
//! reason the server's does not: on stdio, stdout is the conversation. The
//! forwarder in [`child::forward_stderr`] moves those bytes across one line at
//! a time and nowhere else, so a server that logs to stderr cannot corrupt a
//! frame — and, just as importantly, a log line cannot be mistaken for a
//! frame the peer failed to parse.
//!
//! # The two `None`s
//!
//! [`StdioSource::next_frame`] answers `Ok(None)` when the peer has closed, and
//! the decoder underneath it answers `Ok(None)` when a line has no terminator
//! *yet* — opposites, and forwarding the second up would turn "keep reading"
//! into "the child died" and "the child died" into "read again". The source
//! already gets this right; this module passes its answer through unaltered
//! and names the distinction at every call site, because the two failures it
//! produces (a child that exited early, and a child that has not answered yet)
//! are the two a caller most needs told apart.
//!
//! # Where the conversation is
//!
//! The bytes stay in the byte layer. What this module adds is the lifecycle —
//! [`spec::ChildSpec`] for the program, [`child`] for the process, and
//! [`client::StdioClient`] for the exchange — plus the request documents in
//! [`request`], which are the client's mirror of the server's
//! [`reply`](crate::adapters::primary::stdio::reply) module.

pub mod child;
pub mod client;
pub mod request;
pub mod spec;

#[cfg(test)]
mod testing;

pub use child::SpawnError;
pub use client::{ClientConfig, StdioClient, StdioClientError};
pub use request::{RequestError, notification_document, reply_result, request_document};
pub use spec::{ChildSpec, ChildSpecError};
