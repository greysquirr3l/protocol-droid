//! This crate's stateless client against this crate's server, over a socket.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::net::TcpListener;

use crate::adapters::primary::transport::http::{HttpBuilder, ServerConfig};
use crate::application::client::Client;
use crate::application::client::handler::ConnectOptions;
use crate::application::server::Server;
use crate::domain::ir::JsonObject;
use crate::domain::registry::Tool;
use crate::domain::{ProtocolVersion, TransportKind, TransportSpec};

type TestResult = Result<(), Box<dyn std::error::Error>>;

async fn serve(server: Server) -> Result<String, Box<dyn std::error::Error>> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let builder = HttpBuilder::new(server).config(ServerConfig::new()?);
    tokio::spawn(async move {
        let _ = builder.serve_on(listener).await;
    });
    Ok(format!("http://{addr}/mcp"))
}

fn region_tool() -> Result<Tool, crate::domain::ir::JsonObjectError> {
    Tool::new(
        "regional",
        r#"{"type":"object","properties":{"region":{"type":"string","x-mcp-header":"Region"}},"required":["region"]}"#,
    )
}

#[tokio::test]
async fn a_client_negotiates_statelessly_and_mirrors_param_headers() -> TestResult {
    let server = Server::builder("stateless-peer")
        .tool(
            region_tool()?,
            Arc::new(|_ctx, args| {
                Box::pin(async move {
                    JsonObject::from_value(json!({
                        "content": [{ "type": "text", "text": args.as_str() }],
                    }))
                    .unwrap_or_default()
                })
            }),
        )
        .build();
    let url = serve(server).await?;
    let client =
        Client::connect_to(&TransportSpec::new(TransportKind::StatelessHttp, url)?).await?;
    assert_eq!(client.negotiated(), ProtocolVersion::V2026_07_28);

    let listed: Value = serde_json::from_str(client.list_tools().await?.as_str())?;
    assert_eq!(listed.pointer("/tools/0/name"), Some(&json!("regional")));

    // The server refuses a `tools/call` whose `Mcp-Param-Region` is missing or
    // disagrees with the body, so a result here proves the client sent it —
    // and sent it Base64-encoded, because the value is not plain ASCII.
    let called = client
        .call_tool("regional", &JsonObject::new(r#"{"region":"Zürich"}"#)?)
        .await?;
    assert!(called.as_str().contains('Z'), "{}", called.as_str());
    Ok(())
}

#[tokio::test]
async fn a_tool_with_an_invalid_annotation_is_dropped_from_the_listing() -> TestResult {
    let invalid = Tool::new(
        "bad",
        r#"{"type":"object","properties":{"ratio":{"type":"number","x-mcp-header":"Ratio"}}}"#,
    )?;
    let server = Server::builder("stateless-peer")
        .tool(
            region_tool()?,
            Arc::new(|_ctx, _args| Box::pin(async { JsonObject::default() })),
        )
        .tool(
            invalid,
            Arc::new(|_ctx, _args| Box::pin(async { JsonObject::default() })),
        )
        .build();
    let url = serve(server).await?;
    let client =
        Client::connect_to(&TransportSpec::new(TransportKind::StatelessHttp, url)?).await?;
    let listed: Value = serde_json::from_str(client.list_tools().await?.as_str())?;
    let names: Vec<&str> = listed
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect();
    assert_eq!(names, ["regional"]);
    Ok(())
}

fn asking_server() -> Result<Server, Box<dyn std::error::Error>> {
    use crate::application::mcp::input_required::{InputRequired, StateKey};
    Ok(Server::builder("asks")
        .request_state_key(StateKey::from_bytes([3; 32]))
        .tool(
            Tool::new("ask", r#"{"type":"object"}"#)?,
            Arc::new(|ctx, _args| {
                Box::pin(async move {
                    if let Some(answer) = ctx.input_response("name") {
                        let state = ctx.request_state().unwrap_or("none").to_owned();
                        return JsonObject::from_value(json!({
                            "content": [{ "type": "text", "text": format!("{} / {state}", answer.as_str()) }],
                        }))
                        .unwrap_or_default();
                    }
                    let ask = JsonObject::new(
                        r#"{"message":"Name?","requestedSchema":{"type":"object"}}"#,
                    )
                    .unwrap_or_default();
                    InputRequired::new()
                        .elicit("name", &ask)
                        .with_state(ctx.seal_state("round-1").unwrap_or_default())
                        .into_result()
                })
            }),
        )
        .build())
}

fn spec(url: String) -> Result<TransportSpec, Box<dyn std::error::Error>> {
    Ok(TransportSpec::new(TransportKind::StatelessHttp, url)?)
}

fn everything() -> Result<crate::domain::SupportedVersions, crate::domain::SupportedVersionsError> {
    crate::domain::SupportedVersions::all()
}

#[tokio::test]
async fn a_call_is_resolved_across_rounds_with_state_echoed() -> TestResult {
    use crate::domain::{Capability, CapabilitySet};
    let url = serve(asking_server()?).await?;
    let client = Client::connect_with(
        &spec(url)?,
        &everything()?,
        ConnectOptions::new().declaring(CapabilitySet::empty().with(Capability::Elicitation)),
    )
    .await?;
    let answered = client
        .call_tool_resolving(
            "ask",
            &JsonObject::default(),
            &|key: String, method: String, _params: JsonObject| async move {
                assert_eq!(
                    (key.as_str(), method.as_str()),
                    ("name", "elicitation/create")
                );
                JsonObject::new(r#"{"action":"accept","content":{"name":"Ada"}}"#).ok()
            },
        )
        .await?;
    assert!(answered.as_str().contains("Ada"), "{}", answered.as_str());
    assert!(
        answered.as_str().contains("round-1"),
        "the state was echoed: {}",
        answered.as_str()
    );
    Ok(())
}

#[tokio::test]
async fn an_unanswerable_request_fails_naming_the_callback() -> TestResult {
    use crate::application::client::ClientError;
    use crate::domain::{Capability, CapabilitySet};
    let url = serve(asking_server()?).await?;
    let client = Client::connect_with(
        &spec(url)?,
        &everything()?,
        ConnectOptions::new().declaring(CapabilitySet::empty().with(Capability::Elicitation)),
    )
    .await?;
    let refused = client
        .call_tool_resolving(
            "ask",
            &JsonObject::default(),
            &|_: String, _: String, _: JsonObject| async { None },
        )
        .await;
    match refused {
        Err(ClientError::InputUnresolved { method }) => assert_eq!(method, "elicitation/create"),
        other => return Err(format!("expected InputUnresolved, got {other:?}").into()),
    }
    Ok(())
}

/// A server that speaks only an older stateless revision answers the newest
/// with `-32022`; the client retries with one it shares instead of giving up.
#[tokio::test]
async fn an_unsupported_revision_is_renegotiated() -> TestResult {
    use axum::Router;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::post;

    async fn refuse(body: String) -> impl IntoResponse {
        let request: Value = serde_json::from_str(&body).unwrap_or_default();
        let asked = request
            .pointer("/params/_meta/io.modelcontextprotocol~1protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        if asked == "2026-07-28" {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": { "code": -32022, "message": "unsupported",
                        "data": { "requested": asked, "supported": ["2026-01-01"] } },
                })),
            );
        }
        (
            StatusCode::OK,
            axum::Json(json!({ "jsonrpc": "2.0", "id": id, "result": {} })),
        )
    }
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, Router::new().route("/mcp", post(refuse))).await;
    });
    let outcome = Client::connect_to(&spec(format!("http://{addr}/mcp"))?).await;
    // The only revision both sides could share is not one this build speaks, so
    // the connection is refused — but by exhausting the list, not by the first
    // `-32022`.
    assert!(outcome.is_err());
    let message = outcome.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        message.contains("shares no stateless revision"),
        "{message}"
    );
    Ok(())
}

#[tokio::test]
async fn a_bridge_peer_carries_an_envelope_to_a_stateless_server_under_the_callers_id() -> TestResult
{
    use crate::application::bridge::{BridgePeer, TransportPeer};

    let server = Server::builder("stateless-peer")
        .tool(
            region_tool()?,
            Arc::new(|_ctx, args| {
                Box::pin(async move {
                    JsonObject::from_value(json!({
                        "content": [{ "type": "text", "text": args.as_str() }],
                    }))
                    .unwrap_or_default()
                })
            }),
        )
        .build();
    let url = serve(server).await?;
    let (_client, transport) = crate::application::client::open(
        &TransportSpec::new(TransportKind::StatelessHttp, url)?,
        &everything()?,
    )
    .await?;
    let peer = TransportPeer::new(transport);

    let Value::Object(listing) =
        json!({"jsonrpc": "2.0", "id": "downstream-9", "method": "tools/list"})
    else {
        return Err("not an object".into());
    };
    let reply = Value::Object(peer.send(listing).await?);
    assert_eq!(reply.get("id"), Some(&json!("downstream-9")));
    assert_eq!(
        reply.pointer("/result/tools/0/name"),
        Some(&json!("regional"))
    );

    // The bridge does not read arguments, so the `x-mcp-header` mirror has to
    // come from the stateless client underneath, using the listing it relayed.
    let Value::Object(call) = json!({
        "jsonrpc": "2.0", "id": 4, "method": "tools/call",
        "params": {"name": "regional", "arguments": {"region": "Zürich"}},
    }) else {
        return Err("not an object".into());
    };
    let reply = Value::Object(peer.send(call).await?);
    assert_eq!(reply.get("id"), Some(&json!(4)));
    assert!(reply.get("error").is_none(), "{reply}");
    Ok(())
}
