//! `Stream` wrappers for the peer's notification and agent-request channels.
//!
//! # Why a wrapper
//!
//! The peer exposes its notification and agent-request receivers as
//! `&self`-borrowing `recv_*` methods that hold a `tokio::sync::Mutex`
//! across an `await`. A consumer's main loop is, in practice, a
//! `tokio::select!` between "the next streamed notification", "the next
//! agent-originating request", and "the reply to my in-flight request";
//! that shape wants `Stream` combinators (`map`, `filter`,
//! `take_while`, `for_each`, …) rather than three ad-hoc `recv` calls.
//!
//! The wrappers here turn the two `Mutex<Receiver>` channels into
//! `Stream`s. The implementation is a three-state machine: `Idle` (no
//! lock held yet), `Locking` (waiting on the mutex), and `Ready` (the
//! lock guard is held and the inner receiver is the one to poll). On
//! every `poll_next` the state machine either advances a step or polls
//! the receiver. The lock guard is held across `Pending` polls of the
//! inner `recv`, so a waker does not lose its place; the guard is
//! released between successive values, which mirrors what
//! `recv_notification` does today (the guard goes out of scope at the
//! end of the function).
//!
//! # Why a `Stream` and not a `BroadcastStream` or `WatchStream`
//!
//! The existing channel is an `mpsc` with one consumer, which is the
//! shape `Stream` describes directly. `tokio_stream::wrappers` provides
//! `ReceiverStream` for an *owned* receiver; the receiver here is
//! behind an `Arc<Mutex>` so two consumers (`recv_notification` and the
//! new stream) can share it, and `ReceiverStream` does not compose
//! with that. A bespoke state machine is the only way to keep the
//! existing `recv_*` API working alongside a `Stream` API.

use core::future::Future;
use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::sync::Arc;

use futures_core::Stream;
use tokio::sync::{Mutex, OwnedMutexGuard, mpsc};

use crate::domain::acp::notification::SessionNotification;

use super::agent::AgentRequest;

/// A `Stream` of [`SessionNotification`]s the agent streams to the
/// client.
///
/// Returned by [`AcpStdioPeer::notifications`](super::peer::AcpStdioPeer::notifications).
/// The stream owns a clone of the peer's notification-receiver mutex
/// and re-acquires it on every poll; the lock guard is held across
/// `Pending` polls so another task cannot race in and reorder
/// notifications.
///
/// `&self` (not `&mut self`) and `+ '_` so the consumer can hold the
/// peer by `Arc` and call `notifications()` from any task without
/// re-entering a `&mut self` borrow — the same shape
/// [`recv_notification`](super::peer::AcpStdioPeer::recv_notification)
/// supports, but as a `Stream`.
///
/// # Cancel safety
///
/// `Stream::poll_next` is cancel-safe in the same way `Receiver::recv`
/// is: a `select!` that polls this stream and another branch is
/// guaranteed to either return the notification or hand the lock back
/// unchanged, never a partially-consumed channel.
///
/// # Examples
///
/// ```ignore
/// use futures_util::StreamExt;
/// let mut stream = peer.notifications();
/// while let Some(notification) = stream.next().await {
///     render(notification);
/// }
/// ```
pub struct NotificationStream<'a> {
    /// The shared lock that guards the receiver. Cloned from the peer
    /// at construction; the peer's own `recv_notification` method
    /// holds the same `Arc`, so a stream and the `recv_*` method are
    /// mutually exclusive inside one task.
    rx: Arc<Mutex<mpsc::Receiver<SessionNotification>>>,
    /// The state machine. The three states are listed in the module
    /// docs.
    state: NotificationState,
    /// The peer's lifetime. The stream holds a clone of the peer's
    /// `Arc<Mutex>`, not a borrow of the peer, so the lifetime is
    /// structural — the marker is for the documentation claim and
    /// for any future borrow-style refactor.
    _marker: PhantomData<&'a ()>,
}

/// The three states a [`NotificationStream`] can be in.
///
/// Variants are `Box`-pinned where they hold a future, because the
/// future inside `Locking` is `dyn Future + Send` and the boxed pin
/// gives it a stable address for the waker to register against.
enum NotificationState {
    /// The stream has not yet acquired the lock, or has just dropped
    /// its guard because the receiver returned `None`. The first
    /// `poll_next` enters `Locking`.
    Idle,
    /// Waiting to acquire the mutex. The future is owned (not
    /// borrowed) so the stream can move it across the `Idle →
    /// Locking → Ready` cycle.
    Locking(
        Pin<Box<dyn Future<Output = OwnedMutexGuard<mpsc::Receiver<SessionNotification>>> + Send>>,
    ),
    /// Lock held; the receiver is the one to poll.
    Ready(OwnedMutexGuard<mpsc::Receiver<SessionNotification>>),
}

impl NotificationStream<'_> {
    /// Build a stream over the peer's notification channel. The
    /// `Arc<Mutex<Receiver>>` is cloned, not moved, so the peer keeps
    /// its own `recv_notification` method working in parallel.
    pub(crate) const fn new(rx: Arc<Mutex<mpsc::Receiver<SessionNotification>>>) -> Self {
        Self {
            rx,
            state: NotificationState::Idle,
            _marker: PhantomData,
        }
    }
}

impl core::fmt::Debug for NotificationStream<'_> {
    /// The type only — neither the receiver nor the lock state is
    /// worth showing. A `Debug` for a stream that prints its
    /// internal `Pin<Box<dyn Future>>` would format the future's
    /// internal waker and pointer, which is a leak of the executor's
    /// representation into a log line that should be the consumer's
    /// view of the stream.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("NotificationStream")
            .finish_non_exhaustive()
    }
}

impl core::fmt::Debug for AgentRequestStream<'_> {
    /// See [`NotificationStream`]'s `Debug` — the same reasoning
    /// applies to the agent-request stream.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("AgentRequestStream")
            .finish_non_exhaustive()
    }
}

impl Stream for NotificationStream<'_> {
    type Item = SessionNotification;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // The state machine is `Idle → Locking → Ready` and back to
        // the top of the loop. Each pass through the body holds a
        // `&mut self` borrow only for the duration of the `match`
        // arm — when an arm falls through to the next iteration the
        // previous borrow has already been released, so the next
        // `self.as_mut().get_mut()` reborrows from scratch. That is
        // what keeps `significant_drop_tightening` happy: no single
        // `let this = self.get_mut()` lives for the whole loop.
        loop {
            match self.as_mut().get_mut().step(cx) {
                PollStep::Progress => {}
                PollStep::Pending => return Poll::Pending,
                PollStep::Ready(item) => return Poll::Ready(item),
            }
        }
    }
}

/// The advance signal returned by a per-state `step` helper. A
/// loop-driver turns `Progress` into another iteration, `Pending`
/// into `Poll::Pending`, and `Ready` into the corresponding
/// `Poll::Ready`.
enum PollStep<T> {
    /// The state machine advanced (e.g. `Idle` → `Locking` or
    /// `Locking` → `Ready`); loop again.
    Progress,
    /// A future registered the waker and is not yet ready; return
    /// `Poll::Pending`.
    Pending,
    /// The receiver returned a value or signalled closure.
    Ready(Option<T>),
}

impl NotificationStream<'_> {
    /// One step of the state machine. The stream's
    /// `poll_next` calls this in a loop so each `&mut self` borrow
    /// only lives for the duration of one `match` arm.
    fn step(&mut self, cx: &mut Context<'_>) -> PollStep<SessionNotification> {
        match &mut self.state {
            NotificationState::Idle => {
                let fut = self.rx.clone().lock_owned();
                self.state = NotificationState::Locking(Box::pin(fut));
                PollStep::Progress
            }
            NotificationState::Locking(fut) => match fut.as_mut().poll(cx) {
                Poll::Ready(guard) => {
                    self.state = NotificationState::Ready(guard);
                    PollStep::Progress
                }
                Poll::Pending => PollStep::Pending,
            },
            NotificationState::Ready(guard) => match guard.poll_recv(cx) {
                Poll::Ready(item) => {
                    // Drop the guard before the next iteration so the
                    // next `Idle` arm re-acquires the lock cleanly.
                    // Releasing between values matches what
                    // `recv_notification` does today: the guard goes
                    // out of scope at the end of that function.
                    self.state = NotificationState::Idle;
                    PollStep::Ready(item)
                }
                Poll::Pending => PollStep::Pending,
            },
        }
    }
}

/// A `Stream` of [`AgentRequest`]s the agent sends to the client.
///
/// Returned by
/// [`AcpStdioPeer::agent_requests`](super::peer::AcpStdioPeer::agent_requests).
/// The shape is the same as [`NotificationStream`], with the
/// agent-request channel in place of the notification channel. The
/// agent's `fs/read_text_file`, `fs/write_text_file`,
/// `terminal/create`, `terminal/output`, `terminal/release`,
/// `terminal/wait_for_exit`, `terminal/kill`, and
/// `session/request_permission` requests flow through this stream.
pub struct AgentRequestStream<'a> {
    rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
    state: AgentRequestState,
    _marker: PhantomData<&'a ()>,
}

enum AgentRequestState {
    Idle,
    Locking(Pin<Box<dyn Future<Output = OwnedMutexGuard<mpsc::Receiver<AgentRequest>>> + Send>>),
    Ready(OwnedMutexGuard<mpsc::Receiver<AgentRequest>>),
}

impl AgentRequestStream<'_> {
    /// Build a stream over the peer's agent-request channel. See
    /// [`NotificationStream::new`] for the lifetime / ownership
    /// contract — the two are structurally identical.
    pub(crate) const fn new(rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>) -> Self {
        Self {
            rx,
            state: AgentRequestState::Idle,
            _marker: PhantomData,
        }
    }
}

impl Stream for AgentRequestStream<'_> {
    type Item = AgentRequest;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // See [`NotificationStream::poll_next`] — the same shape and
        // the same `significant_drop_tightening` reason. Each pass
        // through the body holds a `&mut self` borrow only for the
        // duration of one `match` arm.
        loop {
            match self.as_mut().get_mut().step(cx) {
                PollStep::Progress => {}
                PollStep::Pending => return Poll::Pending,
                PollStep::Ready(item) => return Poll::Ready(item),
            }
        }
    }
}

impl AgentRequestStream<'_> {
    /// One step of the state machine. See
    /// [`NotificationStream::step`] for the rationale — the two
    /// streams are structurally identical.
    fn step(&mut self, cx: &mut Context<'_>) -> PollStep<AgentRequest> {
        match &mut self.state {
            AgentRequestState::Idle => {
                let fut = self.rx.clone().lock_owned();
                self.state = AgentRequestState::Locking(Box::pin(fut));
                PollStep::Progress
            }
            AgentRequestState::Locking(fut) => match fut.as_mut().poll(cx) {
                Poll::Ready(guard) => {
                    self.state = AgentRequestState::Ready(guard);
                    PollStep::Progress
                }
                Poll::Pending => PollStep::Pending,
            },
            AgentRequestState::Ready(guard) => match guard.poll_recv(cx) {
                Poll::Ready(item) => {
                    self.state = AgentRequestState::Idle;
                    PollStep::Ready(item)
                }
                Poll::Pending => PollStep::Pending,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    //! The state machine is straightforward enough that the unit
    //! tests live next to the type and exercise the three
    //! observable shapes: the lock future advances under contention,
    //! the stream yields values that were already in the channel,
    //! and the stream returns `None` when the sender is dropped.
    //!
    //! The integration test in `tests/acp_stdio_interop.rs` covers
    //! the cross-process shape (a real agent on the other end of
    //! the pipe). What is here is the in-process state-machine
    //! correctness: the lock guard survives across `Pending` polls,
    //! the channel closes when the sender is dropped, and a second
    //! stream over the same `Arc` does not deadlock against itself.
    use super::*;
    use crate::domain::acp::content::{ContentBlock, TextContent};
    use crate::domain::acp::notification::SessionUpdate;
    use crate::domain::acp::session::SessionId;
    use futures_util::StreamExt;
    use std::collections::BTreeMap;

    fn text_chunk(text: &str) -> SessionUpdate {
        SessionUpdate::UserMessageChunk(crate::domain::acp::notification::ContentChunk {
            content: ContentBlock::Text(TextContent {
                text: text.to_owned(),
                annotations: None,
                meta: BTreeMap::new(),
            }),
            message_id: None,
            meta: BTreeMap::new(),
        })
    }

    fn notification(text: &str) -> SessionNotification {
        SessionNotification {
            session_id: SessionId::new("test-session"),
            update: text_chunk(text),
        }
    }

    #[tokio::test]
    async fn yields_values_already_in_the_channel() {
        // The simplest path: a sender drops after a single value;
        // the stream returns `Some(value)` then `None`.
        let (tx, rx) = mpsc::channel::<SessionNotification>(4);
        let rx = Arc::new(Mutex::new(rx));
        let send_result = tx.send(notification("hello")).await;
        assert!(
            send_result.is_ok(),
            "the channel is open at the time of send"
        );
        drop(tx);

        let mut stream = NotificationStream::new(rx);
        let first = stream.next().await;
        assert!(first.is_some(), "the queued value must surface");
        let second = stream.next().await;
        assert!(
            second.is_none(),
            "the channel is closed after the sender drops"
        );
    }

    #[tokio::test]
    async fn returns_none_when_sender_already_dropped() {
        // The empty-channel-closed path: drop the sender before
        // polling. The stream must not block.
        let (tx, rx) = mpsc::channel::<SessionNotification>(4);
        drop(tx);
        let rx = Arc::new(Mutex::new(rx));
        let mut stream = NotificationStream::new(rx);
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn a_second_stream_does_not_deadlock_against_itself() {
        // Two streams over the same `Arc<Mutex<Receiver>>` contend
        // for the same lock. One wins; the other parks until the
        // first drops its guard. The expectation is that the second
        // wakes up and observes the closed channel.
        let (tx, rx) = mpsc::channel::<SessionNotification>(4);
        let rx = Arc::new(Mutex::new(rx));
        let send_result = tx.send(notification("hello")).await;
        assert!(
            send_result.is_ok(),
            "the channel is open at the time of send"
        );
        drop(tx);

        let mut first = NotificationStream::new(Arc::clone(&rx));
        let mut second = NotificationStream::new(Arc::clone(&rx));
        // The first stream takes the only value; the second
        // observes the channel close.
        assert!(first.next().await.is_some());
        assert!(second.next().await.is_none());
    }

    #[tokio::test]
    async fn agent_request_stream_round_trip() {
        // The agent-request stream uses the same state machine, so
        // the same shape of test covers it.
        let (tx, rx) = mpsc::channel::<AgentRequest>(4);
        let rx = Arc::new(Mutex::new(rx));
        let send_result = tx
            .send(AgentRequest {
                id: serde_json::json!(1),
                method: "fs/read_text_file".to_owned(),
                params: serde_json::json!({}),
            })
            .await;
        assert!(
            send_result.is_ok(),
            "the channel is open at the time of send"
        );
        drop(tx);

        let mut stream = AgentRequestStream::new(rx);
        let first = stream.next().await;
        let method = match first {
            Some(request) => request.method,
            None => String::new(),
        };
        assert_eq!(method, "fs/read_text_file");
        assert!(stream.next().await.is_none());
    }
}
