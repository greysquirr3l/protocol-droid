//! A bridge whose upstream answers with an event stream.
//!
//! The specification lets a server answer a `POST` with either a JSON document
//! or `text/event-stream`, and prefers the stream. A client that only knows
//! how to read the document therefore works against half the servers that
//! exist — and the half it fails on is the one the specification prefers.
//!
//! The server below answers every `POST` with a stream carrying a notification
//! *and* the reply, in that order, so it fails a client that reads the first
//! frame and stops, and one that reads to the end and loses the reply among
//! the traffic.

#![cfg(feature = "http")]

use axum::Router;
use std::fmt::Write as _;

use axum::body::Body;
use axum::extract::State;
use axum::response::Response;
use axum::routing::post;
use serde_json::{Map, Value, json};

use protocol_droid::adapters::secondary::transport::http::{HttpClient, HttpClientConfig};
use protocol_droid::application::bridge::BridgePeer;

/// Whether the server volunteers a frame the request did not ask for.
///
/// The notification goes out *before* the reply on purpose. A client that
/// returns the first frame it reads hands the bridge a notification dressed up
/// as a reply, and the downstream client waits forever on an id that never
/// arrives.
#[derive(Clone, Copy)]
struct Script {
    volunteer: bool,
}

async fn serve(State(script): State<Script>, body: axum::body::Bytes) -> Response {
    let Ok(Value::Object(request)) = serde_json::from_slice::<Value>(&body) else {
        return Response::builder()
            .status(axum::http::StatusCode::BAD_REQUEST)
            .body(Body::empty())
            .unwrap_or_default();
    };
    let id = request.get("id").cloned().unwrap_or(Value::Null);

    let mut frames = String::new();
    if script.volunteer {
        let notice = json!({
            "jsonrpc": "2.0",
            "method": "notifications/message",
            "params": {"level": "info", "data": "volunteered"}
        });
        let _ = write!(frames, "event: message\ndata: {notice}\n\n");
    }
    let reply = json!({"jsonrpc": "2.0", "id": id, "result": {"protocolVersion": "2025-11-25"}});
    let _ = write!(frames, "event: message\ndata: {reply}\n\n");

    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from(frames))
        .unwrap_or_default()
}

/// A client bound to a loopback server that answers in streams.
async fn connected(script: Script) -> Result<HttpClient, Box<dyn std::error::Error>> {
    let app = Router::new().route("/mcp", post(serve).with_state(script));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let endpoint = format!("http://{addr}/mcp");
    let config = HttpClientConfig::new(&endpoint)?;
    Ok(HttpClient::new(config)?)
}

/// An `initialize` the way a bridge forwards one, with the id the client chose.
fn initialize(id: i64) -> Map<String, Value> {
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
    .as_object()
    .cloned()
    .unwrap_or_default()
}

#[tokio::test]
async fn a_streamed_answer_comes_back_and_the_reply_keeps_its_id()
-> Result<(), Box<dyn std::error::Error>> {
    let client = connected(Script { volunteer: false }).await?;

    let reply = client.send(initialize(7)).await?;

    // The id is the whole point of the envelope-level API: renumber it and the
    // downstream client is left waiting on an id it never sent.
    if reply.get("id") != Some(&json!(7)) {
        return Err(format!("the reply carried {:?}, not the id 7", reply.get("id")).into());
    }
    Ok(())
}

#[tokio::test]
async fn a_frame_the_server_volunteered_is_kept_rather_than_returned_as_the_reply()
-> Result<(), Box<dyn std::error::Error>> {
    let client = connected(Script { volunteer: true }).await?;

    let reply = client.send(initialize(9)).await?;
    if reply.get("id") != Some(&json!(9)) {
        return Err(format!("the reply carried {:?}, not the id 9", reply.get("id")).into());
    }

    // The notification arrived first on the wire. The only correct outcome is
    // that it was neither mistaken for the reply nor thrown away.
    let held = client.take_pending();
    let methods: Vec<&str> = held
        .iter()
        .filter_map(|frame| frame.get("method").and_then(Value::as_str))
        .collect();
    if methods != ["notifications/message"] {
        return Err(format!("the volunteered frames were {methods:?}").into());
    }
    Ok(())
}

#[tokio::test]
async fn the_buffer_is_emptied_by_taking_it() -> Result<(), Box<dyn std::error::Error>> {
    let client = connected(Script { volunteer: true }).await?;
    let _ = client.send(initialize(1)).await?;

    let first = client.take_pending().len();
    let second = client.take_pending().len();
    if (first, second) != (1, 0) {
        return Err(format!("taking twice gave {first} then {second}").into());
    }
    Ok(())
}

#[tokio::test]
async fn the_handshake_is_forwarded_with_no_session_present()
-> Result<(), Box<dyn std::error::Error>> {
    // The first message a bridge forwards is the one that has no session yet,
    // so a client that demanded one would refuse the handshake the bridge is
    // required to pass through.
    let client = connected(Script { volunteer: false }).await?;
    if client.session_id().is_some() {
        return Err("the client claimed a session before one was minted".into());
    }
    let reply = client.send(initialize(1)).await?;
    if reply.get("id").is_none() {
        return Err("the handshake came back without an id".into());
    }
    Ok(())
}
