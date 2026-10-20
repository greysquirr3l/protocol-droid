//! Why the `2024-11-05` client could not do what it was asked to.

use thiserror::Error;

use crate::domain::MessageEndpointError;

/// The largest a rejected payload is echoed back in an error message.
///
/// An error carries what the peer said, because "the server told the client to
/// post somewhere unusable" is the diagnosis and a bare "bad endpoint" is not.
/// It is capped because the value is peer-supplied and an error that can hold a
/// gigabyte is a way to exhaust memory rather than to report a problem.
pub(super) const PREVIEW: usize = 120;

/// Why a `2024-11-05` client could not connect, send, or read.
///
/// Not a wrapper around the Streamable HTTP client's error, despite sharing most
/// of its cases. The two clients fail in the same ways and mean different things
/// by them: a `GET` refused on this transport is a server that does not speak
/// `2024-11-05` at all, where on Streamable HTTP it is a session that ended.
/// Folding them together would make every caller translate the error back
/// before it could read it, and the translation is the part that matters.
#[derive(Debug, Error)]
pub enum LegacySseError {
    /// The configured URL is not a URL a request can be built against.
    #[error("the url {got:?} is not usable: {reason}")]
    Url {
        /// The URL as given.
        got: String,
        /// The URL parser's account, rendered.
        reason: String,
    },

    /// The request never reached the peer, or the response never came back.
    ///
    /// On this transport a `POST` that may have been received and acted on
    /// before the socket failed is the case the idempotency key exists for, so
    /// this is not a blind-retry error the way it is for a `GET`.
    #[error("the {method} request to the peer could not be completed: {error}")]
    Io {
        /// The HTTP method that was in flight, so a log line names what was lost.
        method: &'static str,
        /// The transport's own account of the failure.
        error: reqwest::Error,
    },

    /// The peer answered with a status outside 2xx.
    #[error("the peer refused the {method} with {status}: {preview}")]
    Refused {
        /// The HTTP method that was refused.
        method: &'static str,
        /// The status the peer answered with.
        status: reqwest::StatusCode,
        /// The `Content-Type`, absent when the peer sent none.
        content_type: Option<String>,
        /// The first 120 characters of the body, decoded lossily.
        preview: String,
    },

    /// The `GET` did not open an event stream.
    ///
    /// The specification's detection step says the client issues a `GET`
    /// "expecting that this will open an SSE stream". A `200` that is not one
    /// is a server that took the URL and speaks neither transport, and it earns
    /// its own error: the alternative is treating the body as frames and
    /// reporting a parse failure several steps later, which points at the codec
    /// rather than at the negotiation that actually failed.
    #[error("the peer answered the stream request with {got:?}, which is not an event stream")]
    NotAStream {
        /// The `Content-Type` the peer answered with, or the empty string when
        /// it sent none.
        got: String,
    },

    /// The first event on a fresh stream was not an `endpoint` event.
    ///
    /// Distinct from a malformed one because the two mean opposite things about
    /// the peer. A `message` first means the server skipped the handshake and is
    /// sending frames into a void; a malformed `endpoint` means the handshake is
    /// there and unreadable. Both stop the client, and only one of them is a
    /// version problem.
    #[error("the first event on a fresh stream was {got:?}, which is not an `endpoint` event")]
    FirstEventNotEndpoint {
        /// The name that arrived, or the empty string for an unnamed event.
        got: String,
    },

    /// The `endpoint` event's payload is not a usable target.
    ///
    /// The only variant here that is a **security** outcome, and the reason
    /// [`MessageEndpoint`](crate::domain::MessageEndpoint) is a domain type rather than a `String`. The server
    /// chooses where the client posts, so a value of
    /// `https://elsewhere.example/steal` is a server directing every
    /// subsequent request — including whatever the client authenticates with —
    /// at a host the caller never named. Refusing at parse time means the
    /// redirect is stopped before a single byte leaves.
    #[error("the peer named a message endpoint this client will not use: {reason}")]
    Endpoint {
        /// Why the value was refused. Its own message, because
        /// [`MessageEndpointError`] is written to be read by whoever has to act
        /// on it.
        reason: MessageEndpointError,
    },

    /// The stream carried bytes this client cannot read.
    ///
    /// One variant for both ways that happens — a terminated SSE block the
    /// grammar rejects, and a frame that parsed but whose data is not JSON —
    /// because a caller cannot act on the difference and splitting them would
    /// mean two arms that both say the same thing.
    #[error("the stream carried {got:?}, which this client cannot read: {reason}")]
    Unreadable {
        /// What the client saw, for a log line, bounded so a peer cannot make an
        /// error expensive.
        got: String,
        /// The reader's own account of the failure.
        reason: String,
    },
}

impl LegacySseError {
    /// Builds a [`LegacySseError::Endpoint`] from a parser's own refusal.
    pub(crate) const fn endpoint(reason: MessageEndpointError) -> Self {
        Self::Endpoint { reason }
    }
}
