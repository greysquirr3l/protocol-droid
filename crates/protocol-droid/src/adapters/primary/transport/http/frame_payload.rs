//! What a stream payload has to be able to become on the wire.
//!
//! # Why this exists
//!
//! Because "the payload is a JSON value" stopped being true when `2024-11-05`
//! arrived. On Streamable HTTP every frame is a `message` event and every
//! payload is JSON, so `EventTail<Value>` and a `Borrow<Value>` bound said
//! everything there was to say. The legacy transport sends one frame that is
//! **not** JSON: the `endpoint` event, whose data is a bare URI.
//!
//! A URI is not JSON. Wrapping it as `Value::String` would put quotes in the
//! data — `data: "/messages?sessionId=x"` — and a client doing what the
//! specification says to do with the event, hand that to a URL parser and fail.
//! The real servers write the bare URI, and so does this crate.
//!
//! # Why the trait is here and not in `domain/`
//!
//! Because it is an SSE question, not a domain one: it answers "how does this
//! payload become a frame", and the answer is a field name, an id, and a
//! serialisation. `EventName` and `SseEvent` are in this tree for the same
//! reason. A domain that knew about `event:` would be a domain modelling a
//! wire format, which is the thing the layering exists to prevent.
//!
//! # Why it is fallible
//!
//! Because encoding can fail and a stream cannot: a payload with no JSON form
//! — a non-string map key — has no wire representation, and the honest answer
//! is a refusal rather than a frame that decodes to something else. That
//! refusal is the server's own bug, which is why the caller reports it as a
//! `500` and not as the client's fault.

use serde_json::Value;

use crate::domain::EventId;

use super::sse::{SseError, SseEvent};

/// A payload an [`EventTail`](super::EventTail) can put on the wire.
///
/// The one method, because it is the only question the ring asks. The
/// alternative shape — a payload that *is* an [`SseEvent`] — was rejected
/// because it would push the id out to every call site, and the id is the
/// ring's business, not the payload's.
pub trait FramePayload: Send + 'static {
    /// This payload as one frame, positioned at `id`.
    ///
    /// # Errors
    ///
    /// [`SseError::Unserialisable`] if the payload has no wire form. The
    /// payload is this server's own data and the failure is this server's
    /// fault, so a caller reports it as a `500`.
    fn to_frame(&self, id: EventId) -> Result<SseEvent, SseError>;
}

/// A JSON payload becomes a `message` event — unnamed, because Streamable
/// HTTP does not name its frames and a name here would be a `2026-07-28`
/// client's problem to have an unexpected field.
impl FramePayload for Value {
    fn to_frame(&self, id: EventId) -> Result<SseEvent, SseError> {
        SseEvent::json(id, self)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::FramePayload;
    use crate::domain::EventId;

    type TestError = Box<dyn std::error::Error>;

    #[test]
    fn a_value_becomes_an_unnamed_frame_at_its_id() -> Result<(), TestError> {
        let frame = Value::String("hi".to_owned()).to_frame(EventId::new(4))?;
        if frame.name().is_some() {
            return Err("a Value frame came out named".into());
        }
        if frame.data() != "\"hi\"" {
            return Err(format!("encoded as {:?}", frame.data()).into());
        }
        Ok(())
    }
}
