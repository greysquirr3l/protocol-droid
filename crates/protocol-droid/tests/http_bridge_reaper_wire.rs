//! A bridge drops the sessions its clients abandon, and only those.
//!
//! Each session holds a task and an upstream connection, so a client that
//! opens one and walks away is a leak with a remote trigger. These tests read
//! the claim off the status codes a client sees on a real socket: the
//! abandoned session's next `POST` is `404`, and a session whose client keeps
//! talking is never touched.

#![cfg(feature = "http")]

use std::sync::Arc;
use std::time::Duration;

use protocol_droid::adapters::primary::transport::http::reaper::IdleReaper;
use protocol_droid::adapters::primary::transport::http::{
    BridgeServeError, HttpBridge, SecurityHeaders, ServerConfig, SessionStore,
};
use protocol_droid::adapters::secondary::clock::MonotonicClock;
use protocol_droid::adapters::secondary::codec::result::WireToolResults;
use protocol_droid::adapters::secondary::transport::http::{
    HttpClient, HttpClientConfig, HttpClientError, HttpReply,
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

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"wired"}}}
"#;
const LIST_TOOLS: &str = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;

/// The window an abandoned session is outwaited against. Only has to be
/// passed, so short is safe.
const IDLE: Duration = Duration::from_millis(300);

/// The window a busy session must survive. A loaded CI runner can stall a test
/// thread for hundreds of milliseconds, and a stall longer than the window is
/// a session legitimately idle, so this is wide against the posting interval.
const BUSY_IDLE: Duration = Duration::from_millis(1500);
const BUSY_INTERVAL: Duration = Duration::from_millis(100);

fn fail(message: impl Into<String>) -> TestError {
    message.into().into()
}

struct Counting(std::sync::atomic::AtomicU64);

impl IdSource for Counting {
    fn next_session_id(&self) -> SessionId {
        SessionId::new(self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1)
    }
}

struct Far;

impl Upstream for Far {
    fn open(
        &self,
    ) -> impl std::future::Future<Output = Result<OpenedUpstream, UpstreamError>> + Send {
        let version = ProtocolVersion::V2025_11_25;
        let tools = FeatureProfile::empty().with(Feature::Tools);
        let leg =
            *Agreement::between(Leg::new(version, tools), Leg::new(version, tools)).upstream();
        std::future::ready(Ok(OpenedUpstream::new(leg, Arc::new(Peer))))
    }
}

struct Peer;

impl BridgePeer for Peer {
    fn send(
        &self,
        envelope: Map<String, Value>,
    ) -> SendFuture<'_, Result<Map<String, Value>, BridgeError>> {
        Box::pin(async move {
            let id = envelope.get("id").cloned().unwrap_or(Value::Null);
            Ok(
                json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": [] } })
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
            )
        })
    }

    fn notify(&self, _envelope: Map<String, Value>) -> SendFuture<'_, Result<(), BridgeError>> {
        Box::pin(async { Ok(()) })
    }

    fn close(&self) -> SendFuture<'_, Result<(), BridgeError>> {
        Box::pin(async { Ok(()) })
    }
}

fn store(
    reaping: bool,
) -> Arc<SessionStore<protocol_droid::adapters::primary::transport::http::BridgeSession>> {
    let ids: Arc<dyn IdSource + Send + Sync> =
        Arc::new(Counting(std::sync::atomic::AtomicU64::new(0)));
    Arc::new(if reaping {
        SessionStore::with_idle_reaper(ids, Arc::new(MonotonicClock::new()))
    } else {
        SessionStore::new(ids)
    })
}

fn bridge(
    sessions: Arc<SessionStore<protocol_droid::adapters::primary::transport::http::BridgeSession>>,
) -> Result<HttpBridge<Far, WireToolResults>, TestError> {
    Ok(HttpBridge::new(
        ServerConfig::new()?.with_security(SecurityHeaders::default()),
        sessions,
        Far,
        Identity::new("reaper-test", "0.0.0"),
        DegradePolicy::default(),
        WireToolResults,
    )?)
}

/// Serves a reaping bridge and returns a client pointed at it.
async fn live(idle: Duration) -> Result<HttpClient, TestError> {
    let bridge = bridge(store(true))?
        .with_idle_reaper(IdleReaper::new(idle)?.with_interval(Duration::from_millis(25)))?;
    let (bound, addr) = bridge.bind("127.0.0.1:0".parse()?).await?;
    tokio::spawn(async move {
        let _ = bound.serve().await;
    });
    Ok(HttpClient::new(HttpClientConfig::new(&format!(
        "http://{addr}/mcp"
    ))?)?)
}

fn status(reply: HttpReply) -> Result<axum::http::StatusCode, TestError> {
    match reply {
        HttpReply::Document(response) => Ok(response.status()),
        HttpReply::Stream(_) => Err(fail("a POST was answered with a stream")),
    }
}

#[tokio::test]
async fn an_abandoned_session_is_gone_by_its_next_post() -> Result<(), TestError> {
    let client = live(IDLE).await?;
    let opened = status(client.post_streaming(INITIALIZE.as_bytes(), false).await?)?;
    assert_eq!(opened, axum::http::StatusCode::ACCEPTED);

    tokio::time::sleep(IDLE * 4).await;

    // The client surfaces a refusal as an error rather than a reply.
    let after = client.post_streaming(LIST_TOOLS.as_bytes(), true).await;
    assert!(
        matches!(
            after,
            Err(HttpClientError::Refused { status, .. })
                if status == axum::http::StatusCode::NOT_FOUND
        ),
        "the abandoned session's next POST was not a 404: {after:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_session_whose_client_keeps_talking_is_not_reaped() -> Result<(), TestError> {
    let client = live(BUSY_IDLE).await?;
    let opened = status(client.post_streaming(INITIALIZE.as_bytes(), false).await?)?;
    assert_eq!(opened, axum::http::StatusCode::ACCEPTED);

    // More than one window in total, never a window of silence.
    for _ in 0..25 {
        tokio::time::sleep(BUSY_INTERVAL).await;
        let reply = status(client.post_streaming(LIST_TOOLS.as_bytes(), true).await?)?;
        assert_eq!(reply, axum::http::StatusCode::ACCEPTED);
    }
    Ok(())
}

#[tokio::test]
async fn a_store_that_cannot_reap_is_refused_rather_than_silently_kept() -> Result<(), TestError> {
    let refused = bridge(store(false))?.with_idle_reaper(IdleReaper::new(IDLE)?);
    assert!(matches!(refused, Err(BridgeServeError::StoreCannotReap)));
    Ok(())
}
