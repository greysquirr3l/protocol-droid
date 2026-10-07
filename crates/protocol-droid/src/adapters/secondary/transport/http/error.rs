//! Every way a Streamable HTTP client call can fail, as one enum.
//!
//! The whole point of this file is that no `reqwest` type crosses the module
//! boundary. A caller above this layer matches on [`HttpClientError`] and never
//! learns that the transport happens to be `reqwest` today — the same reason
//! the domain never names `tokio`.
//!
//! Each variant is a *decision* the caller could act on, not a stack frame:
//! "the peer said no, and here is what it said" ([`HttpClientError::Refused`])
//! is a different thing to handle from "the socket died" ([`HttpClientError::Io`])
//! and from "the peer said something I cannot read"
//! ([`HttpClientError::MediaType`]).

use crate::adapters::primary::transport::http::SseError;
use crate::domain::McpSessionIdError;

/// How many characters of a refused response body to keep for the error.
///
/// A refusal body from this protocol is a JSON-RPC error object and fits. A
/// peer is not obliged to send one, and an error enum holding an unbounded
/// `String` turns a hostile response into a memory cost. Truncating says so
/// rather than implying the message is the whole truth.
const REFUSAL_PREVIEW: usize = 256;

/// Why a Streamable HTTP call did not produce a response the caller can use.
///
/// # Examples
///
/// Every variant is distinguishable, which is the reason this transport maps
/// into one enum instead of letting a `reqwest::Error` through:
///
/// ```
/// use protocol_droid::adapters::secondary::transport::http::HttpClientError;
///
/// // A refusal and a dead socket are different problems for a retry policy:
/// // one is the peer's answer, the other is a network worth waiting on. Both
/// // are `Err`, and a caller that treats `Err` as "retry" cannot tell them
/// // apart without matching on the variant.
/// let error = HttpClientError::MediaType { got: "text/html".into() };
/// assert!(matches!(error, HttpClientError::MediaType { .. }));
/// assert!(!matches!(error, HttpClientError::Io { .. }));
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HttpClientError {
    /// The request never reached the peer, or the response never came back.
    ///
    /// The only variant a caller should retry without thinking — and even then
    /// only for an idempotent request. A `POST` that may have been received and
    /// acted on before the socket failed is exactly the case the idempotency
    /// key exists for, and retrying it blind is how a tool runs twice.
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
        /// The first [`REFUSAL_PREVIEW`] characters of the body, decoded lossily.
        preview: String,
    },

    /// The peer answered `2xx` with a body this client cannot read.
    ///
    /// Kept apart from [`HttpClientError::Refused`] on purpose. A `200` with
    /// the wrong content type is the peer and the client disagreeing about what
    /// a response *is*, and no retry fixes that — a policy that treated it as a
    /// transport blip would loop against a server that answers identically
    /// every time.
    #[error("the peer answered 2xx with {got:?}, which is neither JSON nor an event stream")]
    MediaType {
        /// The `Content-Type` the peer answered with, verbatim.
        got: String,
    },

    /// The peer answered `2xx` with no `Content-Type` at all.
    #[error("the peer answered 2xx with no content type")]
    MissingMediaType,

    /// The peer answered `2xx` with a body that is not a JSON object.
    ///
    /// Its own variant, not [`HttpClientError::MediaType`]: that one means "not
    /// JSON *or* not an event stream" and leaves the caller to pick which. Here
    /// the peer committed to JSON and then sent something that is not a
    /// JSON-RPC message, which is a different fault with a different fix.
    #[error("the peer answered 2xx with a JSON body that is not an object: {got:?}")]
    NotAMessage {
        /// What came back, truncated to something loggable.
        got: String,
    },
    /// A body could not be encoded for sending, or a reply body could not be
    /// read back as JSON.
    ///
    /// Separate from [`HttpClientError::Io`] because that one carries a
    /// `reqwest::Error` and means the request never reached the peer. This is
    /// the opposite: the bytes arrived and could not be understood. Folding
    /// both into `Io` would report a marshalling fault as a network fault.
    #[error("the {method} body could not be {what}: {detail}")]
    Body {
        /// The HTTP method that was in flight.
        method: &'static str,
        /// What was being attempted, in a form that reads as a verb.
        what: &'static str,
        /// The decoder's own account.
        detail: String,
    },
    /// The event stream produced bytes that are not a valid SSE frame.
    #[error("the event stream produced a frame this client cannot read: {source}")]
    Frame {
        /// The frame decoder's account of what was wrong.
        source: SseError,
    },

    /// The event stream ended, or the socket broke, mid-frame.
    ///
    /// Not terminal on its own: a resumable read treats this as "reconnect
    /// from the last delivered id" and only surfaces it once the reconnect
    /// budget is gone. See [`HttpClientError::ResumeExhausted`].
    #[error("the event stream ended after {delivered} delivered event(s)")]
    StreamEnded {
        /// How many events the reader had actually delivered. This is the
        /// number that decides what a resume should ask for, and getting it
        /// wrong is how a resume either repeats an event or skips one.
        delivered: u64,
    },

    /// A resumable read ran out of reconnects.
    #[error(
        "the stream was not completed after {attempts} reconnect attempt(s), having delivered {delivered}; last failure: {source}"
    )]
    ResumeExhausted {
        /// How many reconnects were attempted.
        attempts: u32,
        /// Events delivered before the budget ran out.
        delivered: u64,
        /// The failure that ended the last attempt.
        source: Box<Self>,
    },

    /// A call that requires a session was made before one had been issued.
    #[error("this call needs a session id and the peer has not issued one yet")]
    NoSessionYet,

    /// The peer sent a `Mcp-Session-Id` this client cannot parse.
    ///
    /// Refused rather than ignored: a client that silently drops an
    /// unparseable session id and carries on sends its next request with no
    /// session header, and the resulting failure points at the session layer
    /// instead of at the header that was actually wrong.
    #[error("the peer sent an Mcp-Session-Id that is not a session id: {source}")]
    SessionId {
        /// What was wrong with it.
        source: McpSessionIdError,
    },

    /// The endpoint is not a URL a request can be built against.
    #[error("the endpoint {got:?} is not a usable URL: {reason}")]
    Url {
        /// The endpoint as given.
        got: String,
        /// The URL parser's account, rendered.
        reason: String,
    },
}

impl HttpClientError {
    /// Whether retrying the identical request could plausibly succeed.
    ///
    /// A convenience, not a policy: it says what the *error* implies, and the
    /// caller's idempotency story still decides whether a retry is safe. A
    /// `POST` that may already have run is `false` even when the socket died,
    /// because the honest answer there is "ask the idempotency store", not
    /// "send it again".
    #[must_use]
    pub const fn is_transient(&self) -> bool {
        matches!(self, Self::Io { .. } | Self::StreamEnded { .. })
    }

    /// The status the peer answered with, when there was one.
    ///
    /// `None` for every variant where the request never got an answer, which is
    /// the distinction a caller's logging actually needs.
    #[must_use]
    pub const fn status(&self) -> Option<reqwest::StatusCode> {
        match self {
            Self::Refused { status, .. } => Some(*status),
            _ => None,
        }
    }
}

/// A body preview that is bounded, lossy, and never panics on invalid UTF-8.
///
/// Bounded on characters rather than bytes so a multi-byte sequence at the cut
/// cannot split into a replacement character per byte and report a length
/// several times the limit.
pub(super) fn preview(bytes: &[u8]) -> String {
    let shown = bytes.get(..REFUSAL_PREVIEW).unwrap_or(bytes);
    String::from_utf8_lossy(shown)
        .chars()
        .take(REFUSAL_PREVIEW)
        .collect()
}
