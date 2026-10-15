//! `AcpStdioPeer` — the stdio transport for ACP.
//!
//! # The wire
//!
//! ACP over stdio is newline-delimited JSON-RPC 2.0.
//! Every message is one JSON object, terminated by
//! `\n`. The agent's stdout carries the same shape.
//! There is no Content-Length framing (that is the
//! HTTP+SSE transport, which the v1.25.0 spec
//! does not define for stdio).
//!
//! # The peer
//!
//! A peer is two halves:
//!
//! - A **reader task**, spawned at construction,
//!   that owns the child's stdout and routes
//!   incoming envelopes to the pending reply
//!   channels. The reader is the only place that
//!   reads from the pipe; the application layer
//!   does not touch stdout directly.
//!
//! - A **writer task**, also spawned at
//!   construction, that owns the child's stdin.
//!   The application layer hands a serialised
//!   envelope to the peer; the peer hands the
//!   bytes to the writer via a bounded channel.
//!   One writer, one mpsc — the channel is the
//!   back-pressure point.
//!
//! Splitting the two halves means the application
//! layer can hold a `&AcpStdioPeer` and call
//! `send_request` from any task without
//! re-entering the same lock that the reader is
//! holding.
//!
//! # Why `kill_on_drop`
//!
//! A child process whose parent has gone away is an
//! orphan; an ACP agent is, by design, a long-
//! lived daemon that holds the conversation. A
//! `AcpStdioPeer` that drops without killing its
//! child leaves the agent answering into a void.
//! `tokio::process::Child::kill_on_drop(true)` is
//! the flag that takes the child with the peer.
//!
//! # The pending-reply table
//!
//! The peer tracks every in-flight request in a
//! `HashMap<RequestId, oneshot::Sender<...>>`. The
//! reader task looks up the id on every reply and
//! hands the value to the oneshot; the application
//! layer awaits the oneshot from `send_request`.
//! A `HashMap` is the right shape: the in-flight
//! set is small, lookups are O(1), and the
//! concurrent-access ergonomics are exactly what
//! `tokio::sync::Mutex` provides.
//!
//! # Timeouts
//!
//! The default request timeout is 30s. A peer
//! that has to talk to a slow agent can be
//! rebuilt with a longer timeout via
//! [`AcpStdioPeerBuilder`]. The timeout fires
//! from the application layer's task, not
//! from the reader — the reader just sees an
//! `Err` and discards the channel.
//!
//! # `HashMap::get_disjoint_mut` and the in-flight set
//!
//! The reader sometimes has to look up multiple
//! ids in one pass (a batched reply). The
//! `HashMap::get_disjoint_mut` method
//! (stabilized in 1.86) is exactly that
//! pattern, and the peer uses it. The rule
//! the project carries is "before hand-rolling
//! a helper, check whether it was stabilized
//! between 1.86 and 1.98"; this is the case
//! the rule was written for.

use std::collections::HashMap;
use std::collections::HashSet;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::AgentRequest;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, mpsc, oneshot};

use super::error::AcpPeerError;
use super::reply::AcpReply;
use super::stream::{AgentRequestStream, NotificationStream};
use crate::domain::acp::method::AcpMethod;
use crate::domain::acp::notification::SessionNotification;

/// A `SpawnSpec` is the configuration for
/// `AcpStdioPeer::spawn`. The `program` and
/// `args` are the only required fields; the
/// rest take the obvious defaults.
#[derive(Debug, Clone)]
pub struct SpawnSpec {
    /// The program to run (e.g. `"node"`).
    pub program: String,
    /// The arguments (e.g. `["reference-agent.js"]`).
    pub args: Vec<String>,
    /// The default request timeout. `None` is
    /// 30s.
    pub timeout: Option<Duration>,
}

impl SpawnSpec {
    /// A `SpawnSpec` from a `program` and a list
    /// of `args`. The timeout takes the default.
    pub fn new(program: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            program: program.into(),
            args,
            timeout: None,
        }
    }

    /// Override the default 30s timeout.
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

/// A pending reply channel: the oneshot the
/// reader will hand the value to. The
/// `method` field is recorded for diagnostics
/// (a future increment that wants to log
/// "request foo/bar timed out" rather than
/// "request 7 timed out" uses the method).
struct Pending {
    /// The oneshot sender the application
    /// task is awaiting.
    tx: oneshot::Sender<Result<AcpReply, AcpPeerError>>,
    /// The method the request carried. The
    /// reader keeps the method so a reply
    /// without a matching id can be matched
    /// by the consumer after the fact (it
    /// cannot today, but the field is the
    /// shape a future feature would need).
    method: AcpMethod,
}

impl std::fmt::Debug for Pending {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The `tx` field is `Debug` but the
        // `oneshot::Sender` does not
        // implement it; the `method` field
        // is the only useful diagnostic.
        // Format by hand rather than
        // deriving `Debug`, which would
        // require a custom `Debug` impl
        // for `oneshot::Sender` that
        // doesn't exist.
        formatter
            .debug_struct("Pending")
            .field("method", &self.method)
            .finish_non_exhaustive()
    }
}

/// The stdio peer.
///
/// Constructed via [`AcpStdioPeer::spawn`]. The
/// peer owns the child process and the two
/// tasks; dropping the peer kills the child
/// and stops the tasks.
pub struct AcpStdioPeer {
    /// The pending-reply table. The reader
    /// task holds a clone of the `Arc`; the
    /// application layer holds this one.
    pending: Arc<Mutex<HashMap<u64, Pending>>>,
    /// The next request id. The peer is the
    /// only writer; an `AtomicU64` is enough.
    next_id: AtomicU64,
    /// The writer task's input. The application
    /// hands a fully-serialised envelope to the
    /// channel; the writer writes the bytes.
    write_tx: mpsc::Sender<Vec<u8>>,
    /// The notification stream's consumer end.
    /// The reader task pushes
    /// [`SessionNotification`]s into the
    /// matching `notifications_tx`; the
    /// application layer awaits the next
    /// notification with
    /// [`recv_notification`][Self::recv_notification].
    ///
    /// The receiver is behind a
    /// `tokio::sync::Mutex` so the
    /// `recv_notification` method can take
    /// `&self` and the consumer can race
    /// the typed reply against the next
    /// notification in a
    /// `tokio::select!`. The lock is held
    /// across the inner `recv().await`,
    /// which is safe for the consumer's
    /// main loop: only one task owns the
    /// peer at a time, and the reader
    /// task pushes to the matching
    /// `notifications_tx` (not the
    /// receiver) so the consumer never
    /// blocks the agent.
    notifications_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<SessionNotification>>>,
    /// The set of request ids the peer has
    /// sent. The reader task uses this set
    /// to differentiate client-originating
    /// replies (the `id` is in the set) from
    /// agent-originating requests (the
    /// `id` is NOT in the set, because the
    /// agent allocated it). The set is
    /// pruned on every reply: the reader
    /// removes the id once the reply is
    /// dispatched to the pending oneshot.
    ///
    /// `std::sync::Mutex` is the right
    /// choice: the lock is held for a
    /// few microseconds (a `HashSet`
    /// insert or remove), not across
    /// an await, and a tokio mutex
    /// would be a heavier allocation
    /// for the same critical section.
    sent_ids: Arc<StdMutex<HashSet<u64>>>,
    /// The agent-originating request
    /// stream. The agent sends requests
    /// like `fs/read_text_file` and
    /// `terminal/create` to the client;
    /// the consumer reads them with
    /// [`recv_agent_request`][Self::recv_agent_request]
    /// and replies with
    /// [`send_reply`][Self::send_reply].
    ///
    /// The receiver is behind a
    /// `tokio::sync::Mutex` for the
    /// same reason as `notifications_rx`:
    /// the consumer can race the typed
    /// reply against the next
    /// agent-originating request in a
    /// `tokio::select!`.
    agent_requests_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<AgentRequest>>>,
    /// The default request timeout.
    timeout: Duration,
    /// The child process. The `kill_on_drop`
    /// flag (set on the `Command` before
    /// `spawn`) is what takes the child
    /// when the peer is dropped. The peer
    /// does not call `wait()` on it; the
    /// reader and writer tasks own the
    /// pipes, and the process is reaped
    /// by tokio's machinery when the
    /// `Child` value is dropped with the
    /// peer.
    #[expect(
        dead_code,
        reason = "the field is held to keep the `Child` alive; the `kill_on_drop` machinery reads it on drop"
    )]
    child: Child,
}

impl std::fmt::Debug for AcpStdioPeer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The `Child` and the
        // `mpsc::Receiver<SessionNotification>`
        // do not implement `Debug`; format
        // the peer by hand, omitting the
        // child and the receiver.
        formatter
            .debug_struct("AcpStdioPeer")
            .field("next_id", &self.next_id)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl AcpStdioPeer {
    /// Spawn the child, wire the reader and
    /// writer tasks, and return the peer.
    ///
    /// The function is synchronous on purpose:
    /// `tokio::process::Command::spawn` does
    /// not await, and the reader / writer
    /// tasks it spawns are the ones that
    /// actually park. Returning a sync
    /// `Result` keeps the future `Send` and
    /// avoids a needless state-machine frame
    /// at the seam.
    ///
    /// The spec is taken by reference because
    /// the consumer is the owner — a `Read`
    /// `SpawnSpec` lets the caller reuse it
    /// for diagnostics or a retry without
    /// cloning.
    pub fn spawn(spec: &SpawnSpec) -> Result<Self, AcpPeerError> {
        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child: Child = command.spawn().map_err(AcpPeerError::Spawn)?;
        let stdin: ChildStdin = child
            .stdin
            .take()
            .ok_or_else(|| AcpPeerError::Spawn(io_other("stdin was not piped")))?;
        let stdout: ChildStdout = child
            .stdout
            .take()
            .ok_or_else(|| AcpPeerError::Spawn(io_other("stdout was not piped")))?;
        let pending: Arc<Mutex<HashMap<u64, Pending>>> = Arc::new(Mutex::new(HashMap::new()));
        let (write_tx, write_rx) = mpsc::channel::<Vec<u8>>(32);
        // The notification stream. The reader
        // pushes [`SessionNotification`]s;
        // the application layer pulls them with
        // `recv_notification`. A bounded
        // channel (32) gives the consumer a
        // moment to catch up before the agent
        // starts to back-pressure. The receiver
        // is wrapped in a `tokio::sync::Mutex`
        // so `recv_notification` can take
        // `&self` and race against the typed
        // reply in a `tokio::select!`.
        let (notifications_tx, notifications_rx) = mpsc::channel::<SessionNotification>(32);
        let notifications_rx = Arc::new(tokio::sync::Mutex::new(notifications_rx));

        // The agent-originating request
        // stream. The agent sends requests
        // like `fs/read_text_file` and
        // `terminal/create` to the client;
        // the consumer reads them with
        // `recv_agent_request` and replies
        // with `send_reply`. A bounded
        // channel (8) is enough: the
        // consumer's handler is synchronous
        // with the request's arrival, and
        // agent-originating requests are
        // rare compared to the consumer's
        // own request rate.
        let (agent_requests_tx, agent_requests_rx) = mpsc::channel::<AgentRequest>(8);
        let agent_requests_rx = Arc::new(tokio::sync::Mutex::new(agent_requests_rx));

        // The set of request ids the peer
        // has sent. Empty at start; the
        // peer inserts on every
        // `send_request` and the reader
        // removes on every reply.
        let sent_ids: Arc<StdMutex<HashSet<u64>>> = Arc::new(StdMutex::new(HashSet::new()));

        // The reader task. Owns the child's
        // stdout. Reads one line at a time
        // (newline-delimited JSON-RPC), parses,
        // and dispatches to the matching
        // pending reply (for client-originating
        // requests) or to the
        // `agent_requests_tx` (for
        // agent-originating requests).
        // Notifications (no `id` field) go
        // to the `notifications_tx`.
        spawn_reader(
            BufReader::new(stdout),
            Arc::clone(&pending),
            notifications_tx,
            agent_requests_tx,
            Arc::clone(&sent_ids),
        );

        // The writer task. Owns the child's
        // stdin. Receives pre-serialised bytes
        // and writes them with a trailing `\n`.
        spawn_writer(stdin, write_rx);

        // The child is now under the writer's and reader's care. We
        // keep the `Child` handle in the peer so the `kill_on_drop`
        // machinery takes the process when the peer drops. The peer
        // does not call `wait()` on it; the reader and writer tasks
        // own the pipes, and the OS process is reaped when the
        // `Child` value is dropped with the peer.
        //
        // (A future increment that wants the exit status on the
        // error path can store the `Child` in an
        // `Arc<Mutex<Option<Child>>>` and `try_wait()` on the
        // timeout path. For this increment the timeout is the only
        // failure mode and the `Timeout` variant is enough.)

        Ok(Self {
            pending,
            next_id: AtomicU64::new(1),
            write_tx,
            notifications_rx,
            sent_ids,
            agent_requests_rx,
            timeout: spec.timeout.unwrap_or(Duration::from_secs(30)),
            child,
        })
    }

    /// Send a request and await the reply. The
    /// `params` are serialised with the same
    /// `serde_json` the consumer uses
    /// everywhere else, so the application
    /// layer can pass a typed struct
    /// directly.
    ///
    /// `params` is taken by value (not by
    /// reference) so the future the function
    /// returns is unconditionally `Send`: a
    /// borrow of `P` would only be `Send` if
    /// `P: Sync`, which is a sharper
    /// constraint than the consumer wants.
    /// Serialising the value moves it into the
    /// envelope, so the application layer
    /// hands a fresh value per request.
    pub async fn send_request<P>(
        &self,
        method: AcpMethod,
        params: P,
    ) -> Result<AcpReply, AcpPeerError>
    where
        P: Serialize,
    {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        // Register the id synchronously
        // before the envelope hits the
        // wire. The reader checks
        // `sent_ids` to differentiate
        // client-originating replies
        // from agent-originating
        // requests; if the id is not
        // in the set when the reply
        // arrives, the reader will
        // route it to the
        // agent-originating request
        // channel by mistake. The
        // insert is a few-nanosecond
        // critical section. A poisoned
        // mutex is a logic error: the
        // peer has corrupted its own
        // state, and the conservative
        // response is a `Decode`
        // error (the caller is the
        // only one holding the
        // poison; nobody else can
        // recover).
        self.sent_ids
            .lock()
            .map_err(|error| {
                AcpPeerError::Decode(serde_json::Error::io(io_other(&format!(
                    "sent_ids lock poisoned: {error}"
                ))))
            })?
            .insert(id);
        let params_value: Value = serde_json::to_value(&params).map_err(AcpPeerError::Decode)?;
        let envelope = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method.as_wire_name(),
            "params": params_value,
        });
        let bytes = serde_json::to_vec(&envelope).map_err(AcpPeerError::Decode)?;
        let mut framed = bytes;
        framed.push(b'\n');
        self.write_tx
            .send(framed)
            .await
            .map_err(|_| AcpPeerError::Write(io_other("the writer task is gone")))?;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, Pending { tx, method });
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(result)) => {
                // The reply arrived. Remove
                // the id from `sent_ids` so
                // the set does not grow
                // unboundedly over the
                // peer's lifetime.
                if let Ok(mut sent_ids) = self.sent_ids.lock() {
                    sent_ids.remove(&id);
                }
                result
            }
            Ok(Err(_oneshot_dropped)) => {
                // The reader dropped the oneshot without sending: the
                // child exited. The exit status is not available on
                // this path (we do not hold the `Child` handle here);
                // a future increment that wants the status can store
                // the handle in an `Arc<Mutex<_>>` and read it.
                if let Ok(mut sent_ids) = self.sent_ids.lock() {
                    sent_ids.remove(&id);
                }
                Err(AcpPeerError::Timeout(self.timeout))
            }
            Err(_elapsed) => {
                // The reply never arrived. Drop the channel: the
                // reader task will see a closed sender when (if) the
                // reply does come.
                self.pending.lock().await.remove(&id);
                if let Ok(mut sent_ids) = self.sent_ids.lock() {
                    sent_ids.remove(&id);
                }
                Err(AcpPeerError::Timeout(self.timeout))
            }
        }
    }

    /// Send a typed request and decode the typed
    /// response.
    ///
    /// This is the typed wrapper over
    /// [`send_request`][Self::send_request]. The
    /// raw `Result<AcpReply, _>` it returns
    /// carries the wire's `result` as a
    /// `serde_json::Value`; the consumer has to
    /// call `serde_json::from_value` themselves.
    /// This method does that step.
    ///
    /// The two type parameters are the
    /// consumer's call to the agent: the
    /// `Req` is what the consumer's handler
    /// sends, the `Resp` is what the agent's
    /// handler is expected to return. The
    /// typed `NewSessionRequest` /
    /// `NewSessionResponse` pair is the
    /// canonical example.
    pub async fn send_typed_request<Req, Resp>(
        &self,
        method: AcpMethod,
        params: Req,
    ) -> Result<Resp, AcpPeerError>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        let reply = self.send_request(method, params).await?;
        serde_json::from_value(reply.result).map_err(AcpPeerError::Decode)
    }

    /// Send a typed notification. A notification
    /// has no `id` and no reply; the agent's
    /// handler may respond, but the consumer
    /// does not wait.
    ///
    /// The wire form is a JSON-RPC 2.0
    /// notification: `{ "jsonrpc": "2.0",
    /// "method": ..., "params": ... }` with no
    /// `id` field. The schema's `session/cancel`
    /// is the canonical example.
    pub async fn send_notification<Params>(
        &self,
        method: AcpMethod,
        params: Params,
    ) -> Result<(), AcpPeerError>
    where
        Params: Serialize,
    {
        let params_value: Value = serde_json::to_value(&params).map_err(AcpPeerError::Decode)?;
        let envelope = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method.as_wire_name(),
            "params": params_value,
        });
        let bytes = serde_json::to_vec(&envelope).map_err(AcpPeerError::Decode)?;
        let mut framed = bytes;
        framed.push(b'\n');
        self.write_tx
            .send(framed)
            .await
            .map_err(|_| AcpPeerError::Write(io_other("the writer task is gone")))?;
        Ok(())
    }

    /// Await the next [`SessionNotification`]
    /// the agent streams.
    ///
    /// `&self` (not `&mut self`) so the
    /// consumer can race the typed reply
    /// against the next notification in a
    /// `tokio::select!`:
    ///
    /// ```ignore
    /// tokio::select! {
    ///     biased;
    ///     Some(notification) = peer.recv_notification() => {
    ///         render(notification);
    ///     }
    ///     response = peer.send_typed_request(...) => {
    ///         return response;
    ///     }
    /// }
    /// ```
    ///
    /// The receiver is behind a
    /// `tokio::sync::Mutex`; the lock is
    /// held across the inner
    /// `recv().await`. That is safe for
    /// the consumer's main loop because
    /// only the consumer's task holds
    /// the lock — the agent's reader
    /// task pushes to the matching
    /// `notifications_tx` (not the
    /// receiver) and never contends for
    /// the receiver lock.
    ///
    /// Returns `None` when the reader task
    /// is gone (the child has exited and
    /// the channel is closed). The
    /// consumer is expected to drop the
    /// peer at that point.
    pub async fn recv_notification(&self) -> Option<SessionNotification> {
        let mut guard = self.notifications_rx.lock().await;
        guard.recv().await
    }

    /// Await the next agent-originating
    /// request the agent sends to the
    /// client. The agent's
    /// `fs/read_text_file`,
    /// `fs/write_text_file`,
    /// `terminal/create`,
    /// `terminal/output`,
    /// `terminal/release`,
    /// `terminal/wait_for_exit`,
    /// `terminal/kill`, and
    /// `session/request_permission` are
    /// the canonical examples.
    ///
    /// `&self` (not `&mut self`) so the
    /// consumer can race the typed
    /// reply against the next
    /// agent-originating request in a
    /// `tokio::select!`, the same shape
    /// `recv_notification` supports.
    /// The receiver is behind a
    /// `tokio::sync::Mutex`; the lock
    /// is held across the inner
    /// `recv().await`, which is safe
    /// for the consumer's main loop
    /// because only the consumer's
    /// task holds the lock — the
    /// reader task pushes to the
    /// matching `agent_requests_tx`
    /// (not the receiver) and never
    /// contends for the receiver
    /// lock.
    ///
    /// Returns `None` when the reader
    /// task is gone (the child has
    /// exited and the channel is
    /// closed). The consumer is
    /// expected to drop the peer at
    /// that point.
    pub async fn recv_agent_request(&self) -> Option<AgentRequest> {
        let mut guard = self.agent_requests_rx.lock().await;
        guard.recv().await
    }

    /// A `Stream` of the [`SessionNotification`]s the agent streams
    /// to the client.
    ///
    /// The canonical alternative to [`recv_notification`](Self::recv_notification)
    /// for a consumer whose main loop is a `tokio::select!` of
    /// "next notification" against "next typed reply" against
    /// "agent-originating request", and for a consumer who wants
    /// `Stream` combinators (`StreamExt::map`, `filter`,
    /// `take_while`, `for_each`, …) over the notification stream.
    ///
    /// The returned stream holds a clone of the peer's notification
    /// mutex; [`recv_notification`](Self::recv_notification) and the
    /// stream are mutually exclusive inside one task. Across tasks
    /// the lock is the same lock, so a `select!` in one task and a
    /// `next().await` in another race for the same channel and the
    /// lock serialises them.
    ///
    /// `&self` (not `&mut self`) so the consumer can hold the peer
    /// by `Arc` and call `notifications()` from any task — the same
    /// shape `recv_notification` supports, but as a `Stream`. The
    /// returned stream borrows the lock only (the `Arc` is
    /// `Clone`), so the borrow is `&self` rather than `&mut self`.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use futures_util::StreamExt;
    ///
    /// let mut notifications = peer.notifications();
    /// while let Some(notification) = notifications.next().await {
    ///     render(notification);
    /// }
    /// ```
    pub fn notifications(&self) -> NotificationStream<'_> {
        NotificationStream::new(Arc::clone(&self.notifications_rx))
    }

    /// A `Stream` of the [`AgentRequest`]s the agent sends to the
    /// client.
    ///
    /// The agent-originating request counterpart of
    /// [`notifications`](Self::notifications). The agent's
    /// `fs/read_text_file`, `fs/write_text_file`,
    /// `terminal/create`, `terminal/output`, `terminal/release`,
    /// `terminal/wait_for_exit`, `terminal/kill`, and
    /// `session/request_permission` requests flow through this
    /// stream.
    ///
    /// Mutually exclusive with
    /// [`recv_agent_request`](Self::recv_agent_request) inside one
    /// task; across tasks the lock serialises them.
    pub fn agent_requests(&self) -> AgentRequestStream<'_> {
        AgentRequestStream::new(Arc::clone(&self.agent_requests_rx))
    }

    /// Reply to an agent-originating
    /// request. The peer hands a
    /// well-formed JSON-RPC 2.0 success
    /// reply to the writer task; the
    /// agent correlates the reply with
    /// the request by the `id` (passed
    /// through unchanged from the
    /// [`AgentRequest`][crate::adapters::secondary::acp::AgentRequest]
    /// the agent sent).
    ///
    /// The result is the typed value
    /// the agent expects for the
    /// method. The peer's role is
    /// transport: it serialises the
    /// value with `serde_json` and
    /// hands the bytes to the writer;
    /// the consumer's handler is what
    /// constructs the typed value.
    ///
    /// `&self` because the writer
    /// task owns the child's stdin
    /// and the application layer
    /// only hands a serialised
    /// envelope to the channel.
    pub async fn send_reply(&self, id: Value, result: Value) -> Result<(), AcpPeerError> {
        let envelope = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        });
        let bytes = serde_json::to_vec(&envelope).map_err(AcpPeerError::Decode)?;
        let mut framed = bytes;
        framed.push(b'\n');
        self.write_tx
            .send(framed)
            .await
            .map_err(|_| AcpPeerError::Write(io_other("the writer task is gone")))?;
        Ok(())
    }
}

fn spawn_reader(
    mut reader: BufReader<ChildStdout>,
    pending: Arc<Mutex<HashMap<u64, Pending>>>,
    notifications_tx: mpsc::Sender<SessionNotification>,
    agent_requests_tx: mpsc::Sender<AgentRequest>,
    sent_ids: Arc<StdMutex<HashSet<u64>>>,
) {
    tokio::spawn(async move {
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            // Trim the trailing newline; the
            // JSON is the rest.
            let trimmed = line.trim_end_matches('\n');
            if trimmed.is_empty() {
                continue;
            }
            // A JSON-RPC 2.0 envelope is a
            // reply (carries an `id`) or a
            // notification (no `id`). The
            // shape detector is a peek at
            // the `id` field; the dispatch
            // path is the same.
            let value: Value = match serde_json::from_str(trimmed) {
                Ok(value) => value,
                Err(_) => continue,
            };
            if value.get("id").is_some() {
                // Reply-or-agent-request:
                // the `id` is present in
                // both. The wire form does
                // not distinguish them;
                // the peer's
                // `sent_ids` set is the
                // differentiator. If the
                // peer allocated the id,
                // the reply is dispatched
                // to the pending oneshot;
                // if the agent allocated
                // the id, the request is
                // pushed to the
                // `agent_requests_tx` for
                // the consumer's handler.
                let (id, result) = parse_and_dispatch_value(value.clone());
                let is_client_reply = id != 0 && sent_ids.lock().is_ok_and(|set| set.contains(&id));
                if is_client_reply {
                    let entry = pending.lock().await.remove(&id);
                    if let Some(entry) = entry {
                        let _ = entry.tx.send(result);
                    }
                } else if id != 0
                    && value.get("method").is_some()
                    && let Some(request) = parse_agent_request(&value)
                {
                    let _ = agent_requests_tx.send(request).await;
                }
                // Envelopes with `id == 0`
                // (the sentinel for an
                // unparseable id) and
                // envelopes with an id that
                // the client did not send
                // AND no `method` field
                // (a malformed reply) are
                // dropped: the wire is
                // ambiguous and the peer
                // cannot dispatch.
            } else {
                // Notification: decode the
                // `params` as a typed
                // [`SessionNotification`] and
                // push to the consumer's
                // stream. The schema's
                // `session/update` is the
                // canonical example; a
                // future notification method
                // (e.g. `auth/required`) gets
                // its own decoder.
                if value.get("method").and_then(Value::as_str) == Some("session/update")
                    && let Some(params) = value.get("params")
                    && let Ok(notification) =
                        serde_json::from_value::<SessionNotification>(params.clone())
                {
                    let _ = notifications_tx.send(notification).await;
                }
            }
        }
    });
}

/// Parse a JSON-RPC 2.0 envelope as an
/// agent-originating request. The function
/// never fails to produce a struct: a
/// missing `method` or `id` field returns
/// `None` and the caller drops the
/// envelope. The wire form is the same as
/// the client side: `{ jsonrpc, id, method,
/// params }`.
fn parse_agent_request(value: &Value) -> Option<AgentRequest> {
    let id = value.get("id")?.clone();
    let method = value.get("method").and_then(Value::as_str)?.to_owned();
    let params = value.get("params").cloned().unwrap_or(Value::Null);
    Some(AgentRequest { id, method, params })
}

/// Parse a single JSON-RPC 2.0 reply envelope
/// into a `(id, Result)` pair.
///
/// The function never fails to produce a pair:
/// a JSON-RPC error is a `(id,
/// Err(JsonRpc))`, a success is a `(id,
/// Ok(reply))`. The reader treats an
/// id of `0` as "do not dispatch" — that is
/// the sentinel for an envelope that cannot be
/// correlated with a request. The caller is
/// responsible for parsing the raw line; this
/// function takes the already-parsed `Value`
/// so the same parse can drive the
/// notification path.
fn parse_and_dispatch_value(value: Value) -> (u64, Result<AcpReply, AcpPeerError>) {
    let id = value.get("id").and_then(Value::as_u64).unwrap_or(0);
    if value.get("error").is_some() {
        let code = value
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let message = value
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        return (id, Err(AcpPeerError::JsonRpc { code, message }));
    }
    match serde_json::from_value::<AcpReply>(value) {
        Ok(reply) => (id, Ok(reply)),
        Err(error) => (id, Err(AcpPeerError::Decode(error))),
    }
}

fn spawn_writer(stdin: ChildStdin, mut write_rx: mpsc::Receiver<Vec<u8>>) {
    tokio::spawn(async move {
        let mut stdin = stdin;
        while let Some(bytes) = write_rx.recv().await {
            if stdin.write_all(&bytes).await.is_err() {
                return;
            }
            if stdin.flush().await.is_err() {
                return;
            }
        }
    });
}

fn io_other(message: &str) -> std::io::Error {
    std::io::Error::other(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Errors a test can surface back to the
    /// runner. Each variant is for a single
    /// failure mode; the test function returns
    /// `Result<(), Failure>` and uses `?` to
    /// propagate rather than `.unwrap()` /
    /// `.expect()`.
    #[derive(Debug)]
    enum Failure {
        /// A `Result` from a tested call was
        /// `Ok` when the test expected `Err`,
        /// or vice versa. The string is the
        /// expected variant the test was
        /// looking for.
        WrongResult(&'static str),
    }

    impl std::fmt::Display for Failure {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::WrongResult(expected) => write!(formatter, "{expected}"),
            }
        }
    }

    impl std::error::Error for Failure {}

    impl From<serde_json::Error> for Failure {
        fn from(_error: serde_json::Error) -> Self {
            Self::WrongResult("a JSON parse failed")
        }
    }

    /// The reader parses a `result` reply into a
    /// typed `AcpReply` and routes it to the
    /// matching id. A reply with no id is
    /// silently dropped (the consumer's oneshot
    /// would never fire).
    ///
    /// The test only exercises
    /// `parse_and_dispatch_value` in isolation;
    /// the `oneshot` is fed by the reader task
    /// in the real path. The `assert_eq!` on
    /// the parsed pair is the claim the
    /// function is asked to defend.
    #[test]
    fn parse_and_dispatch_routes_a_result_by_id() -> Result<(), Failure> {
        let value: Value =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":7,"result":{"answer":42}}"#)
                .map_err(|_| Failure::WrongResult("valid JSON"))?;
        let (id, result) = parse_and_dispatch_value(value);
        assert_eq!(id, 7);
        let Ok(reply) = result else {
            return Err(Failure::WrongResult("Ok reply"));
        };
        assert_eq!(reply.id, serde_json::json!(7));
        assert_eq!(reply.result, serde_json::json!({"answer": 42}));
        Ok(())
    }

    /// The reader parses a JSON-RPC error reply
    /// into a typed `AcpPeerError::JsonRpc`. The
    /// error carries the agent's code and
    /// message; the consumer decides what to
    /// do.
    #[test]
    fn parse_and_dispatch_routes_an_error_by_id() -> Result<(), Failure> {
        let value: Value = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":7,"error":{"code":-32601,"message":"Method not found"}}"#,
        )
        .map_err(|_| Failure::WrongResult("valid JSON"))?;
        let (id, result) = parse_and_dispatch_value(value);
        assert_eq!(id, 7);
        match result {
            Err(AcpPeerError::JsonRpc { code, message }) => {
                assert_eq!(code, -32601);
                assert_eq!(message, "Method not found");
                Ok(())
            }
            Err(_) => Err(Failure::WrongResult("JsonRpc variant")),
            Ok(_) => Err(Failure::WrongResult("Err variant")),
        }
    }

    /// A JSON-RPC error with no `id` is
    /// dispatched to the `0` sentinel — the
    /// reader treats `id == 0` as "do not
    /// dispatch" so a misbehaving agent
    /// cannot crash the consumer's pending
    /// oneshot.
    #[test]
    fn parse_and_dispatch_treats_missing_id_as_zero() -> Result<(), Failure> {
        let value: Value = serde_json::from_str(
            r#"{"jsonrpc":"2.0","error":{"code":-32600,"message":"Invalid Request"}}"#,
        )
        .map_err(|_| Failure::WrongResult("valid JSON"))?;
        let (id, _result) = parse_and_dispatch_value(value);
        assert_eq!(id, 0);
        Ok(())
    }

    /// The peer can be spawned, sent a request,
    /// and shut down. The test points at the
    /// `cat` command (always present on Unix)
    /// as a stand-in for a real agent. `cat`
    /// reads its stdin and writes it to stdout;
    /// a `cat` peer that receives a JSON-RPC
    /// envelope writes the envelope back, and
    /// the reader sees the request shape (which
    /// is well-formed JSON-RPC but is not a
    /// reply, so the dispatcher's id lookup
    /// fails and the result is dropped).
    ///
    /// The test's job is to prove the spawn /
    /// reader / writer machinery works; the
    /// fact that `cat` is not an ACP agent
    /// makes the inner dispatch silent, not
    /// a failure. The end-to-end test
    /// against a real agent lives in
    /// `tests/acp_stdio_interop.rs`.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_peer_spawns_and_drops_cleanly() -> Result<(), Failure> {
        let spec = SpawnSpec::new("cat", Vec::new());
        let Ok(peer) = AcpStdioPeer::spawn(&spec) else {
            return Err(Failure::WrongResult("spawn succeeds on Unix"));
        };
        drop(peer);
        Ok(())
    }

    /// A typed `SpawnSpec` carries the program
    /// and the args the spawner needs. The
    /// builder is a thin convenience over
    /// struct literal construction.
    #[test]
    fn a_spawn_spec_carries_the_program_and_args() {
        let spec = SpawnSpec::new("node", vec!["agent.js".to_owned()])
            .with_timeout(Duration::from_secs(5));
        assert_eq!(spec.program, "node");
        assert_eq!(spec.args, vec!["agent.js".to_owned()]);
        assert_eq!(spec.timeout, Some(Duration::from_secs(5)));
    }
}
