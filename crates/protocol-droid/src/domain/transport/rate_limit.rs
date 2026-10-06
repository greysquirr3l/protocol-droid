//! How many requests one subject may make in a window, and what to say when it
//! has made too many.
//!
//! The *policy* lives here and the *bookkeeping* does not, and that split is
//! the whole design. Deciding whether a request is allowed is a function of
//! three values — the limit, the window, and what the counter observed — and
//! nothing else. It does not read a clock, it does not own the counters, and
//! it cannot be surprised by a slow request or a second task. The adapter owns
//! a map from subject to counter and supplies the elapsed time; this module
//! turns that into a verdict.
//!
//! ## Why a fixed window rather than a token bucket
//!
//! A token bucket is smoother, and it is what `governor` implements. It is
//! also continuous state that only makes sense if the reader already trusts the
//! arithmetic, which makes it hard to pin down and hard to audit. A fixed
//! window's worst case is well known and bounded: a caller may make `2 *
//! limit` requests across a window boundary in a span as short as the time it
//! takes to send them. For the abuse this defends against — one client
//! hammering a tool — that is still a bound, and it is a bound whose proof is
//! four lines long.
//!
//! If a deployment needs smoother behaviour, the right change is to swap the
//! algorithm behind [`check`] and keep its signature.
//!
//! ## OWASP
//!
//! API Security Top 10:2023, API4:2023 (Unrestricted Resource Consumption).

use core::num::{NonZeroU32, NonZeroU64};
use core::time::Duration;

/// A request allowance: `limit` requests per `window`.
///
/// `limit` is a [`NonZeroU32`] so that a misconfiguration which means "refuse
/// everything" is not expressible by accident. The type has no "unlimited"
/// state either: a caller that genuinely wants no limit should not put this
/// type in the path at all, so "unlimited" can never be the accidental result
/// of a zero that survived a round trip through configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateLimit {
    limit: NonZeroU32,
    window: Duration,
}

impl RateLimit {
    /// An allowance of `limit` requests per `window`.
    ///
    /// # Panics
    ///
    /// If `window` is zero. A window of zero is a server that refuses
    /// everything, and that is not a condition worth discovering from traffic,
    /// so it is rejected at construction. `limit` cannot be zero by type.
    #[must_use]
    pub const fn new(limit: NonZeroU32, window: Duration) -> Self {
        assert!(
            !window.is_zero(),
            "a rate-limit window of zero refuses everything"
        );
        Self { limit, window }
    }

    /// Requests permitted per window.
    #[must_use]
    pub const fn limit(&self) -> NonZeroU32 {
        self.limit
    }

    /// The width of the window.
    #[must_use]
    pub const fn window(&self) -> Duration {
        self.window
    }
}

/// What [`check`] concluded.
///
/// Not a [`bool`] because the caller needs the remainder to put a
/// `X-RateLimit-Remaining` on the response, and needs the wait on the refusal.
/// Collapsing to `bool` would mean either dropping both or recomputing them in
/// the adapter, which is the same arithmetic in a second place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The request is permitted. `remaining` counts what is left in this
    /// window *including this request*.
    Allowed {
        /// Requests still available in the current window.
        remaining: NonZeroU32,
    },
    /// The window is spent. `retry_after` is how long until it resets.
    Limited {
        /// Whole seconds to wait. Never zero.
        retry_after: NonZeroU64,
    },
}

/// The outcome of one check.
///
/// # Errors
///
/// Never. It is a function of its three arguments and always answers.
///
/// Not `const`: `Duration`'s `Sub` is not a const trait on stable, and this
/// function has to subtract one.
#[must_use]
pub fn check(limit: &RateLimit, seen: u32, elapsed: Duration) -> Verdict {
    // A window that has already elapsed is a fresh one. Handled here rather
    // than by the adapter resetting its counter, so the boundary is decided in
    // one place: the adapter's map is a cache of this answer, and a cache that
    // can disagree with the rule is worse than no cache.
    if elapsed >= limit.window {
        return Verdict::Allowed {
            remaining: limit.limit,
        };
    }

    // `seen` is a `u32` from outside this module. A value at or beyond the
    // limit is refused rather than subtracted from, so a corrupted counter
    // cannot wrap into a large allowance.
    if seen >= limit.limit.get() {
        // `checked_sub` because the arm above established `elapsed < window`
        // and this is the only place that fact is used. A `Duration` underflow
        // would panic, and the alternative — an unchecked `-` — makes a panic
        // reachable if the two comparisons ever drift apart.
        let left = limit
            .window
            .checked_sub(elapsed)
            .unwrap_or(Duration::from_secs(0));
        return Verdict::Limited {
            retry_after: seconds_ceil(left),
        };
    }

    // `limit - seen` is at least one because `seen < limit` is the arm we are
    // in, so the subtraction cannot wrap.
    Verdict::Allowed {
        remaining: NonZeroU32::new(limit.limit.get() - seen).unwrap_or(limit.limit),
    }
}

/// Whole seconds in `duration`, rounded up, and never zero.
///
/// Ceiling rather than truncating because a `Retry-After` of `0` tells the
/// client to retry now, which is the one answer that defeats the backoff. A
/// duration of 300ms becomes 1 second for the same reason.
fn seconds_ceil(duration: Duration) -> NonZeroU64 {
    if duration.is_zero() {
        return NonZeroU64::MIN;
    }
    // `as_millis` is `u128`, and a duration measured in billions of years is
    // not a backoff anyone will wait out. Saturating is the honest reading of
    // "effectively never retry", and it keeps the arithmetic in `u64`.
    let millis = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
    let seconds = millis / 1000;
    let rounded = if millis.is_multiple_of(1000) {
        seconds
    } else {
        seconds + 1
    };
    NonZeroU64::new(rounded).unwrap_or(NonZeroU64::MIN)
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;
    use core::num::NonZeroU32;
    use core::time::Duration;

    use super::{RateLimit, Verdict, check, seconds_ceil};

    type TestError = Box<dyn std::error::Error>;

    /// `NonZeroU32::new` returns an `Option`, and a fixture that cannot build
    /// a rate limit has failed the test rather than the program.
    fn nz(value: u32) -> Result<NonZeroU32, TestError> {
        NonZeroU32::new(value).ok_or_else(|| "a fixture asked for a zero limit".into())
    }

    fn limit() -> Result<RateLimit, TestError> {
        Ok(RateLimit::new(nz(3)?, Duration::from_secs(60)))
    }

    /// The ordinary case: under the limit, allowed, with a remainder that
    /// counts down.
    #[test]
    fn requests_under_the_limit_are_allowed_and_count_down() -> Result<(), TestError> {
        let limit = limit()?;

        assert_eq!(
            check(&limit, 0, Duration::from_secs(1)),
            Verdict::Allowed { remaining: nz(3)? }
        );
        assert_eq!(
            check(&limit, 2, Duration::from_secs(1)),
            Verdict::Allowed { remaining: nz(1)? }
        );
        Ok(())
    }

    /// The window boundary is the interesting case. `seen` at the limit is
    /// refused *within* the window and allowed *after* it, and that decision
    /// lives in one place so an adapter's cached counter cannot disagree.
    #[test]
    fn the_window_boundary_decides_both_ways() -> Result<(), TestError> {
        let limit = limit()?;

        assert_matches!(
            check(&limit, 3, Duration::from_secs(59)),
            Verdict::Limited { .. }
        );
        assert_eq!(
            check(&limit, 3, Duration::from_secs(60)),
            Verdict::Allowed { remaining: nz(3)? }
        );
        Ok(())
    }

    /// A `seen` from a corrupted or hand-written counter must not wrap the
    /// subtraction into a large allowance.
    #[test]
    fn a_count_beyond_the_limit_is_refused_not_wrapped() -> Result<(), TestError> {
        let limit = limit()?;

        assert_matches!(
            check(&limit, u32::MAX, Duration::from_secs(1)),
            Verdict::Limited { .. }
        );
        Ok(())
    }

    /// The refusal has to tell the client when to come back, and "now" is not a
    /// useful answer.
    #[test]
    fn a_refusal_always_asks_the_caller_to_wait() -> Result<(), TestError> {
        let limit = limit()?;

        for elapsed in [0, 1, 30, 59] {
            let Verdict::Limited { retry_after } = check(&limit, 3, Duration::from_secs(elapsed))
            else {
                return Err("a spent window must refuse".into());
            };
            assert!(
                retry_after.get() > 0,
                "Retry-After: 0 tells the client to retry immediately"
            );
        }
        Ok(())
    }

    /// A limit of one is the tightest legal configuration and must still work:
    /// first request allowed, second refused.
    #[test]
    fn a_limit_of_one_permits_exactly_one() -> Result<(), TestError> {
        let limit = RateLimit::new(nz(1)?, Duration::from_secs(10));

        assert_eq!(
            check(&limit, 0, Duration::from_secs(1)),
            Verdict::Allowed { remaining: nz(1)? }
        );
        assert_matches!(
            check(&limit, 1, Duration::from_secs(1)),
            Verdict::Limited { .. }
        );
        Ok(())
    }

    #[test]
    fn seconds_round_up_and_never_reach_zero() {
        assert_eq!(seconds_ceil(Duration::from_millis(999)).get(), 1);
        assert_eq!(seconds_ceil(Duration::from_millis(1000)).get(), 1);
        assert_eq!(seconds_ceil(Duration::from_millis(1001)).get(), 2);
        assert_eq!(seconds_ceil(Duration::from_secs(60)).get(), 60);
        assert_eq!(seconds_ceil(Duration::ZERO).get(), 1);
    }
}
