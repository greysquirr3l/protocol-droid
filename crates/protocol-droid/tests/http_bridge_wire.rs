//! The bridge's listen side, on a socket.
//!
//! The bridge's other half — the HTTP client, the SSE grammar, the resume
//! cursor — is exercised in `http_transport_wire.rs`, and this file is the one
//! place both directions of the bridge meet a real socket. Everything below
//! crosses a real connection: a `POST` the router routes, a session id minted
//! by the server and read by the client out of a response header, and a reply
//! that is written to a tail, framed as SSE, and read back by a client that
//! never saw the `POST` that caused it.
//!
//! Only the upstream is a double, and that is deliberate in the other
//! direction too: the upstream is the *other* end of the bridge, and a bridge
//! whose far end is a double is still a bridge. Faking it would mean standing
//! up a second HTTP server, which would test the server twice and the bridge
//! not at all.
//!
//! No assertion here reads an intermediate value. Every claim is read off the
//! bytes that crossed the socket.

#![cfg(feature = "http")]

use std::sync::Arc;

use protocol_droid::adapters::primary::transport::http::{
    HttpBridge, SecurityHeaders, ServerConfig, SessionStore,
};
use protocol_droid::adapters::secondary::codec::result::WireToolResults;
use protocol_droid::adapters::secondary::transport::http::{
    HttpClient, HttpClientConfig, HttpReply, HttpStream,
};
use protocol_droid::application::bridge::{
    BridgeError, BridgePeer, Identity, OpenedUpstream, SendFuture, Upstream, UpstreamError,
};
use protocol_droid::application::session::IdSource;
use protocol_droid::domain::session::SessionId;
use protocol_droid::domain::{
    Agreement, DegradePolicy, Feature, FeatureProfile, Leg, ProtocolVersion,
};
use serde_json::{Map, Value, json};

type TestError = Box<dyn std::error::Error>;

/// How long a client waits for a bridge to say something before the test calls
/// it silence. Generous, because a CI machine is slow; short enough that a
/// genuinely missing frame is reported in seconds.
const WAIT: std::time::Duration = std::time::Duration::from_secs(10);

fn fail(message: impl Into<String>) -> TestError {
    message.into().into()
}

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"wired"}}}
"#;

const LIST_TOOLS: &str = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;

/// An agreement both peers can actually be held to, at a revision whose
/// handshake still exists.
fn agreeing() -> Agreement {
    let version = ProtocolVersion::V2025_11_25;
    let tools = FeatureProfile::empty().with(Feature::Tools);
    Agreement::between(Leg::new(version, tools), Leg::new(version, tools))
}

// ---------------------------------------------------------------------------
// The upstream: a double standing in for the server on the far side
// ---------------------------------------------------------------------------

/// A shared handle onto what the bridge forwarded, so a test can read it after
/// the loop has moved the peer into the live `Bridge` — which it always does.
type Seen = Arc<std::sync::Mutex<Vec<String>>>;

/// Records what the bridge forwarded, and answers with a tool list the test
/// recognises on the wire.
struct Far {
    seen: Seen,
    leg: Leg,
}

impl Upstream for Far {
    fn open(
        &self,
    ) -> impl std::future::Future<Output = Result<OpenedUpstream, UpstreamError>> + Send {
        let seen = Arc::clone(&self.seen);
        let leg = self.leg;
        std::future::ready(Ok(OpenedUpstream::new(leg, Arc::new(Peer { seen }))))
    }
}

/// The far side of the bridge, as a peer the loop can talk to.
struct Peer {
    seen: Seen,
}

impl BridgePeer for Peer {
    fn send(
        &self,
        envelope: Map<String, Value>,
    ) -> SendFuture<'_, Result<Map<String, Value>, BridgeError>> {
        Box::pin(async move {
            let method = envelope
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let Ok(mut seen) = self.seen.lock() else {
                return Err(BridgeError::Close("the recording was poisoned".to_owned()));
            };
            seen.push(method);
            let id = envelope.get("id").cloned().unwrap_or(Value::Null);
            Ok(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "tools": [{ "name": "echo", "description": "d", "inputSchema": {} }] }
            })
            .as_object()
            .cloned()
            .unwrap_or_default())
        })
    }

    fn notify(&self, envelope: Map<String, Value>) -> SendFuture<'_, Result<(), BridgeError>> {
        Box::pin(async move {
            let _ = envelope;
            Ok(())
        })
    }

    fn close(&self) -> SendFuture<'_, Result<(), BridgeError>> {
        Box::pin(async { Ok(()) })
    }
}

// ---------------------------------------------------------------------------
// A bridge on a socket
// ---------------------------------------------------------------------------

/// Hands out a fixed sequence, so a test can say which id it expects instead of
/// discovering it.
struct Counting(std::sync::atomic::AtomicU64);

impl IdSource for Counting {
    fn next_session_id(&self) -> SessionId {
        SessionId::new(self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1)
    }
}

struct Live {
    url: String,
    seen: Seen,
}

impl Live {
    async fn start() -> Result<Self, TestError> {
        let seen: Seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let ids: Arc<dyn IdSource + Send + Sync> =
            Arc::new(Counting(std::sync::atomic::AtomicU64::new(0)));
        let sessions: Arc<SessionStore<_>> = Arc::new(SessionStore::new(ids));
        let config = ServerConfig::new()?.with_security(SecurityHeaders::default());
        let bridge = HttpBridge::new(
            config,
            sessions,
            Far {
                seen: Arc::clone(&seen),
                leg: *agreeing().upstream(),
            },
            Identity::new("wire-test", "0.0.0"),
            DegradePolicy::default(),
            WireToolResults,
        )?;
        let router = bridge.into_router()?;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        // Bound before this is spawned, so a connection refused here would be a
        // harness bug rather than a race between bind and serve.
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });

        Ok(Self {
            url: format!("http://127.0.0.1:{port}/mcp"),
            seen,
        })
    }

    fn client(&self) -> Result<HttpClient, TestError> {
        Ok(HttpClient::new(HttpClientConfig::new(&self.url)?)?)
    }

    /// The methods the bridge forwarded to its upstream, in order.
    fn forwarded(&self) -> Result<Vec<String>, TestError> {
        let seen = self
            .seen
            .lock()
            .map_err(|_| fail("the recording was poisoned"))?
            .clone();
        Ok(seen)
    }
}

/// A `POST` into a bridge is `202` and an empty body — never the reply.
///
/// Asserted rather than assumed. A bridge that answered its handshake in the
/// `POST` body would still work for a client that only knew to read it there,
/// and would break every client that followed the SSE contract, which is the
/// one the specification describes. This is where that shows up.
fn require_accepted(reply: HttpReply) -> Result<(), TestError> {
    match reply {
        HttpReply::Stream(_) => Err(fail(
            "the POST was answered with a stream; a message POST is accepted, \
             and the reply is on the session's own stream",
        )),
        HttpReply::Document(response) => {
            if response.status() != axum::http::StatusCode::ACCEPTED {
                return Err(fail(format!(
                    "the POST answered {}; a bridge accepts it",
                    response.status()
                )));
            }
            if !response.body().is_empty() {
                return Err(fail(format!(
                    "the POST answered {} bytes; a bridge answers on the stream",
                    response.body().len()
                )));
            }
            Ok(())
        }
    }
}

/// The next frame on a client's own stream, or what was not one.
///
/// One stream, read in order, never reopened. A tail keeps what it has
/// published so a reconnecting client misses nothing, which means a *fresh*
/// stream replays from the beginning — so a test that opened one per message
/// would read the handshake again and call it the answer. That replay is the
/// right behaviour and the reason this takes the stream rather than the
/// client.
async fn next_frame(stream: &mut HttpStream) -> Result<Value, TestError> {
    // Bounded, because the failure a bridge actually has is silence: a frame
    // that never arrives, because the loop is not running or never saw the
    // message. Without a deadline that is a hung test rather than a failed
    // one, and a hung suite reports nothing about which of the two broke.
    let event = tokio::time::timeout(WAIT, stream.next_event())
        .await
        .map_err(|_| fail("the bridge said nothing within the wait"))?
        .map_err(TestError::from)?
        .ok_or_else(|| fail("the stream ended before the bridge said anything"))?;
    serde_json::from_str(event.data())
        .map_err(|error| fail(format!("the stream carried text, not a frame: {error}")))
}

#[tokio::test]
async fn the_handshake_replies_on_the_stream_and_not_in_the_post() -> Result<(), TestError> {
    // The claim this file exists for. A `POST` opening a conversation is
    // answered `202` and nothing else, and the handshake reply comes back on
    // the session's own stream — not in the `POST` body, which is where a
    // client reading the SSE contract would never look for it.
    let live = Live::start().await?;
    let client = live.client()?;

    require_accepted(client.post_streaming(INITIALIZE.as_bytes(), false).await?)?;

    let mut stream = client.open_stream(None, false).await?;
    let frame = next_frame(&mut stream).await?;
    if frame.get("result").is_none_or(Value::is_null) {
        return Err(fail(format!("the handshake reply had no result: {frame}")));
    }
    // Spoken in the revision the client asked for, not the build's newest —
    // which is the other half of terminating the handshake here.
    if frame
        .pointer("/result/protocolVersion")
        .and_then(Value::as_str)
        != Some("2025-11-25")
    {
        return Err(fail(format!(
            "the bridge answered in another revision: {frame}"
        )));
    }
    if frame.get("id").and_then(Value::as_i64) != Some(1) {
        return Err(fail(format!(
            "the reply was not under the request's id: {frame}"
        )));
    }

    // The handshake is answered *locally*, and nothing has reached the upstream
    // yet. That is the bridge's whole job rather than a shortcut: the
    // downstream speaks the revision this client offered, the upstream speaks
    // whatever it supports, and the translation happens between the two — so
    // proxying the handshake would pin both ends to one revision.
    let forwarded = live.forwarded()?;
    if !forwarded.is_empty() {
        return Err(fail(format!(
            "the handshake was proxied upstream as {forwarded:?}; it is the one \
             message a bridge answers itself"
        )));
    }
    Ok(())
}

#[tokio::test]
async fn a_call_after_the_handshake_is_answered_and_forwarded() -> Result<(), TestError> {
    // The whole round trip, over the wire: a client names its session, the
    // bridge routes the call upstream, and the answer comes back on that
    // client's stream. The tool list is read out of the frame the client
    // received, not out of what the double was configured to send.
    let live = Live::start().await?;
    let client = live.client()?;
    require_accepted(client.post_streaming(INITIALIZE.as_bytes(), false).await?)?;
    let mut stream = client.open_stream(None, false).await?;
    // The handshake off the stream first, so the stream is established before
    // anything is claimed about what it carries next.
    let _ = next_frame(&mut stream).await?;

    require_accepted(client.post_streaming(LIST_TOOLS.as_bytes(), true).await?)?;
    let frame = next_frame(&mut stream).await?;

    let tools = frame
        .get("result")
        .and_then(|r| r.get("tools"))
        .ok_or_else(|| fail(format!("the reply carried no tool list: {frame}")))?;
    let names: Vec<&str> = tools
        .as_array()
        .ok_or_else(|| fail("the tool list was not an array"))?
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect();
    if names != ["echo"] {
        return Err(fail(format!("the client saw the tools {names:?}")));
    }

    // The first thing to cross to the far side, and the only thing — the
    // handshake stopped at the bridge.
    let forwarded = live.forwarded()?;
    if forwarded.as_slice() != ["tools/list"] {
        return Err(fail(format!(
            "the upstream was sent {forwarded:?}; the call was not forwarded"
        )));
    }
    Ok(())
}
