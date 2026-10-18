//! Running a peer-observable side effect at most once per key.
//!
//! # The problem this solves
//!
//! On the Streamable HTTP transport a client may re-send a request whose
//! response never reached it — the connection dropped between the server
//! running the tool and the client reading the answer. The client cannot tell
//! that apart from the server never having run it, so it retries, and the tool
//! runs twice. For a `tools/call` that means paying for the same thing twice,
//! sending the same message twice, or writing the same row twice.
//!
//! The ordering is therefore not negotiable, and it is the *claim* that has to
//! be first:
//!
//! 1. **claim** the key — atomically, so exactly one caller owns it;
//! 2. **execute**, only if the claim came back as yours;
//! 3. **record** the result under the same key.
//!
//! Claiming and checking are the same operation here on purpose. A check that
//! does not also claim leaves a window between the two, and in that window a
//! second retry reads the same absence and runs the tool as well. That is the
//! same read-modify-write hazard the registry's compare-and-swap publish
//! carries, and it is closed here by a single lock in the adapter rather than
//! by an ordering argument in the handler.
//!
//! # Why the key is the request id
//!
//! Because that is the only thing a retrying client reliably repeats. JSON-RPC
//! requires a request id to be unique among in-flight requests, and a client
//! retrying an unanswered request reuses the id it never got a response for.
//! So the id *is* the idempotency key for a dropped connection, and this type
//! derives from [`RequestId`] rather than asking the peer for a second header
//! it would have no way to invent.
//!
//! # Why a `u64` generation rides along with the claim
//!
//! Because a claim can be abandoned. If execution fails, the key has to return
//! to absent so a retry can run it — and if the first attempt is merely slow
//! rather than failed, it may come back afterwards and record a result for a
//! key the second attempt now owns. Two writers, one slot, and the second
//! result is the one the retrying client should see. A token carrying the
//! generation it was issued under turns that into a typed [`StaleClaim`]
//! instead of a silent overwrite.
//!
//! # Why the state machine is a domain type
//!
//! It is the transition table and nothing else: a map, three operations and one
//! bounded-resource rule. It has no lock, no I/O and no clock, so it can be
//! tested as a state machine. What it cannot do is make the claim *atomic
//! against another thread*, because a `&mut self` method is only exclusive
//! within one caller. That is the lock's job, and the lock lives in the
//! adapter: see [`crate::application::idempotency`].

use core::fmt;
use core::num::NonZeroUsize;
use core::str::FromStr;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use thiserror::Error;

use crate::domain::RequestId;
use crate::domain::session::SessionId;

/// The key a side effect is deduplicated by.
///
/// Deliberately *not* `Copy`, for the reason [`RequestId`] is not: it wraps an
/// `Arc<str>`, and duplicating a string key is not something anyone means to
/// do. Two "copies" of one key are the same key, and a function that needed
/// only to read it should have taken a reference.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IdempotencyKey(Arc<str>);

impl IdempotencyKey {
    /// A key from anything string-shaped.
    #[must_use]
    pub fn new(value: impl AsRef<str>) -> Self {
        Self(Arc::from(value.as_ref()))
    }

    /// The key, as the request id a retrying client repeats.
    ///
    /// Renders the id through its own `Display`, so a numeric id and a string
    /// id that print the same are the same key. That is the JSON-RPC reading:
    /// an id is whatever appears in the `id` member, and a client that retries
    /// repeats it exactly.
    #[must_use]
    pub fn from_request_id(id: &RequestId) -> Self {
        Self::new(id.to_string())
    }

    /// The key for one request within one session.
    ///
    /// A request id alone is not a key, and the reason is that ids are only
    /// unique per connection: every MCP client numbers its first request `1`,
    /// so `("session-a", 7)` and `("session-b", 7)` are two different calls. A
    /// store keyed on the bare id would answer the second session's `7` with
    /// the first session's result — a cross-session data leak wearing the
    /// costume of a duplicate.
    ///
    /// The separator is a byte that cannot appear in either half. `SessionId`
    /// renders as an integer and `RequestId` is a number or a string, and a
    /// string id is free to contain anything the peer typed, so the join has to
    /// be unambiguous in the general case: `session = 1, id = "2:3"` and
    /// `session = 12, id = "3"` both render, and only a separator that the id
    /// cannot contain keeps them apart. A `/` is the choice because MCP session
    /// ids and the ids a client sends are both opaque, and a `/` in a client-chosen
    /// id is legal — so this is a delimiter, not an escape, and the two halves
    /// are distinguished by the _first_ separator rather than by a scan.
    #[must_use]
    pub fn for_request(session: &SessionId, id: &RequestId) -> Self {
        Self(Arc::from(format!("{session}/{id}").as_str()))
    }

    /// The key for a method invocation, which is the form every
    /// observable-side-effect handler wants.
    ///
    /// [`Self::for_request`] keys on the session and the request id alone, and
    /// that is exactly right for *retrying the same call*: a client whose POST
    /// never got an answer resends the identical request and must not run the
    /// tool twice.
    ///
    /// It is wrong as the only form, because a request id is not a promise that
    /// the same id names the same operation. JSON-RPC 2.0 requires the peer to
    /// be able to reuse ids across a *sequence* of requests, and real clients
    /// do — the conformance suite restarts its counter at `1` per scenario
    /// against a session it never closed. Keyed on the id alone, the second
    /// `tools/call` of a session is answered with the first one's recording: ask
    /// `test_audio_content` as id 1 and `test_embedded_resource` as id 2 in
    /// separate scenarios sharing a connection, and the second is handed back
    /// an audio block. That is worse than a wrong answer, because the caller
    /// cannot tell it from a correct one — it is a *valid* `CallToolResult`
    /// carrying the wrong tool's output.
    ///
    /// Adding the method and the tool name makes the key identify *what was
    /// asked* as well as *who asked and when*, so a genuinely repeated
    /// `tools/call` for the same tool still replays, and a different tool under
    /// a reused id runs. It cannot distinguish a *retry* of a different-argument
    /// call under the same id, which is a caller violating JSON-RPC in the other
    /// direction; treating that as a fresh call is the answer that loses less.
    #[must_use]
    pub fn for_method_call(
        session: &SessionId,
        id: &RequestId,
        method: &str,
        target: &str,
    ) -> Self {
        Self(Arc::from(
            format!("{session}/{id}/{method}/{target}").as_str(),
        ))
    }

    /// The key as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for IdempotencyKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl From<&RequestId> for IdempotencyKey {
    fn from(id: &RequestId) -> Self {
        Self::from_request_id(id)
    }
}

impl FromStr for IdempotencyKey {
    type Err = core::convert::Infallible;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Ok(Self::new(text))
    }
}

/// Proof that one caller, and only that caller, owns a key right now.
///
/// Carries the generation it was issued under so a late completion from an
/// abandoned attempt is refused rather than overwriting the attempt that
/// replaced it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClaimToken {
    key: IdempotencyKey,
    generation: u64,
}

impl ClaimToken {
    /// The key this claim is for.
    #[must_use]
    pub const fn key(&self) -> &IdempotencyKey {
        &self.key
    }

    /// The attempt this token was issued to.
    ///
    /// A counter, not a random value, and exposed for the error a stale claim
    /// produces. It carries no entropy and is not a secret.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }
}

/// The three things a claim can find, and the three responses a caller has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim<T> {
    /// The key was absent. This caller owns it and must either complete it with
    /// a result or abandon it.
    ///
    /// In both cases the key stays reserved until one of them happens, which is
    /// what stops a second caller from taking it in between.
    Claimed {
        /// The token to present when recording or releasing.
        token: ClaimToken,
    },

    /// The key already completed. Hand the recorded result back and do **not**
    /// execute: the whole point is that the side effect does not run a second
    /// time.
    Completed(T),

    /// Another caller holds the key right now. Do not execute and do not steal
    /// it — the holder is a real request, and taking the key from it would run
    /// the same side effect twice from two callers.
    InFlight,
}

impl<T> Claim<T> {
    /// Whether this caller is the one that should execute.
    #[must_use]
    pub const fn is_execution_mine(&self) -> bool {
        matches!(self, Self::Claimed { .. })
    }

    /// The result, if the key had already completed.
    #[must_use]
    pub const fn recorded(&self) -> Option<&T> {
        match self {
            Self::Completed(result) => Some(result),
            Self::Claimed { .. } | Self::InFlight => None,
        }
    }

    /// The token, if the claim came back to this caller.
    #[must_use]
    pub const fn token(&self) -> Option<&ClaimToken> {
        match self {
            Self::Claimed { token } => Some(token),
            Self::Completed(_) | Self::InFlight => None,
        }
    }
}

/// What is stored against one key.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Slot<T> {
    /// A caller owns the key and has not finished with it.
    Held {
        /// The attempt this hold belongs to.
        generation: u64,
    },
    /// The side effect ran and produced this.
    Done(T),
}

/// The transition table, with no lock and no I/O.
///
/// # What `&mut self` guarantees and what it does not
///
/// It guarantees the transitions are consistent within one caller. It does not
/// make [`claim`](Self::claim) atomic against another thread, because a
/// `&mut self` method only excludes callers that share the borrow. An adapter
/// holds this behind a `Mutex` — one acquisition per operation, and the lock
/// is released across the execution rather than held across it, so a slow tool
/// does not block every other key. That is the same division as the registry's
/// compare-and-swap publish: the state machine is domain, the atomicity is the
/// adapter's, and neither is asked to do the other's job.
#[derive(Debug)]
pub struct CompletedResults<T> {
    slots: HashMap<IdempotencyKey, Slot<T>>,
    /// Completed keys, oldest first. A key joins when it completes and leaves
    /// only by eviction, so every key in here is `Done`.
    completed_order: VecDeque<IdempotencyKey>,
    next_generation: u64,
    capacity: NonZeroUsize,
}

impl<T> CompletedResults<T> {
    /// A store holding at most `capacity` keys.
    ///
    /// Bounded because a client chooses the keys: an unbounded map keyed by a
    /// peer-supplied id is a memory exhaustion primitive, and the wire offers
    /// nothing that would stop a client from minting a fresh id per request.
    ///
    /// A capacity of zero is refused at the type level — such a store could
    /// never complete anything, so every call would fail for a reason that has
    /// nothing to do with the work itself.
    #[must_use]
    pub fn with_capacity(capacity: NonZeroUsize) -> Self {
        Self {
            slots: HashMap::with_capacity(capacity.get()),
            completed_order: VecDeque::new(),
            next_generation: 0,
            capacity,
        }
    }

    /// How many keys are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether nothing is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// The ceiling, in keys.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity.get()
    }

    /// Takes ownership of `key`, or reports why it could not.
    ///
    /// # Errors
    ///
    /// [`IdempotencyError::StoreFull`] when the store is at capacity, `key` is
    /// not already present, and every slot is held by a request still running.
    /// That is genuine overload, and refusing the new request is visible where
    /// discarding a live claim would let two callers run one request.
    ///
    /// # Retention
    ///
    /// When the store is full and `key` is new, the **oldest completed**
    /// result is dropped to make room. A retry replays its recorded result for
    /// as long as it is among the last `capacity` completed requests; a retry
    /// of something older than that runs again. The alternative, refusing every
    /// new key once the store has filled with finished work, turns a long-lived
    /// session into one that fails every call after the first `capacity`, which
    /// is the common case and not an edge. A dropped connection is retried in
    /// seconds, so the window is far longer than any retry needs.
    ///
    /// `T: Clone` because handing a recorded result to the retrying caller
    /// leaves the recorded copy in place — a claim that consumed it would break
    /// the second retry, which is the case the store exists for.
    pub fn claim(&mut self, key: &IdempotencyKey) -> Result<Claim<T>, IdempotencyError>
    where
        T: Clone,
    {
        if let Some(slot) = self.slots.get(key) {
            return Ok(match slot {
                Slot::Done(result) => Claim::Completed(result.clone()),
                Slot::Held { .. } => Claim::InFlight,
            });
        }
        if self.slots.len() >= self.capacity.get() {
            let Some(oldest) = self.completed_order.pop_front() else {
                return Err(IdempotencyError::StoreFull {
                    capacity: self.capacity.get(),
                });
            };
            self.slots.remove(&oldest);
        }
        self.next_generation += 1;
        let generation = self.next_generation;
        self.slots.insert(key.clone(), Slot::Held { generation });
        Ok(Claim::Claimed {
            token: ClaimToken {
                key: key.clone(),
                generation,
            },
        })
    }

    /// Records the result of a claim.
    ///
    /// # Errors
    ///
    /// [`StaleClaim`] when the token is not the current holder of the key —
    /// because the claim was abandoned and re-taken, or because it was never
    /// valid. A stale completion is refused rather than applied: the attempt
    /// that replaced it is the one whose result the retrying client is entitled
    /// to, and writing over it would give two callers two different answers for
    /// one request id.
    pub fn complete(&mut self, token: &ClaimToken, result: T) -> Result<(), StaleClaim> {
        match self.slots.get(&token.key) {
            Some(Slot::Held { generation }) if *generation == token.generation => {}
            _ => return Err(StaleClaim::new(token)),
        }
        self.slots.insert(token.key.clone(), Slot::Done(result));
        self.completed_order.push_back(token.key.clone());
        Ok(())
    }

    /// Releases a claim so a later retry can execute.
    ///
    /// # Errors
    ///
    /// [`StaleClaim`] when the token is not the current holder. Releasing a
    /// claim another attempt now owns would hand the same key to a third
    /// caller while the second is still running it.
    pub fn abandon(&mut self, token: &ClaimToken) -> Result<(), StaleClaim> {
        match self.slots.get(&token.key) {
            Some(Slot::Held { generation }) if *generation == token.generation => {}
            _ => return Err(StaleClaim::new(token)),
        }
        self.slots.remove(&token.key);
        Ok(())
    }

    /// Whether a key has completed, without claiming it.
    #[must_use]
    pub fn completed(&self, key: &IdempotencyKey) -> Option<&T> {
        match self.slots.get(key) {
            Some(Slot::Done(result)) => Some(result),
            Some(Slot::Held { .. }) | None => None,
        }
    }

    /// Whether a key is held by a live claim.
    #[must_use]
    pub fn is_in_flight(&self, key: &IdempotencyKey) -> bool {
        matches!(self.slots.get(key), Some(Slot::Held { .. }))
    }
}

/// A claim that is no longer the current holder of its key.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error(
    "the claim on key {key} from attempt {stale_generation} is stale: the key has \
     since been released and re-claimed, and the later attempt owns the result"
)]
pub struct StaleClaim {
    key: IdempotencyKey,
    stale_generation: u64,
}

impl StaleClaim {
    /// The refusal, naming the key and the attempt that was too late.
    #[must_use]
    pub fn new(token: &ClaimToken) -> Self {
        Self {
            key: token.key.clone(),
            stale_generation: token.generation,
        }
    }

    /// The key.
    #[must_use]
    pub const fn key(&self) -> &IdempotencyKey {
        &self.key
    }

    /// The attempt that was too late.
    #[must_use]
    pub const fn stale_generation(&self) -> u64 {
        self.stale_generation
    }
}

/// Why a claim could not be taken.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IdempotencyError {
    /// Every slot is held by a request that has not finished, and the key is new.
    ///
    /// Finished results are evicted oldest-first to make room, so this means the
    /// store is full of work in progress. See [`CompletedResults::claim`].
    #[error(
        "the idempotency store holds {capacity} keys and cannot claim another without risking a lost result"
    )]
    StoreFull {
        /// The ceiling, in keys.
        capacity: usize,
    },

    /// A claim was presented that no longer holds its key.
    #[error(transparent)]
    Stale(#[from] StaleClaim),

    /// The store could not answer, for a reason of its own.
    ///
    /// In the in-memory adapter that is a poisoned lock: some other caller's
    /// panic unwound through it, and the state inside is no longer something
    /// this process can vouch for. The store refuses rather than guessing,
    /// because a wrong answer here means either a side effect that ran twice or
    /// a result handed to a caller that did not earn it.
    ///
    /// It is a variant of its own rather than a [`StaleClaim`] because "your
    /// claim is too late" and "I cannot tell you anything" are different facts,
    /// and a caller can act on the first and only retry on the second.
    #[error("the idempotency store could not answer: {reason}")]
    Unavailable {
        /// What went wrong, in the adapter's words.
        reason: &'static str,
    },
}

#[cfg(test)]
mod tests {
    use super::{
        Claim, ClaimToken, CompletedResults, IdempotencyError, IdempotencyKey, StaleClaim,
    };
    use crate::domain::RequestId;
    use core::num::NonZeroUsize;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    fn store(capacity: usize) -> Result<CompletedResults<String>, Failure> {
        let capacity = NonZeroUsize::new(capacity).ok_or(Failure::ZeroCapacity)?;
        Ok(CompletedResults::with_capacity(capacity))
    }

    /// A test that panics on `.expect()` cannot tell a failed assertion from a
    /// failed fixture, so every fallible call below is `?`'d into a `Failure`
    /// arm instead.
    #[derive(Debug, PartialEq, Eq)]
    enum Failure {
        ZeroCapacity,
        WrongClaim(&'static str),
        WrongResult(&'static str),
        WrongError(&'static str),
        NotStale,
        Stale,
        Unexpected(String),
    }

    impl From<StaleClaim> for Failure {
        fn from(_: StaleClaim) -> Self {
            Self::Stale
        }
    }

    /// A store error carries its own explanation, so it is carried through
    /// rather than flattened. The fixtures that expect a particular error match
    /// on [`Failure::WrongError`] instead, so a conversion here cannot mask the
    /// distinction a test is making.
    impl From<IdempotencyError> for Failure {
        fn from(error: IdempotencyError) -> Self {
            Self::Unexpected(error.to_string())
        }
    }

    fn token_of<T>(claim: &Claim<T>) -> Result<ClaimToken, Failure> {
        claim
            .token()
            .cloned()
            .ok_or(Failure::WrongClaim("not claimed"))
    }

    /// A token names the key it was issued for and the attempt it belongs to.
    ///
    /// Both are what make a token *checkable* rather than merely unique, so
    /// this pins that the accessors read back the key that was claimed rather
    /// than a copy that could drift from it.
    #[test]
    fn a_token_reads_back_the_key_and_the_attempt() -> Result<(), Failure> {
        let mut results = store(4)?;
        let key = IdempotencyKey::new("req-1");
        let token = token_of(&results.claim(&key)?)?;

        if token.key() != &key {
            return Err(Failure::WrongResult("a different key"));
        }
        let first = token.generation();
        if first == 0 {
            return Err(Failure::WrongResult("generation zero"));
        }

        // A re-claim after the attempt was abandoned is a *different* attempt,
        // and the generation is what tells them apart. A store that handed back
        // the old token would let a stale attempt overwrite a newer result.
        results.abandon(&token)?;
        let retaken = token_of(&results.claim(&key)?)?;
        if retaken.generation() <= first {
            return Err(Failure::WrongResult(
                "a re-claim did not advance the generation",
            ));
        }
        Ok(())
    }

    /// The first claim on a fresh key comes back owned, and the key then reads
    /// as in flight rather than as absent — the window a naive check-then-execute
    /// leaves open.
    #[test]
    fn a_fresh_key_is_claimed_and_then_held() -> Result<(), Failure> {
        let mut results = store(4)?;
        let key = IdempotencyKey::new("req-1");

        let claim = results.claim(&key)?;
        if !claim.is_execution_mine() {
            return Err(Failure::WrongClaim("a fresh key was not claimed"));
        }
        if !results.is_in_flight(&key) {
            return Err(Failure::WrongResult("a claimed key did not read as held"));
        }
        if results.completed(&key).is_some() {
            return Err(Failure::WrongResult("a held key read as completed"));
        }
        Ok(())
    }

    /// **The claim is the check.** A second claim on a held key reports
    /// `InFlight` and does not hand out a token, so the second caller has
    /// nothing it could execute under.
    #[test]
    fn a_held_key_refuses_a_second_claim() -> Result<(), Failure> {
        let mut results = store(4)?;
        let key = IdempotencyKey::new("req-1");
        let _first = results.claim(&key)?;

        match results.claim(&key)? {
            Claim::InFlight => Ok(()),
            Claim::Claimed { .. } => Err(Failure::WrongClaim("a held key was re-claimed")),
            Claim::Completed(_) => Err(Failure::WrongClaim("a held key reported a result")),
        }
    }

    /// A completed key hands the recorded result back rather than a token, so
    /// the caller has nothing it could execute under — which is the requirement
    /// that the retry replays the original response.
    #[test]
    fn a_completed_key_hands_back_the_result_without_a_token() -> Result<(), Failure> {
        let mut results = store(4)?;
        let key = IdempotencyKey::new("req-1");
        let token = token_of(&results.claim(&key)?)?;
        results.complete(&token, "original".to_owned())?;

        match results.claim(&key)? {
            Claim::Completed(result) => {
                if result != "original" {
                    return Err(Failure::WrongResult(
                        "the recorded result was not handed back",
                    ));
                }
                if results.is_in_flight(&key) {
                    return Err(Failure::WrongResult("a completed key read as held"));
                }
                Ok(())
            }
            Claim::Claimed { .. } => Err(Failure::WrongClaim("a completed key was re-claimed")),
            Claim::InFlight => Err(Failure::WrongClaim("a completed key reported in flight")),
        }
    }

    /// Abandoning returns the key to absent, so a retry after a *failed* attempt
    /// can run. Without this, one transient failure would poison the key
    /// forever and the client could never get an answer.
    #[test]
    fn abandoning_releases_the_key_for_a_retry() -> Result<(), Failure> {
        let mut results = store(4)?;
        let key = IdempotencyKey::new("req-1");
        let token = token_of(&results.claim(&key)?)?;
        results.abandon(&token)?;

        if results.is_in_flight(&key) {
            return Err(Failure::WrongResult("an abandoned key still reads as held"));
        }
        if results.completed(&key).is_some() {
            return Err(Failure::WrongResult("an abandoned key read as completed"));
        }
        if !results.claim(&key)?.is_execution_mine() {
            return Err(Failure::WrongClaim("an abandoned key was not re-claimable"));
        }
        Ok(())
    }

    /// **The stale-claim case.** A slow first attempt comes back after a failure
    /// released its key and a second attempt re-claimed it. The late completion
    /// is refused, so the retrying client gets the *second* attempt's result
    /// rather than whichever landed last.
    #[test]
    fn a_late_completion_from_an_abandoned_claim_is_refused() -> Result<(), Failure> {
        let mut results = store(4)?;
        let key = IdempotencyKey::new("req-1");

        let slow = token_of(&results.claim(&key)?)?;
        results.abandon(&slow)?;
        let retry = token_of(&results.claim(&key)?)?;
        results.complete(&retry, "second".to_owned())?;

        if results.complete(&slow, "first".to_owned()).is_ok() {
            return Err(Failure::NotStale);
        }
        match results.completed(&key) {
            Some(result) if result == "second" => Ok(()),
            Some(_) => Err(Failure::WrongResult(
                "the stale result overwrote the current one",
            )),
            None => Err(Failure::WrongResult("the completed key read as absent")),
        }
    }

    /// A late *abandon* is refused too. Releasing a key a second attempt now
    /// owns would let a third caller start while the second is still running.
    #[test]
    fn a_late_abandon_from_an_abandoned_claim_is_refused() -> Result<(), Failure> {
        let mut results = store(4)?;
        let key = IdempotencyKey::new("req-1");
        let slow = token_of(&results.claim(&key)?)?;
        results.abandon(&slow)?;
        let retry = token_of(&results.claim(&key)?)?;

        if results.abandon(&slow).is_ok() {
            return Err(Failure::NotStale);
        }
        if !results.is_in_flight(&key) {
            return Err(Failure::WrongResult(
                "a late abandon released the live claim",
            ));
        }
        if results.claim(&key)?.is_execution_mine() {
            return Err(Failure::WrongClaim("the live claim was stealable"));
        }
        results.complete(&retry, "second".to_owned())?;
        Ok(())
    }

    /// Completing a key that was never claimed is refused. A token no store ever
    /// issued cannot install a result nobody executed for.
    #[test]
    fn completing_an_unclaimed_key_is_refused() -> Result<(), Failure> {
        let mut results = store(4)?;
        let key = IdempotencyKey::new("never-seen");
        let forged = ClaimToken {
            key: key.clone(),
            generation: 999,
        };
        if results.complete(&forged, "invented".to_owned()).is_ok() {
            return Err(Failure::NotStale);
        }
        if results.completed(&key).is_some() {
            return Err(Failure::WrongResult("a forged token installed a result"));
        }
        Ok(())
    }

    /// **Counting executions.** Sixty-four threads claim the same key and run
    /// the side effect only if the claim was theirs. The counter is what the
    /// assertion is about: not "the same result came back" but "the work
    /// happened once". This is the test the idempotency requirement is really
    /// about, and it is the one a check-then-execute handler fails — the mutex
    /// is a stand-in here for the adapter's lock, and the domain machine is
    /// what decides who executes.
    #[test]
    fn exactly_one_of_many_concurrent_callers_executes() -> Result<(), Failure> {
        const THREADS: usize = 64;

        let results = Arc::new(Mutex::new(store(8)?));
        let executions = Arc::new(AtomicUsize::new(0));
        let mut workers = Vec::with_capacity(THREADS);
        for _ in 0..THREADS {
            let results = Arc::clone(&results);
            let executions = Arc::clone(&executions);
            workers.push(thread::spawn(move || {
                let key = IdempotencyKey::new("req-1");
                let mut held = results
                    .lock()
                    .map_err(|_| Failure::Unexpected("poisoned".to_owned()))?;
                match held.claim(&key) {
                    Err(error) => Err(Failure::Unexpected(error.to_string())),
                    // Only the owner executes; everyone else must not.
                    Ok(Claim::Claimed { token }) => {
                        executions.fetch_add(1, Ordering::SeqCst);
                        held.complete(&token, "ran".to_owned())
                            .map_err(Failure::from)
                    }
                    Ok(Claim::InFlight | Claim::Completed(_)) => Ok(()),
                }
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
        let held = results
            .lock()
            .map_err(|_| Failure::Unexpected("poisoned".to_owned()))?;
        if held.len() != 1 {
            return Err(Failure::Unexpected(format!("{} keys", held.len())));
        }
        drop(held);
        Ok(())
    }

    /// Distinct keys are independent: claiming one does not block another, and
    /// completing one does not complete the other.
    #[test]
    fn distinct_keys_do_not_interfere() -> Result<(), Failure> {
        let mut results = store(4)?;
        let first = IdempotencyKey::new("req-1");
        let second = IdempotencyKey::new("req-2");

        let token = token_of(&results.claim(&first)?)?;
        if !results.claim(&second)?.is_execution_mine() {
            return Err(Failure::WrongClaim("a second key was blocked by the first"));
        }
        results.complete(&token, "one".to_owned())?;
        if results.completed(&second).is_some() {
            return Err(Failure::WrongResult("completing one key completed another"));
        }
        Ok(())
    }

    /// A full store of finished work makes room for a new key by dropping the
    /// oldest result, so a long-lived session keeps working. Without this the
    /// store fills once and every later call fails.
    #[test]
    fn a_full_store_drops_its_oldest_result_for_a_new_key() -> Result<(), Failure> {
        let mut results = store(2)?;
        for index in 0..2 {
            let key = IdempotencyKey::new(format!("req-{index}"));
            let token = token_of(&results.claim(&key)?)?;
            results.complete(&token, format!("result-{index}"))?;
        }

        let overflow = IdempotencyKey::new("req-overflow");
        if !results.claim(&overflow)?.is_execution_mine() {
            return Err(Failure::WrongClaim("a full store refused a new key"));
        }
        if results.len() != 2 {
            return Err(Failure::WrongResult("the ceiling was exceeded"));
        }

        // The newer result is still replayed; only the oldest went.
        let kept = IdempotencyKey::new("req-1");
        match results.claim(&kept)? {
            Claim::Completed(result) if result == "result-1" => {}
            _ => return Err(Failure::WrongResult("a full store lost the newer result")),
        }
        let dropped = IdempotencyKey::new("req-0");
        if results.completed(&dropped).is_some() {
            return Err(Failure::WrongResult("the oldest result was kept"));
        }
        Ok(())
    }

    /// A claim still running is never the one dropped: discarding it would let
    /// two callers run one request. A store full of running work refuses.
    #[test]
    fn a_store_full_of_running_work_refuses_a_new_key() -> Result<(), Failure> {
        let mut results = store(2)?;
        for index in 0..2 {
            let key = IdempotencyKey::new(format!("req-{index}"));
            let _ = results.claim(&key)?;
        }

        match results.claim(&IdempotencyKey::new("req-overflow")) {
            Err(IdempotencyError::StoreFull { capacity }) => {
                if capacity != 2 {
                    return Err(Failure::WrongError("the ceiling was misreported"));
                }
                Ok(())
            }
            Err(other) => Err(Failure::Unexpected(other.to_string())),
            Ok(_) => Err(Failure::WrongClaim("a full store handed out a claim")),
        }
    }

    /// The key is the request id a retrying client repeats, rendered through
    /// the id's own `Display` so a numeric id and its printed form agree.
    #[test]
    fn the_key_is_the_request_id_a_retry_repeats() -> Result<(), Failure> {
        let numeric = IdempotencyKey::from(&RequestId::number(7));
        let text = IdempotencyKey::new("7");
        if numeric != text {
            return Err(Failure::Unexpected(format!("{numeric} != {text}")));
        }
        if numeric.as_str() != "7" {
            return Err(Failure::Unexpected(numeric.to_string()));
        }

        // And the same key deduplicates regardless of which spelling the client
        // used, which is the property that makes it usable.
        let mut results = store(4)?;
        let token = token_of(&results.claim(&numeric)?)?;
        results.complete(&token, "once".to_owned())?;
        match results.claim(&text)? {
            Claim::Completed(result) if result == "once" => Ok(()),
            _ => Err(Failure::WrongClaim("a re-spelled key was treated as new")),
        }
    }

    /// An empty store claims cleanly, which is the state a fresh server starts
    /// in and the one a call would otherwise fail against.
    #[test]
    fn an_empty_store_claims_cleanly() -> Result<(), Failure> {
        let results = store(4)?;
        if !results.is_empty() {
            return Err(Failure::WrongResult("a fresh store was not empty"));
        }
        let key = IdempotencyKey::new("req-1");
        if results.completed(&key).is_some() || results.is_in_flight(&key) {
            return Err(Failure::WrongResult(
                "a fresh store reported an existing key",
            ));
        }
        Ok(())
    }
}
