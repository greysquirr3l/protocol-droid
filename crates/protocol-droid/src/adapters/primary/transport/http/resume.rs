//! Turning a `Last-Event-ID` header into a decision, before a byte is written.
//!
//! # Why this is a separate type
//!
//! Because the decision and the writing fail differently. Writing a stream is
//! a loop over a socket with a poll in it: it is hard to test exhaustively and
//! a mistake in it is invisible until a real client reconnects mid-stream. The
//! decision — *given this cursor, does the client get a replay, an open stream,
//! a notice, or a refusal* — has no I/O in it at all and can be checked on
//! every branch. So the half that can be wrong quietly is the half that is not
//! entangled with the half that is loud.
//!
//! [`plan`] takes the header and the ring and returns a [`Resume`]. The router
//! in the next dispatch matches on it, writes [`Resume::pending`], and then
//! goes live. Nothing here owns a stream, polls, or names a protocol.
//!
//! # The gap is the load-bearing case
//!
//! The ring drops old events to stay bounded. A client whose cursor is older
//! than the drop point asks for a stream the server cannot produce, and there
//! is a tempting way to answer that is wrong: send what is still held. The
//! client receives a contiguous-looking stream, believes it is current, and the
//! events that were dropped in between are gone with no error anywhere. That is
//! a silent data loss, and it is the failure this module exists to make
//! impossible to write by accident.
//!
//! So the gap gets its own arm, its own wire form, and its own test. What goes
//! on the wire is a **single control frame, and it carries no `id:`**:
//!
//! ```text
//! data: {"type":"resume-gap","evictedThrough":3,"resumeFrom":4,"lastEventId":9}
//!
//! ```
//!
//! Three things about that line are deliberate, and each of them is a way the
//! alternative is worse:
//!
//! 1. **It is a frame, not a close.** Closing the stream leaves the client to
//!    infer *why* from an absent stream, and a client that cannot tell a gap
//!    from a network failure retries blindly forever. The notice says what
//!    happened in the client's own terms.
//! 2. **It is not a keep-alive comment.** A `:`-prefixed comment dispatches
//!    nothing, so a client would learn the gap never happened. An event with
//!    data is dispatched, so it is seen.
//! 3. **It has no `id:`, deliberately.** The `id:` field is the client's resume
//!    cursor, and the browser sets the last-event-id buffer from it whether or
//!    not an event is dispatched. An encoder that put *any* id here would be
//!    making the server assert that the client holds events it demonstrably
//!    does not: `id: <last>` would skip everything up to the newest, and
//!    `id: <evictedThrough>` would claim the client received the very events
//!    that were dropped. Leaving the cursor untouched keeps the client's next
//!    reconnect honest, and the notice hands it the one number that lets it
//!    recover — `resumeFrom`.
//!
//! A client can therefore always tell the cases apart, which is the property
//! the whole shape rests on: `Live` writes *nothing*, and `Gap` writes exactly
//! one frame whose body names itself. Neither can be mistaken for the other,
//! and neither can be mistaken for a successful replay.
//!
//! # Why `resumeFrom` is `evictedThrough + 1`
//!
//! The ring's watermark is inclusive of the dropped event itself — a client
//! quoting the id of a dropped event is asking for what comes *after* it, and
//! the ring cannot prove it kept the rest contiguous. So `evicted_through` is
//! still a gap, and the first cursor that is guaranteed to yield a complete
//! answer is the one after it. The notice publishes that cursor, which turns
//! an unrecoverable gap into a single retry the client can make correctly.
//!
//! # Protocol neutrality
//!
//! No method name, no tool, no revision, no session. The only wire vocabulary
//! here is the event-stream framing in [`SseEvent`] and the three integers above,
//! and a second protocol with a different envelope slots in beside this without
//! a refactor. The `type` member of the notice is the extension point, not a
//! method name: a peer branches on `"resume-gap"` because that is the string
//! this module writes, not because it is a method in any specification.


use serde_json::json;

use super::{FramePayload, Refusal, SseEvent};
use crate::domain::{EventId, EventRing, Replay, StreamEvent};

/// The notice a client gets when its cursor is behind the drop point, reduced
/// to the member that names it.
///
/// Only reachable if the full notice fails to serialise, which a value built by
/// [`json!`] cannot do — its keys are strings by construction. It is here so
/// that the failure mode is a client that is still *told* there was a gap,
/// rather than one handed an empty stream that looks like a clean resume.
const MINIMAL_GAP_NOTICE: &str = r#"{"type":"resume-gap"}"#;

/// What to do with a stream a client has asked to resume.
///
/// Four arms, and the first three are all ways of keeping the stream open. The
/// fourth is not: it means no stream is opened at all, and the client is
/// answered with an error it can branch on. A caller that treated it as a
/// fourth way of staying open would have invented a response the client never
/// receives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resume {
    /// Write these frames, then continue the live stream.
    ///
    /// The frames are the complete answer: everything after the client's
    /// cursor is in here, and every one of them carries its own `id:` so the
    /// client's next reconnect has a cursor to quote.
    Replay {
        /// The frames to write, oldest first.
        frames: Vec<SseEvent>,
        /// The newest event the stream has ever issued.
        last: EventId,
    },

    /// Write nothing, and hold the stream open.
    ///
    /// Not a close, and not a keep-alive comment: a client must not be able to
    /// mistake an empty write for an event, and the comment form dispatches
    /// nothing so a client would learn of no error — but a *client that asked
    /// to resume and is already current* has lost nothing, so the honest answer
    /// is silence until something real happens.
    Live {
        /// The newest event the stream has ever issued.
        last: EventId,
    },

    /// Write [`notice`](Self::Gap::notice), then continue the live stream.
    ///
    /// The client is behind by more than the ring retains. It has lost events
    /// and cannot get them back, and this is how it finds out. See the module
    /// docs for why the notice carries no `id:`.
    Gap {
        /// The control frame to write before going live.
        notice: SseEvent,
        /// The newest event the ring has dropped. Everything at or before this
        /// is gone.
        evicted_through: EventId,
        /// The cursor that yields a complete answer on a retry.
        resume_from: EventId,
        /// The newest event the stream has ever issued.
        last: EventId,
    },

    /// A refusal. Reuses [`Refusal`] so the status code and the JSON-RPC body
    /// come from the one place that defines them.
    Refused(Refusal),
}

impl Resume {
    /// The frames to write before switching to the live stream.
    ///
    /// The write loop's whole job: write these, then go live. Empty for
    /// [`Live`](Self::Live) and [`Refused`](Self::Refused), one frame for
    /// [`Gap`](Self::Gap), and the whole window for [`Replay`](Self::Replay).
    ///
    /// Note that a refusal is `[]` *and* the writer must not open the stream
    /// for it, which is why this is not enough on its own to drive the
    /// response — a caller matches on the arm to get the status.
    #[must_use]
    pub fn pending(&self) -> &[SseEvent] {
        match self {
            Self::Replay { frames, .. } => frames,
            Self::Gap { notice, .. } => core::slice::from_ref(notice),
            Self::Live { .. } | Self::Refused(_) => &[],
        }
    }

    /// The newest event id the stream has issued, when the decision has one.
    ///
    /// `None` only for a refusal, which is not a stream at all. Useful for a
    /// log line or a metric, both of which want the watermark whether the
    /// client got a replay, an open stream, or a notice.
    #[must_use]
    pub const fn last(&self) -> Option<EventId> {
        match self {
            Self::Replay { last, .. } | Self::Live { last } | Self::Gap { last, .. } => Some(*last),
            Self::Refused(_) => None,
        }
    }
}

/// Decides what a client asking to resume gets, given the header it sent.
///
/// A missing header means "I have seen nothing", which is different from a
/// corrupt one and is passed to the ring as "everything". A header that is
/// present and unreadable is a refusal, decided before the ring is consulted at
/// all: a client quoting a cursor this server cannot parse deserves to be told,
/// and quietly restarting it from the top would hand it a stream it believes
/// it has already seen.
///
/// # Example
///
/// ```
/// use protocol_droid::adapters::primary::transport::http::Resume;
///
/// let decision = Resume::Live {
///     last: protocol_droid::domain::EventId::new(9),
/// };
/// assert!(decision.pending().is_empty());
/// assert_eq!(decision.last().map(|id| id.get()), Some(9));
/// ```
#[must_use]
pub fn plan<T: FramePayload>(ring: &EventRing<T>, header: Option<&str>) -> Resume {
    let Some(header) = header else {
        return decide(&ring.replay_from(None));
    };
    parse_cursor(header).map_or(Resume::Refused(Refusal::MalformedCursor), |cursor| {
        decide(&ring.replay_from(Some(cursor)))
    })
}

/// Maps a ring's answer to a decision, for a caller that already holds one.
///
/// Separate from [`plan`] because the mapping is worth testing on its own, and
/// because a caller that has already asked the ring — to log
/// [`Replay::is_complete`], say — should not ask it twice.
#[must_use]
pub fn decide<T: FramePayload>(replay: &Replay<'_, T>) -> Resume {
    match replay {
        Replay::UpToDate { last } => Resume::Live { last: *last },
        Replay::Buffered { events, last } => buffered(events, *last),
        Replay::Gap {
            evicted_through,
            last,
            ..
        } => gap(*evicted_through, *last),
        Replay::Unknown { requested, last } => Resume::Refused(Refusal::UnknownCursor {
            requested: *requested,
            last: *last,
        }),
    }
}

/// Reads a header value as a cursor, using the event-stream parser's rules.
///
/// The rules are [`SseEvent`]'s because they are already the rules for a
/// numeric `id:` field, and a second parser would be a second opinion about
/// what an id is. The header is wrapped in a throwaway block with a data line,
/// because the standard dispatches nothing for a block with no data and this
/// wants the parsed id rather than a dispatched event.
///
/// Returns `None` for anything the parser rejects, which includes the empty
/// string: a header that is present and empty is a client that quoted a
/// cursor, and it quoted nothing.
fn parse_cursor(header: &str) -> Option<EventId> {
    // A line terminator in the value is refused rather than parsed, and the
    // parser cannot catch it: the block below is built by concatenation, so a
    // newline in the value would start a *new field* in a block this function
    // assembled. `1\nid: 99` would then be read as the cursor `99` — the
    // untrusted value rewriting the id of a block the server built itself.
    // HTTP forbids this in a real request, but this function is a `&str` over
    // arbitrary text and does not rely on a caller having checked.
    if header.contains(['\n', '\r']) {
        return None;
    }
    let block = format!("id: {header}\ndata: {{}}\n");
    // The second `?` is the block dispatching nothing, which the block above
    // cannot do — it always carries a data line. Answering "unreadable" rather
    // than treating it as a fresh client keeps that hypothetical fail-secure
    // instead of fail-open, and an `id()` of `None` lands in the same place.
    SseEvent::dispatch(&block).ok()??.id()
}

/// Turns a complete window into frames, keeping every `id:`.
fn buffered<T: FramePayload>(events: &[StreamEvent<T>], last: EventId) -> Resume {
    let mut frames = Vec::with_capacity(events.len());
    for event in events {
        match event.payload().to_frame(event.id()) {
            Ok(frame) => frames.push(frame),
            // A payload with no JSON form — a non-string map key — has no wire
            // representation, and this is the server's bug rather than the
            // client's, so it is the one refusal here that is a 500.
            Err(_) => {
                return Resume::Refused(Refusal::Handler(
                    "an event payload has no JSON form".to_owned(),
                ));
            }
        }
    }
    Resume::Replay { frames, last }
}

/// Builds the gap notice and the watermarks the client recovers from.
fn gap(evicted_through: EventId, last: EventId) -> Resume {
    let resume_from = EventId::new(evicted_through.get().saturating_add(1));
    let notice = gap_notice(evicted_through, resume_from, last);
    Resume::Gap {
        notice,
        evicted_through,
        resume_from,
        last,
    }
}

/// The control frame that tells a client it missed events.
///
/// `without_id` and not [`SseEvent::json`], and the reason is the load-bearing
/// one from the module docs: `json` cannot produce a frame without an id, and
/// this frame must not have one. There is nothing here to serialise but three
/// integers this function already holds, so the payload is built directly and
/// the `json` constructor — which is right for a *payload*, where the id is
/// the event's own — is not the right tool for a control frame.
fn gap_notice(evicted_through: EventId, resume_from: EventId, last: EventId) -> SseEvent {
    let value = json!({
        "type": "resume-gap",
        "evictedThrough": evicted_through.get(),
        "resumeFrom": resume_from.get(),
        "lastEventId": last.get(),
    });
    let data = serde_json::to_string(&value).unwrap_or_else(|_| MINIMAL_GAP_NOTICE.to_owned());
    SseEvent::without_id(data)
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroUsize;

    use axum::http::StatusCode;
    use serde_json::{Value, json};

    use super::{Resume, decide, plan};
    use crate::adapters::primary::transport::http::{Refusal, SseEvent};
    use crate::domain::{EventId, EventRing};

    type TestError = Box<dyn std::error::Error>;

    /// A ring holding `values` and nothing dropped, so a quote inside it is a
    /// complete answer.
    ///
    /// The capacity is the count rather than a constant: a ring that is not
    /// full has no drop point, and a test about the *absence* of a gap is only
    /// meaningful if the ring genuinely cannot produce one.
    fn ring_of(values: &[Value]) -> Result<EventRing<Value>, TestError> {
        let capacity =
            NonZeroUsize::new(values.len().max(1)).ok_or("a ring needs a non-zero capacity")?;
        let mut ring = EventRing::new(capacity);
        for value in values {
            ring.push(value.clone());
        }
        Ok(ring)
    }

    /// Three events and the ids the ring issues for them.
    fn three_events() -> Vec<Value> {
        vec![json!({"seq": 1}), json!({"seq": 2}), json!({"seq": 3})]
    }

    /// The ids on the frames a decision wants written, for the id-preservation
    /// assertions. `None` is a real result here and is kept: a frame with no
    /// cursor is a meaningful outcome, not a missing value.
    fn frame_ids(decision: &Resume) -> Vec<Option<EventId>> {
        decision.pending().iter().map(SseEvent::id).collect()
    }

    /// Walks a decoded body, because `Value`'s `Index` panics on a missing key
    /// and a test that panics cannot tell a wrong expectation from a wrong
    /// body.
    fn at<'a>(body: &'a Value, path: &[&str]) -> Option<&'a Value> {
        let mut node = body;
        for key in path {
            node = node.get(key)?;
        }
        Some(node)
    }

    /// The slug a refusal reports, which is the member a client branches on.
    ///
    /// `as_str` and not `to_string`: the body is a [`Value`], whose `Display`
    /// is JSON, so formatting the slug would hand back `"unknown_cursor"`
    /// *including the quotes*. Reading it as a string also proves the client
    /// receives a JSON string and not a bare word. Owned because the body is
    /// built fresh by `body()` and does not outlive this call.
    fn slug_of(refusal: &Refusal) -> Option<String> {
        let body = refusal.body();
        at(&body, &["error", "data", "error"])
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    }

    /// A client that has never seen the stream gets the whole window.
    ///
    /// The mutation this kills is treating "no header" as a corrupt header and
    /// refusing a brand-new client, or — worse — reading it as the watermark
    /// and answering `UpToDate`, which would connect a fresh client to a
    /// stream that has already said everything and never replays it.
    #[test]
    fn a_fresh_client_with_no_header_gets_the_whole_window() -> Result<(), TestError> {
        let ring = ring_of(&three_events())?;
        let decision = plan(&ring, None);
        let Resume::Replay { frames, last } = &decision else {
            return Err(format!("expected a replay, got {decision:?}").into());
        };
        assert_eq!(frames.len(), 3);
        assert_eq!(*last, EventId::new(3));
        Ok(())
    }

    /// A client sitting exactly at the newest id is already current.
    ///
    /// The mutation this kills is answering `UpToDate` with a close, or with a
    /// synthetic event to fill the space. Both are wrong in a way the client
    /// can act on: a close is indistinguishable from a server that went away,
    /// and a synthetic event advances the client's cursor over a position no
    /// real event ever occupied.
    #[test]
    fn a_client_exactly_at_the_end_is_held_open_and_writes_nothing() -> Result<(), TestError> {
        let ring = ring_of(&three_events())?;
        let decision = plan(&ring, Some("3"));
        assert_eq!(
            decision,
            Resume::Live {
                last: EventId::new(3)
            }
        );
        assert!(decision.pending().is_empty());
        assert_eq!(decision.last(), Some(EventId::new(3)));
        Ok(())
    }

    /// A client behind the drop point is told, with a cursor it can recover on.
    ///
    /// This is the data-loss case. The mutation it kills is the important one
    /// in this file: answering a gap with the retained window anyway, or with
    /// silence, so the client receives a stream that looks contiguous while
    /// events 2 and 3 are simply gone. The second assertion is the half that
    /// makes the first honest — the `resume_from` this publishes has to be a
    /// cursor the ring actually serves completely, or the notice is advice the
    /// client cannot follow.
    #[test]
    fn a_client_quoting_a_dropped_event_is_told_it_missed_them() -> Result<(), TestError> {
        let mut ring = EventRing::new(NonZeroUsize::new(2).ok_or("capacity")?);
        for seq in 1..=5 {
            ring.push(json!({ "seq": seq }));
        }
        // The ring holds two of five, so ids 1..=3 are gone.
        let Resume::Gap {
            evicted_through,
            resume_from,
            last,
            ..
        } = plan(&ring, Some("1"))
        else {
            return Err("expected a gap".into());
        };
        assert_eq!(evicted_through, EventId::new(3));
        assert_eq!(resume_from, EventId::new(4));
        assert_eq!(last, EventId::new(5));

        // The published recovery cursor is one the ring answers completely.
        let retry = plan(&ring, Some(&resume_from.get().to_string()));
        let Resume::Replay { .. } = &retry else {
            return Err(format!("expected a complete replay, got {retry:?}").into());
        };
        assert_eq!(frame_ids(&retry), vec![Some(EventId::new(5))]);
        Ok(())
    }

    /// The gap notice is dispatched, and it does not touch the cursor.
    ///
    /// Three properties in one test because they are one property: a client
    /// must be able to tell this apart from [`Resume::Live`], and every way of
    /// failing to tells it the wrong thing. A leading `:` would make it a
    /// comment that dispatches nothing, so the client would never learn it had
    /// lost anything; a leading `id:` would move the client's resume cursor to
    /// a position it never received an event for, which is the server asserting
    /// a delivery that did not happen.
    #[test]
    fn the_gap_notice_is_dispatched_and_carries_no_id() -> Result<(), TestError> {
        let mut ring = EventRing::new(NonZeroUsize::new(2).ok_or("capacity")?);
        for seq in 1..=5 {
            ring.push(json!({ "seq": seq }));
        }
        let decision = plan(&ring, Some("1"));
        let pending = decision.pending();
        let [notice] = pending else {
            return Err(format!("expected exactly one frame, got {pending:?}").into());
        };
        assert_eq!(notice.id(), None, "the gap notice must not move the cursor");
        assert_eq!(frame_ids(&decision), vec![None]);

        let encoded = notice.encode();
        assert!(
            !encoded.starts_with(':'),
            "a comment dispatches nothing, so the client would never see the gap"
        );
        assert!(
            !encoded.starts_with("id:"),
            "an id here would claim events the client never received"
        );
        assert!(encoded.starts_with("data:"));

        // The body names itself, which is what makes the case recoverable
        // rather than merely detectable.
        let body: Value = serde_json::from_str(notice.data())?;
        assert_eq!(at(&body, &["type"]), Some(&json!("resume-gap")));
        assert_eq!(at(&body, &["evictedThrough"]), Some(&json!(3)));
        assert_eq!(at(&body, &["resumeFrom"]), Some(&json!(4)));
        assert_eq!(at(&body, &["lastEventId"]), Some(&json!(5)));
        Ok(())
    }

    /// A cursor past the end is out of step, and is refused with both ends of
    /// the comparison so the client can see which way it is wrong.
    ///
    /// The mutation this kills is clamping a cursor past the end down to the
    /// newest id, which would answer `UpToDate` to a client that is not up to
    /// date and hand it a stream it silently never catches up on.
    #[test]
    fn a_client_quoting_an_id_past_the_end_is_refused() -> Result<(), TestError> {
        let ring = ring_of(&three_events())?;
        let decision = plan(&ring, Some("99"));
        assert_eq!(
            decision,
            Resume::Refused(Refusal::UnknownCursor {
                requested: EventId::new(99),
                last: EventId::new(3),
            })
        );
        let Resume::Refused(refusal) = &decision else {
            return Err("expected a refusal".into());
        };
        assert_eq!(refusal.status(), StatusCode::CONFLICT);
        assert_eq!(slug_of(refusal).as_deref(), Some("unknown_cursor"));
        assert!(decision.pending().is_empty());
        assert_eq!(decision.last(), None);
        Ok(())
    }

    /// A cursor that is not a number is refused, not guessed at.
    ///
    /// The mutation this kills is falling back to a fresh start, which is the
    /// worst one available: the client believes it has seen the stream, is sent
    /// the whole window again, and re-processes every event it already
    /// handled. It is also the reason this is a 400 and not a 500 — nothing is
    /// broken here but the client's header.
    #[test]
    fn a_corrupt_cursor_is_refused_rather_than_read_as_a_fresh_start() -> Result<(), TestError> {
        let ring = ring_of(&three_events())?;
        for corrupt in ["abc", "", "1x", " ", "-1", "99999999999999999999999"] {
            let decision = plan(&ring, Some(corrupt));
            assert_eq!(
                decision,
                Resume::Refused(Refusal::MalformedCursor),
                "expected {corrupt:?} to be refused"
            );
            let Resume::Refused(refusal) = &decision else {
                return Err("expected a refusal".into());
            };
            assert_eq!(refusal.status(), StatusCode::BAD_REQUEST);
            assert_eq!(slug_of(refusal).as_deref(), Some("malformed_cursor"));
            assert!(decision.pending().is_empty());
            assert_eq!(decision.last(), None);
        }
        Ok(())
    }

    /// A cursor containing a line terminator cannot inject a second field.
    ///
    /// The parse reuses the event-stream parser by wrapping the header in a
    /// block it assembles itself, so a newline in the value would start a new
    /// field *in that block* — and the untrusted value would be rewriting the
    /// id of a frame the server built. The mutation this kills is dropping the
    /// guard, which reads `1\nid: 99` as the cursor `99`.
    #[test]
    fn a_cursor_with_a_line_terminator_cannot_inject_a_second_id() -> Result<(), TestError> {
        let ring = ring_of(&three_events())?;
        for injected in ["1\nid: 99", "1\r\nid: 99", "abc\ndata: forged"] {
            let decision = plan(&ring, Some(injected));
            assert_eq!(
                decision,
                Resume::Refused(Refusal::MalformedCursor),
                "expected {injected:?} to be refused"
            );
        }
        Ok(())
    }

    /// Every replayed frame carries its own id, and the id is the event's.
    ///
    /// The mutation this kills is emitting the window with `without_id`, which
    /// reads correct in a log and breaks every subsequent reconnect: the client
    /// has no cursor to quote, so it reconnects from wherever it last was and
    /// is re-sent the same window forever. The assertion is on the ids
    /// themselves rather than on the text containing `id:`, so a frame carrying
    /// the *wrong* id fails too.
    #[test]
    fn every_replayed_frame_keeps_its_own_id() -> Result<(), TestError> {
        let ring = ring_of(&three_events())?;
        let decision = plan(&ring, Some("1"));
        assert_eq!(
            frame_ids(&decision),
            vec![Some(EventId::new(2)), Some(EventId::new(3))]
        );
        for frame in decision.pending() {
            assert!(frame.encode().starts_with("id: "));
        }
        Ok(())
    }

    /// A payload containing a newline survives the wire.
    ///
    /// A JSON payload can never contain a *raw* newline — the encoder escapes
    /// it — so this asserts the escaped form survives the whole trip: encoded,
    /// decoded by the standard's own parser, and parsed back to a value whose
    /// string holds a real newline. The mutation this kills is writing the
    /// payload into the frame with `Debug`, which emits `\n` for a newline and
    /// a literal `\u{7f}` for DEL — neither of which is JSON, and both of which
    /// arrive as the six characters a client sees.
    #[test]
    fn a_payload_containing_a_newline_survives_the_round_trip() -> Result<(), TestError> {
        let original = json!({ "text": "one\ntwo\nthree" });
        let ring = ring_of(core::slice::from_ref(&original))?;
        let decision = plan(&ring, None);
        let [frame] = decision.pending() else {
            return Err(format!("expected one frame, got {decision:?}").into());
        };

        // Exactly one `data:` line. The newline is escaped by the JSON
        // encoder, so the payload can never legitimately need a second — and an
        // encoder that wrote a raw one would have split the frame, which a
        // client reads as an event followed by a field named after the tail.
        let encoded = frame.encode();
        assert_eq!(
            encoded.matches("\ndata: ").count(),
            1,
            "the payload must occupy exactly one data line: {encoded:?}"
        );

        let decoded = SseEvent::decode(&encoded)?;
        let recovered: Value = serde_json::from_str(decoded.data())?;
        assert_eq!(recovered, original);
        assert_eq!(
            at(&recovered, &["text"]).and_then(Value::as_str),
            Some("one\ntwo\nthree")
        );
        Ok(())
    }

    /// A payload that is not a string goes out as JSON, not as text.
    ///
    /// The mutation this kills is `format!("{value:?}")` on the payload, which
    /// is the mistake `SseEvent::json` exists to make unreachable. Rust's
    /// `Debug` is not JSON: an object prints as `Object {"a": 1}`, which no
    /// client can parse, and it is still *valid-looking* enough that the
    /// failure shows up as a parse error on the far side rather than here.
    #[test]
    fn a_payload_that_is_not_a_string_is_written_as_json() -> Result<(), TestError> {
        let original = json!([1, "two", null, { "nested": true }]);
        let ring = ring_of(core::slice::from_ref(&original))?;
        let decision = plan(&ring, None);
        let [frame] = decision.pending() else {
            return Err(format!("expected one frame, got {decision:?}").into());
        };
        let data = frame.data();
        assert!(data.starts_with('['), "Debug would print `Object`/`Array`");
        assert!(!data.contains("Object"), "Debug leaked: {data}");
        assert_eq!(serde_json::from_str::<Value>(data)?, original);
        Ok(())
    }

    /// DEL is written as itself, not as a Rust escape.
    ///
    /// The specific character the module docs in `sse.rs` name, isolated
    /// because it is the one a general round-trip test will not catch: a
    /// newline round-trips through *both* `Debug` and JSON as the two
    /// characters `\n`, so it cannot tell the two encoders apart. DEL does —
    /// `Debug` writes the six characters `\u{7f}`, which is not a JSON escape
    /// sequence at all.
    #[test]
    fn a_delete_character_is_not_written_as_a_rust_escape() -> Result<(), TestError> {
        let original = json!({ "t": "\u{7f}" });
        let ring = ring_of(core::slice::from_ref(&original))?;
        let decision = plan(&ring, None);
        let [frame] = decision.pending() else {
            return Err(format!("expected one frame, got {decision:?}").into());
        };
        let data = frame.data();
        assert!(!data.contains("\\u{7f}"), "Debug escape leaked: {data}");
        assert!(data.contains('\u{7f}'), "DEL should be written literally");
        assert_eq!(serde_json::from_str::<Value>(data)?, original);
        Ok(())
    }

    /// The mapping is available on its own, for a caller holding a `Replay`.
    ///
    /// This is the seam Stage 2d uses to log [`Replay::is_complete`] before
    /// asking what to do about it, without asking the ring twice — so it needs
    /// to agree with [`plan`] on the arms rather than being a second opinion.
    #[test]
    fn the_mapping_agrees_with_the_whole_ring() -> Result<(), TestError> {
        let ring = ring_of(&three_events())?;
        for header in [None, Some("1"), Some("2"), Some("3"), Some("4")] {
            let planned = plan(&ring, header);
            let cursor = header.and_then(|raw| raw.parse::<u64>().ok());
            let mapped = cursor.map_or_else(
                || decide(&ring.replay_from(None)),
                |value| decide(&ring.replay_from(Some(EventId::new(value)))),
            );
            assert_eq!(planned, mapped, "disagreed on {header:?}");
        }
        Ok(())
    }
}
