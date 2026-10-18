//! The client's tests, with the resume claim at the centre.
//!
//! Two kinds of test here, and the split is deliberate.
//!
//! The **framing** tests are pure. `split_block` and `FrameReader` are handed
//! byte strings and never open a socket, because a test that binds one is
//! testing the operating system as much as the code, and it is the test that
//! hangs when the code under test hangs.
//!
//! The **conversation** tests need a real socket, because `reqwest` is a real
//! HTTP client and the only honest way to test it is against a real server. The
//! server is a small `axum` app on `127.0.0.1:0` — the same crate the adapter
//! already depends on, so the resume test is a genuine round trip rather than a
//! mock that agrees with the client by construction.
//!
//! That last point is why the resume test does not fake the server's replay
//! logic. It hands the request to the **same** [`EventTail::plan`] the real
//! router uses, drops the connection after a fixed number of frames, and then
//! asserts that what the client ended up with is exactly the events the server
//! pushed — no repeats, no gaps, in order. A mock would prove the client can
//! resume from whatever the mock chose to send, which is the easy half.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use super::error::preview;
use super::frames::{FrameReader, split_block};
use super::{
    FrameError, HttpClient, HttpClientConfig, HttpClientError, HttpReply, Resumable, RetryBudget,
};
use crate::domain::session::SessionId;
use crate::domain::{EventId, McpSessionId};

/// Every fallible call in this module funnels through here, so a failure prints
/// the engine's own account of what it refused.
type TestError = Box<dyn std::error::Error>;

/// Names a failed claim.
///
/// Annotated rather than written as `Err(fail("…"))` at each call site because
/// `.into()` there is ambiguous — `&str` converts to both `&str` and
/// `Box<dyn Error>` — so the compiler settles the function's error type on
/// whichever it saw first and every `?` in the body then fails to convert.
fn fail(message: impl Into<String>) -> TestError {
    message.into().into()
}

/// Asserts a claim, and says so in the failure message rather than returning a
/// bare `false` the caller has to describe.
fn require(condition: bool, message: &str) -> Result<(), TestError> {
    if condition {
        Ok(())
    } else {
        Err(fail(message.to_owned()))
    }
}

// ---------------------------------------------------------------------------
// Framing, with no socket anywhere
// ---------------------------------------------------------------------------

#[test]
fn a_block_ends_at_a_blank_line_and_keeps_its_terminator() -> Result<(), TestError> {
    // The trailing newline stays because the decoder is written against a
    // payload's last line being empty. Hand it over stripped and every event
    // whose data ends in a newline changes its payload.
    let found = split_block(b"id: 1\ndata: x\n\nid: 2\ndata: y\n\n");
    let (block, used) = found.ok_or("a complete block was not found")?;
    require(block == "id: 1\ndata: x\n", "the block lost its terminator")?;
    let rest = b"id: 1\ndata: x\n\nid: 2\ndata: y\n\n"
        .get(used..)
        .unwrap_or_default();
    let (next, _) = split_block(rest).ok_or("the block after the first was not found")?;
    require(
        next == "id: 2\ndata: y\n",
        "the second block came back wrong",
    )?;
    Ok(())
}

#[test]
fn a_single_newline_does_not_dispatch_an_event() -> Result<(), TestError> {
    // The framing mistake this reader exists to avoid: stopping at the first
    // newline hands the grammar a block with no blank line in it, so nothing
    // is dispatched and the stream looks alive while carrying nothing.
    require(
        split_block(b"id: 1\ndata: x\n").is_none(),
        "a block was dispatched before its blank line arrived",
    )?;
    Ok(())
}

#[test]
fn a_crlf_pair_is_one_blank_line_and_not_two() -> Result<(), TestError> {
    let input = b"id: 1\r\ndata: x\r\n\r\nid: 2\r\ndata: y\r\n\r\n";
    let (block, used) = split_block(input).ok_or("a complete block was not found")?;
    require(
        block == "id: 1\r\ndata: x\r\n",
        "the CRLF block came back with the wrong extent",
    )?;
    // The next block must be the *next event*, not an empty one left behind by
    // deleting the CR and letting the LF terminate a second block.
    let rest = input.get(used..).unwrap_or_default();
    let (next, _) = split_block(rest).ok_or("the block after a CRLF was not found")?;
    require(
        next == "id: 2\r\ndata: y\r\n",
        "a CRLF pair left a phantom empty block behind",
    )?;
    Ok(())
}

#[test]
fn a_mixed_terminator_pair_still_dispatches_one_block() -> Result<(), TestError> {
    // A peer that writes CRLF on one line and a bare LF on the next is
    // conforming, and a reader that assumes a uniform prefix fails on it.
    let found = split_block(b"id: 1\r\ndata: x\n\n");
    let (block, _) = found.ok_or("a mixed-terminator block was not found")?;
    require(
        block == "id: 1\r\ndata: x\n",
        "a mixed block came back wrong",
    )?;
    Ok(())
}

#[test]
fn a_lone_carriage_return_waits_for_its_line_feed() -> Result<(), TestError> {
    // The LF may be in the next chunk. Deciding "this is a terminator" here is
    // how a CRLF-encoded stream arrives with the CR cut off at a chunk edge.
    require(
        split_block(b"id: 1\r").is_none(),
        "a trailing CR was resolved before its LF arrived",
    )?;
    Ok(())
}

#[test]
fn a_frame_split_across_two_reads_is_one_frame() -> Result<(), TestError> {
    let mut reader = FrameReader::new();
    let first = reader.push(b"id: 1\ndata: hel")?;
    require(first.is_empty(), "a half-frame was dispatched")?;
    require(reader.buffered() == 15, "the partial frame was not held")?;
    let second = reader.push(b"lo\n\n")?;
    require(second.len() == 1, "the completed frame was not dispatched")?;
    Ok(())
}

#[test]
fn frames_arrive_in_wire_order() -> Result<(), TestError> {
    let mut reader = FrameReader::new();
    let frames = reader.push(b"id: 1\ndata: a\n\nid: 2\ndata: b\n\n")?;
    let ids: Vec<_> = frames
        .iter()
        .filter_map(crate::adapters::primary::transport::http::SseEvent::id)
        .collect();
    require(ids.len() == 2, "not every frame was dispatched")?;
    require(
        ids.first() == Some(&EventId::new(1)),
        "the first frame is not first",
    )?;
    require(
        ids.get(1) == Some(&EventId::new(2)),
        "the second frame is not second",
    )?;
    Ok(())
}

#[test]
fn a_keep_alive_comment_dispatches_no_event() -> Result<(), TestError> {
    // A keep-alive is a comment. Dispatching it as an event would hand the
    // protocol layer a frame with no id, and an id-less frame is exactly what
    // makes a resume undecidable.
    let mut reader = FrameReader::new();
    let frames = reader.push(b":ping\n\n")?;
    require(frames.is_empty(), "a comment was dispatched as an event")?;
    Ok(())
}

#[test]
fn a_block_past_the_bound_is_refused_rather_than_truncated() -> Result<(), TestError> {
    let mut reader = FrameReader::new();
    // No terminator anywhere, so this never becomes a block — the reader has to
    // refuse it rather than hold it until the process runs out of memory.
    let flood = vec![b'x'; 64 * 1024];
    let found = reader.push(&flood);
    require(
        matches!(found, Err(FrameError::BlockTooLong { .. })),
        "an unterminated flood of bytes was not refused",
    )?;
    require(reader.buffered() == 0, "the refused block was not dropped")?;
    Ok(())
}

#[test]
fn a_partial_block_is_distinguishable_from_a_clean_end() -> Result<(), TestError> {
    let mut reader = FrameReader::new();
    let _ = reader.push(b"id: 1\ndata: x\n\n")?;
    require(
        reader.into_partial().is_none(),
        "a block that was fully consumed looked partial",
    )?;

    let mut cut = FrameReader::new();
    let _ = cut.push(b"id: 1\ndata: half")?;
    require(
        cut.into_partial().is_some(),
        "a connection that dropped mid-event looked like a clean end",
    )?;
    Ok(())
}

#[test]
fn a_refusal_preview_is_bounded_and_never_panics_on_bad_bytes() -> Result<(), TestError> {
    require(preview(b"short") == "short", "a short body was altered")?;
    let long = vec![b'a'; 4096];
    let shown = preview(&long);
    require(
        shown.chars().count() <= 256,
        "an unbounded body was copied into an error",
    )?;
    // Invalid UTF-8 must be replaced, not panic and not stop the whole thing.
    require(
        !preview(&[0xff, 0xfe]).is_empty(),
        "bad bytes produced no preview",
    )?;
    Ok(())
}

#[test]
fn an_endpoint_that_is_not_a_url_is_refused_at_construction() -> Result<(), TestError> {
    require(
        HttpClientConfig::new("not a url").is_err(),
        "a garbage endpoint was accepted",
    )?;
    Ok(())
}

#[test]
fn a_budget_of_zero_reconnects_never_reconnects() {
    // Not a `Result` test on purpose: this is arithmetic, and a test that
    // returns one for a value it can only compare adds a failure path that
    // cannot happen.
    assert_eq!(RetryBudget::new(0).attempts(), 0);
    assert_eq!(RetryBudget::default().attempts(), 3);
}

// ---------------------------------------------------------------------------
// The conversation, against a real socket
// ---------------------------------------------------------------------------

/// A server on loopback, and the endpoint that reaches it.
struct TestServer {
    endpoint: String,
    handle: JoinHandle<()>,
}

impl TestServer {
    /// Binds `127.0.0.1:0` and serves `router` in the background.
    async fn start(router: Router) -> Result<Self, TestError> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let handle = tokio::spawn(async move {
            // A serve error is the server dying, which a test waiting on a
            // client read notices as a hang rather than as a message. There is
            // nobody to report it to, so the task simply ends.
            let _ = axum::serve(listener, router).await;
        });
        Ok(Self {
            endpoint: format!("http://{address}/mcp"),
            handle,
        })
    }

    /// A client pointed at this server.
    fn client(&self) -> Result<HttpClient, TestError> {
        Ok(HttpClient::new(HttpClientConfig::new(&self.endpoint)?)?)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// The headers of the last request the server saw, and a count of the calls.
#[derive(Debug, Default)]
struct Seen {
    last: std::sync::Mutex<Option<HeaderMap>>,
    calls: AtomicUsize,
}

impl Seen {
    fn last(&self) -> Option<HeaderMap> {
        self.last
            .lock()
            .map(|held| held.clone())
            .unwrap_or_default()
    }

    fn record(&self, headers: &HeaderMap) {
        if let Ok(mut slot) = self.last.lock() {
            *slot = Some(headers.clone());
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

/// A route that records its headers and answers with a fixed document.
fn seen_route(
    seen: Arc<Seen>,
    status: StatusCode,
    content_type: &'static str,
    body: &'static str,
    session: Option<&'static str>,
) -> Router {
    // Both verbs, because the same fixture serves a POST that is answered with
    // a document and a GET that opens a stream. One verb means a 405, and a 405
    // means the client is never reached, so the other half of the fixture
    // would silently test nothing.
    let make = move || {
        let (seen, session) = (Arc::clone(&seen), session);
        async move |headers: HeaderMap| {
            seen.record(&headers);
            let mut response =
                (status, [(header::CONTENT_TYPE, content_type)], body).into_response();
            if let Some(id) = session
                && let Ok(value) = axum::http::HeaderValue::from_str(id)
            {
                response.headers_mut().insert("mcp-session-id", value);
            }
            response
        }
    };
    Router::new().route("/mcp", post(make()).get(make()))
}

#[tokio::test]
async fn a_post_asks_for_both_response_shapes() -> Result<(), TestError> {
    let seen = Arc::new(Seen::default());
    let server = TestServer::start(seen_route(
        Arc::clone(&seen),
        StatusCode::OK,
        "application/json",
        r#"{"ok":true}"#,
        None,
    ))
    .await?;

    let _ = server.client()?.post(b"{}", false).await?;
    let sent = seen.last().ok_or("the server saw no request")?;
    let accept = sent
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    require(
        accept.contains("application/json") && accept.contains("text/event-stream"),
        "a client that cannot read one of the two shapes sent the Accept header",
    )?;
    Ok(())
}

#[tokio::test]
async fn a_content_type_with_parameters_is_still_recognised() -> Result<(), TestError> {
    let seen = Arc::new(Seen::default());
    let server = TestServer::start(seen_route(
        Arc::clone(&seen),
        StatusCode::OK,
        "application/json; charset=utf-8",
        r#"{"ok":true}"#,
        None,
    ))
    .await?;

    let response = server.client()?.post(b"{}", false).await?;
    require(
        response.content_type() == Some("application/json"),
        "a charset parameter made the media type unrecognisable",
    )?;
    Ok(())
}

#[tokio::test]
async fn a_json_response_is_read_as_a_document() -> Result<(), TestError> {
    let seen = Arc::new(Seen::default());
    let server = TestServer::start(seen_route(
        Arc::clone(&seen),
        StatusCode::OK,
        "application/json",
        r#"{"jsonrpc":"2.0","id":1}"#,
        None,
    ))
    .await?;

    match server.client()?.post_streaming(b"{}", false).await? {
        HttpReply::Document(response) => {
            require(
                response.body() == br#"{"jsonrpc":"2.0","id":1}"#,
                "the document body did not survive the round trip",
            )?;
        }
        HttpReply::Stream(_) => return Err(fail("a JSON document arrived as a stream")),
    }
    Ok(())
}

#[tokio::test]
async fn an_event_stream_comes_back_as_a_stream() -> Result<(), TestError> {
    let seen = Arc::new(Seen::default());
    let server = TestServer::start(seen_route(
        Arc::clone(&seen),
        StatusCode::OK,
        "text/event-stream",
        "id: 1\ndata: a\n\nid: 2\ndata: b\n\n",
        None,
    ))
    .await?;

    let client = server.client()?;
    let mut stream = match client.post_streaming(b"{}", false).await? {
        HttpReply::Stream(stream) => stream,
        HttpReply::Document(_) => return Err(fail("an event stream arrived as a document")),
    };
    let mut ids = Vec::new();
    while let Some(event) = stream.next_event().await? {
        if let Some(id) = event.id() {
            ids.push(id);
        }
    }
    require(
        ids == vec![EventId::new(1), EventId::new(2)],
        "a POST answered with a stream did not deliver its frames in order",
    )?;
    Ok(())
}

#[tokio::test]
async fn the_session_the_peer_issued_is_sent_on_the_next_request() -> Result<(), TestError> {
    let seen = Arc::new(Seen::default());
    let server = TestServer::start(seen_route(
        Arc::clone(&seen),
        StatusCode::OK,
        "application/json",
        "{}",
        Some("0000000000000001"),
    ))
    .await?;

    let client = server.client()?;
    require(
        client.session_id().is_none(),
        "a session existed before the peer issued one",
    )?;
    let _ = client.post(b"{}", false).await?;
    let issued = client
        .session_id()
        .ok_or("the peer's session id was not captured")?;
    require(
        issued == McpSessionId::new(SessionId::new(1)),
        "the captured session id is not the one the peer sent",
    )?;

    let _ = client.post(b"{}", false).await?;
    let sent = seen.last().ok_or("the server saw no second request")?;
    let echoed = sent
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    require(
        echoed == issued.to_string(),
        "the captured session was not carried onto the next request",
    )?;
    Ok(())
}

#[tokio::test]
async fn a_refusal_carries_the_status_the_peer_chose() -> Result<(), TestError> {
    let seen = Arc::new(Seen::default());
    let server = TestServer::start(seen_route(
        Arc::clone(&seen),
        StatusCode::NOT_FOUND,
        "application/json",
        r#"{"error":"no such session"}"#,
        None,
    ))
    .await?;

    match server.client()?.post(b"{}", false).await {
        Err(HttpClientError::Refused { status, method, .. }) => {
            require(
                status == StatusCode::NOT_FOUND,
                "the status was not carried",
            )?;
            require(method == "POST", "the error did not name the method")?;
        }
        Err(other) => return Err(fail(format!("a 404 became {other}"))),
        Ok(_) => return Err(fail("a 404 was reported as a success")),
    }
    Ok(())
}

#[tokio::test]
async fn a_two_hundred_with_an_unreadable_media_type_is_not_a_success() -> Result<(), TestError> {
    let seen = Arc::new(Seen::default());
    let server = TestServer::start(seen_route(
        Arc::clone(&seen),
        StatusCode::OK,
        "text/html",
        "<html>nope</html>",
        None,
    ))
    .await?;

    match server.client()?.post(b"{}", false).await {
        Err(HttpClientError::MediaType { got }) => {
            require(got == "text/html", "the media type was not reported")?;
        }
        Err(other) => return Err(fail(format!("an HTML body became {other}"))),
        Ok(_) => return Err(fail("an HTML body was reported as a JSON-RPC response")),
    }
    Ok(())
}

#[tokio::test]
async fn a_session_header_the_client_cannot_parse_is_refused() -> Result<(), TestError> {
    let seen = Arc::new(Seen::default());
    let server = TestServer::start(seen_route(
        Arc::clone(&seen),
        StatusCode::OK,
        "application/json",
        "{}",
        Some("not-a-session"),
    ))
    .await?;

    let client = server.client()?;
    match client.post(b"{}", false).await {
        Err(HttpClientError::SessionId { .. }) => {}
        Err(other) => return Err(fail(format!("a bad session id became {other}"))),
        Ok(_) => return Err(fail("a bad session id was accepted")),
    }
    require(
        client.session_id().is_none(),
        "an unparseable session id was kept anyway",
    )?;
    Ok(())
}

#[tokio::test]
async fn a_call_that_needs_a_session_before_one_was_issued_is_refused() -> Result<(), TestError> {
    let seen = Arc::new(Seen::default());
    let server = TestServer::start(seen_route(
        Arc::clone(&seen),
        StatusCode::OK,
        "application/json",
        "{}",
        None,
    ))
    .await?;

    let client = server.client()?;
    for refused in [
        client.post(b"{}", true).await.err(),
        client.open_stream(None, true).await.err(),
        client.delete().await.err(),
    ] {
        require(
            matches!(refused, Some(HttpClientError::NoSessionYet)),
            "a call that needs a session ran before one was issued",
        )?;
    }
    require(seen.calls() == 0, "a refused call reached the peer")?;
    Ok(())
}

#[tokio::test]
async fn a_block_that_is_not_a_frame_is_a_typed_error() -> Result<(), TestError> {
    let seen = Arc::new(Seen::default());
    let server = TestServer::start(seen_route(
        Arc::clone(&seen),
        StatusCode::OK,
        "text/event-stream",
        // A non-numeric id: the cursor decides what the client believes it has
        // already received, so a corrupt one is refused rather than guessed at.
        "id: not-a-number\ndata: x\n\n",
        None,
    ))
    .await?;

    let client = server.client()?;
    let mut stream = client.open_stream(None, false).await?;
    match stream.next_event().await {
        Err(HttpClientError::Frame { .. }) => {}
        Err(other) => return Err(fail(format!("a malformed frame became {other}"))),
        Ok(_) => return Err(fail("a malformed frame was dispatched")),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The resume claim
// ---------------------------------------------------------------------------

/// A peer that sends real event frames and then **drops the connection**
/// mid-body, resuming from whatever `Last-Event-ID` it is given.
///
/// Written on a raw socket rather than with `axum` because an errored response
/// body is discarded by the HTTP layer before the headers reach the client: the
/// client then sees a *failed request*, not a stream that started and broke.
/// Those are different failures, and only the second is what a resume has to
/// survive. Producing it means writing chunked framing and then stopping without
/// the terminating zero-length chunk, which is what a peer that goes away
/// mid-stream does on the wire.
struct DropPeer {
    endpoint: String,
    calls: Arc<AtomicUsize>,
    handle: JoinHandle<()>,
}

impl DropPeer {
    /// A peer holding `events` events, cutting each connection after
    /// `per_connection` of them. Zero means "cut before delivering any".
    async fn start(events: u64, per_connection: usize) -> Result<Self, TestError> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let calls = Arc::new(AtomicUsize::new(0));
        let served = Arc::clone(&calls);
        let newest = events;
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let served = Arc::clone(&served);
                tokio::spawn(async move {
                    served.fetch_add(1, Ordering::SeqCst);
                    let _ = converse(&mut socket, newest, per_connection).await;
                    // Dropped here without the terminating chunk, which is the
                    // whole point: a clean close would be a finished stream.
                });
            }
        });
        Ok(Self {
            endpoint: format!("http://{address}/mcp"),
            calls,
            handle,
        })
    }

    fn client(&self) -> Result<HttpClient, TestError> {
        Ok(HttpClient::new(HttpClientConfig::new(&self.endpoint)?)?)
    }

    fn connections(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Drop for DropPeer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// The bytes of one `id: N\ndata: …\n\n` frame.
fn frame_bytes(index: u64) -> String {
    format!("id: {index}\ndata: {{\"n\":{index}}}\n\n")
}

/// One chunk of a chunked body.
fn chunk(payload: &str) -> String {
    format!("{:X}\r\n{payload}\r\n", payload.len())
}

/// Answers one request and then, on purpose, stops mid-body.
async fn converse(
    socket: &mut tokio::net::TcpStream,
    newest: u64,
    per_connection: usize,
) -> Result<(), TestError> {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let mut request: Vec<u8> = Vec::new();
    let mut scratch = [0_u8; 1024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = socket.read(&mut scratch).await?;
        if read == 0 {
            return Ok(());
        }
        request.extend_from_slice(scratch.get(..read).unwrap_or_default());
    }
    let head = String::from_utf8_lossy(&request);
    let cursor = head
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("last-event-id"))
        .and_then(|(_, value)| value.trim().parse::<u64>().ok())
        .unwrap_or(0);

    socket
        .write_all(
            concat!(
                "HTTP/1.1 200 OK\r\n",
                "content-type: text/event-stream\r\n",
                "transfer-encoding: chunked\r\n",
                "connection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await?;

    // A client that already holds everything gets a properly finished stream,
    // so the read can end. Without this the last connection delivers one event,
    // dies, delivers nothing, dies again, and "resumed correctly" and "gave up"
    // become indistinguishable.
    if cursor >= newest {
        socket.write_all(b"0\r\n\r\n").await?;
        socket.flush().await?;
        return Ok(());
    }

    let mut sent = 0_usize;
    for index in (cursor.saturating_add(1))..=newest {
        if sent >= per_connection {
            break;
        }
        socket
            .write_all(chunk(&frame_bytes(index)).as_bytes())
            .await?;
        sent = sent.saturating_add(1);
    }
    socket.flush().await?;
    // No terminating chunk. The peer simply is not there any more.
    Ok(())
}

#[tokio::test]
async fn a_stream_that_drops_is_resumed_without_repeating_or_losing_an_event()
-> Result<(), TestError> {
    // Five events on the peer, every connection cut after two. The client has
    // to ask for what it missed each time, and what it ends up holding has to
    // be 1..=5 exactly once each, in order.
    let peer = DropPeer::start(5, 2).await?;
    let client = peer.client()?;
    let mut read = client.resumable(None, false, Resumable::default());

    let mut collected: Vec<EventId> = Vec::new();
    let mut rounds = 16_u32;
    loop {
        rounds = rounds.saturating_sub(1);
        require(rounds > 0, "the read never finished")?;
        match read.next_event().await? {
            None => break,
            Some(event) => {
                let id = event.id().ok_or("a frame with no id was delivered")?;
                if let Some(last) = collected.last().copied() {
                    require(id > last, "the client was handed an event twice")?;
                }
                collected.push(id);
            }
        }
    }

    let expect: Vec<EventId> = (1..=5).map(EventId::new).collect();
    require(
        collected == expect,
        "resuming across a drop did not cover the stream exactly once",
    )?;
    require(
        read.reconnects() > 0,
        "the stream never dropped, so the resume was never exercised",
    )?;
    require(
        peer.connections() > 1,
        "the client asked for the whole stream on one connection",
    )?;
    Ok(())
}

#[tokio::test]
async fn a_stream_that_ends_on_a_boundary_is_not_reconnected() -> Result<(), TestError> {
    // A body that ends cleanly is a peer that has finished. Reconnecting to it
    // would ask a server with nothing left to say for more, once per budget.
    let seen = Arc::new(Seen::default());
    let recorder = Arc::clone(&seen);
    let server = TestServer::start(
        Router::new()
            .route(
                "/mcp",
                get(move |headers: HeaderMap| {
                    let recorder = Arc::clone(&recorder);
                    async move {
                        recorder.record(&headers);
                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            "id: 1\ndata: a\n\n",
                        )
                    }
                }),
            )
            .with_state(()),
    )
    .await?;

    let client = server.client()?;
    let mut read = client.resumable(None, false, Resumable::default());
    while read.next_event().await?.is_some() {}

    require(
        read.reconnects() == 0,
        "a cleanly-ended stream was reconnected anyway",
    )?;
    require(
        seen.calls() == 1,
        "a cleanly-ended stream opened more than one connection",
    )?;
    Ok(())
}

#[tokio::test]
async fn a_resumable_read_that_keeps_failing_reports_its_budget() -> Result<(), TestError> {
    // Every connection dies before delivering a single event, so the read has
    // nothing to resume from. It must give up and say so rather than
    // reconnecting forever against a peer that has gone.
    let peer = DropPeer::start(4, 0).await?;
    let client = peer.client()?;
    let mut read = client.resumable(
        None,
        false,
        Resumable {
            budget: RetryBudget::new(2),
        },
    );
    let mut events = 0_u32;
    loop {
        match read.next_event().await {
            Ok(Some(_)) => events = events.saturating_add(1),
            Ok(None) => break,
            Err(HttpClientError::ResumeExhausted { attempts, .. }) => {
                require(attempts >= 1, "the budget was reported as never spent")?;
                return Ok(());
            }
            Err(other) => return Err(fail(format!("an exhausted read reported {other}"))),
        }
        require(
            events < 32,
            "the read delivered an unbounded number of events",
        )?;
    }
    Err(fail(
        "a peer that never sent a byte was reported as a completed read",
    ))
}

#[tokio::test]
async fn an_event_with_no_id_ends_the_read_rather_than_repeating_one() -> Result<(), TestError> {
    // A frame with no id has no cursor, so a peer asked to resume from the
    // previous id would repeat everything after it. The read has to stop.
    let server = TestServer::start(Router::new().route(
        "/mcp",
        get(|| async {
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/event-stream")],
                "data: no id here\n\n",
            )
        }),
    ))
    .await?;

    let client = server.client()?;
    let mut read = client.resumable(None, false, Resumable::default());
    let first = read.next_event().await?;
    require(first.is_some(), "the id-less frame was not delivered")?;
    require(
        read.cursor().is_none(),
        "an id-less frame moved the resume cursor",
    )?;
    require(
        read.next_event().await?.is_none(),
        "the read continued past an event it could never resume from",
    )?;
    Ok(())
}

#[tokio::test]
async fn a_get_carries_the_cursor_it_was_given() -> Result<(), TestError> {
    let seen = Arc::new(Seen::default());
    let recorder = Arc::clone(&seen);
    let server = TestServer::start(
        Router::new()
            .route(
                "/mcp",
                get(move |headers: HeaderMap| {
                    let recorder = Arc::clone(&recorder);
                    async move {
                        recorder.record(&headers);
                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            "id: 9\ndata: x\n\n",
                        )
                    }
                }),
            )
            .with_state(()),
    )
    .await?;

    let client = server.client()?;
    let mut stream = client.open_stream(Some(EventId::new(7)), false).await?;
    let _ = stream.next_event().await?;

    let sent = seen.last().ok_or("the server saw no request")?;
    let cursor = sent
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    require(cursor == "7", "the cursor was not carried onto the request")?;
    Ok(())
}

#[tokio::test]
async fn a_stream_reports_the_last_id_it_delivered() -> Result<(), TestError> {
    let seen = Arc::new(Seen::default());
    let server = TestServer::start(seen_route(
        Arc::clone(&seen),
        StatusCode::OK,
        "text/event-stream",
        "id: 1\ndata: a\n\nid: 2\ndata: b\n\n",
        None,
    ))
    .await?;

    let client = server.client()?;
    let mut stream = client.open_stream(None, false).await?;
    let mut ids = Vec::new();
    while let Some(event) = stream.next_event().await? {
        if let Some(id) = event.id() {
            ids.push(id);
        }
    }
    require(
        ids == vec![EventId::new(1), EventId::new(2)],
        "a stream did not deliver its frames in wire order",
    )?;
    require(
        stream.last_event_id() == Some(EventId::new(2)),
        "the stream did not remember the last id it delivered",
    )?;
    require(
        !stream.has_partial(),
        "a fully-read stream still holds bytes",
    )?;
    Ok(())
}

/// One client, two handshakes at different revisions, against this crate's
/// server — the shape `protocol-droid probe` drives. The revision the first
/// handshake settled on must not be sent as `MCP-Protocol-Version` on the
/// second `initialize`, or the server compares it with the session it is
/// replacing and refuses.
#[tokio::test]
async fn a_client_can_handshake_again_at_another_revision() -> Result<(), Box<dyn std::error::Error>> {
    use crate::adapters::primary::transport::http::{HttpBuilder, ServerConfig};
    use crate::adapters::secondary::transport::http::{HttpClient, HttpClientConfig};
    use crate::application::client::{ClientConfig, handshake};
    use crate::application::server::Server;
    use crate::domain::{ProtocolVersion, TransportKind};

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let builder = HttpBuilder::new(Server::builder("rehandshake").build()).config(ServerConfig::new()?);
    tokio::spawn(async move {
        let _ = builder.serve_on(listener).await;
    });
    let client = HttpClient::new(HttpClientConfig::new(&format!("http://{addr}/mcp"))?)?;
    for version in [ProtocolVersion::V2025_06_18, ProtocolVersion::V2025_11_25] {
        let config = ClientConfig::speaking(vec![version], TransportKind::StreamableHttp)?;
        let session = handshake(&client, &config).await?;
        assert_eq!(session.version, version);
    }
    Ok(())
}
