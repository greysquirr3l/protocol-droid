//! The in-memory [`IdempotencyStore`].
//!
//! # Where the atomicity comes from, and why it is here
//!
//! A `Mutex` around [`CompletedResults`], taken once per operation and released
//! before the caller executes. That single acquisition is the whole of the
//! guarantee, and it has to live in an adapter because it is a claim about
//! concurrency that the state machine cannot make: `CompletedResults::claim`
//! takes `&mut self`, which is exclusive for the duration of one call and
//! nothing more, and an HTTP server has many calls in flight at once.
//!
//! This is the same division the registry's compare-and-swap publish makes. The
//! transitions are domain knowledge and sit in `domain/idempotency.rs`; the
//! exclusion is infrastructure and sits here. Neither is asked to do the other's
//! job, and a second store implementation would reuse the same table rather than
//! re-derive it — so the two cannot disagree about when a claim is stale.
//!
//! # Why the lock is not held across the execution
//!
//! Because holding it would serialise every request in the process behind the
//! slowest tool. The claim is a microsecond-scale map operation; the execution
//! is the whole cost of the request. Taking the lock, deciding, releasing it,
//! then executing is what makes the store correct *and* concurrent: a slow
//! execution blocks retries of its own key — which must wait, because the first
//! attempt is still in flight — and nothing else.
//!
//! # Why a poisoned lock refuses rather than recovers
//!
//! Because the state behind it is no longer something this process can vouch
//! for: a panic elsewhere unwound through the critical section, and the map may
//! be missing the insert it was midway through. Answering from it could run a
//! side effect twice, or hand a caller a result nobody executed for. Refusing
//! is the fail-secure direction, and a caller that sees
//! [`IdempotencyError::Unavailable`] retries rather than guessing.

use core::num::NonZeroUsize;
use std::sync::Mutex;

use crate::application::idempotency::{IdempotencyStore, default_capacity};
use crate::domain::idempotency::{
    Claim, ClaimToken, CompletedResults, IdempotencyError, IdempotencyKey,
};

/// A process-local store of completed results, one `Mutex` wide.
///
/// # Example
///
/// ```
/// use std::num::NonZeroUsize;
///
/// use protocol_droid::adapters::secondary::idempotency::InMemoryIdempotencyStore;
/// use protocol_droid::application::idempotency::IdempotencyStore;
/// use protocol_droid::domain::idempotency::{Claim, IdempotencyKey};
///
/// let store = InMemoryIdempotencyStore::with_capacity(NonZeroUsize::MIN);
/// let key = IdempotencyKey::new("req-1");
///
/// let first = store.claim(&key)?;
/// assert!(first.is_execution_mine());
/// let token = first.token().ok_or("claimed without a token")?;
/// store.complete(token, "charged once")?;
///
/// // The client retries because the connection dropped. It gets the recorded
/// // result and no second charge.
/// let replay = store.claim(&key)?;
/// // `matches!` on a `&replay` binds `result` as `&&str`, so the recorded
/// // value is two derefs in.
/// assert!(matches!(&replay, Claim::Completed(result) if **result == *"charged once"));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct InMemoryIdempotencyStore<T> {
    results: Mutex<CompletedResults<T>>,
}

impl<T> InMemoryIdempotencyStore<T> {
    /// A store holding at most `capacity` keys.
    ///
    /// Bounded because the keys are peer-chosen; see
    /// [`CompletedResults::with_capacity`].
    #[must_use]
    pub fn with_capacity(capacity: NonZeroUsize) -> Self {
        Self {
            results: Mutex::new(CompletedResults::with_capacity(capacity)),
        }
    }

    /// The ceiling, in keys.
    ///
    /// Takes the lock, so a poisoned store reports the error rather than a
    /// made-up number.
    ///
    /// # Errors
    ///
    /// [`IdempotencyError::Unavailable`] when the lock is poisoned.
    pub fn capacity(&self) -> Result<usize, IdempotencyError> {
        Ok(self.locked()?.capacity())
    }

    /// How many keys are held, including claims that are still in flight.
    ///
    /// # Errors
    ///
    /// [`IdempotencyError::Unavailable`] when the lock is poisoned.
    pub fn len(&self) -> Result<usize, IdempotencyError> {
        Ok(self.locked()?.len())
    }

    /// Whether nothing is held.
    ///
    /// # Errors
    ///
    /// [`IdempotencyError::Unavailable`] when the lock is poisoned.
    pub fn is_empty(&self) -> Result<bool, IdempotencyError> {
        Ok(self.locked()?.is_empty())
    }

    /// One lock acquisition, shared by every operation.
    fn locked(&self) -> Result<std::sync::MutexGuard<'_, CompletedResults<T>>, IdempotencyError> {
        self.results
            .lock()
            .map_err(|_| IdempotencyError::Unavailable {
                reason: "the idempotency store's lock is poisoned",
            })
    }
}

impl<T: Clone + Send + Sync> IdempotencyStore for InMemoryIdempotencyStore<T> {
    type Result = T;

    fn claim(&self, key: &IdempotencyKey) -> Result<Claim<T>, IdempotencyError> {
        self.locked()?.claim(key)
    }

    fn complete(&self, token: &ClaimToken, result: T) -> Result<(), IdempotencyError> {
        self.locked()?.complete(token, result)?;
        Ok(())
    }

    fn abandon(&self, token: &ClaimToken) -> Result<(), IdempotencyError> {
        self.locked()?.abandon(token)?;
        Ok(())
    }
}

impl<T> Default for InMemoryIdempotencyStore<T> {
    /// A store of [`default_capacity`] keys.
    fn default() -> Self {
        Self::with_capacity(default_capacity())
    }
}

#[cfg(test)]
mod tests {
    use super::InMemoryIdempotencyStore;
    use crate::application::idempotency::IdempotencyStore;
    use crate::domain::idempotency::{Claim, IdempotencyError, IdempotencyKey};
    use core::num::NonZeroUsize;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;

    fn store(capacity: usize) -> Result<InMemoryIdempotencyStore<String>, Failure> {
        let capacity = NonZeroUsize::new(capacity).ok_or(Failure::ZeroCapacity)?;
        Ok(InMemoryIdempotencyStore::with_capacity(capacity))
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Failure {
        ZeroCapacity,
        Unexpected(String),
    }

    impl From<IdempotencyError> for Failure {
        fn from(error: IdempotencyError) -> Self {
            Self::Unexpected(error.to_string())
        }
    }

    /// **Counting executions, through the real adapter.** The threads are
    /// released from a [`Barrier`] so they arrive at the claim together rather
    /// than in whatever order the scheduler picks — a race that only sometimes
    /// happens is not a race test. The counter is the claim: exactly one
    /// execution, every other caller either saw it in flight or replayed the
    /// recorded result.
    #[test]
    fn one_execution_survives_a_synchronised_race() -> Result<(), Failure> {
        const THREADS: usize = 32;

        let store: Arc<dyn IdempotencyStore<Result = String>> = Arc::new(store(4)?);
        let executions = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(THREADS));
        let replays = Arc::new(AtomicUsize::new(0));
        let inflight = Arc::new(AtomicUsize::new(0));

        let mut workers = Vec::with_capacity(THREADS);
        for _ in 0..THREADS {
            let store = Arc::clone(&store);
            let executions = Arc::clone(&executions);
            let replays = Arc::clone(&replays);
            let inflight = Arc::clone(&inflight);
            let barrier = Arc::clone(&barrier);
            workers.push(thread::spawn(move || -> Result<(), Failure> {
                let key = IdempotencyKey::new("req-1");
                barrier.wait();
                match store.claim(&key)? {
                    Claim::Claimed { token } => {
                        executions.fetch_add(1, Ordering::SeqCst);
                        // The work happens here, with no lock held.
                        store.complete(&token, "the one run".to_owned())?;
                    }
                    Claim::Completed(_) => {
                        replays.fetch_add(1, Ordering::SeqCst);
                    }
                    Claim::InFlight => {
                        inflight.fetch_add(1, Ordering::SeqCst);
                    }
                }
                Ok(())
            }));
        }
        for worker in workers {
            worker
                .join()
                .map_err(|_| Failure::Unexpected("a worker panicked".to_owned()))??;
        }

        let ran = executions.load(Ordering::SeqCst);
        if ran != 1 {
            return Err(Failure::Unexpected(format!("{ran} executions")));
        }
        // Every non-owner accounted for: one of the two benign outcomes.
        let benign = replays.load(Ordering::SeqCst) + inflight.load(Ordering::SeqCst);
        if benign != THREADS - 1 {
            return Err(Failure::Unexpected(format!(
                "{benign} of {} losers accounted for",
                THREADS - 1
            )));
        }
        Ok(())
    }

    /// A stale completion through the adapter is refused with the same
    /// [`StaleClaim`] the domain machine produces — the adapter adds the lock
    /// and nothing else, so it cannot report a different fault for the same
    /// transition.
    #[test]
    fn a_stale_completion_is_refused_with_the_domain_error() -> Result<(), Failure> {
        let store = store(4)?;
        let key = IdempotencyKey::new("req-1");

        let slow = store.claim(&key)?;
        let slow_token = slow
            .token()
            .cloned()
            .ok_or_else(|| Failure::Unexpected("no token".to_owned()))?;
        store.abandon(&slow_token)?;

        let retry = store.claim(&key)?;
        let retry_token = retry
            .token()
            .cloned()
            .ok_or_else(|| Failure::Unexpected("no token".to_owned()))?;
        store.complete(&retry_token, "second".to_owned())?;

        match store.complete(&slow_token, "first".to_owned()) {
            Err(IdempotencyError::Stale(_)) => Ok(()),
            other => Err(Failure::Unexpected(format!("{other:?}"))),
        }
    }

    /// The ceiling is visible through the adapter, and a full store of running
    /// work refuses a new key rather than discarding a live claim.
    #[test]
    fn the_ceiling_is_observable_and_fail_secure() -> Result<(), Failure> {
        let store = store(1)?;
        if store.capacity()? != 1 {
            return Err(Failure::Unexpected("capacity".to_owned()));
        }
        // Held and never completed: the one thing that is not evicted.
        let key = IdempotencyKey::new("req-1");
        let _running = store.claim(&key)?;

        match store.claim(&IdempotencyKey::new("req-2")) {
            Err(IdempotencyError::StoreFull { capacity }) => {
                if capacity != 1 {
                    return Err(Failure::Unexpected("misreported ceiling".to_owned()));
                }
            }
            other => return Err(Failure::Unexpected(format!("{other:?}"))),
        }
        if store.len()? != 1 {
            return Err(Failure::Unexpected("lost a key".to_owned()));
        }
        if store.is_empty()? {
            return Err(Failure::Unexpected("reported empty".to_owned()));
        }
        Ok(())
    }

    /// Two adapters sharing nothing do not share results — the obvious
    /// consequence of a process-local store, checked so a future change that
    /// made this global would be caught.
    #[test]
    fn the_store_is_process_local() -> Result<(), Failure> {
        let first = store(4)?;
        let second = store(4)?;
        let key = IdempotencyKey::new("req-1");

        let claim = first.claim(&key)?;
        first.complete(
            claim
                .token()
                .ok_or_else(|| Failure::Unexpected("no token".to_owned()))?,
            "only here".to_owned(),
        )?;

        if !second.claim(&key)?.is_execution_mine() {
            return Err(Failure::Unexpected(
                "results leaked between stores".to_owned(),
            ));
        }
        Ok(())
    }
}
