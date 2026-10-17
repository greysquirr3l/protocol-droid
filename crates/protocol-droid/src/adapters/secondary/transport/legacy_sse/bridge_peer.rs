//! Carries envelopes over a `2024-11-05` stream.
//!
//! ## Why this is a wrapper and not an `impl` on [`LegacySseClient`]
//!
//! The [`HttpClient`](crate::adapters::secondary::transport::http::HttpClient)
//! peer is a plain `impl BridgePeer` on the client type, because an HTTP reply
//! arrives in the response to the POST that asked for it. There is nothing to
//! correlate and nowhere for an unsolicited frame to appear.
//!
//! This transport has both problems. `send` answers `202` with an empty body
//! and the real answer arrives later as a `message` event on the stream, so
//! `send` has to read until it finds the frame carrying *its own* id — and a
//! frame that belongs to no call in flight, such as a server-pushed
//! notification, arrives on the same stream and belongs to nobody. Neither
//! needs new state on the client to handle, so the wrapper owns it.
//!
//! ## The id is the caller's, not ours
//!
//! Same rule as the HTTP peer, and for the same reason: a proxy that renumbers
//! the id leaves the downstream client waiting for a reply carrying an id it
//! never sent.
//!
//! ## An unsolicited frame is buffered, never dropped
//!
//! The crate's whole promise is that an unmodelled directive reaches the other
//! side byte-for-byte. A notification the server pushed while the bridge waited
//! for a call's answer is real traffic, and discarding it to make the
//! correlation loop tidy would quietly violate that. It goes to
//! [`take_pending`](Self::take_pending) instead, and the caller decides where it
//! goes next.

use std::sync::Arc;

use serde_json::{Map, Value};

use crate::application::bridge::{BridgeError, BridgePeer, SendFuture, Unsolicited};

use super::{ClientFrame, LegacySseClient, LegacySseError};

/// A `2024-11-05` peer that holds the frames nothing was waiting for.
#[derive(Debug)]
pub struct LegacySsePeer {
    inner: Arc<LegacySseClient>,
    unsolicited: Unsolicited,
}

impl LegacySsePeer {
    /// Wraps a connected client.
    #[must_use]
    pub fn new(inner: Arc<LegacySseClient>) -> Self {
        Self {
            inner,
            unsolicited: Unsolicited::new(),
        }
    }

    /// The client underneath, for the parts of it a bridge does not use.
    #[must_use]
    pub const fn client(&self) -> &Arc<LegacySseClient> {
        &self.inner
    }

    /// Takes every frame that arrived while some other call was in flight.
    pub fn take_pending(&self) -> Vec<Map<String, Value>> {
        self.unsolicited.take()
    }
}

impl BridgePeer for LegacySsePeer {
    /// Delivers one envelope and yields the frame carrying its answer.
    ///
    /// # Errors
    ///
    /// [`BridgeError::Unreachable`] when the POST failed, when the stream
    /// closed before the answer arrived, or when the answer was not an object.
    /// A peer that answered with an `error` member is *not* an error here: it
    /// comes back through `Ok`, because a declined request is a successful
    /// exchange.
    fn send(
        &self,
        envelope: Map<String, Value>,
    ) -> SendFuture<'_, Result<Map<String, Value>, BridgeError>> {
        Box::pin(async move {
            // A notification has no id, so there is nothing to match the answer
            // against and a `send` that tried would read the stream until it
            // hung. `notify` is the method for that, and reaching here with no
            // id is the caller using the wrong one.
            let Some(id) = envelope.get("id").cloned() else {
                return Err(BridgeError::Unreachable {
                    reason: "a notification was sent through the request path".to_owned(),
                });
            };
            self.inner
                .send(&encode(&envelope)?)
                .await
                .map_err(|error| unreachable(&error))?;

            loop {
                match self
                    .inner
                    .next()
                    .await
                    .map_err(|error| unreachable(&error))?
                {
                    None => {
                        return Err(BridgeError::Unreachable {
                            reason: "the stream closed before the peer answered".to_owned(),
                        });
                    }
                    // The `endpoint` event is how the client learned where to
                    // post in the first place; a second one carries nothing
                    // this call is waiting for.
                    Some(ClientFrame::Endpoint(_)) => {}
                    Some(ClientFrame::Message(value)) => {
                        let Some(answer) = value.as_object().cloned() else {
                            return Err(BridgeError::Unreachable {
                                reason: "the peer answered with something that is not a JSON-RPC message"
                                    .to_owned(),
                            });
                        };
                        if answer.get("id") == Some(&id) {
                            return Ok(answer);
                        }
                        // Someone else's frame, or an unsolicited notification.
                        // Held rather than dropped — see the module docs.
                        self.unsolicited.hold(answer);
                    }
                }
            }
        })
    }

    /// Delivers one envelope that expects no answer.
    ///
    /// # Errors
    ///
    /// [`BridgeError::Unreachable`] when the POST failed.
    fn notify(&self, envelope: Map<String, Value>) -> SendFuture<'_, Result<(), BridgeError>> {
        Box::pin(async move {
            self.inner
                .send(&encode(&envelope)?)
                .await
                .map_err(|error| unreachable(&error))
        })
    }

    /// Ends the upstream session.
    ///
    /// # Errors
    ///
    /// Never. `2024-11-05` has no teardown verb — the stream closing *is* the
    /// teardown, and dropping the client drops it. Reporting a failure here
    /// would be reporting on something the transport does not have.
    fn close(&self) -> SendFuture<'_, Result<(), BridgeError>> {
        Box::pin(async { Ok(()) })
    }

    /// The frames the stream carried while another call was in flight.
    ///
    /// Overrides the trait's empty answer because `send` walks past every frame
    /// that is not its own answer and holds it here rather than discarding it —
    /// see [`send`](Self::send).
    fn take_pending(&self) -> Vec<Map<String, Value>> {
        Self::take_pending(self)
    }

    fn wait_pending(&self) -> SendFuture<'static, ()> {
        // The queue is where the frames are, so waiting on the queue is
        // waiting on the peer.
        Box::pin(self.unsolicited.wait())
    }
}

/// Serialises an envelope for the POST body.
///
/// Mapping by hand rather than with `?`: there is no `From<serde_json::Error>`
/// into [`BridgeError`], because a failure to *encode* our own outbound
/// envelope says nothing about the peer — it is a bridge-side fault, and the
/// nearest honest thing to say to the downstream client is that the exchange
/// could not be completed.
fn encode(envelope: &Map<String, Value>) -> Result<Vec<u8>, BridgeError> {
    serde_json::to_vec(&Value::Object(envelope.clone())).map_err(|error| BridgeError::Unreachable {
        reason: format!("the envelope could not be encoded for the wire: {error}"),
    })
}

/// Maps a transport failure onto the bridge's vocabulary.
fn unreachable(error: &LegacySseError) -> BridgeError {
    BridgeError::Unreachable {
        reason: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_json::{Map, Value, json};

    use crate::adapters::primary::transport::http::{Refusal, ServerConfig, SessionStore};
    use crate::adapters::primary::transport::legacy_sse::{
        DEFAULT_STREAM_PATH, LegacyDispatcher, LegacyRouter,
    };
    use crate::application::bridge::{BridgeError, BridgePeer};
    use crate::application::session::IdSource;
    use crate::domain::McpSessionId;
    use crate::domain::registry::AuthContext;
    use crate::domain::session::SessionId;

    use super::LegacySsePeer;
    use crate::adapters::secondary::transport::legacy_sse::{LegacySseClient, LegacySseConfig};

    type TestError = Box<dyn std::error::Error>;

    struct Counting(AtomicU64);

    impl IdSource for Counting {
        fn next_session_id(&self) -> SessionId {
            SessionId::new(self.0.fetch_add(1, Ordering::SeqCst))
        }
    }

    /// The real `2024-11-05` router, so these tests are a claim about the two
    /// halves of this crate agreeing rather than about a stub agreeing with a
    /// stub. A peer that pushes an extra frame needs a dispatcher that does
    /// that, and the real router has no reason to.
    struct Pushes {
        unsolicited: Option<Value>,
        decline: bool,
    }

    impl LegacyDispatcher for Pushes {
        fn dispatch(
            &self,
            _auth: &AuthContext,
            _session: &McpSessionId,
            frame: &Value,
        ) -> Result<Vec<Value>, Refusal> {
            let id = frame.get("id").cloned().unwrap_or(Value::Null);
            let answer = if self.decline {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "no"}})
            } else {
                json!({"jsonrpc": "2.0", "id": id, "result": {"ok": true}})
            };
            match self.unsolicited.clone() {
                Some(extra) => Ok(vec![extra, answer]),
                None => Ok(vec![answer]),
            }
        }
    }

    async fn peer(dispatcher: Pushes) -> Result<LegacySsePeer, TestError> {
        let router = LegacyRouter::new(
            ServerConfig::new()?,
            Arc::new(SessionStore::new(Arc::new(Counting(AtomicU64::new(0))))),
            dispatcher,
        )
        .into_router()?;

        // Port 0 so two tests can run at once; a fixed port fails the moment
        // anything else takes it, and looks like a transport bug.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });

        let config =
            LegacySseConfig::new(&format!("http://127.0.0.1:{port}{DEFAULT_STREAM_PATH}"))?;
        let client = LegacySseClient::connect(config).await?;
        Ok(LegacySsePeer::new(Arc::new(client)))
    }

    fn envelope(text: &str) -> Result<Map<String, Value>, TestError> {
        Ok(serde_json::from_str(text)?)
    }

    /// The point of the transport: the answer comes back on the stream, carrying
    /// the id the caller sent. Renumbering it would leave the downstream client
    /// waiting for a reply it was never going to receive.
    #[tokio::test]
    async fn an_answer_comes_back_on_the_stream_carrying_the_callers_id() -> Result<(), TestError> {
        let peer = peer(Pushes {
            unsolicited: None,
            decline: false,
        })
        .await?;

        let answer = peer
            .send(envelope(r#"{"jsonrpc":"2.0","id":41,"method":"ping"}"#)?)
            .await?;

        assert_eq!(answer.get("id"), Some(&json!(41)));
        assert_eq!(answer.get("result"), Some(&json!({"ok": true})));
        Ok(())
    }

    /// A frame that belongs to no call in flight is held, not dropped.
    ///
    /// The crate promises lossless forwarding, and a server-pushed
    /// notification is real traffic. Discarding it to keep the correlation loop
    /// tidy would violate that silently.
    #[tokio::test]
    async fn an_unsolicited_frame_is_held_rather_than_dropped() -> Result<(), TestError> {
        let pushed = json!({
            "jsonrpc": "2.0",
            "method": "notifications/tools/list_changed"
        });
        let peer = peer(Pushes {
            unsolicited: Some(pushed.clone()),
            decline: false,
        })
        .await?;

        let answer = peer
            .send(envelope(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#)?)
            .await?;

        assert_eq!(answer.get("id"), Some(&json!(1)));
        let held = peer.take_pending();
        assert_eq!(held.len(), 1, "the pushed frame was not held");
        assert_eq!(
            held.first().and_then(|frame| frame.get("method")),
            Some(&json!("notifications/tools/list_changed"))
        );

        // Draining, not cloning: the second poll must not hand it back again.
        assert!(peer.take_pending().is_empty());
        Ok(())
    }

    /// A declined request is a successful exchange, not a transport fault.
    ///
    /// Conflating the two tells the downstream client "network error" about a
    /// request the server processed and deliberately refused.
    #[tokio::test]
    async fn a_refusal_comes_back_through_ok_carrying_the_error_member() -> Result<(), TestError> {
        let peer = peer(Pushes {
            unsolicited: None,
            decline: true,
        })
        .await?;

        let answer = peer
            .send(envelope(
                r#"{"jsonrpc":"2.0","id":9,"method":"tools/call"}"#,
            )?)
            .await?;

        assert_eq!(
            answer
                .get("error")
                .and_then(|error| error.get("code"))
                .and_then(Value::as_i64),
            Some(-32601)
        );
        Ok(())
    }

    /// A notification sent down the request path is refused, not waited on.
    ///
    /// It has no id, so the correlation loop would match nothing and read the
    /// stream until it hung. `notify` is the method for it.
    #[tokio::test]
    async fn a_notification_on_the_request_path_is_refused() -> Result<(), TestError> {
        let peer = peer(Pushes {
            unsolicited: None,
            decline: false,
        })
        .await?;

        let outcome = peer
            .send(envelope(
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            )?)
            .await;

        assert!(
            matches!(outcome, Err(BridgeError::Unreachable { .. })),
            "a notification on the request path was not refused"
        );
        Ok(())
    }

    /// A notification really is deliverable: it posts and expects no answer.
    #[tokio::test]
    async fn a_notification_posts_without_waiting_for_a_reply() -> Result<(), TestError> {
        let peer = peer(Pushes {
            unsolicited: None,
            decline: false,
        })
        .await?;

        peer.notify(envelope(
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        )?)
        .await?;
        Ok(())
    }
}
