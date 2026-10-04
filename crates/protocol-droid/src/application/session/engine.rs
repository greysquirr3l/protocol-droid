//! The engine: a lifecycle, a state machine, and a table of what is running.
//!
//! # What this owns
//!
//! A [`Session`] and a [`RequestRegistry`], plus the negotiated
//! [`Resolution`] a [`Lifecycle`] produced. Everything protocol-specific is a
//! call *into* the lifecycle; the two associated types are stored opaquely and
//! handed back whenever a judgement is needed. Nothing here names
//! `ProtocolVersion`, `FeatureProfile`, or a method of any protocol.
//!
//! # Single-writer, and why that is a decision
//!
//! Every method here takes `&mut self`. The engine is single-writer on
//! purpose: the session's phase, the negotiated profile, and the in-flight
//! table have to move together, and a lock would let two of them move
//! separately. A transport that reads frames on one task and runs handlers on
//! others puts the engine behind a `Mutex` and calls the `&mut` methods
//! between polls — which is why [`dispatch`](SessionEngine::dispatch) hands the
//! handler its own [`CancellationToken`] *before* it awaits, so a canceller
//! never needs the engine to stay unlocked across the wait.
//!
//! What the engine refuses to be is a `Send`-and-`Sync` bag of locks. The
//! concurrency belongs to the adapter that knows how many tasks it has.

use core::fmt;
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;

use crate::domain::RequestId;
use crate::domain::session::{
    AbortReason, CancelOutcome, CancelReason, CancellationToken, DuplicateRequest, InFlight,
    LifecycleError, LifecycleEvent, ProgressToken, RequestOrigin, RequestRegistry, Session,
    SessionId, SessionState,
};

use super::clock::Clock;
use super::event_log::{EventLog, SessionEvent};
use super::id_source::IdSource;
use super::lifecycle::{Classification, Declaration, DeclarationError, Lifecycle, Resolution};

/// Why a session could not do what was asked.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EngineError {
    /// The protocol's declaration could not be turned into a profile.
    #[error("the peer could not be answered: {0}")]
    Declaration(#[from] DeclarationError),
    /// The lifecycle refused the transition.
    #[error(transparent)]
    Lifecycle(#[from] LifecycleError),
}

/// A message the session would not admit, and why.
///
/// Separate from [`EngineError`] because nothing failed: the message was
/// understood, the session simply is not in a position to take it, and the
/// peer is owed a JSON-RPC error rather than a dropped connection. Every
/// variant carries the state that produced the refusal, because that is the
/// one fact an operator needs and the one a peer's bug report will not have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The session is closing or closed, so nothing is admitted at all.
    ///
    /// Checked before the lifecycle is consulted, so no protocol has to
    /// remember to re-implement the terminal case.
    SessionNotAccepting {
        /// Where the session was.
        state: SessionState,
    },
    /// The protocol said this method is not permitted here.
    NotPermitted {
        /// Where the session was.
        state: SessionState,
    },
    /// The method is ordinary work and the session has not finished its
    /// handshake.
    HandshakeIncomplete {
        /// Where the session was.
        state: SessionState,
    },
    /// Nothing has been negotiated, so there is no profile to gate against.
    ///
    /// Distinct from [`HandshakeIncomplete`](Self::HandshakeIncomplete) because
    /// the lifecycle may well have *permitted* the method and had no profile
    /// to check it against. Reporting "not permitted" there would blame the
    /// peer for a session that has not started.
    Unnegotiated {
        /// Where the session was.
        state: SessionState,
    },
}

impl Refusal {
    /// The state that produced the refusal.
    #[must_use]
    pub const fn state(&self) -> SessionState {
        match self {
            Self::SessionNotAccepting { state }
            | Self::NotPermitted { state }
            | Self::HandshakeIncomplete { state }
            | Self::Unnegotiated { state } => *state,
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state();
        match self {
            Self::SessionNotAccepting { .. } => {
                write!(formatter, "the session is {state} and admits nothing")
            }
            Self::NotPermitted { .. } => {
                write!(formatter, "the method is not permitted while {state}")
            }
            Self::HandshakeIncomplete { .. } => {
                write!(
                    formatter,
                    "the session is {state} and the handshake is unfinished"
                )
            }
            Self::Unnegotiated { .. } => {
                write!(
                    formatter,
                    "nothing is negotiated yet and the session is {state}"
                )
            }
        }
    }
}

impl std::error::Error for Refusal {}

/// What a handler produced, and what happened to it.
///
/// Not `Copy`: [`AbortReason`] carries the peer's own words for why it withdrew
/// a request, and duplicating them would mean two copies of a string nobody
/// asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dispatch<T> {
    /// The handler returned, and the request is no longer in flight.
    Completed(T),
    /// The handler returned, but the request had already been aborted.
    ///
    /// The value is discarded because a response to an aborted request is not
    /// a response the specification defines: the peer has withdrawn the work,
    /// and answering anyway is how a bridge ends up delivering a result to a
    /// peer that has moved on.
    Aborted {
        /// What the handler produced before it noticed.
        discarded: T,
        /// Why it was stopped.
        reason: AbortReason,
    },
}

impl<T> Dispatch<T> {
    /// The handler's value, whether or not it will be sent.
    #[must_use]
    pub fn into_inner(self) -> T {
        match self {
            Self::Completed(value)
            | Self::Aborted {
                discarded: value, ..
            } => value,
        }
    }

    /// Whether the result will be written to the peer.
    #[must_use]
    pub const fn is_completed(&self) -> bool {
        matches!(self, Self::Completed(_))
    }
}

/// A session, its protocol, and everything in flight on it.
///
/// `L` is the protocol, `C` the clock, `I` the id source, `G` the event log.
/// All four are consumed by `impl Trait` at the call site so a server with one
/// of each monomorphises once.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
///
/// use protocol_droid::application::session::{
///     Clock, Declaration, DeclarationError, EngineError, IdSource, Lifecycle, NullEventLog,
///     Resolution, SessionEngine,
/// };
/// use protocol_droid::domain::session::{Instant, SessionId, SessionState};
///
/// struct Fixed;
///
/// impl Clock for Fixed {
///     fn now(&self) -> Instant {
///         Instant::from_duration(Duration::ZERO)
///     }
/// }
///
/// struct Counting(std::sync::atomic::AtomicU64);
///
/// impl IdSource for Counting {
///     fn next_session_id(&self) -> SessionId {
///         SessionId::new(self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
///     }
/// }
///
/// /// A protocol that agrees to whatever it is asked for.
/// struct Echo;
///
/// impl Lifecycle for Echo {
///     type Profile = ();
///     type Version = u16;
///
///     fn classify(
///         &self,
///         state: SessionState,
///         _profile: Option<()>,
///         method: &str,
///     ) -> protocol_droid::application::session::Classification {
///         use protocol_droid::application::session::Classification;
///         match method {
///             "handshake/start" => Classification::HandshakeStart,
///             "handshake/confirm" => Classification::HandshakeConfirm,
///             _ if state.admits_ordinary_work() => Classification::Ordinary,
///             _ => Classification::Refused,
///         }
///     }
///
///     fn resolve(
///         &self,
///         declaration: Declaration<'_>,
///     ) -> Result<Resolution<(), u16>, DeclarationError> {
///         let Declaration::Handshake { version, .. } = declaration else {
///             return Err(DeclarationError::NoVersion);
///         };
///         Ok(Resolution {
///             profile: (),
///             version: u16::try_from(version.len()).unwrap_or(u16::MAX),
///             echoed: true,
///         })
///     }
/// }
///
/// let mut engine = SessionEngine::open(
///     Echo,
///     Fixed,
///     &Counting(std::sync::atomic::AtomicU64::new(0)),
///     NullEventLog,
/// );
/// assert_eq!(engine.state(), SessionState::Uninitialised);
/// # Ok::<(), EngineError>(())
/// ```
pub struct SessionEngine<L, C, G>
where
    L: Lifecycle,
{
    lifecycle: L,
    clock: C,
    log: G,
    session: Session,
    registry: RequestRegistry,
    resolved: Option<Resolution<L::Profile, L::Version>>,
}

impl<L, C, G> fmt::Debug for SessionEngine<L, C, G>
where
    L: Lifecycle,
    C: fmt::Debug,
    G: fmt::Debug,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The ports are named but not printed: a `dyn` inside one of them has
        // no useful `Debug`, and the session's own state is the fact a reader
        // came for. `finish_non_exhaustive` says so to a reader who diffs two
        // renderings, rather than letting them assume the struct has no other
        // fields.
        formatter
            .debug_struct("SessionEngine")
            .field("id", &self.session.id())
            .field("state", &self.session.state())
            .field("in_flight", &self.registry.len())
            .field("negotiated", &self.resolved.is_some())
            .finish_non_exhaustive()
    }
}

impl<L, C, G> SessionEngine<L, C, G>
where
    L: Lifecycle,
    C: Clock,
    G: EventLog,
{
    /// Opens a session and records that it exists.
    ///
    /// The identity comes from the injected source rather than from a counter
    /// in here, so a deployment that needs ids unique across processes seeds
    /// its own source and this constructor does not change.
    #[must_use]
    pub fn open<J: IdSource>(lifecycle: L, clock: C, ids: &J, log: G) -> Self {
        let session = Session::new(ids.next_session_id(), clock.now());
        log.record(SessionEvent::Opened {
            session: session.id(),
            at: session.opened_at(),
        });
        Self {
            lifecycle,
            clock,
            log,
            session,
            registry: RequestRegistry::new(),
            resolved: None,
        }
    }

    /// Opens a session that negotiates per message and retains nothing.
    ///
    /// The 2026-07-28 shape: there is no handshake to wait for, and the
    /// session exists only so a request has something to be negotiated
    /// against.
    #[must_use]
    pub fn open_stateless<J: IdSource>(lifecycle: L, clock: C, ids: &J, log: G) -> Self {
        let session = Session::stateless(ids.next_session_id(), clock.now());
        log.record(SessionEvent::Opened {
            session: session.id(),
            at: session.opened_at(),
        });
        Self {
            lifecycle,
            clock,
            log,
            session,
            registry: RequestRegistry::new(),
            resolved: None,
        }
    }

    /// Which session this is.
    #[must_use]
    pub const fn id(&self) -> SessionId {
        self.session.id()
    }

    /// Where it is in its lifecycle.
    #[must_use]
    pub const fn state(&self) -> SessionState {
        self.session.state()
    }

    /// The negotiated profile, if anything has been negotiated.
    #[must_use]
    pub const fn resolution(&self) -> Option<Resolution<L::Profile, L::Version>> {
        self.resolved
    }

    /// The protocol this session speaks.
    #[must_use]
    pub const fn lifecycle(&self) -> &L {
        &self.lifecycle
    }

    /// How many requests are in flight.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.registry.len()
    }

    /// The ids of everything in flight, in no particular order.
    pub fn in_flight_ids(&self) -> impl Iterator<Item = &RequestId> {
        self.registry.ids()
    }

    /// The entry for `id`, if it is still running.
    #[must_use]
    pub fn request(&self, id: &RequestId) -> Option<&InFlight> {
        self.registry.get(id)
    }

    /// Decides whether a message naming `method` may run.
    ///
    /// The order of the checks is the contract, and it is not arbitrary:
    /// terminal first, so a closing session is closed to the lifecycle's
    /// opinion of it; then the protocol's own classification; then the
    /// handshake gate; then the profile gate. A lifecycle that answers
    /// `Ordinary` for a method on an uninitialised session is describing its
    /// own vocabulary, not overriding the state machine.
    ///
    /// # Errors
    ///
    /// A [`Refusal`], carrying the state that produced it.
    pub fn admit(&self, method: &str, id: Option<&RequestId>) -> Result<Classification, Refusal> {
        let state = self.session.state();
        if state.is_terminal() || state == SessionState::Closing {
            return Err(self.refuse(Refusal::SessionNotAccepting { state }, method, id));
        }
        let profile = self.resolved.map(|resolved| resolved.profile);
        match self.lifecycle.classify(state, profile, method) {
            Classification::Refused => {
                Err(self.refuse(Refusal::NotPermitted { state }, method, id))
            }
            classification
            @ (Classification::HandshakeStart | Classification::HandshakeConfirm) => {
                Ok(classification)
            }
            Classification::Ordinary if !state.admits_ordinary_work() => {
                Err(self.refuse(Refusal::HandshakeIncomplete { state }, method, id))
            }
            Classification::Ordinary if self.resolved.is_none() => {
                Err(self.refuse(Refusal::Unnegotiated { state }, method, id))
            }
            Classification::Ordinary => Ok(Classification::Ordinary),
        }
    }

    /// Turns what a peer declared into a profile, and moves the session on.
    ///
    /// The event applied is chosen by the session's current state rather than
    /// by what was declared: a stateless session stays stateless, and a
    /// stateful one starts its handshake. That keeps "went stateless" a
    /// property of how the session was opened, which is the only place the
    /// distinction is actually known.
    ///
    /// # Errors
    ///
    /// [`EngineError::Declaration`] when the declaration is not something the
    /// protocol can answer, and [`EngineError::Lifecycle`] when the resulting
    /// transition is not legal from here — a second handshake on a session
    /// that already has one.
    pub fn negotiate(
        &mut self,
        declaration: Declaration<'_>,
    ) -> Result<Resolution<L::Profile, L::Version>, EngineError> {
        let resolution = self.lifecycle.resolve(declaration)?;
        // Only apply a transition when the session is actually going to move.
        // A session opened with `open_stateless` is already `Stateless`, and
        // `WentStateless` is only legal from `Uninitialised`, so re-applying it
        // would turn the second message of a stateless exchange into a fatal
        // transition error.
        if !self.session.state().is_stateless() {
            self.transition(LifecycleEvent::HandshakeStarted)?;
        }
        let version = resolution.version.to_string();
        self.log.record(SessionEvent::Negotiated {
            session: self.session.id(),
            at: self.clock.now(),
            version,
            echoed: resolution.echoed,
        });
        self.resolved = Some(resolution);
        Ok(resolution)
    }

    /// Records that the peer confirmed the handshake.
    ///
    /// Separate from [`negotiate`](Self::negotiate) because the specification
    /// has two messages here and only the first one carries a version: a peer
    /// that sends the second is saying "I have it", and there is nothing left
    /// to parse.
    ///
    /// # Errors
    ///
    /// [`EngineError::Lifecycle`] when the session is not waiting for a
    /// confirmation.
    pub fn confirm(&mut self) -> Result<(), EngineError> {
        self.transition(LifecycleEvent::HandshakeConfirmed)?;
        self.log.record(SessionEvent::Operating {
            session: self.session.id(),
            at: self.clock.now(),
        });
        Ok(())
    }

    /// Starts closing, and aborts everything that was still running.
    ///
    /// The aborts happen here rather than at [`finish`](Self::finish) because
    /// a handler awaiting a signal that will never arrive is a leaked task,
    /// and the whole point of entering `Closing` is that nothing more will be
    /// admitted.
    ///
    /// # Errors
    ///
    /// [`EngineError::Lifecycle`] from a session that is already closing or
    /// closed.
    pub fn close(&mut self) -> Result<Vec<RequestId>, EngineError> {
        self.transition(LifecycleEvent::CloseRequested)?;
        Ok(self.abandon(&AbortReason::SessionClosed))
    }

    /// Records that the session finished. Terminal.
    ///
    /// # Errors
    ///
    /// [`EngineError::Lifecycle`] from a session that is not closing.
    pub fn finish(&mut self) -> Result<(), EngineError> {
        self.transition(LifecycleEvent::Finished)
    }

    /// Records a request as in flight and hands back its cancellation handle.
    ///
    /// The handle is returned rather than left in the table because a handler
    /// that has to await it cannot hold the engine's borrow across the wait,
    /// and this engine is single-writer. Everything a canceller needs is on
    /// the token; everything an observer needs is on
    /// [`request`](Self::request).
    ///
    /// # Errors
    ///
    /// [`DuplicateRequest`] when the id is already in flight. The existing
    /// entry is untouched — the first request owns the id.
    pub fn register(
        &mut self,
        id: RequestId,
        origin: RequestOrigin,
        method: &str,
        token: Option<ProgressToken>,
        budget: Option<Duration>,
    ) -> Result<CancellationToken, DuplicateRequest> {
        let entry = InFlight::new(origin, method, token, budget, self.clock.now());
        let handle = entry.cancellation_handle();
        let method = Arc::from(method);
        self.registry.register(id.clone(), entry)?;
        self.log.record(SessionEvent::Registered {
            session: self.session.id(),
            at: self.clock.now(),
            id,
            method,
            origin,
        });
        Ok(handle)
    }

    /// The cancellation handle for `id`, without taking the engine mutably.
    ///
    /// The read-only half of [`register`](Self::register), for a task that is
    /// already running and wants to stop waiting when the peer says so.
    #[must_use]
    pub fn cancellation(&self, id: &RequestId) -> Option<CancellationToken> {
        self.registry.get(id).map(InFlight::cancellation_handle)
    }

    /// Cancels a request.
    ///
    /// Never fails. See [`CancelOutcome`], and the specification's reason for
    /// it: a cancellation notification may arrive after its request has
    /// finished, and reporting that as an error would turn a race the sender
    /// is entitled to lose into a fault.
    pub fn cancel(&mut self, id: &RequestId, reason: Option<CancelReason>) -> CancelOutcome {
        // Cloned because the reason is recorded twice: the registry takes it
        // as the reason the token was aborted, and the event log takes it as
        // the reason the operator will read. `CancelReason` is the peer's own
        // words, so there is no cheaper spelling of "keep it".
        let outcome = self.registry.cancel(id, reason.clone());
        if outcome.cancelled() {
            self.log_abort(id, AbortReason::Cancelled(reason));
        }
        outcome
    }

    /// Records progress for `id` and says whether it should go on the wire.
    ///
    /// `false` for an unknown id, for one that carried no progress token, and
    /// for a value that does not move that request's cursor forward.
    pub fn progress(&mut self, id: &RequestId, progress: f64, message: Option<String>) -> bool {
        self.registry
            .get_mut(id)
            .is_some_and(|entry| entry.advance_progress(progress, message))
    }

    /// Aborts and forgets every request whose budget is spent.
    ///
    /// Returns the ids that were expired, in no particular order. This is a
    /// tick rather than a per-frame cost: the arithmetic is a subtraction and
    /// the table walk is proportional to what is in flight, which is bounded
    /// by what the peer asked for.
    pub fn expire(&mut self) -> Vec<RequestId> {
        let expired = self.registry.expire(self.clock.now());
        for id in &expired {
            self.log_abort(id, AbortReason::Expired);
        }
        expired
    }

    /// Forgets a finished request, and says whether it may be answered.
    ///
    /// `None` for an id that is not in flight, which is the case a transport
    /// has to treat as "already reported" rather than as a fault. `Some(reason)`
    /// means the request was aborted while it ran and **no response is due** —
    /// the peer withdrew it, or its budget ran out.
    pub fn complete(&mut self, id: &RequestId) -> Option<Option<AbortReason>> {
        let entry = self.registry.complete(id)?;
        let reason = entry.cancellation().reason().cloned();
        let method = Arc::from(entry.method());
        let at = self.clock.now();
        // Matched on a reference so the reason reaches the log without being
        // moved out from under the return value the caller needs to decide
        // whether a response is still owed.
        match reason.as_ref() {
            Some(reason) => self.log.record(SessionEvent::Aborted {
                session: self.session.id(),
                at,
                id: id.clone(),
                method,
                reason: reason.clone(),
            }),
            None => self.log.record(SessionEvent::Completed {
                session: self.session.id(),
                at,
                id: id.clone(),
                method,
            }),
        }
        Some(reason)
    }

    /// Runs one request from registration to completion.
    ///
    /// The three steps are the specification's ordering and the engine's
    /// whole reason for existing: register so a cancellation has something to
    /// find, run the handler with a handle it owns, then remove the entry and
    /// report what happened. A handler that returns after its request was
    /// aborted produces [`Dispatch::Aborted`], and the caller writes nothing —
    /// that is the invariant the cancellation test pins.
    ///
    /// The handler is an [`AsyncFnOnce`] rather than a boxed future so the
    /// closure is monomorphised with the call rather than erased behind a
    /// `Pin<Box<dyn Future>>`; it is called once, so `async_call_once`'s
    /// owning-capture future is the right shape.
    ///
    /// # Errors
    ///
    /// [`DuplicateRequest`] when the id is already in flight. Nothing is run
    /// and the existing entry is left alone.
    pub async fn dispatch<H, T>(
        &mut self,
        id: RequestId,
        origin: RequestOrigin,
        method: &str,
        token: Option<ProgressToken>,
        budget: Option<Duration>,
        handler: H,
    ) -> Result<Dispatch<T>, DuplicateRequest>
    where
        H: AsyncFnOnce(CancellationToken) -> T,
    {
        let handle = self.register(id.clone(), origin, method, token, budget)?;
        let produced: T = handler(handle).await;
        Ok(match self.complete(&id) {
            // Nothing in flight: something else already removed it. The
            // handler's value cannot be attributed to a live request, so it is
            // reported as aborted rather than sent.
            None => Dispatch::Aborted {
                discarded: produced,
                reason: AbortReason::SessionClosed,
            },
            Some(None) => Dispatch::Completed(produced),
            Some(Some(reason)) => Dispatch::Aborted {
                discarded: produced,
                reason,
            },
        })
    }

    /// Applies a lifecycle event, logging the state it moved to.
    fn transition(&mut self, event: LifecycleEvent) -> Result<(), EngineError> {
        self.session = self.session.apply(event)?;
        self.log.record(SessionEvent::StateChanged {
            session: self.session.id(),
            at: self.clock.now(),
            state: self.session.state(),
        });
        Ok(())
    }

    /// Records a refusal against the event log.
    fn refuse(&self, refusal: Refusal, method: &str, id: Option<&RequestId>) -> Refusal {
        self.log.record(SessionEvent::Refused {
            session: self.session.id(),
            at: self.clock.now(),
            id: id.cloned(),
            method: Arc::from(method),
            state: refusal.state(),
        });
        refusal
    }

    /// Records why an id was aborted, reading the reason back off the entry.
    ///
    /// The reason is read rather than passed in because a deadline and a
    /// peer's notification can land in the same instant, and the one the
    /// request actually acted on is the one a post-mortem needs.
    fn log_abort(&self, id: &RequestId, fallback: AbortReason) {
        let Some(entry) = self.registry.get(id) else {
            return;
        };
        let reason = entry.cancellation().reason().cloned().unwrap_or(fallback);
        self.log.record(SessionEvent::Aborted {
            session: self.session.id(),
            at: self.clock.now(),
            id: id.clone(),
            method: Arc::from(entry.method()),
            reason,
        });
    }

    /// Aborts everything in flight and returns the ids.
    ///
    /// `&mut self` because [`RequestRegistry::drain`] takes the table and
    /// leaves it empty. The reasons are read off each entry *before* the
    /// drain, because afterwards there is no entry left to read them from.
    fn abandon(&mut self, reason: &AbortReason) -> Vec<RequestId> {
        let recorded: Vec<(RequestId, Arc<str>)> = self
            .registry
            .ids()
            .map(|id| {
                let method = self.registry.get(id).map_or("", InFlight::method);
                (id.clone(), Arc::from(method))
            })
            .collect();
        let ids = self.registry.drain();
        let at = self.clock.now();
        let session = self.session.id();
        for (id, method) in recorded {
            self.log.record(SessionEvent::Aborted {
                session,
                at,
                id,
                method,
                reason: reason.clone(),
            });
        }
        ids
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicU64, Ordering};
    use core::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::application::session::lifecycle::MetaView;
    use crate::domain::session::{Instant, SessionId};

    /// Everything the engine can be asked to do, in one error.
    ///
    /// The engine's own methods answer three unrelated error types, and a
    /// test that had to unwrap each one would be testing its fixtures rather
    /// than the engine. Collapsing them here lets every test body be a plain
    /// `?`, so a failure surfaces as the assertion that noticed it. The
    /// payloads are carried rather than discarded so that a failing test
    /// prints the engine's own account of what it refused.
    #[derive(Debug)]
    #[expect(dead_code, reason = "the payloads are read by the derived Debug")]
    enum Failure {
        /// A declaration or transition the engine refused.
        Engine(EngineError),
        /// A message the engine would not admit.
        Refusal(Refusal),
        /// A request id that was already in flight.
        Duplicate(DuplicateRequest),
        /// A test future suspended. None of these do, so reaching it means a
        /// handler grew a `.await` the harness cannot drive.
        Suspended,
        /// A task on a real runtime did not finish inside the guard. The
        /// guard is deliberately far larger than any real budget, so reaching
        /// it means the task was never woken at all rather than that it was
        /// slow.
        TimedOut(&'static str),
        /// The dispatched request was answered when it should not have been.
        /// Carries what came back so the failure names the wrong variant.
        NotAborted(String),
    }

    impl From<EngineError> for Failure {
        fn from(error: EngineError) -> Self {
            Self::Engine(error)
        }
    }

    impl From<Refusal> for Failure {
        fn from(refusal: Refusal) -> Self {
            Self::Refusal(refusal)
        }
    }

    impl From<DuplicateRequest> for Failure {
        fn from(duplicate: DuplicateRequest) -> Self {
            Self::Duplicate(duplicate)
        }
    }

    /// A lifecycle that answers `Ordinary` for everything the handshake is not,
    /// and so leaves the engine's own gates as the only thing refusing. It
    /// still has to name the two handshake methods, because a real protocol
    /// knows them by name and the engine is not allowed to.
    struct Open;

    impl Lifecycle for Open {
        type Profile = ();
        type Version = u16;

        fn classify(
            &self,
            _state: SessionState,
            _profile: Option<()>,
            method: &str,
        ) -> Classification {
            match method {
                "initialize" => Classification::HandshakeStart,
                "notifications/initialized" => Classification::HandshakeConfirm,
                _ => Classification::Ordinary,
            }
        }

        fn resolve(
            &self,
            declaration: Declaration<'_>,
        ) -> Result<Resolution<(), u16>, DeclarationError> {
            let version = match declaration {
                Declaration::Handshake { version, .. } => {
                    u16::try_from(version.len()).unwrap_or(u16::MAX)
                }
                Declaration::Stateless { .. } => 1,
            };
            Ok(Resolution {
                profile: (),
                version,
                echoed: true,
            })
        }
    }

    /// A lifecycle that narrows what its own phase would permit: it refuses
    /// until it has a profile to answer with. Nothing else in these tests
    /// exercises the protocol's veto, only the engine's gates.
    struct Strict;

    impl Lifecycle for Strict {
        type Profile = bool;
        type Version = u16;

        fn classify(
            &self,
            _state: SessionState,
            profile: Option<bool>,
            _method: &str,
        ) -> Classification {
            match profile {
                Some(true) => Classification::Ordinary,
                _ => Classification::Refused,
            }
        }

        fn resolve(
            &self,
            declaration: Declaration<'_>,
        ) -> Result<Resolution<bool, u16>, DeclarationError> {
            let Declaration::Handshake { version, .. } = declaration else {
                return Err(DeclarationError::NoVersion);
            };
            Ok(Resolution {
                profile: true,
                version: u16::try_from(version.len()).unwrap_or(u16::MAX),
                echoed: true,
            })
        }
    }

    struct Fixed(Instant);

    impl Clock for Fixed {
        fn now(&self) -> Instant {
            self.0
        }
    }

    /// A clock a test can move, so a budget can expire without a sleep.
    struct Stepping {
        now: AtomicU64,
    }

    /// Borrowed so the same clock outlives the engine it was moved into and
    /// the test can still advance it afterwards.
    impl Clock for &Stepping {
        fn now(&self) -> Instant {
            <Stepping as Clock>::now(self)
        }
    }

    impl Stepping {
        fn new() -> Self {
            Self {
                now: AtomicU64::new(0),
            }
        }

        fn advance(&self, by: Duration) {
            let millis = u64::try_from(by.as_millis()).unwrap_or(u64::MAX);
            self.now.fetch_add(millis, Ordering::Relaxed);
        }
    }

    impl Clock for Stepping {
        fn now(&self) -> Instant {
            Instant::from_duration(Duration::from_millis(self.now.load(Ordering::Relaxed)))
        }
    }

    struct Counting;

    impl IdSource for Counting {
        fn next_session_id(&self) -> SessionId {
            SessionId::new(1)
        }
    }

    /// Keeps every event, so a test can assert on what the engine reported
    /// rather than only on what it returned.
    #[derive(Default)]
    struct Recording {
        events: core::cell::RefCell<Vec<SessionEvent>>,
    }

    impl EventLog for Recording {
        fn record(&self, event: SessionEvent) {
            self.events.borrow_mut().push(event);
        }
    }

    impl Recording {
        fn kinds(&self) -> Vec<&'static str> {
            self.events
                .borrow()
                .iter()
                .map(|event| match event {
                    SessionEvent::Opened { .. } => "opened",
                    SessionEvent::Negotiated { .. } => "negotiated",
                    SessionEvent::Operating { .. } => "operating",
                    SessionEvent::Registered { .. } => "registered",
                    SessionEvent::Completed { .. } => "completed",
                    SessionEvent::Aborted { .. } => "aborted",
                    SessionEvent::Refused { .. } => "refused",
                    SessionEvent::StateChanged { .. } => "state-changed",
                })
                .collect()
        }
    }

    fn engine() -> SessionEngine<Open, Fixed, Recording> {
        SessionEngine::open(
            Open,
            Fixed(Instant::from_duration(Duration::ZERO)),
            &Counting,
            Recording::default(),
        )
    }

    /// An engine that has completed its handshake.
    fn negotiated() -> Result<SessionEngine<Open, Fixed, Recording>, Failure> {
        let mut engine = engine();
        engine.negotiate(Declaration::Handshake {
            version: "2025-06-18",
            capabilities: Some(&json!({})),
        })?;
        engine.confirm()?;
        Ok(engine)
    }

    #[test]
    fn a_new_session_admits_nothing_until_the_handshake_settles() {
        let engine = engine();
        assert_eq!(engine.state(), SessionState::Uninitialised);
        assert_eq!(
            engine.admit("tools/list", None),
            Err(Refusal::HandshakeIncomplete {
                state: SessionState::Uninitialised
            })
        );
        assert!(engine.resolution().is_none());
    }

    /// A handshake is the one thing admitted before the handshake is done —
    /// the engine cannot gate the gate on itself.
    #[test]
    fn the_handshake_messages_pass_the_gate_that_ordinary_work_fails() -> Result<(), Failure> {
        let mut engine = engine();
        assert_eq!(
            engine.admit("initialize", None)?,
            Classification::HandshakeStart,
            "the handshake is always admitted"
        );
        engine.negotiate(Declaration::Handshake {
            version: "2025-06-18",
            capabilities: None,
        })?;
        assert_eq!(
            engine.admit("notifications/initialized", None)?,
            Classification::HandshakeConfirm,
            "the confirmation is admitted too"
        );
        Ok(())
    }

    #[test]
    fn the_handshake_moves_the_session_to_operating() -> Result<(), Failure> {
        let engine = negotiated()?;
        assert_eq!(engine.state(), SessionState::Operating);
        assert_eq!(engine.admit("tools/list", None)?, Classification::Ordinary);
        Ok(())
    }

    #[test]
    fn the_lifecycle_decides_admission_not_just_the_phase() -> Result<(), Failure> {
        let mut engine = SessionEngine::open(
            Strict,
            Fixed(Instant::from_duration(Duration::ZERO)),
            &Counting,
            Recording::default(),
        );
        // No profile yet, so the protocol itself says no — a different
        // refusal from the one an uninitialised phase would produce.
        assert_eq!(
            engine.admit("tools/list", None),
            Err(Refusal::NotPermitted {
                state: SessionState::Uninitialised
            }),
            "the protocol's own veto, not the phase gate"
        );

        engine.negotiate(Declaration::Handshake {
            version: "2025-06-18",
            capabilities: None,
        })?;
        engine.confirm()?;
        assert_eq!(engine.admit("tools/list", None)?, Classification::Ordinary);
        Ok(())
    }

    #[test]
    fn a_stateless_session_never_waits_for_a_handshake() -> Result<(), Failure> {
        let mut engine = SessionEngine::open_stateless(
            Open,
            Fixed(Instant::from_duration(Duration::ZERO)),
            &Counting,
            Recording::default(),
        );
        assert!(engine.state().is_stateless());
        engine.negotiate(Declaration::Stateless {
            meta: MetaView::new(None),
        })?;
        // Still stateless: nothing moved it into the stateful lifecycle.
        assert!(engine.state().is_stateless());
        Ok(())
    }

    #[test]
    fn a_duplicate_in_flight_id_is_refused_and_leaves_the_first_alone() -> Result<(), Failure> {
        let mut engine = negotiated()?;
        let id = RequestId::text("a");
        let Ok(_) = engine.register(id.clone(), RequestOrigin::Inbound, "tools/call", None, None)
        else {
            return Err(Failure::Suspended);
        };
        assert_eq!(
            engine
                .register(id.clone(), RequestOrigin::Inbound, "tools/call", None, None)
                .map(|_| ()),
            Err(DuplicateRequest { id }),
            "the id is already in flight"
        );
        assert_eq!(engine.in_flight(), 1, "the original entry survives");
        Ok(())
    }

    #[test]
    fn cancelling_an_unknown_or_finished_request_is_a_no_op() -> Result<(), Failure> {
        let mut engine = negotiated()?;
        let outcome = engine.cancel(&RequestId::text("never-seen"), None);
        assert!(!outcome.cancelled());
        assert_eq!(engine.in_flight(), 0);
        Ok(())
    }

    #[test]
    fn cancelling_an_in_flight_request_is_visible_to_its_handler() -> Result<(), Failure> {
        let mut engine = negotiated()?;
        let id = RequestId::text("a");
        let Ok(token) =
            engine.register(id.clone(), RequestOrigin::Inbound, "tools/call", None, None)
        else {
            return Err(Failure::Suspended);
        };
        assert!(!token.is_cancelled());
        let outcome = engine.cancel(&id, None);
        assert!(outcome.cancelled());
        assert!(token.is_cancelled(), "the handler observes the abort");
        // Still in flight: cancelling marks the token, it does not remove the
        // entry. The entry goes when the handler returns, so that a handler
        // which is mid-poll keeps a row it can be reported against.
        assert_eq!(engine.in_flight(), 1);
        assert_eq!(
            engine.complete(&id),
            Some(Some(AbortReason::Cancelled(None)))
        );
        assert_eq!(engine.in_flight(), 0);
        Ok(())
    }

    #[test]
    fn a_request_that_ran_to_completion_is_reported_as_completed() -> Result<(), Failure> {
        let mut engine = negotiated()?;
        let id = RequestId::text("a");
        let _ = engine.register(id.clone(), RequestOrigin::Inbound, "tools/call", None, None)?;
        assert_eq!(engine.complete(&id), Some(None), "no abort reason");
        assert_eq!(engine.complete(&id), None, "already finished");
        Ok(())
    }

    #[test]
    fn a_cancelled_request_reports_an_abort_rather_than_a_completion() -> Result<(), Failure> {
        let mut engine = negotiated()?;
        let id = RequestId::text("a");
        let _ = engine.register(id.clone(), RequestOrigin::Inbound, "tools/call", None, None)?;
        engine.cancel(&id, None);
        assert!(matches!(
            engine.complete(&id),
            Some(Some(AbortReason::Cancelled(_)))
        ));
        Ok(())
    }

    /// A cancelled request produces a value and still answers nothing.
    ///
    /// The handler returns normally, so the only thing standing between the
    /// peer and a response is `dispatch`'s own check against the abort. This
    /// runs on a real scheduler rather than the single-poll harness because
    /// the handler and the canceller have to interleave.
    ///
    /// It deliberately does **not** claim the wake. Stopping `Cancelled::poll`
    /// from registering its waker does not fail this test: the runtime re-polls
    /// the handler for unrelated reasons once the spawned canceller finishes.
    /// `a_spent_budget_wakes_a_handler_parked_on_its_handle` is the test that
    /// fails on that mutation, and it is the one that carries the claim.
    #[tokio::test(start_paused = true)]
    async fn a_cancelled_request_answers_nothing_even_when_the_handler_returns()
    -> Result<(), Failure> {
        use crate::domain::session::CancelReason;

        let mut engine = negotiated()?;
        let id = RequestId::text("a");

        let dispatched = engine.dispatch(
            id,
            RequestOrigin::Inbound,
            "tools/call",
            None,
            None,
            |signal: CancellationToken| async move {
                // The peer withdraws the request while this handler is parked
                // on the signal below.
                let peer = signal.clone();
                let canceller = tokio::spawn(async move {
                    peer.cancel(AbortReason::Cancelled(Some(CancelReason::new("peer"))));
                });
                signal.cancelled().await;
                let _ = canceller.await;
                "a value the peer will never receive"
            },
        );

        let outcome = tokio::time::timeout(Duration::from_secs(5), dispatched)
            .await
            .map_err(|_| Failure::TimedOut("the handler was parked, not woken"))??;

        // The handler ran to the end and produced a value, and the answer is
        // still not sent: `Aborted` carries what was discarded rather than
        // what is returned.
        let Dispatch::Aborted { discarded, reason } = outcome else {
            return Err(Failure::NotAborted(format!("{outcome:?}")));
        };
        assert_eq!(discarded, "a value the peer will never receive");
        assert!(matches!(reason, AbortReason::Cancelled(_)), "{reason:?}");
        Ok(())
    }

    /// The deadline half of the same race, charged to the engine's own clock.
    ///
    /// This one sits at the registry rather than at `dispatch` on purpose.
    /// `dispatch` takes `&mut self` for the whole handler, so a test cannot
    /// call `expire` on the engine while a handler is parked — the borrow is
    /// held for the handler's lifetime. What the registry can do, and what is
    /// the whole contract between a deadline and a handler, is prove that
    /// expiring an entry wakes a real task waiting on that entry's handle.
    #[tokio::test(start_paused = true)]
    async fn a_spent_budget_wakes_a_handler_parked_on_its_handle() -> Result<(), Failure> {
        let clock = &Stepping::new();
        let mut engine = SessionEngine::open(Open, clock, &Counting, Recording::default());
        engine.negotiate(Declaration::Handshake {
            version: "2025-06-18",
            capabilities: None,
        })?;
        engine.confirm()?;

        let id = RequestId::text("a");
        let handle = engine.register(
            id.clone(),
            RequestOrigin::Inbound,
            "tools/call",
            None,
            Some(Duration::from_millis(50)),
        )?;

        // Park a real task on the handle and let it poll, so its waker is
        // actually registered. Without the yield there is nothing to wake.
        let waiter = tokio::spawn({
            let handle = handle.clone();
            async move {
                handle.cancelled().await;
                handle.reason().cloned()
            }
        });
        tokio::task::yield_now().await;

        // Half the budget must not expire it.
        clock.advance(Duration::from_millis(25));
        assert!(
            engine.expire().is_empty(),
            "a budget spent only half way is not spent"
        );

        clock.advance(Duration::from_millis(25));
        assert_eq!(engine.expire(), vec![id], "the budget is now spent");

        let reason = tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .map_err(|_| Failure::TimedOut("the expiry never woke the parked task"))?
            .map_err(|panic| Failure::NotAborted(format!("the parked task panicked: {panic}")))?;
        assert_eq!(
            reason,
            Some(AbortReason::Expired),
            "the handler woke for the reason the engine recorded"
        );
        Ok(())
    }

    #[test]
    fn a_spent_budget_expires_and_nothing_else_does() -> Result<(), Failure> {
        let clock = &Stepping::new();
        let mut engine = SessionEngine::open(Open, clock, &Counting, Recording::default());
        engine.negotiate(Declaration::Handshake {
            version: "2025-06-18",
            capabilities: None,
        })?;
        engine.confirm()?;

        let quick = RequestId::text("quick");
        let slow = RequestId::text("slow");
        let _ = engine.register(
            quick.clone(),
            RequestOrigin::Inbound,
            "tools/call",
            None,
            Some(Duration::from_millis(50)),
        )?;
        let _ = engine.register(
            slow.clone(),
            RequestOrigin::Inbound,
            "tools/call",
            None,
            Some(Duration::from_millis(5_000)),
        )?;

        clock.advance(Duration::from_millis(100));
        let expired = engine.expire();
        assert_eq!(expired, vec![quick], "only the spent budget expires");
        assert!(
            engine.request(&slow).is_some(),
            "the other is still running"
        );
        Ok(())
    }

    #[test]
    fn closing_aborts_everything_still_in_flight() -> Result<(), Failure> {
        let mut engine = negotiated()?;
        let _ = engine.register(
            RequestId::text("a"),
            RequestOrigin::Outbound,
            "sampling/createMessage",
            None,
            None,
        )?;
        let _ = engine.register(
            RequestId::text("b"),
            RequestOrigin::Outbound,
            "elicitation/create",
            None,
            None,
        )?;
        assert_eq!(engine.in_flight(), 2);
        engine.close()?;
        assert_eq!(engine.in_flight(), 0, "nothing is left to leak");
        assert_eq!(
            engine.state(),
            SessionState::Closing,
            "closing is not yet terminal — the peer still has to finish"
        );
        engine.finish()?;
        assert!(engine.state().is_terminal());
        assert!(
            engine.admit("tools/list", None).is_err(),
            "a finished session admits nothing"
        );
        Ok(())
    }

    #[test]
    fn dispatch_registers_runs_and_completes() -> Result<(), Failure> {
        let mut engine = negotiated()?;
        let outcome = futures_lite_block_on(engine.dispatch(
            RequestId::text("a"),
            RequestOrigin::Inbound,
            "tools/call",
            None,
            None,
            |_token| async { "ok" },
        ))
        .ok_or(Failure::Suspended)??;
        assert!(outcome.is_completed(), "the handler ran to the end");
        assert_eq!(outcome.into_inner(), "ok");
        assert_eq!(engine.in_flight(), 0);
        assert_eq!(
            engine.log.kinds(),
            [
                "opened",
                "state-changed",
                "negotiated",
                "state-changed",
                "operating",
                "registered",
                "completed",
            ],
            "the log is a transcript: every transition, then the work"
        );
        Ok(())
    }

    #[test]
    fn every_recorded_event_names_the_session_and_when_it_happened() {
        let engine = engine();
        let events = engine.log.events.borrow();
        // Checked across every event rather than just the first: the invariant
        // is that no event escapes without them, so sampling one would not
        // test it.
        for event in events.iter() {
            assert_eq!(event.session(), engine.id());
            assert_eq!(event.at(), Instant::from_duration(Duration::ZERO));
        }
        assert!(!events.is_empty(), "opening records at least one event");
    }

    #[test]
    fn a_refusal_is_recorded_before_it_is_returned() {
        let engine = engine();
        let id = RequestId::text("a");
        assert!(engine.admit("tools/call", Some(&id)).is_err());
        assert_eq!(engine.log.kinds(), ["opened", "refused"]);
    }

    #[test]
    fn a_cancelled_dispatch_sends_no_response() -> Result<(), Failure> {
        let mut engine = negotiated()?;
        let outcome = futures_lite_block_on(engine.dispatch(
            RequestId::text("a"),
            RequestOrigin::Inbound,
            "tools/call",
            None,
            None,
            |token| async move {
                // The peer withdrew the request while the handler was running.
                let _ = engine_cancel(&token);
                "should not be sent"
            },
        ))
        .ok_or(Failure::Suspended)??;
        assert!(
            matches!(outcome, Dispatch::Aborted { .. }),
            "an aborted request is never answered"
        );
        assert!(!outcome.is_completed(), "an abort is not a completion");
        assert_eq!(
            outcome.into_inner(),
            "should not be sent",
            "the value is still handed back, for the log rather than the wire"
        );
        Ok(())
    }

    /// Drives a token's cancel path from inside a handler.
    fn engine_cancel(token: &CancellationToken) -> bool {
        token.cancel(AbortReason::Cancelled(None))
    }

    /// A no-op executor: every future these tests build is immediately ready,
    /// because nothing in the engine's own contract sleeps. A `Pending` here
    /// would be a bug, so it is reported rather than retried forever.
    fn futures_lite_block_on<F: core::future::Future>(future: F) -> Option<F::Output> {
        use core::task::{Context, Poll};
        use std::task::Waker;

        let mut context = Context::from_waker(Waker::noop());
        let mut future = Box::pin(future);
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => Some(value),
            Poll::Pending => None,
        }
    }
}
