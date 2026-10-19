use core::future::Future;
use core::pin::Pin;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};

use super::TransportPeer;
use crate::application::bridge::{BridgeError, BridgePeer};
use crate::application::client::{ClientTransport, TransportError};
use crate::domain::ir::JsonObject;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Answers from a script and records what it was asked.
struct Scripted {
    answer: Mutex<Option<Result<JsonObject, TransportError>>>,
    seen: Mutex<Vec<(String, String)>>,
}

impl Scripted {
    fn new(answer: Result<JsonObject, TransportError>) -> Arc<Self> {
        Arc::new(Self {
            answer: Mutex::new(Some(answer)),
            seen: Mutex::new(Vec::new()),
        })
    }
}

impl ClientTransport for Scripted {
    fn send(
        &self,
        method: &str,
        params: &JsonObject,
    ) -> Pin<Box<dyn Future<Output = Result<JsonObject, TransportError>> + Send + '_>> {
        if let Ok(mut seen) = self.seen.lock() {
            seen.push((method.to_owned(), params.as_str().to_owned()));
        }
        let answer = self
            .answer
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
            .unwrap_or_else(|| Err(TransportError::Unavailable("no script".to_owned())));
        Box::pin(async move { answer })
    }

    fn notify(
        &self,
        method: &str,
        params: &JsonObject,
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
        if let Ok(mut seen) = self.seen.lock() {
            seen.push((method.to_owned(), params.as_str().to_owned()));
        }
        Box::pin(async { Ok(()) })
    }
}

fn request(id: &Value) -> Map<String, Value> {
    match json!({"jsonrpc": "2.0", "id": id, "method": "tools/list", "params": {"cursor": "c"}}) {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

#[tokio::test]
async fn the_answer_carries_the_callers_id_not_the_transports() -> TestResult {
    let transport = Scripted::new(Ok(JsonObject::new(r#"{"tools":[]}"#)?));
    let peer = TransportPeer::new(Arc::clone(&transport) as Arc<dyn ClientTransport>);
    let reply = peer.send(request(&json!("client-7"))).await?;
    assert_eq!(reply.get("id"), Some(&json!("client-7")));
    assert_eq!(reply.get("result"), Some(&json!({"tools": []})));
    let seen = transport.seen.lock().map_err(|_| "poisoned")?.clone();
    assert_eq!(
        seen,
        [("tools/list".to_owned(), r#"{"cursor":"c"}"#.to_owned())]
    );
    Ok(())
}

#[tokio::test]
async fn a_refusal_is_an_error_envelope_not_a_failure() -> TestResult {
    let transport = Scripted::new(Err(TransportError::Refused {
        code: -32022,
        message: "unsupported".to_owned(),
        data: Some(r#"{"supported":["2026-07-28"]}"#.to_owned()),
    }));
    let peer = TransportPeer::new(transport);
    let reply = peer.send(request(&json!(3))).await?;
    assert_eq!(reply.get("id"), Some(&json!(3)));
    let reply = Value::Object(reply);
    assert_eq!(reply.pointer("/error/code"), Some(&json!(-32022)));
    assert_eq!(
        reply.pointer("/error/data/supported/0"),
        Some(&json!("2026-07-28"))
    );
    assert!(reply.get("result").is_none());
    Ok(())
}

#[tokio::test]
async fn an_unreachable_peer_is_a_failure() -> TestResult {
    let transport = Scripted::new(Err(TransportError::Unavailable("down".to_owned())));
    let peer = TransportPeer::new(transport);
    let outcome = peer.send(request(&json!(1))).await;
    assert!(
        matches!(outcome, Err(BridgeError::Unreachable { .. })),
        "{outcome:?}"
    );
    Ok(())
}

#[tokio::test]
async fn an_envelope_with_no_method_is_not_forwarded() -> TestResult {
    let transport = Scripted::new(Ok(JsonObject::new("{}")?));
    let peer = TransportPeer::new(Arc::clone(&transport) as Arc<dyn ClientTransport>);
    let outcome = peer.send(Map::new()).await;
    assert!(matches!(outcome, Err(BridgeError::Unreachable { .. })));
    assert!(transport.seen.lock().map_err(|_| "poisoned")?.is_empty());
    Ok(())
}

#[tokio::test]
async fn a_notification_is_delivered_without_an_answer() -> TestResult {
    let transport = Scripted::new(Ok(JsonObject::new("{}")?));
    let peer = TransportPeer::new(Arc::clone(&transport) as Arc<dyn ClientTransport>);
    let mut note = Map::new();
    note.insert("jsonrpc".to_owned(), json!("2.0"));
    note.insert("method".to_owned(), json!("notifications/cancelled"));
    note.insert("params".to_owned(), json!({"requestId": 4}));
    peer.notify(note).await?;
    assert_eq!(transport.seen.lock().map_err(|_| "poisoned")?.len(), 1);
    Ok(())
}
