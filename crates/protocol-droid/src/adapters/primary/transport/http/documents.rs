//! The JSON-RPC documents a server writes, and the reading of the ones a peer
//! writes back.
//!
//! Shared by every server transport that answers in JSON-RPC: the Streamable
//! HTTP handler and the legacy HTTP+SSE dispatcher both build their replies
//! here, so a refusal reads the same on either.

use serde_json::Value;

use crate::adapters::primary::transport::http::exchange::Answer;
use crate::adapters::primary::transport::http::refusal::Refusal;
use crate::application::server::context::ExchangeError;
use crate::domain::ir::{IrErrorCode, JsonObject};
use crate::domain::jsonrpc::reply::{self, ReplyError};
use crate::domain::jsonrpc::{Envelope, Frame, WireId, decode};

/// The `id` a reply must be written under, borrowed from the body it answers.
pub(in crate::adapters::primary::transport) type Id<'a> = WireId<'a>;

/// The JSON-RPC success document for `result`.
///
/// A result with no wire form is this server's own bug rather than anything a
/// peer did, so it becomes a refusal rather than a frame that decodes to
/// something else.
pub(in crate::adapters::primary::transport) fn result_document(
    id: Option<&Id<'_>>,
    result: &JsonObject,
) -> Result<Value, Refusal> {
    let id = id.ok_or_else(|| {
        Refusal::Handler("a result was produced for a request that asked no question".to_owned())
    })?;
    reply::result_reply(id, result).map_err(|error| match error {
        ReplyError::UnreadableResult => {
            Refusal::Handler("the dispatcher's result has no JSON form".to_owned())
        }
    })
}

/// The JSON-RPC error document for a refused request.
///
/// Every refusal gets an answer. The alternative is to drop it, which leaves
/// the peer waiting on a response that is never coming — and a server that
/// hangs is worse than one that says no. The code is the one the error carries,
/// not one this module picked: the message is never the method name the peer
/// chose, because a method name is a JSON string a peer controls and JSON
/// permits a newline inside one.
/// A refusal rendered as the JSON-RPC error a peer will read.
pub(in crate::adapters::primary::transport) fn refusal_document_from_refusal(
    id: Option<&WireId<'_>>,
    refusal: &Refusal,
) -> Value {
    id.map_or_else(
        || reply::unidentified_error_reply(IrErrorCode::InternalError, &refusal.to_string()),
        |id| reply::error_reply(id, IrErrorCode::InternalError, &refusal.to_string()),
    )
}

pub(in crate::adapters::primary::transport) fn refusal_document(
    id: Option<&WireId<'_>>,
    error: &crate::application::server::DispatchError,
) -> Value {
    let (code, message) = error.as_wire();
    // A code whose specification gives it a `data` member — `-32022`'s
    // supported revisions, `-32021`'s required capabilities — is written with
    // it: a code without its data tells the client the problem and withholds
    // the fix.
    match (id, error.wire_data()) {
        (Some(id), Some(data)) => reply::error_reply_with_data(id, code, &message, &data),
        (Some(id), None) => reply::error_reply(id, code, &message),
        (None, _) => reply::unidentified_error_reply(code, &message),
    }
}

/// The method name in a body, if it is a single routable request.
///
/// Owned rather than borrowed: the envelope borrows from the buffer `decode`
/// parsed, and that buffer is dropped when this returns. A body that is a
/// batch or a reply shape reads as `None` here and takes the same arm, because
/// neither is something this handler should open a session from.
pub(in crate::adapters::primary::transport) fn method_of(body: &[u8]) -> Option<String> {
    let decoded = decode(body).ok()?;
    let Frame::Single(envelope) = decoded else {
        return None;
    };
    envelope.method_str().map(str::to_owned)
}

/// The params object a request carries, or an empty one.
///
/// A request with no `params` is a request for a method that takes none, so
/// the empty object is right rather than a guess. Valid JSON that is not an
/// object is a well-formed request with a bad argument, which is `-32602`
/// rather than a parse failure.
pub(in crate::adapters::primary::transport) fn params_of(
    envelope: &Envelope<'_>,
) -> Result<JsonObject, Refusal> {
    envelope.params().map_or_else(
        || Ok(JsonObject::default()),
        |raw| {
            JsonObject::new(raw.get()).map_err(|_| Refusal::MalformedBody {
                reason: "params is not a JSON object".to_owned(),
            })
        },
    )
}

/// The numeric id a wire id carries, if it is one.
///
/// `None` for a string id or an absent one. A server-initiated request's id is
/// minted as a number, so a string id on an inbound frame is the peer answering
/// something this server did not ask — which is a message to discard, not one
/// to guess a correlation for.
pub(in crate::adapters::primary::transport) const fn id_as_i64(id: &WireId<'_>) -> Option<i64> {
    match id {
        WireId::Number(value) => Some(*value),
        WireId::Text(_) => None,
    }
}

/// What a peer said in answer to a server-initiated request.
///
/// A JSON-RPC `error` member becomes the refusal it is — the peer's own words,
/// not a re-phrasing — because a tool asked a question and the answer "the
/// client declined" is different from the answer "the server broke", and
/// collapsing them costs the caller the one distinction it could act on.
pub(in crate::adapters::primary::transport) fn answer_of(body: &[u8]) -> Answer {
    let Ok(document) = serde_json::from_slice::<Value>(body) else {
        return Err(ExchangeError::new("the answer was not JSON"));
    };
    if let Some(failure) = document.get("error") {
        return Err(ExchangeError::new(failure.to_string()));
    }
    document.get("result").map_or_else(
        || {
            Err(ExchangeError::new(
                "the answer carried neither a result nor an error",
            ))
        },
        |result| {
            JsonObject::from_value(result.clone())
                .map_err(|error| ExchangeError::new(error.to_string()))
        },
    )
}
