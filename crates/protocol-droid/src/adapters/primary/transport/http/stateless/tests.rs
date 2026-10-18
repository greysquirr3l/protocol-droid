//! The `2026-07-28` stateless lifecycle, over the real router.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures_util::StreamExt as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;

use crate::adapters::primary::transport::http::config::{ServerConfig, Streaming};
use crate::adapters::primary::transport::http::serve::test_routes;
use crate::application::mcp::input_required::{InputRequired, StateKey};
use crate::application::server::{Server, ToolHandler};
use crate::application::session::IdSource;
use crate::domain::ProtocolVersion;
use crate::domain::ir::{JsonObject, LogLevel};
use crate::domain::registry::Tool;
use crate::domain::session::SessionId;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const REVISION: &str = "2026-07-28";

struct Counting(core::sync::atomic::AtomicU64);

impl IdSource for Counting {
    fn next_session_id(&self) -> SessionId {
        SessionId::new(self.0.fetch_add(1, Ordering::SeqCst))
    }
}

fn tool(
    name: &str,
    handler: ToolHandler,
) -> Result<(Tool, ToolHandler), Box<dyn std::error::Error>> {
    Ok((Tool::new(name, r#"{"type":"object"}"#)?, handler))
}

fn text(message: &str) -> JsonObject {
    JsonObject::from_value(json!({ "content": [{ "type": "text", "text": message }] }))
        .unwrap_or_default()
}

/// A server with the tools these tests call, behind the stateless router.
fn router(
    streaming: Streaming,
) -> Result<(axum::Router, Arc<AtomicUsize>), Box<dyn std::error::Error>> {
    let runs = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&runs);
    let mut builder = Server::builder("stateless").request_state_key(StateKey::from_bytes([9; 32]));
    for (definition, handler) in [
        tool(
            "count",
            Arc::new(move |_ctx, _args| {
                let runs = Arc::clone(&counted);
                Box::pin(async move {
                    runs.fetch_add(1, Ordering::SeqCst);
                    text("counted")
                })
            }),
        )?,
        tool(
            "progress",
            Arc::new(|ctx, _args| {
                Box::pin(async move {
                    let update =
                        JsonObject::new(r#"{"progress":50,"total":100}"#).unwrap_or_default();
                    let _ = ctx.report_progress(&update).await;
                    text("done")
                })
            }),
        )?,
        tool(
            "logs",
            Arc::new(|ctx, _args| {
                Box::pin(async move {
                    let _ = ctx.log(LogLevel::Info, "hello").await;
                    text("logged")
                })
            }),
        )?,
        tool(
            "needs_sampling",
            Arc::new(|_ctx, _args| {
                Box::pin(async move {
                    let ask =
                        JsonObject::new(r#"{"messages":[],"maxTokens":1}"#).unwrap_or_default();
                    InputRequired::new().sample("q", &ask).into_result()
                })
            }),
        )?,
        tool(
            "asks_name",
            Arc::new(|ctx, _args| {
                Box::pin(async move {
                    if let Some(answer) = ctx.input_response("name") {
                        let state = ctx.request_state().unwrap_or("none").to_owned();
                        return text(&format!("got {} with {state}", answer.as_str()));
                    }
                    let ask = JsonObject::new(
                        r#"{"message":"Name?","requestedSchema":{"type":"object"}}"#,
                    )
                    .unwrap_or_default();
                    let sealed = ctx.seal_state("round-1").unwrap_or_default();
                    InputRequired::new()
                        .elicit("name", &ask)
                        .with_state(sealed)
                        .into_result()
                })
            }),
        )?,
    ] {
        builder = builder.tool(definition, handler);
    }
    let config = ServerConfig::new()?.with_streaming(streaming);
    let router = test_routes(
        builder.build(),
        ProtocolVersion::V2025_11_25,
        Some(config),
        Some(Arc::new(Counting(core::sync::atomic::AtomicU64::new(100)))),
    )?;
    Ok((router, runs))
}

fn meta(capabilities: &Value) -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": REVISION,
        "io.modelcontextprotocol/clientCapabilities": capabilities,
    })
}

/// A request with the headers a conforming client sends.
fn request(id: i64, method: &str, params: &Value) -> Request<Body> {
    request_with(id, method, params, &[])
}

fn request_with(
    id: i64,
    method: &str,
    params: &Value,
    overrides: &[(&str, &str)],
) -> Request<Body> {
    let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    let mut headers: Vec<(String, String)> = vec![
        ("host".into(), "localhost".into()),
        ("content-type".into(), "application/json".into()),
        (
            "accept".into(),
            "application/json, text/event-stream".into(),
        ),
        ("mcp-protocol-version".into(), REVISION.into()),
        ("mcp-method".into(), method.into()),
    ];
    if let Some(name) = params
        .get("name")
        .or_else(|| params.get("uri"))
        .and_then(Value::as_str)
    {
        headers.push(("mcp-name".into(), name.into()));
    }
    for (name, value) in overrides {
        headers.retain(|(existing, _)| existing != name);
        if !value.is_empty() {
            headers.push(((*name).into(), (*value).into()));
        }
    }
    let mut builder = Request::builder().method("POST").uri("/mcp");
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    builder
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| Request::new(Body::empty()))
}

/// Every JSON document in a response, whether it is one document or a stream.
async fn documents(
    response: axum::response::Response,
) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let mut chunks = response.into_body().into_data_stream();
    let mut text = String::new();
    while let Some(chunk) = chunks.next().await {
        text.push_str(std::str::from_utf8(&chunk?)?);
    }
    if let Ok(document) = serde_json::from_str::<Value>(&text) {
        return Ok(vec![document]);
    }
    Ok(text
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|data| serde_json::from_str(data.trim()).ok())
        .collect())
}

async fn send(
    router: &axum::Router,
    request: Request<Body>,
) -> Result<(StatusCode, Vec<Value>), Box<dyn std::error::Error>> {
    let response = router.clone().oneshot(request).await?;
    let status = response.status();
    Ok((status, documents(response).await?))
}

fn error_code(document: &Value) -> Option<i64> {
    document.pointer("/error/code").and_then(Value::as_i64)
}

#[tokio::test]
async fn a_request_with_no_meta_is_invalid_params_with_400() -> TestResult {
    let (router, _) = router(Streaming::EventStream)?;
    let (status, docs) = send(&router, request(101, "server/discover", &json!({}))).await?;
    let first = docs.first().ok_or("no reply")?;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(first), Some(-32602));
    assert_eq!(first.get("id"), Some(&json!(101)), "the id is echoed");
    Ok(())
}

#[tokio::test]
async fn a_meta_missing_capabilities_is_invalid_params() -> TestResult {
    let (router, _) = router(Streaming::EventStream)?;
    let params = json!({ "_meta": { "io.modelcontextprotocol/protocolVersion": REVISION } });
    let (status, docs) = send(&router, request(104, "server/discover", &params)).await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(docs.first().ok_or("no reply")?), Some(-32602));
    Ok(())
}

#[tokio::test]
async fn client_info_is_optional() -> TestResult {
    let (router, _) = router(Streaming::EventStream)?;
    let params = json!({ "_meta": meta(&json!({})) });
    let (status, docs) = send(&router, request(105, "server/discover", &params)).await?;
    let first = docs.first().ok_or("no reply")?;
    assert_eq!(status, StatusCode::OK);
    assert!(first.pointer("/result/supportedVersions").is_some());
    assert!(
        first
            .pointer("/result/_meta/io.modelcontextprotocol~1serverInfo/name")
            .is_some(),
        "the server identifies itself: {first}"
    );
    Ok(())
}

#[tokio::test]
async fn a_header_that_disagrees_with_meta_is_a_header_mismatch() -> TestResult {
    let (router, _) = router(Streaming::EventStream)?;
    let mut params = json!({ "_meta": meta(&json!({})) });
    if let Some(meta) = params.get_mut("_meta").and_then(Value::as_object_mut) {
        meta.insert(
            "io.modelcontextprotocol/protocolVersion".into(),
            json!("v999.0.0"),
        );
    }
    let (status, docs) = send(&router, request(302, "server/discover", &params)).await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(docs.first().ok_or("no reply")?), Some(-32020));
    Ok(())
}

#[tokio::test]
async fn an_unsupported_version_echoes_what_was_asked() -> TestResult {
    let (router, _) = router(Streaming::EventStream)?;
    let mut params = json!({ "_meta": meta(&json!({})) });
    if let Some(meta) = params.get_mut("_meta").and_then(Value::as_object_mut) {
        meta.insert(
            "io.modelcontextprotocol/protocolVersion".into(),
            json!("v999.0.0"),
        );
    }
    let (status, docs) = send(
        &router,
        request_with(
            301,
            "server/discover",
            &params,
            &[("mcp-protocol-version", "v999.0.0")],
        ),
    )
    .await?;
    let first = docs.first().ok_or("no reply")?;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(first), Some(-32022));
    assert_eq!(
        first.pointer("/error/data/requested"),
        Some(&json!("v999.0.0"))
    );
    assert!(
        first
            .pointer("/error/data/supported")
            .and_then(Value::as_array)
            .is_some_and(|supported| supported.contains(&json!(REVISION)))
    );
    assert_eq!(first.get("id"), Some(&json!(301)));
    Ok(())
}

#[tokio::test]
async fn removed_and_unknown_methods_are_404() -> TestResult {
    let (router, _) = router(Streaming::EventStream)?;
    for method in [
        "initialize",
        "ping",
        "logging/setLevel",
        "resources/subscribe",
        "resources/unsubscribe",
        "unknown/method",
    ] {
        let params = json!({ "_meta": meta(&json!({})) });
        let (status, docs) = send(&router, request(500, method, &params)).await?;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method}");
        assert_eq!(
            error_code(docs.first().ok_or("no reply")?),
            Some(-32601),
            "{method}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_method_header_that_disagrees_is_a_header_mismatch() -> TestResult {
    let (router, _) = router(Streaming::EventStream)?;
    let params = json!({ "_meta": meta(&json!({})) });
    let (status, docs) = send(
        &router,
        request_with(7, "tools/list", &params, &[("mcp-method", "prompts/list")]),
    )
    .await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(docs.first().ok_or("no reply")?), Some(-32020));
    Ok(())
}

#[tokio::test]
async fn the_name_header_is_checked_and_may_be_base64() -> TestResult {
    let (router, _) = router(Streaming::EventStream)?;
    let params = json!({ "_meta": meta(&json!({})), "name": "count", "arguments": {} });
    let (status, _) = send(
        &router,
        request_with(8, "tools/call", &params, &[("mcp-name", "other")]),
    )
    .await?;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a wrong name is refused");

    let (status, _) = send(
        &router,
        request_with(9, "tools/call", &params, &[("mcp-name", "")]),
    )
    .await?;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a missing name is refused");

    let (status, docs) = send(
        &router,
        request_with(
            10,
            "tools/call",
            &params,
            &[("mcp-name", "=?base64?Y291bnQ=?=")],
        ),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{docs:?}");
    Ok(())
}

#[tokio::test]
async fn two_clients_reusing_an_id_both_run() -> TestResult {
    let (router, runs) = router(Streaming::EventStream)?;
    let params = json!({ "_meta": meta(&json!({})), "name": "count", "arguments": {} });
    for _ in 0..2 {
        let (status, _) = send(&router, request(1, "tools/call", &params)).await?;
        assert_eq!(status, StatusCode::OK);
    }
    assert_eq!(
        runs.load(Ordering::SeqCst),
        2,
        "the second call was answered from the first's recording"
    );
    Ok(())
}

#[tokio::test]
async fn progress_streams_before_the_result_with_the_token_as_sent() -> TestResult {
    let (router, _) = router(Streaming::EventStream)?;
    let mut meta = meta(&json!({}));
    if let Some(object) = meta.as_object_mut() {
        object.insert("progressToken".into(), json!(42));
    }
    let params = json!({ "_meta": meta, "name": "progress", "arguments": {} });
    let (status, docs) = send(&router, request(11, "tools/call", &params)).await?;
    assert_eq!(status, StatusCode::OK);
    let progress = docs
        .iter()
        .find(|doc| doc.get("method") == Some(&json!("notifications/progress")))
        .ok_or("no progress notification")?;
    assert_eq!(
        progress.pointer("/params/progressToken"),
        Some(&json!(42)),
        "a number stays a number"
    );
    assert_eq!(
        docs.last().and_then(|doc| doc.get("id")),
        Some(&json!(11)),
        "the result comes last"
    );
    Ok(())
}

#[tokio::test]
async fn no_log_records_without_a_log_level() -> TestResult {
    let (router, _) = router(Streaming::EventStream)?;
    let params = json!({ "_meta": meta(&json!({})), "name": "logs", "arguments": {} });
    let (_, docs) = send(&router, request(12, "tools/call", &params)).await?;
    assert!(
        docs.iter()
            .all(|doc| doc.get("method") != Some(&json!("notifications/message"))),
        "{docs:?}"
    );

    let mut opted = meta(&json!({}));
    if let Some(object) = opted.as_object_mut() {
        object.insert("io.modelcontextprotocol/logLevel".into(), json!("debug"));
    }
    let params = json!({ "_meta": opted, "name": "logs", "arguments": {} });
    let (_, docs) = send(&router, request(13, "tools/call", &params)).await?;
    assert!(
        docs.iter()
            .any(|doc| doc.get("method") == Some(&json!("notifications/message"))),
        "{docs:?}"
    );
    Ok(())
}

#[tokio::test]
async fn asking_for_an_undeclared_capability_is_32021_with_400() -> TestResult {
    let (router, _) = router(Streaming::EventStream)?;
    let params = json!({ "_meta": meta(&json!({})), "name": "needs_sampling", "arguments": {} });
    let (status, docs) = send(&router, request(401, "tools/call", &params)).await?;
    let first = docs.first().ok_or("no reply")?;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(first), Some(-32021));
    assert_eq!(
        first.pointer("/error/data/requiredCapabilities/sampling"),
        Some(&json!({})),
        "requiredCapabilities is a ClientCapabilities object: {first}"
    );
    Ok(())
}

#[tokio::test]
async fn a_round_trip_carries_answers_and_verified_state() -> TestResult {
    let (router, _) = router(Streaming::EventStream)?;
    let caps = json!({ "elicitation": {} });
    let params = json!({ "_meta": meta(&caps), "name": "asks_name", "arguments": {} });
    let (status, docs) = send(&router, request(20, "tools/call", &params)).await?;
    let first = docs.first().ok_or("no reply")?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        first.pointer("/result/resultType"),
        Some(&json!("input_required")),
        "{first}"
    );
    let state = first
        .pointer("/result/requestState")
        .and_then(Value::as_str)
        .ok_or("no state")?
        .to_owned();

    let retry = json!({
        "_meta": meta(&caps),
        "name": "asks_name",
        "arguments": {},
        "inputResponses": { "name": { "action": "accept", "content": { "name": "Ada" } } },
        "requestState": state,
    });
    let (status, docs) = send(&router, request(21, "tools/call", &retry)).await?;
    let first = docs.first().ok_or("no reply")?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        first.pointer("/result/resultType"),
        Some(&json!("complete"))
    );
    let said = first
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .ok_or("no text")?;
    assert!(said.contains("Ada") && said.contains("round-1"), "{said}");
    Ok(())
}

#[tokio::test]
async fn a_tampered_state_is_refused_before_the_handler_runs() -> TestResult {
    let (router, _) = router(Streaming::EventStream)?;
    let caps = json!({ "elicitation": {} });
    let retry = json!({
        "_meta": meta(&caps),
        "name": "asks_name",
        "arguments": {},
        "inputResponses": { "name": {} },
        "requestState": "00.round-1",
    });
    let (_, docs) = send(&router, request(22, "tools/call", &retry)).await?;
    assert_eq!(error_code(docs.first().ok_or("no reply")?), Some(-32602));
    Ok(())
}

#[tokio::test]
async fn under_json_streaming_a_reply_is_one_document() -> TestResult {
    let (router, _) = router(Streaming::Json)?;
    let params = json!({ "_meta": meta(&json!({})) });
    let response = router
        .clone()
        .oneshot(request(30, "tools/list", &params))
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(
        content_type.starts_with("application/json"),
        "{content_type}"
    );
    let docs = documents(response).await?;
    let first = docs.first().ok_or("no reply")?;
    assert_eq!(
        first.pointer("/result/resultType"),
        Some(&json!("complete"))
    );
    assert_eq!(first.pointer("/result/cacheScope"), Some(&json!("private")));
    Ok(())
}
