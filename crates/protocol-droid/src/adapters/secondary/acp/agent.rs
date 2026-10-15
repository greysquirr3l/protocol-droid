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

use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use super::error::AcpPeerError;

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
    /// The writer task's input. The
    /// application hands a fully-serialised
    /// envelope to the channel; the writer
    /// writes the bytes.
    write_tx: mpsc::Sender<Vec<u8>>,
}

impl std::fmt::Debug for AcpAgentPeer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The `mpsc::Sender` does not
        // implement `Debug`; format the
        // peer by hand, omitting the
        // channels.
        formatter.debug_struct("AcpAgentPeer").finish_non_exhaustive()
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

        // The reader task. Owns the
        // agent's stdin. Reads one line
        // at a time, parses, and pushes
        // the request to the application
        // channel. The line is silently
        // dropped on parse error (a
        // misbehaving client cannot
        // crash the agent's main loop).
        tokio::spawn(spawn_reader(BufReader::new(stdin), request_tx));

        // The writer task. Owns the
        // agent's stdout. Receives
        // pre-serialised bytes and
        // writes them with a trailing
        // `\n`.
        tokio::spawn(spawn_writer(stdout, write_rx));

        Self {
            request_rx,
            write_tx,
        }
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

    /// Send a success reply for a previously
    /// received request.
    ///
    /// The wire form is
    /// `{ "jsonrpc": "2.0", "id": <id>,
    /// "result": <result> }`. The `id` is
    /// the value the request carried; the
    /// peer passes it through unchanged.
    pub async fn send_reply(
        &self,
        id: Value,
        result: Value,
    ) -> Result<(), AcpPeerError> {
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
        let params_value: Value =
            serde_json::to_value(&params).map_err(AcpPeerError::Decode)?;
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

async fn spawn_reader<R>(
    mut reader: BufReader<R>,
    request_tx: mpsc::Sender<AgentRequest>,
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
        let id = value.get("id").cloned().unwrap_or(Value::Null);
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

#[cfg(test)]
mod tests {
    use super::*;
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
        client_end.write_all(request_bytes).await.map_err(AcpPeerError::Write)?;

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
        let value: Value =
            serde_json::from_str(&line).map_err(AcpPeerError::Decode)?;
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
    async fn a_notification_written_by_the_agent_peer_omits_the_id() -> Result<(), AcpPeerError>
    {
        let (client_end, agent_end) = duplex(4096);
        let (agent_read, agent_write) = tokio::io::split(agent_end);
        let peer = AcpAgentPeer::from_stdio(agent_read, agent_write);

        peer.send_notification("session/update", serde_json::json!({"foo": "bar"}))
            .await?;

        let line = read_line_from(BufReader::new(client_end)).await?;
        let value: Value =
            serde_json::from_str(&line).map_err(AcpPeerError::Decode)?;
        assert!(value.get("id").is_none(), "the notification has no `id` field");
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
    async fn an_error_reply_written_by_the_agent_peer_round_trips() -> Result<(), AcpPeerError>
    {
        let (client_end, agent_end) = duplex(4096);
        let (agent_read, agent_write) = tokio::io::split(agent_end);
        let peer = AcpAgentPeer::from_stdio(agent_read, agent_write);

        peer.send_error(serde_json::json!(3), -32601, "Method not found")
            .await?;

        let line = read_line_from(BufReader::new(client_end)).await?;
        let value: Value =
            serde_json::from_str(&line).map_err(AcpPeerError::Decode)?;
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
    async fn read_line_from<R>(
        mut reader: BufReader<R>,
    ) -> Result<String, AcpPeerError>
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
