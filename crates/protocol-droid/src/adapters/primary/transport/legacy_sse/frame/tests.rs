//! What the two frame kinds are *for*.
//!
//! The `Endpoint` arm is the load-bearing one. A `Value::String` holding
//! `/messages?sessionId=1` serialises to `"/messages?sessionId=1"` — quotes and
//! all — and a client that reads the `data` of an `endpoint` event gets those
//! quotes as part of the URL. Every implementation in the wild special-cases
//! this, and the special case is invisible until a client that does not know
//! about it tries to post to a path beginning with a double quote.
//!
//! So the first test here is a byte comparison against the exact frame the
//! specification describes, not a round-trip through the decoder. A round trip
//! would pass with the quotes in place, because the decoder is the same code
//! that wrote them.

use super::{LegacyFrame, LegacyTail};
use crate::adapters::primary::transport::http::{EventTail, FramePayload};
use crate::domain::{EventId, MessageEndpoint, names};

type TestError = Box<dyn std::error::Error>;

fn fail(message: impl Into<String>) -> TestError {
    message.into().into()
}

fn require<T: PartialEq + core::fmt::Debug>(
    got: &T,
    want: &T,
    because: &str,
) -> Result<(), TestError> {
    if got == want {
        Ok(())
    } else {
        Err(fail(format!("{because}: got {got:?}, wanted {want:?}")))
    }
}

/// [`require`] for the common case here, which is a `String` from the encoder
/// against a literal.
///
/// A separate helper because `&String` and `&str` do not unify through the
/// single type parameter of `require`, and writing `.as_str()` at every call
/// site would put the comparison's shape in the way of reading the claim.
fn require_str(got: &str, want: &str, because: &str) -> Result<(), TestError> {
    if got == want {
        Ok(())
    } else {
        Err(fail(format!("{because}:\n  got  {got:?}\n  want {want:?}")))
    }
}

fn endpoint(value: &str) -> Result<MessageEndpoint, TestError> {
    Ok(value.parse::<MessageEndpoint>()?)
}

const fn first() -> EventId {
    EventId::new(1)
}

/// The exact bytes the specification describes, with nothing quoted.
#[test]
fn an_endpoint_frame_is_a_bare_uri_and_not_a_json_string() -> Result<(), TestError> {
    let frame = LegacyFrame::Endpoint(endpoint("/messages?sessionId=1")?);
    let event = frame.to_frame(first())?;
    require_str(
        &event.encode(),
        "event: endpoint\nid: 1\ndata: /messages?sessionId=1\n\n",
        "the endpoint event's data is a bare URI, so a client that reads the data verbatim gets a path",
    )
}

/// The claim stated as its negation, because that is the shape of the bug: it
/// is not enough that the happy path has no quotes, the value must not be
/// *able* to acquire them by going through a JSON serialiser.
#[test]
fn an_endpoint_uri_is_never_quoted_the_way_a_value_would_be() -> Result<(), TestError> {
    let value = serde_json::Value::String("/messages?sessionId=1".to_owned());
    let quoted = serde_json::to_string(&value)?;
    require_str(
        &quoted,
        "\"/messages?sessionId=1\"",
        "a Value::String would serialise with quotes, which is the mistake this enum exists to prevent",
    )
}

/// The `event` name is what tells a client this frame is not a JSON-RPC
/// message, and the omission is what tells it a `Message` frame is.
#[test]
fn a_message_frame_is_unnamed() -> Result<(), TestError> {
    let frame = LegacyFrame::Message(serde_json::json!({"jsonrpc": "2.0", "id": 1}));
    let event = frame.to_frame(first())?;
    require(
        &event.name().is_none(),
        &true,
        "a message frame carries no event name, so a client reading this transport cannot mistake it for the endpoint event",
    )?;
    require_str(
        &event.encode(),
        "id: 1\ndata: {\"id\":1,\"jsonrpc\":\"2.0\"}\n\n",
        "and its data is the JSON-RPC object on its own line, with no event name in front of it",
    )
}

/// Both kinds land on one tail, and they are numbered in the order they were
/// pushed. The numbering is the order — the ring is what decides which frame a
/// client reads first, and a client reads frame 1 to learn where to post. That
/// it is *this* transport pushing the endpoint first is the router's claim, and
/// the byte order on the wire is asserted in `router/tests.rs`; what is
/// asserted here is that both kinds are the same kind of thing, sharing one
/// sequence.
#[test]
fn both_kinds_share_one_numbered_sequence() -> Result<(), TestError> {
    let capacity = core::num::NonZeroUsize::new(4).ok_or("a capacity of zero is not a capacity")?;
    let tail: LegacyTail = EventTail::new(capacity);
    let endpoint_id = tail.push(LegacyFrame::Endpoint(endpoint("/messages?sessionId=7")?));
    let message_id = tail.push(LegacyFrame::Message(serde_json::json!({"jsonrpc": "2.0"})));
    require(
        &tail.len(),
        &2,
        "both frames are on the tail, because they share one stream",
    )?;
    require(
        &(endpoint_id.get(), message_id.get()),
        &(1, 2),
        "and the endpoint pushed first is event 1, which is the frame a client reads first",
    )?;
    require(
        &tail.last().get(),
        &2,
        "so the tail's cursor is past both, and a client that reconnects asking for event 2 is told there is nothing newer",
    )
}

#[test]
fn a_frame_reports_which_kind_it_is() -> Result<(), TestError> {
    let endpoint = LegacyFrame::Endpoint(endpoint("/messages?sessionId=1")?);
    let message = LegacyFrame::Message(serde_json::json!({}));

    require(
        &endpoint.endpoint().map(MessageEndpoint::as_str),
        &Some("/messages?sessionId=1"),
        "an endpoint frame answers `endpoint` with its URI",
    )?;
    require(
        &endpoint.message().is_none(),
        &true,
        "and `message` with nothing, because it is one or the other",
    )?;
    require(
        &message.message().is_some(),
        &true,
        "a message frame answers `message` with the object",
    )?;
    require(
        &message.endpoint().is_none(),
        &true,
        "and `endpoint` with nothing",
    )
}

/// The two spec-defined names, checked here so a rename in `names` cannot leave
/// the encoder writing a name no client recognises.
#[test]
fn the_endpoint_event_is_named_by_the_specification() -> Result<(), TestError> {
    require(
        &names::ENDPOINT.as_str(),
        &"endpoint",
        "the 2024-11-05 specification names this event `endpoint`",
    )?;
    require(
        &names::MESSAGE.as_str(),
        &"message",
        "and the data events it defines are `message`, which this transport leaves unnamed because an unnamed event is what every SSE parser already reads",
    )
}
