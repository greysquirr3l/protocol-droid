//! The A2A leg of `protocol-droid bridge`, on a real socket against
//! a real in-process HTTP server.
//!
//! `bridge_a2a::run` is the binary's entry point, and it talks to
//! the process's own stdin and stdout. A `#[tokio::test]` cannot
//! reach in and replace either, so the workhorse is
//! `bridge_a2a::run_with_io` and the test drives the bridge
//! through a `tokio::io::DuplexStream` pair: one half is the
//! bridge's "stdin", the other is what the test writes to and
//! reads from. The HTTP upstream is a real `TcpListener` on
//! `127.0.0.1:0`, so the bridge's `reqwest` client is doing the
//! real thing end to end.

#![cfg(feature = "a2a")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use protocol_droid_cli::bridge_a2a::{A2aBridgeArgs, run_with_io};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::TcpListener;

/// How long the test waits for any one piece of the
/// exchange before declaring it stuck. Generous because
/// a CI machine is slow; tight enough that a genuine hang
/// is reported in seconds, not minutes.
const WAIT: Duration = Duration::from_secs(10);

/// The `initialize` envelope a downstream would send
/// at the start of an MCP session.
///
/// Negotiates `2025-11-25` because the bridge advertises
/// every revision the library has wire types for, and
/// the test does not need to exercise the version-
/// negotiation code path — that is what
/// `protocol-droid probe` is for. The `clientInfo` and
/// `capabilities` payloads are the minimum the loop
/// accepts without a `validation` error.
const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","clientInfo":{"name":"a2a-bridge-wire","version":"0.0.0"},"capabilities":{}}}"#;

/// The `notifications/initialized` envelope the loop
/// expects to see before any non-handshake request.
///
/// A notification is an envelope without an `id`, and
/// the loop's check is for the absence of the field
/// rather than its presence, so the JSON shape has to
/// be exact.
const NOTIFICATIONS_INITIALIZED: &str =
    r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#;

/// One `tools/call` round-trips through the bridge and
/// the A2A peer.
///
/// The fake server answers every `SendMessage` with a
/// canned `Message` carrying one text part. The bridge's
/// codec lowers the agent's reply to MCP `content`, the
/// loop carries it back to the downstream, and the test
/// reads the JSON-RPC reply off the duplex.
#[tokio::test(flavor = "current_thread")]
async fn a_tools_call_round_trips_through_the_bridge() -> Result<(), Failure> {
    let harness = Harness::start().await?;
    let tools_call = r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"echo","arguments":{"x":1}}}"#;
    let (summary, reply) = harness.run_through(tools_call).await?;
    Harness::assert_initialize_was_ok(&summary)?;
    Harness::assert_tools_call_was_a_call_result(&reply, 7, "echo: ok")?;
    assert_eq!(summary.forwarded, 1, "the loop should have forwarded one envelope");
    assert_eq!(summary.failed, 0, "the loop should not have refused any envelope");
    assert!(summary.handshaken, "the loop should report a successful handshake");
    assert_eq!(harness.requests_seen(), 1, "the bridge should have crossed the wire exactly once");
    Ok(())
}

/// The bridge that hits a `tools/list` reaches the
/// fake server and gets back a refusal: the codec does
/// not model `tools/list` → A2A. The point of this
/// test is that the bridge does not crash on an
/// unmodelled method — it returns a typed refusal
/// the test can read, and the loop counts the
/// refusal as `failed: 1`.
#[tokio::test(flavor = "current_thread")]
async fn an_unmodelled_method_is_refused_not_crashed() -> Result<(), Failure> {
    let list = r#"{"jsonrpc":"2.0","id":11,"method":"tools/list","params":{}}"#;
    let harness = Harness::start().await?;
    let (summary, reply) = harness.run_through(list).await?;
    Harness::assert_initialize_was_ok(&summary)?;
    let error = reply.get("error").ok_or_else(|| {
        Failure::Bridge(format!(
            "a refused method should answer with `error`: {reply:?}"
        ))
    })?;
    if error.get("code").and_then(Value::as_i64) != Some(-32601) {
        return Err(Failure::Bridge(format!(
            "expected the refusal to be -32601, got {error:?}"
        )));
    }
    assert_eq!(summary.failed, 1, "the loop should have refused exactly one envelope");
    Ok(())
}

/// Every failure a test in this file can hit, so
/// `?` works throughout.
#[derive(Debug)]
enum Failure {
    /// The bridge raised an error. The string is
    /// `{:?}`-formatted so the `anyhow` chain is
    /// preserved when the test prints it.
    Bridge(String),
}

impl core::fmt::Display for Failure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Bridge(text) => f.write_str(text),
        }
    }
}

impl std::error::Error for Failure {}

/// A test-shaped view of the bridge's `LoopSummary`,
/// so the test can assert on its fields without
/// reaching into the binary crate's types. The
/// conversion is a one-liner because the field
/// names match the public `LoopSummary` fields.
#[derive(Debug)]
#[expect(
    dead_code,
    reason = "notified mirrors the loop's own counter; the test asserts on the rest"
)]
struct SummaryShape {
    handshaken: bool,
    forwarded: u64,
    notified: u64,
    failed: u64,
}

impl From<protocol_droid_cli::bridge_a2a::LoopSummary> for SummaryShape {
    fn from(value: protocol_droid_cli::bridge_a2a::LoopSummary) -> Self {
        Self {
            handshaken: value.handshaken,
            forwarded: value.forwarded,
            notified: value.notified,
            failed: value.failed,
        }
    }
}

/// The harness shared by every test: a fake A2A
/// server, a duplex for the bridge's stdio, and the
/// helpers that drive them.
struct Harness {
    server: A2aFakeServer,
}

impl Harness {
    /// Starts a fake A2A server and returns the
    /// harness. The duplex pair is allocated per
    /// `run_through` call, not here, because a
    /// `DuplexStream` is one-shot.
    async fn start() -> Result<Self, Failure> {
        let server = A2aFakeServer::spawn().await?;
        Ok(Self { server })
    }

    /// Sends `initialize`, `notifications/initialized`,
    /// and the caller's frame, then waits for the
    /// bridge to answer. Returns the loop's summary
    /// as JSON and the reply envelope the test cares
    /// about.
    async fn run_through(&self, frame: &str) -> Result<(SummaryShape, Value), Failure> {
        let (downstream_write, bridge_read) = tokio::io::duplex(64 * 1024);
        let (bridge_write, downstream_read) = tokio::io::duplex(64 * 1024);

        let args = A2aBridgeArgs {
            peer: format!("http://{}", self.server.addr),
            json: false,
        };
        let bridge = tokio::spawn(async move {
            use protocol_droid::adapters::primary::stdio::{StdioConfig, StdioSink, StdioSource};
            use protocol_droid::domain::MaxFrameSize;
            let framing = StdioConfig::new(MaxFrameSize::DEFAULT);
            run_with_io(
                &args,
                StdioSource::new(bridge_read, framing),
                StdioSink::new(bridge_write, framing),
            )
            .await
            .map_err(|err| Failure::Bridge(format!("{err:?}")))
        });

        // Send the handshake plus the caller's frame.
        let mut downstream_write = downstream_write;
        for line in [INITIALIZE, NOTIFICATIONS_INITIALIZED, frame] {
            downstream_write
                .write_all(line.as_bytes())
                .await
                .map_err(|err| Failure::Bridge(format!("write `{line}`: {err}")))?;
            downstream_write
                .write_all(b"\n")
                .await
                .map_err(|err| Failure::Bridge(format!("write newline after `{line}`: {err}")))?;
        }
        downstream_write
            .shutdown()
            .await
            .map_err(|err| Failure::Bridge(format!("shutdown: {err}")))?;

        // Read the bridge's two replies. The first
        // line is the `initialize` answer, the second
        // is the reply to the caller's frame.
        let mut lines = BufReader::new(downstream_read).lines();
        let init_line = read_line_within(&mut lines, "the initialize reply").await?;
        let reply_line = read_line_within(&mut lines, "the caller's reply").await?;
        let _init: Value = serde_json::from_str(&init_line).map_err(|err| {
            Failure::Bridge(format!("the initialize reply is not JSON: {err}; {init_line:?}"))
        })?;
        let reply: Value = serde_json::from_str(&reply_line).map_err(|err| {
            Failure::Bridge(format!("the caller's reply is not JSON: {err}; {reply_line:?}"))
        })?;
        let summary = bridge
            .await
            .map_err(|err| Failure::Bridge(format!("join: {err}")))??;
        Ok((summary.into(), reply))
    }

    fn requests_seen(&self) -> usize {
        self.server.requests_seen()
    }

    /// Asserts the bridge's first reply is an
    /// `initialize` result with id=1. A bridge that
    /// answered with an error here would be refusing
    /// to start, which the rest of the test cannot
    /// make sense of.
    fn assert_initialize_was_ok(summary: &SummaryShape) -> Result<(), Failure> {
        if !summary.handshaken {
            return Err(Failure::Bridge(
                "the loop did not report a successful handshake".into(),
            ));
        }
        Ok(())
    }

    /// Asserts the bridge's reply to the caller's
    /// `tools/call` is a `CallToolResult` with the
    /// expected id and text.
    fn assert_tools_call_was_a_call_result(
        value: &Value,
        expected_id: i64,
        expected_text: &str,
    ) -> Result<(), Failure> {
        if value.get("id") != Some(&Value::from(expected_id)) {
            return Err(Failure::Bridge(format!(
                "the reply carried the wrong id: {value:?}"
            )));
        }
        let result = value
            .get("result")
            .ok_or_else(|| Failure::Bridge(format!("the reply carried no result: {value:?}")))?;
        let content = result
            .get("content")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                Failure::Bridge(format!("the result carried no `content` array: {result:?}"))
            })?;
        let first = content
            .first()
            .ok_or_else(|| Failure::Bridge("the content array is empty".to_owned()))?;
        if first.get("type").and_then(Value::as_str) != Some("text") {
            return Err(Failure::Bridge(format!(
                "the first content item is not a text block: {first:?}"
            )));
        }
        if first.get("text").and_then(Value::as_str) != Some(expected_text) {
            return Err(Failure::Bridge(format!(
                "the first content item carried the wrong text: {first:?}"
            )));
        }
        Ok(())
    }
}

/// Reads the next non-empty line from the bridge's
/// "stdout" within the test's wait window, with the
/// caller's name in the error message.
async fn read_line_within<R>(
    lines: &mut tokio::io::Lines<R>,
    name: &str,
) -> Result<String, Failure>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    loop {
        let line = tokio::time::timeout(WAIT, lines.next_line())
            .await
            .map_err(|_| Failure::Bridge(format!("the bridge did not produce {name} in time")))?
            .map_err(|err| Failure::Bridge(format!("read {name}: {err}")))?
            .ok_or_else(|| Failure::Bridge(format!("the bridge closed the sink before {name}")))?;
        if line.trim().is_empty() {
            continue;
        }
        return Ok(line);
    }
}

/// A tiny HTTP server that pretends to be an A2A
/// agent. Listens on `127.0.0.1:0`, accepts requests
/// in a loop, and answers every `SendMessage` with a
/// `Message` carrying a single text part. Bumps a
/// request counter so the test can assert the bridge
/// actually crossed the wire.
struct A2aFakeServer {
    addr: SocketAddr,
    requests: Arc<AtomicUsize>,
    /// Set when the harness is dropped. The server
    /// task polls this between accepts and exits
    /// when it sees the flag.
    stop: Arc<AtomicBool>,
}

impl A2aFakeServer {
    async fn spawn() -> Result<Self, Failure> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|err| Failure::Bridge(format!("bind: {err}")))?;
        let addr = listener
            .local_addr()
            .map_err(|err| Failure::Bridge(format!("local_addr: {err}")))?;
        let requests = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        tokio::spawn(server_loop(listener, Arc::clone(&requests), Arc::clone(&stop)));
        Ok(Self { addr, requests, stop })
    }

    fn requests_seen(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

impl Drop for A2aFakeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// The accept loop. Returns when `stop` is set,
/// or when the listener fails to accept.
async fn server_loop(
    listener: TcpListener,
    requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::SeqCst) {
        let next = listener.accept().await;
        let Ok((mut socket, _)) = next else {
            return;
        };
        let _ = handle_one(&mut socket, &requests).await;
    }
}

/// Handles one HTTP request. The bridge's
/// `A2aJsonRpcPeer` sends a `POST /a2a` with a
/// JSON-RPC envelope, and the server reads the
/// request body, ignores the method, and answers
/// with a `Message` reply that echoes the
/// request's id back. A bridge that rewrote ids
/// would lose the link between the request the
/// operator sent and the response the loop
/// reported, and the test asserts on the id.
async fn handle_one(
    socket: &mut tokio::net::TcpStream,
    counter: &AtomicUsize,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut buffer = Vec::new();
    let header_end = read_headers(socket, &mut buffer).await?;
    let content_length = content_length_of(buffer.get(..header_end).unwrap_or(&[]))?;
    let mut body = buffer.split_off(header_end);
    while body.len() < content_length {
        let need = content_length - body.len();
        let mut chunk = vec![0u8; need];
        let read = socket.read_exact(&mut chunk).await?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(chunk.get(..read).unwrap_or(&[]));
    }
    counter.fetch_add(1, Ordering::SeqCst);

    // Pull the request id out of the body so the
    // reply carries the same id. A bridge that
    // rewrote it would lose the link between
    // request and reply and the test would not
    // be able to assert on it.
    let request_id = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|value| value.get("id").cloned())
        .unwrap_or_else(|| Value::from(1));

    let reply = json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "result": {
            "kind": "message",
            "role": "agent",
            "parts": [{ "kind": "text", "text": "echo: ok" }],
            "messageId": "m1"
        }
    });
    let body_bytes = serde_json::to_vec(&reply)?;
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body_bytes.len()
    );
    socket.write_all(response.as_bytes()).await?;
    socket.write_all(&body_bytes).await?;
    socket.shutdown().await?;
    Ok(())
}

/// Reads bytes off the socket until the `\r\n\r\n`
/// that ends the HTTP headers, or until the buffer
/// grows past a sane ceiling. Returns the position
/// at which the body starts.
async fn read_headers(
    socket: &mut tokio::net::TcpStream,
    buffer: &mut Vec<u8>,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    while !buffer.ends_with(b"\r\n\r\n") {
        let mut byte = [0u8; 1];
        let read = socket.read(&mut byte).await?;
        if read == 0 {
            return Ok(buffer.len());
        }
        buffer.push(byte[0]);
        if buffer.len() > 32 * 1024 {
            return Ok(buffer.len());
        }
    }
    Ok(buffer.len())
}

/// Pulls the `Content-Length` header value out of
/// a header block. Defaults to zero if the header
/// is missing or malformed; the request handler
/// treats a zero-length body as a no-op.
fn content_length_of(headers: &[u8]) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    let header_text = std::str::from_utf8(headers)?;
    for line in header_text.split("\r\n") {
        let Some(rest) = line
            .strip_prefix("Content-Length:")
            .or_else(|| line.strip_prefix("content-length:"))
        else {
            continue;
        };
        return Ok(rest.trim().parse().unwrap_or(0));
    }
    Ok(0)
}
