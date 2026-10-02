//! Procedural macros for the `protocol-droid` toolkit.
//!
//! This crate is separate from `protocol-droid` because a proc-macro crate is
//! compiled for the host and consumed only by other crates, so it cannot be a
//! module of the library that would use it.
//!
//! It intentionally carries no `domain`, `application` or `adapters` modules.
//! The other crates in this workspace are built around a hexagonal layering,
//! and that layering describes where business logic lives. This crate has no
//! business logic: it is pure tooling that runs at compile time. Giving it four
//! empty directories to satisfy a pattern would be cargo cult, and would hide
//! the fact that the layout is chosen per crate rather than imposed blindly.
//!
//! The attribute macros on the server and client APIs land with T12.
