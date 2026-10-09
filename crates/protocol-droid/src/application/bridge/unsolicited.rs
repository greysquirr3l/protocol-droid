//! Frames a peer sent that no call in flight was waiting for.
//!
//! Two of this crate's transports put replies somewhere other than the response
//! to the request that asked for them — `2024-11-05` answers `202` and streams
//! the answer later, and a `stdio` server may push a notification between the
//! write and the read. A peer that reads until it finds its own id therefore
//! walks straight past frames that belong to nobody.
//!
//! What it must not do is drop them. The crate's promise is that an unmodelled
//! directive reaches the other side byte-for-byte, and a notification that
//! happened to land mid-call is real traffic that somebody downstream is
//! waiting for. So a peer that skips a frame parks it here, and the caller
//! decides where it goes next.
//!
//! ## Why this lives in `application` and not in each adapter
//!
//! Both peers want it, and the honest factoring for something two adapters want
//! is the layer they both already depend on. Putting it here also keeps the
//! decision in one place: *a frame the bridge could not attribute is never
//! discarded* is a property of the bridge, not of HTTP.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use serde_json::{Map, Value};

/// A queue of frames the peer pushed while some other call was in flight.
///
/// Draining rather than cloning: the frames are the peer's, and a caller that
/// polls in a loop should get each one exactly once.
#[derive(Debug, Default)]
pub struct Unsolicited {
    /// The frames and the wakers together, under one lock.
    ///
    /// Together because "is the queue empty, and if so park me" has to be one
    /// atomic step. With the queue and the waker list under separate locks a
    /// frame could land between the check and the park, and the waker that
    /// would have been told about it is the one just stored.
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug, Default)]
struct Inner {
    frames: VecDeque<Map<String, Value>>,
    wakers: Vec<Waker>,
}

/// Resolves once something has been parked in an [`Unsolicited`].
#[derive(Debug)]
struct Pending {
    inner: Arc<Mutex<Inner>>,
}

impl Future for Pending {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let Ok(mut inner) = self.inner.lock() else {
            // A poisoned lock means the queue is unreadable, so waiting on it
            // is waiting forever. Resolving lets the caller drain, see
            // nothing, and carry on — which beats hanging.
            return Poll::Ready(());
        };
        if !inner.frames.is_empty() {
            return Poll::Ready(());
        }
        if !inner.wakers.iter().any(|held| held.will_wake(cx.waker())) {
            inner.wakers.push(cx.waker().clone());
        }
        Poll::Pending
    }
}

impl Unsolicited {
    /// An empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
        }
    }

    /// Parks a frame that no call was waiting for.
    ///
    /// A poisoned lock is a frame not parked. It is dropped rather than
    /// reported because the caller is in the middle of a call and has no way to
    /// act on a queue fault; the alternative — panicking — would take the
    /// bridge down over a bookkeeping problem.
    pub fn hold(&self, frame: Map<String, Value>) {
        let mut wakers = Vec::new();
        {
            let Ok(mut inner) = self.inner.lock() else {
                return;
            };
            inner.frames.push_back(frame);
            wakers.append(&mut inner.wakers);
        }
        // Woken outside the lock: a woken task polled straight away takes this
        // lock, and holding it across `wake` is how a wake-up becomes a stall.
        for waker in wakers {
            waker.wake();
        }
    }

    /// Takes everything held so far, leaving the queue empty.
    #[must_use]
    pub fn take(&self) -> Vec<Map<String, Value>> {
        self.inner
            .lock()
            .map_or_else(|_| Vec::new(), |mut inner| inner.frames.drain(..).collect())
    }

    /// Resolves once something has been parked here.
    ///
    /// A queue that is already non-empty resolves on the first poll, which is
    /// what stops a caller parking a frame between its own `take` and its first
    /// poll from waiting for a wake-up that has already happened.
    ///
    /// No `#[must_use]` here: the returned future is itself `#[must_use]`, so
    /// a second copy of the attribute adds nothing this signature does not
    /// already say.
    pub fn wait(&self) -> impl Future<Output = ()> + Send + 'static {
        Pending {
            inner: Arc::clone(&self.inner),
        }
    }

    /// How many frames are waiting.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().map_or(0, |inner| inner.frames.len())
    }

    /// Whether nothing is waiting.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value, json};

    use super::Unsolicited;

    fn frame(id: i64) -> Map<String, Value> {
        json!({"jsonrpc": "2.0", "id": id})
            .as_object()
            .cloned()
            .unwrap_or_default()
    }

    /// A held frame comes back, in order, exactly once.
    #[test]
    fn a_held_frame_comes_back_exactly_once() {
        let held = Unsolicited::new();
        held.hold(frame(1));
        held.hold(frame(2));
        assert_eq!(held.len(), 2);

        let first = held.take();
        assert_eq!(first.len(), 2);
        assert_eq!(first.first().and_then(|f| f.get("id")), Some(&json!(1)));
        assert_eq!(first.get(1).and_then(|f| f.get("id")), Some(&json!(2)));

        // Draining, not cloning: a second poll must not hand them back.
        assert!(held.take().is_empty());
        assert!(held.is_empty());
    }

    /// A queue nobody wrote to reports itself empty rather than guessing.
    #[test]
    fn a_fresh_queue_is_empty() {
        let held = Unsolicited::new();
        assert!(held.is_empty());
        assert_eq!(held.len(), 0);
        assert!(held.take().is_empty());
    }
}
