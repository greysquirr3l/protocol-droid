//! Per-revision Model Context Protocol wire types.
//!
//! Every published spec revision is vendored and generated from its official
//! `schema.json`, and the resulting types all coexist in this crate. They are
//! never mutually exclusive: which revision a process speaks is decided at
//! runtime, by negotiation, not by a feature flag chosen at compile time.
//!
//! This crate is data. It performs no I/O, holds no session state, and makes no
//! decisions. Lifting wire messages into the version-agnostic IR and lowering
//! them back is the job of the `protocol-droid` crate, which depends on this
//! one.
//!
//! The layer split below is the project's hexagonal layout, shared by every
//! crate in the workspace:
//!
//! - [`domain`] — pure value objects describing a spec revision.
//! - [`application`] — use cases that operate on those value objects.
//! - [`adapters::primary`] — driving adapters: the generators that read a
//!   vendored `schema.json` and emit Rust source.
//! - [`adapters::secondary`] — driven adapters: the filesystem and the vendored
//!   specification sources they read from.
//!
//! There is deliberately no `ports` module. A port trait is owned by the
//! handler that needs it and lives beside that handler, so a shared port
//! module is never the place for one.

pub mod adapters;
pub mod application;
pub mod domain;
