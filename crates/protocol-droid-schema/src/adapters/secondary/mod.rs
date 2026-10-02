//! Driven adapters for the schema crate.
//!
//! Filesystem access to the vendored specification sources. Verification logic
//! lives in [`crate::domain::manifest`]; this module only supplies the I/O that
//! logic asks for.

pub mod spec_digest;

pub use spec_digest::{SpecDigestReader, hex_digest};
