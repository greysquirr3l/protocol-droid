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
//! is ever asked to trust — the vendored revisions never say otherwise, and
//! 2026-07-28 removes the header outright (SEP-2567).
//!
//! A deployment that does want ids a peer cannot enumerate from one it has seen
//! should use `adapters::secondary::id_source::RandomIds`, behind the
//! `random-ids` feature. A path rather than an intra-doc link, because that
//! module is optional and this one is not. It is not the default because a
//! single-process embedding is better served by a reproducible counter, and this
//! is a port precisely so it can have one.

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
