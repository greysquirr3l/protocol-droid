//! An operator learns why a client's bridge session died.
//!
//! The loop used to end in `let _ =`: a client saw its stream close and the
//! operator saw nothing. The report is the operator's half.

#![cfg(feature = "http")]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use protocol_droid::adapters::primary::transport::http::{
    HttpBridge, SecurityHeaders, ServerConfig, SessionReport, SessionStore,
};
use protocol_droid::adapters::secondary::codec::result::WireToolResults;
use protocol_droid::adapters::secondary::transport::http::{HttpClient, HttpClientConfig};
use protocol_droid::application::bridge::{Identity, OpenedUpstream, Upstream, UpstreamError};
use protocol_droid::application::session::IdSource;
use protocol_droid::domain::DegradePolicy;
use protocol_droid::domain::session::SessionId;

type TestError = Box<dyn std::error::Error>;

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"wired"}}}"#;

struct Counting(std::sync::atomic::AtomicU64);

impl IdSource for Counting {
    fn next_session_id(&self) -> SessionId {
        SessionId::new(self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1)
    }
}

/// An upstream nobody can reach.
struct Unreachable;

impl Upstream for Unreachable {
    fn open(
        &self,
    ) -> impl std::future::Future<Output = Result<OpenedUpstream, UpstreamError>> + Send {
        std::future::ready(Err(UpstreamError::Unreachable("connection refused".into())))
    }
}

#[tokio::test]
async fn a_session_that_cannot_reach_its_upstream_is_reported() -> Result<(), TestError> {
    let reports: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = Arc::clone(&reports);
    let ids: Arc<dyn IdSource + Send + Sync> =
        Arc::new(Counting(std::sync::atomic::AtomicU64::new(0)));
    let bridge = HttpBridge::new(
        ServerConfig::new()?.with_security(SecurityHeaders::default()),
        Arc::new(SessionStore::new(ids)),
        Unreachable,
        Identity::new("report-test", "0.0.0"),
        DegradePolicy::default(),
        WireToolResults,
    )?
    .with_session_report(SessionReport::new(move |error| {
        if let Ok(mut seen) = sink.lock() {
            seen.push(error.to_string());
        }
    }));
    let (bound, addr) = bridge.bind("127.0.0.1:0".parse()?).await?;
    tokio::spawn(async move {
        let _ = bound.serve().await;
    });

    let client = HttpClient::new(HttpClientConfig::new(&format!("http://{addr}/mcp"))?)?;
    client.post_streaming(INITIALIZE.as_bytes(), false).await?;

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let seen = reports
            .lock()
            .map_err(|_| "the report was poisoned")?
            .clone();
        if let Some(first) = seen.first() {
            assert!(
                first.contains("connection refused"),
                "the report did not say why: {first}"
            );
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err("the operator was never told the session died".into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
