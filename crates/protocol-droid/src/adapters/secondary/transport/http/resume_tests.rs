//! The two stateful-revision client behaviours `2025-11-25` added: resuming a
//! stream the server closed early, and answering a request the server sent
//! mid-call. Both against a scripted server, because both are about what the
//! *server* does to the client.

use std::convert::Infallible;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use crate::application::client::Client;
use crate::application::client::handler::{
    ConnectOptions, HandlerRefusal, ServerRequestHandler, apply_elicitation_defaults,
};
use crate::domain::ir::JsonObject;
use crate::domain::{Capability, CapabilitySet, SupportedVersions, TransportKind, TransportSpec};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const SESSION: &str = "session-1";

#[derive(Default)]
struct Script {
    call_id: Mutex<Option<Value>>,
    resumed_with: Mutex<Vec<String>>,
    resumed_at: Mutex<Option<Instant>>,
    closed_at: Mutex<Option<Instant>>,
    stream: Mutex<Option<mpsc::UnboundedSender<Bytes>>>,
    answered: Mutex<Option<Value>>,
}

fn json_response(body: &Value) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json"), (header::HeaderName::from_static("mcp-session-id"), SESSION)],
        body.to_string(),
    )
        .into_response()
}

fn sse(body: String) -> Response {
    ([(header::CONTENT_TYPE, "text/event-stream"), (header::HeaderName::from_static("mcp-session-id"), SESSION)], body)
        .into_response()
}

/// Answers `initialize`, `tools/list`, and notifications the same way for both
/// scripts; `call` decides what `tools/call` does.
fn common(script: &Script, request: &Value) -> Option<Response> {
    let id = request.get("id").cloned();
    match request.get("method").and_then(Value::as_str) {
        Some("initialize") => Some(json_response(&json!({
            "jsonrpc": "2.0", "id": id,
            "result": {
                "protocolVersion": "2025-11-25",
                "serverInfo": { "name": "script", "version": "1" },
                "capabilities": { "tools": {} },
            },
        }))),
        Some(method) if method.starts_with("notifications/") => {
            Some(StatusCode::ACCEPTED.into_response())
        }
        Some("tools/call") => {
            *script.call_id.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = id;
            None
        }
        _ if request.get("result").is_some() || request.get("error").is_some() => {
            *script.answered.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(request.clone());
            Some(StatusCode::ACCEPTED.into_response())
        }
        _ => Some(json_response(&json!({ "jsonrpc": "2.0", "id": id, "result": {} }))),
    }
}

async fn spawn(router: Router) -> Result<String, Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok(format!("http://{addr}/mcp"))
}

fn spec(url: String) -> Result<TransportSpec, Box<dyn std::error::Error>> {
    Ok(TransportSpec::new(TransportKind::StreamableHttp, url)?)
}

fn versions() -> Result<SupportedVersions, Box<dyn std::error::Error>> {
    Ok(SupportedVersions::from_slice(&[crate::domain::ProtocolVersion::V2025_11_25])?)
}

// ── SEP-1699: the server closes the stream and says when to come back ──────

async fn closes_early(State(script): State<Arc<Script>>, body: String) -> Response {
    let request: Value = serde_json::from_str(&body).unwrap_or_default();
    if let Some(response) = common(&script, &request) {
        return response;
    }
    *script.closed_at.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Instant::now());
    // The priming event: an id and a retry interval, no data. Then the stream
    // ends with the answer still owed.
    sse("id: event-1\nretry: 200\ndata: \n\n".to_owned())
}

async fn resumed(State(script): State<Arc<Script>>, headers: HeaderMap) -> Response {
    *script.resumed_at.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Instant::now());
    if let Some(id) = headers.get("last-event-id").and_then(|v| v.to_str().ok()) {
        script.resumed_with.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(id.to_owned());
    }
    let id = script.call_id.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let result = json!({
        "jsonrpc": "2.0", "id": id,
        "result": { "content": [{ "type": "text", "text": "resumed" }] },
    });
    sse(format!("id: event-2\nevent: message\ndata: {result}\n\n"))
}

#[tokio::test]
async fn a_stream_closed_early_is_resumed_after_the_retry_interval() -> TestResult {
    let script = Arc::new(Script::default());
    let router = Router::new()
        .route("/mcp", post(closes_early).get(resumed))
        .with_state(Arc::clone(&script));
    let url = spawn(router).await?;
    let client = Client::connect_with(&spec(url)?, &versions()?, ConnectOptions::new()).await?;

    let answered = client.call_tool("t", &JsonObject::default()).await?;
    assert!(answered.as_str().contains("resumed"), "{}", answered.as_str());

    let resumed_with = script.resumed_with.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    assert_eq!(resumed_with, ["event-1"], "the last id is sent back verbatim");

    let closed = script.closed_at.lock().unwrap_or_else(std::sync::PoisonError::into_inner).ok_or("never closed")?;
    let came_back = script.resumed_at.lock().unwrap_or_else(std::sync::PoisonError::into_inner).ok_or("never resumed")?;
    // 50 ms of tolerance, the same slack the suite allows for a timer firing
    // early.
    assert!(
        came_back.duration_since(closed) + Duration::from_millis(50) >= Duration::from_millis(200),
        "the client came back after {:?}, before the 200 ms the server asked for",
        came_back.duration_since(closed)
    );
    Ok(())
}

// ── a request the server sends mid-call ─────────────────────────────────────

async fn asks(State(script): State<Arc<Script>>, body: String) -> Response {
    let request: Value = serde_json::from_str(&body).unwrap_or_default();
    if let Some(response) = common(&script, &request) {
        // The client's reply to the question arrives as its own POST; once it
        // does, the call's stream gets its final result.
        if request.get("result").is_some() {
            let id = script.call_id.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
            let tx = script.stream.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
            if let Some(tx) = tx {
                let done = json!({ "jsonrpc": "2.0", "id": id, "result": { "content": [{ "type": "text", "text": "done" }] } });
                let _ = tx.send(Bytes::from(format!("event: message\ndata: {done}\n\n")));
            }
        }
        return response;
    }
    let (tx, rx) = mpsc::unbounded_channel::<Bytes>();
    let question = json!({
        "jsonrpc": "2.0", "id": 99, "method": "elicitation/create",
        "params": {
            "message": "Details?",
            "requestedSchema": { "type": "object", "properties": {
                "name": { "type": "string", "default": "John Doe" },
                "age": { "type": "integer", "default": 30 },
            }},
        },
    });
    let _ = tx.send(Bytes::from(format!("event: message\ndata: {question}\n\n")));
    *script.stream.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(tx);
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|bytes| (Ok::<Bytes, Infallible>(bytes), rx))
    });
    (
        [(header::CONTENT_TYPE, "text/event-stream"), (header::HeaderName::from_static("mcp-session-id"), SESSION)],
        Body::from_stream(stream),
    )
        .into_response()
}

/// Accepts every elicitation with nothing filled in, as the suite's client does.
#[derive(Debug)]
struct EmptyAnswers;

impl ServerRequestHandler for EmptyAnswers {
    fn handle<'a>(
        &'a self,
        method: &'a str,
        params: &'a JsonObject,
    ) -> Pin<Box<dyn Future<Output = Result<JsonObject, HandlerRefusal>> + Send + 'a>> {
        Box::pin(async move {
            if method != "elicitation/create" {
                return Err(HandlerRefusal::unsupported(method));
            }
            let accepted = JsonObject::new(r#"{"action":"accept","content":{}}"#).unwrap_or_default();
            Ok(apply_elicitation_defaults(params, &accepted))
        })
    }
}

#[tokio::test]
async fn a_request_the_server_sends_mid_call_is_answered_with_defaults_applied() -> TestResult {
    let script = Arc::new(Script::default());
    let router = Router::new().route("/mcp", post(asks)).with_state(Arc::clone(&script));
    let url = spawn(router).await?;
    let client = Client::connect_with(
        &spec(url)?,
        &versions()?,
        ConnectOptions::new()
            .declaring(CapabilitySet::empty().with(Capability::Elicitation))
            .answering(Arc::new(EmptyAnswers)),
    )
    .await?;

    let answered = tokio::time::timeout(
        Duration::from_secs(5),
        client.call_tool("t", &JsonObject::default()),
    )
    .await
    .map_err(|_| "the call hung waiting for an answer nobody gave")??;
    assert!(answered.as_str().contains("done"), "{}", answered.as_str());

    let reply = script.answered.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone().ok_or("no reply")?;
    assert_eq!(reply.get("id"), Some(&json!(99)), "the reply carries the question's id");
    assert_eq!(reply.pointer("/result/content/name"), Some(&json!("John Doe")));
    assert_eq!(reply.pointer("/result/content/age"), Some(&json!(30)));
    Ok(())
}
