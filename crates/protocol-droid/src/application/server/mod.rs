//! The server API: what a handler is given, and what it is allowed to ask for.
//!
//! # What lives here
//!
//! [`context::RequestContext`] is the argument every tool, resource and prompt
//! handler receives. It carries the request's identity — id, cancellation
//! token, progress token — and holds the [`context::PeerSession`] that
//! negotiated it. [`context::CallbackExchange`] and the frame sink are declared
//! here, in the module that consumes them, rather than collected into a shared
//! ports module.
//!
//! [`builder::Server`] is the value a user of this crate builds: a registry, a
//! set of tool handlers, and a name. It binds nothing and runs nothing — the
//! transports in T13 and T14 give it somewhere to run, and until then a test
//! can drive it in-process.
//!
//! [`error::GatedError`] is what a handler gets when it asks for something this
//! session did not negotiate.
//!
//! [`dispatch::Dispatcher`] is the method table: it turns a peer's method name
//! into a call on [`builder::Server`], and is what a transport holds once the
//! frames are decoded. It is separate from the builder because describing a
//! server and routing a request against it are different jobs, and only the
//! second one is I/O-shaped.
//!
//! # The boundary this module sits on
//!
//! The session engine below in [`super::session`] is protocol-neutral: it holds
//! an opaque profile handle and never names a version, so an A2A or ACP session
//! can use it unchanged. This module is the MCP-facing facade over that core,
//! and it is allowed to speak MCP because a user of a library does not want to
//! be handed an opaque handle when asking "can I elicit here?" is the question
//! they actually have. The core stays neutral; the facade does not have to.

pub mod builder;
pub mod context;
pub mod dispatch;
pub mod error;
pub mod result;

pub use builder::{BoxFuture, DispatchError, Server, ServerBuilder, ToolHandler};
pub use context::{CallbackExchange, DynExchange, ExchangeError, PeerSession, RequestContext};
pub use dispatch::Dispatcher;
pub use error::GatedError;
pub use result::OutcomeError;
