//! The session lifecycle: five states, six events, and the table between them.
//!
//! # Why the table is explicit
//!
//! Every edge is written out. A conditional inside a match arm would be shorter
//! and would let the legal set drift away from the documented one, which is
//! exactly the failure a lifecycle has to be able to rule out. Written out, the
//! set of states is the set of match arms and the set of edges is the set of
//! arms, so a reader can check one against the other by eye.
//!
//! # Why `Closed` is terminal
//!
//! A session that could re-open would leak every in-flight request it was
//! holding, and the specification has no transition back. [`Session::apply`]
//! answers [`LifecycleError::Terminal`] from `Closed` for *every* event rather
//! than silently treating the session as freshly uninitialised.

use core::fmt;
use std::time::Duration;

use thiserror::Error;

/// A session's identity.
///
/// A `u64` behind a newtype rather than a string: it is compared constantly,
/// and `Copy` is what lets it sit inside every event without a borrow.
///
/// # Redaction
///
/// [`Debug`](fmt::Debug) prints `SessionId(…)` and never the value. `Debug` is
/// what a panic, an `assert_eq!` and a `dbg!` expand to, so leaving it derived
/// made "we never log session ids" a convention rather than a property of the
/// type. [`RequestId`](crate::domain::request_id::RequestId) keeps a
/// four-character prefix so two ids in one line can be told apart; this cannot,
/// because there is nothing here to keep — the whole value is a counter, so
/// every digit of it is either the value itself or a leading zero.
///
/// [`McpSessionId`](crate::domain::mcp::session_id::McpSessionId) derives its
/// `Debug` from this one, so it inherits the redaction rather than needing an
/// impl of its own.
///
/// # Why `Display` is not redacted
///
/// This is load-bearing, not an oversight.
/// [`IdempotencyKey::for_request`](crate::domain::idempotency::IdempotencyKey::for_request)
/// composes `"{session}/{id}"`, so a redacted `Display` would collapse every
/// session onto one key and let a result recorded for one session answer
/// another's request. `Display` is the identity and serialisation API here, not
/// a logging one, and the redaction belongs at `Debug` because that is the
/// path a value escapes through by accident.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionId(u64);

impl fmt::Debug for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SessionId(…)")
    }
}

impl SessionId {
    /// The identity a source minted.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// The underlying value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "session#{}", self.0)
    }
}

/// A monotonic reading from an injected clock.
///
/// # Why a duration and not a wall clock
///
/// Two sessions started in the same millisecond are indistinguishable, and two
/// readings of a wall clock can go backwards when NTP steps. A duration from
/// whichever origin the adapter chose has neither problem, and it makes every
/// comparison in the engine a subtraction.
///
/// The origin is the adapter's business. Nothing here reads a clock, so
/// nothing here can disagree with one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct Instant(Duration);

impl Instant {
    /// A reading `since` after the clock's origin.
    #[must_use]
    pub const fn from_duration(since: Duration) -> Self {
        Self(since)
    }

    /// The reading, as a duration from the clock's origin.
    #[must_use]
    pub const fn as_duration(self) -> Duration {
        self.0
    }

    /// How much time passed between `earlier` and `self`.
    ///
    /// Saturating rather than wrapping: a clock whose second reading somehow
    /// precedes the first should report no elapsed time, not a duration near
    /// `u64::MAX` that would expire every in-flight request at once.
    #[must_use]
    pub const fn since(self, earlier: Self) -> Duration {
        self.0.saturating_sub(earlier.0)
    }

    /// This reading, `by` later.
    #[must_use]
    pub const fn advanced(self, by: Duration) -> Self {
        Self(self.0.saturating_add(by))
    }
}

impl fmt::Display for Instant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}ms", self.0.as_millis())
    }
}

/// Where a session is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SessionState {
    /// A connection exists; no handshake has been offered.
    Uninitialised,
    /// A handshake was accepted and answered; the peer has not confirmed it.
    Initialising,
    /// The handshake completed. Ordinary work is admitted.
    Operating,
    /// A close was asked for. In-flight work is being drained; nothing new is
    /// admitted.
    Closing,
    /// Finished. Terminal.
    Closed,
    /// No handshake at all: every message negotiates for itself and the
    /// session retains nothing between them.
    Stateless,
}

impl SessionState {
    /// Whether ordinary work — anything that is not part of the handshake — may
    /// be dispatched in this state.
    ///
    /// The gate a lifecycle is asked to refine, not replace: a protocol
    /// narrows this further for its own methods, but the engine refuses
    /// everything in a closing or closed session regardless of what any
    /// lifecycle says.
    #[must_use]
    pub const fn admits_ordinary_work(self) -> bool {
        matches!(self, Self::Operating | Self::Stateless)
    }

    /// Whether the session retains nothing between messages.
    #[must_use]
    pub const fn is_stateless(self) -> bool {
        matches!(self, Self::Stateless)
    }

    /// Whether no further transition is possible.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Closed)
    }

    /// The state name, for messages and event logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Uninitialised => "uninitialised",
            Self::Initialising => "initialising",
            Self::Operating => "operating",
            Self::Closing => "closing",
            Self::Closed => "closed",
            Self::Stateless => "stateless",
        }
    }
}

impl fmt::Display for SessionState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Something that happened to a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LifecycleEvent {
    /// The peer offered its handshake.
    HandshakeStarted,
    /// The peer confirmed the handshake result landed.
    HandshakeConfirmed,
    /// A message negotiated its own profile, so there is no handshake to wait
    /// for.
    WentStateless,
    /// A close was asked for.
    CloseRequested,
    /// The session finished.
    Finished,
}

impl LifecycleEvent {
    /// The event name, for messages and event logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HandshakeStarted => "handshake-started",
            Self::HandshakeConfirmed => "handshake-confirmed",
            Self::WentStateless => "went-stateless",
            Self::CloseRequested => "close-requested",
            Self::Finished => "finished",
        }
    }
}

impl fmt::Display for LifecycleEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Why a lifecycle transition was refused.
///
/// Every variant is a case where guessing an edge would invent a state the
/// protocol never described — there is deliberately no "force" arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum LifecycleError {
    /// The session has finished and admits nothing.
    #[error("{event} is not legal on a closed session")]
    Terminal {
        /// The event that was refused.
        event: LifecycleEvent,
    },
    /// The event does not apply to the state the session is in.
    #[error("{event} is not legal while the session is {state}")]
    IllegalTransition {
        /// The state the session is in.
        state: SessionState,
        /// The event that was refused.
        event: LifecycleEvent,
    },
}

impl LifecycleError {
    /// Whether the refusal is final rather than a "not yet".
    ///
    /// A caller that batches transitions — draining a transport, say — needs
    /// to tell "you are early" from "stop asking". Only a closed session is
    /// final.
    #[must_use]
    pub const fn is_final(self) -> bool {
        matches!(self, Self::Terminal { .. })
    }
}

/// A session and the phase it is in.
///
/// `apply` takes `&self` and returns a new value rather than mutating: the
/// transition table is then the only thing that can produce a state, and a
/// caller that gets an `Err` still holds the session it had. The value is four
/// machine words, so returning one costs less than the clone-and-mutate the
/// alternative would need at the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Session {
    id: SessionId,
    state: SessionState,
    opened_at: Instant,
    transitions: u32,
}

impl Session {
    /// A new session, waiting for a handshake.
    #[must_use]
    pub const fn new(id: SessionId, opened_at: Instant) -> Self {
        Self {
            id,
            state: SessionState::Uninitialised,
            opened_at,
            transitions: 0,
        }
    }

    /// A session that negotiates per message and retains nothing.
    #[must_use]
    pub const fn stateless(id: SessionId, opened_at: Instant) -> Self {
        Self {
            id,
            state: SessionState::Stateless,
            opened_at,
            transitions: 0,
        }
    }

    /// Which session this is.
    #[must_use]
    pub const fn id(&self) -> SessionId {
        self.id
    }

    /// Where it is in its lifecycle.
    #[must_use]
    pub const fn state(&self) -> SessionState {
        self.state
    }

    /// When it opened, as read from the injected clock.
    #[must_use]
    pub const fn opened_at(&self) -> Instant {
        self.opened_at
    }

    /// How many transitions have been applied.
    ///
    /// A cheap way for the event log to order two sessions' histories, and the
    /// number a test asserts on to show that a refused event changed nothing.
    #[must_use]
    pub const fn transitions(&self) -> u32 {
        self.transitions
    }

    /// Whether ordinary work is admitted right now.
    #[must_use]
    pub const fn admits_ordinary_work(&self) -> bool {
        self.state.admits_ordinary_work()
    }

    /// Applies one event.
    ///
    /// # Errors
    ///
    /// [`LifecycleError::Terminal`] from [`SessionState::Closed`], and
    /// [`LifecycleError::IllegalTransition`] for any event the table does not
    /// list for the current state.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::domain::session::{LifecycleEvent, Session, SessionId};
    ///
    /// let open = Session::new(SessionId::new(1), Default::default());
    /// let starting = open.apply(LifecycleEvent::HandshakeStarted)?;
    /// assert_eq!(starting.state().as_str(), "initialising");
    ///
    /// // A second handshake offer is not a thing the specification has, so it
    /// // is refused rather than treated as a restart.
    /// assert!(starting.apply(LifecycleEvent::HandshakeStarted).is_err());
    /// # Ok::<(), protocol_droid::domain::session::LifecycleError>(())
    /// ```
    pub const fn apply(&self, event: LifecycleEvent) -> Result<Self, LifecycleError> {
        if self.state.is_terminal() {
            return Err(LifecycleError::Terminal { event });
        }
        let state = match (self.state, event) {
            (SessionState::Uninitialised, LifecycleEvent::HandshakeStarted) => {
                SessionState::Initialising
            }
            (SessionState::Uninitialised, LifecycleEvent::WentStateless) => SessionState::Stateless,
            (SessionState::Initialising, LifecycleEvent::HandshakeConfirmed) => {
                SessionState::Operating
            }
            // Closing is reachable from every live state and from nowhere else,
            // so a session can never be closed twice by two different paths.
            (
                SessionState::Uninitialised
                | SessionState::Initialising
                | SessionState::Operating
                | SessionState::Stateless,
                LifecycleEvent::CloseRequested,
            ) => SessionState::Closing,
            (SessionState::Closing, LifecycleEvent::Finished) => SessionState::Closed,
            (state, event) => return Err(LifecycleError::IllegalTransition { state, event }),
        };
        Ok(Self {
            id: self.id,
            state,
            opened_at: self.opened_at,
            transitions: self.transitions.saturating_add(1),
        })
    }
}

#[cfg(test)]
mod tests {
    // Test code unwraps deliberately: the strict profile exists to keep
    use proptest::prelude::*;

    use super::{Instant, LifecycleError, LifecycleEvent, Session, SessionId, SessionState};

    fn open() -> Session {
        Session::new(
            SessionId::new(7),
            Instant::from_duration(std::time::Duration::ZERO),
        )
    }

    /// Every edge the lifecycle is documented to have, checked one at a time.
    ///
    /// Written out rather than generated because the list *is* the
    /// specification: a generated list would only prove the implementation
    /// agrees with itself, which is the mistake the property test below is
    /// careful to avoid.
    #[test]
    fn every_documented_edge_is_reachable() {
        let cases: &[(SessionState, LifecycleEvent, SessionState)] = &[
            (
                SessionState::Uninitialised,
                LifecycleEvent::HandshakeStarted,
                SessionState::Initialising,
            ),
            (
                SessionState::Uninitialised,
                LifecycleEvent::WentStateless,
                SessionState::Stateless,
            ),
            (
                SessionState::Uninitialised,
                LifecycleEvent::CloseRequested,
                SessionState::Closing,
            ),
            (
                SessionState::Initialising,
                LifecycleEvent::HandshakeConfirmed,
                SessionState::Operating,
            ),
            (
                SessionState::Initialising,
                LifecycleEvent::CloseRequested,
                SessionState::Closing,
            ),
            (
                SessionState::Operating,
                LifecycleEvent::CloseRequested,
                SessionState::Closing,
            ),
            (
                SessionState::Stateless,
                LifecycleEvent::CloseRequested,
                SessionState::Closing,
            ),
            (
                SessionState::Closing,
                LifecycleEvent::Finished,
                SessionState::Closed,
            ),
        ];
        for (from, event, to) in cases {
            let session = Session {
                state: *from,
                ..open()
            };
            assert_eq!(
                session.apply(*event).map(|next| next.state()),
                Ok(*to),
                "{event} from {from} should be legal"
            );
        }
    }

    #[test]
    fn closing_is_reachable_from_every_live_state_and_nothing_reaches_operating_from_closed()
    -> Result<(), LifecycleError> {
        for state in [
            SessionState::Uninitialised,
            SessionState::Initialising,
            SessionState::Operating,
            SessionState::Stateless,
        ] {
            let session = Session { state, ..open() };
            let closing = session.apply(LifecycleEvent::CloseRequested)?;
            assert_eq!(closing.state(), SessionState::Closing, "from {state}");
            let closed = closing.apply(LifecycleEvent::Finished)?;
            assert_eq!(closed.state(), SessionState::Closed, "from {state}");
        }
        Ok(())
    }

    /// A session walked through `events`.
    ///
    /// `apply` already answers `LifecycleError`, so a `Result`-returning test
    /// can `?` its way to the state it means to assert on. A refused step then
    /// fails the test with the table's own explanation attached, rather than
    /// panicking at an unwrap that names neither the event nor the state.
    fn walked(events: &[LifecycleEvent]) -> Result<Session, LifecycleError> {
        events
            .iter()
            .try_fold(open(), |session, event| session.apply(*event))
    }

    #[test]
    fn a_refused_event_leaves_the_session_exactly_as_it_was() -> Result<(), LifecycleError> {
        let operating = walked(&[
            LifecycleEvent::HandshakeStarted,
            LifecycleEvent::HandshakeConfirmed,
        ])?;
        let before = operating.transitions();
        let refused = operating.apply(LifecycleEvent::HandshakeConfirmed);
        assert_matches::assert_matches!(
            refused,
            Err(LifecycleError::IllegalTransition {
                state: SessionState::Operating,
                event: LifecycleEvent::HandshakeConfirmed
            })
        );
        assert_eq!(
            operating.transitions(),
            before,
            "a refused event was counted"
        );
        assert!(operating.admits_ordinary_work());
        Ok(())
    }

    #[test]
    fn closed_is_terminal_for_every_event() -> Result<(), LifecycleError> {
        let closed = walked(&[LifecycleEvent::CloseRequested, LifecycleEvent::Finished])?;
        for event in [
            LifecycleEvent::HandshakeStarted,
            LifecycleEvent::HandshakeConfirmed,
            LifecycleEvent::WentStateless,
            LifecycleEvent::CloseRequested,
            LifecycleEvent::Finished,
        ] {
            let refused = closed.apply(event);
            assert!(
                matches!(refused, Err(LifecycleError::Terminal { .. })),
                "{event} revived a closed session"
            );
        }
        Ok(())
    }

    /// A caller that batches transitions has to tell "you are early" from
    /// "stop asking", and that is the whole of [`LifecycleError::is_final`].
    #[test]
    fn only_a_closed_session_gives_a_final_refusal() -> Result<(), LifecycleError> {
        let closed = walked(&[LifecycleEvent::CloseRequested, LifecycleEvent::Finished])?;
        let terminal = closed.apply(LifecycleEvent::HandshakeStarted);
        assert!(
            matches!(terminal, Err(refusal) if refusal.is_final()),
            "a closed session will never accept anything again: {terminal:?}"
        );

        // Still initialising, so a handshake out of turn is a "not yet" and a
        // batched drain must keep going rather than abandon the queue.
        let early = open().apply(LifecycleEvent::HandshakeConfirmed);
        assert!(
            matches!(early, Err(refusal) if !refusal.is_final()),
            "an out-of-turn event is recoverable, not a dead end: {early:?}"
        );
        Ok(())
    }

    /// The property the task asks for: no sequence of events reaches a state
    /// that is not one of the five.
    ///
    /// It is weaker than it looks, because the state is an enum and a caller
    /// cannot construct an invalid one. The falsifiable half is `closed_is_absorbing`
    /// below, which a "reset to uninitialised when an event does not apply"
    /// implementation would fail.
    fn any_event() -> impl Strategy<Value = LifecycleEvent> {
        prop_oneof![
            Just(LifecycleEvent::HandshakeStarted),
            Just(LifecycleEvent::HandshakeConfirmed),
            Just(LifecycleEvent::WentStateless),
            Just(LifecycleEvent::CloseRequested),
            Just(LifecycleEvent::Finished),
        ]
    }

    proptest! {
        #[test]
        fn no_event_sequence_reaches_an_unmodelled_state(events in prop::collection::vec(any_event(), 0..24)) {
            let mut session = open();
            for event in &events {
                if let Ok(next) = session.apply(*event) {
                    session = next;
                }
                prop_assert!(matches!(
                    session.state(),
                    SessionState::Uninitialised
                        | SessionState::Initialising
                        | SessionState::Operating
                        | SessionState::Closing
                        | SessionState::Closed
                        | SessionState::Stateless
                ));
                // The count only ever goes up by one per accepted event, and
                // never past the number of events offered.
                prop_assert!(session.transitions() as usize <= events.len());
            }
        }

        /// The strong half. Once closed, no suffix of events can reopen the
        /// session, and a session that has closed admits no ordinary work.
        #[test]
        fn closed_is_absorbing(
            prefix in prop::collection::vec(any_event(), 0..12),
            suffix in prop::collection::vec(any_event(), 0..12),
        ) {
            let mut session = open();
            for event in &prefix {
                if let Ok(next) = session.apply(*event) {
                    session = next;
                }
            }
            for event in &suffix {
                if let Ok(next) = session.apply(*event) {
                    session = next;
                }
            }
            if session.state().is_terminal() {
                prop_assert!(!session.admits_ordinary_work());
                // A second sweep cannot escape either: the guard is a property
                // of the state, not of the first event that produced it.
                for event in &suffix {
                    prop_assert!(session.apply(*event).is_err());
                }
            }
        }

        /// Closing is one-way. A session that has been asked to close never
        /// reaches `operating` again, however many events follow.
        #[test]
        fn closing_does_not_reverse(
            prefix in prop::collection::vec(any_event(), 0..12),
            suffix in prop::collection::vec(any_event(), 0..12),
        ) {
            let mut session = open();
            for event in prefix {
                if let Ok(next) = session.apply(event) {
                    session = next;
                }
            }
            if !matches!(session.state(), SessionState::Closing) {
                return Ok(());
            }
            for event in suffix {
                if let Ok(next) = session.apply(event) {
                    session = next;
                }
            }
            prop_assert!(matches!(
                session.state(),
                SessionState::Closing | SessionState::Closed
            ));
        }
    }

    #[test]
    fn a_backwards_clock_reports_no_elapsed_time_rather_than_wrapping() {
        let later = Instant::from_duration(std::time::Duration::from_secs(10));
        let earlier = Instant::from_duration(std::time::Duration::from_secs(1));
        assert_eq!(later.since(earlier).as_secs(), 9);
        assert_eq!(earlier.since(later), std::time::Duration::ZERO);
    }

    #[test]
    fn advancing_saturates_instead_of_overflowing() {
        let base = Instant::from_duration(std::time::Duration::MAX);
        let advanced = base.advanced(std::time::Duration::from_secs(1));
        assert_eq!(advanced, base);
    }

    #[test]
    fn ordinary_work_is_admitted_only_where_the_specification_says() {
        assert!(!SessionState::Uninitialised.admits_ordinary_work());
        assert!(!SessionState::Initialising.admits_ordinary_work());
        assert!(SessionState::Operating.admits_ordinary_work());
        assert!(!SessionState::Closing.admits_ordinary_work());
        assert!(!SessionState::Closed.admits_ordinary_work());
        assert!(SessionState::Stateless.admits_ordinary_work());
    }

    /// The redaction is the property the type documents, so it is asserted
    /// against values that would be unmistakable if they leaked: a small one, a
    /// large one, and the extremes.
    #[test]
    fn debug_never_renders_the_value() {
        for value in [0_u64, 7, 42, u64::from(u32::MAX), u64::MAX] {
            let rendered = format!("{:?}", SessionId::new(value));
            assert_eq!(
                rendered, "SessionId(…)",
                "value {value} must not reach Debug"
            );
            assert!(
                !rendered.contains(&value.to_string()),
                "Debug leaked the value {value}: {rendered}"
            );
        }
    }

    /// `Debug` is what a failing `assert_eq!` and a panic expand to, so that is
    /// the path a session id actually escapes through by accident. Rendered
    /// explicitly, because the interesting case never appears in a passing suite.
    #[test]
    fn two_ids_are_indistinguishable_in_debug() {
        let rendered = format!("{:?} {:?}", SessionId::new(11), SessionId::new(12));
        assert_eq!(rendered, "SessionId(…) SessionId(…)");
        assert!(
            !rendered.contains('1'),
            "a digit escaped into Debug: {rendered}"
        );
    }

    /// `Display` stays full, and this is the reason the exemption is allowed:
    /// idempotency keys are composed from it, so a redacted form would make
    /// every session share one key.
    #[test]
    fn display_is_still_the_full_identity() {
        assert_eq!(SessionId::new(42).to_string(), "session#42");
    }

    /// `McpSessionId` derives `Debug` from `SessionId` and so inherits the
    /// redaction. Asserted rather than assumed: the property rests on that
    /// derive, and deleting it would silently re-expose the internal counter.
    #[test]
    fn the_mcp_session_id_inherits_the_redaction() {
        let mcp = crate::domain::McpSessionId::new(SessionId::new(0xDEAD_BEEF));
        let rendered = format!("{mcp:?}");
        assert!(
            !rendered.contains("deadbeef") && !rendered.contains("3735928559"),
            "the MCP session id leaked its value through Debug: {rendered}"
        );
    }
}
