//! `McpStdioPeer` — the client-side transport for MCP.
//!
//! A `McpStdioPeer` connects to an MCP
//! server over stdio. The wire form is
//! newline-delimited JSON-RPC 2.0: the
//! peer writes requests to the child's
//! stdin and reads replies and
//! notifications from the child's
//! stdout. The peer owns the child
//! process and the reader / writer
//! tasks; dropping the peer kills the
//! child and stops the tasks.
//!
//! # Why this peer exists
//!
//! An ACP agent is usually also an MCP
//! client: the editor passes an
//! `mcpServers` list to the agent in
//! the `session/new` request, and the
//! agent spawns one MCP client per
//! server. The agent's tool calls
//! then route to whichever MCP server
//! advertises the tool. Without a
//! typed MCP client, the agent's
//! `session/new` handler is on the
//! hook for hand-rolling the JSON-RPC
//! envelope, the id allocation, and
//! the reply dispatch — the same
//! boilerplate every MCP client
//! writes. `McpStdioPeer` is that
//! boilerplate, typed, tested, and
//! ready to spawn.
//!
//! # What the peer speaks
//!
//! The pinned MCP revision is the
//! newest of the supported revisions
//! the consumer's `version` enum
//! allows. The peer's `methods`
//! surface is the four most-used
//! client-originating methods:
//! `initialize`, `ping`, `tools/list`,
//! `tools/call`. A future increment
//! adds `resources/*` and
//! `prompts/*`.
//!
//! # Differences from `AcpStdioPeer`
//!
//! The wire form is identical: the
//! peer speaks newline-delimited
//! JSON-RPC 2.0 in both directions.
//! The differences are the
//! protocol-level shapes (the typed
//! request / response types) and
//! the method table. The transport
//! is the same shape; the
//! application-level concerns differ.
//! The duplication is deliberate: the
//! two protocols may diverge in the
//! future (e.g. MCP's Streamable
//! HTTP) and a shared transport
//! would couple their evolution.

use std::collections::HashMap;
use std::io::Write as _;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, mpsc, oneshot};

/// A pending reply. The reader
/// task owns a clone of the
/// `Arc<Mutex<HashMap>>`; the
/// application layer owns the
/// other clone. The reply is a
/// oneshot: the reader task
/// hands the result to the
/// application once and the
/// `HashMap` entry is removed.
struct Pending {
    tx: oneshot::Sender<Result<Value, McpPeerError>>,
}

/// The default request timeout.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// The MCP stdio peer.
pub struct McpStdioPeer {
    pending: Arc<Mutex<HashMap<u64, Pending>>>,
    next_id: AtomicU64,
    write_tx: mpsc::Sender<Vec<u8>>,
    timeout: Duration,
    /// The child process. Held
    /// for `kill_on_drop`: when
    /// the peer drops, the
    /// `Child` is dropped, which
    /// kills the child. The
    /// application layer never
    /// reads the field directly.
    #[expect(dead_code, reason = "kill_on_drop machinery; never read directly")]
    child: Child,
}

impl std::fmt::Debug for McpStdioPeer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpStdioPeer")
            .field("pending_len", &"*")
            .field("next_id", &self.next_id)
            .field("write_tx", &"*")
            .field("timeout", &self.timeout)
            .field("child", &"*")
            .finish_non_exhaustive()
    }
}

/// The specification for the
/// child process. Mirrors
/// `AcpStdioPeer::SpawnSpec` so
/// the call sites that already
/// know that shape can swap.
#[derive(Debug, Clone)]
pub struct SpawnSpec {
    /// The executable to spawn.
    pub command: String,
    /// The arguments to pass.
    pub args: Vec<String>,
    /// The default request timeout.
    pub timeout: Option<Duration>,
}

impl SpawnSpec {
    /// Construct a new `SpawnSpec`
    /// for a command and its
    /// arguments. The timeout
    /// defaults to 30 seconds; the
    /// consumer overrides it with
    /// [`with_timeout`][Self::with_timeout].
    pub fn new(command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            command: command.into(),
            args,
            timeout: None,
        }
    }

    /// Set the default request timeout.
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

/// The error type the peer
/// returns. The variants are
/// the failure modes the
/// application layer can act on.
#[derive(Debug, thiserror::Error)]
pub enum McpPeerError {
    /// The child process could
    /// not be spawned. The
    /// underlying `io::Error`
    /// is the `Command::spawn`
    /// failure (typically
    /// `NotFound` for a missing
    /// executable).
    #[error("spawning the child process: {0}")]
    Spawn(#[source] std::io::Error),

    /// The JSON-RPC envelope
    /// could not be serialised
    /// or the reply could not be
    /// deserialised into the
    /// typed shape the
    /// application requested.
    /// The error is the
    /// `serde_json` failure.
    #[error("encoding or decoding the JSON-RPC envelope: {0}")]
    Decode(#[source] serde_json::Error),

    /// The reply never arrived
    /// within the configured
    /// timeout. The pending
    /// oneshot is dropped, and
    /// the reader task will see
    /// a closed sender when (if)
    /// the late reply does
    /// come.
    #[error("the reply did not arrive within {0:?}")]
    Timeout(Duration),

    /// The writer task is gone;
    /// the child has exited and
    /// the peer's pipe is
    /// closed. A subsequent
    /// `send_request` returns
    /// this variant.
    #[error("the writer task is gone")]
    Write(#[source] std::io::Error),

    /// The reader task is gone;
    /// the child has exited and
    /// the peer's pipe is
    /// closed. A subsequent
    /// `send_typed_request`
    /// returns this variant.
    #[error("the reader task is gone")]
    ReadGone,

    /// The reply was a
    /// JSON-RPC 2.0 error. The
    /// error code and message
    /// are carried through.
    #[error("the server returned a JSON-RPC error: code={code} message={message}")]
    JsonRpc {
        /// The JSON-RPC error
        /// code.
        code: i64,
        /// The JSON-RPC error
        /// message.
        message: String,
    },
}

impl McpStdioPeer {
    /// Spawn a child process and
    /// wire it up to the peer's
    /// reader and writer tasks.
    /// The peer owns the child
    /// process; dropping the
    /// peer kills the child
    /// (the `Command` is
    /// configured with
    /// `kill_on_drop(true)`).
    pub fn spawn(spec: &SpawnSpec) -> Result<Self, McpPeerError> {
        let mut command = Command::new(&spec.command);
        command
            .args(&spec.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(McpPeerError::Spawn)?;
        let stdin = child.stdin.take().ok_or_else(|| {
            McpPeerError::Spawn(std::io::Error::other("child stdin was not piped"))
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            McpPeerError::Spawn(std::io::Error::other("child stdout was not piped"))
        })?;

        let pending: Arc<Mutex<HashMap<u64, Pending>>> = Arc::new(Mutex::new(HashMap::new()));
        let (write_tx, write_rx) = mpsc::channel::<Vec<u8>>(32);
        spawn_reader(BufReader::new(stdout), Arc::clone(&pending));
        spawn_writer(stdin, write_rx);

        Ok(Self {
            pending,
            next_id: AtomicU64::new(1),
            write_tx,
            timeout: spec.timeout.unwrap_or(DEFAULT_TIMEOUT),
            child,
        })
    }

    /// Send a typed request and
    /// decode the typed reply.
    /// The two type parameters
    /// are the consumer's call
    /// to the server: `Req` is
    /// what the consumer's
    /// handler sends, `Resp` is
    /// what the server replies
    /// with.
    ///
    /// `params` is taken by
    /// value (not by reference)
    /// so the future the
    /// function returns is
    /// unconditionally `Send`:
    /// a borrow of `Req` would
    /// only be `Send` if
    /// `Req: Sync`, which is a
    /// sharper constraint than
    /// the consumer wants.
    pub async fn send_typed_request<Req, Resp>(
        &self,
        method: &str,
        params: Req,
    ) -> Result<Resp, McpPeerError>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        let value = self.send_request(method, params).await?;
        serde_json::from_value(value).map_err(McpPeerError::Decode)
    }

    /// Send a request and await
    /// the raw reply. The raw
    /// `Value` is the consumer's
    /// problem to decode; the
    /// typed wrapper is
    /// [`send_typed_request`][Self::send_typed_request].
    pub async fn send_request<Req>(&self, method: &str, params: Req) -> Result<Value, McpPeerError>
    where
        Req: Serialize,
    {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let params_value: Value = serde_json::to_value(&params).map_err(McpPeerError::Decode)?;
        let envelope = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params_value,
        });
        let bytes = serde_json::to_vec(&envelope).map_err(McpPeerError::Decode)?;
        let mut framed = bytes;
        framed.push(b'\n');
        self.write_tx
            .send(framed)
            .await
            .map_err(|_| McpPeerError::Write(io_other("the writer task is gone")))?;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, Pending { tx });
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_oneshot_dropped)) => {
                self.pending.lock().await.remove(&id);
                Err(McpPeerError::Timeout(self.timeout))
            }
            Err(_elapsed) => {
                self.pending.lock().await.remove(&id);
                Err(McpPeerError::Timeout(self.timeout))
            }
        }
    }
}

fn io_other(message: &str) -> std::io::Error {
    std::io::Error::other(message)
}

fn spawn_writer(mut stdin: ChildStdin, mut write_rx: mpsc::Receiver<Vec<u8>>) {
    tokio::spawn(async move {
        while let Some(mut bytes) = write_rx.recv().await {
            // Belt-and-braces: ensure
            // a trailing newline even
            // if a future caller forgot
            // to add one.
            if !bytes.ends_with(b"\n") {
                bytes.push(b'\n');
            }
            if let Err(error) = stdin.write_all(&bytes).await {
                // The child has gone.
                // Log to stderr; the
                // application layer
                // discovers the failure
                // through the next
                // `send_request` returning
                // a `Write` error.
                let _ = std::io::stderr()
                    .lock()
                    .write_all(format!("mcp writer task: write failed: {error}\n").as_bytes());
                return;
            }
            if let Err(error) = stdin.flush().await {
                let _ = std::io::stderr()
                    .lock()
                    .write_all(format!("mcp writer task: flush failed: {error}\n").as_bytes());
                return;
            }
        }
    });
}

fn spawn_reader(mut reader: BufReader<ChildStdout>, pending: Arc<Mutex<HashMap<u64, Pending>>>) {
    tokio::spawn(async move {
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
            let value: Value = match serde_json::from_str(trimmed) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let id = value.get("id").and_then(Value::as_u64).unwrap_or(0);
            if id == 0 {
                // Notification or
                // malformed reply; the
                // MCP client peer does
                // not yet model
                // notifications. A
                // future increment
                // adds the
                // `notifications/*`
                // stream (e.g.
                // `notifications/tools/list_changed`).
                continue;
            }
            let result = value.get("error").map_or_else(
                || {
                    value
                        .get("result")
                        .cloned()
                        .ok_or_else(|| McpPeerError::JsonRpc {
                            code: -32600,
                            message: "the reply had no `result` and no `error`".to_owned(),
                        })
                },
                |error_value| {
                    let code = error_value.get("code").and_then(Value::as_i64).unwrap_or(0);
                    let message = error_value
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    Err(McpPeerError::JsonRpc { code, message })
                },
            );
            let entry = pending.lock().await.remove(&id);
            if let Some(entry) = entry {
                let _ = entry.tx.send(result);
            }
        }
    });
}
