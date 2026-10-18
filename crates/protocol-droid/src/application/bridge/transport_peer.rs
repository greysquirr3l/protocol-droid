//! A bridge peer over any [`ClientTransport`].
//!
//! The HTTP and legacy-SSE peers carry the caller's envelope verbatim, because
//! those transports have a connection that already speaks JSON-RPC. A transport
//! that is built from `(method, params)` — one that writes its own envelope,
//! like the `2026-07-28` stateless client, which adds the revision and the
//! client's capabilities to every request's `_meta` — cannot take a finished
//! envelope. This peer takes the envelope apart, hands the pieces to the
//! transport, and puts the answer back together under **the caller's id**.
//!
//! The id is the caller's for the reason it is on every other peer: a proxy
//! that renumbers it leaves the downstream client waiting for a reply carrying
//! an id it never sent. The transport mints its own id for the upstream hop and
//! it never reaches the downstream side.
//!
//! A peer that declines is not a failure. [`TransportError::Refused`] comes
//! back as an envelope carrying an `error` member, exactly as the HTTP peer's
//! does, because the exchange succeeded and the answer was no.
//!
//! This transport has no standing stream, so there are no unsolicited frames
//! to hold: the trait's defaults are the true answer.

use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::application::bridge::{BridgeError, BridgePeer, SendFuture};
use crate::application::client::{ClientTransport, TransportError};
use crate::domain::ir::JsonObject;

/// Forwards envelopes through a [`ClientTransport`].
#[derive(Clone)]
pub struct TransportPeer {
    transport: Arc<dyn ClientTransport>,
}

impl core::fmt::Debug for TransportPeer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TransportPeer").finish_non_exhaustive()
    }
}

impl TransportPeer {
    /// A peer sending through `transport`.
    #[must_use]
    pub fn new(transport: Arc<dyn ClientTransport>) -> Self {
        Self { transport }
    }
}

/// An envelope's method and params, or why it has none.
fn parts(envelope: &Map<String, Value>) -> Result<(&str, JsonObject), BridgeError> {
    let method = envelope
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| BridgeError::Unreachable {
            reason: "the envelope has no method to forward".to_owned(),
        })?;
    let params = match envelope.get("params") {
        None | Some(Value::Null) => json!({}),
        Some(other) => other.clone(),
    };
    let params = JsonObject::from_value(params).map_err(|error| BridgeError::Unreachable {
        reason: format!("the params are not a JSON object: {error}"),
    })?;
    Ok((method, params))
}

fn unreachable(error: impl core::fmt::Display) -> BridgeError {
    BridgeError::Unreachable {
        reason: error.to_string(),
    }
}

impl BridgePeer for TransportPeer {
    /// Delivers one request and yields the answer, under the caller's id.
    ///
    /// # Errors
    ///
    /// [`BridgeError::Unreachable`] when the request could not be delivered or
    /// its answer could not be read. A refusal from the peer is an `Ok`
    /// envelope with an `error` member.
    fn send(
        &self,
        envelope: Map<String, Value>,
    ) -> SendFuture<'_, Result<Map<String, Value>, BridgeError>> {
        Box::pin(async move {
            let (method, params) = parts(&envelope)?;
            let id = envelope.get("id").cloned().unwrap_or(Value::Null);
            let mut reply = Map::new();
            reply.insert("jsonrpc".to_owned(), Value::from("2.0"));
            reply.insert("id".to_owned(), id);
            match self.transport.send(method, &params).await {
                Ok(result) => {
                    let result: Value =
                        serde_json::from_str(result.as_str()).map_err(unreachable)?;
                    reply.insert("result".to_owned(), result);
                }
                Err(TransportError::Refused {
                    code,
                    message,
                    data,
                }) => {
                    let mut error = Map::new();
                    error.insert("code".to_owned(), Value::from(code));
                    error.insert("message".to_owned(), Value::from(message));
                    if let Some(data) = data
                        && let Ok(data) = serde_json::from_str::<Value>(&data)
                    {
                        error.insert("data".to_owned(), data);
                    }
                    reply.insert("error".to_owned(), Value::Object(error));
                }
                Err(other) => return Err(unreachable(other)),
            }
            Ok(reply)
        })
    }

    /// Delivers a notification.
    ///
    /// # Errors
    ///
    /// [`BridgeError::Unreachable`] when it could not be delivered.
    fn notify(&self, envelope: Map<String, Value>) -> SendFuture<'_, Result<(), BridgeError>> {
        Box::pin(async move {
            let (method, params) = parts(&envelope)?;
            self.transport
                .notify(method, &params)
                .await
                .map_err(unreachable)
        })
    }

    /// There is no session to end.
    fn close(&self) -> SendFuture<'_, Result<(), BridgeError>> {
        Box::pin(async { Ok(()) })
    }
}

#[cfg(test)]
mod tests;
