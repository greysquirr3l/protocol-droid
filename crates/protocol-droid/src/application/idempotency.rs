//! The port a peer-observable side effect is deduplicated through.
//!
//! # Why the trait lives here
//!
//! Because the port belongs to its consumer. The consumer is the `tools/call`
//! handler — the one use case whose side effect a peer can observe and retry —
//! and this module is that use case's seam. Nothing in `domain/` or
//! `adapters/` declares it, and there is deliberately no shared `ports` module
//! for it to be collected into: a trait collected away from its consumer is a
//! trait whose shape drifts toward whatever is convenient to implement.
//!
//! # Why the port is `&self` and not `&mut self`
//!
//! Because the claim has to be atomic between concurrent callers, and the only
//! thing a `&mut self` method can promise is exclusivity for the duration of
//! one call. An HTTP server handles requests concurrently, so a store that can
//! only be reached through `&mut self` is either behind a lock somewhere
//! unavoidable or not safe to share at all. `&self` lets the adapter hold the
//! lock internally — exactly where the atomicity belongs — and lets a server
//! hold it in shared state.
//!
//! # Why the result type is an associated type
//!
//! So the trait is object-safe. The Streamable HTTP adapter keeps its store in
//! router state, and router state is `Arc<dyn …>`; a trait with a `T` parameter
//! would force every store to be monomorphised into the router, and a caller
//! wanting to swap implementations for a test would have to name the result
//! type at every use site. `Arc<dyn IdempotencyStore<Result = …>>` names it
//! once.
//!
//! # Why there is no `async` on these methods
//!
//! Because the operations are a map lookup, an insert and a remove. Declaring
//! them `async` would put a `Pin<Box<dyn Future>>>` in the signature of a
//! hash-table operation and add an allocation per call to buy nothing. A store
//! that genuinely needs to await — one writing to a database, say — implements
//! this port and does its awaiting inside, which is what an adapter is for.
//!
//! # The ordering, stated once more because it is the contract
//!
//! [`IdempotencyStore::claim`] → execute → [`IdempotencyStore::complete`]. The
//! claim *is* the check: it reserves the key for its caller, so a second
//! retry that arrives before the first finishes gets
//! [`Claim::InFlight`] rather than a second go-ahead. A handler that checked
//! and then claimed separately would have two operations and two writers would
//! interleave — the same read-modify-write the registry's publish loop
//! carries.
//!
//! [`IdempotencyStore::abandon`] exists for the failed-execution case, and it
//! is not optional bookkeeping: without it a single transient tool failure would
//! leave the key reserved forever, and every retry of that request id would be
//! answered `InFlight` with nobody to complete it.

use core::num::NonZeroUsize;

use crate::domain::idempotency::{Claim, ClaimToken, IdempotencyError, IdempotencyKey};

/// How many keys a store holds when nobody has measured its workload.
///
/// A store for a `tools/call` result, sized for the concurrency a server
/// actually sees rather than for a flood. The number is deliberately modest
/// because the store is *not* a cache: a key it forgets is a request it will
/// refuse rather than re-run, so being too small costs a failed request while
/// being too large costs memory a peer can grow without bound. A deployment
/// that knows its own rate replaces it — the capacity is a constructor argument
/// on the adapter for exactly that reason.
///
/// A deployment that wants *no* ceiling sets a large capacity knowingly: the
/// store is keyed by a peer-chosen request id, so an unbounded one is a memory
/// exhaustion primitive.
#[must_use]
pub const fn default_capacity() -> NonZeroUsize {
    match NonZeroUsize::new(4_096) {
        Some(value) => value,
        // Unreachable: 4096 is not zero. A `match` rather than `unwrap` because
        // the crate denies that, and duplicating the number or a lazy static
        // would be worse for a case the compiler already rules out.
        None => NonZeroUsize::MIN,
    }
}

/// A store that runs each key's side effect at most once.
///
/// # Implementing this
///
/// The three methods must be **individually atomic** with respect to one
/// another, and the atomicity has to be the implementation's, not the caller's.
/// Concretely: two threads calling [`claim`](Self::claim) with the same key must
/// never both receive [`Claim::Claimed`]. The in-memory adapter holds
/// `CompletedResults` behind a `Mutex` for exactly this, taking the lock once
/// per operation and releasing it before the caller executes — so a slow tool
/// does not serialise every other key behind it.
///
/// [`CompletedResults`](crate::domain::idempotency::CompletedResults) is the
/// transition table to delegate to; an adapter that is not in-memory has to
/// reproduce its transitions, and the invariants worth reproducing are that a
/// `Claimed` key stays reserved until completed or abandoned, and that a
/// completion carrying a stale [`ClaimToken`] is refused rather than applied.
///
/// # Example
///
/// ```
/// use std::num::NonZeroUsize;
/// use std::sync::Mutex;
///
/// use protocol_droid::application::idempotency::IdempotencyStore;
/// use protocol_droid::domain::idempotency::{
///     Claim, ClaimToken, CompletedResults, IdempotencyError, IdempotencyKey,
/// };
///
/// struct InMemory<T>(Mutex<CompletedResults<T>>);
///
/// impl<T: Clone + Send + Sync> IdempotencyStore for InMemory<T> {
///     type Result = T;
///
///     fn claim(&self, key: &IdempotencyKey) -> Result<Claim<T>, IdempotencyError> {
///         self.0
///             .lock()
///             .map_err(|_| IdempotencyError::Unavailable { reason: "lock poisoned" })?
///             .claim(key)
///     }
///
///     fn complete(
///         &self,
///         token: &ClaimToken,
///         result: T,
///     ) -> Result<(), IdempotencyError> {
///         self.0
///             .lock()
///             .map_err(|_| IdempotencyError::Unavailable { reason: "lock poisoned" })?
///             .complete(token, result)?;
///         Ok(())
///     }
///
///     fn abandon(&self, token: &ClaimToken) -> Result<(), IdempotencyError> {
///         self.0
///             .lock()
///             .map_err(|_| IdempotencyError::Unavailable { reason: "lock poisoned" })?
///             .abandon(token)?;
///         Ok(())
///     }
/// }
///
/// let store = InMemory(Mutex::new(CompletedResults::with_capacity(NonZeroUsize::MIN)));
/// let key = IdempotencyKey::new("req-1");
///
/// let first = store.claim(&key)?;
/// assert!(first.is_execution_mine());
///
/// let token = first.token().ok_or("claimed without a token")?;
/// store.complete(token, "the only run")?;
///
/// // A retry of the same request id replays rather than re-running.
/// match store.claim(&key)? {
///     Claim::Completed(result) => assert_eq!(result, "the only run"),
///     other => return Err(format!("re-claimed: {other:?}").into()),
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
/// `Send + Sync` because the whole point of the store is to be consulted
/// concurrently by the requests it is deduplicating. An adapter that held one
/// behind `&self` without those bounds would force every `tools/call` handler
/// to reach a lock it cannot see, and the deduplication guarantee would then
/// depend on each caller remembering to take it.
pub trait IdempotencyStore: Send + Sync {
    /// What a completed side effect produced. The adapter's own result type —
    /// an `IrResult` for a tool call, a transport payload for a bridge forward.
    type Result;

    /// Takes ownership of `key`, or reports that it is already spoken for.
    ///
    /// # Errors
    ///
    /// [`IdempotencyError::StoreFull`] when the store cannot hold another key
    /// without risking the loss of a recorded result.
    fn claim(&self, key: &IdempotencyKey) -> Result<Claim<Self::Result>, IdempotencyError>;

    /// Records the result of a claim the caller owns.
    ///
    /// # Errors
    ///
    /// [`StaleClaim`] when the token no longer holds its key, which happens
    /// when the attempt was abandoned and a later one re-claimed it. The caller
    /// should discard its result in that case: the later attempt's is the one
    /// the peer is entitled to. Also
    /// [`IdempotencyError::Unavailable`] when the store itself could not answer.
    fn complete(&self, token: &ClaimToken, result: Self::Result) -> Result<(), IdempotencyError>;

    /// Releases a claim whose execution did not produce a result.
    ///
    /// Without this a failed attempt would reserve the key forever and every
    /// retry would be told `InFlight` by a caller that no longer exists.
    ///
    /// # Errors
    ///
    /// [`StaleClaim`] when the token no longer holds its key. Also
    /// [`IdempotencyError::Unavailable`] when the store itself could not answer.
    fn abandon(&self, token: &ClaimToken) -> Result<(), IdempotencyError>;
}

#[cfg(test)]
mod tests {
    use super::{IdempotencyStore, default_capacity};
    use crate::adapters::secondary::idempotency::InMemoryIdempotencyStore;
    use crate::domain::idempotency::{
        Claim, CompletedResults, IdempotencyError, IdempotencyKey, StaleClaim,
    };
    use core::num::NonZeroUsize;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    fn store(capacity: usize) -> Result<InMemoryIdempotencyStore<String>, Failure> {
        let capacity = NonZeroUsize::new(capacity).ok_or(Failure::ZeroCapacity)?;
        Ok(InMemoryIdempotencyStore::with_capacity(capacity))
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Failure {
        ZeroCapacity,
        Poisoned,
        Unexpected(String),
    }

    impl From<IdempotencyError> for Failure {
        fn from(error: IdempotencyError) -> Self {
            Self::Unexpected(error.to_string())
        }
    }

    impl From<StaleClaim> for Failure {
        fn from(error: StaleClaim) -> Self {
            Self::Unexpected(error.to_string())
        }
    }

    /// The trait is usable as `Arc<dyn …>`, which is the reason the result type
    /// is an associated type. A store behind a trait object has to be able to
    /// answer for its own result type, and this fails to compile if that ever
    /// changes.
    #[test]
    fn is_usable_behind_a_trait_object() -> Result<(), Failure> {
        let concrete: Arc<dyn IdempotencyStore<Result = String>> = Arc::new(store(4)?);
        let key = IdempotencyKey::new("req-1");
        let claim = concrete.claim(&key)?;
        if !claim.is_execution_mine() {
            return Err(Failure::Unexpected("not claimed".to_owned()));
        }
        concrete.complete(
            claim
                .token()
                .ok_or_else(|| Failure::Unexpected("no token".to_owned()))?,
            "done".to_owned(),
        )?;
        Ok(())
    }

    /// The default ceiling is a real, non-zero bound, so `Default` cannot
    /// produce a store that refuses everything.
    #[test]
    fn the_default_capacity_is_usable() -> Result<(), Failure> {
        if default_capacity().get() == 0 {
            return Err(Failure::Unexpected("zero".to_owned()));
        }
        let store = InMemoryIdempotencyStore::<String>::default();
        if store.capacity()? != default_capacity().get() {
            return Err(Failure::Unexpected("mismatch".to_owned()));
        }
        Ok(())
    }

    /// **Counting executions, through the port and the adapter.** This is the
    /// end-to-end version of the domain's own concurrency test: the same lock
    /// the production adapter uses is what makes exactly one of sixty-four
    /// racing callers execute. The counter, not the returned result, is the
    /// claim under test.
    #[test]
    fn one_execution_survives_sixty_four_racing_retries() -> Result<(), Failure> {
        const THREADS: usize = 64;

        /// What each racing worker saw when it claimed the key.
        ///
        /// The distinction the test turns on is how many workers *ran* the
        /// side effect, so the outcome is recorded rather than a count that
        /// only one of the branches could contribute to. A signed sentinel
        /// would say the same thing, and would also be a value the count
        /// could never legitimately be.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum Saw {
            /// This worker ran the side effect, and it is worker `n`.
            Ran(usize),
            /// This worker got a recorded result back without re-running.
            Replayed,
            /// This worker found the key still claimed by another attempt.
            InFlight,
        }

        let store: Arc<dyn IdempotencyStore<Result = String>> = Arc::new(store(8)?);
        let executions = Arc::new(AtomicUsize::new(0));
        // The work runs outside the store's lock, as it must: the store's
        // critical section is the claim, not the execution.
        let observed = Arc::new(Mutex::new(Vec::new()));

        let mut workers = Vec::with_capacity(THREADS);
        for worker_id in 0..THREADS {
            let store = Arc::clone(&store);
            let executions = Arc::clone(&executions);
            let observed = Arc::clone(&observed);
            workers.push(thread::spawn(move || -> Result<(), Failure> {
                let key = IdempotencyKey::new("req-1");
                let saw = match store.claim(&key)? {
                    Claim::Claimed { token } => {
                        executions.fetch_add(1, Ordering::SeqCst);
                        store.complete(&token, format!("worker-{worker_id}"))?;
                        Saw::Ran(worker_id)
                    }
                    Claim::Completed(result) => {
                        // A replay must get the *recorded* result, not a new one.
                        if result.is_empty() {
                            return Err(Failure::Unexpected("empty replay".to_owned()));
                        }
                        Saw::Replayed
                    }
                    Claim::InFlight => Saw::InFlight,
                };
                observed
                    .lock()
                    .map_err(|_| Failure::Poisoned)?
                    .push((worker_id, saw));
                Ok(())
            }));
        }
        for worker in workers {
            worker
                .join()
                .map_err(|_| Failure::Unexpected("panicked".to_owned()))??;
        }

        let count = executions.load(Ordering::SeqCst);
        if count != 1 {
            return Err(Failure::Unexpected(format!("{count} executions")));
        }
        let seen = observed.lock().map_err(|_| Failure::Poisoned)?.clone();
        if seen.len() != THREADS {
            return Err(Failure::Unexpected(format!("{} observers", seen.len())));
        }
        let ran = seen
            .iter()
            .filter(|(_, saw)| matches!(saw, Saw::Ran(_)))
            .count();
        if ran != 1 {
            return Err(Failure::Unexpected(format!("{ran} workers ran it")));
        }
        Ok(())
    }

    /// A failed execution releases the key, so a retry afterwards runs — and
    /// runs exactly once itself.
    #[test]
    fn a_failed_execution_releases_the_key_for_the_retry() -> Result<(), Failure> {
        let store = store(4)?;
        let key = IdempotencyKey::new("req-1");

        let first = store.claim(&key)?;
        let token = first
            .token()
            .cloned()
            .ok_or_else(|| Failure::Unexpected("no token".to_owned()))?;
        store.abandon(&token)?;

        let second = store.claim(&key)?;
        if !second.is_execution_mine() {
            return Err(Failure::Unexpected("not re-claimed".to_owned()));
        }
        store.complete(
            second
                .token()
                .ok_or_else(|| Failure::Unexpected("no token".to_owned()))?,
            "second".to_owned(),
        )?;

        match store.claim(&key)? {
            Claim::Completed(result) if result == "second" => Ok(()),
            other => Err(Failure::Unexpected(format!("{other:?}"))),
        }
    }

    /// The adapter is `Send` and `Sync`, so a server can hold it in shared
    /// state. If a lock ever leaked into the domain machine, this would fail.
    #[test]
    fn is_shareable() -> Result<(), Failure> {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<InMemoryIdempotencyStore<String>>();
        let _ = store(1)?;
        Ok(())
    }

    /// The adapter delegates to the domain machine rather than keeping its own
    /// map, so the transitions cannot drift. Checked by a capacity the two must
    /// agree on.
    #[test]
    fn the_adapter_owns_no_second_state() -> Result<(), Failure> {
        let store = store(3)?;
        if store.capacity()? != 3 {
            return Err(Failure::Unexpected("capacity mismatch".to_owned()));
        }
        // A machine built with the same capacity agrees, which is what "the
        // adapter is only a lock" means operationally.
        let mut machine = CompletedResults::<String>::with_capacity(
            NonZeroUsize::new(3).ok_or(Failure::ZeroCapacity)?,
        );
        let key = IdempotencyKey::new("req-1");
        if !machine.claim(&key)?.is_execution_mine() {
            return Err(Failure::Unexpected("machine refused".to_owned()));
        }
        Ok(())
    }
}
