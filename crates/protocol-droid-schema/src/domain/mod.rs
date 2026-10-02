//! Domain layer: pure value objects describing an MCP spec revision.
//!
//! Nothing in here performs I/O, depends on a framework, or carries behaviour
//! that needs a runtime. Verification of the vendored tree is pure too: it takes
//! a digest source as a parameter, and [`crate::adapters::secondary`] supplies
//! the one that touches the filesystem.

pub mod manifest;
