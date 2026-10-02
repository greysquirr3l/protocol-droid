//! Application layer: use cases over the schema domain objects.
//!
//! Each module here owns its own narrow port trait rather than sharing a `ports`
//! module, following the workspace-wide layout.

pub mod verify_vendored_specs;

pub use verify_vendored_specs::{VerifyError, verify_vendored_specs};
