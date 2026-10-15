//! The `AcpPeerError` — every error the ACP stdio peer
//! can surface to the application layer.
//!
//! # Why `thiserror`
//!
//! The peer is a library component; the consumer
//! matches on the variants. A free-form `String`
//! error would be a silent log line, not a typed
//! signal. `thiserror`'s derive keeps the variants
//! typed and the `Display` impl consistent across
//! them.

use std::time::Duration;

use thiserror::Error;

/// Every error the ACP stdio peer can produce.
///
/// The variants are mapped from the underlying
/// `tokio::io::Error`, `serde_json::Error`, and the
/// peer's own protocol errors. A consumer that
/// matches on the variants knows what category of
/// failure happened, not just that "something
/// failed".
#[derive(Debug, Error)]
pub enum AcpPeerError {
    /// The peer could not be spawned, or the
    /// stdio pipes could not be wired up.
    #[error("spawning the ACP peer failed: {0}")]
    Spawn(#[source] std::io::Error),

    /// Writing an envelope to the child's stdin
    /// failed. The peer is unrecoverable: the
    /// child's write side is broken and the
    /// reader task is about to see EOF.
    #[error("writing the envelope to the peer failed: {0}")]
    Write(#[source] std::io::Error),

    /// The reply from the child was not a
    /// well-formed JSON-RPC envelope. The peer
    /// is recoverable: future requests can still
    /// succeed if the agent's framing is
    /// consistent and this one frame was a
    /// one-off.
    #[error("the peer returned a malformed reply: {0}")]
    Decode(#[source] serde_json::Error),

    /// The reply was well-formed JSON but did
    /// not match the JSON-RPC 2.0 shape the
    /// peer expects (`id` must be present on a
    /// reply, `result`/`error` exactly one).
    #[error("the peer returned an ill-shaped reply: {0}")]
    IllShapedReply(String),

    /// The reply was not received within the
    /// configured timeout. The default is 30s;
    /// a consumer that knows the agent is slow
    /// can override.
    #[error("the peer did not reply within {0:?}")]
    Timeout(Duration),

    /// The reply was a JSON-RPC error. The error
    /// code and message are part of the error;
    /// the consumer is the one that decides
    /// whether to retry, surface, or fail.
    #[error("the peer returned a JSON-RPC error: code={code}, message={message}")]
    JsonRpc {
        /// The JSON-RPC error code. `0` is
        /// reserved (the wire does not use it),
        /// so the integer is always the agent's
        /// own code or one of the standard
        /// pre-defined codes.
        code: i64,
        /// The human-readable message the
        /// agent attached. The peer does not
        /// log it; the consumer decides what
        /// to do.
        message: String,
    },
}
