//! The two frame kinds a legacy stream carries, and the port that fills them.
//!
//! # Why the ring holds a type rather than a `Value`
//!
//! Because one of the two frames is not JSON. The `endpoint` event's data is
//! a bare URI, and `Value::String("/messages?sessionId=x")` would put quotes
//! in it — `data: "/messages?sessionId=x"` — which is not a URI and is not
//! what the specification says to send. A client doing the specified thing
//! with the event hands that to a URL parser and fails.
//!
//! So the payload is an enum and the *variant* picks the name, and it is
//! impossible to send a `message` event carrying a URI or an `endpoint` event
//! carrying JSON. That is the reason this is an enum rather than a
//! `MessageEndpoint` plus a `Value` in a struct: the struct would let a
//! caller pair the wrong one with the wrong name, and the mismatch would be
//! found by a peer in production.
//!
//! # Why the `endpoint` frame rides the ring
//!
//! Because the specification requires it to be the **first** event, and
//! ordering enforced by a struct field is a convention while ordering enforced
//! by the ring's insertion order is a guarantee. The router pushes the
//! `endpoint` frame before it opens the stream and before the session can
//! receive anything, so a `message` frame physically cannot precede it. A
//! router that prepended the frame to the response body instead would have
//! two things to keep in order — the frame and the stream — and one place to
//! get it wrong.
//!
//! The cost is that the frame is a ring entry, so it is replayed by a client
//! that reconnects. That is harmless and in fact useful: a reconnecting
//! client needs the endpoint again. The legacy transport has no `Last-Event-ID`
//! and no resume, so the ring is used here for ordering, not for replay.

use serde_json::Value;

use crate::adapters::primary::transport::http::{
    EventTail, FramePayload, Refusal, SseError, SseEvent,
};
use crate::domain::registry::AuthContext;
use crate::domain::session::SessionId;
use crate::domain::{EventId, McpSessionId, MessageEndpoint, names};

/// One frame on a legacy stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyFrame {
    /// The `endpoint` event: where the client POSTs its messages.
    ///
    /// Exactly one of these per session, and the first frame on the stream.
    Endpoint(MessageEndpoint),
    /// A JSON-RPC message, as a `message` event.
    Message(Value),
}

impl LegacyFrame {
    /// The endpoint, if this is one.
    #[must_use]
    pub const fn endpoint(&self) -> Option<&MessageEndpoint> {
        match self {
            Self::Endpoint(endpoint) => Some(endpoint),
            Self::Message(_) => None,
        }
    }

    /// The message, if this is one.
    #[must_use]
    pub const fn message(&self) -> Option<&Value> {
        match self {
            Self::Endpoint(_) => None,
            Self::Message(value) => Some(value),
        }
    }
}

impl FramePayload for LegacyFrame {
    /// # Errors
    ///
    /// [`SseError::Unserialisable`] for a [`Message`] whose value has no JSON
    /// form — a non-string map key. A [`Endpoint`](Self::Endpoint) cannot
    /// fail: the URI is already parsed, and `MessageEndpoint` refused every
    /// value that could terminate a line.
    fn to_frame(&self, id: EventId) -> Result<SseEvent, SseError> {
        match self {
            Self::Endpoint(endpoint) => Ok(SseEvent::named_with_id(
                names::ENDPOINT,
                id,
                endpoint.as_str().to_owned(),
            )),
            Self::Message(value) => SseEvent::json(id, value),
        }
    }
}

/// A legacy session's server→client tail.
pub type LegacyTail = EventTail<LegacyFrame>;

/// One legacy session: the stream the server writes to, and whatever the
/// dispatcher keeps for it.
///
/// The store holds these, so a session that expires or is deleted takes its
/// dispatcher state with it. A dispatcher that kept its own table keyed by
/// session id would have no way to learn the session had gone.
#[derive(Debug)]
pub struct LegacySession<S> {
    tail: LegacyTail,
    state: S,
}

impl<S> LegacySession<S> {
    /// A session writing to `tail`, with `state` kept for the dispatcher.
    #[must_use]
    pub const fn new(tail: LegacyTail, state: S) -> Self {
        Self { tail, state }
    }

    /// The stream the server writes this session's frames to.
    #[must_use]
    pub const fn tail(&self) -> &LegacyTail {
        &self.tail
    }

    /// What the dispatcher kept when the session was opened.
    #[must_use]
    pub const fn state(&self) -> &S {
        &self.state
    }
}

/// What the protocol layer has to be able to do for the legacy `POST`.
///
/// A port, and it belongs here because the router is its consumer. One method,
/// because the router asks one question: *what should this client frame cause
/// the server to send back?*
///
/// It is not [`RequestHandler`](crate::adapters::primary::transport::http::RequestHandler),
/// and the difference is not cosmetic. That trait returns an HTTP `Response`,
/// which is right when a request's answer travels in the response. Here it
/// does not: the answer goes onto the SSE stream, and the `POST`'s own response
/// carries only a status. Handing the router a `Response` it has to take apart
/// to find the body, and push that body somewhere else, is a shape that invites
/// the mistake of forwarding the `POST`'s response body directly to the client
/// — which is exactly the bug the 2025-03-26 client fallback has to avoid.
///
/// Returns a `Vec` because one frame can produce a response *and* notifications
/// the server chose to send, and a single-output signature would force the
/// caller to pick one.
///
/// # Examples
///
/// ```
/// use protocol_droid::adapters::primary::transport::http::Refusal;
/// use protocol_droid::adapters::primary::transport::legacy_sse::{LegacyDispatcher, LegacyTail};
/// use protocol_droid::domain::McpSessionId;
/// use protocol_droid::domain::session::SessionId;
/// use protocol_droid::domain::registry::AuthContext;
///
/// /// Answers every request with the frame it was sent, which is all this test needs.
/// struct Echo;
///
/// impl LegacyDispatcher for Echo {
///     type Session = ();
///
///     fn open(&self, _id: SessionId, _tail: &LegacyTail) {}
///
///     async fn dispatch(
///         &self,
///         _auth: &AuthContext,
///         _id: &McpSessionId,
///         _session: &(),
///         frame: &serde_json::Value,
///     ) -> Result<Vec<serde_json::Value>, Refusal> {
///         Ok(vec![frame.clone()])
///     }
/// }
/// # let _ = Echo;
/// ```
pub trait LegacyDispatcher: Send + Sync + 'static {
    /// What the dispatcher keeps for one session.
    ///
    /// Dropped with the session, so a subscription or a negotiated state held
    /// here ends when the session does.
    type Session: Send + Sync + 'static;

    /// Called once when a stream opens, before the `endpoint` event is sent.
    ///
    /// `tail` is the stream the server's own frames go to: notifications, and
    /// requests the server makes of the client. Keep it if you will write to it.
    fn open(&self, id: SessionId, tail: &LegacyTail) -> Self::Session;

    /// Handles one client frame, returning every frame to send back.
    ///
    /// `auth` is the principal the router resolved for **this** request, after
    /// the `Origin` check and the endpoint's [`AuthPolicy`] have both passed.
    /// It is the only place a legacy-SSE deployment gets to see who is asking:
    /// the router authenticates and this port is where that result goes, so an
    /// implementation that ignores it is serving every caller as anonymous.
    ///
    /// Per request rather than per session, for the reason it is on
    /// Streamable HTTP too — a bearer token can be refreshed or withdrawn
    /// between two `POST`s to one session id.
    ///
    /// A dispatcher may also write its answer to the session's tail itself and
    /// return nothing. The `POST` is answered `202` when this returns, so a
    /// handler that has to wait on the client (an elicitation, a sampling
    /// request) can answer on the stream once it has the client's reply instead
    /// of holding the `POST` open.
    ///
    /// # Errors
    ///
    /// [`Refusal::Handler`] if the frame cannot be handled. The frames are
    /// published to the session's tail in order, and a refusal here publishes
    /// nothing — a client whose request failed gets an error on the stream
    /// rather than a half-written batch, because a partial batch is
    /// indistinguishable from a complete one on a stream that carries no
    /// transaction.
    ///
    /// [`AuthPolicy`]: crate::adapters::primary::transport::http::AuthPolicy
    fn dispatch(
        &self,
        auth: &AuthContext,
        id: &McpSessionId,
        session: &Self::Session,
        frame: &Value,
    ) -> impl core::future::Future<Output = Result<Vec<Value>, Refusal>> + Send;
}

#[cfg(test)]
mod tests;
