//! Cancellation: why a request stopped, and how a waiter learns.
//!
//! # The two halves
//!
//! [`AbortReason`] is the domain's answer to "why did this request stop", and
//! it is a value object with no machinery at all. [`CancellationToken`] is the
//! standard-library plumbing that publishes that answer to whoever is awaiting
//! it. The token is cloneable and shareable, and the reason is set exactly
//! once: the first abort wins, and a second one is ignored rather than
//! overwriting the peer's stated reason with a deadline.
//!
//! # No executor is named
//!
//! The token registers [`Waker`]s. It does not own a timer, a channel, or a
//! task, and it does not require a particular runtime to be useful — a handler
//! awaits [`CancellationToken::cancelled`] and finds out when the peer says so,
//! whichever executor the handler is running on. That is what keeps the
//! session engine free of a runtime dependency.
//!
//! # Why there is no lost-wakeup race
//!
//! The reason is published *before* the waker list is drained, and a waiter
//! re-reads the reason *after* it has taken the lock. So a waiter that
//! registered a moment too late still sees the reason on its re-read, and one
//! that registered a moment too early has its waker in the list before the
//! drain. There is no window in which a waker is both unregistered and the
//! reason already set.

use core::fmt;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

/// Why an in-flight request stopped before its handler returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbortReason {
    /// The peer sent its cancellation notification.
    ///
    /// The reason is the optional string that notification carried. The
    /// specification says it MAY be logged or shown to a user, so it is
    /// carried through rather than dropped, and it is a peer's own words
    /// rather than something this side inferred.
    Cancelled(Option<CancelReason>),
    /// The request outlived the budget the tracker was given.
    ///
    /// Distinct from a cancellation because the two are reported differently:
    /// a peer asked, or this side stopped waiting.
    Expired,
    /// The session closed underneath the request.
    ///
    /// Its own case rather than a cancellation, because nobody asked: the
    /// work is abandoned, and a handler that treated it as a peer
    /// cancellation would report a peer's decision that was never made.
    SessionClosed,
}

impl AbortReason {
    /// Whether a peer asked for this.
    #[must_use]
    pub const fn is_peer_requested(&self) -> bool {
        matches!(self, Self::Cancelled(_))
    }
}

impl fmt::Display for AbortReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled(Some(reason)) => write!(formatter, "cancelled: {reason}"),
            Self::Cancelled(None) => formatter.write_str("cancelled"),
            Self::Expired => formatter.write_str("expired"),
            Self::SessionClosed => formatter.write_str("session closed"),
        }
    }
}

/// The optional human-readable reason a cancellation notification carried.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CancelReason(String);

impl CancelReason {
    /// A stated reason.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self(reason.into())
    }

    /// The text, as the peer wrote it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CancelReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Default)]
struct AbortState {
    reason: OnceLock<AbortReason>,
    waiters: Mutex<Vec<Waker>>,
}

impl AbortState {
    /// The waker list.
    ///
    /// A poisoned lock is recovered rather than propagated. The guard only ever
    /// holds `Vec<Waker>`, and the sole way it is held across a call that can
    /// panic is `Waker::wake`, so the worst a poisoning can leave behind is a
    /// waker for a future that has since been dropped — and waking that does
    /// nothing at all. Failing every later cancellation on the session over
    /// that would be a much worse failure than the one already recorded.
    fn waiters(&self) -> MutexGuard<'_, Vec<Waker>> {
        self.waiters.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A shared, one-shot signal that a request was aborted.
///
/// Cloning shares the signal. [`cancel`](Self::cancel) is one-shot by
/// construction: the reason is stored in a [`OnceLock`], so the first caller
/// wins and later ones observe the same reason. That matters because a
/// deadline and a peer's cancellation notification can land at the same
/// instant, and whichever the request actually acted on is the one a
/// post-mortem needs to see.
#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    state: Arc<AbortState>,
}

impl CancellationToken {
    /// A token that has not been aborted.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Aborts, unless something already has.
    ///
    /// Wakes every waiter currently registered. Returns whether *this* call is
    /// the one that published the reason, which is what tells a caller racing a
    /// deadline against a peer's notification which of the two the request
    /// actually acted on.
    pub fn cancel(&self, reason: AbortReason) -> bool {
        let first = self.state.reason.set(reason).is_ok();
        if first {
            // Drain, never clear: a waiter that is about to register will
            // re-read the reason and find it set, so nothing is lost by
            // dropping the list, and the allocation is reused if a second
            // abort ever arrives.
            let mut waiters = self.state.waiters();
            for waiter in waiters.drain(..) {
                waiter.wake();
            }
        }
        first
    }

    /// Whether the request was aborted.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.state.reason.get().is_some()
    }

    /// Why it was aborted, if it was.
    #[must_use]
    pub fn reason(&self) -> Option<&AbortReason> {
        self.state.reason.get()
    }

    /// Resolves once the request is aborted.
    ///
    /// Never resolves before that, and never resolves twice.
    ///
    /// # Examples
    ///
    /// ```
    /// # async fn demo() {
    /// use protocol_droid::domain::session::{AbortReason, CancellationToken};
    ///
    /// let token = CancellationToken::new();
    /// let observer = token.clone();
    /// let signal = token.cancelled();
    /// token.cancel(AbortReason::Expired);
    /// signal.await;
    /// assert!(observer.is_cancelled());
    /// # }
    /// ```
    #[must_use]
    pub const fn cancelled(&self) -> Cancelled<'_> {
        Cancelled { token: self }
    }
}

/// A future that resolves when a [`CancellationToken`] is aborted.
///
/// Created by [`CancellationToken::cancelled`]. A `'static` handle is available
/// through [`CancellationToken::clone`] when a task has to own one.
#[derive(Debug)]
pub struct Cancelled<'a> {
    token: &'a CancellationToken,
}

impl Future for Cancelled<'_> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        if self.token.is_cancelled() {
            return Poll::Ready(());
        }
        let mut waiters = self.token.state.waiters();
        if self.token.is_cancelled() {
            return Poll::Ready(());
        }
        if !waiters.iter().any(|w| w.will_wake(context.waker())) {
            waiters.push(context.waker().clone());
        }
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {

    use core::future::Future;
    use core::pin::Pin;
    use core::sync::atomic::{AtomicUsize, Ordering};
    use core::task::{Context, Poll};
    use std::sync::Arc;
    use std::task::{Wake, Waker};

    use super::{AbortReason, CancelReason, CancellationToken};

    /// Counts how many times it was woken, so a test can prove the abort
    /// reached a waiter rather than merely being recorded.
    struct Counting {
        wakes: AtomicUsize,
    }

    impl Counting {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                wakes: AtomicUsize::new(0),
            })
        }

        fn wakes(&self) -> usize {
            self.wakes.load(Ordering::Relaxed)
        }
    }

    impl Wake for Counting {
        fn wake(self: Arc<Self>) {
            self.wakes.fetch_add(1, Ordering::Relaxed);
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.wakes.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn poll_once<F: Future>(future: Pin<&mut F>, waker: &Arc<Counting>) -> Poll<F::Output> {
        let waker = Waker::from(waker.clone());
        let mut context = Context::from_waker(&waker);
        future.poll(&mut context)
    }

    #[test]
    fn an_uncancelled_token_leaves_a_waiter_pending_and_does_not_wake_it() {
        let token = CancellationToken::new();
        let counter = Counting::new();
        let mut signal = Box::pin(token.cancelled());
        assert_eq!(poll_once(signal.as_mut(), &counter), Poll::Pending);
        assert_eq!(counter.wakes(), 0, "nothing happened, so nothing to wake");
        assert!(!token.is_cancelled());
    }

    #[test]
    fn aborting_wakes_exactly_the_waiters_that_registered() {
        let token = CancellationToken::new();
        let counter = Counting::new();
        let mut signal = Box::pin(token.cancelled());
        assert_eq!(poll_once(signal.as_mut(), &counter), Poll::Pending);

        assert!(token.cancel(AbortReason::Cancelled(Some(CancelReason::new("stop")))));
        assert_eq!(counter.wakes(), 1, "the registered waiter was woken");
        assert_eq!(poll_once(signal.as_mut(), &counter), Poll::Ready(()));
    }

    #[test]
    fn a_token_cancelled_before_anything_waits_still_reads_as_cancelled() {
        let token = CancellationToken::new();
        let counter = Counting::new();
        assert!(token.cancel(AbortReason::Expired));
        // Nothing was waiting, so nobody is woken — but the signal is already
        // ready, which is the case a handler that was slow to start hits.
        assert_eq!(counter.wakes(), 0);
        let mut signal = Box::pin(token.cancelled());
        assert_eq!(poll_once(signal.as_mut(), &counter), Poll::Ready(()));
    }

    #[test]
    fn a_second_abort_is_reported_as_a_no_op_and_keeps_the_first_reason() {
        let token = CancellationToken::new();
        assert!(token.cancel(AbortReason::Cancelled(Some(CancelReason::new("first")))));
        assert!(
            !token.cancel(AbortReason::Expired),
            "the first reason is the one that happened"
        );
        let reason = token.reason();
        assert!(
            matches!(reason, Some(AbortReason::Cancelled(Some(inner))) if inner.as_str() == "first"),
            "the first reason is the one that stuck, got {reason:?}"
        );
    }

    #[test]
    fn only_a_peer_cancellation_counts_as_peer_requested() {
        assert!(AbortReason::Cancelled(None).is_peer_requested());
        assert!(AbortReason::Cancelled(Some(CancelReason::new("why"))).is_peer_requested());
        assert!(!AbortReason::Expired.is_peer_requested());
        assert!(!AbortReason::SessionClosed.is_peer_requested());
    }
}
