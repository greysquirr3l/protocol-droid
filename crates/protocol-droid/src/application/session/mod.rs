//! The session engine: one use case, assembled from a lifecycle and three ports.
//!
//! # What lives here
//!
//! The engine owns a [`Session`](crate::domain::session::Session) and a
//! [`RequestRegistry`](crate::domain::session::RequestRegistry), asks a
//! [`Lifecycle`] what each message means, and records what happened on an
//! [`EventLog`]. It reaches the outside world through the traits it declares
//! itself — [`Clock`], [`IdSource`], [`EventLog`], and a frame sink once the
//! engine needs to write. None of them is implemented here. A transport
//! implements the sink; a test implements the other three in three lines.
//!
//! # Why the engine knows nothing about MCP
//!
//! Every protocol-specific decision is a call into [`Lifecycle`]. The engine
//! holds the negotiated profile and version as opaque values, passes the
//! profile back whenever it needs a judgement, and renders the version into a
//! string for the event log — it never inspects either. That is the whole
//! reason [`Lifecycle`] has associated types instead of naming
//! `FeatureProfile` and `ProtocolVersion`: an A2A session carries
//! `Major.Minor` and a profile of a completely different shape, and it drops
//! in as a second implementation of the same trait rather than as a fork of
//! this module.
//!
//! # Why time is a port and not a call
//!
//! The engine decides that a request has outlived its budget, and that
//! decision is arithmetic over two readings of the same clock. Taking the
//! second reading from a global would mean a test could not set up a request
//! that started ninety seconds ago without sleeping for ninety seconds. So the
//! clock is injected, the reading is a `Duration` from an origin the adapter
//! picked, and nothing in this module calls a clock function.

pub mod clock;
pub mod engine;
pub mod event_log;
pub mod frame_sink;
pub mod id_source;
pub mod lifecycle;

pub use clock::Clock;
pub use engine::{Dispatch, EngineError, Refusal, SessionEngine};
pub use event_log::{EventLog, NullEventLog, SessionEvent};
pub use frame_sink::{DynFrameSink, FrameSink, SinkError};
pub use id_source::IdSource;
pub use lifecycle::{
    Classification, Declaration, DeclarationError, Lifecycle, MetaView, Resolution,
};
