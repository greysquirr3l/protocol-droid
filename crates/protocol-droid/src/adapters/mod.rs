//! Adapters: the boundary between the core and the outside world.
//!
//! Driving adapters are entry points. Driven adapters are what the core calls
//! out to. Both are re-exported here so the split is visible in the module tree
//! rather than only in a directory listing.
//!
//! Adapters depend inward. They may import from `application` and `domain`;
//! neither of those may import from here.

pub mod primary;
pub mod secondary;
