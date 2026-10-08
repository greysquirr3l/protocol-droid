//! The documents a client writes, and the one it reads back.
//!
//! # This is a mirror, not a second encoder
//!
//! A stdio client and a stdio server speak the same wire format, and the
//! server's half of it is
//! [`reply`](crate::domain::jsonrpc::reply). The two modules cannot be
//! merged — one is a driving adapter, one is a driven one, and a client crate
//! that cannot build a reply has no business knowing how — but they are one
//! format with one shape, and this one is kept deliberately beside its
//! counterpart: the members are inserted through [`serde_json::Map`], so
//! `jsonrpc`, `id`, `method` and `params` serialise in the same order they
//! always have, and a server reading a client request sees what it would see
//! from any other client.
//!
//! # The id is re-escaped, not debug-printed
//!
//! [`RequestId::to_json`] is the wrong one-liner here for the same reason
//! [`reply`] says so: `Debug` for a string is `escape_debug`, which writes DEL
//! as `\u{7f}` — Rust syntax, and not a legal JSON escape. So the id is handed
//! to `serde_json` as a [`Value`], which escapes it the way
//! [`WireId::to_json`](crate::domain::jsonrpc::WireId::to_json) does, for the
//! same reason, and a peer that chose an id full of control characters gets its
//! own id back rather than a parse error against its own request.
//!
//! # A reply is matched by id, not taken on faith
//!
//! [`reply_result`] is given the id the request went out with and refuses a
//! frame that answers a different one. A stdio server is free to send
//! notifications and server-initiated requests on the same stream the
//! replies arrive on, so the first frame after a request is not
//! automatically that request's answer — and a client that assumed it was
//! would hand its caller somebody else's result.
//!
//! A frame that is not a reply at all is [`RequestError::NotAnObject`] or
//! [`RequestError::MissingResult`], not a silent skip. On stdio, stdout is
//! the protocol; a peer that puts anything else there has broken it, and the
//! useful thing to do with that is say so.

use serde_json::{Map, Value};

use crate::domain::RequestId;
use crate::domain::ir::{IrErrorCode, JsonObject, MethodName};

/// What a peer said in place of the message an error reply is required to
/// carry. A constant rather than a per-call allocation, and a fixed string
/// rather than nothing, because a peer that reports a failure without saying
/// what it was is still reporting a failure.
const NO_PEER_MESSAGE: &str = "the peer reported an error with no message";

/// The request document, ready to frame.
///
/// # Errors
///
/// [`RequestError::UnreadableParams`] when the parameters cannot be read back
/// as JSON, which a validated [`JsonObject`] should not be able to cause. It is
/// a variant rather than an `unwrap` because the alternative is a claim this
/// module cannot make, and because a panic on the path that runs for every
/// request a client ever sends is the wrong place to defend a hypothesis.
///
/// # Examples
///
/// ```
/// use protocol_droid::application::jsonrpc::document::request_document;
/// use protocol_droid::domain::RequestId;
/// use protocol_droid::domain::ir::{JsonObject, MethodName};
///
/// # fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let method = MethodName::new("tools/list")?;
/// let document = request_document(&RequestId::number(1), &method, &JsonObject::default())?;
///
/// assert_eq!(
///     document.to_string(),
///     r#"{"id":1,"jsonrpc":"2.0","method":"tools/list","params":{}}"#,
/// );
/// # Ok(())
/// # }
/// ```
pub fn request_document(
    id: &RequestId,
    method: &MethodName,
    params: &JsonObject,
) -> Result<Value, RequestError> {
    let mut members = base_members();
    members.insert("id".to_owned(), id_value(id));
    members.insert("method".to_owned(), Value::from(method.as_str()));
    members.insert("params".to_owned(), read_back(params)?);
    Ok(Value::Object(members))
}

/// The notification document, ready to frame.
///
/// A notification has no id and never gets a reply, so this is the request
/// document without one rather than a request with a null id — the two are
/// different on the wire and a peer reads the difference.
///
/// # Errors
///
/// As [`request_document`].
///
/// # Examples
///
/// ```
/// use protocol_droid::application::jsonrpc::document::notification_document;
/// use protocol_droid::domain::ir::{JsonObject, MethodName};
///
/// # fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let method = MethodName::new("notifications/initialized")?;
/// let document = notification_document(&method, &JsonObject::default())?;
///
/// assert_eq!(
///     document.to_string(),
///     r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#,
/// );
/// # Ok(())
/// # }
/// ```
pub fn notification_document(
    method: &MethodName,
    params: &JsonObject,
) -> Result<Value, RequestError> {
    let mut members = base_members();
    members.insert("method".to_owned(), Value::from(method.as_str()));
    members.insert("params".to_owned(), read_back(params)?);
    Ok(Value::Object(members))
}

/// The result out of a reply frame, if that frame is this request's answer.
///
/// # Errors
///
/// [`RequestError::UnreadableFrame`] when the frame is not JSON, or not an
/// object; [`RequestError::IdMismatch`] when it answers a different request;
/// [`RequestError::MissingResult`] when it is neither a result nor an error;
/// [`RequestError::ResultNotAnObject`] when the result is not an object; and
/// [`RequestError::PeerError`] when the peer reported a failure, which is the
/// one of these a caller can act on by changing what it asked for.
///
/// The id comparison is the reason this takes `expected` rather than reading
/// the frame's own: a stream carrying notifications as well as replies hands
/// the caller frames in an order they did not choose.
///
/// # Examples
///
/// ```
/// use protocol_droid::application::jsonrpc::document::reply_result;
/// use protocol_droid::domain::RequestId;
///
/// # fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let frame = r#"{"id":1,"jsonrpc":"2.0","result":{"tools":[]}}"#;
///
/// let result = reply_result(frame, &RequestId::number(1))?;
/// assert_eq!(result.as_str(), r#"{"tools":[]}"#);
/// # Ok(())
/// # }
/// ```
pub fn reply_result(frame: &str, expected: &RequestId) -> Result<JsonObject, RequestError> {
    let document: Value = serde_json::from_str(frame).map_err(|_| RequestError::UnreadableFrame)?;
    let Value::Object(members) = &document else {
        return Err(RequestError::UnreadableFrame);
    };
    match members.get("id") {
        None => return Err(RequestError::MissingId),
        Some(found) if found != &id_value(expected) => {
            return Err(RequestError::IdMismatch {
                expected: expected.redacted(),
                found: redact_member(found),
            });
        }
        Some(_) => {}
    }
    if let Some(error) = members.get("error") {
        return Err(peer_error(error));
    }
    let Some(result) = members.get("result") else {
        return Err(RequestError::MissingResult);
    };
    JsonObject::from_value(result.clone()).map_err(|_| RequestError::ResultNotAnObject)
}

/// The members every document on this transport carries.
fn base_members() -> Map<String, Value> {
    let mut members = Map::new();
    members.insert("jsonrpc".to_owned(), Value::from("2.0"));
    members
}

/// The id as a `Value` — a number or a string, never a string containing
/// digits.
///
/// The four lines are deliberately identical to the ones in
/// [`reply`](crate::domain::jsonrpc::reply). They could be shared and
/// are not, because the two modules sit on opposite sides of the hexagonal
/// boundary and a shared helper between a primary and a secondary adapter would
/// be a module with no owner. The duplication is small, it is documented on
/// both sides, and the alternative — a client that cannot build its own wire
/// format — is worse.
fn id_value(id: &RequestId) -> Value {
    match id {
        RequestId::Number(value) => Value::from(*value),
        RequestId::Text(text) => Value::from(text.as_ref()),
    }
}

/// Reads a validated object back so it can be nested in a document.
///
/// The parse cannot fail — the text was validated when the [`JsonObject`] was
/// built — and the signature says it can anyway, for the reason
/// [`RequestError::UnreadableParams`] does.
fn read_back(params: &JsonObject) -> Result<Value, RequestError> {
    serde_json::from_str::<Value>(params.as_str()).map_err(|_| RequestError::UnreadableParams)
}

/// The `error` member, as the failure it is.
///
/// An error member with no numeric code is not a JSON-RPC error and is
/// reported as an unreadable frame rather than as a failure with an invented
/// code, because a code this client made up is one the caller would then
/// branch on.
fn peer_error(error: &Value) -> RequestError {
    let Value::Object(error) = error else {
        return RequestError::UnreadableFrame;
    };
    let Some(code) = error.get("code").and_then(Value::as_i64) else {
        return RequestError::UnreadableFrame;
    };
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or(NO_PEER_MESSAGE);
    RequestError::PeerError {
        code: IrErrorCode::from_code(code),
        message: message.to_owned(),
    }
}

/// A label for an id that was not the one we expected, without disclosing it.
///
/// Ids are peer-chosen and can carry anything, so a mismatch is reported the
/// way a log line is: enough to correlate, not enough to disclose.
fn redact_member(found: &Value) -> String {
    match found {
        Value::String(text) => RequestId::text(text).redacted(),
        other => other.to_string(),
    }
}

/// Why a document could not be built, or a frame could not be read as one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RequestError {
    /// The parameters could not be read back as JSON.
    ///
    /// Unreachable for a [`JsonObject`], which validated the text it stores.
    #[error("the params could not be read back as JSON")]
    UnreadableParams,

    /// A frame that was handed to [`reply_result`] is not a JSON object.
    #[error("the frame is not a JSON object")]
    UnreadableFrame,

    /// A reply carried no id, so there is nothing to match the request against.
    #[error("the frame carries no id")]
    MissingId,

    /// The frame answers a request other than the one being read.
    #[error("the frame answers id {found}, not {expected}")]
    IdMismatch {
        /// The id the request went out with, redacted.
        expected: String,
        /// The id the frame carries, redacted.
        found: String,
    },

    /// The frame is a JSON-RPC object with neither a result nor an error.
    #[error("the frame carries neither a result nor an error")]
    MissingResult,

    /// The result was present but was not a JSON object.
    #[error("the peer's result is not a JSON object")]
    ResultNotAnObject,

    /// The peer reported a failure, which is a result of the request and not a
    /// fault in the transport.
    #[error("the peer reported {code:?}: {message}")]
    PeerError {
        /// The code the peer sent.
        code: IrErrorCode,
        /// The message the peer sent.
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use assert_matches::assert_matches;

    use super::{RequestError, notification_document, reply_result, request_document};
    use crate::domain::RequestId;
    use crate::domain::ir::{
        IrErrorCode, JsonObject, JsonObjectError, MethodName, MethodNameError,
    };

    /// What a `?` in a test here can produce.
    ///
    /// The frames under test answer [`RequestError`], but the fixtures feeding
    /// them are built by the IR's own parsers, which answer
    /// [`MethodNameError`] and [`JsonObjectError`]. Collapsing the three lets
    /// every test body be a plain `?` rather than a `map_err` per fixture, and
    /// carrying the payloads means a failing test prints the parser's own
    /// account of what it refused instead of a bare variant name.
    #[derive(Debug)]
    #[expect(dead_code, reason = "the payloads are read by the derived Debug")]
    enum Failure {
        /// A frame this module built or read did not survive the round trip.
        Frame(RequestError),
        /// A method name the IR would not admit.
        Method(MethodNameError),
        /// A params object the IR would not admit.
        Params(JsonObjectError),
    }

    impl From<RequestError> for Failure {
        fn from(error: RequestError) -> Self {
            Self::Frame(error)
        }
    }

    impl From<MethodNameError> for Failure {
        fn from(error: MethodNameError) -> Self {
            Self::Method(error)
        }
    }

    impl From<JsonObjectError> for Failure {
        fn from(error: JsonObjectError) -> Self {
            Self::Params(error)
        }
    }

    /// The four members a request carries, in the order they serialise, and
    /// the same document any other client would put on the wire.
    #[test]
    fn a_request_carries_the_jsonrpc_member_id_method_and_params() -> Result<(), Failure> {
        let method = MethodName::new("tools/list")?;
        let params = JsonObject::new(r#"{"cursor":"c"}"#)?;

        let document = request_document(&RequestId::number(7), &method, &params)?.to_string();

        assert_eq!(
            document,
            r#"{"id":7,"jsonrpc":"2.0","method":"tools/list","params":{"cursor":"c"}}"#
        );
        Ok(())
    }

    /// A notification is a request without an id rather than a request with a
    /// null one. A peer reads those two differently and this is the difference.
    #[test]
    fn a_notification_has_no_id() -> Result<(), Failure> {
        let method = MethodName::new("notifications/initialized")?;

        let document = notification_document(&method, &JsonObject::default())?.to_string();

        assert!(!document.contains("\"id\""), "{document}");
        Ok(())
    }

    /// A string id stays a JSON string. `format!("{id:?}")` would too, but
    /// `Debug` is `escape_debug`, and a DEL in the id would be written as
    /// `\u{7f}` — Rust syntax, and not a legal JSON escape, so the peer would
    /// answer with a parse error against its own id.
    #[test]
    fn a_string_id_is_escaped_as_json() -> Result<(), Failure> {
        let method = MethodName::new("tools/list")?;
        let id = RequestId::text("a\u{7f}b\"c");

        let document = request_document(&id, &method, &JsonObject::default())?.to_string();

        assert!(!document.contains("u{7f}"), "{document}");
        let read_back: Value =
            serde_json::from_str(&document).map_err(|_| RequestError::UnreadableFrame)?;
        assert_eq!(read_back.get("id"), Some(&Value::from("a\u{7f}b\"c")));
        Ok(())
    }

    /// The result comes back as the object the peer sent, not as the whole
    /// envelope: a caller asking `tools/list` wants the list.
    #[test]
    fn a_result_is_read_back_as_an_object() -> Result<(), Failure> {
        let frame = r#"{"id":1,"jsonrpc":"2.0","result":{"tools":[]}}"#;

        let result = reply_result(frame, &RequestId::number(1))?;

        assert_eq!(result.as_str(), r#"{"tools":[]}"#);
        Ok(())
    }

    /// A stream carrying notifications as well as replies hands the caller
    /// frames in an order they did not choose, so a frame that answers a
    /// different request is refused rather than handed over as this one's
    /// answer.
    #[test]
    fn a_reply_for_another_request_is_refused() {
        let frame = r#"{"id":2,"jsonrpc":"2.0","result":{}}"#;

        assert_matches!(
            reply_result(frame, &RequestId::number(1)),
            Err(RequestError::IdMismatch { .. })
        );
    }

    /// A text id and the number that spells it are different ids, which is the
    /// point of the comparison being on the parsed value rather than on text.
    #[test]
    fn a_text_id_is_not_its_numeric_spelling() {
        let frame = r#"{"id":7,"jsonrpc":"2.0","result":{}}"#;

        assert_matches!(
            reply_result(frame, &RequestId::text("7")),
            Err(RequestError::IdMismatch { .. })
        );
    }

    /// An error reply is a result of the request, and it arrives as the peer's
    /// code and message rather than as a transport failure the caller cannot
    /// tell from a broken pipe.
    #[test]
    fn an_error_reply_carries_the_code_the_peer_sent() {
        let frame =
            r#"{"error":{"code":-32601,"message":"no such method"},"id":1,"jsonrpc":"2.0"}"#;

        assert_matches!(
            reply_result(frame, &RequestId::number(1)),
            Err(RequestError::PeerError {
                code: IrErrorCode::MethodNotFound,
                ..
            })
        );
    }

    /// A frame that is neither an object nor a result is a broken protocol,
    /// and saying so beats handing the caller something they have to guess
    /// about.
    #[test]
    fn a_frame_that_is_not_an_object_is_refused() {
        assert_matches!(
            reply_result("not json at all", &RequestId::number(1)),
            Err(RequestError::UnreadableFrame)
        );
        assert_matches!(
            reply_result("[1,2,3]", &RequestId::number(1)),
            Err(RequestError::UnreadableFrame)
        );
    }

    /// A frame with neither member is not an answer, and reading a `null`
    /// result out of it would hand the caller a value the peer never sent.
    #[test]
    fn a_frame_with_neither_result_nor_error_is_refused() {
        let frame = r#"{"id":1,"jsonrpc":"2.0"}"#;

        assert_matches!(
            reply_result(frame, &RequestId::number(1)),
            Err(RequestError::MissingResult)
        );
    }

    /// A reply whose result is not an object cannot become a [`JsonObject`],
    /// and the port this feeds hands one back — so the mismatch is reported
    /// rather than coerced.
    #[test]
    fn a_result_that_is_not_an_object_is_refused() {
        let frame = r#"{"id":1,"jsonrpc":"2.0","result":[1,2,3]}"#;

        assert_matches!(
            reply_result(frame, &RequestId::number(1)),
            Err(RequestError::ResultNotAnObject)
        );
    }
}
