//! The registry's pure half: what a primitive _is_, who may see it, and the
//! immutable set a publish hands to readers.
//!
//! Nothing here touches an `ArcSwap`, a lock, or a clock. The types in this
//! module are the values that get stored, the rule that decides visibility, and
//! the snapshot they are packed into — the decision half of the problem, with
//! the concurrency half left to
//! [`application::registry`](crate::application::registry).
//!
//! The split is load-bearing in one direction and cosmetic in the other.
//! [`is_visible`] must be pure because the whole feature-gating capability is
//! a claim that a filter is a function of its inputs, and a function that can
//! observe a lock's state is not one. `Snapshot` has no mutating method
//! because the publish is the transaction boundary, and a value that can be
//! edited in place has no boundary at all.
//!
//! # Examples
//!
//! ```
//! use protocol_droid::domain::registry::{
//!     AuthContext, Primitive, Registered, Scope, ScopeSet, Snapshot, Tool, is_visible,
//! };
//! use protocol_droid::domain::FeatureProfile;
//!
//! let mut builder = Snapshot::builder();
//! builder.insert(Primitive::from(Tool::new("echo", r#"{"type":"object"}"#)?));
//! let snapshot = builder.build();
//!
//! // A session holding every scope sees a tool that asks for none.
//! let alice = AuthContext::unrestricted("alice");
//! let registered = Registered::from(Primitive::from(
//!     Tool::new("echo", r#"{"type":"object"}"#)?,
//! ));
//! assert!(is_visible(&alice, &FeatureProfile::all(), &registered.meta()));
//!
//! // A session holding no scope does not see a tool that asks for one.
//! let restricted = AuthContext::new("bob", ScopeSet::empty());
//! let guarded = registered
//!     .clone()
//!     .requiring_scope(Scope::Admin)
//!     .requiring(FeatureProfile::all().as_set());
//! assert!(!is_visible(&restricted, &FeatureProfile::all(), &guarded.meta()));
//! # Ok::<(), protocol_droid::domain::ir::JsonObjectError>(())
//! ```

pub mod auth;
pub mod primitive;
pub mod snapshot;
pub mod visibility;

/// What a test in this module reports when it is the one complaining.
///
/// The fallible calls here answer two unrelated error types —
/// [`JsonObjectError`](crate::domain::ir::JsonObjectError) from parsing a tool
/// schema, and a test's own `&str` complaint. A test that has to choose one
/// type can only report half its failures, so it takes a `Box` and lets `?`
/// convert.
#[cfg(test)]
pub(crate) type TestError = Box<dyn core::error::Error>;

pub use auth::{AuthContext, Scope, ScopeSet};
pub use primitive::{
    Primitive, PrimitiveId, PrimitiveKind, Prompt, Registered, Resource, ResourceTemplate, Tool,
};
pub use snapshot::{Snapshot, SnapshotBuilder};
pub use visibility::{is_visible, missing_features, missing_scopes};
