//! Per-subject request accounting, and the map that holds it.
//!
//! The decision this makes — [`check`] from `domain::transport::rate_limit` —
//! is a pure function. What lives here is the part that is not: the counters,
//! the clock, and the arithmetic of keeping the table from growing without
//! bound.
//!
//! ## Why the table is bounded
//!
//! The obvious implementation is a `HashMap<subject, Window>` that only ever
//! grows, and it is a slow memory leak with a remote trigger: a principal name
//! is attacker-influenced on an unauthenticated endpoint, so every rejected
//! `Authorization` header is another entry that never expires on its own. An
//! attacker who can make a server mint tokens they will not reuse gets a
//! process to grow without limit, one small string at a time.
//!
//! So the table is capped. When it fills, entries whose window has already
//! elapsed are dropped first — they carry no state worth keeping — and only if
//! that is not enough is the whole table cleared. Clearing under pressure is
//! deliberately harsher than it needs to be: it hands every subject a fresh
//! window, so the cost is a rate limit that is briefly more generous. That is
//! the right side to err on. Refusing requests that should have been allowed is
//! a bug a client reports; serving more than configured is the thing this
//! whole module exists to prevent.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::domain::{RateLimit, Verdict, check_rate_limit as check};

/// Entries held before the table is swept.
const DEFAULT_CAPACITY: usize = 4096;

/// One subject's current window.
#[derive(Debug)]
struct Window {
    /// When this window opened. `elapsed` for [`check`] is measured from here.
    opened: Instant,
    /// Requests counted in this window, **including** the one being judged.
    seen: u32,
}

/// Per-subject rate limiting.
///
/// Cheap to clone into an axum layer: the counters are behind an [`Arc`], and
/// two clones are one table rather than two.
#[derive(Clone, Debug)]
pub struct RateLimiter {
    policy: RateLimit,
    capacity: usize,
    windows: Arc<Mutex<HashMap<Arc<str>, Window>>>,
}

impl RateLimiter {
    /// A limiter applying `policy`, holding [`DEFAULT_CAPACITY`] subjects.
    #[must_use]
    pub fn new(policy: RateLimit) -> Self {
        Self::with_capacity(policy, DEFAULT_CAPACITY)
    }

    /// A limiter holding at most `capacity` subjects before it sweeps.
    ///
    /// `capacity` is never read as zero: a table that cannot hold a subject
    /// would clear itself on every request and limit nobody.
    #[must_use]
    pub fn with_capacity(policy: RateLimit, capacity: usize) -> Self {
        Self {
            policy,
            capacity: capacity.max(1),
            windows: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The policy this limiter applies.
    #[must_use]
    pub const fn policy(&self) -> &RateLimit {
        &self.policy
    }

    /// Counts one request from `subject` and says whether it may proceed.
    ///
    /// The clock is read here rather than passed in so a caller cannot
    /// accidentally supply a stale `Instant`; [`check_at`](Self::check_at) is
    /// the same function with the clock supplied, and exists for tests.
    #[must_use]
    pub fn check(&self, subject: &str) -> Verdict {
        self.check_at(subject, Instant::now())
    }

    /// [`check`](Self::check) with the observation time supplied.
    ///
    /// Public because a test that has to sleep for a wall-clock window is a
    /// test that is skipped in CI.
    #[must_use]
    pub fn check_at(&self, subject: &str, now: Instant) -> Verdict {
        // A poisoned lock must not become an outage. The map holds only
        // counters, so recovering the guard and treating the subject as unseen
        // is both safe and the conservative direction: it costs the caller one
        // request's worth of allowance, and it never lets a request through
        // that would otherwise have been refused.
        let Ok(mut windows) = self.windows.lock() else {
            return Verdict::Allowed {
                remaining: self.policy.limit(),
            };
        };

        if windows.len() >= self.capacity {
            self.sweep(&mut windows, now);
        }

        let entry = windows
            .entry(Arc::from(subject))
            .or_insert(Window { opened: now, seen: 0 });

        // A window that has elapsed starts again, and the elapsed time is
        // measured from the new opening rather than from the old one.
        let elapsed = now.saturating_duration_since(entry.opened);
        if elapsed >= self.policy.window() {
            entry.opened = now;
            entry.seen = 0;
        }

        let verdict = check(&self.policy, entry.seen, now.saturating_duration_since(entry.opened));

        // `seen` counts *including* this request. Only an allowed request
        // consumes allowance: a refusal must not count the caller twice for
        // the attempt that was already turned away, or a client that backs off
        // correctly is punished faster than one that does not.
        if matches!(verdict, Verdict::Allowed { .. }) {
            entry.seen = entry.seen.saturating_add(1);
        }

        verdict
    }

    /// Drops subjects whose window has elapsed, and the lot if that is not
    /// enough room.
    fn sweep(&self, windows: &mut HashMap<Arc<str>, Window>, now: Instant) {
        windows.retain(|_, window| now.saturating_duration_since(window.opened) < self.policy.window());
        if windows.len() >= self.capacity {
            windows.clear();
        }
    }

    /// How many subjects are currently tracked.
    ///
    /// Public so a test can assert the bound is actually a bound.
    #[must_use]
    pub fn tracked(&self) -> usize {
        self.windows.lock().map_or(0, |windows| windows.len())
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU32;
    use core::time::Duration;
    use std::time::Instant;

    use crate::domain::{RateLimit, Verdict};

    use super::RateLimiter;

    type TestError = Box<dyn std::error::Error>;

    fn limiter_policy(limit: u32, window: Duration) -> Result<RateLimit, TestError> {
        Ok(RateLimit::new(
            NonZeroU32::new(limit).ok_or("a fixture asked for a zero limit")?,
            window,
        ))
    }

    fn limiter(limit: u32, window: Duration) -> Result<RateLimiter, TestError> {
        Ok(RateLimiter::new(RateLimit::new(
            NonZeroU32::new(limit).ok_or("a fixture asked for a zero limit")?,
            window,
        )))
    }

    /// The limiter counts per subject: one client hammering must not spend
    /// another's allowance. This is the whole reason the key exists.
    #[test]
    fn subjects_do_not_spend_each_others_allowance() -> Result<(), TestError> {
        let limiter = limiter(2, Duration::from_secs(60))?;
        let now = Instant::now();

        for _ in 0..2 {
            assert!(
                matches!(limiter.check_at("alice", now), Verdict::Allowed { .. }),
                "alice should be within her own allowance"
            );
        }
        assert_matches::assert_matches!(
            limiter.check_at("alice", now),
            Verdict::Limited { .. }
        );

        assert!(
            matches!(limiter.check_at("bob", now), Verdict::Allowed { .. }),
            "bob must not inherit alice's spent window"
        );
        Ok(())
    }

    /// A refusal must not itself consume allowance. If it did, a client that
    /// backs off correctly would be punished faster than one that hammers.
    #[test]
    fn a_refusal_does_not_consume_allowance() -> Result<(), TestError> {
        let limiter = limiter(1, Duration::from_secs(60))?;
        let now = Instant::now();

        assert_matches::assert_matches!(
            limiter.check_at("alice", now),
            Verdict::Allowed { .. }
        );
        // Three refusals in a row must not deepen the hole.
        for _ in 0..3 {
            assert_matches::assert_matches!(
                limiter.check_at("alice", now),
                Verdict::Limited { .. }
            );
        }
        Ok(())
    }

    /// The window reopens when it elapses, without the caller resetting
    /// anything.
    #[test]
    fn the_allowance_comes_back_after_the_window() -> Result<(), TestError> {
        let limiter = limiter(1, Duration::from_secs(60))?;
        let start = Instant::now();

        assert_matches::assert_matches!(
            limiter.check_at("alice", start),
            Verdict::Allowed { .. }
        );
        assert_matches::assert_matches!(
            limiter.check_at("alice", start + Duration::from_secs(30)),
            Verdict::Limited { .. }
        );
        assert!(
            matches!(
                limiter.check_at("alice", start + Duration::from_secs(61)),
                Verdict::Allowed { .. }
            ),
            "a spent window must reopen on its own"
        );
        Ok(())
    }

    /// A subject name is attacker-influenced, so the table must not be a slow
    /// remote-triggered leak. This is the test that says the bound is real.
    #[test]
    fn the_table_stays_bounded_under_distinct_subjects() -> Result<(), TestError> {
        let limiter = RateLimiter::with_capacity(limiter_policy(10, Duration::from_secs(3600))?, 16);
        let now = Instant::now();

        for index in 0..5000u32 {
            let _ = limiter.check_at(&format!("subject-{index}"), now);
        }

        assert!(
            limiter.tracked() <= 16,
            "the table grew to {} subjects under 5000 distinct names",
            limiter.tracked()
        );
        Ok(())
    }

    /// A sweep drops what has expired *before* dropping anything, so a full
    /// table does not reset subjects that are still inside their window.
    ///
    /// The previous version of this test made every subject expire at once,
    /// which meant it passed for any implementation that swept at all — it
    /// could not tell "preferred the expired ones" from "threw the lot away".
    #[test]
    fn sweeping_drops_expired_subjects_and_keeps_live_ones() -> Result<(), TestError> {
        let limiter = RateLimiter::with_capacity(limiter_policy(5, Duration::from_secs(60))?, 4);
        let start = Instant::now();

        // Three subjects whose window closes while we are not looking.
        for index in 0..3 {
            let _ = limiter.check_at(&format!("old-{index}"), start);
        }
        assert_eq!(limiter.tracked(), 3, "three subjects so far");

        // A live one, which brings the table to its capacity of four.
        let _ = limiter.check_at("live", start + Duration::from_secs(90));
        assert_eq!(limiter.tracked(), 4, "the table is now full");

        // The next request finds a full table and sweeps. The three expired
        // subjects go; the live one must stay, or every subject is being
        // cleared the moment the table fills.
        let _ = limiter.check_at("trigger", start + Duration::from_secs(90));

        assert_eq!(
            limiter.tracked(),
            2,
            "the sweep should have kept only the two live subjects"
        );
        Ok(())
    }
}
