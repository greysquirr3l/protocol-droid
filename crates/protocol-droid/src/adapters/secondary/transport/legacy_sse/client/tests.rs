//! The `2024-11-05` client, against servers that misbehave in specific ways.
//!
//! The handshake is served by the real [`LegacyRouter`] wherever it can be,
//! because the claim there is that two halves of this crate agree. The refusals
//! need a peer that misbehaves, and the real router has no reason to, so those
//! use a scripted server — a test that invented a misbehaving real server would
//! be testing its own stub.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::adapters::primary::transport::http::{Refusal, ServerConfig, SessionStore};
use crate::adapters::primary::transport::legacy_sse::{
    DEFAULT_MESSAGE_PATH, DEFAULT_STREAM_PATH, LegacyDispatcher, LegacyRouter,
};
use crate::application::session::IdSource;
use crate::domain::McpSessionId;
use crate::domain::registry::AuthContext;
use crate::domain::session::SessionId;
use axum::http::StatusCode;
use serde_json::{Value, json};

use super::{ClientFrame, LegacySseClient, LegacySseConfig, LegacySseError, resolve};
use crate::adapters::secondary::transport::test_support::ScriptedServer;
use crate::domain::MessageEndpoint;

type TestError = Box<dyn std::error::Error>;

fn fail(message: impl Into<String>) -> TestError {
    message.into().into()
}

fn require(condition: bool, because: &str) -> Result<(), TestError> {
    if condition {
        Ok(())
    } else {
        Err(fail(because.to_owned()))
    }
}

/// Takes references because it only compares: a by-value signature makes every
/// caller clone a field to print it in a message it may never build.
fn require_eq<T>(got: &T, wanted: &T, because: &str) -> Result<(), TestError>
where
    T: PartialEq + std::fmt::Debug,
{
    if got == wanted {
        Ok(())
    } else {
        Err(fail(format!("{because}: wanted {wanted:?}, got {got:?}")))
    }
}

/// The real `2024-11-05` server.
///
/// A secondary adapter's test driving a primary one is the one direction these
/// two layers do not otherwise face, and it is the right one: both halves of the
/// `2024-11-05` transport are this crate's, so the round trip is a claim about
/// agreement rather than about a third party being reasonable.
struct Live {
    url: String,
    calls: Arc<AtomicU64>,
}

impl Live {
    async fn start() -> Result<Self, TestError> {
        let calls = Arc::new(AtomicU64::new(0));
        let router = LegacyRouter::new(
            ServerConfig::new()?,
            Arc::new(SessionStore::new(Arc::new(Counting(AtomicU64::new(0))))),
            Echo {
                calls: Arc::clone(&calls),
            },
        )
        .into_router()?;

        // Port 0, so the OS picks one and two tests can run at once. A
        // hard-coded port is a test that fails the moment anything else takes
        // it, and the failure looks like a transport bug.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(Self {
            url: format!("http://127.0.0.1:{port}{DEFAULT_STREAM_PATH}"),
            calls,
        })
    }

    async fn client(&self) -> Result<LegacySseClient, LegacySseError> {
        LegacySseClient::connect(LegacySseConfig::new(&self.url)?).await
    }
}

/// Answers every frame with a result carrying the frame back, so a reply that
/// does not contain what was sent is visible from the client side alone.
struct Echo {
    calls: Arc<AtomicU64>,
}

impl LegacyDispatcher for Echo {
    fn dispatch(
        &self,
        _auth: &AuthContext,
        _session: &McpSessionId,
        frame: &Value,
    ) -> Result<Vec<Value>, Refusal> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(vec![json!({
            "jsonrpc": "2.0",
            "id": frame.get("id").cloned().unwrap_or(Value::Null),
            "result": { "echoed": frame }
        })])
    }
}

/// Hands out a fixed prefix, so a test can tell one session from another.
struct Counting(AtomicU64);

impl IdSource for Counting {
    fn next_session_id(&self) -> SessionId {
        SessionId::new(self.0.fetch_add(1, Ordering::SeqCst))
    }
}
/// The point of the transport: the server names where to post, the client goes
/// there, and the answer comes back on the stream rather than on the POST.
///
/// A round trip rather than an inspection, because the interesting failure is
/// the two halves disagreeing — a client that reads the session out of the
/// wrong place, or a server that writes a path the client's parser cannot take,
/// both produce a `200` and an `endpoint` that looks right in isolation.
#[tokio::test]
async fn a_request_posted_to_the_advertised_endpoint_is_answered_on_the_stream()
-> Result<(), TestError> {
    let live = Live::start().await?;
    let client = live.client().await?;

    require_eq(
        &client.message_endpoint().path(),
        &DEFAULT_MESSAGE_PATH,
        "the endpoint the server advertised is the path the client posts to",
    )?;

    client
        .send(br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
        .await?;
    let reply = client
        .next()
        .await?
        .ok_or("the stream ended without a reply")?;
    let ClientFrame::Message(value) = reply else {
        return Err(fail("expected a message frame carrying the result"));
    };
    require_eq(
        &value.pointer("/result/echoed/id").and_then(Value::as_u64),
        &Some(1),
        "the reply that came back is the reply to what was posted",
    )?;
    require_eq(
        &live.calls.load(Ordering::SeqCst),
        &1,
        "and it was dispatched exactly once",
    )?;
    Ok(())
}

/// An unnamed frame is a message, which is the case a naive filter drops.
///
/// The specification calls these `message` events, and this crate's server
/// writes them with **no** `event:` field, because that is the SSE standard's
/// own default. A client written as `if event.is_named(MESSAGE)` passes every
/// test whose fixture happens to name its frames, and receives nothing from a
/// conforming server.
#[tokio::test]
async fn an_unnamed_frame_off_the_wire_arrives_as_a_message() -> Result<(), TestError> {
    let live = Live::start().await?;
    let client = live.client().await?;
    client
        .send(br#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#)
        .await?;
    let reply = client
        .next()
        .await?
        .ok_or("the stream ended without a reply")?;
    require(
        matches!(reply, ClientFrame::Message(_)),
        "an unnamed frame is a message, because unnamed IS the standard's default name",
    )
}
/// A frame explicitly named `message` is also a message.
///
/// The other direction, and the one a real `2024-11-05` SDK exercises: some
/// servers write the name out rather than relying on the default. A client that
/// only understood the unnamed form would work against this crate's server and
/// fail against those.
#[tokio::test]
async fn a_frame_explicitly_named_message_is_also_a_message() -> Result<(), TestError> {
    // The handshake first: the client refuses to connect to a stream that
    // opens with anything else, so a stream that went straight to the frame
    // under test would never reach `next`.
    let server = ScriptedServer::start(vec![
        format!("event: endpoint\ndata: {DEFAULT_MESSAGE_PATH}\n\n"),
        "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{}}\n\n".to_owned(),
    ])
    .await?;
    let client = LegacySseClient::connect(LegacySseConfig::new(server.url())?).await?;
    let frame = client
        .next()
        .await?
        .ok_or("the stream ended without a frame")?;
    require(
        matches!(frame, ClientFrame::Message(_)),
        "a frame named `message` is a message, spelled out or defaulted",
    )
}

/// A server that opens with a `message` has skipped the handshake.
///
/// There is nowhere to post, so the client stops. A client that guessed a path
/// here would be the bug: it would work against this server and against none of
/// the servers that name a different one.
#[tokio::test]
async fn a_stream_that_opens_with_a_message_is_refused() -> Result<(), TestError> {
    let server = ScriptedServer::start(vec![
        "data: {\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{}}\n\n".to_owned(),
    ])
    .await?;
    let client = LegacySseClient::connect(LegacySseConfig::new(server.url())?).await;
    let Err(LegacySseError::FirstEventNotEndpoint { got }) = client else {
        return Err(fail("a stream that skipped the handshake must not connect"));
    };
    require(
        got.contains("message frame"),
        "the refusal names what actually arrived rather than saying `unknown`",
    )
}

/// A server that names another host is refused before a byte is sent.
///
/// The whole reason an `endpoint` payload is a domain type. The server chooses
/// this URL, so a client that obeyed it would be posting — with whatever
/// credentials it holds — wherever a hostile or compromised server said.
#[tokio::test]
async fn an_endpoint_naming_another_host_is_refused() -> Result<(), TestError> {
    for hostile in [
        "https://elsewhere.example/steal",
        "//elsewhere.example/steal",
    ] {
        let server =
            ScriptedServer::start(vec![format!("event: endpoint\ndata: {hostile}\n\n")]).await?;
        let client = LegacySseClient::connect(LegacySseConfig::new(server.url())?).await;
        let Err(error) = client else {
            return Err(fail(format!("{hostile} must not be usable as an endpoint")));
        };
        require(
            matches!(error, LegacySseError::Endpoint { .. }),
            "the refusal is the endpoint parser's, not a later connection failure",
        )?;
    }
    Ok(())
}

/// A `200` that is not an event stream is a URL that is not an MCP server, and
/// it is worth saying so at the point it shows rather than as a parse failure
/// three steps later.
#[tokio::test]
async fn a_get_answered_with_json_is_not_a_stream() -> Result<(), TestError> {
    let server = ScriptedServer::start(Vec::new())
        .await?
        .with_get_status(StatusCode::OK, "application/json");
    let client = LegacySseClient::connect(LegacySseConfig::new(server.url())?).await;
    require(
        matches!(client, Err(LegacySseError::NotAStream { .. })),
        "a 200 that is not an event stream is refused by name",
    )
}

/// The property the origin check in [`resolve`] stands for.
///
/// The check itself cannot be driven from here — no origin-relative target
/// joins onto another origin, which is the point of the parser — so the *claim*
/// is asserted rather than the branch. A rewrite of the join that stopped
/// preserving the origin would show up as this failing, which is what makes the
/// check worth keeping even though it never fires today.
#[test]
fn an_origin_relative_target_never_leaves_the_origin() -> Result<(), TestError> {
    use core::str::FromStr;

    for base in [
        "http://127.0.0.1:8080/sse",
        "https://mcp.example/base/sse",
        "https://mcp.example:8443/sse?x=1",
    ] {
        for target in [
            "/messages",
            "/messages?sessionId=abc",
            "/a/b/c?x=1&y=2",
            "/",
        ] {
            let endpoint = MessageEndpoint::from_str(target)?;
            let resolved = resolve(base, &endpoint)?;
            let want = reqwest::Url::parse(base)?;
            if resolved.origin() != want.origin() {
                return Err(fail(format!(
                    "{base} + {target} left the origin: {resolved}"
                )));
            }
        }
    }
    Ok(())
}

/// A target naming another host is refused before it is ever joined, so the
/// join is not the only thing standing between a peer and a redirect.
#[test]
fn a_target_naming_another_host_never_reaches_the_join() -> Result<(), TestError> {
    use core::str::FromStr;

    for hostile in [
        "https://elsewhere.example/steal",
        "//elsewhere.example/steal",
    ] {
        if let Ok(endpoint) = MessageEndpoint::from_str(hostile) {
            return Err(fail(format!("{hostile} parsed as {endpoint}")));
        }
    }
    Ok(())
}
