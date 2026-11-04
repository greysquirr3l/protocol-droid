//! `AcpAgentPeer` — the agent-side transport for ACP.
//!
//! The agent process receives JSON-RPC 2.0
//! requests from the client on its stdin and
//! sends replies (and notifications) on its
//! stdout. The peer is the inverse of
//! [`AcpStdioPeer`](super::peer::AcpStdioPeer): the
//! client peer writes to the agent's stdin
//! and reads from the agent's stdout; the agent
//! peer reads from its own stdin and writes to
//! its own stdout.
//!
//! # The wire
//!
//! Same shape as the client side: newline-
//! delimited JSON-RPC 2.0. A request is
//! `{ "jsonrpc": "2.0", "id": N, "method": ...,
//! "params": ... }`; a reply is `{ "jsonrpc":
//! "2.0", "id": N, "result": ... }` or
//! `{ "jsonrpc": "2.0", "id": N, "error": {...} }`.
//! A notification is a request without an `id`.
//!
//! # The peer
//!
//! Like the client peer, the agent peer is two
//! halves:
//!
//! - A **reader task**, spawned at construction,
//!   that owns the agent's stdin and routes
//!   incoming envelopes to the application
//!   via an mpsc channel.
//! - A **writer task**, also spawned at
//!   construction, that owns the agent's
//!   stdout. The application hands a
//!   serialised envelope to the peer; the
//!   peer hands the bytes to the writer.
//!
//! Splitting the two halves means the
//! application can hold a `&AcpAgentPeer` and
//! call `send_reply` from any task without
//! re-entering the same lock that the reader
//! is holding.
//!
//! # Both directions
//!
//! ACP is bidirectional: the editor calls the
//! agent, and the agent calls the editor back.
//! The eleven `clientMethods` (`fs/read_text_file`,
//! `terminal/create`, `session/request_permission`,
//! ...) are the agent calling the editor, and an
//! agent that cannot call them cannot ask the
//! user for permission or read a file.
//!
//! So the agent peer speaks both directions:
//! [`recv_request`](Self::recv_request) takes the
//! editor's calls, and
//! [`send_request`](Self::send_request) issues the
//! agent's own, awaiting the editor's reply.
//! The reply arrives on the same reader task, so
//! the peer keeps a pending-reply table keyed by
//! the request id — the same shape the client
//! peer uses for its own outbound requests.
//!
//! # Why a generic `from_stdio` and not a
//! `spawn` builder
//!
//! The client peer owns the lifecycle of the
//! child process — it spawns the agent and
//! kills it on drop. The agent peer is
//! the other half: the agent process is
//! already running (it is the process the
//! client spawned), and the agent peer's job
//! is to talk to its own stdin / stdout. A
//! `spawn` builder would be a synonym for
//! "wire the peer's halves to my own pipes";
//! a generic `from_stdio` is the seam a test
//! (or a server that embeds the peer) uses to
//! hand in any `AsyncRead + AsyncWrite` pair.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc, oneshot};

use super::error::AcpPeerError;
use super::reply::AcpReply;
use crate::domain::acp::method::AcpMethod;

/// The pending-reply table the agent peer keeps for
/// its own outbound requests.
///
/// Keyed by the request id the peer allocated; the
/// value is the oneshot the caller's
/// `send_request` future is awaiting.
type PendingReplies = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<AcpReply, AcpPeerError>>>>>;

/// The agent's outbound half: the ability to write
/// envelopes to the editor.
///
/// Split out of [`AcpAgentPeer`] because the two
/// halves want opposite ownership. The peer owns the
/// *inbound* side and is consumed by the receive
/// loop; the outbound side has to be callable from
/// inside a handler, which runs while the receive
/// loop is parked awaiting the next request. Cloning
/// the handle is what lets a handler call the editor
/// back without borrowing the peer the loop is
/// holding.
///
/// Every method takes `&self`, so the handle can be
/// shared across tasks without a lock.
#[derive(Clone)]
pub struct AcpAgentSender {
    /// The writer task's input. The
    /// application hands a fully-serialised
    /// envelope to the channel; the writer
    /// writes the bytes.
    write_tx: mpsc::Sender<Vec<u8>>,
    /// The pending-reply table for the agent's
    /// own outbound requests. Shared with the
    /// reader task, which routes replies back
    /// through it.
    pending: PendingReplies,
    /// The next request id the peer allocates
    /// for its own outbound requests.
    ///
    /// The agent's ids and the editor's ids are
    /// independent counters, so they can collide:
    /// the editor may send a request with id `1`
    /// while the agent's own outbound request
    /// also carries id `1`.
    ///
    /// The reader task tells the two apart by the
    /// envelope's shape, not by the id: a reply
    /// carries no `method`, a request always does.
    /// The id space therefore does not have to be
    /// shared or partitioned, and a collision is
    /// harmless.
    next_id: Arc<AtomicU64>,
}

/// A JSON-RPC 2.0 request the agent receives
/// from the client.
///
/// The wire form is `{ jsonrpc, id, method,
/// params }`. The peer decodes the envelope
/// and hands the application the typed shape;
/// the application decodes the `params` into
/// the concrete request type the method
/// expects (`InitializeRequest`,
/// `NewSessionRequest`, etc.).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRequest {
    /// The JSON-RPC 2.0 `id`. The application
    /// passes it back unchanged in the reply
    /// envelope so the client can correlate
    /// the reply with the request. The id is
    /// a `serde_json::Value` because the wire
    /// spec allows string, number, or null;
    /// the agent peer passes it through
    /// verbatim.
    pub id: Value,
    /// The method name. The schema's method
    /// names are dotted slugs
    /// (`session/new`, `session/prompt`); the
    /// application routes by this string.
    pub method: String,
    /// The raw params. The application decodes
    /// into the typed request for the
    /// method; the peer does not attempt
    /// any routing.
    pub params: Value,
}

/// The agent-side transport for ACP.
///
/// Constructed via [`AcpAgentPeer::from_stdio`].
/// The peer owns two tasks (the reader and
/// the writer); dropping the peer stops the
/// tasks. The agent process's own lifecycle
/// is independent — the agent process is
/// not owned by this peer.
pub struct AcpAgentPeer {
    /// The reader task's output. The
    /// application awaits the next
    /// [`AgentRequest`] with
    /// [`recv_request`][Self::recv_request].
    request_rx: mpsc::Receiver<AgentRequest>,
    /// The agent's outbound half. The peer
    /// keeps a copy so the application can
    /// reply to the editor without going
    /// through the sender handle, and the
    /// handler can take a clone of it to call
    /// the editor back.
    sender: AcpAgentSender,
}

impl std::fmt::Debug for AcpAgentPeer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The `mpsc::Sender` does not
        // implement `Debug`; format the
        // peer by hand, omitting the
        // channels.
        formatter
            .debug_struct("AcpAgentPeer")
            .field("next_id", &self.sender.next_id)
            .finish_non_exhaustive()
    }
}

async fn spawn_reader<R>(
    mut reader: BufReader<R>,
    request_tx: mpsc::Sender<AgentRequest>,
    pending: PendingReplies,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let trimmed = line.trim_end_matches('\n');
        if trimmed.is_empty() {
            continue;
        }
        // A request carries an `id`; a
        // notification does not. The agent
        // peer hands the application only
        // requests (the application is the
        // one that wants the correlation);
        // notifications from the client
        // (e.g. `session/cancel`) are
        // silently dropped today. A future
        // increment exposes them through a
        // second channel.
        let value: Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if value.get("id").is_none() {
            // Notification. Not yet
            // surfaced to the
            // application.
            continue;
        }
        // An envelope with an `id` is either a
        // reply to one of the agent's own
        // outbound requests or a request the
        // editor sent.
        //
        // The two are told apart by the
        // envelope's shape: a reply carries a
        // `result` or an `error` and no
        // `method`, a request always carries a
        // `method`. The id is not part of the
        // test — the agent's ids and the
        // editor's ids are independent counters,
        // so they can collide, and the shape
        // rule holds either way.
        let id = value.get("id").cloned().unwrap_or(Value::Null);
        let is_reply = value.get("method").is_none();
        if is_reply {
            // A reply to the agent's own request.
            // `id` is the agent's own counter, so
            // it is a number; anything else is not
            // a reply to anything the agent sent.
            let Some(reply_id) = id.as_u64() else {
                continue;
            };
            let Some(entry) = pending.lock().await.remove(&reply_id) else {
                // A reply to an id the agent never
                // sent, or a duplicate reply to one
                // already answered. Nothing to
                // complete.
                continue;
            };
            let result = decode_reply(&value);
            let _ = entry.send(result);
            continue;
        }
        let method = value
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let params = value.get("params").cloned().unwrap_or(Value::Null);
        let request = AgentRequest { id, method, params };
        if request_tx.send(request).await.is_err() {
            // The application dropped
            // the receiver; the agent
            // is shutting down.
            return;
        }
    }
}

/// Decode a JSON-RPC 2.0 reply envelope into the
/// `Result` the pending sender expects.
///
/// A reply carries exactly one of `result` or
/// `error`. A JSON-RPC error is a
/// `Err(AcpPeerError::JsonRpc)`; a success is
/// `Ok(AcpReply)`. An envelope with neither is
/// ill-shaped.
fn decode_reply(value: &Value) -> Result<AcpReply, AcpPeerError> {
    if let Some(error) = value.get("error") {
        let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        return Err(AcpPeerError::JsonRpc { code, message });
    }
    let Some(result) = value.get("result") else {
        return Err(AcpPeerError::IllShapedReply(
            "the reply carried neither a `result` nor an `error`".to_owned(),
        ));
    };
    Ok(AcpReply {
        jsonrpc: value
            .get("jsonrpc")
            .and_then(Value::as_str)
            .unwrap_or("2.0")
            .to_owned(),
        id: value.get("id").cloned().unwrap_or(Value::Null),
        result: result.clone(),
    })
}

async fn spawn_writer<W>(mut stdout: W, mut write_rx: mpsc::Receiver<Vec<u8>>)
where
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    while let Some(bytes) = write_rx.recv().await {
        if stdout.write_all(&bytes).await.is_err() {
            return;
        }
        if stdout.flush().await.is_err() {
            return;
        }
    }
}

fn io_other(message: &str) -> std::io::Error {
    std::io::Error::other(message)
}

impl std::fmt::Debug for AcpAgentSender {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The `mpsc::Sender` does not implement
        // `Debug`; format the sender by hand,
        // omitting the channel.
        formatter
            .debug_struct("AcpAgentSender")
            .field("next_id", &self.next_id)
            .finish_non_exhaustive()
    }
}

impl AcpAgentSender {
    /// Send a request to the editor and await its
    /// reply.
    ///
    /// This is the agent calling the editor — the
    /// reverse of the peer's receive path. The eleven
    /// `clientMethods` are all issued this way:
    /// `session/request_permission` asks the user's
    /// editor whether a tool call may proceed,
    /// `fs/read_text_file` asks for a file's contents,
    /// `terminal/create` asks for a terminal.
    ///
    /// The wire form is
    /// `{ "jsonrpc": "2.0", "id": <id>,
    /// "method": ..., "params": ... }`. The peer
    /// allocates the id, registers it in the
    /// pending-reply table, and waits for the
    /// editor's reply to arrive on the reader task.
    ///
    /// A reply carrying a JSON-RPC `error` is
    /// returned as [`AcpPeerError::JsonRpc`]; a
    /// successful reply is returned as an
    /// [`AcpReply`]. The reply's `result` is the
    /// raw JSON the editor sent — the caller
    /// decodes it into the method's response type.
    ///
    /// # Errors
    ///
    /// - [`AcpPeerError::Decode`] — the params did
    ///   not serialise, or the reply's `result` did
    ///   not decode into the expected shape.
    /// - [`AcpPeerError::Write`] — the writer task
    ///   is gone, so the request never left the
    ///   agent.
    /// - [`AcpPeerError::JsonRpc`] — the editor
    ///   answered with a JSON-RPC error.
    /// - [`AcpPeerError::IllShapedReply`] — the
    ///   editor's reply carried neither a `result`
    ///   nor an `error`.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use protocol_droid::adapters::secondary::acp::AcpAgentPeer;
    /// use protocol_droid::domain::acp::method::{
    ///     AcpMethod, ClientMethod,
    /// };
    ///
    /// # async fn ask(peer: &AcpAgentPeer) -> Result<(), Box<dyn std::error::Error>> {
    /// let reply = peer
    ///     .send_request(
    ///         AcpMethod::Client(ClientMethod::FsReadTextFile),
    ///         serde_json::json!({"path": "/tmp/x"}),
    ///     )
    ///     .await?;
    /// # let _ = reply;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send_request<P>(
        &self,
        method: AcpMethod,
        params: P,
    ) -> Result<AcpReply, AcpPeerError>
    where
        P: Serialize,
    {
        let params_value: Value = serde_json::to_value(&params).map_err(AcpPeerError::Decode)?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let envelope = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method.as_wire_name(),
            "params": params_value,
        });
        let bytes = serde_json::to_vec(&envelope).map_err(AcpPeerError::Decode)?;
        let mut framed = bytes;
        framed.push(b'\n');

        // Register the pending reply BEFORE the
        // envelope hits the wire. A fast editor
        // can answer before `write_tx.send`
        // returns, and a reply that arrives
        // before the id is in the table would
        // find nothing to complete.
        let (reply_tx, reply_rx) = oneshot::channel();
        self.pending.lock().await.insert(id, reply_tx);

        if self.write_tx.send(framed).await.is_err() {
            // The writer task is gone. Drop the
            // pending entry so the table does not
            // grow with a request that can never
            // be answered.
            self.pending.lock().await.remove(&id);
            return Err(AcpPeerError::Write(io_other("the writer task is gone")));
        }

        // The reader task dropped the pending
        // entry without answering: the editor
        // closed the connection.
        reply_rx.await.unwrap_or_else(|_| {
            Err(AcpPeerError::Write(io_other(
                "the reader task closed before the editor replied",
            )))
        })
    }

    /// Send a typed request to the editor and
    /// await its typed reply.
    ///
    /// The typed form of [`send_request`](Self::send_request):
    /// the params serialise from `Req` and the
    /// reply's `result` decodes into `Resp`.
    ///
    /// # Errors
    ///
    /// The same set as [`send_request`](Self::send_request),
    /// plus a decode failure when the editor's
    /// `result` does not match `Resp`.
    pub async fn send_typed_request<Req, Resp>(
        &self,
        method: AcpMethod,
        params: Req,
    ) -> Result<Resp, AcpPeerError>
    where
        Req: Serialize,
        Resp: serde::de::DeserializeOwned,
    {
        let reply = self.send_request(method, params).await?;
        serde_json::from_value(reply.result).map_err(AcpPeerError::Decode)
    }

    /// Send a success reply for a previously
    /// received request.
    ///
    /// The wire form is
    /// `{ "jsonrpc": "2.0", "id": <id>,
    /// "result": <result> }`. The `id` is
    /// the value the request carried; the
    /// peer passes it through unchanged.
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

    /// Send a JSON-RPC 2.0 error reply for a
    /// previously received request. The
    /// `code` and `message` are the
    /// JSON-RPC error code and human-readable
    /// message; the wire form is
    /// `{ "jsonrpc": "2.0", "id": <id>,
    /// "error": { "code": <code>, "message":
    /// <message> } }`.
    pub async fn send_error(
        &self,
        id: Value,
        code: i64,
        message: impl Into<String>,
    ) -> Result<(), AcpPeerError> {
        let envelope = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message.into() },
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

    /// Send a notification to the client.
    /// The wire form is `{ "jsonrpc": "2.0",
    /// "method": ..., "params": ... }` with
    /// no `id` field. The schema's
    /// `session/update` is the canonical
    /// example.
    pub async fn send_notification<Params>(
        &self,
        method: &str,
        params: Params,
    ) -> Result<(), AcpPeerError>
    where
        Params: Serialize,
    {
        let params_value: Value = serde_json::to_value(&params).map_err(AcpPeerError::Decode)?;
        let envelope = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
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
}

impl AcpAgentPeer {
    /// Construct an agent peer from a stdin
    /// reader and a stdout writer. The
    /// generic parameters let the test suite
    /// pass `tokio::io::DuplexStream` halves
    /// instead of a real `ChildStdin` /
    /// `ChildStdout`.
    ///
    /// The peer spawns a reader task and a
    /// writer task; the application
    /// communicates with the peer through
    /// the mpsc channels the reader and
    /// writer tasks own. The half moves
    /// happen at construction time; the
    /// returned peer is `Send + 'static`.
    pub fn from_stdio<R, W>(stdin: R, stdout: W) -> Self
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
        W: tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let (request_tx, request_rx) = mpsc::channel::<AgentRequest>(32);
        let (write_tx, write_rx) = mpsc::channel::<Vec<u8>>(32);
        let pending: PendingReplies = Arc::new(Mutex::new(HashMap::new()));
        let sender = AcpAgentSender {
            write_tx,
            pending: Arc::clone(&pending),
            next_id: Arc::new(AtomicU64::new(1)),
        };

        // The reader task. Owns the
        // agent's stdin. Reads one line
        // at a time, parses, and pushes
        // the request to the application
        // channel. The line is silently
        // dropped on parse error (a
        // misbehaving client cannot
        // crash the agent's main loop).
        tokio::spawn(spawn_reader(
            BufReader::new(stdin),
            request_tx,
            Arc::clone(&pending),
        ));

        // The writer task. Owns the
        // agent's stdout. Receives
        // pre-serialised bytes and
        // writes them with a trailing
        // `\n`.
        tokio::spawn(spawn_writer(stdout, write_rx));

        Self { request_rx, sender }
    }

    /// Await the next [`AgentRequest`] the
    /// client sent.
    ///
    /// Returns `None` when the reader task
    /// is gone (the client closed its end
    /// of the wire, or the pipe broke).
    /// The application is expected to
    /// shut down its handler loop on
    /// `None`.
    pub async fn recv_request(&mut self) -> Option<AgentRequest> {
        self.request_rx.recv().await
    }

    /// Send a request to the editor and await its
    /// reply.
    ///
    /// This is the agent calling the editor — the
    /// reverse of [`recv_request`](Self::recv_request).
    /// The eleven `clientMethods` are all issued
    /// this way: `session/request_permission` asks
    /// the user's editor whether a tool call may
    /// proceed, `fs/read_text_file` asks for a file's
    /// contents, `terminal/create` asks for a
    /// terminal.
    ///
    /// The wire form is
    /// `{ "jsonrpc": "2.0", "id": <id>,
    /// "method": ..., "params": ... }`. The peer
    /// allocates the id, registers it in the
    /// pending-reply table, and waits for the
    /// editor's reply to arrive on the reader task.
    ///
    /// A reply carrying a JSON-RPC `error` is
    /// returned as [`AcpPeerError::JsonRpc`]; a
    /// successful reply is returned as an
    /// [`AcpReply`]. The reply's `result` is the
    /// raw JSON the editor sent — the caller
    /// decodes it into the method's response type.
    ///
    /// # Errors
    ///
    /// - [`AcpPeerError::Decode`] — the params did
    ///   not serialise, or the reply's `result` did
    ///   not decode into the expected shape.
    /// - [`AcpPeerError::Write`] — the writer task
    ///   is gone, so the request never left the
    ///   agent.
    /// - [`AcpPeerError::JsonRpc`] — the editor
    ///   answered with a JSON-RPC error.
    /// - [`AcpPeerError::IllShapedReply`] — the
    ///   editor's reply carried neither a `result`
    ///   nor an `error`.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use protocol_droid::adapters::secondary::acp::AcpAgentPeer;
    /// use protocol_droid::domain::acp::method::{AcpMethod, ClientMethod};
    /// use serde_json::json;
    ///
    /// # async fn ask(peer: &AcpAgentPeer) -> Result<(), Box<dyn std::error::Error>> {
    /// let reply = peer
    ///     .send_request(
    ///         AcpMethod::Client(ClientMethod::FsReadTextFile),
    ///         json!({ "path": "/tmp/notes.txt" }),
    ///     )
    ///     .await?;
    /// println!("contents: {}", reply.result);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// The agent's outbound half.
    ///
    /// The handle is what lets a handler call the
    /// editor back. A consumer's handler runs
    /// while the serving loop is parked awaiting
    /// the next request, so the handler cannot
    /// borrow the peer the loop holds — but it
    /// can hold a clone of this handle, and every
    /// method on it takes `&self`.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use protocol_droid::adapters::secondary::acp::AcpAgentPeer;
    ///
    /// # fn split(peer: &AcpAgentPeer) {
    /// let sender = peer.sender();
    /// # }
    /// ```
    pub fn sender(&self) -> AcpAgentSender {
        self.sender.clone()
    }

    /// Send a success reply for a previously
    /// received request.
    ///
    /// A convenience wrapper over
    /// [`AcpAgentSender::send_reply`].
    pub async fn send_reply(&self, id: Value, result: Value) -> Result<(), AcpPeerError> {
        self.sender.send_reply(id, result).await
    }

    /// Send a JSON-RPC 2.0 error reply for a
    /// previously received request.
    ///
    /// A convenience wrapper over
    /// [`AcpAgentSender::send_error`].
    pub async fn send_error(
        &self,
        id: Value,
        code: i64,
        message: impl Into<String>,
    ) -> Result<(), AcpPeerError> {
        self.sender.send_error(id, code, message).await
    }

    /// Send a notification to the editor.
    ///
    /// A convenience wrapper over
    /// [`AcpAgentSender::send_notification`].
    pub async fn send_notification<Params>(
        &self,
        method: &str,
        params: Params,
    ) -> Result<(), AcpPeerError>
    where
        Params: Serialize,
    {
        self.sender.send_notification(method, params).await
    }

    /// Send a request to the editor and await its
    /// reply.
    ///
    /// A convenience wrapper over
    /// [`AcpAgentSender::send_request`].
    pub async fn send_request<P>(
        &self,
        method: AcpMethod,
        params: P,
    ) -> Result<AcpReply, AcpPeerError>
    where
        P: Serialize,
    {
        self.sender.send_request(method, params).await
    }

    /// Send a typed request to the editor and
    /// await its typed reply.
    ///
    /// A convenience wrapper over
    /// [`AcpAgentSender::send_typed_request`].
    pub async fn send_typed_request<Req, Resp>(
        &self,
        method: AcpMethod,
        params: Req,
    ) -> Result<Resp, AcpPeerError>
    where
        Req: Serialize,
        Resp: serde::de::DeserializeOwned,
    {
        self.sender.send_typed_request(method, params).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::acp::method::ClientMethod;
    use tokio::io::AsyncBufReadExt;
    use tokio::io::BufReader;
    use tokio::io::duplex;

    /// An agent peer wired to a duplex
    /// pair reads a request the test
    /// wrote to the other end. The test
    /// asserts the request decodes to
    /// the expected shape.
    #[tokio::test]
    async fn a_request_arrives_at_the_agent_peer() -> Result<(), AcpPeerError> {
        let (mut client_end, agent_end) = duplex(4096);
        let (agent_read, agent_write) = tokio::io::split(agent_end);
        let mut peer = AcpAgentPeer::from_stdio(agent_read, agent_write);

        // The "client" writes a request
        // to its end of the duplex; the
        // agent peer should surface it.
        let request_bytes = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"foo\":\"bar\"}}\n";
        client_end
            .write_all(request_bytes)
            .await
            .map_err(AcpPeerError::Write)?;

        let request = peer
            .recv_request()
            .await
            .ok_or_else(|| AcpPeerError::Write(io_other("no request arrived")))?;

        assert_eq!(request.id, serde_json::json!(1));
        assert_eq!(request.method, "initialize");
        assert_eq!(request.params, serde_json::json!({"foo": "bar"}));

        drop(peer);
        drop(client_end);
        Ok(())
    }

    /// A reply the agent sends is the
    /// test reads from the other end of
    /// the duplex. The test asserts the
    /// wire form is well-formed and
    /// carries the request's `id`.
    #[tokio::test]
    async fn a_reply_written_by_the_agent_peer_round_trips() -> Result<(), AcpPeerError> {
        let (client_end, agent_end) = duplex(4096);
        let (agent_read, agent_write) = tokio::io::split(agent_end);
        let peer = AcpAgentPeer::from_stdio(agent_read, agent_write);

        peer.send_reply(serde_json::json!(7), serde_json::json!({"answer": 42}))
            .await?;

        let line = read_line_from(BufReader::new(client_end)).await?;
        let value: Value = serde_json::from_str(&line).map_err(AcpPeerError::Decode)?;
        assert_eq!(value.get("jsonrpc"), Some(&Value::String("2.0".to_owned())));
        assert_eq!(value.get("id"), Some(&Value::from(7u64)));
        assert_eq!(
            value.get("result"),
            Some(&Value::Object(
                std::iter::once(("answer".to_owned(), Value::from(42u64))).collect()
            ))
        );

        drop(peer);
        Ok(())
    }

    /// A notification the agent sends
    /// has no `id` field on the wire.
    /// The test asserts the wire form
    /// is `{ jsonrpc, method, params }`
    /// only.
    #[tokio::test]
    async fn a_notification_written_by_the_agent_peer_omits_the_id() -> Result<(), AcpPeerError> {
        let (client_end, agent_end) = duplex(4096);
        let (agent_read, agent_write) = tokio::io::split(agent_end);
        let peer = AcpAgentPeer::from_stdio(agent_read, agent_write);

        peer.send_notification("session/update", serde_json::json!({"foo": "bar"}))
            .await?;

        let line = read_line_from(BufReader::new(client_end)).await?;
        let value: Value = serde_json::from_str(&line).map_err(AcpPeerError::Decode)?;
        assert!(
            value.get("id").is_none(),
            "the notification has no `id` field"
        );
        assert_eq!(
            value.get("method"),
            Some(&Value::String("session/update".to_owned()))
        );

        drop(peer);
        Ok(())
    }

    /// A JSON-RPC error reply the agent
    /// sends carries the error code and
    /// message. The test asserts the
    /// wire form is `{ jsonrpc, id,
    /// error: { code, message } }`.
    #[tokio::test]
    async fn an_error_reply_written_by_the_agent_peer_round_trips() -> Result<(), AcpPeerError> {
        let (client_end, agent_end) = duplex(4096);
        let (agent_read, agent_write) = tokio::io::split(agent_end);
        let peer = AcpAgentPeer::from_stdio(agent_read, agent_write);

        peer.send_error(serde_json::json!(3), -32601, "Method not found")
            .await?;

        let line = read_line_from(BufReader::new(client_end)).await?;
        let value: Value = serde_json::from_str(&line).map_err(AcpPeerError::Decode)?;
        assert_eq!(value.get("id"), Some(&Value::from(3u64)));
        let error = value
            .get("error")
            .ok_or_else(|| AcpPeerError::Write(io_other("missing `error` field")))?;
        assert_eq!(error.get("code"), Some(&Value::from(-32601)));
        assert_eq!(
            error.get("message"),
            Some(&Value::String("Method not found".to_owned()))
        );

        drop(peer);
        Ok(())
    }

    /// The agent calling the editor back is
    /// the half of ACP that was missing. The
    /// test drives the full round trip: the
    /// agent issues a `session/request_permission`
    /// request, the "editor" reads it off
    /// the wire, answers it, and the agent's
    /// `send_request` future resolves with the
    /// reply.
    ///
    /// The test asserts the wire form the editor
    /// sees, because a round trip that passes
    /// with a mangled wire form would still
    /// "work" against a peer that ignored it.
    #[tokio::test]
    async fn the_agent_can_call_the_editor_back_and_get_a_reply() -> Result<(), AcpPeerError> {
        let (client_end, agent_end) = duplex(4096);
        let (client_read, mut client_write) = tokio::io::split(client_end);
        let (agent_read, agent_write) = tokio::io::split(agent_end);
        let peer = AcpAgentPeer::from_stdio(agent_read, agent_write);

        // The editor side. It reads the agent's
        // request, answers it, and the answer
        // travels back down the same duplex.
        let editor = tokio::spawn(async move {
            let mut reader = BufReader::new(client_read);
            let line = read_line_from_mut(&mut reader).await?;
            let request: Value = serde_json::from_str(&line).map_err(AcpPeerError::Decode)?;
            let id = request.get("id").cloned().unwrap_or(Value::Null);
            let reply = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "outcome": { "outcome": "selected", "optionId": "allow" } },
            });
            let mut framed = serde_json::to_vec(&reply).map_err(AcpPeerError::Decode)?;
            framed.push(b'\n');
            client_write
                .write_all(&framed)
                .await
                .map_err(AcpPeerError::Write)?;
            Ok::<Value, AcpPeerError>(request)
        });

        let reply = peer
            .send_request(
                AcpMethod::Client(ClientMethod::SessionRequestPermission),
                serde_json::json!({
                    "sessionId": "session-1",
                    "toolCall": { "toolCallId": "call-1", "title": "write the file" },
                    "options": [ { "optionId": "allow", "name": "Allow", "kind": "allow_once" } ],
                }),
            )
            .await?;

        // What the editor saw on the wire.
        let request = editor
            .await
            .map_err(|_| AcpPeerError::Write(io_other("the editor task panicked")))??;
        assert_eq!(
            request.get("jsonrpc"),
            Some(&Value::String("2.0".to_owned()))
        );
        assert_eq!(
            request.get("method"),
            Some(&Value::String("session/request_permission".to_owned()))
        );
        assert!(request.get("id").is_some(), "the request carries an `id`");
        assert_eq!(
            request
                .get("params")
                .and_then(|params| params.get("sessionId")),
            Some(&Value::String("session-1".to_owned()))
        );

        // What the agent got back.
        assert_eq!(
            reply
                .result
                .get("outcome")
                .and_then(|outcome| outcome.get("optionId")),
            Some(&Value::String("allow".to_owned()))
        );

        drop(peer);
        Ok(())
    }

    /// A JSON-RPC error reply to the agent's
    /// own request is an error, not a result.
    /// The test asserts the code and message
    /// survive to the caller — the agent
    /// cannot answer a permission prompt it
    /// was refused.
    #[tokio::test]
    async fn a_json_rpc_error_to_the_agents_request_is_an_error() -> Result<(), AcpPeerError> {
        let (client_end, agent_end) = duplex(4096);
        let (client_read, mut client_write) = tokio::io::split(client_end);
        let (agent_read, agent_write) = tokio::io::split(agent_end);
        let peer = AcpAgentPeer::from_stdio(agent_read, agent_write);

        let editor = tokio::spawn(async move {
            let mut reader = BufReader::new(client_read);
            let line = read_line_from_mut(&mut reader).await?;
            let request: Value = serde_json::from_str(&line).map_err(AcpPeerError::Decode)?;
            let id = request.get("id").cloned().unwrap_or(Value::Null);
            let reply = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": "Method not found" },
            });
            let mut framed = serde_json::to_vec(&reply).map_err(AcpPeerError::Decode)?;
            framed.push(b'\n');
            client_write
                .write_all(&framed)
                .await
                .map_err(AcpPeerError::Write)?;
            Ok::<(), AcpPeerError>(())
        });

        let result = peer
            .send_request(
                AcpMethod::Client(ClientMethod::SessionRequestPermission),
                serde_json::json!({}),
            )
            .await;

        editor
            .await
            .map_err(|_| AcpPeerError::Write(io_other("the editor task panicked")))??;

        match result {
            Err(AcpPeerError::JsonRpc { code, message }) => {
                assert_eq!(code, -32601);
                assert_eq!(message, "Method not found");
            }
            other => {
                return Err(AcpPeerError::Write(io_other(&format!(
                    "expected a JSON-RPC error, got {other:?}"
                ))));
            }
        }

        drop(peer);
        Ok(())
    }

    /// The editor's requests and the agent's
    /// own share one id space, and both count
    /// from 1. The test asserts the peer tells
    /// them apart: the editor's request with
    /// id 1 arrives as a request, and the
    /// agent's request with id 1 is answered
    /// by the reply the editor sends.
    ///
    /// Without the distinction the reply would
    /// be handed to the application as a
    /// request, and the agent's own `send_request`
    /// would hang.
    #[tokio::test]
    async fn an_editor_request_and_an_agent_request_can_share_an_id() -> Result<(), AcpPeerError> {
        let (client_end, agent_end) = duplex(4096);
        let (client_read, client_write) = tokio::io::split(client_end);
        // The editor writes from its own task and
        // the test body writes the editor's request,
        // so the write half is shared.
        let client_write = Arc::new(Mutex::new(client_write));
        let editor_write = Arc::clone(&client_write);
        let (agent_read, agent_write) = tokio::io::split(agent_end);
        let mut peer = AcpAgentPeer::from_stdio(agent_read, agent_write);

        let editor = tokio::spawn(async move {
            let mut reader = BufReader::new(client_read);
            // The agent's request, id 1 — the
            // agent's writes are what the editor
            // reads.
            let line = read_line_from_mut(&mut reader).await?;
            let request: Value = serde_json::from_str(&line).map_err(AcpPeerError::Decode)?;
            let id = request.get("id").cloned().unwrap_or(Value::Null);
            let reply = serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": { "ok": true } });
            let mut framed = serde_json::to_vec(&reply).map_err(AcpPeerError::Decode)?;
            framed.push(b'\n');
            editor_write
                .lock()
                .await
                .write_all(&framed)
                .await
                .map_err(AcpPeerError::Write)?;
            Ok::<(), AcpPeerError>(())
        });

        // The editor's request arrives as a request.
        let editor_request = serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}
        });
        let mut framed = serde_json::to_vec(&editor_request).map_err(AcpPeerError::Decode)?;
        framed.push(b'\n');
        client_write
            .lock()
            .await
            .write_all(&framed)
            .await
            .map_err(AcpPeerError::Write)?;

        let request = peer
            .recv_request()
            .await
            .ok_or_else(|| AcpPeerError::Write(io_other("no request arrived")))?;
        assert_eq!(request.method, "initialize");

        // The agent's own request, same id, is
        // answered and does not surface as a
        // request.
        let reply = peer
            .send_request(
                AcpMethod::Client(ClientMethod::FsReadTextFile),
                serde_json::json!({ "path": "/tmp/x" }),
            )
            .await?;
        assert_eq!(reply.result.get("ok"), Some(&Value::Bool(true)));

        editor
            .await
            .map_err(|_| AcpPeerError::Write(io_other("the editor task panicked")))??;

        drop(peer);
        Ok(())
    }

    /// A cloned sender allocates its own request ids.
    ///
    /// The serving adapter hands the handler a clone
    /// of the sender, so the handler's outbound
    /// requests and the adapter's own must not share
    /// an id — the pending-reply table is keyed by
    /// id, and two requests with the same id would
    /// mean one silently overwrites the other.
    #[tokio::test]
    async fn a_cloned_sender_allocates_its_own_request_ids() -> Result<(), AcpPeerError> {
        let (client_end, agent_end) = duplex(4096);
        let (client_read, client_write) = tokio::io::split(client_end);
        let (agent_read, agent_write) = tokio::io::split(agent_end);
        let peer = AcpAgentPeer::from_stdio(agent_read, agent_write);

        let first = peer.sender();
        let second = peer.sender();

        // The peer sends a request, so its id and the
        // clone's id have to differ.
        let client_write = client_write;
        let peer_request = tokio::spawn(async move {
            let mut reader = BufReader::new(client_read);
            let first_line = read_line_from_mut(&mut reader).await?;
            let second_line = read_line_from_mut(&mut reader).await?;
            Ok::<(String, String), AcpPeerError>((first_line, second_line))
        });

        // The two senders each ask the editor for a
        // file. The sends are spawned because
        // `send_request` waits for a reply and no
        // reply ever arrives — the test is about the
        // ids on the wire, not about the replies.
        let first_send = tokio::spawn(async move {
            let _ = first
                .send_request(
                    AcpMethod::Client(ClientMethod::FsReadTextFile),
                    serde_json::json!({"path": "/tmp/one"}),
                )
                .await;
        });
        let second_send = tokio::spawn(async move {
            let _ = second
                .send_request(
                    AcpMethod::Client(ClientMethod::FsReadTextFile),
                    serde_json::json!({"path": "/tmp/two"}),
                )
                .await;
        });

        let (first_line, second_line) = peer_request
            .await
            .map_err(|error| AcpPeerError::Write(std::io::Error::other(error.to_string())))??;

        let first_id = serde_json::from_str::<Value>(&first_line)
            .map_err(AcpPeerError::Decode)?
            .get("id")
            .cloned()
            .unwrap_or(Value::Null);
        let second_id = serde_json::from_str::<Value>(&second_line)
            .map_err(AcpPeerError::Decode)?
            .get("id")
            .cloned()
            .unwrap_or(Value::Null);

        assert!(
            first_id != second_id,
            "two senders must not allocate the same id: {first_id} and {second_id}"
        );

        // The two sends are still waiting for a reply
        // that will never come. Abort them so the
        // test does not leave tasks running.
        first_send.abort();
        second_send.abort();

        drop(client_write);
        Ok(())
    }

    /// Read one `\n`-terminated line from
    /// the buffered reader. The helper
    /// exists so the wire-shape tests can
    /// consume the agent peer's output
    /// without depending on
    /// `tokio::io::AsyncReadExt::read_buf`'s
    /// "may return short" semantics. The
    /// trailing newline is stripped so the
    /// caller parses the JSON object
    /// directly.
    async fn read_line_from<R>(mut reader: BufReader<R>) -> Result<String, AcpPeerError>
    where
        R: tokio::io::AsyncRead + Unpin + Send,
    {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .map_err(AcpPeerError::Write)?;
        Ok(line.trim_end_matches('\n').to_owned())
    }

    /// Read one line from a reader the caller
    /// keeps. The `read_line_from` helper takes
    /// the reader by value, but a test that
    /// reads several lines off one duplex needs
    /// to hold on to it between reads.
    async fn read_line_from_mut<R>(reader: &mut BufReader<R>) -> Result<String, AcpPeerError>
    where
        R: tokio::io::AsyncRead + Unpin + Send,
    {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .map_err(AcpPeerError::Write)?;
        Ok(line.trim_end_matches('\n').to_owned())
    }
}
