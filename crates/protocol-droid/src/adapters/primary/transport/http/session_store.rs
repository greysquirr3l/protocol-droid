//! The `Mcp-Session-Id` map: the server's half of the session lifecycle.
//!
//! # What this owns
//!
//! Three transitions, and the specification names all three:
//!
//! - `initialize` **inserts**. The server mints an id, hands it back in a
//!   response header, and every later request quotes it.
//! - every later request **looks up**.
//! - `DELETE` **removes**, and the specification says the server "MUST NOT
//!   respond to requests containing that session ID" afterwards.
//!
//! Between those, a lookup miss is [`None`](SessionStore::lookup), and the
//! router turns that into `404`. That is deliberately a different outcome from
//! a header that will not parse: `McpSessionId::from_str` fails *before* the
//! store is consulted, so a malformed id is `400` and never becomes a `404`.
//! The two are not interchangeable — the specification's own recovery is
//! "on 404, start a new session", and a client whose header is malformed has
//! a different problem to fix.
//!
//! # Why the lock never escapes
//!
//! Every method takes the lock, does one map operation, and returns something
//! that is *not* a guard. Lookups hand back an [`Arc`] cloned out of the map,
//! which is what makes it structurally impossible for a caller to hold the
//! lock across an `.await`: the guard is a local, dropped at the end of the
//! function, and the returned value has no lifetime tied to `self`. A handler
//! that receives an `Arc<S>` can hold it for as long as it likes, including
//! across suspension points, and the store stays available to every other
//! request. Returning `&S` instead would push the same constraint on every
//! caller and be wrong on exactly the one that matters.
//!
//! [`concurrent_lookups_never_block_one_another`](self::tests::concurrent_lookups_never_block_one_another)
//! is the test that holds the `Send + Sync` claim — the router puts this in
//! shared state and a type that is neither would not compile there.
//!
//! # Why there is no capacity ceiling
//!
//! Because a ceiling on this map has exactly one honest eviction rule — drop
//! the oldest session — and that rule is indistinguishable, from the client,
//! from a server that forgot the session. Both produce `404`, and both reach
//! the client as "start a new session", so a load spike silently destroys live
//! state that the client believes it still owns. Refusing to open the
//! thousand-and-first session is loud and fixable; evicting the first is quiet
//! and is not. A deployment that needs a bound puts one in front of the
//! process.
//!
//! Note what that leaves out: unlike
//! [`InMemoryIdempotencyStore`](crate::adapters::secondary::InMemoryIdempotencyStore),
//! these keys are not peer-chosen. Nobody can make the server allocate a
//! session except by successfully completing an `initialize`, which is the
//! one operation that already does work worth bounding elsewhere.
//!
//! # Why the id comes from the engine's `IdSource`
//!
//! Because the alternative is a process-global RNG call inside an adapter,
//! which is the determinism rule. A test that wants to know which id a
//! `initialize` will produce cannot arrange it if the adapter reaches for
//! `rand::thread_rng()`; a test that runs the whole session lifecycle twice
//! cannot compare anything if the second run's ids differ from the first's.
//! [`IdSource`](crate::application::session::IdSource) is the seam the engine
//! already owns for exactly this, and sharing it means an HTTP session and an
//! engine session are numbered from the same sequence rather than two
//! unrelated ones.
//!
//! # Protocol neutrality
//!
//! The store names a *header*, not a method. Nothing here knows what
//! `initialize` is called, what it negotiates, or what a session does once it
//! exists — the payload is a type parameter, so a protocol whose sessions are
//! shaped differently instantiates this with a different `S` and changes
//! nothing. See [`SessionStore`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use thiserror::Error;

use crate::application::session::IdSource;
use crate::domain::mcp::McpSessionId;

/// The sessions this server currently holds open.
///
/// Generic over what a session *is*, because this type is a map and a map does
/// not need to know. The Streamable HTTP adapter instantiates it with whatever
/// the router keeps per session — an engine handle, a protocol state machine, a
/// cancellation token — and the `Arc` in [`lookup`](Self::lookup) is what lets
/// a handler hold that state across an `.await` without borrowing the store.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use protocol_droid::adapters::primary::transport::http::SessionStore;
/// use protocol_droid::application::session::IdSource;
/// use protocol_droid::domain::session::state::SessionId;
///
/// // A source that hands out a fixed sequence, so the test can say which id it
/// // expects instead of discovering it.
/// struct Counting(core::sync::atomic::AtomicU64);
/// impl IdSource for Counting {
///     fn next_session_id(&self) -> SessionId {
///         SessionId::new(
///             self.0.fetch_add(1, core::sync::atomic::Ordering::SeqCst) + 1,
///         )
///     }
/// }
/// let ids: Arc<dyn IdSource + Send + Sync> =
///     Arc::new(Counting(core::sync::atomic::AtomicU64::new(0)));
///
/// let store = SessionStore::new(ids);
/// let id = store.open(Arc::new("the negotiated state"))?;
/// // The id is a header value, so it round-trips through the header parser.
/// assert_eq!(id.to_string().parse(), Ok(id));
/// assert!(store.lookup(&id)?.is_some());
///
/// // DELETE removes it, and a later request is a miss rather than a leftover.
/// assert!(store.terminate(&id)?.is_some());
/// assert!(store.lookup(&id)?.is_none());
/// # Ok::<(), protocol_droid::adapters::primary::transport::http::SessionStoreError>(())
/// ```
pub struct SessionStore<S> {
    sessions: Mutex<HashMap<McpSessionId, Arc<S>>>,
    ids: Arc<dyn IdSource + Send + Sync>,
}

impl<S> SessionStore<S> {
    /// A store with no sessions, numbering them from `ids`.
    ///
    /// Takes the source rather than constructing one: the engine already has an
    /// `IdSource` for the same reason, and two sources means two sequences for
    /// what a reader will reasonably assume is one numbering.
    #[must_use]
    pub fn new(ids: Arc<dyn IdSource + Send + Sync>) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            ids,
        }
    }

    /// Registers `session` under a freshly minted id and returns that id.
    ///
    /// The caller puts the id in the `Mcp-Session-Id` response header; this type
    /// never formats it, because the buffer-based
    /// [`McpSessionId::as_bytes`](crate::domain::mcp::McpSessionId::as_bytes) is
    /// how the header gets filled without an allocation per response.
    ///
    /// # Errors
    ///
    /// [`SessionStoreError::DuplicateId`] if the injected source returned an id
    /// this store is already holding. [`IdSource`] promises not to repeat one,
    /// so a source that has wrapped or one that was seeded twice has broken its
    /// contract, and overwriting would hand the second caller a session
    /// belonging to the first. One id is tried: retrying a source that is
    /// provably out of ids is a livelock with better manners, and the error
    /// says what to do.
    ///
    /// [`SessionStoreError::Unavailable`] if the lock is poisoned.
    pub fn open(&self, session: Arc<S>) -> Result<McpSessionId, SessionStoreError> {
        let id = McpSessionId::new(self.ids.next_session_id());
        // The guard is scoped to the block so it is released before the
        // `Ok`, not at the end of the function: a caller that has just opened a
        // session should not still be holding the lock by the time it starts
        // doing the work the session exists for.
        {
            let mut sessions = self.locked()?;
            if sessions.contains_key(&id) {
                return Err(SessionStoreError::DuplicateId);
            }
            sessions.insert(id, session);
        }
        Ok(id)
    }

    /// The session under `id`, if this server still has it.
    ///
    /// A returned [`Arc`] is a snapshot of *reachability*, not of the session's
    /// contents: a concurrent `DELETE` can remove the map entry while this
    /// clone is still alive and being used. That is the intended semantics —
    /// the in-flight request finishes against the session it already found,
    /// and only the *next* request sees the `404` — so callers must not treat
    /// a `Some` as a promise that the entry will still be there in a moment.
    ///
    /// # Errors
    ///
    /// [`SessionStoreError::Unavailable`] if the lock is poisoned.
    pub fn lookup(&self, id: &McpSessionId) -> Result<Option<Arc<S>>, SessionStoreError> {
        Ok(self.locked()?.get(id).cloned())
    }

    /// Removes the session under `id`, returning it if it was there.
    ///
    /// A `None` here is the same `None` [`lookup`](Self::lookup) returns, and
    /// the router maps both to the same outcome. Deciding between "terminate
    /// this session" and "this endpoint does not offer termination here" is
    /// the router's, because only the router knows what the endpoint offers.
    ///
    /// # Errors
    ///
    /// [`SessionStoreError::Unavailable`] if the lock is poisoned.
    pub fn terminate(&self, id: &McpSessionId) -> Result<Option<Arc<S>>, SessionStoreError> {
        Ok(self.locked()?.remove(id))
    }

    /// How many sessions are open.
    ///
    /// # Errors
    ///
    /// [`SessionStoreError::Unavailable`] if the lock is poisoned. A made-up
    /// number here would be worse than the error: this is the count an
    /// operator reads to decide whether something is wrong.
    pub fn len(&self) -> Result<usize, SessionStoreError> {
        Ok(self.locked()?.len())
    }

    /// Whether no session is open.
    ///
    /// # Errors
    ///
    /// [`SessionStoreError::Unavailable`] if the lock is poisoned.
    pub fn is_empty(&self) -> Result<bool, SessionStoreError> {
        Ok(self.locked()?.is_empty())
    }

    /// One lock acquisition, shared by every operation.
    ///
    /// The guard's lifetime is this function's return type and nothing longer.
    /// That is the whole of the "no lock across an await" rule: the guard is
    /// consumed by the caller of `locked` inside a single expression, so it
    /// cannot outlive the statement it was needed for.
    fn locked(&self) -> Result<MutexGuard<'_, HashMap<McpSessionId, Arc<S>>>, SessionStoreError> {
        self.sessions
            .lock()
            .map_err(|_| SessionStoreError::Unavailable)
    }
}

impl<S> core::fmt::Debug for SessionStore<S> {
    /// Prints the type's name and nothing else.
    ///
    /// Deliberately opaque, and the omission is the point: a derived `Debug`
    /// on this struct would print the whole map, which is a table of session
    /// ids — and the project does not put a full session id in a log line.
    /// A `{:?}` in a log statement must not become a way around that.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("SessionStore")
            .finish_non_exhaustive()
    }
}

/// Why a [`SessionStore`] operation could not be answered.
///
/// Not an `Error` for its own sake — it is one so a handler can `?` it into the
/// same failure path as everything else the adapter refuses, and so
/// `thiserror` writes the text that reaches a log line.
///
/// # Why no id appears in either message
///
/// Because `Display` output here is very likely to end up in a log, and the
/// project does not log a full session id. Carrying the value inside the error
/// would make the leak one careless `tracing::error!("{err}")` away, and the
/// operator debugging a duplicate id wants to know that it happened far more
/// than they want to know which id it was — the source is the thing to inspect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SessionStoreError {
    /// A panic elsewhere unwound through the lock, so the map may be missing
    /// an insert it was midway through.
    ///
    /// The store refuses rather than recovering, matching the idempotency
    /// store's rule: answering from state this process cannot vouch for is how
    /// one request ends up served another request's session.
    #[error("the session store's lock is poisoned")]
    Unavailable,

    /// The injected [`IdSource`] returned an id that is already registered.
    ///
    /// [`IdSource`] requires that an id is not reused while either session can
    /// still appear in the event log, so this means the source has wrapped or
    /// was seeded twice. Replacing the source is the fix; the store will not
    /// overwrite, because doing so silently transfers an established session to
    /// whoever asked next.
    #[error("the session id source returned an id that is already in use")]
    DuplicateId,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{SessionStore, SessionStoreError};
    use crate::application::session::IdSource;
    use crate::domain::mcp::{McpSessionId, VISIBLE_ASCII_RANGE};
    use crate::domain::session::SessionId;

    /// A source that hands out a fixed sequence and counts what it produced.
    struct Counting(AtomicU64);

    impl Counting {
        fn new() -> Self {
            Self(AtomicU64::new(0))
        }

        fn shared() -> Arc<dyn IdSource + Send + Sync> {
            Arc::new(Self::new())
        }

        /// How many ids this source has been asked for.
        fn issued(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    impl IdSource for Counting {
        fn next_session_id(&self) -> SessionId {
            SessionId::new(self.0.fetch_add(1, Ordering::SeqCst))
        }
    }

    /// A source that always returns the same id, to exercise the collision
    /// path that a well-behaved source can never reach.
    struct Stuck(SessionId);

    impl IdSource for Stuck {
        fn next_session_id(&self) -> SessionId {
            self.0
        }
    }

    type TestError = Box<dyn std::error::Error>;

    /// What a lookup thread can report.
    ///
    /// A named error rather than a boxed `dyn Error`, because the two failures
    /// a thread has — the store refusing, and a session that was there at open
    /// time being absent at lookup time — are different bugs and the message
    /// has to survive the thread boundary to say which.
    #[derive(Debug)]
    enum ThreadFailure {
        /// The store refused an operation.
        Store(SessionStoreError),
        /// A session that was open was not found.
        Vanished,
    }

    impl From<SessionStoreError> for ThreadFailure {
        fn from(error: SessionStoreError) -> Self {
            Self::Store(error)
        }
    }

    impl core::fmt::Display for ThreadFailure {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::Store(error) => write!(formatter, "the store refused a lookup: {error}"),
                Self::Vanished => write!(formatter, "a live session was not found"),
            }
        }
    }

    impl std::error::Error for ThreadFailure {}

    /// The two failure modes name themselves in their `Display`, so a
    /// `?`-chained test can distinguish them without a match. If this ever
    /// fails, the two messages have been swapped or made identical.
    #[test]
    fn the_error_variants_are_distinguishable() {
        assert_ne!(
            SessionStoreError::Unavailable.to_string(),
            SessionStoreError::DuplicateId.to_string()
        );
        assert!(
            SessionStoreError::DuplicateId
                .to_string()
                .contains("already in use")
        );
    }

    /// The store's ids must be ids the header parser accepts.
    ///
    /// The store and the wire format are separate types, and nothing in the
    /// type system connects them: `McpSessionId` is constructed from a
    /// `SessionId` by a `const fn` that cannot fail, so the only thing standing
    /// between "the store minted an id" and "the client can quote it" is that
    /// the two agree. This is the assertion that says they do — parse the
    /// rendered header value back and compare the whole `Result`, so a failure
    /// prints what the parser objected to rather than just a mismatch.
    #[test]
    fn a_minted_id_round_trips_through_the_header_parser() -> Result<(), TestError> {
        let store = SessionStore::new(Counting::shared());
        let id = store.open(Arc::new(42usize))?;
        assert_eq!(id.to_string().parse::<McpSessionId>(), Ok(id));
        Ok(())
    }

    /// The rendered header value is fixed-width and entirely legal in a header.
    ///
    /// Two separate claims, and they fail differently. A width that varied with
    /// the value would let a length-sensitive intermediary treat `7` and
    /// `0000000000000007` as different sessions; a byte outside visible ASCII
    /// would not be a header at all. So the assertion checks both the exact
    /// spelling and the byte range, rather than "it parsed".
    #[test]
    fn the_header_value_is_fixed_width_and_visible_ascii() -> Result<(), TestError> {
        let store = SessionStore::new(Counting::shared());
        for _ in 0..4 {
            let id = store.open(Arc::new(()))?;
            let rendered = id.to_string();
            assert_eq!(
                rendered.len(),
                16,
                "session id {rendered:?} is not sixteen characters"
            );
            assert!(
                rendered
                    .bytes()
                    .all(|byte| VISIBLE_ASCII_RANGE.contains(&byte)),
                "session id {rendered:?} carries a byte no header may contain"
            );
            assert!(
                rendered.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "session id {rendered:?} is not hexadecimal"
            );
        }
        Ok(())
    }

    /// Ids come from the injected source, and the store never numbers on its own.
    ///
    /// The claim is *substitution*: if the adapter reached for a process-global
    /// RNG instead of the injected port, the sequence below would not be the
    /// one the source produced. Asserting the exact first id is what makes a
    /// `thread_rng` mutant fail here rather than merely being unlikely to be
    /// noticed.
    #[test]
    fn ids_come_from_the_injected_source() -> Result<(), TestError> {
        let source = Arc::new(Counting::new());
        let store = SessionStore::new(source.clone());
        let first = store.open(Arc::new(()))?;
        let second = store.open(Arc::new(()))?;
        assert_eq!(first, McpSessionId::new(SessionId::new(0)));
        assert_eq!(second, McpSessionId::new(SessionId::new(1)));
        assert_eq!(source.issued(), 2, "the store asked for a different count");
        Ok(())
    }

    /// A lookup miss is `None`, and it is reached only after the header parsed.
    ///
    /// This is the 404-versus-400 boundary. `McpSessionId` is produced by
    /// `from_str`, so a client that sends garbage never gets as far as
    /// `lookup` — and the test says so by parsing a malformed header and
    /// checking the failure is a parse error, *then* checking that a
    /// well-formed header naming nothing is a miss. Conflating the two would
    /// answer a broken header with "start a new session", which is advice that
    /// cannot possibly help.
    #[test]
    fn a_miss_and_a_malformed_header_are_different_outcomes() -> Result<(), TestError> {
        let store = SessionStore::new(Counting::shared());
        let live = store.open(Arc::new(()))?;
        store.terminate(&live)?;

        // Well-formed, and unknown: a miss, which the router answers 404.
        assert_eq!(store.lookup(&live)?, None);

        // Not well-formed: the header parser refuses before the store is asked,
        // which is the router's 400.
        assert!("not-a-session".parse::<McpSessionId>().is_err());
        assert!("".parse::<McpSessionId>().is_err());
        Ok(())
    }

    /// `DELETE` of a session that is not there is also a miss, not a panic.
    ///
    /// The specification lets a server answer `405` on a `DELETE` it does not
    /// offer, and either way the store's answer is the same `None` the router
    /// turns into a decision. The claim under test is that removing twice is
    /// well-defined and the second removal changes nothing.
    #[test]
    fn terminating_an_absent_session_is_a_miss() -> Result<(), TestError> {
        let store = SessionStore::new(Counting::shared());
        let id = store.open(Arc::new(()))?;
        assert!(store.terminate(&id)?.is_some());
        assert_eq!(store.terminate(&id)?, None);
        assert!(store.is_empty()?);
        Ok(())
    }

    /// A source that repeats an id must not transfer a live session to a new
    /// caller.
    ///
    /// Overwriting here would be a session hijack: the second `initialize`
    /// would be handed the first client's negotiated state, its event ring,
    /// and its id, and neither side would find out until a request came back
    /// addressed to someone else's conversation. `IdSource` forbids this, so
    /// the store's job is to notice rather than to comply.
    #[test]
    fn a_repeated_id_refuses_instead_of_overwriting() -> Result<(), TestError> {
        let store = SessionStore::new(Arc::new(Stuck(SessionId::new(7))));
        let first = store.open(Arc::new("first"))?;
        let second = store.open(Arc::new("second"));
        assert_eq!(second, Err(SessionStoreError::DuplicateId));

        // And the original session is untouched: one entry, still the first.
        assert_eq!(store.len()?, 1);
        let survivor = store.lookup(&first)?;
        assert_eq!(survivor.as_deref(), Some(&"first"));
        Ok(())
    }

    /// The store is usable from many threads at once, and no lookup waits on
    /// another.
    ///
    /// Two claims in one test because they have the same failure mode to
    /// detect: if the guard were part of a returned value, or if `S` were not
    /// `Send + Sync`, this would not compile — and the router putting the store
    /// in shared state needs it to. The assertion that every thread finds every
    /// session also catches a store that accidentally hands out a shared
    /// mutable view instead of a clone.
    #[test]
    fn concurrent_lookups_never_block_one_another() -> Result<(), TestError> {
        const SESSIONS: usize = 8;
        const THREADS: usize = 4;

        let store = Arc::new(SessionStore::new(Counting::shared()));
        let mut ids = Vec::new();
        for _ in 0..SESSIONS {
            ids.push(store.open(Arc::new(SESSIONS))?);
        }

        let threads: Vec<_> = (0..THREADS)
            .map(|_| {
                let store = Arc::clone(&store);
                let ids = ids.clone();
                std::thread::spawn(move || -> Result<(), ThreadFailure> {
                    for _ in 0..64 {
                        for id in &ids {
                            if store.lookup(id)?.is_none() {
                                return Err(ThreadFailure::Vanished);
                            }
                        }
                    }
                    Ok(())
                })
            })
            .collect();
        for thread in threads {
            match thread.join() {
                Ok(result) => result?,
                Err(_) => return Err("a lookup thread panicked".into()),
            }
        }

        assert_eq!(store.len()?, SESSIONS);
        Ok(())
    }

    /// `len` and `is_empty` describe the same map the other methods mutate.
    ///
    /// Without this, an adapter could report a healthy zero-session server
    /// while refusing every request, and the operator would be looking at the
    /// wrong number.
    #[test]
    fn the_counters_track_the_operations() -> Result<(), TestError> {
        let store = SessionStore::new(Counting::shared());
        assert!(store.is_empty()?);
        assert_eq!(store.len()?, 0);

        let first = store.open(Arc::new(()))?;
        let second = store.open(Arc::new(()))?;
        assert_eq!(store.len()?, 2);
        assert!(!store.is_empty()?);

        store.terminate(&first)?;
        assert_eq!(store.len()?, 1);
        store.terminate(&second)?;
        assert!(store.is_empty()?);
        Ok(())
    }
}
