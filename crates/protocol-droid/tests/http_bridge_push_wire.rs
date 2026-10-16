//! A server that pushes while the client is idle.
//!
//! A Streamable HTTP server may send a message at any time on the stream a
//! `GET` opens — `notifications/tools/list_changed`, a sampling request, a log
//! line. The question here is whether a client that has said nothing since its
//! handshake is still told.
//!
//! Nothing here uses a double. The upstream is a real [`HttpClient`] against a
//! real server over a real socket, because the behaviour under test lives in
//! the client's transport and in the loop that waits on it — and a hand-written
//! peer proved the bridge's routing worked while saying nothing about either.
//!
//! ## Why there are two push tests
//!
//! One test cannot tell "the push never left the server" apart from "the push
//! arrived and nobody had a stream to put it on". Those are different bugs with
//! different fixes, so the upstream here is *held* until the test says the
//! client is reading ([`a_push_reaches_a_client_that_has_said_nothing`]) and is
//! *not* held in the second ([`a_push_that_lands_before_the_client_reads_is_not_lost`]),
//! which is where the distinction actually bites.

#![cfg(feature = "http")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::response::Response;
use axum::routing::post;
use serde_json::{Value, json};
use tokio::sync::Notify;

use protocol_droid::adapters::primary::transport::http::{
    HttpBridge, SecurityHeaders, ServerConfig, SessionStore,
};
use protocol_droid::adapters::secondary::codec::result::WireToolResults;
use protocol_droid::adapters::secondary::transport::http::{HttpClient, HttpClientConfig};
use protocol_droid::application::bridge::{
    BridgePeer, Identity, OpenedUpstream, Upstream, UpstreamError,
};
use protocol_droid::application::client::{ClientConfig, handshake};
use protocol_droid::application::session::IdSource;
use protocol_droid::domain::session::SessionId;
use protocol_droid::domain::{DegradePolicy, Leg, SupportedVersions, TransportKind};

/// A deterministic id source, so a session id in a failure is reproducible.
struct Counting(std::sync::atomic::AtomicU64);

impl IdSource for Counting {
    fn next_session_id(&self) -> SessionId {
        SessionId::new(self.0.fetch_add(1, Ordering::SeqCst) + 1)
    }
}

/// Hex, because `SessionId` is hex: a server answering anything else is telling
/// the client it has no session worth naming.
const UPSTREAM_SESSION: &str = "0f0e0d0c0b0a0908";

/// How long a test waits for a frame before calling the bridge broken.
///
/// Short on purpose: the bridge relays on its own task, so a frame that is
/// going to arrive has arrived within milliseconds. A long budget only turns a
/// failing assertion into a slow build for everyone waiting on it.
const PATIENCE: Duration = Duration::from_secs(5);

/// Whether the bridge opened the upstream's standing stream, within `PATIENCE`.
///
/// A poll rather than a single read. The bridge opens that stream from a task
/// spawned when the client's handshake was answered, so "the handshake returned"
/// and "the GET left" are two different moments with nothing ordering them —
/// reading the counter once would be a race that passes most of the time.
async fn wait_for_open(state: &Server) -> bool {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        if state.opened.load(Ordering::SeqCst) > 0 {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// How the upstream server should behave about when it pushes.
struct Server {
    /// How many times the standing stream was opened. The direct evidence of
    /// whether the bridge asked to listen at all, which is the first thing to
    /// establish when a push goes missing.
    opened: AtomicUsize,
    /// When set, the stream waits for this before answering.
    release: Option<Arc<Notify>>,
}

async fn upstream_post(body: axum::body::Bytes) -> Response {
    let Ok(Value::Object(request)) = serde_json::from_slice::<Value>(&body) else {
        return Response::builder()
            .status(400)
            .body(Body::empty())
            .unwrap_or_default();
    };
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let result = match request.get("method").and_then(Value::as_str) {
        Some("initialize") => json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "far", "version": "1"}
        }),
        Some("tools/list") => {
            json!({"tools": [{"name": "echo", "inputSchema": {"type": "object"}}]})
        }
        _ => Value::Null,
    };
    let reply = json!({"jsonrpc": "2.0", "id": id, "result": result});
    // A stream, because that is the shape the specification prefers and the one
    // a client has to be able to read — and the header has to say so, or the
    // client is right to refuse to read SSE out of a body labelled JSON.
    Response::builder()
        .header("content-type", "text/event-stream")
        .header("mcp-session-id", UPSTREAM_SESSION)
        .body(Body::from(format!("event: message\ndata: {reply}\n\n")))
        .unwrap_or_default()
}

/// The standing stream: one unsolicited notification, then the peer goes quiet.
async fn upstream_get(State(state): axum::extract::State<Arc<Server>>) -> Response {
    state.opened.fetch_add(1, Ordering::SeqCst);
    if let Some(release) = &state.release {
        release.notified().await;
    }
    let notice = json!({
        "jsonrpc": "2.0",
        "method": "notifications/message",
        "params": {"level": "info", "data": "pushed while you were quiet"}
    });
    let frames = format!("event: message\ndata: {notice}\n\n");
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from(frames))
        .unwrap_or_default()
}

async fn spawn_upstream(state: Arc<Server>) -> Result<String, Box<dyn std::error::Error>> {
    // One `route` call, with both methods chained onto it. Two calls at the
    // same path do not merge: the second replaces the first, so the POST route
    // would silently vanish and every request would get a 405 with an empty
    // body — which reads downstream as "the peer sent something that was not
    // JSON".
    let app = Router::new().route(
        "/mcp",
        post(upstream_post)
            .get(upstream_get)
            .with_state(Arc::clone(&state)),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(format!("http://{addr}/mcp"))
}

/// The production shape, spelled out: dial a real client, run the real
/// handshake, hand the bridge the leg and the peer together.
///
/// The same two steps the CLI's `ConfiguredUpstream` takes, because a test that
/// wired the bridge up some other way would be testing a bridge nobody runs.
struct HttpUpstream {
    url: String,
}

impl Upstream for HttpUpstream {
    async fn open(&self) -> Result<OpenedUpstream, UpstreamError> {
        let config = HttpClientConfig::new(&self.url).map_err(wire)?;
        let client = HttpClient::new(config).map_err(wire)?;
        let versions = SupportedVersions::all().map_err(wire)?;
        let speaking =
            ClientConfig::speaking(versions.as_slice().to_vec(), TransportKind::StreamableHttp)
                .map_err(wire)?;
        let session = handshake(&client, &speaking).await.map_err(wire)?;
        let leg = Leg::new(session.version, session.profile);
        Ok(OpenedUpstream::new(
            leg,
            Arc::new(client) as Arc<dyn BridgePeer>,
        ))
    }
}

fn wire(error: impl std::fmt::Display) -> UpstreamError {
    let text = error.to_string();
    // The reason a session failed to start is otherwise dropped by the
    // bridge's task, which leaves "the upstream could not open" and "the
    // upstream opened and said nothing" looking identical from the outside.
    UpstreamError::Unreachable(text)
}

/// A bridge in front of a real HTTP upstream, bound and serving.
async fn running_bridge(state: Arc<Server>) -> Result<String, Box<dyn std::error::Error>> {
    let url = spawn_upstream(Arc::clone(&state)).await?;
    let ids: Arc<dyn IdSource + Send + Sync> =
        Arc::new(Counting(std::sync::atomic::AtomicU64::new(0)));
    let sessions: Arc<SessionStore<_>> = Arc::new(SessionStore::new(ids));
    let server = ServerConfig::new()?.with_security(SecurityHeaders::default());

    let bridge = HttpBridge::new(
        server,
        sessions,
        HttpUpstream { url },
        Identity::new("push-test", "0.0.0"),
        DegradePolicy::default(),
        WireToolResults,
    )?;
    let router = bridge.into_router()?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok(format!("http://{addr}/mcp"))
}

/// Opens the client's event stream and reads from it.
///
/// Incremental, because the stream is not supposed to end: a downstream client
/// holds it for the life of its session, so reading it to completion waits for
/// a close that never comes. Chunks are decoded as they arrive, since a frame
/// can straddle two of them.
struct Reading {
    response: reqwest::Response,
    pending: String,
}

impl Reading {
    async fn open(
        client: &reqwest::Client,
        url: &str,
        session: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let response = client
            .get(url)
            .header("accept", "text/event-stream")
            .header("mcp-session-id", session)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(format!("the stream answered {}", response.status()).into());
        }
        Ok(Self {
            response,
            pending: String::new(),
        })
    }

    /// Every frame that arrives within `budget`, looking for `needle`.
    ///
    /// One deadline for the whole scan rather than one per frame. A per-frame
    /// deadline multiplied by the number of frames is a test that takes most of
    /// a minute to fail, which is long enough that nobody re-runs it and short
    /// enough that everyone assumes it is slow rather than stuck.
    ///
    /// Returns every frame it saw — a failure has to show what *did* arrive, or
    /// "it did not work" and "it never got that far" look the same.
    async fn await_frame(
        &mut self,
        needle: &str,
        budget: Duration,
    ) -> Result<Option<Vec<String>>, Box<dyn std::error::Error>> {
        let deadline = tokio::time::Instant::now() + budget;
        let mut seen: Vec<String> = Vec::new();
        loop {
            let arrived = tokio::time::timeout_at(deadline, self.response.chunk()).await;
            let Ok(Ok(Some(bytes))) = arrived else {
                return Ok(if seen.iter().any(|f| f.contains(needle)) {
                    Some(seen)
                } else {
                    None
                });
            };
            self.pending.push_str(&String::from_utf8_lossy(&bytes));
            let ready: Vec<String> = self
                .pending
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(|data| data.trim().to_owned())
                .collect();
            if !ready.is_empty() {
                seen.extend(ready);
                // Only the tail can hold half a line; keeping the rest would
                // re-scan every frame the stream has already delivered.
                let cut = self.pending.rfind('\n').map_or(0, |at| at + 1);
                self.pending = self.pending.split_off(cut);
            }
        }
    }
}

fn initialize(id: i64) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "t", "version": "1"}
        }
    })
    .to_string()
}

/// Completes the handshake and hands back the session and an open stream.
async fn connected(
    url: &str,
) -> Result<(reqwest::Client, String, Reading), Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let opened = client
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(initialize(1))
        .send()
        .await?;
    if !opened.status().is_success() {
        return Err(format!("the handshake answered {}", opened.status()).into());
    }
    let session = opened
        .headers()
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .ok_or("no session id came back")?
        .to_owned();
    let reading = Reading::open(&client, url, &session).await?;
    Ok((client, session, reading))
}

#[tokio::test]
async fn a_push_reaches_a_client_that_has_said_nothing() -> Result<(), Box<dyn std::error::Error>> {
    let release = Arc::new(Notify::new());
    let state = Arc::new(Server {
        opened: AtomicUsize::new(0),
        release: Some(Arc::clone(&release)),
    });
    let url = running_bridge(Arc::clone(&state)).await?;

    // Handshake, then open the stream — and say nothing else. The client is now
    // idle, which is the whole question: if delivery needs a client frame to
    // trigger it, this is the assertion that catches it.
    let (_client, _session, mut reading) = connected(&url).await?;

    // Only now does the server push, so nothing here is a race.
    release.notify_one();

    // Waited for rather than read once. The bridge opens the upstream's stream
    // from a spawned task, so "the handshake returned" and "the GET left" are
    // two different moments and nothing orders them. Reading the counter once
    // would make this a race that passes most of the time.
    if !wait_for_open(&state).await {
        return Err("the bridge never opened the upstream's standing stream".into());
    }

    // Not "the next frame": the handshake reply is already on this stream, and
    // the notification the server volunteered upstream is a second frame behind
    // it. Reading one frame and asserting on it would be asserting on the
    // handshake however the bridge behaved.
    let _ = reading
        .await_frame("notifications/message", PATIENCE)
        .await?
        .ok_or("no frame carried it")?;
    Ok(())
}

#[tokio::test]
async fn a_push_that_lands_before_the_client_reads_is_not_lost()
-> Result<(), Box<dyn std::error::Error>> {
    let state = Arc::new(Server {
        opened: AtomicUsize::new(0),
        release: None,
    });
    let url = running_bridge(Arc::clone(&state)).await?;

    let client = reqwest::Client::new();
    let opened = client
        .post(&url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(initialize(1))
        .send()
        .await?;
    let session = opened
        .headers()
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .ok_or("no session id came back")?
        .to_owned();

    // The push is not held back, so it very probably lands before the `GET`
    // below registers a stream. A client opening its stream slightly late is
    // normal, not exotic, and a frame written to a sink that did not exist yet
    // is exactly the way this would be lost.
    tokio::time::sleep(Duration::from_millis(150)).await;

    if !wait_for_open(&state).await {
        return Err("the bridge never opened the upstream's standing stream".into());
    }

    let mut reading = Reading::open(&client, &url, &session).await?;
    reading
        .await_frame("notifications/message", PATIENCE)
        .await?
        .ok_or("no frame carried the notification")?;
    Ok(())
}

#[tokio::test]
async fn a_request_still_crosses_to_the_upstream() -> Result<(), Box<dyn std::error::Error>> {
    // The push path is only worth having if the ordinary path still works, and
    // changing how the loop waits is exactly the kind of thing that can cost it
    // a request.
    let state = Arc::new(Server {
        opened: AtomicUsize::new(0),
        release: None,
    });
    let url = running_bridge(Arc::clone(&state)).await?;
    let (client, session, mut reading) = connected(&url).await?;

    client
        .post(&url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session)
        .body(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}).to_string())
        .send()
        .await?;

    // The upstream volunteers a notification, so the reply may not be first.
    reading
        .await_frame("\"echo\"", PATIENCE)
        .await?
        .ok_or("no frame carried the tool list")?;
    Ok(())
}
