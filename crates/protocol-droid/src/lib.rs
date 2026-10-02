//! The `protocol-droid` library: one canonical IR, many per-revision codecs.
//!
//! Application code in this crate speaks a version-agnostic superset IR. Each
//! per-revision codec lifts wire messages into that IR on the way in and
//! lowers them back to the wire on the way out, for whichever revision was
//! negotiated with the peer. Every feature-specific path consults the session's
//! negotiated `FeatureProfile` before it runs.
//!
//! The layer split is the project's hexagonal layout:
//!
//! - [`domain`] — value objects, aggregates and pure functions. No I/O, no
//!   runtime, no framework derives.
//! - [`application`] — one module per use case. Each declares its own narrow
//!   port trait inline, beside the handler that consumes it.
//! - [`adapters::primary`] — driving adapters: the server transports.
//! - [`adapters::secondary`] — driven adapters: the client transports, upstream
//!   peers, clocks, RNG and the filesystem.
//!
//! There is deliberately no `ports` module. Port traits belong to the consumer,
//! so they live in the `application` module that needs them, and are never
//! collected into a shared module.

pub mod adapters;
pub mod application;
pub mod domain;
