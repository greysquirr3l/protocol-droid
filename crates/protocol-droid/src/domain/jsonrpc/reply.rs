//! The reply document, assembled from a dispatched result and a request id.
//!
//! # Why this is not a codec
//!
//! [`Codec`](crate::adapters::secondary::codec) lifts and lowers a
//! [`ToolResult`](crate::domain::ir::ToolResult) and nothing else, so nothing
//! in the crate yet turns a [`Dispatcher`] result plus a request id into the
//! document a peer reads. Rather than reach past it, this module writes that
//! document and says so where a reader will find it. A request-side codec
//! landing later replaces these four functions and nothing else, because the
//! serve loop knows no method names and no revision: it hands a result and an
//! id to this module and writes the bytes it gets back.
//!
//! # Nothing is spliced, and that is the point
//!
//! The obvious implementation is string concatenation — take
//! [`JsonObject::as_str`] and write it between two members. It is faster and
//! it is **wrong**, and the reason is worth stating because the mistake is
//! invisible in review and on the happy path.
//!
//! [`JsonObject::new`] validates its text and then stores it *verbatim*. JSON
//! permits whitespace between tokens, so `JsonObject::new("{\n\"a\":1\n}")`
//! succeeds and `as_str` hands back two raw newlines. Splicing that into a
//! newline-delimited frame splits one reply into two lines, and a peer reading
//! them sees two malformed frames instead of an answer. No test of the handler
//! would notice, because the corruption happens afterwards, in the transport.
//!
//! So every document here is built as a [`serde_json::Value`] and serialised
//! once. `Value`'s writer escapes control characters rather than passing them
//! through, which makes a raw newline in a reply structurally impossible
//! instead of merely unlikely — and it is one code path, so there is no second
//! serialiser to drift from the first.
//!
//! # The id is re-escaped, not debug-printed
//!
//! [`WireId::to_json`] is the right answer for a caller splicing an id, and
//! `format!("{id:?}")` is the wrong one in a way this crate has already paid
//! for: `Debug` for a string is `escape_debug`, which writes DEL as `\u{7f}`.
//! That is Rust syntax and not JSON, so the peer would see a parse error rather
//! than its own id. Both builders below hand the id to `serde_json` as a
//! `Value`, which escapes it the same way `to_json` does, for the same reason.
//!
//! # What a parse failure is answered with
//!
//! A frame that is not JSON has no id — there was never one to read — so
//! [`unidentified_error_reply`] writes `"id": null`, which is what the
//! specification requires and the only honest answer available. A caller that
//! invents an id for a frame it could not parse is telling the peer something
//! about a message the peer never successfully sent.

use serde_json::{Map, Value};

use crate::domain::ir::{IrErrorCode, JsonObject};
use crate::domain::jsonrpc::WireId;

/// Why a reply could not be assembled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReplyError {
    /// The result could not be read back as JSON.
    ///
    /// Unreachable for a [`JsonObject`], which validated the text it stores.
    /// It is a variant rather than an `unwrap` because the alternative is a
    /// claim this module cannot make: a future constructor that stored
    /// something it had not parsed would turn that `unwrap` into a panic on a
    /// path that runs for every reply a server ever sends.
    #[error("the result could not be read back as JSON")]
    UnreadableResult,
}

/// The members every reply carries before its own.
fn base_members() -> Map<String, Value> {
    let mut members = Map::new();
    members.insert("jsonrpc".to_owned(), Value::from("2.0"));
    members
}

/// [`base_members`] plus the id the answer belongs to.
fn reply_members(id: &WireId<'_>) -> Map<String, Value> {
    let mut members = base_members();
    members.insert("id".to_owned(), id_value(id));
    members
}

/// The id as a `Value` — a number or a string, never a string containing
/// digits.
fn id_value(id: &WireId<'_>) -> Value {
    match id {
        WireId::Number(value) => Value::from(*value),
        WireId::Text(text) => Value::from(text.as_ref()),
    }
}

/// The `error` member.
fn error_member(code: IrErrorCode, message: &str) -> Value {
    error_member_with_data(code, message, None)
}

/// The `error` member, with the `data` a code may require.
///
/// `None` adds nothing at all rather than adding `"data": null`: the JSON-RPC
/// specification marks `data` optional, and a peer that branches on its
/// presence would see a member that says nothing.
fn error_member_with_data(code: IrErrorCode, message: &str, data: Option<&Value>) -> Value {
    let mut error = Map::new();
    error.insert("code".to_owned(), Value::from(code.code()));
    error.insert("message".to_owned(), Value::from(message));
    if let Some(data) = data {
        error.insert("data".to_owned(), data.clone());
    }
    Value::Object(error)
}

/// Reads a dispatched result back so it can be nested in a document.
///
/// The parse cannot fail — the text was validated when the `JsonObject` was
/// built — but the signature says it can, because a function that can fail and
/// claims it cannot is a promise this crate does not keep. The caller answers a
/// genuine failure with an internal error rather than guessing.
fn read_back(result: &JsonObject) -> Result<Value, ReplyError> {
    serde_json::from_str::<Value>(result.as_str()).map_err(|_| ReplyError::UnreadableResult)
}

/// A successful reply to the request with this id.
///
/// # Errors
///
/// [`ReplyError::UnreadableResult`] when the result cannot be read back as
/// JSON, which a validated [`JsonObject`] should not be able to cause.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::jsonrpc::reply::result_reply;
/// use protocol_droid::domain::ir::JsonObject;
/// use protocol_droid::domain::jsonrpc::WireId;
///
/// # fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let id = WireId::text("r1");
/// let result = JsonObject::new(r#"{"tools":[]}"#)?;
///
/// let document = result_reply(&id, &result)?.to_string();
/// assert_eq!(
///     document,
///     r#"{"id":"r1","jsonrpc":"2.0","result":{"tools":[]}}"#,
/// );
/// # Ok(())
/// # }
/// ```
pub fn result_reply(id: &WireId<'_>, result: &JsonObject) -> Result<Value, ReplyError> {
    let mut members = reply_members(id);
    members.insert("result".to_owned(), read_back(result)?);
    Ok(Value::Object(members))
}

/// A failed reply to the request with this id.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::jsonrpc::reply::error_reply;
/// use protocol_droid::domain::ir::IrErrorCode;
/// use protocol_droid::domain::jsonrpc::WireId;
///
/// let id = WireId::Number(7);
/// let document = error_reply(&id, IrErrorCode::MethodNotFound, "no such method").to_string();
///
/// assert_eq!(
///     document,
///     r#"{"error":{"code":-32601,"message":"no such method"},"id":7,"jsonrpc":"2.0"}"#,
/// );
/// ```
#[must_use]
pub fn error_reply(id: &WireId<'_>, code: IrErrorCode, message: &str) -> Value {
    let mut members = reply_members(id);
    members.insert("error".to_owned(), error_member(code, message));
    Value::Object(members)
}

/// A failed reply carrying the structured `data` some codes require.
///
/// SEP-2575's `-32022` is the case this exists for: its `data` names the
/// revision the client asked for and every revision this server speaks, and a
/// client cannot retry correctly without both. `error_reply` cannot express
/// that, so this is a second function rather than an optional argument on the
/// first — every other error in the crate has no data, and a parameter they
/// would all pass `None` to is a parameter nobody reads.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::jsonrpc::reply::error_reply_with_data;
/// use protocol_droid::domain::ir::{IrErrorCode, JsonObject};
/// use protocol_droid::domain::jsonrpc::WireId;
///
/// let data = JsonObject::new(r#"{"requested":"2027-01-01","supported":["2026-07-28"]}"#)?;
/// let document =
///     error_reply_with_data(&WireId::text("r1"), IrErrorCode::UNSUPPORTED_PROTOCOL_VERSION,
///         "unsupported version", &data).to_string();
///
/// assert!(document.contains(r#""code":-32022"#));
/// assert!(document.contains(r#""supported":["2026-07-28"]"#));
/// # Ok::<(), protocol_droid::domain::ir::JsonObjectError>(())
/// ```
#[must_use]
pub fn error_reply_with_data(
    id: &WireId<'_>,
    code: IrErrorCode,
    message: &str,
    data: &JsonObject,
) -> Value {
    let Ok(data) = read_back(data) else {
        // A `JsonObject` was validated when it was built, so this branch is
        // unreachable in practice. Answering with the data-less error is worse
        // than answering with something a client can act on, so the fallback
        // is the one that still carries the code.
        return error_reply(id, code, message);
    };
    let mut members = reply_members(id);
    members.insert(
        "error".to_owned(),
        error_member_with_data(code, message, Some(&data)),
    );
    Value::Object(members)
}

/// A failed reply to a frame that never had an id to answer.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::jsonrpc::reply::unidentified_error_reply;
/// use protocol_droid::domain::ir::IrErrorCode;
///
/// let document =
///     unidentified_error_reply(IrErrorCode::ParseError, "the frame is not valid JSON");
///
/// assert_eq!(document.get("id"), Some(&serde_json::Value::Null));
/// ```
#[must_use]
pub fn unidentified_error_reply(code: IrErrorCode, message: &str) -> Value {
    let mut members = base_members();
    members.insert("id".to_owned(), Value::Null);
    members.insert("error".to_owned(), error_member(code, message));
    Value::Object(members)
}

#[cfg(test)]
mod tests {
    use super::{ReplyError, base_members, error_reply, result_reply, unidentified_error_reply};
    use crate::domain::ir::{IrErrorCode, JsonObject};
    use crate::domain::jsonrpc::WireId;

    /// What a test here can fail with, so a fallible call still propagates
    /// instead of unwrapping.
    #[derive(Debug)]
    enum Failure {
        /// A fixture was not a JSON object.
        Object(crate::domain::ir::JsonObjectError),
        /// A builder refused a value it should have accepted.
        Reply(ReplyError),
        /// A document this module produced did not read back as JSON, which a
        /// `serde_json::Value` should make impossible.
        Json(serde_json::Error),
    }

    impl std::fmt::Display for Failure {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Object(error) => write!(f, "bad fixture: {error}"),
                Self::Reply(error) => write!(f, "reply refused: {error}"),
                Self::Json(error) => write!(f, "a reply did not parse: {error}"),
            }
        }
    }

    impl std::error::Error for Failure {}

    impl From<crate::domain::ir::JsonObjectError> for Failure {
        fn from(error: crate::domain::ir::JsonObjectError) -> Self {
            Self::Object(error)
        }
    }

    impl From<ReplyError> for Failure {
        fn from(error: ReplyError) -> Self {
            Self::Reply(error)
        }
    }

    impl From<serde_json::Error> for Failure {
        fn from(error: serde_json::Error) -> Self {
            Self::Json(error)
        }
    }

    /// A successful reply carries the id, the version, and the result.
    ///
    /// Byte-exact on purpose. This is the byte layer, and the member order is
    /// `serde_json::Map`'s — a `BTreeMap`, since `preserve_order` is off — so
    /// the order is deterministic and a change to it would arrive as a
    /// dependency change worth noticing, not as a stylistic preference.
    #[test]
    fn a_result_reply_carries_all_three() -> Result<(), Failure> {
        let id = WireId::text("r1");
        let result = JsonObject::new(r#"{"content":[]}"#)?;

        let document = result_reply(&id, &result)?.to_string();

        assert_eq!(
            document,
            r#"{"id":"r1","jsonrpc":"2.0","result":{"content":[]}}"#,
        );
        Ok(())
    }

    /// The id survives a round trip in both arms. A reply that quoted the
    /// number `7` as `"7"` would still parse here, and would be the wrong
    /// answer to a peer that is matching on type.
    #[test]
    fn the_id_keeps_its_type() -> Result<(), Failure> {
        let numeric = result_reply(&WireId::Number(7), &JsonObject::default())?.to_string();
        let textual = result_reply(&WireId::text("7"), &JsonObject::default())?.to_string();

        assert!(numeric.contains(r#""id":7"#), "{numeric}");
        assert!(textual.contains(r#""id":"7""#), "{textual}");
        Ok(())
    }

    /// An id comes back as JSON, not as Rust syntax. `format!("{id:?}")` is the
    /// wrong one-liner: `Debug` for `str` is `escape_debug`, which writes a
    /// control character as `\u{7f}` — Rust syntax, and not a legal JSON
    /// escape, which is `\u007f`.
    ///
    /// The claim is asserted as a round trip rather than as a byte comparison,
    /// because what is being claimed is that the id survives as valid JSON, not
    /// that this crate picks a particular escape spelling. DEL is a legal raw
    /// byte inside a JSON string — only U+0000–U+001F, `"` and `\` must be
    /// escaped — and `serde_json` accordingly passes it through, so a test
    /// demanding `\u007f` would be testing that crate's escape table rather
    /// than this module. The quote in the fixture is the member that genuinely
    /// has to be escaped, and the round trip is what proves it was.
    #[test]
    fn an_escaped_id_is_escaped_as_json() -> Result<(), Failure> {
        let id = WireId::text("a\u{7f}b\"c");

        let document = result_reply(&id, &JsonObject::default())?.to_string();

        // The Rust spelling is the bug this test exists to keep out.
        assert!(!document.contains("u{7f}"), "{document}");
        let read_back = serde_json::from_str::<serde_json::Value>(&document)?;
        assert_eq!(read_back.get("id"), Some(&serde_json::json!("a\u{7f}b\"c")));
        Ok(())
    }

    /// A result holding raw newlines is serialised with them escaped, so the
    /// document cannot be split across two frames of a newline-delimited
    /// stream. `JsonObject::new` stores its text verbatim, so this is reachable
    /// from any handler that pretty-prints.
    #[test]
    fn a_pretty_printed_result_stays_on_one_line() -> Result<(), Failure> {
        let result = JsonObject::new("{\n  \"a\": 1\n}")?;

        let document = result_reply(&WireId::Number(1), &result)?.to_string();

        assert!(!document.contains('\n'), "{document}");
        Ok(())
    }

    /// An error reply carries the numeric code, not the variant name.
    ///
    /// `code` and `message` are members of the `error` **object**, not of the
    /// response around it. Every revision's schema says so: `JSONRPCError` in
    /// 2024-11-05 through 2025-06-18 carries an `error` object requiring
    /// `code` and `message`, and `JSONRPCErrorResponse` in 2025-11-25 and
    /// 2026-07-28 requires `error` and `jsonrpc`, with `error` a `$ref` to
    /// `Error`. A response with `message` at the top level is not a JSON-RPC
    /// error response at all.
    ///
    /// The bytes are pinned because this is a byte layer and the order is
    /// deterministic rather than incidental: `serde_json::Map` is a `BTreeMap`
    /// here (`preserve_order` is not enabled), so members serialise
    /// alphabetically and a reorder would mean a dependency change, not a
    /// stylistic one.
    #[test]
    fn an_error_reply_carries_the_numeric_code() {
        let document =
            error_reply(&WireId::Number(1), IrErrorCode::InvalidParams, "nope").to_string();

        assert_eq!(
            document,
            r#"{"error":{"code":-32602,"message":"nope"},"id":1,"jsonrpc":"2.0"}"#,
        );
    }

    /// A message that would break the log line it is quoted in is escaped.
    #[test]
    fn an_error_message_is_escaped() {
        let document = error_reply(
            &WireId::Number(1),
            IrErrorCode::InternalError,
            "line one\nline two",
        )
        .to_string();

        assert!(!document.contains('\n'), "{document}");
        assert!(document.contains(r"\n"), "{document}");
    }

    /// A frame with no id is answered with `null`, because there was no id to
    /// read rather than because the id was the word "null".
    #[test]
    fn an_unidentified_error_carries_a_null_id() {
        let document =
            unidentified_error_reply(IrErrorCode::ParseError, "the frame is not valid JSON");

        assert_eq!(document.get("id"), Some(&serde_json::Value::Null));
    }

    /// The two builders share one set of base members, so a future change to
    /// them cannot land on the success path and miss the failure path.
    #[test]
    fn both_builders_start_from_the_same_members() {
        let base = base_members();
        let identified = error_reply(&WireId::Number(1), IrErrorCode::InternalError, "x");
        let unidentified = unidentified_error_reply(IrErrorCode::InternalError, "x");

        for document in [&identified, &unidentified] {
            assert_eq!(document.get("jsonrpc"), base.get("jsonrpc"));
        }
    }
}
