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
use std::sync::Mutex;

use serde_json::{Map, Value};

/// A queue of frames the peer pushed while some other call was in flight.
///
/// Draining rather than cloning: the frames are the peer's, and a caller that
/// polls in a loop should get each one exactly once.
#[derive(Debug, Default)]
pub struct Unsolicited {
    frames: Mutex<VecDeque<Map<String, Value>>>,
}

impl Unsolicited {
    /// An empty queue.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            frames: Mutex::new(VecDeque::new()),
        }
    }

    /// Parks a frame that no call was waiting for.
    ///
    /// A poisoned lock is a frame not parked. It is dropped rather than
    /// reported because the caller is in the middle of a call and has no way to
    /// act on a queue fault; the alternative — panicking — would take the
    /// bridge down over a bookkeeping problem.
    pub fn hold(&self, frame: Map<String, Value>) {
        let Ok(mut queued) = self.frames.lock() else {
            return;
        };
        queued.push_back(frame);
    }

    /// Takes everything held so far, leaving the queue empty.
    #[must_use]
    pub fn take(&self) -> Vec<Map<String, Value>> {
        let Ok(mut queued) = self.frames.lock() else {
            return Vec::new();
        };
        queued.drain(..).collect()
    }

    /// How many frames are waiting.
    #[must_use]
    pub fn len(&self) -> usize {
        self.frames.lock().map_or(0, |queued| queued.len())
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
