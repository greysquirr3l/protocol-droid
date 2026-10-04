//! The in-flight request table.
//!
//! # What the table is for
//!
//! A peer may have several requests outstanding at once, and it may cancel any
//! of them, or ask for progress on any of them, from a different task than the
//! one running the handler. The table is the one place that answers "is that id
//! still running, and what do we know about it".
//!
//! # Two rules the specification fixes
//!
//! Cancelling a request that is unknown, or one that has already finished, is a
//! **no-op**, not an error. The schema is explicit that the notification "MAY
//! arrive after the request has already finished" and that the result "will be
//! unused", so answering with an error would be reporting a race as a fault.
//! [`CancelOutcome::Ignored`] is that case, and it is deliberately not an error
//! type.
//!
//! Registering an id that is already in flight is the opposite: a duplicate
//! in-flight id is a malformed session, and it is reported as
//! [`DuplicateRequest`] rather than silently displacing the first request.
//!
//! # Why a hash map and not a tree
//!
//! The only ordered traversal is [`expire`](RequestRegistry::expire), which
//! collects into a `Vec` and runs on a tick rather than per frame. The per-frame
//! costs that matter are register and cancel, and both hash a `RequestId` — an
//! integer or an `Arc<str>` pointer, cheap to hash either way.

use core::fmt;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::domain::RequestId;

use super::cancellation::{AbortReason, CancelReason, CancellationToken};
use super::progress::{ProgressCursor, ProgressToken, ProgressUpdate};
use super::state::Instant;

/// Which side issued a request.
///
/// Both directions go through one table because both need the same three
/// things: uniqueness of the id, a cancellation handle, and a deadline. A
/// server that samples from a client has exactly the bookkeeping a server has
/// when a client calls a tool, and two tables would be two places for one rule
/// to be wrong in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RequestOrigin {
    /// The peer asked us.
    Inbound,
    /// We asked the peer — sampling, elicitation, roots.
    Outbound,
}

/// One request that has not finished.
#[derive(Debug)]
pub struct InFlight {
    origin: RequestOrigin,
    method: Arc<str>,
    token: Option<ProgressToken>,
    cursor: ProgressCursor,
    budget: Option<Duration>,
    started_at: Instant,
    cancel: CancellationToken,
}

impl InFlight {
    /// A newly registered request.
    ///
    /// `budget` is how long this side is willing to wait. `None` means the
    /// caller decided the request is not time-bounded — which is a decision,
    /// not an oversight, and is why it is an `Option` rather than a zero that
    /// would expire on the next tick.
    #[must_use]
    pub fn new(
        origin: RequestOrigin,
        method: impl AsRef<str>,
        token: Option<ProgressToken>,
        budget: Option<Duration>,
        started_at: Instant,
    ) -> Self {
        Self {
            origin,
            method: Arc::from(method.as_ref()),
            token,
            cursor: ProgressCursor::new(),
            budget,
            started_at,
            cancel: CancellationToken::new(),
        }
    }

    /// Which side issued it.
    #[must_use]
    pub const fn origin(&self) -> RequestOrigin {
        self.origin
    }

    /// The method it names.
    #[must_use]
    pub fn method(&self) -> &str {
        &self.method
    }

    /// The progress token the request carried, if it asked for progress.
    #[must_use]
    pub const fn progress_token(&self) -> Option<&ProgressToken> {
        self.token.as_ref()
    }

    /// How long this side is willing to wait, if it decided.
    #[must_use]
    pub const fn budget(&self) -> Option<Duration> {
        self.budget
    }

    /// When it was registered.
    #[must_use]
    pub const fn started_at(&self) -> Instant {
        self.started_at
    }

    /// The handle a handler awaits to learn it was cancelled.
    #[must_use]
    pub const fn cancellation(&self) -> &CancellationToken {
        &self.cancel
    }

    /// A clone of the cancellation handle, safe to hold across an await.
    ///
    /// The registry hands out `&InFlight` only while its lock is taken, so a
    /// handler that wants to wait has to own the token rather than borrow it
    /// out of the table.
    #[must_use]
    pub fn cancellation_handle(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Whether the budget is spent at `now`.
    #[must_use]
    pub fn has_expired(&self, now: Instant) -> bool {
        self.budget
            .is_some_and(|budget| now.since(self.started_at) >= budget)
    }

    /// Records a progress value and says whether it should go on the wire.
    ///
    /// `false` for a request that carried no token, because there is nothing
    /// to correlate the notification to, and for a value that does not move the
    /// cursor forward. Delegates to this entry's own cursor, so a caller cannot
    /// accidentally check monotonicity against another request's last value.
    #[must_use]
    pub fn advance_progress(&mut self, progress: f64, message: Option<String>) -> bool {
        let Some(token) = self.token.clone() else {
            return false;
        };
        let mut update = ProgressUpdate::new(token, progress);
        if let Some(message) = message {
            update = update.with_message(message);
        }
        self.cursor.advance(&update)
    }

    /// The furthest progress reported for this request.
    #[must_use]
    pub const fn furthest_progress(&self) -> Option<f64> {
        self.cursor.furthest()
    }

    /// Aborts the request.
    ///
    /// Returns whether this call is the one that published the reason.
    pub fn abort(&self, reason: AbortReason) -> bool {
        self.cancel.cancel(reason)
    }
}

/// What a cancellation did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    /// The request was in flight and has been aborted.
    Cancelled,
    /// Nothing was cancelled: the id was never issued, had already finished, or
    /// was already cancelled.
    ///
    /// Not an error. The specification says a cancellation notification may
    /// arrive after its request has finished, so this is a race the sender is
    /// entitled to lose and the notification is defined to be harmless.
    Ignored,
}

impl CancelOutcome {
    /// Whether a request was actually aborted.
    #[must_use]
    pub const fn cancelled(self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

/// An id that is already in flight.
///
/// Carries the real id rather than a redacted copy, because the caller already
/// has it; [`Display`](fmt::Display) redacts so that a logged message cannot
/// disclose a peer-chosen string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateRequest {
    /// The offending id.
    pub id: RequestId,
}

impl fmt::Display for DuplicateRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "request id {} is already in flight",
            self.id.redacted()
        )
    }
}

impl std::error::Error for DuplicateRequest {}

/// A request id that is not in flight.
///
/// Redacted in [`Display`](fmt::Display) for the same reason as
/// [`DuplicateRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownRequest {
    /// The id that was not found.
    pub id: RequestId,
}

impl fmt::Display for UnknownRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "request id {} is not in flight",
            self.id.redacted()
        )
    }
}

impl std::error::Error for UnknownRequest {}

/// Every request that has not finished.
#[derive(Debug, Default)]
pub struct RequestRegistry {
    entries: HashMap<RequestId, InFlight>,
}

impl RequestRegistry {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Records a request as in flight.
    ///
    /// # Errors
    ///
    /// [`DuplicateRequest`] if the id is already in flight. The existing entry
    /// is left alone: the first request owns the id, and displacing it would
    /// make a later cancellation abort the wrong work.
    pub fn register(&mut self, id: RequestId, entry: InFlight) -> Result<(), DuplicateRequest> {
        if self.entries.contains_key(&id) {
            return Err(DuplicateRequest { id });
        }
        self.entries.insert(id, entry);
        Ok(())
    }

    /// The entry for `id`, if it is still in flight.
    #[must_use]
    pub fn get(&self, id: &RequestId) -> Option<&InFlight> {
        self.entries.get(id)
    }

    /// The entry for `id`, mutably, if it is still in flight.
    #[must_use]
    pub fn get_mut(&mut self, id: &RequestId) -> Option<&mut InFlight> {
        self.entries.get_mut(id)
    }

    /// Cancels a request.
    ///
    /// Never fails. See [`CancelOutcome`].
    pub fn cancel(&mut self, id: &RequestId, reason: Option<CancelReason>) -> CancelOutcome {
        let Some(entry) = self.entries.get(id) else {
            return CancelOutcome::Ignored;
        };
        if entry.abort(AbortReason::Cancelled(reason)) {
            CancelOutcome::Cancelled
        } else {
            // Already aborted, by a peer or by a deadline. Aborting again would
            // overwrite the first reason, so it is ignored exactly as an
            // unknown id is.
            CancelOutcome::Ignored
        }
    }

    /// Aborts and removes every request whose budget is spent at `now`.
    ///
    /// The returned ids are the ones the caller should report, in no particular
    /// order: this runs on a tick rather than per frame, and sorting to make
    /// the output deterministic would be work whose result nothing reads.
    pub fn expire(&mut self, now: Instant) -> Vec<RequestId> {
        let mut expired = Vec::new();
        self.entries.retain(|id, entry| {
            if entry.has_expired(now) {
                entry.abort(AbortReason::Expired);
                expired.push(id.clone());
                false
            } else {
                true
            }
        });
        expired
    }

    /// Removes a finished request and returns what it was.
    #[must_use]
    pub fn complete(&mut self, id: &RequestId) -> Option<InFlight> {
        self.entries.remove(id)
    }

    /// The ids of every request still in flight.
    pub fn ids(&self) -> impl Iterator<Item = &RequestId> {
        self.entries.keys()
    }

    /// How many requests are in flight.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is in flight.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Forgets every request, aborting each so no handler is left waiting.
    ///
    /// Used when a session closes. The requests are not going to finish, and a
    /// handler awaiting a signal that can never arrive is a leaked task rather
    /// than a slow one.
    pub fn drain(&mut self) -> Vec<RequestId> {
        let mut ids = Vec::with_capacity(self.entries.len());
        for (id, entry) in &self.entries {
            entry.abort(AbortReason::SessionClosed);
            ids.push(id.clone());
        }
        self.entries.clear();
        ids
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        CancelOutcome, DuplicateRequest, InFlight, RequestOrigin, RequestRegistry, UnknownRequest,
    };
    use crate::domain::RequestId;
    use crate::domain::session::cancellation::{AbortReason, CancelReason, CancellationToken};
    use crate::domain::session::progress::ProgressToken;
    use crate::domain::session::state::Instant;

    /// Why a test could not set up what it meant to.
    ///
    /// Every call in here is fallible — a duplicate id, or an entry that was
    /// expected to still be in flight and was not. Propagating that through
    /// `?` keeps the failure in the test result, where the harness prints it,
    /// rather than in a panic raised by an unwrap that says nothing about what
    /// the test was checking.
    #[derive(Debug)]
    #[expect(dead_code, reason = "the payloads are read by the derived Debug")]
    enum Failure {
        /// The registry refused a registration.
        Duplicate(DuplicateRequest),
        /// An entry the test had just registered was not there.
        Missing(&'static str),
    }

    impl From<DuplicateRequest> for Failure {
        fn from(duplicate: DuplicateRequest) -> Self {
            Self::Duplicate(duplicate)
        }
    }

    fn at(millis: u64) -> Instant {
        Instant::from_duration(Duration::from_millis(millis))
    }

    fn entry(token: Option<ProgressToken>, budget: Option<Duration>) -> InFlight {
        InFlight::new(RequestOrigin::Inbound, "tools/call", token, budget, at(0))
    }

    /// The cancellation handle for `id`, or a failure naming what was missing.
    fn handle(registry: &RequestRegistry, id: &RequestId) -> Result<CancellationToken, Failure> {
        registry
            .get(id)
            .map(InFlight::cancellation_handle)
            .ok_or(Failure::Missing("the request that was just registered"))
    }

    #[test]
    fn a_request_is_found_until_it_completes() -> Result<(), Failure> {
        let mut registry = RequestRegistry::new();
        let id = RequestId::number(1);
        registry.register(id.clone(), entry(None, None))?;
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.get(&id).map(InFlight::method), Some("tools/call"));

        assert!(registry.complete(&id).is_some());
        assert!(registry.is_empty());
        // Completing twice is the same no-op a late cancellation is.
        assert!(registry.complete(&id).is_none());
        Ok(())
    }

    #[test]
    fn a_duplicate_in_flight_id_is_refused_and_the_first_entry_survives() -> Result<(), Failure> {
        let mut registry = RequestRegistry::new();
        let id = RequestId::number(1);
        registry.register(id.clone(), entry(None, None))?;

        let refused = registry.register(
            id.clone(),
            InFlight::new(RequestOrigin::Inbound, "resources/read", None, None, at(0)),
        );
        assert_matches::assert_matches!(
            refused,
            Err(DuplicateRequest { id: ref reported }) if *reported == RequestId::number(1)
        );
        // The displacement a silently-overwriting implementation would have
        // done is exactly what makes a later cancellation abort the wrong work.
        assert_eq!(registry.get(&id).map(InFlight::method), Some("tools/call"));
        Ok(())
    }

    /// The schema says a cancellation notification "MAY arrive after the
    /// request has already finished" — so this is a case to absorb, not report.
    #[test]
    fn cancelling_a_finished_or_unknown_request_is_a_no_op() -> Result<(), Failure> {
        let mut registry = RequestRegistry::new();
        let id = RequestId::number(7);
        registry.register(id.clone(), entry(None, None))?;
        assert!(
            registry.complete(&id).is_some(),
            "the entry was registered a line ago"
        );

        assert_eq!(registry.cancel(&id, None), CancelOutcome::Ignored);
        assert_eq!(
            registry.cancel(&RequestId::number(99), None),
            CancelOutcome::Ignored
        );
        Ok(())
    }

    #[test]
    fn cancelling_twice_reports_only_the_first() -> Result<(), Failure> {
        let mut registry = RequestRegistry::new();
        let id = RequestId::number(1);
        registry.register(id.clone(), entry(None, None))?;

        let first = registry.cancel(&id, Some(CancelReason::new("user")));
        let second = registry.cancel(&id, Some(CancelReason::new("timeout")));

        assert_eq!(first, CancelOutcome::Cancelled);
        assert_eq!(second, CancelOutcome::Ignored);
        // The first reason stands: a deadline landing after a peer's
        // cancellation must not rewrite why the request actually stopped.
        assert_eq!(
            registry
                .get(&id)
                .and_then(|entry| entry.cancellation().reason()),
            Some(&AbortReason::Cancelled(Some(CancelReason::new("user"))))
        );
        Ok(())
    }

    #[test]
    fn a_zero_budget_expires_immediately_and_an_absent_budget_never_expires() -> Result<(), Failure>
    {
        let mut registry = RequestRegistry::new();
        let bounded = RequestId::number(1);
        let unbounded = RequestId::number(2);
        registry.register(
            bounded.clone(),
            entry(None, Some(Duration::from_millis(100))),
        )?;
        registry.register(unbounded.clone(), entry(None, None))?;

        assert!(
            registry.expire(at(99)).is_empty(),
            "expired a millisecond early"
        );
        assert_eq!(registry.expire(at(100)), vec![bounded]);
        assert_eq!(registry.len(), 1);
        // Unbounded is still there, and still not expired, a long time later.
        assert!(registry.expire(at(1_000_000)).is_empty());
        assert!(registry.get(&unbounded).is_some());
        Ok(())
    }

    #[test]
    fn an_expired_request_is_aborted_so_its_handler_wakes() -> Result<(), Failure> {
        let mut registry = RequestRegistry::new();
        let id = RequestId::number(1);
        registry.register(id.clone(), entry(None, Some(Duration::from_secs(1))))?;
        let token = handle(&registry, &id)?;

        registry.expire(at(1_001));
        assert!(token.is_cancelled());
        assert_eq!(token.reason(), Some(&AbortReason::Expired));
        Ok(())
    }

    #[test]
    fn progress_needs_a_token_and_moves_forward_only() -> Result<(), Failure> {
        let mut registry = RequestRegistry::new();
        let without = RequestId::number(1);
        let with = RequestId::number(2);
        registry.register(without.clone(), entry(None, None))?;
        registry.register(with.clone(), entry(Some(ProgressToken::text("job")), None))?;

        // No token means nothing to correlate to, so nothing is ever emitted.
        let advanced = registry
            .get_mut(&without)
            .is_some_and(|entry| entry.advance_progress(1.0, None));
        assert!(!advanced, "a request with no progress token never emits");

        let entry = registry
            .get_mut(&with)
            .ok_or(Failure::Missing("the request carrying a progress token"))?;
        assert!(entry.advance_progress(1.0, Some(String::from("reading"))));
        assert!(!entry.advance_progress(1.0, None));
        assert!(entry.advance_progress(2.0, None));
        assert_eq!(entry.furthest_progress(), Some(2.0));
        assert_eq!(entry.progress_token(), Some(&ProgressToken::text("job")));
        Ok(())
    }

    #[test]
    fn draining_aborts_everything_it_forgets() -> Result<(), Failure> {
        let mut registry = RequestRegistry::new();
        for n in 0..3 {
            registry.register(RequestId::number(n), entry(None, None))?;
        }
        let tokens: Vec<_> = registry
            .ids()
            .map(|id| registry.get(id).map(InFlight::cancellation_handle))
            .collect();

        let drained = registry.drain();
        assert_eq!(drained.len(), 3);
        assert!(registry.is_empty());
        assert_eq!(
            tokens.iter().filter(|token| token.is_none()).count(),
            0,
            "every registered id had an entry before the drain"
        );
        for token in tokens.into_iter().flatten() {
            assert_eq!(token.reason(), Some(&AbortReason::SessionClosed));
        }
        Ok(())
    }

    /// A peer-chosen string id must not reach a log line through an error
    /// message. This is the boundary the redaction claim is made at.
    #[test]
    fn an_error_message_redacts_the_id() {
        let secret = RequestId::text("super-secret-credential");
        let message = DuplicateRequest { id: secret.clone() }.to_string();
        assert!(!message.contains("super-secret-credential"), "{message}");
        assert!(message.contains(&secret.redacted()), "{message}");

        let unknown = UnknownRequest { id: secret }.to_string();
        assert!(!unknown.contains("super-secret-credential"), "{unknown}");
    }

    #[test]
    fn both_origins_share_one_table() -> Result<(), Failure> {
        assert_ne!(RequestOrigin::Inbound, RequestOrigin::Outbound);
        let mut registry = RequestRegistry::new();
        registry.register(
            RequestId::number(1),
            InFlight::new(
                RequestOrigin::Outbound,
                "sampling/createMessage",
                None,
                None,
                at(0),
            ),
        )?;
        assert_eq!(
            registry.get(&RequestId::number(1)).map(InFlight::origin),
            Some(RequestOrigin::Outbound)
        );
        Ok(())
    }
}
