//! Driving adapters for the schema crate.
//!
//! The schema-to-Rust emitter moved to [`crate::generator`] when it became a
//! build-time input rather than something the library calls: `build.rs` runs
//! it, and the library compiles the same source so its output is testable here.
//! Keeping it under `adapters/primary/` would have implied the library drives
//! it, which it does not.

// Intentionally empty for now. The one driving adapter this crate had was the
// generator, and it lives in `crate::generator` for the reason above.
