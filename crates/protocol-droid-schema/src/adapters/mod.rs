//! Adapters: the boundary between the schema core and the outside world.
//!
//! Driving adapters are the entry points. Driven adapters are what the core
//! calls out to. Both are re-exported here so the split is visible in the
//! module tree rather than only in a directory listing.

pub mod primary;
pub mod secondary;
