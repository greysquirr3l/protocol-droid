//! Driving [`handshake`](super::super::handshake) over the port.
//!
//! These need a runtime to poll a future, so they are behind the `http`
//! feature — the one that brings a `tokio` with `rt`. They answer the
//! questions the pure tests cannot: which method actually went out, in what
//! order, and what happened when the transport refused.

use core::future::Future;
use core::pin::Pin;
use std::sync::Mutex;

use super::{DISCOVERY_ANSWER, MODERN_SERVER, TestError, at, client_at, json};
use crate::application::client::handshake::{
    DISCOVER, HandshakeError, INITIALIZE, INITIALIZED, handshake,
};
use crate::application::client::{ClientTransport, TransportError};
use crate::domain::ProtocolVersion;
use crate::domain::ir::JsonObject;

/// A transport that answers every request with the same canned result and
/// remembers every method it was given, in order.
struct Recording {
    answer: Option<JsonObject>,
    failure: Option<String>,
    methods: Mutex<Vec<String>>,
    params: Mutex<Vec<JsonObject>>,
}

impl Recording {
    fn answering(answer: &str) -> Result<Self, TestError> {
        Ok(Self {
            answer: Some(json(answer)?),
            failure: None,
            methods: Mutex::new(Vec::new()),
            params: Mutex::new(Vec::new()),
        })
    }

    fn failing(reason: &'static str) -> Self {
        Self {
            answer: None,
            failure: Some(reason.to_owned()),
            methods: Mutex::new(Vec::new()),
            params: Mutex::new(Vec::new()),
        }
    }

    /// Every method, in the order the port sent them.
    fn methods(&self) -> Vec<String> {
        self.methods
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The params of the call at `index`, parsed.
    fn params(&self, index: usize) -> Result<serde_json::Value, TestError> {
        let stored = self
            .params
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(index)
            .cloned()
            .ok_or(TestError::NoCallAt(index))?;
        Ok(serde_json::from_str(stored.as_str())?)
    }

    fn record(&self, method: &str, params: &JsonObject) {
        self.methods
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(method.to_owned());
        self.params
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(params.clone());
    }
}

impl ClientTransport for Recording {
    fn send(
        &self,
        method: &str,
        params: &JsonObject,
    ) -> Pin<Box<dyn Future<Output = Result<JsonObject, TransportError>> + Send + '_>> {
        self.record(method, params);
        let answer = self.answer.clone();
        let failure = self.failure.clone().unwrap_or_default();
        Box::pin(std::future::ready(answer.ok_or(TransportError::Unavailable(failure))))
    }

    fn notify(
        &self,
        method: &str,
        params: &JsonObject,
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
        self.record(method, params);
        Box::pin(std::future::ready(Ok(())))
    }
}

#[tokio::test]
async fn the_result_is_read_before_the_closing_notification() -> Result<(), TestError> {
    let transport = Recording::answering(MODERN_SERVER)?;
    let config = client_at(ProtocolVersion::V2025_06_18);

    let session = handshake(&transport, &config).await?;

    // In this order, not merely both present: a notification sent before the
    // result has been read is a client announcing readiness before it knows
    // what it agreed to.
    assert_eq!(transport.methods(), vec![INITIALIZE, INITIALIZED]);
    assert_eq!(session.version, ProtocolVersion::V2025_06_18);
    Ok(())
}

#[tokio::test]
async fn the_negotiated_revision_is_the_servers_not_the_clients() -> Result<(), TestError> {
    // A client that prefers 2025-11-25 but meets a 2025-06-18 server. The
    // session must land on what the server said, not on what the client hoped.
    let transport = Recording::answering(MODERN_SERVER)?;
    let config = crate::application::client::ClientConfig::speaking(
        [ProtocolVersion::V2025_11_25, ProtocolVersion::V2025_06_18],
        crate::domain::TransportKind::StreamableHttp,
    )?;

    let session = handshake(&transport, &config).await?;

    assert_eq!(session.version, ProtocolVersion::V2025_06_18);
    Ok(())
}

#[tokio::test]
async fn the_offer_that_goes_out_names_the_clients_preferred_revision() -> Result<(), TestError> {
    let transport = Recording::answering(MODERN_SERVER)?;
    let config = crate::application::client::ClientConfig::speaking(
        [ProtocolVersion::V2025_11_25, ProtocolVersion::V2025_06_18],
        crate::domain::TransportKind::StreamableHttp,
    )?;

    handshake(&transport, &config).await?;

    // The offer, not the outcome. Sending the *result* here would be a client
    // that already assumed the negotiation it is about to perform.
    let params = transport.params(0)?;
    assert_eq!(at(&params, "protocolVersion"), Some(&serde_json::json!("2025-11-25")));
    Ok(())
}

#[tokio::test]
async fn discovery_is_sent_where_there_is_no_initialize() -> Result<(), TestError> {
    let transport = Recording::answering(DISCOVERY_ANSWER)?;
    let config = client_at(ProtocolVersion::V2026_07_28);

    handshake(&transport, &config).await?;

    // SEP-2575 removed `initialize` and `InitializedNotification` from this
    // revision. Sending either is a request for a method that is not in the
    // peer's schema, and `initialize` is the one a client reaches for by habit.
    assert_eq!(transport.methods(), vec![DISCOVER]);
    Ok(())
}

#[tokio::test]
async fn a_negotiation_failure_sends_no_notification() -> Result<(), TestError> {
    let transport = Recording::answering(
        r#"{"protocolVersion":"2099-01-01","capabilities":{},
            "serverInfo":{"name":"n","version":"1"}}"#,
    )?;
    let config = client_at(ProtocolVersion::V2025_06_18);

    let error = expect_err(handshake(&transport, &config) .await)?;

    // There is no session to announce, and a server that was told "ready" by a
    // client that then refused to talk is a worse state than one that was told
    // nothing.
    assert_eq!(transport.methods(), vec![INITIALIZE]);
    assert!(matches!(error, HandshakeError::Negotiation(_)));
    Ok(())
}

#[tokio::test]
async fn a_transport_failure_surfaces_as_a_transport_error() -> Result<(), TestError> {
    let transport = Recording::failing("connection refused");
    let config = client_at(ProtocolVersion::V2025_06_18);

    let error = expect_err(handshake(&transport, &config) .await)?;

    let HandshakeError::Transport(TransportError::Unavailable(detail)) = &error else {
        return Err(TestError::WrongShape { got: error });
    };
    assert!(
        detail.contains("connection refused"),
        "the detail should say why, and said {detail:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_handshake_that_fails_sends_no_notification() -> Result<(), TestError> {
    // The transport failing on the *request* means the handshake never got far
    // enough to have a session, so the notification must not be attempted. This
    // is the same rule as the negotiation failure above, reached a different
    // way — which is why it is worth its own test: the two failure paths are
    // different code and either one could regress alone.
    let transport = Recording::failing("connection reset");
    let config = client_at(ProtocolVersion::V2025_06_18);

    let _error = expect_err(handshake(&transport, &config) .await)?;

    assert_eq!(transport.methods(), vec![INITIALIZE]);
    Ok(())
}

/// The error half of a result, for a test that expects the call to fail.
///
/// A function rather than a `.map` at each call site because the shape under
/// test is the *error*, and a `TestError` that wrapped one would have the test
/// matching on its own wrapper instead of on what the handshake produced.
fn expect_err<T>(result: Result<T, HandshakeError>) -> Result<HandshakeError, TestError> {
    match result {
        Ok(_) => Err(TestError::ExpectedAnError),
        Err(error) => Ok(error),
    }
}
