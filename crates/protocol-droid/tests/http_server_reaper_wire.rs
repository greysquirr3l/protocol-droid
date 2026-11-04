//! A served MCP endpoint drops the sessions its clients abandon.
//!
//! The builder reaps by default; these tests read that off a real socket.

#![cfg(feature = "http")]

use std::net::SocketAddr;
use std::time::Duration;

use protocol_droid::adapters::primary::transport::http::HttpBuilder;
use protocol_droid::adapters::primary::transport::http::reaper::IdleReaper;
use protocol_droid::adapters::secondary::transport::http::{
    HttpClient, HttpClientConfig, HttpClientError,
};
use protocol_droid::application::server::Server;

type TestError = Box<dyn std::error::Error>;

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"wired"}}}"#;
const PING: &str = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;

const IDLE: Duration = Duration::from_millis(300);

/// Serves on a free loopback port and returns a client pointed at it.
async fn serve(builder: HttpBuilder) -> Result<HttpClient, TestError> {
    let any = SocketAddr::from(([127, 0, 0, 1], 0));
    let listener = tokio::net::TcpListener::bind(any).await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = builder.serve_on(listener).await;
    });
    Ok(HttpClient::new(HttpClientConfig::new(&format!(
        "http://{addr}/mcp"
    ))?)?)
}

fn builder() -> HttpBuilder {
    HttpBuilder::new(Server::builder("reaper-test").build())
}

fn reaper() -> Result<IdleReaper, TestError> {
    Ok(IdleReaper::new(IDLE)?.with_interval(Duration::from_millis(25)))
}

#[tokio::test]
async fn an_abandoned_session_is_gone_by_its_next_request() -> Result<(), TestError> {
    let client = serve(builder().with_idle_reaper(reaper()?)).await?;
    client.post_streaming(INITIALIZE.as_bytes(), false).await?;

    tokio::time::sleep(IDLE * 4).await;

    let after = client.post_streaming(PING.as_bytes(), true).await;
    assert!(
        matches!(
            after,
            Err(HttpClientError::Refused { status, .. })
                if status == axum::http::StatusCode::NOT_FOUND
        ),
        "the abandoned session's next request was not a 404: {after:?}"
    );
    Ok(())
}

#[tokio::test]
async fn the_reaper_can_be_turned_off() -> Result<(), TestError> {
    let client = serve(builder().without_idle_reaper()).await?;
    client.post_streaming(INITIALIZE.as_bytes(), false).await?;

    tokio::time::sleep(IDLE * 4).await;

    client.post_streaming(PING.as_bytes(), true).await?;
    Ok(())
}
