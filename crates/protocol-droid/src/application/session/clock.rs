//! Where the session's sense of time comes from.
//!
//! # Why a port and not `Instant::now`
//!
//! The session engine decides that a request has outlived its budget, and that
//! decision is arithmetic over two readings. If it took the second reading from
//! a global clock, a test could not set up a request that started 90 seconds
//! ago without sleeping for 90 seconds, and a deterministic domain would stop
//! being one. Everything else follows from this: the clock is injected, the
//! reading is a [`std::time::Duration`] from an origin the adapter chose, and no session
//! code calls a clock function at all.
//!
//! # Why the origin is the adapter's business
//!
//! Two sessions opened in the same millisecond must still be distinguishable,
//! and a wall clock can step backwards under NTP. A duration from a monotonic
//! source has neither problem, and which monotonic source is the transport's
//! problem — `Instant` against process start, a monotonic clock value, a
//! virtual time in a test.

use crate::domain::session::Instant;

/// A monotonic source of time.
///
/// Declared by the engine, which is its only consumer, and consumed through
/// `impl Clock` so the call monomorphises. One method: the engine reads the
/// clock and does the arithmetic itself, because "how long is a sampling
/// request allowed to take" is a session decision and a timer that knows the
/// policy would be a policy in an adapter.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use protocol_droid::application::session::Clock;
/// use protocol_droid::domain::session::Instant;
///
/// /// A clock the test drives by hand.
/// struct TestClock(std::sync::atomic::AtomicU64);
///
/// impl Clock for TestClock {
///     fn now(&self) -> Instant {
///         let millis = self.0.load(std::sync::atomic::Ordering::Relaxed);
///         Instant::from_duration(Duration::from_millis(millis))
///     }
/// }
///
/// let clock = TestClock(std::sync::atomic::AtomicU64::new(1_000));
/// assert_eq!(clock.now().as_duration(), Duration::from_secs(1));
/// ```
pub trait Clock {
    /// The current reading.
    ///
    /// Only the difference between two readings of the same clock is
    /// meaningful; the origin carries no information.
    fn now(&self) -> Instant;
}
