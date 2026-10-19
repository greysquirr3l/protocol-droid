//! Schema-to-Rust generation for one MCP revision.
//!
//! Split into two modules because the two halves fail differently and cost
//! differently:
//!
//! - [`schema_model`](crate::generator::schema_model) parses `schema.json` into a typed model and rejects any
//!   keyword it does not understand. It is the part that must be *complete* —
//!   a keyword it silently drops becomes a wire type that quietly disagrees
//!   with the spec.
//! - [`emit`](crate::generator::emit) turns that model into Rust source. It is a pure function of the
//!   model, which is what makes the generated output testable without touching
//!   the filesystem.
//!
//! # Why the module is exempt from `dead_code`
//!
//! The consumer of this module is `build.rs`, which compiles the same two
//! files through `#[path]` and runs them. A library build — and a library
//! *test* build, where an integration test does not count as a caller under
//! this project's lint profile — therefore sees every item here as unused.
//!
//! The expectation is scoped to `test` only, because a plain library build does
//! not report the lints at all: `pub` items are reachable from a dependent
//! crate, so the rule that fires here is `dead_code_pub_in_binary`, which the
//! project denies. Scoping it to `test` is what makes the expectation *true* in
//! every configuration — an unsatisfied `expect` is itself an error under
//! `-D warnings`, so over-broad suppression would fail the build differently.

pub mod emit;
pub mod schema_model;
