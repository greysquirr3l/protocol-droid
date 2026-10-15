//! ACP's application layer — use cases the wire
//! codec routes into.
//!
//! Each handler is the *consumer* of a narrow port
//! trait declared here. The port trait's return
//! type is a domain error, not an infrastructure
//! one, so the handler can be tested without a
//! socket and the adapter can map `tokio` /
//! serialization failures into the domain shape
//! at the boundary.
//!
//! Today: the dispatch trait and its typed
//! wrappers — the consumer's extension point for
//! running an ACP agent. Future increments slot
//! in here: a per-revision wire codec that turns
//! a `Value` payload into a typed
//! `Envelope` (the `domain::acp::Envelope` work is
//! already underway as part of T29's
//! "per-revision wire codec" follow-on), a
//! conformance runner modelled on
//! [`application::a2a::conformance`](super::a2a::conformance),
//! and an `acp_agent` example.

pub(crate) mod dispatch;
pub(crate) mod typed_dispatch;

pub use dispatch::{AcpError, AcpRequestContext, AcpRequestHandler};
pub use typed_dispatch::AcpRequestHandlerExt;
