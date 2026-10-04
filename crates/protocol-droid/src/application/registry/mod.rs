//! The use case: a set of tools, resources, templates and prompts that changes
//! while sessions are using it.
//!
//! # The three concerns, and which file owns each
//!
//! | File | Concern |
//! |---|---|
//! | [`store`] | The publish. Copy the live snapshot, change one thing, swap. |
//! | [`subscriber`] | The fan-out, gated on each session's negotiated profile. |
//! | [`listing`] | The read: one snapshot filtered for one session. |
//!
//! The split is the same one T10's engine makes. A use case is a sequence of
//! steps, and each step that could be described on its own gets a file, rather
//! than three methods on one type that a reader has to hold in their head
//! together.
//!
//! # What the swap means for a running tool
//!
//! An in-flight `tools/call` completes against the snapshot it resolved
//! against, even if the tool is unregistered a microsecond later. Nothing here
//! can change that: the reader holds an owned `Arc<Snapshot>`, so there is no
//! borrow for a publish to wait on, and blocking a publish until every running
//! tool finished would let one slow tool stall every registration on the
//! server. The alternative — cancelling in-flight calls on unregister — is a
//! policy question, and it is not this module's to answer.
//!
//! # Where the protocol lives
//!
//! Nothing here names a method, a notification, or a revision. The
//! [`subscriber::Subscriber`] is given a [`Feature`](crate::domain::Feature)
//! and a method name by whoever wires the server up, because which
//! notification means "the tool list changed" and which capability has to be
//! advertised for it to be welcome are facts about a protocol, and this module
//! is the part that would have to work for A2A as well as for MCP.
//!
//! # Examples
//!
//! ```
//! use protocol_droid::application::registry::RegistryStore;
//! use protocol_droid::domain::registry::{AuthContext, Primitive, PrimitiveKind, Tool};
//! use protocol_droid::domain::FeatureProfile;
//!
//! let store = RegistryStore::new();
//! store.register(Primitive::from(Tool::new("echo", r#"{"type":"object"}"#)?));
//!
//! let session = AuthContext::unrestricted("alice");
//! let listing = store.list_visible(&session, &FeatureProfile::all(), PrimitiveKind::Tool);
//!
//! assert_eq!(listing.visible().len(), 1);
//! assert_eq!(listing.hidden(), 0);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod listing;
pub mod store;
pub mod subscriber;

pub use listing::Listing;
pub use store::{Published, RegistryStore};
pub use subscriber::{Fanout, Subscriber, SubscriberSet};
