//! Server-initiated requests over Streamable HTTP — sampling, elicitation.
//!
//! # Why this is more than "write the request onto the tail"
//!
//! A server-initiated request is the one message shape in the protocol that
//! does not terminate. A tool asks the peer a question and then waits; the
//! answer arrives on a *later* `POST`, as a JSON-RPC **response** — a message
//! with an id and no method.
//!
//! So there are two halves and neither is optional. The server writes
//! `sampling/createMessage` onto the stream the in-flight `tools/call` opened,
//! and the router has to recognise the peer's answer when it arrives rather than
//! dispatching it as a request of its own. Without the recognition the answer
//! fails to find a method, comes back as a JSON-RPC error, and the tool keeps
//! waiting for an answer the server has already decided does not exist. The
//! symptom the peer sees is "the server did not wait for my answer", which is
//! exactly backwards.
//!
//! # Why the table is keyed by request id and nothing else
//!
//! The id is the only thing the peer's answer echoes, so it is the only thing a
//! correlation can rest on. A `result` and an `error` with the same id are both
//! answers — the difference is in the body, and the body is what the waiter
//! receives — so the entry is not split by outcome.
//!
//! Ids are minted from a per-table counter rather than derived from the request
//! being answered. A server-initiated request is a request *this* server made,
//! so it has its own id space: reusing the peer's would collide on every
//! `tools/call` whose id happened to match.
//!
//! # Why an unknown id is not an error
//!
//! JSON-RPC says an answer to an unknown request is discarded, and this server
//! cannot tell "the tool already gave up" from "the peer answered twice" from
//! "that id was never ours". All three are silently dropped, because the
//! alternative is inventing a request to attach the answer to.
//!
//! # Why a dropped waiter is a named refusal
//!
//! A client that never answers is a client that walked away, and this layer has
//! no way to tell that from one that is still thinking — so the waiter waits,
//! bounded by its session. When the session ends the table goes with it and
//! nothing leaks. A timeout here would be a policy decision this layer has no
//! standing to make.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::PoisonError;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicI64;
use std::sync::atomic::Ordering;

use serde_json::Value;
use tokio::sync::oneshot;

use crate::adapters::primary::transport::http::stream::EventTail;
use crate::application::server::context::{CallbackExchange, ExchangeError};
use crate::domain::ir::JsonObject;

/// The stream a session's server-initiated messages go out on.
type Tail = EventTail<Value>;

/// The table of server-initiated requests a session is waiting on.
///
/// Cheap to clone: a session's exchange and the router answering its requests
/// are two handles to one table, never two tables. That matters because a
/// request and the answer completing it must land on the same session, and
/// sharing is what makes that structural rather than a convention.
#[derive(Debug, Clone, Default)]
pub struct ExchangeTable {
    /// The waiters, and the counter they draw ids from.
    ///
    /// One mutex over both rather than a `Mutex<HashMap>` plus a separate
    /// atomic: minting an id and registering under it is one step, and two
    /// steps would leave a window where the id is reserved but unclaimed.
    inner: Arc<Mutex<Table>>,
    /// Whether anything is draining the session's tail.
    ///
    /// A server answering a `POST` with one JSON document is not draining the
    /// tail, because there is no body left to drain it into. A tool that
    /// elicits on such a server pushes its request into the ring and then
    /// waits for an answer that has nowhere to be read, which is a hang with
    /// no error and no timeout — the worst shape a protocol defect can take.
    ///
    /// So this is checked before the wait rather than left to be discovered by
    /// the peer, and the check answers with a refusal naming the reason.
    /// Outside the mutex because it never changes after construction.
    readable: Arc<AtomicBool>,
}

/// The first id a session's server-initiated requests are minted from.
///
/// Above every id the peer is likely to use. Not a range with a documented
/// ceiling, because JSON-RPC gives an id no bound to exceed — what matters is
/// that this space and the peer's do not meet, and starting a peer at `1` is
/// the only claim either side can make about what the other will reach.
const ID_ORIGIN: i64 = 1 << 40;

/// What the table holds.
#[derive(Debug)]
struct Table {
    /// Outstanding requests, by the id the peer will echo.
    waiters: HashMap<i64, oneshot::Sender<Answer>>,
    /// The next id to hand out, starting at [`ID_ORIGIN`].
    next_id: AtomicI64,
}

/// What a peer's answer carries: the peer's own result, or its own error.
pub type Answer = Result<JsonObject, ExchangeError>;

impl Default for Table {
    fn default() -> Self {
        Self {
            waiters: HashMap::new(),
            next_id: AtomicI64::new(ID_ORIGIN),
        }
    }
}

impl ExchangeTable {
    /// A table with nothing in flight and nothing reading the tail.
    ///
    /// The unreadable default is the safe one. A session that has not said it
    /// is served as a stream has not established that anything is reading, so
    /// the honest answer to "may I ask the peer a question?" is no.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A table whose session's tail this server drains.
    ///
    /// Called once, when a session is opened against a server configured to
    /// answer with an event stream. Every later [`Self::new`] on the same
    /// handle — and there are none, this is the only constructor that sets it
    /// — inherits the answer.
    #[must_use]
    pub fn readable() -> Self {
        Self {
            readable: Arc::new(AtomicBool::new(true)),
            ..Self::default()
        }
    }

    /// Whether a server-initiated request can be asked and answered here.
    #[must_use]
    pub fn is_readable(&self) -> bool {
        self.readable.load(Ordering::Relaxed)
    }

    /// Records a waiter under a fresh id, returning it.
    ///
    /// The id is minted here rather than by the caller so registering a waiter
    /// and claiming its id cannot come apart.
    ///
    /// It starts at [`ID_ORIGIN`] rather than `1`, and that is load-bearing in
    /// a way that is invisible until it is wrong. The session engine keeps its
    /// own in-flight map keyed by the *peer's* request ids, and a minted
    /// server-initiated id that overlaps that space makes the engine report
    /// "request 7 is already in flight" for a request the peer never sent twice.
    /// The two id spaces share one JSON-RPC channel, so they have to be
    /// disjoint even though nothing else forces them to be.
    fn register(&self, sender: oneshot::Sender<Answer>) -> i64 {
        let mut table = self.lock();
        let id = table.next_id.fetch_add(1, Ordering::Relaxed);
        table.waiters.insert(id, sender);
        id
    }

    /// Hands a peer's answer to whoever is waiting for `id`.
    ///
    /// `false` when the id is unknown — a reply to a request that already
    /// completed, was refused, or never existed. All three are the same from
    /// here and all three are correctly dropped.
    pub fn resolve(&self, id: i64, answer: Answer) -> bool {
        let Some(waiter) = self.lock().waiters.remove(&id) else {
            return false;
        };
        waiter.send(answer).is_ok()
    }

    /// How many requests are outstanding.
    ///
    /// For a diagnostics line and a test. Taking the lock is why this is not on
    /// any per-frame path.
    #[must_use]
    pub fn outstanding(&self) -> usize {
        self.lock().waiters.len()
    }

    /// The lock, recovered from a poisoned one.
    ///
    /// Recovered rather than propagated: the values held here are channels
    /// whose only failure mode is "the peer never answered", and a panic in an
    /// unrelated task must not turn one dropped connection into a refusal of
    /// every subsequent request on the session.
    fn lock(&self) -> MutexGuard<'_, Table> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The exchange handed to a session's [`PeerSession`](crate::application::server::context::PeerSession).
///
/// Holds the table it records into and the stream it writes to. Two handles to
/// one session, kept together so a request and the answer that completes it
/// cannot end up on different sessions.
#[derive(Clone)]
pub struct HttpExchange {
    /// Where in-flight requests are recorded.
    table: ExchangeTable,
    /// Writes a request onto the session's event stream.
    publish: Publisher,
}

/// How a request reaches the session's stream. A closure rather than a tail
/// because the legacy transport's stream carries a different frame type, and
/// the rendezvous with the answer is the same for both.
type Publisher = std::sync::Arc<dyn Fn(Value) + Send + Sync>;

impl core::fmt::Debug for HttpExchange {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("HttpExchange")
            .field("table", &self.table)
            .finish_non_exhaustive()
    }
}

impl HttpExchange {
    /// An exchange publishing into `tail` and recording into `table`.
    #[must_use]
    pub fn new(table: ExchangeTable, tail: Tail) -> Self {
        Self::publishing(table, move |request| {
            tail.push(request);
        })
    }

    /// An exchange that hands each request to `publish` and records into
    /// `table`.
    #[must_use]
    pub fn publishing(
        table: ExchangeTable,
        publish: impl Fn(Value) + Send + Sync + 'static,
    ) -> Self {
        Self {
            table,
            publish: std::sync::Arc::new(publish),
        }
    }

    /// The table this session's answers are routed back through.
    #[must_use]
    pub const fn table(&self) -> &ExchangeTable {
        &self.table
    }
}

impl CallbackExchange for HttpExchange {
    /// Publishes a server-initiated request and waits for its answer.
    ///
    /// The wait is the part a `POST`-shaped transport makes awkward: the
    /// request goes out on the stream the *current* request opened, and the
    /// answer comes back on a later `POST` this future does not own. The table
    /// is the rendezvous — this side parks on a oneshot, and
    /// [`ExchangeTable::resolve`] completes it from whichever task happens to be
    /// reading the peer's next request.
    async fn exchange(
        &self,
        method: &str,
        params: &crate::domain::ir::JsonObject,
    ) -> Result<crate::domain::ir::JsonObject, ExchangeError> {
        // Asked before anything is pushed or waited on. Pushing first would
        // leave a request in the ring that the peer never sees and a wait on a
        // channel that can only be closed by the session ending, which reads
        // from the outside as a server that stopped responding. Refusing here
        // gives the tool an error it can put in its result, so the peer is
        // told "this cannot be asked here" instead of waiting for an answer
        // that was never on its way.
        if !self.table.is_readable() {
            return Err(ExchangeError::new(
                "this server answers with a single JSON document, so a tool \
                 cannot ask the peer a question and wait: set \
                 Streaming::EventStream to offer elicitation or sampling",
            ));
        }
        let (sender, receiver) = oneshot::channel();
        let id = self.table.register(sender);

        let mut request = serde_json::Map::new();
        request.insert("jsonrpc".to_owned(), Value::from("2.0"));
        request.insert("id".to_owned(), Value::from(id));
        request.insert("method".to_owned(), Value::from(method));
        // `params` is copied through verbatim: it was built by the caller as a
        // `JsonObject`, so `as_str` is already the member text and there is
        // one parse of it in the process rather than a round trip through
        // `Value` that would only be re-parsed on the way out.
        request.insert(
            "params".to_owned(),
            serde_json::from_str::<Value>(params.as_str())
                .map_err(|_| ExchangeError::new("the request's params were not JSON"))?,
        );
        // A `push` cannot fail — it evicts the oldest event if the ring is
        // full — so there is no refusal to write here and inventing one would
        // be a failure this transport cannot actually have.
        (self.publish)(Value::Object(request));

        // A dropped waiter means the session ended under the tool, which the
        // tool sees as a refusal it can handle — so it is named rather than
        // left as a bare channel error.
        receiver
            .await
            .map_err(|_| ExchangeError::new("the session ended before the peer answered"))?
    }
}

#[cfg(test)]
mod tests {
    use super::{ExchangeTable, HttpExchange};
    use crate::adapters::primary::transport::http::stream::EventTail;
    use crate::application::server::context::CallbackExchange;
    use crate::domain::ir::JsonObject;

    type TestError = Box<dyn std::error::Error>;

    /// A ring big enough for the handful of events these tests push.
    fn capacity() -> Result<std::num::NonZeroUsize, TestError> {
        Ok(4.try_into()?)
    }

    /// An unreadable exchange refuses, and the refusal names the setting that
    /// would make it possible.
    ///
    /// The wording is the point as much as the refusal. A tool that cannot ask
    /// its question reports the reason back to whoever called it, and
    /// "something went wrong" sends that caller looking in the wrong place —
    /// the tool, the client, the peer — when the answer is one setting on one
    /// server.
    #[tokio::test]
    async fn an_unreadable_exchange_names_the_setting() -> Result<(), TestError> {
        let exchange = HttpExchange::new(ExchangeTable::new(), EventTail::new(capacity()?));
        let params = JsonObject::new("{}")?;

        let Err(error) = exchange.exchange("elicitation/create", &params).await else {
            return Err("an exchange with no reader was answered".into());
        };
        let message = error.to_string();
        if !message.contains("EventStream") {
            return Err(format!("the refusal does not name the setting: {message}").into());
        }
        Ok(())
    }

    /// Nothing is pushed and nothing is left waiting when the exchange is
    /// refused.
    ///
    /// The refusal has to happen *before* the register-and-push, or the
    /// request sits in the ring the peer will never read and the table holds a
    /// waiter nothing can complete — the same leak, one step later.
    #[tokio::test]
    async fn a_refused_exchange_leaves_nothing_in_flight() -> Result<(), TestError> {
        let table = ExchangeTable::new();
        let exchange = HttpExchange::new(table.clone(), EventTail::new(capacity()?));
        let params = JsonObject::new("{}")?;

        let _ = exchange.exchange("elicitation/create", &params).await;

        if table.outstanding() != 0 {
            return Err("a refused exchange left a waiter behind".into());
        }
        Ok(())
    }

    /// A readable exchange still registers a waiter, so the refusal above is
    /// about the mode and not about the plumbing being broken.
    #[tokio::test]
    async fn a_readable_exchange_registers_a_waiter() -> Result<(), TestError> {
        let table = ExchangeTable::readable();
        let exchange = HttpExchange::new(table.clone(), EventTail::new(capacity()?));
        let params = JsonObject::new("{}")?;

        // The exchange is left pending rather than awaited: resolving it needs
        // the peer's answer, which is the point of the table.
        let pending = exchange.exchange("elicitation/create", &params);
        tokio::pin!(pending);
        let _ = tokio::time::timeout(std::time::Duration::from_millis(50), &mut pending).await;

        if table.outstanding() != 1 {
            return Err("a readable exchange registered no waiter".into());
        }

        // A second question is asked only to prove the first one is still
        // outstanding: two waiters means the table did not answer the first
        // from its own ring. It is polled rather than awaited — an exchange
        // resolves when the *peer* answers, and nothing here plays the peer,
        // so awaiting it is a hang rather than an assertion.
        let second = exchange.exchange("elicitation/create", &params);
        tokio::pin!(second);
        let answered =
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut second).await;
        if answered.is_ok() {
            return Err("an exchange answered without the peer".into());
        }
        if table.outstanding() != 2 {
            return Err("the second exchange registered no waiter of its own".into());
        }
        Ok(())
    }

    /// The default is unreadable, which is the direction that fails safe.
    #[test]
    fn the_default_table_is_unreadable() {
        assert!(!ExchangeTable::new().is_readable());
        assert!(ExchangeTable::readable().is_readable());
    }
}
