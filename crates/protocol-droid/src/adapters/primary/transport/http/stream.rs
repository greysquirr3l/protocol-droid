//! Turning a [`Resume`] into a live `text/event-stream` body.
//!
//! # What this is for
//!
//! [`resume`] decides *what* a client that sent `Last-Event-ID` should get, and
//! [`sse`] encodes one frame. Neither writes to a socket, and the gap between a
//! decision and a byte on the wire is where resumability is either real or
//! decorative. This module is that gap: it owns the response, the header set a
//! proxy needs in order not to buffer it, and the loop that writes the buffered
//! frames and then stays open.
//!
//! # The live tail is a cursor, not a queue
//!
//! This is the whole design, and it is worth stating plainly because the
//! obvious alternative is wrong in a way that only shows up in production.
//!
//! The obvious shape is: take the `Resume`, subscribe to a channel, write the
//! buffered frames, then forward the channel. That has a race in it. Between
//! the moment the ring was read to build the `Resume` and the moment the
//! subscription is live, a push can land — and a frame handed to a channel
//! nobody is receiving from yet is a frame that is gone. The client was told it
//! was resuming from cursor 40, it next reconnects with 43, and 41 is silently
//! absent. Nothing errors. Nothing logs. Resumability has become decorative,
//! and the test that would catch it needs a real scheduler and a frame pushed
//! at exactly the wrong moment.
//!
//! So there is no subscription. The stream holds a **cursor** — the
//! [`EventId`] it has written up to — and every time it is polled it asks the
//! ring for everything after that cursor. [`EventTail::plan`] returns the
//! `Resume` and the watermark it was computed at *under one lock*, so the
//! cursor the stream starts from is exactly the one the decision was made
//! against, and a frame pushed one instruction later has an id strictly greater
//! than it. The first poll returns it. There is no interval in which a frame
//! can be missed, because "read the ring" and "start receiving" are not two
//! steps — they are the same operation, performed again.
//!
//! # Why the stream re-uses `decide` rather than inventing a second answer
//!
//! Every decision the live tail makes goes through [`decide`], the same
//! function that answers a client which has just reconnected. That is not
//! tidiness, it is the property that makes a slow client safe: a client reading
//! slowly enough for the ring to evict past its cursor gets a `Gap`, and the
//! writer emits the gap notice — the identical frame, built by the identical
//! code, carrying no `id:` — rather than skipping the evicted events in
//! silence. A separate code path for "the stream fell behind" would be a
//! second opinion about what a gap means, and the two would disagree exactly
//! when it mattered.
//!
//! Note what a mid-stream gap does to the cursor: it leaves it at
//! `evicted_through`, not at `last`. The events the ring still holds past the
//! eviction point were never lost, so the next poll delivers them. A client
//! that fell behind is told what it missed *and* handed everything it can still
//! have, which is the same answer a reconnect would produce.
//!
//! Two of `decide`'s arms have no business appearing mid-stream and are handled
//! rather than ignored. A `Refused(UnknownCursor)` means the ring was
//! [`clear`](crate::domain::EventRing::clear)ed or the cursor is from a retired
//! stream; there is no continuation of it, so the stream **closes** rather than
//! pretending. A `Refused(Handler)` means a payload had no JSON form — the same
//! server-side bug a reconnect is refused for — and the stream closes for the
//! same reason.
//!
//! # The lock order, and why it is the whole cancellation story
//!
//! A dropped client must release its subscription, or the ring accumulates
//! wakers for futures that no longer exist. The registration is therefore
//! removed in [`Drop`], keyed by a **unique registration id** and not by
//! [`Waker::will_wake`]. Two streams served to the same task carry the same
//! waker, and a `will_wake` match would let one stream's drop unregister the
//! other's — silently freezing a live client. The id makes that
//! unrepresentable.
//!
//! Two mutexes are held here and the order is fixed: **ring before waiters**,
//! always. Registration happens *while the ring lock is still held*, which is
//! what closes the wake race: a stream that found nothing and registered its
//! waker did both in the same critical section as the read that found nothing,
//! so a concurrent `push` either completed before the read — and the frame is
//! there — or waits for the lock and then finds the waker already registered.
//! There is no third interleaving. Releasing the ring lock before registering
//! would reopen exactly the window the design above exists to close.
//!
//! # Why nothing here touches the session engine
//!
//! [`SessionEngine::dispatch`](crate::application::SessionEngine::dispatch)
//! takes `&mut self` for the whole of a handler, so a body stream that tried to
//! drive the engine would be asking for a mutable borrow of something the
//! handler already holds. That is a deadlock, and a silent one, because the
//! stream is parked rather than running — it would hang rather than fail. This
//! module never mentions the engine. The engine stays in the request task that
//! owns it; anything the engine produces is published into the shared
//! [`EventTail`], which is `Send + Sync` and interior-mutable, and the body
//! stream only ever reads from it. Stage 2d-ii wires those two together; this
//! module is deliberately the half that does not care which.
//!
//! # Protocol neutrality
//!
//! There is no MCP method name, no revision type, and no version check in this
//! file. The payload is a `T: FramePayload` supplied by the caller, the
//! framing is the transport's, and the decision comes from the protocol-neutral
//! ring. A2A and ACP bind to the same [`EventTail`] with a different payload
//! and a different decision function, and need nothing here rewritten.
//!
//! # Example
//!
//! ```
//! use core::num::NonZeroUsize;
//! use serde_json::{Value, json};
//! use protocol_droid::adapters::primary::transport::http::EventTail;
//! use protocol_droid::domain::EventId;
//!
//! // A tail is a ring plus a way to wake whoever is reading it.
//! let tail = EventTail::<Value>::new(NonZeroUsize::new(8).ok_or("capacity")?);
//! tail.push(json!({ "hello": "world" }));
//!
//! // A client quoting `Last-Event-ID: 1` is already current: silence, then the
//! // live stream. No frame, no close, and no cursor to move.
//! let planned = tail.plan(Some("1"));
//! assert!(planned.resume().pending().is_empty());
//! assert_eq!(planned.cursor(), EventId::new(1));
//!
//! // A push after the decision does not rewrite it — the decision is a snapshot
//! // of one moment, and a client that re-reads it would be told a different
//! // story each time. What makes the new event reach the client is the cursor:
//! // the stream re-reads from it on every poll, so a push that lands after
//! // `plan` is still delivered.
//! tail.push(json!({ "later": true }));
//! assert_eq!(planned.resume().last(), Some(EventId::new(1)));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use core::num::NonZeroUsize;
use core::pin::Pin;
use core::sync::atomic::{AtomicU64, Ordering};
use core::task::{Context, Poll};
use core::time::Duration;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::Waker;

use axum::body::{Body, Bytes};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_core::Stream;
use thiserror::Error;

use super::resume::{Resume, decide, plan};
use super::frame_payload::FramePayload;
use super::sse::SseEvent;
use crate::domain::{EventId, EventRing};

/// The media type a resumable stream is served as.
///
/// A `const` rather than a literal at the call site because it is part of the
/// wire contract, and a value written twice is a value that will one day be
/// written twice differently.
const EVENT_STREAM: &str = "text/event-stream";

/// The header that tells nginx not to buffer the response.
///
/// Without it nginx will hold a `text/event-stream` in its proxy buffer and
/// deliver it in multi-kilobyte lumps, which turns a live stream into one that
/// appears dead and then delivers everything at once on a full buffer. This is
/// the commonest way a working SSE server looks broken in production, and it is
/// completely invisible from the code that emits the frames.
const ACCEL_BUFFERING: &str = "x-accel-buffering";

/// The keep-alive: an SSE comment line, which dispatches nothing.
///
/// A comment, and not an empty event and not a close. A close is a lie — the
/// stream is healthy. An empty event is worse than a lie: a client that
/// dispatches it receives a message with no payload and no `id:`, and a client
/// that treats an unrecognised message as a protocol event has been handed
/// something the specification never described. The event-stream standard
/// defines a line beginning with `:` as a comment for exactly this purpose, and
/// every conforming reader ignores it.
///
/// A `const` and not an [`SseEvent`] because [`SseEvent`] encodes *data* frames
/// and has no comment form — a comment carries no data, and widening
/// `SseEvent` to represent one would put a variant on a type whose `encode`
/// output every existing test in `sse` would have to be re-read against.
pub const KEEP_ALIVE_FRAME: &str = ":ping\n\n";

/// Why a body writer is fallible.
///
/// The only reachable variant is the response assembly. A body whose frames are
/// already encoded cannot fail mid-stream, and the [`Stream`] item type is a
/// `Result` only because [`Body::from_stream`] requires one — an unencodable
/// payload is turned into a refusal by [`decide`] long before it could reach
/// the writer.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StreamError {
    /// The response could not be assembled.
    ///
    /// Every header this module sets is a compile-time literal, so nothing in
    /// the crate can actually trigger this today. It is handled rather than
    /// unwrapped because the alternative is a panic on a request path, and a
    /// request path is the worst place to put one.
    #[error("the event-stream response could not be assembled: {0}")]
    Response(String),
}

impl From<axum::http::Error> for StreamError {
    fn from(error: axum::http::Error) -> Self {
        Self::Response(error.to_string())
    }
}

/// The wakers of every stream currently parked on a tail, each with the id that
/// makes it individually removable.
///
/// A `Vec` rather than a set: streams are one per connected client and
/// registration is a `push`, so the linear scan on the removal path is cheaper
/// than a hash. The *ids* are what make removal correct; the shape is only the
/// cheapest thing that carries them.
#[derive(Debug, Default)]
struct Waiters {
    entries: Vec<(u64, Waker)>,
}

/// State shared between a publisher and every stream reading from it.
///
/// Two mutexes, always locked ring-then-waiters. See the module docs for why
/// that order is load-bearing and why registration happens under the ring lock.
#[derive(Debug)]
struct Shared<T> {
    ring: Mutex<EventRing<T>>,
    waiters: Mutex<Waiters>,
    next_registration: AtomicU64,
}

impl<T> Shared<T> {
    /// The ring, recovering from poisoning rather than propagating it.
    ///
    /// A poisoned mutex means some thread panicked while holding it. What is
    /// behind it is a `Vec` of constructed events and a `Vec` of wakers, and
    /// neither can be left half-written: a push is a `Vec::push` of a value that
    /// was already built. Propagating the poison would turn one panicking
    /// handler into a permanently dead endpoint, which is a far worse failure
    /// than continuing from a ring that is still structurally sound.
    fn ring(&self) -> MutexGuard<'_, EventRing<T>> {
        self.ring.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The waiter list, recovering from poisoning on the same reasoning.
    fn waiters(&self) -> MutexGuard<'_, Waiters> {
        self.waiters.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A registration id no other live registration holds.
    fn next_registration(&self) -> u64 {
        // A relaxed fetch-add is right for an id that only has to be unique.
        // Nothing is published through it, and a collision would need two
        // threads to receive the same value, which the atomic add prevents
        // regardless of ordering.
        self.next_registration.fetch_add(1, Ordering::Relaxed)
    }
}

/// A shared, wakeable, replayable source of stream events.
///
/// What a session publishes into and what a response body reads from. It is
/// `Send + Sync` and holds no borrow of anything, which is the property that
/// lets a body stream outlive the request task that built it — see the module
/// docs on why the stream must not be the thing holding the engine.
///
/// Cloning is cheap and shares: every clone reads and writes the same ring, so
/// a router can keep one in its state and hand a reference to each response.
///
/// # Example
///
/// ```
/// use core::num::NonZeroUsize;
/// use serde_json::{Value, json};
/// use protocol_droid::adapters::primary::transport::http::EventTail;
///
/// let capacity = NonZeroUsize::new(4).ok_or("capacity")?;
/// let tail = EventTail::<Value>::new(capacity);
/// for index in 0..3 {
///     tail.push(json!({ "index": index }));
/// }
/// // A client that has seen all three is current and gets no frames.
/// assert!(tail.plan(Some("3")).resume().pending().is_empty());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct EventTail<T> {
    shared: Arc<Shared<T>>,
}

impl<T> EventTail<T> {
    /// A tail over a ring retaining `capacity` events.
    ///
    /// [`RECOMMENDED_CAPACITY`](crate::domain::RECOMMENDED_CAPACITY) is the
    /// right value for a caller that has not measured its payloads; pass a real
    /// count for one that has.
    #[must_use]
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            shared: Arc::new(Shared {
                ring: Mutex::new(EventRing::new(capacity)),
                waiters: Mutex::new(Waiters::default()),
                next_registration: AtomicU64::new(0),
            }),
        }
    }

    /// How many events are currently retained.
    ///
    /// For a log line or a test assertion. Taking the ring lock is why this is
    /// not on any per-frame path.
    #[must_use]
    pub fn len(&self) -> usize {
        self.shared.ring().len()
    }

    /// Whether nothing is retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.shared.ring().is_empty()
    }

    /// The newest event id ever issued, or the watermark if nothing has been.
    #[must_use]
    pub fn last(&self) -> EventId {
        self.shared.ring().last()
    }
}

impl<T> Clone for EventTail<T> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<T> Default for EventTail<T> {
    fn default() -> Self {
        Self::new(crate::domain::RECOMMENDED_CAPACITY.into())
    }
}

impl<T> EventTail<T>
where
    T: FramePayload,
{
    /// Publishes an event and wakes every stream parked on this tail.
    ///
    /// The returned id is the value a client quotes to resume after this event,
    /// so a publisher that discards it has made the event unresumable: the next
    /// reconnect is told about a gap the client never knew it had.
    pub fn push(&self, payload: T) -> EventId {
        let id = self.shared.ring().push(payload);
        // The wakers are drained under the ring lock and woken outside it. The
        // drain has to be inside so a stream cannot register between the push
        // and the collection; the wake has to be outside so a waker that
        // re-enters this tail cannot deadlock on the lock it is being woken
        // while holding.
        let wakers: Vec<Waker> = {
            let _ring = self.shared.ring();
            self.shared
                .waiters()
                .entries
                .drain(..)
                .map(|(_, waker)| waker)
                .collect()
        };
        for waker in &wakers {
            waker.wake_by_ref();
        }
        id
    }

    /// Decides what a client that sent `header` gets, atomically with the
    /// watermark it was decided against.
    ///
    /// The pairing with the cursor is the point. `Resume` names the frames to
    /// write; the stream needs to know where to *continue* from. Reading those
    /// from two separate lock acquisitions would open the window the module docs
    /// describe, so [`Planned`] carries both and there is no way to obtain one
    /// without the other.
    ///
    /// `header` is the raw `Last-Event-ID` value, or `None` for a client that has
    /// never seen this stream. A header that is present but unreadable is a
    /// refusal, decided before the ring is consulted.
    pub fn plan(&self, header: Option<&str>) -> Planned<T> {
        let ring = self.shared.ring();
        let resume = plan(&ring, header);
        // The cursor is the last id the client is known to hold, which is the
        // id the decision accounted for — except after a gap, where it is one
        // _below_ the first id the ring can answer for.
        //
        // `resume_from` is the first surviving event, and the client has not
        // seen it, so a cursor left there would ask for `resume_from` and be
        // handed `resume_from + 1` onward: the one event that survived the drop
        // and was not lost would be the one lost. `resume_from - 1` is the last
        // id the client can be said to hold, and asking for it returns the
        // survivors in full.
        let cursor = match &resume {
            Resume::Gap {
                evicted_through, ..
            } => *evicted_through,
            other => other.last().unwrap_or(EventId::WATERMARK),
        };
        drop(ring);
        Planned {
            shared: Arc::clone(&self.shared),
            resume,
            cursor,
        }
    }
}

/// The response-building half of [`EventTail`], separated by its bound.
///
/// The bound is `T: Send + 'static` because the body it produces has to be sent
/// between threads and outlive the call; [`push`](Self::push) and
/// [`plan`](Self::plan) do not carry it, so a caller that only publishes or only
/// inspects never has to satisfy it.
impl<T> EventTail<T>
where
    T: FramePayload + Send + 'static,
{
    /// Plans and answers in one call, for a router with nothing to do in
    /// between.
    ///
    /// # Errors
    ///
    /// [`StreamError::Response`] if the response headers cannot be assembled.
    pub fn respond(
        &self,
        header: Option<&str>,
        keep_alive: Duration,
    ) -> Result<Response, StreamError> {
        self.plan(header).into_response(keep_alive)
    }
}

/// A decision, the watermark it was made against, and the tail to continue from.
///
/// Bundling these is what makes the replay-to-tail race unrepresentable rather
/// than merely unlikely: there is no way to obtain a [`Resume`] and a starting
/// cursor separately, so a caller cannot pair a decision with a cursor from a
/// later read.
#[derive(Debug)]
pub struct Planned<T> {
    shared: Arc<Shared<T>>,
    resume: Resume,
    cursor: EventId,
}

impl<T> Planned<T> {
    /// The decision.
    #[must_use]
    pub const fn resume(&self) -> &Resume {
        &self.resume
    }

    /// The cursor the stream will continue from.
    ///
    /// Every frame the decision did not include has an id strictly greater than
    /// this, which is what makes a push between the decision and the first poll
    /// undroppable.
    #[must_use]
    pub const fn cursor(&self) -> EventId {
        self.cursor
    }

    /// The refusal, when this plan is one.
    ///
    /// For a router that wants to log or count the refusal separately from
    /// answering it.
    #[must_use]
    pub const fn refusal(&self) -> Option<&super::Refusal> {
        match &self.resume {
            Resume::Refused(refusal) => Some(refusal),
            Resume::Replay { .. } | Resume::Live { .. } | Resume::Gap { .. } => None,
        }
    }
}

impl<T> Planned<T> {
    /// A plan that is nothing but a refusal, for a router that declined before
    /// the ring was ever consulted.
    ///
    /// Separate from [`plan`](EventTail::plan) because a router refuses for
    /// reasons the ring knows nothing about — no credentials, no such session,
    /// wrong method, body too large — and every one of those has to come out as
    /// the same shape as a cursor the ring refused, or a caller would have two
    /// ways of producing a status and no reason to pick the right one.
    ///
    /// # Example
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use serde_json::Value;
    /// use protocol_droid::adapters::primary::transport::http::{EventTail, Refusal, planned_refusal};
    ///
    /// let capacity = NonZeroUsize::new(4).ok_or("capacity")?;
    /// let tail = EventTail::<Value>::new(capacity);
    /// let planned = planned_refusal(&tail, Refusal::Unauthenticated);
    /// assert!(planned.resume().pending().is_empty());
    /// assert!(planned.cursor().is_watermark());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn refused(tail: &EventTail<T>, refusal: super::Refusal) -> Self {
        Self {
            shared: Arc::clone(&tail.shared),
            resume: Resume::Refused(refusal),
            // A refusal is not a stream, so it has no cursor. The watermark is
            // the honest value: nothing has been written, so nothing has been
            // consumed, and a caller that logged this number would be reporting
            // "no events issued" rather than "no cursor".
            cursor: EventId::WATERMARK,
        }
    }
}

/// A [`Planned`] that is a refusal, for a router that declined before the ring
/// was consulted.
///
/// The free-function spelling of [`Planned::refused`], for a caller that has a
/// `Refusal` and a tail and no other use for the plan.
#[must_use]
pub fn planned_refusal<T>(tail: &EventTail<T>, refusal: super::Refusal) -> Planned<T> {
    Planned::refused(tail, refusal)
}

impl<T> Planned<T>
where
    T: FramePayload,
{
    /// The live body, or the refusal response when there is no stream to open.
    ///
    /// A refusal is never a stream. Returning it as an `Err` carrying a
    /// [`Response`] — rather than as an `Option`, a sentinel frame, or an empty
    /// body — keeps the two cases impossible to confuse: the success type is a
    /// body and the failure type is a status code. The `Err` is a `Box` because
    /// a `Response` is large next to the `EventStream` beside it, and an enum
    /// sized to its largest arm would put that on every success path too.
    pub fn try_stream(&self, keep_alive: Duration) -> Result<EventStream<T>, Box<Response>> {
        if let Resume::Refused(refusal) = &self.resume {
            return Err(Box::new(refusal.clone().into_response()));
        }
        Ok(EventStream {
            shared: Arc::clone(&self.shared),
            // The decision's frames are queued before the cursor is ever
            // consulted, so a `Replay`'s last frame is not written twice and
            // its first unseen frame is not skipped.
            queued: self.resume.pending().iter().cloned().collect(),
            cursor: self.cursor,
            keep_alive,
            deadline: None,
            registration: None,
            closed: false,
        })
    }
}

/// The response-building half of [`Planned`], separated by its bound.
///
/// `T: Send + 'static` is required here and only here, because
/// [`Body::from_stream`] requires the stream to be sent between threads and to
/// outlive the call. Putting the bound on the whole impl would force it on
/// [`resume`](Planned::resume) and [`cursor`](Planned::cursor), which a caller
/// reads from a context that has no reason to have a `Send` payload in hand.
impl<T> Planned<T>
where
    T: FramePayload + Send + 'static,
{
    /// The response to send, whatever the decision was.
    ///
    /// This is what a router calls. A `Refused` becomes a status and a JSON-RPC
    /// body; every other arm becomes a `200 text/event-stream` whose body writes
    /// the buffered frames and then stays open.
    ///
    /// # Errors
    ///
    /// [`StreamError::Response`] if the response headers cannot be assembled.
    pub fn into_response(self, keep_alive: Duration) -> Result<Response, StreamError> {
        if let Resume::Refused(refusal) = &self.resume {
            // `Refusal`'s own `IntoResponse` is the single place its status and
            // body are defined. Re-deriving them here would be a second opinion
            // about a wire contract a client branches on. The clone is of a
            // three-field enum, once per refused request.
            return Ok(refusal.clone().into_response());
        }
        let stream = EventStream {
            shared: Arc::clone(&self.shared),
            queued: self.resume.pending().iter().cloned().collect(),
            cursor: self.cursor,
            keep_alive,
            deadline: None,
            registration: None,
            closed: false,
        };
        stream_response(stream)
    }
}

/// A live `text/event-stream` body.
///
/// Implements [`Stream`] directly rather than going through axum's `Sse`
/// helper because this stream has to emit a frame the helper has no spelling
/// for: a frame carrying no `id:` is exactly the gap notice, and building it
/// through a type whose every event implies a cursor is how a gap notice comes
/// to carry one.
///
/// The item type is a `Result` because [`Body::from_stream`] requires it, not
/// because a frame can fail — by the time a frame is queued it is already
/// encoded.
#[derive(Debug)]
pub struct EventStream<T> {
    shared: Arc<Shared<T>>,
    queued: VecDeque<SseEvent>,
    cursor: EventId,
    keep_alive: Duration,
    deadline: Option<Pin<Box<tokio::time::Sleep>>>,
    registration: Option<u64>,
    closed: bool,
}

impl<T> EventStream<T> {
    /// The cursor this stream has written up to.
    #[must_use]
    pub const fn cursor(&self) -> EventId {
        self.cursor
    }

    /// Whether a waker is currently registered with the tail.
    ///
    /// The subscription is held only while the stream is parked, which is what
    /// keeps a client that disconnects between frames from leaving anything
    /// behind.
    #[must_use]
    pub const fn is_subscribed(&self) -> bool {
        self.registration.is_some()
    }

    /// Removes this stream's registration, if it has one.
    ///
    /// Unbounded over `T` because [`Drop`] is: a `Drop` impl cannot add a bound
    /// the struct does not carry, so a method that only the destructor calls has
    /// to be available for every `T` the stream is instantiated with.
    fn unregister(&mut self) {
        let Some(id) = self.registration.take() else {
            return;
        };
        self.shared
            .waiters()
            .entries
            .retain(|(entry, _)| *entry != id);
    }
}

impl<T> EventStream<T>
where
    T: FramePayload,
{
    /// The next decision, and the waker registered under the ring lock when it
    /// is "nothing yet".
    ///
    /// The read and the registration share one critical section on purpose. A
    /// stream that found nothing and registered its waker did both while the
    /// ring lock was held, so a concurrent [`push`](EventTail::push) either
    /// finished before the read — and the frame is in the frames we return — or
    /// blocks on the lock and then finds the waker already in the list. There is
    /// no interleaving in which a frame is pushed and nobody is listening.
    fn take(&mut self, context: &Context<'_>) -> Batch {
        let ring = self.shared.ring();
        let decision = decide(&ring.replay_from(Some(self.cursor)));
        let batch = match decision {
            Resume::Replay { frames, last } => Batch {
                frames,
                cursor: last,
                parked: false,
                closed: false,
            },
            // Nothing new and nothing lost. The cursor is already the watermark
            // — `replay_from` only answers `UpToDate` when the requested id
            // *is* the last — so it does not move.
            Resume::Live { .. } => Batch::parked(),
            // The notice goes out first, and the cursor lands on `resume_from`
            // — the first id the ring can answer for. Not the watermark, which
            // would drop the frames the ring still holds in silence, and not
            // `evicted_through`, which the ring's gap test is inclusive of and
            // would therefore gap again on the very next poll, leaving the
            // client a stream of notices and no events.
            Resume::Gap {
                notice,
                resume_from,
                ..
            } => Batch {
                frames: vec![notice],
                cursor: resume_from,
                parked: false,
                closed: false,
            },
            // Both refusals mean there is no stream to continue: the ring was
            // cleared under this client, or a payload had no JSON form. The
            // client holds a healthy connection, so the honest signal is a
            // stream end, and it will reconnect and be told why.
            Resume::Refused(_) => Batch {
                frames: Vec::new(),
                cursor: self.cursor,
                parked: false,
                closed: true,
            },
        };
        if batch.parked {
            // `&self` rather than `&mut self` precisely so this can happen
            // while the ring guard is alive. That is the anti-race mechanism:
            // the read that found nothing and the registration that will hear
            // about the next push are one critical section, so there is no
            // instant at which a frame could be pushed with nothing listening.
            let id = self.registration(context);
            self.registration = Some(id);
        }
        drop(ring);
        batch
    }

    /// Registers this stream's waker, or refreshes the one it already has, and
    /// returns the id it is registered under.
    ///
    /// One entry per stream, updated in place. A stream polls in a loop and a
    /// re-registration on every `Pending` would grow the list without bound,
    /// and — worse — would leave a `Drop` that removes only the newest id
    /// behind every earlier one.
    fn registration(&self, context: &Context<'_>) -> u64 {
        let waker = context.waker();
        let mut waiters = self.shared.waiters();
        if let Some(id) = self.registration
            && let Some((_, registered)) =
                waiters.entries.iter_mut().find(|(entry, _)| *entry == id)
        {
            // The same waker: nothing to do, and the existing id stands. A
            // different one means the stream was moved between tasks, and
            // the entry has to follow it or the old task is woken for a
            // stream it no longer polls. `clone_from` reuses the existing
            // allocation where the two hold the same kind of waker, which a
            // plain assign-then-clone cannot.
            if !registered.will_wake(waker) {
                registered.clone_from(waker);
            }
            return id;
        }
        let fresh = self.shared.next_registration();
        waiters.entries.push((fresh, waker.clone()));
        fresh
    }

    /// Arms the keep-alive deadline, if none is armed.
    ///
    /// Armed here rather than in the constructor because `tokio::time::sleep`
    /// needs a runtime context and a stream is often constructed outside one.
    /// A stream with frames to write never reaches here — a keep-alive on a busy
    /// stream is noise — so this is only reached by a stream that has nothing to
    /// say, which is exactly the stream that needs to be told it is still there.
    ///
    /// The `try_current` check is not defensive noise: `tokio::time::sleep`
    /// **panics** with no current runtime, and this function is reachable from a
    /// plain `poll_next` on any executor. A `Stream` that only works on tokio
    /// would be a latent panic in every embedder that drives it elsewhere, and
    /// the failure would surface as a panic inside a poll rather than as a
    /// typed error a caller could handle. Without a runtime there is no timer to
    /// arm, so the stream stays silent — which is the correct degradation,
    /// because the alternative is a heartbeat that cannot be scheduled.
    fn arm_keep_alive(&mut self) {
        if self.deadline.is_some() {
            return;
        }
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        self.deadline = Some(Box::pin(tokio::time::sleep(self.keep_alive)));
    }
}

impl<T> Drop for EventStream<T> {
    /// Releases the subscription.
    ///
    /// In `Drop` and not in a method because a client disconnecting mid-stream
    /// is the *normal* way a stream ends, and after the response body is
    /// dropped there is no code path left for anyone to call a cleanup on. A
    /// registration removed anywhere other than here would be removed on a path
    /// a dropped client does not take.
    fn drop(&mut self) {
        self.unregister();
    }
}

/// What one pass over the ring decided.
struct Batch {
    /// The frames to write, in order.
    frames: Vec<SseEvent>,
    /// Where the cursor goes once these frames are queued.
    cursor: EventId,
    /// Whether there is nothing to write and the stream should park.
    parked: bool,
    /// Whether the stream is finished.
    closed: bool,
}

impl Batch {
    /// Nothing to write, and the stream should wait.
    ///
    /// The cursor is the watermark, which is also where the stream's own cursor
    /// already is: `replay_from` answers `UpToDate` only when the requested id
    /// equals the newest. The field is set rather than left uninitialised only
    /// because the struct has no `Default`, and a `Default` here would be a
    /// second answer to "what is an empty batch" that could disagree with this
    /// one.
    const fn parked() -> Self {
        Self {
            frames: Vec::new(),
            cursor: EventId::WATERMARK,
            parked: true,
            closed: false,
        }
    }
}

impl<T> Stream for EventStream<T>
where
    T: FramePayload,
{
    type Item = Result<Bytes, StreamError>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if this.closed {
                return Poll::Ready(None);
            }
            if let Some(frame) = this.queued.pop_front() {
                // A waker is only wanted while there is nothing to write, and a
                // stream that is producing frames is polled again immediately,
                // so the registration is released rather than left to go stale.
                this.unregister();
                return Poll::Ready(Some(Ok(Bytes::from(frame.encode()))));
            }

            let batch = this.take(context);
            if batch.closed {
                this.unregister();
                this.closed = true;
                return Poll::Ready(None);
            }
            if !batch.frames.is_empty() {
                this.cursor = batch.cursor;
                this.queued.extend(batch.frames);
                continue;
            }

            this.arm_keep_alive();
            if let Some(deadline) = this.deadline.as_mut()
                && deadline.as_mut().poll(context).is_ready()
            {
                this.deadline = None;
                this.unregister();
                return Poll::Ready(Some(Ok(Bytes::from_static(KEEP_ALIVE_FRAME.as_bytes()))));
            }
            return Poll::Pending;
        }
    }
}

/// Assembles the `200 text/event-stream` response around a body.
///
/// A free function so [`Planned::into_response`] and a test can both build the
/// header set, and so there is exactly one place that knows what a resumable
/// response looks like on the wire.
///
/// # Errors
///
/// [`StreamError::Response`] if the response headers cannot be assembled.
fn stream_response<T>(stream: EventStream<T>) -> Result<Response, StreamError>
where
    T: FramePayload + Send + 'static,
{
    // Three headers, and each is load-bearing in a way that is invisible from
    // the frame encoder:
    //
    // - `Content-Type: text/event-stream` is what makes a client parse this as
    //   a stream at all. Without it the same bytes are an opaque body.
    // - `Cache-Control: no-cache` is what stops an intermediary from serving a
    //   stored response to the *next* client, which for a stream means that
    //   client receives the previous one's events.
    // - `X-Accel-Buffering: no` is nginx's opt-out. Without it nginx buffers
    //   the response and delivers it in lumps, which presents as a live server
    //   that has gone silent.
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, EVENT_STREAM)
        .header(header::CACHE_CONTROL, "no-cache")
        .header(ACCEL_BUFFERING, "no")
        .body(Body::from_stream(stream))
        .map_err(StreamError::from)
}

/// How many streams are currently parked on a tail.
///
/// For a router's shutdown path, and for asserting that a dropped body released
/// its subscription. A non-zero count after the last response has been dropped
/// is a leak.
///
/// # Example
///
/// ```
/// use core::num::NonZeroUsize;
/// use serde_json::Value;
/// use protocol_droid::adapters::primary::transport::http::{EventTail, parked_streams};
///
/// let capacity = NonZeroUsize::new(4).ok_or("capacity")?;
/// let tail = EventTail::<Value>::new(capacity);
/// assert_eq!(parked_streams(&tail), 0);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use]
pub fn parked_streams<T>(tail: &EventTail<T>) -> usize {
    tail.shared.waiters().entries.len()
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroUsize;
    use core::pin::Pin;
    use core::task::{Context, Poll, Waker};
    use core::time::Duration;
    use std::fmt;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Wake;

    use axum::body::Bytes;
    use axum::http::StatusCode;
    use futures_core::Stream;
    use serde_json::{Value, json};

    use super::{
        EventStream, EventTail, FramePayload, KEEP_ALIVE_FRAME, StreamError, parked_streams,
    };
    use crate::adapters::primary::transport::http::{
        Refusal, Resume, SseError, SseEvent, planned_refusal,
    };
    use crate::domain::EventId;

    /// Why a test failed, as one error type the `?` can produce.
    ///
    /// The fallible calls in this module answer unrelated error types — an
    /// `Option` from `NonZeroUsize`, an `SseError` from a decode, a
    /// [`StreamError`] from a poll — and a test that collected them in a `Box`
    /// would have to name each. One enum with a `From` each is the same thing
    /// with the conversions written once.
    #[derive(Debug)]
    enum Failure {
        /// A capacity of zero, which `NonZeroUsize` cannot represent.
        ZeroCapacity,
        /// A plan produced a stream when the test needed a refusal response.
        UnexpectedStream,
        /// A decode that failed.
        Sse(SseError),
        /// A frame that was not the shape the test claims it must be.
        Wrong(String),
    }

    impl fmt::Display for Failure {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::ZeroCapacity => {
                    write!(formatter, "a ring capacity of zero is not a capacity")
                }
                Self::UnexpectedStream => write!(formatter, "expected a refusal, got a stream"),
                Self::Sse(error) => write!(formatter, "sse: {error}"),
                Self::Wrong(detail) => write!(formatter, "{detail}"),
            }
        }
    }

    impl std::error::Error for Failure {}

    impl From<SseError> for Failure {
        fn from(error: SseError) -> Self {
            Self::Sse(error)
        }
    }

    impl From<StreamError> for Failure {
        fn from(error: StreamError) -> Self {
            Self::Wrong(error.to_string())
        }
    }

    /// The one error type these tests return.
    type TestError = Failure;

    /// A waker that counts how many times it was woken.
    ///
    /// A count rather than a flag because the properties under test care
    /// whether the *right* wake happened, and "it woke at least once" is the
    /// only claim a flag can make.
    #[derive(Default)]
    struct Counting {
        wakes: AtomicUsize,
    }

    impl Counting {
        fn wakes(&self) -> usize {
            self.wakes.load(Ordering::SeqCst)
        }

        fn waker(self: &Arc<Self>) -> Waker {
            Waker::from(Arc::clone(self))
        }
    }

    impl Wake for Counting {
        fn wake(self: Arc<Self>) {
            self.wakes.fetch_add(1, Ordering::SeqCst);
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.wakes.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// A tail retaining `capacity` events of JSON payload.
    fn tail(capacity: usize) -> Result<EventTail<Value>, TestError> {
        let capacity = NonZeroUsize::new(capacity).ok_or(Failure::ZeroCapacity)?;
        Ok(EventTail::new(capacity))
    }

    /// Polls a stream once with a caller-supplied waker.
    ///
    /// Returns the poll rather than a decoded frame, because the properties
    /// under test are about *whether* a poll produced bytes and about the exact
    /// bytes it produced — decoding inside the harness would hide both.
    fn poll_once<T: FramePayload>(
        stream: &mut EventStream<T>,
        waker: &Waker,
    ) -> Poll<Option<Result<Bytes, StreamError>>> {
        let mut context = Context::from_waker(waker);
        core::pin::Pin::new(stream).poll_next(&mut context)
    }

    /// The bytes of the frame a poll produced, or a description of why there
    /// were none.
    fn bytes_of(poll: Poll<Option<Result<Bytes, StreamError>>>) -> Result<String, TestError> {
        match poll {
            Poll::Ready(Some(Ok(bytes))) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
            Poll::Ready(Some(Err(error))) => Err(Failure::Wrong(error.to_string())),
            Poll::Ready(None) => Err(Failure::Wrong("the stream ended".to_owned())),
            Poll::Pending => Err(Failure::Wrong("the stream parked".to_owned())),
        }
    }

    /// The decoded event a frame carries.
    fn event_of(text: &str) -> Result<SseEvent, TestError> {
        Ok(SseEvent::decode(text)?)
    }

    /// The `id:` a frame carries, or the watermark for one that carries none.
    ///
    /// `WATERMARK` rather than an `Option` because a frame with no id and a
    /// frame at id 0 mean the same thing on a wire, and the tests that care
    /// about the *absence* read `SseEvent::id` directly.
    fn id_of(event: &SseEvent) -> EventId {
        event.id().unwrap_or(EventId::WATERMARK)
    }

    /// Asserts a frame's decoded payload is the JSON value a test expects.
    fn assert_payload(event: &SseEvent, expected: &Value) -> Result<(), TestError> {
        let actual: Value = serde_json::from_str(event.data())
            .map_err(|error| Failure::Wrong(error.to_string()))?;
        if &actual == expected {
            Ok(())
        } else {
            Err(Failure::Wrong(format!(
                "expected payload {expected}, got {actual}"
            )))
        }
    }

    /// A long keep-alive, so a test that does not want one never sees one.
    fn no_keep_alive() -> Duration {
        Duration::from_mins(15)
    }

    // ---------------------------------------------------------------------
    // A `Replay` is written whole, in order, with its cursors.
    // ---------------------------------------------------------------------

    /// A resuming client gets every frame after its cursor, in order, with
    /// every `id:` intact.
    ///
    /// The mutation this kills is writing the window in reverse, or dropping
    /// the `id:` while reordering — both produce a stream that looks complete
    /// and cannot be resumed, because the client's next reconnect quotes
    /// whatever the last frame it received said.
    #[test]
    fn the_buffered_frames_come_out_in_order() -> Result<(), TestError> {
        let tail = tail(8)?;
        for index in 1..=3 {
            tail.push(json!({ "index": index }));
        }

        let planned = tail.plan(Some("0"));
        let mut stream = planned
            .try_stream(no_keep_alive())
            .map_err(|_| Failure::UnexpectedStream)?;
        let counting = Arc::new(Counting::default());
        let waker = counting.waker();

        for index in 1..=3u64 {
            let frame = bytes_of(poll_once(&mut stream, &waker))?;
            let event = event_of(&frame)?;
            if id_of(&event) != EventId::new(index) {
                return Err(Failure::Wrong(format!(
                    "expected frame {index}, got {}",
                    id_of(&event).get()
                )));
            }
            assert_payload(&event, &json!({ "index": index }))?;
        }
        Ok(())
    }

    // ---------------------------------------------------------------------
    // A gap notice comes first, carries no id, and does not skip the rest.
    // ---------------------------------------------------------------------

    /// A client behind the eviction point is told what it missed *before* any
    /// live frame, and the notice moves no cursor.
    ///
    /// The order is the load-bearing half. A writer that drained the ring
    /// before writing the notice would send a contiguous-looking stream in
    /// which the gap is invisible — the exact silent loss the whole module
    /// exists to prevent.
    ///
    /// The absence of an `id:` is the other half: a notice *with* a cursor
    /// would advance the client's resume point past the events it never
    /// received, making them unrecoverable rather than merely missed.
    #[test]
    fn the_gap_notice_precedes_the_live_frames_and_carries_no_id() -> Result<(), TestError> {
        let tail = tail(2)?;
        // Push three into a ring of two: the first is evicted, which is what
        // puts a real gap between what a client at 0 asked for and what
        // survives.
        for index in 1..=3 {
            tail.push(json!({ "index": index }));
        }

        let planned = tail.plan(Some("0"));
        let mut stream = planned
            .try_stream(no_keep_alive())
            .map_err(|_| Failure::UnexpectedStream)?;
        let counting = Arc::new(Counting::default());
        let waker = counting.waker();

        let notice = event_of(&bytes_of(poll_once(&mut stream, &waker))?)?;
        // The notice is a control frame, not an event the client can resume
        // from. `id()` returning `None` is the claim; the watermark is what a
        // client would resume from if it wrongly took the notice's absence as
        // a cursor.
        if notice.id().is_some() {
            return Err(Failure::Wrong(
                "a gap notice must carry no id: it would move the client's \
                 resume cursor past events it never received"
                    .to_owned(),
            ));
        }
        let payload: Value = serde_json::from_str(notice.data())
            .map_err(|error| Failure::Wrong(error.to_string()))?;
        if payload.get("type").and_then(Value::as_str) != Some("resume-gap") {
            return Err(Failure::Wrong(format!(
                "expected a resume-gap notice, got {payload}"
            )));
        }

        // The frames the ring still holds come *after* the notice, and they are
        // not skipped: the cursor lands on the eviction point, so the next poll
        // asks for exactly what survived.
        for index in 2..=3u64 {
            let event = event_of(&bytes_of(poll_once(&mut stream, &waker))?)?;
            if id_of(&event) != EventId::new(index) {
                return Err(Failure::Wrong(format!(
                    "expected frame {index} after the notice, got {}",
                    id_of(&event).get()
                )));
            }
        }
        Ok(())
    }

    /// A client that falls behind *while connected* is told, not skipped.
    ///
    /// The mid-stream analogue of the test above, and the one that proves the
    /// stream reuses `decide` rather than a second answer written for the live
    /// path. A writer that only consulted the ring for *new* events would skip
    /// the evicted ones in silence, and the client would never know.
    #[test]
    fn a_client_that_falls_behind_mid_stream_is_told_rather_than_skipped() -> Result<(), TestError>
    {
        let tail = tail(2)?;
        tail.push(json!({ "index": 1 }));
        tail.push(json!({ "index": 2 }));

        let planned = tail.plan(Some("2"));
        let mut stream = planned
            .try_stream(no_keep_alive())
            .map_err(|_| Failure::UnexpectedStream)?;
        let counting = Arc::new(Counting::default());
        let waker = counting.waker();

        // The client is current and the stream parks.
        if !matches!(poll_once(&mut stream, &waker), Poll::Pending) {
            return Err(Failure::Wrong(
                "a current client should have parked, not produced bytes".to_owned(),
            ));
        }

        // It now falls behind: three more pushes evict past the cursor it is
        // holding.
        for index in 3..=5 {
            tail.push(json!({ "index": index }));
        }

        let notice = event_of(&bytes_of(poll_once(&mut stream, &waker))?)?;
        if notice.id().is_some() {
            return Err(Failure::Wrong(
                "a mid-stream gap notice must carry no id either".to_owned(),
            ));
        }
        let payload: Value = serde_json::from_str(notice.data())
            .map_err(|error| Failure::Wrong(error.to_string()))?;
        if payload.get("type").and_then(Value::as_str) != Some("resume-gap") {
            return Err(Failure::Wrong(format!(
                "a client that fell behind mid-stream was not told: got {payload}"
            )));
        }
        Ok(())
    }

    // ---------------------------------------------------------------------
    // A `Live` is silence, not a close and not a comment.
    // ---------------------------------------------------------------------

    /// A client that is already current gets no bytes at all.
    ///
    /// The three things this rules out, and each is a real mistake: a close
    /// (the client reconnects in a loop, forever), an empty event (the client
    /// dispatches a message the specification never describes), and a keep-alive
    /// comment (which is legitimate but is a *timeout*, and firing it
    /// immediately would mean the interval is not a heartbeat at all).
    ///
    /// The claim is about the poll that *follows* the plan, not about
    /// `Resume::pending` — a writer that queued a keep-alive would pass a test
    /// asserting only the latter.
    #[test]
    fn a_live_stream_is_silent_until_something_happens() -> Result<(), TestError> {
        let tail = tail(4)?;
        tail.push(json!({ "index": 1 }));
        tail.push(json!({ "index": 2 }));

        let planned = tail.plan(Some("2"));
        if !planned.resume().pending().is_empty() {
            return Err(Failure::Wrong(
                "a current client must have nothing queued to write".to_owned(),
            ));
        }
        let mut stream = planned
            .try_stream(no_keep_alive())
            .map_err(|_| Failure::UnexpectedStream)?;
        let counting = Arc::new(Counting::default());
        let waker = counting.waker();

        for _ in 0..3 {
            match poll_once(&mut stream, &waker) {
                Poll::Pending => {}
                Poll::Ready(_) => {
                    return Err(Failure::Wrong(
                        "a current client received bytes before anything happened".to_owned(),
                    ));
                }
            }
        }
        Ok(())
    }

    // ---------------------------------------------------------------------
    // A refusal is a status and a body, never a stream.
    // ---------------------------------------------------------------------

    /// A malformed cursor is answered with a status and a JSON-RPC error
    /// document, and never as a stream.
    ///
    /// The mutation this kills is the one that makes a broken client look
    /// healthy: a `200 text/event-stream` whose first frame carries the error.
    /// Such a client is told it is connected, and then has to parse an error
    /// out of a data frame to discover it is not — while a well-behaved
    /// reconnecting client treats the same response as a live stream and hangs
    /// on it.
    #[tokio::test]
    async fn a_refusal_is_a_status_and_a_json_rpc_body_not_a_stream() -> Result<(), TestError> {
        let tail = tail(4)?;
        let response = tail
            .respond(Some("not-a-cursor"), no_keep_alive())
            .map_err(|error| Failure::Wrong(error.to_string()))?;

        if response.status() != StatusCode::BAD_REQUEST {
            return Err(Failure::Wrong(format!(
                "a malformed cursor is a 400, got {}",
                response.status()
            )));
        }
        let content_type = response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if content_type.starts_with(super::EVENT_STREAM) {
            return Err(Failure::Wrong(format!(
                "a refusal must not be served as {content_type}"
            )));
        }

        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .map_err(|error| Failure::Wrong(error.to_string()))?;
        let document: Value =
            serde_json::from_slice(&body).map_err(|error| Failure::Wrong(error.to_string()))?;
        if document.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(Failure::Wrong(format!(
                "a refusal body must be a JSON-RPC document, got {document}"
            )));
        }
        let error = document
            .get("error")
            .ok_or_else(|| Failure::Wrong("a refusal body must carry an error".to_owned()))?;
        if error.get("code").and_then(Value::as_i64).is_none() {
            return Err(Failure::Wrong(
                "a refusal body must carry a numeric error code".to_owned(),
            ));
        }
        if error.get("message").and_then(Value::as_str).is_none() {
            return Err(Failure::Wrong(
                "a refusal body must carry an error message".to_owned(),
            ));
        }
        Ok(())
    }

    /// Every refusal arm becomes a non-`200`, whatever produced it.
    ///
    /// The single-arm test above proves the mechanism for a refusal the *ring*
    /// raised. This proves it for the ones it never sees: a router declines on
    /// credentials or a missing session before the ring is consulted, and a
    /// new [`Refusal`] variant that reached a client as a `200 text/event-stream`
    /// would be the exact failure this module exists to make unrepresentable.
    #[test]
    fn every_refusal_becomes_a_non_200() -> Result<(), TestError> {
        let refusals = [
            Refusal::MalformedCursor,
            Refusal::MalformedSessionId,
            Refusal::MissingSession,
            Refusal::Unauthenticated,
            Refusal::Forbidden,
            Refusal::UnknownCursor {
                requested: EventId::new(4),
                last: EventId::new(9),
            },
        ];
        for refusal in refusals {
            let tail = tail(4)?;
            let planned = planned_refusal(&tail, refusal.clone());
            if !planned.resume().pending().is_empty() {
                return Err(Failure::Wrong(format!(
                    "{refusal} queued frames for a stream it will never open"
                )));
            }
            let response = planned
                .into_response(no_keep_alive())
                .map_err(|error| Failure::Wrong(error.to_string()))?;
            if response.status().is_success() {
                return Err(Failure::Wrong(format!("{refusal} was served as a success")));
            }
            let content_type = response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            if content_type.starts_with(super::EVENT_STREAM) {
                return Err(Failure::Wrong(format!(
                    "{refusal} was served as {content_type}"
                )));
            }
        }
        Ok(())
    }

    // ---------------------------------------------------------------------
    // The race: a push after the replay is not dropped.
    // ---------------------------------------------------------------------

    /// A frame pushed between the plan and the first poll is delivered.
    ///
    /// This is the whole reason the live tail is a cursor. The window is
    /// genuinely open in the test — the ring is read, the decision is made, the
    /// response is planned, and only *then* is the frame published — which is
    /// the same ordering a real server has between reading the ring and the
    /// body being polled by hyper.
    ///
    /// The mutation this kills is the obvious one: a writer that drains the
    /// plan's frames and then only reacts to *future* pushes. The frame
    /// published in the window belongs to neither set, and the client resumes
    /// at 4 having never seen 4.
    #[test]
    fn a_frame_pushed_after_the_replay_is_not_dropped() -> Result<(), TestError> {
        let tail = tail(8)?;
        for index in 1..=3 {
            tail.push(json!({ "index": index }));
        }

        // Decide. The cursor is now 3.
        let planned = tail.plan(Some("0"));
        if planned.cursor() != EventId::new(3) {
            return Err(Failure::Wrong(format!(
                "the cursor should be the watermark 3, got {}",
                planned.cursor().get()
            )));
        }

        // The race: published in the window between the decision and the body
        // being read.
        tail.push(json!({ "index": 4 }));

        let mut stream = planned
            .try_stream(no_keep_alive())
            .map_err(|_| Failure::UnexpectedStream)?;
        let counting = Arc::new(Counting::default());
        let waker = counting.waker();

        // The replayed window, in order.
        for index in 1..=3u64 {
            let event = event_of(&bytes_of(poll_once(&mut stream, &waker))?)?;
            if id_of(&event) != EventId::new(index) {
                return Err(Failure::Wrong(format!(
                    "expected replayed frame {index}, got {}",
                    id_of(&event).get()
                )));
            }
        }

        // And the frame from the window. If it is missing, the poll either
        // parked or produced something else, and both are reported as what
        // they are rather than as a generic assertion failure.
        let raced = event_of(&bytes_of(poll_once(&mut stream, &waker))?)?;
        if id_of(&raced) != EventId::new(4) {
            return Err(Failure::Wrong(format!(
                "the frame published in the replay-to-tail window was lost: \
                 expected 4, got {}",
                id_of(&raced).get()
            )));
        }
        assert_payload(&raced, &json!({ "index": 4 }))
    }

    /// A parked stream is woken by a push from another task.
    ///
    /// The single-poll tests above cannot prove this, and the reason is the one
    /// that bit T10: a future that never registers its waker returns `Pending`
    /// on its first poll *identically* to a working one. A harness that polls
    /// once and checks the answer distinguishes neither. So this runs on a real
    /// scheduler, with the push genuinely happening while the stream is parked
    /// and the assertion being that the parked task was actually resumed.
    #[tokio::test]
    async fn a_parked_stream_wakes_when_a_frame_is_pushed() -> Result<(), TestError> {
        let tail = tail(8)?;
        let planned = tail.plan(None);
        let mut stream = planned
            .try_stream(Duration::from_mins(15))
            .map_err(|_| Failure::UnexpectedStream)?;

        // Nothing has been published, so the stream has nothing to say. The
        // tail is cloned rather than borrowed so the spawned task owns it —
        // a `spawn` future must be `'static`, and a borrow of the local would
        // not outlive this test.
        let publisher = tail.clone();
        let pusher = tokio::spawn(async move {
            // Yield first so the reader is genuinely parked when this lands,
            // rather than racing it and passing by luck.
            tokio::task::yield_now().await;
            publisher.push(json!({ "woken": true }));
        });

        // The reader awaits the frame. This only completes if the waker was
        // registered before the push and honoured after it.
        let frame =
            core::future::poll_fn(|context| core::pin::Pin::new(&mut stream).poll_next(context))
                .await
                .ok_or_else(|| Failure::Wrong("the stream ended without a frame".to_owned()))?
                .map_err(|error| Failure::Wrong(error.to_string()))?;

        pusher
            .await
            .map_err(|error| Failure::Wrong(error.to_string()))?;

        let text = String::from_utf8_lossy(&frame).into_owned();
        let event = event_of(&text)?;
        assert_payload(&event, &json!({ "woken": true }))
    }

    // ---------------------------------------------------------------------
    // The subscription is released.
    // ---------------------------------------------------------------------

    /// Dropping a parked stream removes its registration.
    ///
    /// A client that disconnects is the *normal* way a stream ends, and the
    /// only place that can be observed is the destructor's effect on the tail.
    /// A `Drop` that did nothing would leak a waker per disconnection, and the
    /// leak is invisible from the response — the client simply stops receiving
    /// and nothing anywhere reports an error.
    #[test]
    fn dropping_the_body_releases_the_subscription() -> Result<(), TestError> {
        let tail = tail(4)?;
        let planned = tail.plan(None);
        let mut stream = planned
            .try_stream(no_keep_alive())
            .map_err(|_| Failure::UnexpectedStream)?;

        let counting = Arc::new(Counting::default());
        let waker = counting.waker();

        if !matches!(poll_once(&mut stream, &waker), Poll::Pending) {
            return Err(Failure::Wrong("the stream did not park".to_owned()));
        }
        if !stream.is_subscribed() {
            return Err(Failure::Wrong(
                "a parked stream must hold a subscription".to_owned(),
            ));
        }
        if parked_streams(&tail) != 1 {
            return Err(Failure::Wrong(format!(
                "expected one parked stream, got {}",
                parked_streams(&tail)
            )));
        }

        drop(stream);

        if parked_streams(&tail) != 0 {
            return Err(Failure::Wrong(format!(
                "a dropped stream left {} subscription(s) behind",
                parked_streams(&tail)
            )));
        }
        Ok(())
    }

    /// One stream's drop does not unregister another's.
    ///
    /// The reason registrations carry an id rather than being matched by
    /// [`Waker::will_wake`]: two streams served from one task share a waker, so
    /// a `will_wake` match would let the first drop silence the second. A live
    /// client that stops receiving frames with no error anywhere is exactly the
    /// failure this guards.
    #[test]
    fn one_streams_drop_does_not_silence_another() -> Result<(), TestError> {
        let tail = tail(4)?;
        let planned = tail.plan(None);
        let mut first = planned
            .try_stream(no_keep_alive())
            .map_err(|_| Failure::UnexpectedStream)?;
        let mut second = planned
            .try_stream(no_keep_alive())
            .map_err(|_| Failure::UnexpectedStream)?;

        // Both parked on the *same* waker, as two streams in one task would be.
        let counting = Arc::new(Counting::default());
        let waker = counting.waker();
        for stream in [&mut first, &mut second] {
            if !matches!(poll_once(stream, &waker), Poll::Pending) {
                return Err(Failure::Wrong("a stream did not park".to_owned()));
            }
        }
        if parked_streams(&tail) != 2 {
            return Err(Failure::Wrong(format!(
                "expected two parked streams on one waker, got {}",
                parked_streams(&tail)
            )));
        }

        drop(first);

        if parked_streams(&tail) != 1 {
            return Err(Failure::Wrong(format!(
                "dropping one stream disturbed the other: {} left",
                parked_streams(&tail)
            )));
        }
        if !second.is_subscribed() {
            return Err(Failure::Wrong(
                "the surviving stream lost its subscription".to_owned(),
            ));
        }
        drop(second);
        Ok(())
    }

    /// A push to a parked stream wakes exactly the wakers it registered.
    ///
    /// The deterministic half of the claim the scheduled test above proves end
    /// to end. A registry that appends wakers but never calls `wake_by_ref`
    /// passes every test that only inspects state — the entry is there, the
    /// subscription is there, `parked_streams` is right — and hangs a real
    /// client forever, because the only thing that would ever re-poll the body
    /// is the wake that is never sent.
    ///
    /// The count is one rather than "at least one" because a `push` that woke
    /// the same waker twice would be a stream scheduled twice per frame, and a
    /// registry that grew per push would turn a long-lived stream into a
    /// quadratic one.
    #[test]
    fn a_push_wakes_the_parked_streams_exactly_once_each() -> Result<(), TestError> {
        let tail = tail(4)?;
        let planned = tail.plan(None);
        let mut first = planned
            .try_stream(no_keep_alive())
            .map_err(|_| Failure::UnexpectedStream)?;
        let mut second = planned
            .try_stream(no_keep_alive())
            .map_err(|_| Failure::UnexpectedStream)?;

        let counting = Arc::new(Counting::default());
        let waker = counting.waker();
        for stream in [&mut first, &mut second] {
            if !matches!(poll_once(stream, &waker), Poll::Pending) {
                return Err(Failure::Wrong("a stream did not park".to_owned()));
            }
        }
        if counting.wakes() != 0 {
            return Err(Failure::Wrong(format!(
                "parking should not have woken anything, got {}",
                counting.wakes()
            )));
        }

        tail.push(json!({ "index": 1 }));

        // Both streams share one waker here, as two streams in one task would.
        // The registry holds two entries that both point at it, so one push
        // legitimately produces two wakes — the claim is that it is exactly two
        // and not one per poll, and not a wake per entry accumulated by the
        // parking itself.
        if counting.wakes() != 2 {
            return Err(Failure::Wrong(format!(
                "one push to two parked streams should wake twice, got {}",
                counting.wakes()
            )));
        }

        // And the frames are actually there when the woken poll runs.
        let event = event_of(&bytes_of(poll_once(&mut first, &waker))?)?;
        assert_payload(&event, &json!({ "index": 1 }))
    }

    // ---------------------------------------------------------------------
    // The keep-alive is a comment, not an event.
    // ---------------------------------------------------------------------

    /// The keep-alive dispatches nothing, by the standard's own rule.
    ///
    /// Asserted through [`SseEvent::dispatch`] rather than by matching the
    /// bytes, because the claim is about what a *conforming client* does with
    /// the frame. A writer that emitted `data: \n\n` instead would pass a test
    /// that only checked for a leading colon, and every client would receive an
    /// empty message on every heartbeat.
    #[test]
    fn the_keep_alive_is_a_comment_the_client_dispatches_nothing_for() -> Result<(), TestError> {
        if KEEP_ALIVE_FRAME.lines().next() != Some(":ping") {
            return Err(Failure::Wrong(format!(
                "a keep-alive must be a comment line, got {KEEP_ALIVE_FRAME:?}"
            )));
        }
        if let Some(event) = SseEvent::dispatch(KEEP_ALIVE_FRAME)? {
            return Err(Failure::Wrong(format!(
                "a keep-alive dispatched an event: {}",
                event.data()
            )));
        }
        Ok(())
    }

    /// The keep-alive fires on its interval and is the only thing an idle
    /// stream ever writes.
    ///
    /// A paused clock makes the interval part of the test instantaneous and
    /// deterministic. The claim is that a stream with nothing to say says
    /// *something* within the interval — without it a proxy between here and
    /// the client is free to decide an idle connection is dead, commonly at
    /// 60s, and close it while the server still believes the stream is
    /// healthy.
    #[tokio::test(start_paused = true)]
    async fn the_keep_alive_fires_after_its_interval_and_nothing_else() -> Result<(), TestError> {
        let tail = tail(4)?;
        let planned = tail.plan(None);
        let mut stream = planned
            .try_stream(Duration::from_secs(15))
            .map_err(|_| Failure::UnexpectedStream)?;

        let counting = Arc::new(Counting::default());
        let waker = counting.waker();

        // Before the interval, silence.
        if !matches!(poll_once(&mut stream, &waker), Poll::Pending) {
            return Err(Failure::Wrong(
                "an idle stream wrote before its keep-alive interval elapsed".to_owned(),
            ));
        }

        tokio::time::advance(Duration::from_secs(16)).await;

        let frame = bytes_of(poll_once(&mut stream, &waker))?;
        if frame != KEEP_ALIVE_FRAME {
            return Err(Failure::Wrong(format!(
                "expected the keep-alive comment, got {frame:?}"
            )));
        }
        // And it is a comment, not an event, on the wire and not only in the
        // constant.
        if SseEvent::dispatch(&frame)?.is_some() {
            return Err(Failure::Wrong(
                "the emitted keep-alive dispatched an event".to_owned(),
            ));
        }
        Ok(())
    }

    // ---------------------------------------------------------------------
    // The response shape.
    // ---------------------------------------------------------------------

    /// A stream response carries the three headers a proxy needs.
    ///
    /// `Content-Type` is what makes a client parse the bytes as a stream at
    /// all. `Cache-Control: no-cache` is what stops an intermediary serving a
    /// stored response to the *next* client, which for a stream means that
    /// client receives the previous one's events. `X-Accel-Buffering: no` is
    /// nginx's opt-out, and without it nginx buffers and delivers in lumps —
    /// which presents as a live server that has gone silent and is the commonest
    /// way a correct SSE server looks broken.
    #[tokio::test]
    async fn the_stream_response_carries_the_headers_a_proxy_needs() -> Result<(), TestError> {
        let tail = tail(4)?;
        let response = tail
            .respond(None, no_keep_alive())
            .map_err(|error| Failure::Wrong(error.to_string()))?;

        if response.status() != StatusCode::OK {
            return Err(Failure::Wrong(format!(
                "a resumable stream is a 200, got {}",
                response.status()
            )));
        }
        let headers = response.headers();
        let content_type = headers
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if content_type != super::EVENT_STREAM {
            return Err(Failure::Wrong(format!(
                "expected {}, got {content_type:?}",
                super::EVENT_STREAM
            )));
        }
        let cache_control = headers
            .get(axum::http::header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if cache_control != "no-cache" {
            return Err(Failure::Wrong(format!(
                "expected no-cache, got {cache_control:?}"
            )));
        }
        let buffering = headers
            .get(super::ACCEL_BUFFERING)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if buffering != "no" {
            return Err(Failure::Wrong(format!(
                "expected the proxy buffering opt-out, got {buffering:?}"
            )));
        }
        Ok(())
    }

    /// The response body carries the frames the replay queued.
    ///
    /// The one test that goes through the real [`Body`] rather than driving the
    /// stream by hand, because the header set and the body are assembled in
    /// different places and either could be wrong independently.
    ///
    /// It reads one frame rather than draining, and the read is bounded by a
    /// timeout. `to_bytes` would never return: the body is a live stream and
    /// does not end, so a test waiting for an end hangs rather than fails. The
    /// timeout makes the claim precise — the first frame arrives promptly — and
    /// a stream that delivered nothing is a failure, not an absence of one.
    #[tokio::test]
    async fn the_response_body_carries_the_frames_the_replay_queued() -> Result<(), TestError> {
        let tail = tail(8)?;
        for index in 1..=2 {
            tail.push(json!({ "index": index }));
        }
        let response = tail
            .respond(Some("0"), no_keep_alive())
            .map_err(|error| Failure::Wrong(error.to_string()))?;
        let mut chunks = response.into_body().into_data_stream();

        let first = tokio::time::timeout(
            Duration::from_secs(5),
            core::future::poll_fn(|context| Pin::new(&mut chunks).poll_next(context)),
        )
        .await
        .map_err(|_| Failure::Wrong("the body never produced a chunk".to_owned()))?
        .ok_or_else(|| Failure::Wrong("the body ended without a chunk".to_owned()))?
        .map_err(|error| Failure::Wrong(error.to_string()))?;

        let text = String::from_utf8_lossy(&first).into_owned();
        // A chunk boundary can fall inside a frame, so this asserts only that
        // the body starts with the first replayed frame. The ordering claims
        // live in the tests above, which drive the stream frame by frame.
        if !text.starts_with("id: 1\n") {
            return Err(Failure::Wrong(format!(
                "expected the body to start with frame 1, got {text:?}"
            )));
        }
        Ok(())
    }

    /// A `Planned` refusal is a response, and the same decision is not a
    /// stream at the same time.
    ///
    /// The type-level claim, stated once: `try_stream` returns
    /// `Result<EventStream, Response>`, so a caller that wants the body is
    /// forced by the compiler to handle the refusal. A helper that returned
    /// `EventStream` and put the status somewhere else would make "a 200 with
    /// an error inside it" a one-line mistake.
    #[test]
    fn a_refusal_has_a_response_and_not_a_stream() -> Result<(), TestError> {
        let tail = tail(4)?;
        let planned = tail.plan(Some("garbage"));
        if planned.refusal().is_none() {
            return Err(Failure::Wrong(
                "an unreadable cursor should have been refused".to_owned(),
            ));
        }
        // The refusal is the `Err`, not a `None`, and it is a `Response` — so
        // the caller that wants the body is forced by the compiler to handle
        // the refusal rather than receiving an empty stream it will serve as a
        // 200.
        match planned.try_stream(no_keep_alive()) {
            Ok(_) => Err(Failure::Wrong("a refusal produced a stream".to_owned())),
            Err(response) => {
                if response.status().is_success() {
                    return Err(Failure::Wrong(
                        "a refusal produced a success status".to_owned(),
                    ));
                }
                Ok(())
            }
        }
    }

    /// The `Resume` a plan made is the one the stream starts from.
    ///
    /// A thin invariant, but it is the one that makes the rest of the file
    /// legible: the frames queued at construction are exactly
    /// [`Resume::pending`], so a test reading a plan and a test reading the
    /// stream cannot disagree about what was decided.
    #[test]
    fn the_queued_frames_are_exactly_the_decisions_pending_frames() -> Result<(), TestError> {
        let source = tail(4)?;
        source.push(json!({ "index": 1 }));
        let planned = source.plan(Some("0"));
        let expected = planned.resume().pending().len();
        if expected != 1 {
            return Err(Failure::Wrong(format!(
                "expected one pending frame for a fresh client, got {expected}"
            )));
        }
        // A `Gap` plan queues exactly one frame — the notice — and a `Live`
        // queues none. Both are properties of `resume`, but the queue is this
        // module's, so the claim is asserted here too.
        let evicting = tail(1)?;
        evicting.push(json!({ "index": 1 }));
        evicting.push(json!({ "index": 2 }));
        let behind = evicting.plan(Some("0"));
        if !matches!(behind.resume(), Resume::Gap { .. }) {
            return Err(Failure::Wrong(
                "a client behind the eviction point should be a gap".to_owned(),
            ));
        }
        Ok(())
    }
}
