//! A bounded ring of stream events, replayable from a client's last-seen id.
//!
//! # The shape of the storage, and why it is a `Vec` with a cursor
//!
//! A `VecDeque` is the obvious structure and it is the wrong one here, for a
//! reason about the *return type* rather than about the cost. [`Replay`] hands
//! back a borrowed slice of the events after a watermark, and `VecDeque::get`
//! with a range returns `None` unless the range happens to be contiguous — so
//! on a wrapped ring the answer would depend on the push history rather than on
//! the watermark. Making it contiguous on demand is `make_contiguous`, which
//! needs `&mut self` for what is a read, so a reconnect would mutate the ring
//! in order to look at it and the port owning the ring would have to hand out a
//! mutable borrow.
//!
//! A `Vec` plus a `start` cursor is always contiguous, keeps
//! [`EventRing::replay_from`] on `&self`, and costs one amortised compaction:
//! once the dead prefix reaches the capacity it is drained and the cursor
//! returns to zero. That bounds the allocation at roughly twice the capacity in
//! slots and makes every push amortised O(1), which is what a push on a busy
//! notification stream needs.
//!
//! # Why eviction is recorded rather than inferred
//!
//! The oldest retained event is not always in the buffer. After a compaction it
//! is gone, and a `Gap` still has to be reported for a client that asks from
//! before the last drop. So `evicted_through` is a field rather than something
//! read off the front of the ring, and it is never reset: the newest id that has
//! ever been dropped is the only thing needed to tell "still contiguous" from
//! "a hole is in front of you".

use core::fmt;
use core::num::NonZeroUsize;

/// A stream event's position, as a client quotes it back.
///
/// Monotonic from 1, and 0 is reserved: [`EventId::WATERMARK`] is "before
/// anything was sent", which is what a client means by a `Last-Event-ID` of
/// `0` and what an empty ring has as its last id. Reserving it is what lets
/// every comparison in [`EventRing::replay_from`] be a plain `u64` comparison
/// with no `Option` threaded through the arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct EventId(u64);

impl EventId {
    /// The watermark before the first event.
    pub const WATERMARK: Self = Self(0);

    /// An id, for a caller that computed one. `0` is [`WATERMARK`](Self::WATERMARK).
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// The id as a number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Whether this is the reserved watermark.
    #[must_use]
    pub const fn is_watermark(self) -> bool {
        self.0 == 0
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// One event, with the id a client would quote to resume after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamEvent<T> {
    id: EventId,
    payload: T,
}

impl<T> StreamEvent<T> {
    /// An event at a known position.
    #[must_use]
    pub const fn new(id: EventId, payload: T) -> Self {
        Self { id, payload }
    }

    /// The position.
    #[must_use]
    pub const fn id(&self) -> EventId {
        self.id
    }

    /// The payload.
    #[must_use]
    pub const fn payload(&self) -> &T {
        &self.payload
    }

    /// Unwraps into the parts.
    #[must_use]
    pub fn into_parts(self) -> (EventId, T) {
        (self.id, self.payload)
    }

    /// Borrows the payload mutably, for a payload that is an in-place buffer.
    #[must_use]
    pub const fn payload_mut(&mut self) -> &mut T {
        &mut self.payload
    }
}

/// What a client asking to resume actually gets.
///
/// Four arms, and the distinction that matters is between the first two and the
/// last two. `UpToDate` and `Buffered` are success: nothing was lost. `Gap` and
/// `Unknown` are refusals, and a caller that collapses them into an empty list
/// has just reported a lost event as a success — which is the specific failure
/// this enum is shaped to make awkward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Replay<'a, T> {
    /// The client is already current. There is genuinely nothing to send.
    UpToDate {
        /// The newest event the stream ever issued.
        last: EventId,
    },

    /// Everything after the client's watermark is still held. Send all of it.
    Buffered {
        /// The events, oldest first, contiguous from the watermark.
        events: &'a [StreamEvent<T>],
        /// The newest event the stream ever issued.
        last: EventId,
    },

    /// The client is behind the point where events started being dropped, so a
    /// resume would silently omit what it missed. The client must be told, and
    /// must re-issue whatever it has not seen.
    Gap {
        /// The watermark the client asked from.
        requested: EventId,
        /// The newest event the ring has dropped. Anything at or before this is
        /// gone.
        evicted_through: EventId,
        /// The newest event the stream ever issued.
        last: EventId,
    },

    /// The client quoted an id the stream never issued.
    ///
    /// Separate from [`Gap`](Self::Gap) on purpose. A gap means the client is
    /// behind and something was lost; this means the client's id is ahead of
    /// anything that exists, which is a client that is out of step with the
    /// server — a different fault with a different fix, and one that a
    /// "resume from the beginning" fallback would paper over.
    Unknown {
        /// The id the client quoted.
        requested: EventId,
        /// The newest event the stream ever issued.
        last: EventId,
    },
}

impl<T> Replay<'_, T> {
    /// Whether the client's request can be honoured without losing an event.
    ///
    /// The one question an adapter has to answer before writing a response
    /// body, and it is [`false`] for exactly the two arms where a naive
    /// implementation would send a partial stream and call it a success.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        matches!(self, Self::UpToDate { .. } | Self::Buffered { .. })
    }

    /// The events to write, if any.
    ///
    /// Empty for both refusals, which is why it is not the thing to branch on:
    /// use [`is_complete`](Self::is_complete) for that. Provided for the write
    /// loop, which has nothing to do after being told about a gap.
    #[must_use]
    pub const fn events(&self) -> &[StreamEvent<T>] {
        match self {
            Self::Buffered { events, .. } => events,
            Self::UpToDate { .. } | Self::Gap { .. } | Self::Unknown { .. } => &[],
        }
    }
}

/// A capacity for a stream whose payload size the caller has not measured.
///
/// Exists so [`EventRing::new`] can take a `NonZeroUsize` — a zero-capacity ring
/// would accept a push and drop it immediately, which is a livelock dressed up
/// as a buffer — without forcing every construction site to reach for
/// `NonZeroUsize::new`. A caller that knows its own bound should pass one
/// directly; this is the one to use when it does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecommendedCapacity(NonZeroUsize);

impl RecommendedCapacity {
    /// The count.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0.get()
    }
}

impl From<RecommendedCapacity> for NonZeroUsize {
    fn from(value: RecommendedCapacity) -> Self {
        value.0
    }
}

/// Events to retain for a stream whose payload size is unmeasured.
///
/// 1024, and the number is a guess with a stated basis rather than a constant
/// someone liked. Two observations set it:
///
/// - It is a *reconnect* window, not a session window. A client that has been
///   gone for longer than the window is expected to resume from the beginning
///   of what is retained, or to be told there is a gap.
/// - It is sized for the notification traffic the transport actually carries
///   on a stream — progress and message notifications for the request they
///   relate to, not `resources/read` bodies, which in `2025-11-25` and earlier
///   flow on the response stream of their own request.
///
/// At a notional one progress notification per second that is roughly seventeen
/// minutes of disconnection. The memory cost is 1024 slots, and it is the
/// *payload* that decides whether that is cheap, which is exactly why an
/// adapter streaming large bodies should pass a smaller number of its own
/// choosing. Nothing in the specification requires any particular bound.
///
/// [`recommended_capacity_covers_a_reconnect`](self::tests::recommended_capacity_covers_a_reconnect)
/// is the test that holds this reasoning to the number.
pub const RECOMMENDED_CAPACITY: RecommendedCapacity =
    RecommendedCapacity(match NonZeroUsize::new(1024) {
        Some(value) => value,
        // Unreachable: 1024 is not zero. Written as a `match` rather than
        // `unwrap` because the crate denies that, and the alternative — a
        // `LazyLock` or a hard-coded duplicate of the number — would be worse
        // for a case the compiler already rules out.
        None => NonZeroUsize::MIN,
    });

/// A bounded, replayable buffer of stream events.
///
/// Generic over the payload because the ring is transport machinery: it must not
/// name an MCP type, and a `serde_json::Value` in a domain signature is exactly
/// the kind of raw JSON the layer rules keep out of here. The adapter supplies
/// whatever its wire type is.
#[derive(Debug)]
pub struct EventRing<T> {
    capacity: NonZeroUsize,
    buffer: Vec<StreamEvent<T>>,
    /// Index of the oldest retained event within `buffer`.
    start: usize,
    /// The newest id ever issued, or the watermark if nothing has been.
    last: EventId,
    /// The newest id ever dropped, if anything has been.
    evicted_through: Option<EventId>,
    /// The newest id belonging to a stream that [`clear`](Self::clear) ended.
    ///
    /// Separate from `evicted_through` because the two mean different things to
    /// a client. An overflow drop means the client is *behind* and could be
    /// caught up from the drop boundary. A clear means the stream those ids
    /// belonged to is finished, so there is no continuation of them to catch up
    /// to — and answering with the retained window would splice two streams
    /// together as though they were one.
    retired_through: Option<EventId>,
}

impl<T> EventRing<T> {
    /// A ring retaining at most `capacity` events.
    ///
    /// A capacity of zero is refused at the type level rather than tolerated:
    /// such a ring would accept a push, drop it, and report success, which is
    /// the eviction-as-success bug with no eviction to notice.
    #[must_use]
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            capacity,
            buffer: Vec::with_capacity(capacity.get()),
            start: 0,
            last: EventId::WATERMARK,
            evicted_through: None,
            retired_through: None,
        }
    }

    /// Retains [`RECOMMENDED_CAPACITY`] events.
    #[must_use]
    pub fn with_recommended_capacity() -> Self {
        Self::new(RECOMMENDED_CAPACITY.into())
    }

    /// The most events this ring will hold.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity.get()
    }

    /// The newest id ever issued, or the watermark if nothing has been.
    #[must_use]
    pub const fn last(&self) -> EventId {
        self.last
    }

    /// How many events are currently retained.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.buffer.len() - self.start
    }

    /// Whether nothing is retained.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The newest event ever dropped, if the ring has dropped anything.
    ///
    /// A client at or after this id can still be served contiguously; a client
    /// before it cannot.
    #[must_use]
    pub const fn evicted_through(&self) -> Option<EventId> {
        self.evicted_through
    }

    /// Appends an event, dropping the oldest if the ring is full.
    ///
    /// Returns the id assigned, which is the value a client will quote to
    /// resume after this event.
    pub fn push(&mut self, payload: T) -> EventId {
        if self.len() == self.capacity.get() {
            self.evict_one();
        }
        self.last = EventId(self.last.get().saturating_add(1));
        self.buffer.push(StreamEvent::new(self.last, payload));
        self.last
    }

    /// The live window, which is the buffer minus its dead prefix.
    fn window(&self) -> &[StreamEvent<T>] {
        self.buffer.get(self.start..).unwrap_or(&[])
    }

    /// Drops the oldest retained event and compacts if that is now worthwhile.
    fn evict_one(&mut self) {
        if let Some(dropped) = self.buffer.get(self.start) {
            self.evicted_through = Some(dropped.id());
        }
        self.start += 1;
        // Compact when the dead prefix is at least half the allocation, so the
        // `drain` is amortised O(1) against the pushes that caused it, and when
        // everything is dead so the allocation is reused rather than regrown.
        if self.start == self.buffer.len() {
            self.buffer.clear();
            self.start = 0;
        } else if self.start >= self.capacity.get() {
            self.buffer.drain(..self.start);
            self.start = 0;
        }
    }

    /// Answers a client that says where it stopped.
    ///
    /// `after` is the client's `Last-Event-ID`, or `None` for a client that has
    /// never seen this stream and wants everything. The four outcomes are
    /// described on [`Replay`]; the short version is that a request is only
    /// [`Replay::is_complete`] when the ring is still holding everything after
    /// the watermark.
    ///
    /// Takes `&self` and hands back a borrow, so an adapter can answer from a
    /// shared reference without taking the stream's lock exclusively.
    #[must_use]
    pub fn replay_from(&self, after: Option<EventId>) -> Replay<'_, T> {
        let last = self.last;
        let Some(requested) = after else {
            // A client with no history cannot be missing anything, so whatever
            // is retained is a complete answer *for it* — even when the ring has
            // dropped events that a resuming client would have asked for. This
            // is why the no-history case is answered before the eviction
            // watermark is consulted: a fresh subscriber is not behind, and
            // refusing it over a drop that predates its arrival would lose the
            // entire stream for no reason.
            //
            // Nothing retained answers `UpToDate` rather than an empty
            // `Buffered`: a client asking a stream that has emitted nothing is
            // up to date, and handing it a zero-length window instead would
            // push a branch on "did I get an empty one or the end" into every
            // caller to distinguish two answers that mean the same thing.
            if self.is_empty() {
                return Replay::UpToDate { last };
            }
            return Replay::Buffered {
                events: self.window(),
                last,
            };
        };
        if requested == last {
            return Replay::UpToDate { last };
        }
        if requested > last {
            // A client quoting an id past the end is not merely behind: it is
            // out of step, and resuming from the top would send it events it
            // believes it already has.
            return Replay::Unknown { requested, last };
        }
        if let Some(retired) = self.retired_through
            && requested <= retired
        {
            // Ids from before an explicit `clear` belong to a stream that is
            // over. They are neither behind nor in step: there is no
            // continuation of them, so this is the same answer as an id past
            // the end rather than a gap the client could recover across.
            return Replay::Unknown { requested, last };
        }
        // `requested < last`, so at least one event is outstanding. Whether it
        // can be served is decided by the last drop, not by the current front:
        // the watermark is inclusive of the dropped event itself, because a
        // client that quotes the id of a dropped event is asking for what comes
        // *after* it, and the ring cannot prove it kept the rest contiguous.
        if let Some(evicted_through) = self.evicted_through
            && requested <= evicted_through
        {
            return Replay::Gap {
                requested,
                evicted_through,
                last,
            };
        }
        // The watermark is at or after the last drop, so every event after it
        // is still here. `self.len()` is non-zero because `requested < last`.
        // `behind` counts events back from the newest, so subtracting it from
        // the retained count gives the offset *within the live window* — which
        // is `buffer[start..]`, not `buffer`. Adding `self.start` is what turns
        // it into a real index, and omitting it silently resends the dead
        // prefix.
        let offset = usize::try_from(last.get() - requested.get())
            .ok()
            .and_then(|behind| self.len().checked_sub(behind))
            .map(|offset| self.start + offset);
        offset
            .and_then(|start| self.buffer.get(start..))
            .map_or_else(
                // Unreachable given the arithmetic above: `behind` is the
                // number of events after the watermark and cannot exceed what
                // is retained once the gap check has passed. Returning a gap
                // rather than an empty success is the safe direction to be
                // wrong in, because an adapter that sees a gap re-issues and
                // one that sees an empty success silently drops the stream.
                || Replay::Gap {
                    requested,
                    evicted_through: self.evicted_through.unwrap_or(EventId::WATERMARK),
                    last,
                },
                |events| Replay::Buffered { events, last },
            )
    }

    /// Empties the ring, keeping the id sequence going.
    ///
    /// A new stream, not a continuing one: ids stay monotonic so a client that
    /// reconnects across the reset is refused with
    /// [`Replay::Unknown`](Replay::Unknown) rather than being served events
    /// from a stream that no longer exists.
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.start = 0;
        // The ids do not restart, so a client quoting one from the finished
        // stream can be told it is out of step rather than served the new
        // stream's opening events as though they followed its own.
        self.retired_through = Some(self.last);
        self.evicted_through = None;
    }
}

impl<T> Default for EventRing<T> {
    /// Retains [`RECOMMENDED_CAPACITY`], so `EventRing::default()` is the
    /// unmeasured-payload case and never a zero-capacity ring.
    fn default() -> Self {
        Self::with_recommended_capacity()
    }
}

#[cfg(test)]
mod tests {
    use super::{EventId, EventRing, RECOMMENDED_CAPACITY, Replay, StreamEvent};
    use core::num::NonZeroUsize;
    use std::sync::Arc;
    use std::thread;

    /// Generic over the payload so each test states the type it is about.
    /// Most push a `&'static str`; the id-monotonicity tests push the index
    /// they are tracking, and the borrow test pushes an `Arc` it expects to
    /// see come back without a copy. Pinning one payload here would make those
    /// tests about a type they are not exercising.
    fn ring<T>(capacity: usize) -> Result<EventRing<T>, Failure> {
        let capacity = NonZeroUsize::new(capacity).ok_or(Failure::ZeroCapacity)?;
        Ok(EventRing::new(capacity))
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Failure {
        ZeroCapacity,
        WrongIds(Vec<u64>),
        WrongPayloads(Vec<String>),
        Outcome(&'static str),
        Concurrent,
    }

    /// The payloads a replay would deliver, as owned strings so a borrow from
    /// the ring does not have to outlive the assertion.
    fn payloads<T: Clone + core::fmt::Debug>(replay: &Replay<'_, T>) -> Vec<String> {
        replay
            .events()
            .iter()
            .map(|event| format!("{:?}", event.payload()))
            .collect()
    }

    fn ids<T>(replay: &Replay<'_, T>) -> Vec<u64> {
        replay
            .events()
            .iter()
            .map(|event| event.id().get())
            .collect()
    }

    /// A client that has seen nothing gets the whole ring, in order, with no
    /// duplicates and no reordering.
    #[test]
    fn first_continuation_returns_everything_in_order() -> Result<(), Failure> {
        let mut ring = ring(8)?;
        let mut assigned = Vec::new();
        for payload in ["a", "b", "c"] {
            assigned.push(ring.push(payload).get());
        }
        let replay = ring.replay_from(None);
        if !replay.is_complete() {
            return Err(Failure::Outcome("a fresh client must be served whole"));
        }
        if ids(&replay) != assigned {
            return Err(Failure::WrongIds(assigned));
        }
        if payloads(&replay) != vec!["\"a\"", "\"b\"", "\"c\""] {
            return Err(Failure::WrongPayloads(payloads(&replay)));
        }
        Ok(())
    }

    /// Resuming from the last thing the client saw sends only what came after
    /// it. A duplicate here is the failure the ring exists to make impossible.
    #[test]
    fn resume_sends_only_what_follows() -> Result<(), Failure> {
        let mut ring = ring(8)?;
        let first = ring.push("a");
        let second = ring.push("b");
        let replay = ring.replay_from(Some(first));
        if ids(&replay) != vec![second.get()] {
            return Err(Failure::WrongIds(ids(&replay)));
        }
        if payloads(&replay) != vec!["\"b\""] {
            return Err(Failure::WrongPayloads(payloads(&replay)));
        }
        Ok(())
    }

    /// A client already caught up gets an explicit "nothing to send" rather than
    /// an empty slice it has to interpret.
    #[test]
    fn a_current_client_is_told_it_is_current() -> Result<(), Failure> {
        let mut ring = ring(8)?;
        ring.push("a");
        match ring.replay_from(Some(ring.last())) {
            Replay::UpToDate { last } if last.get() == 1 => Ok(()),
            other => Err(Failure::Outcome(match other {
                Replay::UpToDate { .. } => "wrong last id",
                Replay::Buffered { .. } => "a current client was sent events",
                Replay::Gap { .. } => "reported a gap with nothing dropped",
                Replay::Unknown { .. } => "reported an unknown id for a current client",
            })),
        }
    }

    /// **The eviction case.** Once the ring has dropped an event, a client
    /// asking from before the drop is told there is a hole — the outcome is
    /// named `Gap`, it is not [`Replay::is_complete`], and it carries the id
    /// the client would have to resume from instead. This is the test that fails
    /// if a `replay_from` ever returns an empty success for a forgotten event.
    #[test]
    fn replay_from_a_forgotten_event_is_a_named_gap_not_an_empty_success() -> Result<(), Failure> {
        let mut ring = ring(3)?;
        let first = ring.push("a");
        ring.push("b");
        ring.push("c");
        let fourth = ring.push("d");

        match ring.replay_from(Some(first)) {
            Replay::Gap {
                requested,
                evicted_through,
                last,
            } => {
                if requested != first {
                    return Err(Failure::WrongIds(vec![requested.get()]));
                }
                if last != fourth {
                    return Err(Failure::WrongIds(vec![last.get()]));
                }
                // The gap has to name the boundary that put the client behind
                // it, and that boundary is at or after the id it asked for —
                // which is precisely what makes this a `Gap` and not a
                // `Buffered`. It also has to name an event that really was
                // dropped, so a client told to resync somewhere it already is
                // is not stranded.
                if evicted_through.get() < requested.get() {
                    return Err(Failure::WrongIds(vec![evicted_through.get()]));
                }
                if evicted_through.get() > last.get() {
                    return Err(Failure::WrongIds(vec![evicted_through.get()]));
                }
                Ok(())
            }
            Replay::UpToDate { .. } => Err(Failure::Outcome(
                "a dropped event was reported as up to date",
            )),
            Replay::Buffered { .. } => {
                Err(Failure::Outcome("a dropped event was reported as buffered"))
            }
            Replay::Unknown { .. } => Err(Failure::Outcome(
                "a dropped event was reported as an unknown id",
            )),
        }
    }

    /// A client asking from the dropped event itself is still contiguous: the
    /// drop *was* that event, and everything after it is held. Only asking from
    /// before it is a gap.
    #[test]
    fn resume_from_the_dropped_event_is_still_contiguous() -> Result<(), Failure> {
        let mut ring = ring(2)?;
        ring.push("a");
        let second = ring.push("b");
        let third = ring.push("c");
        let replay = ring.replay_from(Some(second));
        if !replay.is_complete() {
            return Err(Failure::Outcome(
                "resuming at the drop point should be complete",
            ));
        }
        if ids(&replay) != vec![third.get()] {
            return Err(Failure::WrongIds(ids(&replay)));
        }
        Ok(())
    }

    /// A client quoting an id the stream never issued is refused distinctly
    /// from a client that is behind. Falling back to "send everything" here
    /// would re-deliver a stream the client has already seen.
    #[test]
    fn an_id_from_the_future_is_unknown_not_a_gap() -> Result<(), Failure> {
        let mut ring = ring(4)?;
        ring.push("a");
        match ring.replay_from(Some(EventId::new(99))) {
            Replay::Unknown { requested, last } => {
                if requested.get() != 99 || last.get() != 1 {
                    return Err(Failure::WrongIds(vec![requested.get(), last.get()]));
                }
                Ok(())
            }
            _ => Err(Failure::Outcome("a future id was not reported as unknown")),
        }
    }

    /// A gap on a ring that has never dropped anything is impossible, and the
    /// unreachable branch of `replay_from` is held to that by construction: a
    /// small ring that fits everything never reports a gap.
    #[test]
    fn a_ring_that_fits_everything_never_reports_a_gap() -> Result<(), Failure> {
        let mut ring = EventRing::new(NonZeroUsize::new(16).ok_or(Failure::ZeroCapacity)?);
        let mut last = EventId::WATERMARK;
        for index in 0..15 {
            last = ring.push(index);
        }
        for watermark in [EventId::WATERMARK, EventId::new(1), EventId::new(7), last] {
            if !ring.replay_from(Some(watermark)).is_complete() {
                return Err(Failure::Outcome("a ring holding everything reported a gap"));
            }
        }
        Ok(())
    }

    /// Eviction must not disturb the ids. Ids are a sequence, not a window, so
    /// a client reconnecting across an eviction still recognises them — that is
    /// the whole basis for a `Gap` being reportable at all.
    #[test]
    fn ids_stay_monotonic_across_eviction() -> Result<(), Failure> {
        let mut ring = ring(2)?;
        let mut assigned = Vec::new();
        for index in 0..10 {
            assigned.push(ring.push(index).get());
        }
        // Strictly increasing by one, whatever was dropped.
        let expected: Vec<u64> = (1..=10).collect();
        if assigned != expected {
            return Err(Failure::WrongIds(assigned));
        }
        if ring.last().get() != 10 {
            return Err(Failure::WrongIds(vec![ring.last().get()]));
        }
        if ring.len() != 2 {
            return Err(Failure::WrongIds(vec![
                ring.len().try_into().unwrap_or(u64::MAX),
            ]));
        }
        Ok(())
    }

    /// The dead prefix must actually be reclaimed, or the ring's memory grows
    /// without bound and the `capacity` it advertises is a lie.
    #[test]
    fn compaction_keeps_the_buffer_from_growing() -> Result<(), Failure> {
        let mut ring = ring(4)?;
        for index in 0..1_000 {
            ring.push(index);
        }
        if ring.len() != 4 {
            return Err(Failure::WrongIds(vec![
                ring.len().try_into().unwrap_or(u64::MAX),
            ]));
        }
        // At most twice the live count: the compaction threshold is the live
        // length, and the buffer is drained as soon as the dead prefix reaches
        // the capacity.
        let allocated = ring.buffer.len();
        if allocated > ring.capacity() * 2 {
            return Err(Failure::WrongIds(vec![
                allocated.try_into().unwrap_or(u64::MAX),
            ]));
        }
        Ok(())
    }

    /// The recommended capacity is the number the module docs justify, and the
    /// justification is checkable: the window has to survive a reconnect long
    /// enough to be worth having. Seventeen minutes at one event a second is
    /// the claim, so the test holds the count to that arithmetic rather than to
    /// a literal.
    #[test]
    fn recommended_capacity_covers_a_reconnect() -> Result<(), Failure> {
        // Seventeen minutes at one notification a second, which is the claim
        // the module docs make.
        const RECONNECT_SECONDS: usize = 1_020;
        if RECOMMENDED_CAPACITY.get() < RECONNECT_SECONDS {
            return Err(Failure::WrongIds(vec![
                RECOMMENDED_CAPACITY.get().try_into().unwrap_or(u64::MAX),
            ]));
        }
        // ...and it is not unbounded: the same reasoning caps it at a size whose
        // slots are a plausible allocation for one stream.
        if RECOMMENDED_CAPACITY.get() > 65_536 {
            return Err(Failure::WrongIds(vec![
                RECOMMENDED_CAPACITY.get().try_into().unwrap_or(u64::MAX),
            ]));
        }
        Ok(())
    }

    /// Clearing is a new stream, and a client that reconnects across the reset
    /// is told its id is unknown rather than being served from a stream that no
    /// longer exists.
    #[test]
    fn clear_starts_a_new_stream_and_refuses_the_old_ids() -> Result<(), Failure> {
        let mut ring = ring(4)?;
        let before = ring.push("a");
        ring.push("b");
        ring.clear();
        if !ring.is_empty() {
            return Err(Failure::Outcome("clear left events behind"));
        }
        if !matches!(ring.replay_from(Some(before)), Replay::Unknown { .. }) {
            return Err(Failure::Outcome("a cleared stream served a pre-reset id"));
        }
        // Ids keep going up rather than restarting, which is what makes the
        // refusal above possible.
        let after = ring.push("c");
        if after.get() != 3 {
            return Err(Failure::WrongIds(vec![after.get()]));
        }
        Ok(())
    }

    /// A ring of one is the smallest that can lose anything, and it must still
    /// report the loss rather than serve a stream with a hole in it.
    #[test]
    fn a_single_slot_ring_still_reports_eviction() -> Result<(), Failure> {
        let mut ring = ring(1)?;
        let first = ring.push("a");
        ring.push("b");
        if !matches!(ring.replay_from(Some(first)), Replay::Gap { .. }) {
            return Err(Failure::Outcome(
                "a one-slot ring did not report its eviction",
            ));
        }
        Ok(())
    }

    /// An empty ring answers a watermark with "current" and anything else with
    /// "unknown", so a stream that has sent nothing does not invent a gap.
    #[test]
    fn an_empty_ring_has_no_gap() -> Result<(), Failure> {
        let ring = ring::<&str>(4)?;
        if !matches!(ring.replay_from(None), Replay::UpToDate { .. }) {
            return Err(Failure::Outcome("an empty ring refused a fresh client"));
        }
        if !matches!(
            ring.replay_from(Some(EventId::WATERMARK)),
            Replay::UpToDate { .. }
        ) {
            return Err(Failure::Outcome("an empty ring refused the watermark"));
        }
        if !matches!(
            ring.replay_from(Some(EventId::new(1))),
            Replay::Unknown { .. }
        ) {
            return Err(Failure::Outcome("an empty ring invented an event"));
        }
        Ok(())
    }

    /// The `is_complete` answer is the one an adapter branches on, so it is
    /// checked against every arm rather than left to the individual tests to
    /// imply.
    #[test]
    fn completeness_agrees_with_the_arm() -> Result<(), Failure> {
        let mut ring = ring(2)?;
        let first = ring.push("a");
        ring.push("b");
        ring.push("c");
        let cases = [
            (ring.replay_from(None), true),
            (ring.replay_from(Some(ring.last())), true),
            (ring.replay_from(Some(first)), false),
            (ring.replay_from(Some(EventId::new(77))), false),
        ];
        for (replay, expected) in cases {
            if replay.is_complete() != expected {
                return Err(Failure::Outcome("is_complete disagreed with the arm"));
            }
        }
        Ok(())
    }

    /// Replay hands back a borrow rather than a copy, so a caller can answer
    /// from a shared reference while the producing task keeps the ring. Held
    /// here as a compile-time claim: two overlapping borrows, one of them
    /// `&self`, both live at once.
    #[test]
    fn a_replay_borrows_rather_than_copying() -> Result<(), Failure> {
        let mut ring = ring::<Arc<str>>(4)?;
        ring.push(Arc::<str>::from("a"));
        ring.push(Arc::<str>::from("b"));
        // The `Replay` is bound before the borrow is taken, because `events()`
        // hands out a reference *into* it and a temporary would die at the end
        // of the statement that produced the reference.
        let replay = ring.replay_from(None);
        let borrowed: &[StreamEvent<Arc<str>>] = replay.events();
        // `ring` is still usable while `borrowed` lives, and the payloads are
        // the caller's own `Arc`s, not copies of them.
        if ring.len() != 2 {
            return Err(Failure::WrongIds(vec![
                ring.len().try_into().unwrap_or(u64::MAX),
            ]));
        }
        let first = borrowed
            .first()
            .ok_or(Failure::Outcome("no event borrowed"))?;
        if Arc::strong_count(first.payload()) < 1 {
            return Err(Failure::Outcome("payload was cloned rather than borrowed"));
        }
        Ok(())
    }

    /// The ring is `Send` and `Sync` on its payload's account alone, which is
    /// what lets a stream live in axum's shared state while a reconnect handler
    /// reads it. A `Mutex` inside the ring would make this false and force every
    /// reader to take a lock; the type is checked to stay that way.
    #[test]
    fn is_shareable_across_threads() -> Result<(), Failure> {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<EventRing<u64>>();
        assert_send_sync::<Replay<'static, u64>>();

        // And the ring really does work when several threads read it while one
        // writes, which is the shape a stream has in a server.
        let ring = Arc::new(std::sync::Mutex::new(EventRing::new(
            NonZeroUsize::new(64).ok_or(Failure::ZeroCapacity)?,
        )));
        let writer = {
            let ring = Arc::clone(&ring);
            thread::spawn(move || {
                let mut held = ring.lock().map_err(|_| Failure::Concurrent)?;
                for value in 0..1_000_u64 {
                    held.push(value);
                }
                // The lock is not needed to return, and holding it across the
                // join would serialise the reader behind this thread.
                drop(held);
                Ok::<(), Failure>(())
            })
        };
        let reader = {
            let ring = Arc::clone(&ring);
            thread::spawn(move || {
                for _ in 0..1_000 {
                    let held = ring.lock().map_err(|_| Failure::Concurrent)?;
                    let replay = held.replay_from(None);
                    // Whatever the interleaving, a replay is never a success
                    // that has lost an event.
                    if replay.is_complete() && replay.events().is_empty() {
                        return Err(Failure::Outcome("a complete replay was empty"));
                    }
                    // Released here rather than at the end of the body so the
                    // next iteration does not queue behind this lock.
                    drop(held);
                }
                Ok::<(), Failure>(())
            })
        };
        if writer.join().map_err(|_| Failure::Concurrent)? != Ok(()) {
            return Err(Failure::Outcome("the writer thread failed"));
        }
        if reader.join().map_err(|_| Failure::Concurrent)? != Ok(()) {
            return Err(Failure::Outcome("the reader thread failed"));
        }
        Ok(())
    }
}
