//! The production [`Clock`]: a monotonic reading of the operating system's
//! clock.
//!
//! `Clock` has been a port with only test implementations since T10. A reaper
//! that needs one has to be handed one, and writing a `Stepping` clock in
//! production code by accident is the failure the other injected sources
//! (`RandomIds`) already exist to prevent.

use std::time::Instant as StdInstant;

use crate::application::session::Clock;
use crate::domain::session::Instant;

/// Readings of [`std::time::Instant`], measured from the moment the clock was
/// made.
///
/// Monotonic, so a wall-clock step (NTP, a suspended laptop being corrected)
/// cannot make a session look older or younger than it is. The origin is the
/// clock's own construction; only the difference between two readings means
/// anything, which is all [`Clock`] promises.
///
/// # Examples
///
/// ```
/// use protocol_droid::adapters::secondary::clock::MonotonicClock;
/// use protocol_droid::application::session::Clock;
///
/// let clock = MonotonicClock::new();
/// let first = clock.now();
/// let second = clock.now();
/// assert!(second.as_duration() >= first.as_duration());
/// ```
#[derive(Debug, Clone, Copy)]
pub struct MonotonicClock {
    origin: StdInstant,
}

impl MonotonicClock {
    /// A clock whose zero is now.
    #[must_use]
    pub fn new() -> Self {
        Self {
            origin: StdInstant::now(),
        }
    }
}

impl Default for MonotonicClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for MonotonicClock {
    /// Time elapsed since this clock was made.
    fn now(&self) -> Instant {
        Instant::from_duration(self.origin.elapsed())
    }
}

#[cfg(test)]
mod tests {
    use super::MonotonicClock;
    use crate::application::session::Clock;

    #[test]
    fn readings_never_go_backwards() {
        let clock = MonotonicClock::new();
        let mut last = clock.now().as_duration();
        for _ in 0..1000 {
            let next = clock.now().as_duration();
            assert!(next >= last);
            last = next;
        }
    }
}
