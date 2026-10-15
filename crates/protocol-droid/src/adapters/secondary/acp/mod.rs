//! Driven adapters for the ACP (Agent Client Protocol) module.
//!
//! The only adapter in this module today is the stdio
//! peer: a process spawn, a newline-delimited JSON-RPC
//! envelope read loop, and a write side that hands
//! envelopes to a writer task. The peer is the seam a
//! future application layer dispatches `initialize`
//! (and the session lifecycle methods) through.
//!
//! # Why a single peer, not a transport trait
//!
//! The transport shape is fixed for v1.25.0: stdio,
//! newline-delimited JSON-RPC 2.0. The peer is the
//! concrete implementation, not an abstraction over
//! "a transport", because no second transport exists
//! for ACP today and a trait is the abstraction the
//! second transport would earn. Adding a trait now
//! is the kind of speculative abstraction the rules
//! in `AGENTS.md` warn against: it is easy to
//! invent and costs the second transport a refactor.

#![cfg(feature = "acp-experimental")]

mod error;
mod peer;
mod reply;

pub use error::AcpPeerError;
pub use peer::{AcpStdioPeer, SpawnSpec};
pub use reply::AcpReply;
