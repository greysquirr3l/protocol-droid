//! Evicting sessions nobody is driving any more.
//!
//! A session is a map entry, and for a bridge it is also a task and an
//! upstream connection. Nothing but a client `DELETE` removes one, so a client
//! that opens sessions and walks away costs the server a little more each
//! time, with no limit. [`SessionStore::reap_idle`] is the eviction; this
//! module is the clock that calls it.
//!
//! ## Why the handle aborts on drop
//!
//! The store's own documentation declines to start a task inside a
//! constructor, because a deployment cannot stop one. The task here is started
//! by the serve call, held by it, and aborted when it returns, so it ends with
//! the server it serves and never outlives it.
//!
//! ## What a reaped session means to its client
//!
//! The next request carries an id the server no longer has and gets `404`,
//! which the specification tells a client to answer by starting a new session.
//! A client that holds only a standing `GET` stream and sends nothing is idle
//! by this definition too; it is evicted like any other and recovers the same
//! way.

use std::sync::Arc;
use std::time::Duration;

use super::session_store::{SessionStore, SessionStoreError};

/// The lower bound on how often the reaper wakes.
const MIN_INTERVAL: Duration = Duration::from_secs(1);

/// The idle window a server uses unless told otherwise.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_mins(30);

/// The one operation a reaper needs from a session store.
///
/// Declared here, by its consumer, so a router with one store per transport
/// can hand them all to a single reaper without the reaper naming each
/// store's session type.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use protocol_droid::adapters::primary::transport::http::reaper::IdleSessions;
/// use protocol_droid::adapters::primary::transport::http::SessionStoreError;
///
/// struct Nothing;
///
/// impl IdleSessions for Nothing {
///     fn reap_idle(&self, _max_age: Duration) -> Result<usize, SessionStoreError> {
///         Ok(0)
///     }
/// }
/// ```
pub trait IdleSessions: Send + Sync {
    /// Evicts every session untouched for longer than `max_age`, and returns
    /// how many went.
    ///
    /// # Errors
    ///
    /// [`SessionStoreError::Unavailable`] when the store's lock is poisoned.
    /// The reaper stops on it: a poisoned store refuses every lookup too, so
    /// there is nothing left to reap.
    fn reap_idle(&self, max_age: Duration) -> Result<usize, SessionStoreError>;
}

impl<S: Send + Sync> IdleSessions for SessionStore<S> {
    /// As [`SessionStore::reap_idle`], counting rather than naming the ids:
    /// the reaper has no use for them and an id is not a thing to pass around.
    fn reap_idle(&self, max_age: Duration) -> Result<usize, SessionStoreError> {
        Self::reap_idle(self, max_age).map(|reaped| reaped.len())
    }
}

/// An idle window of zero, which would evict every session on the first sweep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("an idle timeout of zero would evict every session")]
pub struct ZeroIdleTimeout;

/// How long a session may sit untouched, and how often to look.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use protocol_droid::adapters::primary::transport::http::reaper::IdleReaper;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let reaper = IdleReaper::new(Duration::from_secs(600))?;
/// assert_eq!(reaper.max_age(), Duration::from_secs(600));
/// assert!(IdleReaper::new(Duration::ZERO).is_err());
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdleReaper {
    max_age: Duration,
    interval: Duration,
}

impl IdleReaper {
    /// [`DEFAULT_IDLE_TIMEOUT`], swept four times per window.
    pub const DEFAULT: Self = Self {
        max_age: DEFAULT_IDLE_TIMEOUT,
        interval: Duration::from_secs(450),
    };

    /// A reaper that evicts sessions idle for longer than `max_age`, sweeping
    /// four times per window and no more than once a second.
    ///
    /// # Errors
    ///
    /// [`ZeroIdleTimeout`] when `max_age` is zero.
    pub fn new(max_age: Duration) -> Result<Self, ZeroIdleTimeout> {
        if max_age.is_zero() {
            return Err(ZeroIdleTimeout);
        }
        Ok(Self {
            max_age,
            interval: (max_age / 4).max(MIN_INTERVAL),
        })
    }

    /// Replaces how often the reaper sweeps.
    ///
    /// Mostly for tests, which cannot wait a second per sweep. A sweep
    /// interval longer than the window just means sessions live up to one
    /// interval past it.
    #[must_use]
    pub const fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    /// The idle window.
    #[must_use]
    pub const fn max_age(&self) -> Duration {
        self.max_age
    }

    /// Starts sweeping `stores` on the current Tokio runtime.
    ///
    /// The returned handle owns the task; dropping it stops the sweeping.
    ///
    /// # Panics
    ///
    /// Panics outside a Tokio runtime, as `tokio::spawn` does.
    #[must_use = "dropping the handle stops the reaper"]
    pub fn start(self, stores: Vec<Arc<dyn IdleSessions>>) -> ReaperHandle {
        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(self.interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // The first tick of an interval completes at once; sessions made
            // a moment ago are not idle, so it is spent rather than swept on.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                for store in &stores {
                    if store.reap_idle(self.max_age).is_err() {
                        return;
                    }
                }
            }
        });
        ReaperHandle { task }
    }
}

/// A running reaper. Dropping it stops the task.
#[derive(Debug)]
pub struct ReaperHandle {
    task: tokio::task::JoinHandle<()>,
}

impl ReaperHandle {
    /// Whether the task has ended, which outside of a drop means a store
    /// reported its lock poisoned.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }
}

impl Drop for ReaperHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::{IdleReaper, IdleSessions, ZeroIdleTimeout};
    use crate::adapters::primary::transport::http::SessionStore;
    use crate::adapters::secondary::clock::MonotonicClock;
    use crate::application::session::IdSource;
    use crate::domain::session::SessionId;

    struct Counting(std::sync::atomic::AtomicU64);

    impl IdSource for Counting {
        fn next_session_id(&self) -> SessionId {
            SessionId::new(self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
        }
    }

    fn store() -> Arc<SessionStore<()>> {
        Arc::new(SessionStore::with_idle_reaper(
            Arc::new(Counting(std::sync::atomic::AtomicU64::new(0))),
            Arc::new(MonotonicClock::new()),
        ))
    }

    #[test]
    fn a_zero_window_is_refused_rather_than_evicting_everything() {
        assert_eq!(IdleReaper::new(Duration::ZERO), Err(ZeroIdleTimeout));
    }

    #[test]
    fn the_sweep_interval_is_a_quarter_window_and_never_under_a_second()
    -> Result<(), ZeroIdleTimeout> {
        let long = IdleReaper::new(Duration::from_mins(20))?;
        assert_eq!(long.interval, Duration::from_secs(300));
        let short = IdleReaper::new(Duration::from_millis(10))?;
        assert_eq!(short.interval, Duration::from_secs(1));
        Ok(())
    }

    #[test]
    fn the_default_is_what_new_would_build_for_the_default_window() -> Result<(), ZeroIdleTimeout> {
        assert_eq!(
            IdleReaper::DEFAULT,
            IdleReaper::new(super::DEFAULT_IDLE_TIMEOUT)?
        );
        Ok(())
    }

    #[tokio::test]
    async fn an_abandoned_session_is_evicted_and_a_touched_one_is_kept()
    -> Result<(), Box<dyn std::error::Error>> {
        let sessions = store();
        let abandoned = sessions.open(Arc::new(()))?;
        let kept = sessions.open(Arc::new(()))?;
        let reaper = IdleReaper::new(Duration::from_millis(150))?
            .with_interval(Duration::from_millis(20))
            .start(vec![Arc::clone(&sessions) as Arc<dyn IdleSessions>]);

        let deadline = Instant::now() + Duration::from_secs(5);
        // `len`, not `lookup`: a lookup is a touch, and would keep the
        // abandoned session alive by asking whether it is still there.
        while Instant::now() < deadline && sessions.len()? == 2 {
            // Touching `kept` is what a client still driving its session does.
            assert!(sessions.lookup(&kept)?.is_some());
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(sessions.lookup(&abandoned)?.is_none());
        assert!(sessions.lookup(&kept)?.is_some());
        assert!(!reaper.is_finished());
        Ok(())
    }

    #[tokio::test]
    async fn dropping_the_handle_stops_the_sweeping() -> Result<(), Box<dyn std::error::Error>> {
        let sessions = store();
        let id = sessions.open(Arc::new(()))?;
        let reaper = IdleReaper::new(Duration::from_millis(50))?
            .with_interval(Duration::from_millis(10))
            .start(vec![Arc::clone(&sessions) as Arc<dyn IdleSessions>]);
        drop(reaper);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(sessions.lookup(&id)?.is_some());
        Ok(())
    }
}
