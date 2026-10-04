//! Where a session's identity comes from.
//!
//! # Why a session needs an id at all
//!
//! Two things need to say which session they are talking about: the event log,
//! which interleaves the history of every open session, and the redaction rule
//! that says a session id is never logged in full. Neither is possible without
//! one, and both are why it is a value rather than an `Arc<Self>` a task can
//! reach through.
//!
//! # What this does not promise
//!
//! Uniqueness *within a process*. A session id correlates one process's work
//! with itself; it is not a capability, not a secret, and not something a peer
//! is ever asked to trust. A deployment that needs ids unique across processes
//! seeds its own source with whatever entropy it already has — which is why
//! this is a port rather than a concrete type the engine constructs for itself.

use crate::domain::session::SessionId;

/// Mints session identities.
///
/// Declared by the engine, its only consumer, and consumed through
/// `impl IdSource`. One method, because a source that also answered "how long
/// is this session" would be two responsibilities in one adapter.
pub trait IdSource {
    /// A fresh identity.
    ///
    /// Must not repeat an id this source has already returned, for as long as
    /// either session can still appear in the event log.
    fn next_session_id(&self) -> SessionId;
}
