//! The session's typed event history.
//!
//! # Why the events are typed rather than a string
//!
//! The CLI's record mode replays this history, and a replay that has to parse
//! a message to find out which transition it was is a replay that breaks when
//! somebody rewords a message. Each variant is a fact, and a fact can be
//! matched on.
//!
//! # Why the version is a `String`
//!
//! A protocol's version is that protocol's type: a date for MCP, a
//! `Major.Minor` for A2A. Storing it as a string keeps this enum free of a
//! dependency on either, and the engine renders it through [`Display`](fmt::Display)
//! on the way in. It is a lossy choice and a deliberate one — the log is for
//! asking "what happened", and the log answers that by naming the version
//! that was agreed, not by being able to compare it.
//!
//! # Redaction happens on the way out
//!
//! These events carry the full request id, because the record mode has to
//! correlate a notification with the request that caused it and a redacted id
//! cannot do that. A sink that writes them to a log rather than to a file
//! redacts at that boundary, which is where the redaction belongs — a value
//! that cannot be logged is a value a record cannot hold.

use core::fmt;
use std::sync::Arc;

use crate::domain::RequestId;
use crate::domain::session::{AbortReason, Instant, RequestOrigin, SessionId, SessionState};

/// One thing that happened to a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    /// The session came into existence.
    Opened {
        /// Which session.
        session: SessionId,
        /// When, as read from the injected clock.
        at: Instant,
    },
    /// A protocol agreed what it speaks.
    Negotiated {
        /// Which session.
        session: SessionId,
        /// When.
        at: Instant,
        /// The version, as that protocol spells it.
        version: String,
        /// Whether the peer asked for this one and got it, or was answered with
        /// something else. A bridge that was downgraded needs the difference,
        /// and `server/discover` advertises what is supported rather than what
        /// one client was given.
        echoed: bool,
    },
    /// The peer confirmed the handshake and ordinary work became admissible.
    Operating {
        /// Which session.
        session: SessionId,
        /// When.
        at: Instant,
    },
    /// A request became in flight.
    Registered {
        /// Which session.
        session: SessionId,
        /// When.
        at: Instant,
        /// Which request.
        id: RequestId,
        /// The method it named.
        method: Arc<str>,
        /// Which side issued it.
        origin: RequestOrigin,
    },
    /// A request finished and is no longer in flight.
    Completed {
        /// Which session.
        session: SessionId,
        /// When.
        at: Instant,
        /// Which request.
        id: RequestId,
        /// The method it named.
        method: Arc<str>,
    },
    /// A request was aborted before it finished.
    Aborted {
        /// Which session.
        session: SessionId,
        /// When.
        at: Instant,
        /// Which request.
        id: RequestId,
        /// The method it named.
        method: Arc<str>,
        /// Why.
        reason: AbortReason,
    },
    /// A request was refused before it ran.
    Refused {
        /// Which session.
        session: SessionId,
        /// When.
        at: Instant,
        /// Which request, when the message carried one.
        id: Option<RequestId>,
        /// The method it named.
        method: Arc<str>,
        /// The state the session was in, which is what made the refusal.
        state: SessionState,
    },
    /// The session left the operating state, for any reason.
    StateChanged {
        /// Which session.
        session: SessionId,
        /// When.
        at: Instant,
        /// Where it went.
        state: SessionState,
    },
}

impl SessionEvent {
    /// Which session this concerns.
    #[must_use]
    pub const fn session(&self) -> SessionId {
        match self {
            Self::Opened { session, .. }
            | Self::Negotiated { session, .. }
            | Self::Operating { session, .. }
            | Self::Registered { session, .. }
            | Self::Completed { session, .. }
            | Self::Aborted { session, .. }
            | Self::Refused { session, .. }
            | Self::StateChanged { session, .. } => *session,
        }
    }

    /// When it happened, as read from the injected clock.
    #[must_use]
    pub const fn at(&self) -> Instant {
        match self {
            Self::Opened { at, .. }
            | Self::Negotiated { at, .. }
            | Self::Operating { at, .. }
            | Self::Registered { at, .. }
            | Self::Completed { at, .. }
            | Self::Aborted { at, .. }
            | Self::Refused { at, .. }
            | Self::StateChanged { at, .. } => *at,
        }
    }
}

impl fmt::Display for SessionEvent {
    /// One line, for a human reading a trace.
    ///
    /// The request id is redacted here rather than at the sink, because this is
    /// the form that reaches a terminal; a sink writing the event *value* is
    /// the record mode and needs the real id to correlate.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Opened { session, at } => write!(formatter, "{at} {session} opened"),
            Self::Negotiated {
                session,
                at,
                version,
                echoed,
            } => write!(
                formatter,
                "{at} {session} negotiated {version} ({})",
                if *echoed { "echoed" } else { "counter-offered" }
            ),
            Self::Operating { session, at } => write!(formatter, "{at} {session} operating"),
            Self::Registered {
                session,
                at,
                id,
                method,
                origin,
            } => write!(
                formatter,
                "{at} {session} {} {} as {}",
                origin_label(*origin),
                method,
                id.redacted()
            ),
            Self::Completed {
                session,
                at,
                id,
                method,
            } => write!(
                formatter,
                "{at} {session} completed {} as {}",
                method,
                id.redacted()
            ),
            Self::Aborted {
                session,
                at,
                id,
                method,
                reason,
            } => write!(
                formatter,
                "{at} {session} aborted {} ({reason}) as {}",
                method,
                id.redacted()
            ),
            Self::Refused {
                session,
                at,
                id,
                method,
                state,
            } => write!(
                formatter,
                "{at} {session} refused {method} while {state}{}",
                id.as_ref()
                    .map_or_else(String::new, |id| format!(" as {}", id.redacted()))
            ),
            Self::StateChanged { session, at, state } => {
                write!(formatter, "{at} {session} is {state}")
            }
        }
    }
}

/// The word an origin gets in a log line.
const fn origin_label(origin: RequestOrigin) -> &'static str {
    match origin {
        RequestOrigin::Inbound => "received",
        RequestOrigin::Outbound => "sent",
    }
}

/// Where a session's history goes.
///
/// Declared by the engine, its only consumer. One method and no result: a log
/// that can fail is a log the engine has to make a decision about mid-request,
/// and the only correct decision — dropping the event — is one that hides the
/// fact that it dropped it. A sink that cannot accept an event says so by
/// not taking the type.
pub trait EventLog {
    /// Appends one event.
    fn record(&self, event: SessionEvent);
}

/// An event log that goes nowhere.
///
/// Not a default parameter and not a blanket impl: choosing to discard history
/// is a decision a caller makes at the call site, and the type makes it
/// visible there rather than burying it in a default.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullEventLog;

impl EventLog for NullEventLog {
    fn record(&self, _event: SessionEvent) {}
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::{EventLog, NullEventLog, SessionEvent};
    use crate::domain::RequestId;
    use crate::domain::session::{
        AbortReason, CancelReason, Instant, RequestOrigin, SessionId, SessionState,
    };

    fn at() -> Instant {
        Instant::from_duration(Duration::from_millis(1_500))
    }

    fn session() -> SessionId {
        SessionId::new(4)
    }

    /// The redaction boundary. An event *value* keeps the real id so a record
    /// can correlate; its `Display` form does not, so a terminal line cannot
    /// disclose a peer-chosen string.
    #[test]
    fn the_display_form_redacts_but_the_value_does_not() {
        let id = RequestId::text("super-secret-credential");
        let event = SessionEvent::Completed {
            session: session(),
            at: at(),
            id: id.clone(),
            method: Arc::from("tools/call"),
        };
        let line = event.to_string();
        assert!(!line.contains("super-secret-credential"), "{line}");
        assert!(line.contains(&id.redacted()), "{line}");
        // The value is untouched, which is the half the record mode needs.
        assert_matches::assert_matches!(&event, SessionEvent::Completed { id: kept, .. } if *kept == id);
    }

    #[test]
    fn every_variant_renders_a_line() {
        let id = RequestId::number(1);
        let events = [
            SessionEvent::Opened {
                session: session(),
                at: at(),
            },
            SessionEvent::Negotiated {
                session: session(),
                at: at(),
                version: String::from("2025-06-18"),
                echoed: true,
            },
            SessionEvent::Negotiated {
                session: session(),
                at: at(),
                version: String::from("2026-07-28"),
                echoed: false,
            },
            SessionEvent::Operating {
                session: session(),
                at: at(),
            },
            SessionEvent::Registered {
                session: session(),
                at: at(),
                id: id.clone(),
                method: Arc::from("tools/call"),
                origin: RequestOrigin::Inbound,
            },
            SessionEvent::Registered {
                session: session(),
                at: at(),
                id: id.clone(),
                method: Arc::from("sampling/createMessage"),
                origin: RequestOrigin::Outbound,
            },
            SessionEvent::Completed {
                session: session(),
                at: at(),
                id: id.clone(),
                method: Arc::from("tools/call"),
            },
            SessionEvent::Aborted {
                session: session(),
                at: at(),
                id: id.clone(),
                method: Arc::from("tools/call"),
                reason: AbortReason::Cancelled(Some(CancelReason::new("user"))),
            },
            SessionEvent::Refused {
                session: session(),
                at: at(),
                id: Some(id),
                method: Arc::from("tools/call"),
                state: SessionState::Uninitialised,
            },
            // A notification has no id, and the line has to read without one.
            SessionEvent::Refused {
                session: session(),
                at: at(),
                id: None,
                method: Arc::from("notifications/initialized"),
                state: SessionState::Operating,
            },
            SessionEvent::StateChanged {
                session: session(),
                at: at(),
                state: SessionState::Closed,
            },
        ];
        for event in &events {
            let line = event.to_string();
            assert!(line.contains("session#4"), "{line}");
            assert!(!line.is_empty());
        }
    }

    #[test]
    fn the_null_log_accepts_and_discards() {
        let log = NullEventLog;
        log.record(SessionEvent::Opened {
            session: session(),
            at: at(),
        });
    }
}
