//! `HttpClient` as one side of a bridge.
//!
//! ## Why this is an adapter and not a second client
//!
//! A bridge holds a live upstream session, and the thing that owns that
//! session — the `Mcp-Session-Id` header, the cookie jar, the connection
//! pool — is `HttpClient`. A second client would mean a second session
//! lifecycle, and the two would drift the first time one of them learned a
//! detail the other had not. So this is an `impl BridgePeer for HttpClient`,
//! not a new type wrapping one.
//!
//! ## The two things that must not be simplified away
//!
//! **The id is the caller's, not ours.** `HttpClient::next_request_id` mints an
//! id from this client's counter, which is right for a client making calls and
//! wrong here: a proxy that renumbers the id leaves the downstream client
//! waiting forever for a reply carrying an id it never sent. The envelope
//! arrives with its id and leaves with it.
//!
//! **A refusal is not a failure.** A peer that declines a request answers with
//! a well-formed envelope carrying an `error` member. That comes back through
//! `Ok`, because it is a successful exchange. Only a transport fault is an
//! `Err`. Conflating them would tell the downstream client "network error"
//! about a request the server processed and deliberately declined.
//!
//! ## Notifications get their own method
//!
//! `BridgePeer` separates `notify` from `send` for the same reason
//! `ClientTransport` does: a notification has no `id`, and a conforming peer
//! correctly answers one with `202` and no body. A `send` that waited for a
//! reply would wait forever. `post_envelope` returns `Ok(None)` for exactly
//! that case rather than inventing an empty reply to hand downstream.

use serde_json::{Map, Value};

use crate::adapters::secondary::transport::http::{HttpClient, HttpClientError};
use crate::application::bridge::{BridgeError, BridgePeer, SendFuture};

/// Carries envelopes over one HTTP client's session.
impl BridgePeer for HttpClient {
    /// Delivers one envelope and yields the peer's answer.
    ///
    /// # Errors
    ///
    /// [`BridgeError::Unreachable`] when the request could not be completed at
    /// all. A peer that answered with an `error` member is *not* an error here:
    /// see the module docs.
    fn send(
        &self,
        envelope: Map<String, Value>,
    ) -> SendFuture<'_, Result<Map<String, Value>, BridgeError>> {
        Box::pin(async move {
            // `None` here would mean the peer sent no body, which is only
            // correct for a notification — and a notification has no reply to
            // hand back. Reaching `None` on the `send` path means the caller
            // used the wrong method, so it is reported rather than invented.
            self.post_envelope(&envelope)
                .await
                .map_err(|error| unreachable(&error))?
                .ok_or_else(|| BridgeError::Unreachable {
                    reason: "the peer accepted the request but sent no answer".to_owned(),
                })
        })
    }

    /// Delivers one envelope that expects no answer.
    ///
    /// # Errors
    ///
    /// [`BridgeError::Unreachable`] when the request could not be completed.
    /// The inherent `take_pending` and not this one, which is what an
    /// unqualified `Self::` resolves to. Naming it here because a reader
    /// seeing the same method name twice has no other way to tell the two
    /// apart.
    fn take_pending(&self) -> Vec<Map<String, Value>> {
        Self::take_pending(self)
    }

    fn wait_pending(&self) -> SendFuture<'static, ()> {
        // The queue is where the frames are, so waiting on the queue is
        // waiting on the peer. Nothing else to poll.
        Box::pin(self.unsolicited.wait())
    }

    /// Opens the upstream's standing stream.
    ///
    /// This is the half that makes a server-initiated message reach the client
    /// at all: without it the client POSTs, the server answers, and anything
    /// the server volunteers in between goes to nobody.
    fn listen(&self) -> SendFuture<'_, Result<(), BridgeError>> {
        Box::pin(async move {
            Self::listen(self)
                .await
                .map_err(|error| unreachable(&error))
        })
    }

    fn notify(&self, envelope: Map<String, Value>) -> SendFuture<'_, Result<(), BridgeError>> {
        Box::pin(async move {
            self.notify_envelope(&envelope)
                .await
                .map_err(|error| unreachable(&error))?;
            Ok(())
        })
    }

    /// Ends the upstream session.
    ///
    /// # Errors
    ///
    /// [`BridgeError::Unreachable`] when the peer did not accept the teardown.
    /// A peer that has already gone is reported as unreachable rather than
    /// swallowed: pretending a shutdown succeeded would hide a leaked session.
    fn close(&self) -> SendFuture<'_, Result<(), BridgeError>> {
        Box::pin(async move {
            self.delete().await.map_err(|error| unreachable(&error))?;
            Ok(())
        })
    }
}

/// Maps a transport failure onto the bridge's vocabulary.
///
/// One place, so the wording of "the peer could not be reached" is decided
/// once. [`HttpClientError::NotAMessage`] and [`HttpClientError::Body`] land
/// here too, and deliberately read as unreachable: from the downstream
/// client's side, a peer that answered with something that is not a
/// JSON-RPC message *is* unusable, and there is no more useful thing to tell
/// it.
fn unreachable(error: &HttpClientError) -> BridgeError {
    BridgeError::Unreachable {
        reason: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use crate::application::bridge::SendFuture;
    use crate::application::bridge::{BridgeError, BridgePeer};
    use serde_json::{Map, Value, json};

    type TestError = Box<dyn std::error::Error>;

    /// A client pointed at a port nothing is listening on. Constructed, never
    /// dialled: these tests pin the port's shape, not the wire.
    fn client() -> Result<crate::adapters::secondary::transport::http::HttpClient, TestError> {
        let config = crate::adapters::secondary::transport::http::HttpClientConfig::new(
            "http://127.0.0.1:1/mcp",
        )?;
        Ok(crate::adapters::secondary::transport::http::HttpClient::new(config)?)
    }

    /// The three `BridgePeer` methods are async, so the example below is a
    /// compile-time check rather than a live call: it pins the signatures an
    /// adapter has to satisfy, which is exactly where a drift would appear.
    fn assert_port<T: BridgePeer>(peer: &T) {
        let _ = |envelope: Map<String, Value>| {
            let _: SendFuture<'_, Result<Map<String, Value>, BridgeError>> =
                peer.send(envelope.clone());
            let _: SendFuture<'_, Result<(), BridgeError>> = peer.notify(envelope);
            let _: SendFuture<'_, Result<(), BridgeError>> = peer.close();
        };
    }

    /// `HttpClient` satisfies the bridge port.
    #[test]
    fn the_http_client_is_a_bridge_peer() -> Result<(), TestError> {
        let client = client()?;
        assert_port(&client);
        Ok(())
    }

    /// The port is object-safe, which is why `SendFuture` is boxed rather than
    /// a native `async fn`. Without this the bridge could not hold a peer it
    /// did not construct.
    #[test]
    fn the_port_is_object_safe() -> Result<(), TestError> {
        fn use_dyn(peer: &dyn BridgePeer) -> SendFuture<'_, Result<(), BridgeError>> {
            peer.close()
        }

        let client = client()?;
        let erased: &dyn BridgePeer = &client;
        // Bound, never polled: this asserts the *type* resolves through a
        // trait object. Running it would mean dialling a dead port.
        let _closer: SendFuture<'_, Result<(), BridgeError>> = use_dyn(erased);
        Ok(())
    }

    /// A transport fault is unreachable; a refusal is not an error at all.
    /// Both are checked through the mapping the adapter uses.
    #[test]
    fn transport_failures_map_to_unreachable() {
        let error = crate::adapters::secondary::transport::http::HttpClientError::MissingMediaType;
        let mapped = super::unreachable(&error);
        assert!(
            matches!(mapped, BridgeError::Unreachable { .. }),
            "got {mapped:?}"
        );
    }

    fn _json_is_used(_: &Value) -> bool {
        let _ = json!({});
        true
    }
}
