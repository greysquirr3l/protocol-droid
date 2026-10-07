//! Carries envelopes over a `stdio` child.
//!
//! ## Why this is a wrapper
//!
//! The obvious impl is on [`StdioClient`] directly, and it nearly is: the
//! client already exposes exactly the two halves a peer needs, `send_document`
//! and `next_frame`. What it does not offer is a way to say "this frame is not
//! the one I asked for". Reading until the id matches means walking past
//! anything the child pushed in between, and a `stdio` server is free to push
//! a `notifications/tools/list_changed` at any moment.
//!
//! So the wrapper owns that decision, and it owns it for the same reason the
//! `2024-11-05` peer does: the crate promises that an unmodelled directive
//! reaches the other side byte-for-byte, and a notification that landed mid-call
//! is real traffic. It goes to [`take_pending`](Self::take_pending).
//!
//! ## Why the client sits behind a lock
//!
//! A request is a write and a read, and the two have to belong to the *same*
//! call: two peers sharing one child's stdout, each reading frames it did not
//! ask for, is a bug the lock makes impossible rather than unlikely. The lock
//! is held across both halves, not just the write.

use serde_json::{Map, Value};
use tokio::sync::Mutex;

use crate::application::bridge::{BridgeError, BridgePeer, SendFuture, Unsolicited};
use crate::application::jsonrpc::SourceError;

use super::StdioClient;

/// A `stdio` peer over one spawned child.
#[derive(Debug)]
pub struct StdioPeer<DW> {
    client: Mutex<StdioClient<DW>>,
    unsolicited: Unsolicited,
}

impl<DW> StdioPeer<DW> {
    /// Wraps a spawned client.
    #[must_use]
    pub fn new(client: StdioClient<DW>) -> Self {
        Self {
            client: Mutex::new(client),
            unsolicited: Unsolicited::new(),
        }
    }

    /// Takes every frame that arrived while some other call was in flight.
    #[must_use]
    pub fn take_pending(&self) -> Vec<Map<String, Value>> {
        self.unsolicited.take()
    }
}

impl<DW> BridgePeer for StdioPeer<DW>
where
    DW: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    /// Delivers one envelope and yields the frame carrying its answer.
    ///
    /// # Errors
    ///
    /// [`BridgeError::Unreachable`] when the child's stdin refused the
    /// document, when its stdout closed before the answer arrived, or when the
    /// child closed rather than answering. A peer that answered with an `error`
    /// member is *not* an error here: it comes back through `Ok`, because a
    /// declined request is a successful exchange.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the guard spans the write AND the read on purpose: a request is                   two halves, and a peer that released the lock between them would                   let a second call read frames it never asked for"
    )]
    fn send(
        &self,
        envelope: Map<String, Value>,
    ) -> SendFuture<'_, Result<Map<String, Value>, BridgeError>> {
        Box::pin(async move {
            // A notification has no id, so nothing could ever match it and the
            // read below would walk to end-of-stream. `notify` is its method.
            let Some(id) = envelope.get("id").cloned() else {
                return Err(BridgeError::Unreachable {
                    reason: "a notification was sent through the request path".to_owned(),
                });
            };

            // One lock across both halves, deliberately: the read has to
            // belong to the write that preceded it.
            let client = self.client.lock().await;
            client
                .send_document(&Value::Object(envelope).to_string())
                .await
                .map_err(|error| unreachable(&error.to_string()))?;

            loop {
                let Some(frame) = client.next_frame().await.map_err(|error| read(&error))? else {
                    return Err(BridgeError::Unreachable {
                        reason: "the child closed before it answered".to_owned(),
                    });
                };
                let Some(answer) = answer_in(&frame)? else {
                    continue;
                };
                if answer.get("id") == Some(&id) {
                    return Ok(answer);
                }
                self.unsolicited.hold(answer);
            }
        })
    }

    /// Delivers one envelope that expects no answer.
    ///
    /// # Errors
    ///
    /// [`BridgeError::Unreachable`] when the child's stdin refused the document.
    fn notify(&self, envelope: Map<String, Value>) -> SendFuture<'_, Result<(), BridgeError>> {
        Box::pin(async move {
            let client = self.client.lock().await;
            client
                .send_document(&Value::Object(envelope).to_string())
                .await
                .map_err(|error| BridgeError::Unreachable {
                    reason: error.to_string(),
                })
        })
    }

    /// Ends the upstream session.
    ///
    /// # Errors
    ///
    /// [`BridgeError::Unreachable`] when the child could not be reaped. Calling
    /// this twice is not a second kill — the second call reports `Ok(None)`,
    /// because there is nothing left to wait for — and that is not an error
    /// either.
    fn close(&self) -> SendFuture<'_, Result<(), BridgeError>> {
        Box::pin(async move {
            let mut client = self.client.lock().await;
            let outcome = client
                .terminate()
                .await
                .map_err(|error| unreachable(&error.to_string()));
            // Nothing reads after the kill, so the guard has no reason to
            // outlive the call — unlike `send`, where it deliberately spans
            // both halves of the exchange.
            drop(client);
            outcome?;
            Ok(())
        })
    }
}

/// Reads one frame the child wrote, if it is a JSON-RPC message at all.
fn answer_in(frame: &str) -> Result<Option<Map<String, Value>>, BridgeError> {
    let value: Value = serde_json::from_str(frame).map_err(|error| BridgeError::Unreachable {
        reason: format!("the child answered with something that is not JSON: {error}"),
    })?;
    match value {
        Value::Object(object) => Ok(Some(object)),
        other => Err(BridgeError::Unreachable {
            reason: format!(
                "the child answered with a JSON {} rather than a JSON-RPC message",
                kind(&other)
            ),
        }),
    }
}

/// The JSON type name, so the refusal says what arrived.
const fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Maps a transport failure onto the bridge's vocabulary.
fn read(error: &SourceError) -> BridgeError {
    unreachable(&error.to_string())
}

fn unreachable(reason: &str) -> BridgeError {
    BridgeError::Unreachable {
        reason: reason.to_owned(),
    }
}
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{StdioPeer, answer_in};
    use crate::adapters::primary::stdio::Diagnostics;
    use crate::adapters::primary::stdio::StdioConfig;
    use crate::adapters::secondary::stdio::StdioClient;
    use crate::adapters::secondary::stdio::client::ClientConfig;
    use crate::adapters::secondary::stdio::testing::{Capture, Role, SpawnFailure, child_spec};

    /// Every failure a test can hit, in one type, so `?` works throughout.
    #[derive(Debug, thiserror::Error)]
    enum Failure {
        /// A test child could not be described. Carried rather than converted,
        /// because `SpawnFailure` is a plain enum with no `Display` of its own
        /// and adding one to reach a `?` would be noise in a test helper.
        #[error("a test child could not be described: {0:?}")]
        Spawn(SpawnFailure),
        /// The child could not be started.
        #[error(transparent)]
        Client(#[from] crate::adapters::secondary::stdio::child::SpawnError),
        /// The spec could not be built.
        #[error(transparent)]
        Spec(#[from] crate::adapters::secondary::stdio::spec::ChildSpecError),
        /// Anything boxed, for the config.
        #[error(transparent)]
        Boxed(#[from] Box<dyn std::error::Error>),
    }

    /// The peer is constructible from a real child and starts with nothing
    /// held.
    ///
    /// Construction is all this claims. Driving `send` against this harness
    /// would test the harness: libtest writes `running 1 test` to the child's
    /// stdout, which *is* the protocol stream, so frame zero is harness output
    /// rather than a message. `answer_in` rightly refuses that, and a test that
    /// taught it to skip would be teaching the adapter that garbage on a
    /// `stdio` stream is fine.
    #[tokio::test]
    async fn a_fresh_peer_holds_nothing() -> Result<(), Failure> {
        let spec = child_spec(Role::Echo).map_err(Failure::Spawn)?;
        let client = StdioClient::spawn(
            &spec,
            ClientConfig::new(StdioConfig::default()),
            Diagnostics::new(Capture::shared()),
        )
        .await?;

        assert!(StdioPeer::new(client).take_pending().is_empty());
        Ok(())
    }

    /// A well-formed message is the answer.
    #[test]
    fn a_message_is_read_as_an_answer() {
        let read = answer_in(r#"{"jsonrpc":"2.0","id":7,"result":{"ok":true}}"#);
        assert!(read.is_ok(), "a JSON-RPC object is an answer");
        if let Ok(Some(frame)) = read {
            assert_eq!(frame.get("id"), Some(&json!(7)));
        }
    }

    /// Anything that will not parse is refused rather than skipped.
    ///
    /// A `stdio` peer that walked past unparseable frames would silently drop
    /// whatever a misbehaving child wrote instead of saying so.
    #[test]
    fn a_frame_that_is_not_json_is_refused() {
        assert!(answer_in("running 1 test").is_err());
    }

    /// The refusal says what arrived, so the caller can act on it.
    #[test]
    fn a_json_scalar_is_refused_naming_its_type() {
        let read = answer_in("[1,2,3]");
        assert!(read.is_err(), "an array is not a JSON-RPC message");
        let Err(reason) = read else {
            return;
        };
        let text = reason.to_string();
        assert!(text.contains("array"), "{text}");
    }
}
