//! The JSON a generated `#[tool]` handler answers with.
//!
//! # Why this lives here and not in the macro
//!
//! A generated handler has to turn two Rust things into one wire thing: a
//! deserialised argument struct, and a `Result<T, E>`. The shape of a failed
//! tool call — a text block under `content` with `isError` set — is the
//! specification's, not the macro author's. A macro that built the JSON itself
//! would be free to disagree with [`ToolResult::error`](crate::domain::ir::ToolResult::error)
//! and with the codec that lowers it, and nothing would notice until a client
//! read the difference.
//!
//! So the macro generates a call into [`success`] and [`failure`] and owns no
//! document shape at all.
//!
//! # Why `structuredContent` is decided here rather than by a codec
//!
//! It is not decided here — this function emits it, and the codec drops it for
//! a revision before `2025-06-18`. That split is deliberate: whether the field
//! _exists_ is a codec question, and whether the answer _has_ an object in it
//! is a Rust question, and putting both in one place would make the codec
//! re-derive a type it should be handed.

use serde::Serialize;

use crate::domain::ir::{JsonObject, JsonObjectError};

/// Why a generated handler could not produce an answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OutcomeError {
    /// The peer's `arguments` object did not deserialise into the handler's
    /// argument struct.
    ///
    /// Carries the deserialiser's own message. A second, shorter vocabulary for
    /// "that is not the right shape" would lose the one field a caller can act
    /// on, which is *which* field and *why*.
    #[error("the arguments do not match this tool: {0}")]
    Arguments(String),

    /// The handler's return value could not be serialised.
    ///
    /// A `Serialize` impl that fails is a bug in the handler's types — a
    /// non-string map key, a NaN — not a bad request, and the message says so
    /// rather than blaming the peer.
    #[error("the handler's answer cannot be encoded: {0}")]
    Answer(String),

    /// The assembled document was not a JSON object.
    ///
    /// Cannot happen for a value this function builds, and propagated rather
    /// than swallowed so that a future member with a different shape fails
    /// loudly instead of producing an empty result.
    #[error("the assembled result is not a JSON object: {0}")]
    NotAnObject(JsonObjectError),
}

/// A handler's answer, as a `tools/call` result.
///
/// An object answer becomes `structuredContent` **and** a text block holding
/// the same JSON. The duplication is the specification's own recommendation,
/// not redundancy: a `2025-06-18` client reads `structuredContent` and a
/// `2024-11-05` one reads the text, and a result carrying only the former is
/// unreadable to the latter. A non-object answer is a text block alone, since
/// there is no structured form to carry it.
///
/// `structuredContent` here is a `2025-06-18`-or-later value. A codec targeting
/// an earlier revision drops it, and that is the codec's call to make.
///
/// # Errors
///
/// [`OutcomeError::Answer`] when `answer` will not serialise — a bug in the
/// handler's own types.
pub fn success(answer: &impl Serialize) -> Result<JsonObject, OutcomeError> {
    let value =
        serde_json::to_value(answer).map_err(|error| OutcomeError::Answer(error.to_string()))?;

    let (structured, text) = match &value {
        serde_json::Value::Object(_) => (Some(value.clone()), value.to_string()),
        // A string answer is its own text. Taking `to_string` of it would give
        // `"hello"` *including the quotes*, because that is the JSON encoding
        // of a string rather than the string — a detail visible to every user
        // of a tool that returns a message.
        serde_json::Value::String(text) => (None, text.clone()),
        // `null` and everything else are their JSON form. There is no shorter
        // honest rendering of `42` that is still the same value.
        other => (None, other.to_string()),
    };

    let mut document = serde_json::json!({
        "content": [{ "type": "text", "text": text }],
        "isError": false,
    });
    if let (serde_json::Value::Object(map), Some(structured)) = (&mut document, structured) {
        map.insert("structuredContent".to_string(), structured);
    }

    JsonObject::from_value(document).map_err(OutcomeError::NotAnObject)
}

/// A handler's failure, as a `tools/call` result.
///
/// Always an error result rather than a transport failure, because a tool that
/// refuses is a normal answer: the call reached the tool and the tool said no.
/// The distinction matters to a caller deciding whether to retry, and folding
/// the two together would make every refusal look like a broken connection.
#[must_use]
pub fn failure(message: impl core::fmt::Display) -> JsonObject {
    let document = serde_json::json!({
        "content": [{ "type": "text", "text": message.to_string() }],
        "isError": true,
    });
    JsonObject::from_value(document).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use serde::Serialize;

    use super::{OutcomeError, failure, success};
    use crate::domain::ir::JsonObject;

    /// A return type that serialises to a JSON object.
    #[derive(Serialize)]
    struct ObjectAnswer {
        /// A member, so the object is not empty.
        total: i32,
    }

    /// A return type that serialises to a bare string.
    #[derive(Serialize)]
    struct TextAnswer(String);

    /// The document's `content` member, as the text blocks it holds.
    fn text_blocks(document: &JsonObject) -> Result<Vec<String>, String> {
        let parsed: serde_json::Value =
            serde_json::from_str(document.as_str()).map_err(|error| error.to_string())?;
        parsed
            .get("content")
            .and_then(serde_json::Value::as_array)
            .ok_or("the result has no content array")?
            .iter()
            .map(|block| {
                block
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| "a content block is not text".to_string())
            })
            .collect()
    }

    /// An object answer carries `structuredContent` and a text copy of it.
    ///
    /// The duplication is what lets a pre-`2025-06-18` client read the result
    /// at all, so it is asserted rather than left to a reader to infer.
    #[test]
    fn an_object_answer_becomes_structured_content_and_a_text_copy() -> Result<(), String> {
        let document = success(&ObjectAnswer { total: 7 }).map_err(|e| e.to_string())?;
        let parsed: serde_json::Value =
            serde_json::from_str(document.as_str()).map_err(|error| error.to_string())?;

        assert_eq!(
            parsed
                .get("structuredContent")
                .and_then(|value| value.get("total")),
            Some(&serde_json::Value::from(7)),
            "the object answer did not reach structuredContent"
        );
        let blocks = text_blocks(&document)?;
        if blocks.first() != Some(&"{\"total\":7}".to_string()) {
            return Err(format!("the text copy is not the object: {blocks:?}"));
        }
        Ok(())
    }

    /// A non-object answer is a text block alone, with no structured member.
    #[test]
    fn a_text_answer_becomes_a_text_block_and_nothing_else() -> Result<(), String> {
        let document = success(&TextAnswer("hello".to_string())).map_err(|e| e.to_string())?;
        let parsed: serde_json::Value =
            serde_json::from_str(document.as_str()).map_err(|error| error.to_string())?;

        if parsed.get("structuredContent").is_some() {
            return Err("a text answer produced structuredContent".to_string());
        }
        if text_blocks(&document)? != ["hello".to_string()] {
            return Err("the text answer did not survive".to_string());
        }
        Ok(())
    }

    /// A success is never an error result, whatever it carries.
    #[test]
    fn a_success_is_never_flagged_as_an_error() -> Result<(), String> {
        let document = success(&ObjectAnswer { total: 0 }).map_err(|e| e.to_string())?;
        let parsed: serde_json::Value =
            serde_json::from_str(document.as_str()).map_err(|error| error.to_string())?;
        assert_eq!(parsed.get("isError"), Some(&serde_json::Value::Bool(false)));
        Ok(())
    }

    /// A failure carries the handler's message and is flagged.
    #[test]
    fn a_failure_carries_its_message_and_is_flagged() -> Result<(), String> {
        let document = failure("the widget is missing");
        let parsed: serde_json::Value =
            serde_json::from_str(document.as_str()).map_err(|error| error.to_string())?;
        assert_eq!(parsed.get("isError"), Some(&serde_json::Value::Bool(true)));
        if text_blocks(&document)? != ["the widget is missing".to_string()] {
            return Err("the failure lost its message".to_string());
        }
        Ok(())
    }

    /// A `Serialize` that cannot produce a value is the handler's bug, and the
    /// error says so rather than blaming the peer.
    #[test]
    fn an_unserialisable_answer_is_reported_as_the_handlers_fault() -> Result<(), String> {
        match success(&NotSerializable) {
            Err(OutcomeError::Answer(_)) => Ok(()),
            Err(other) => Err(format!("wrong error for a bad answer: {other}")),
            Ok(document) => Err(format!("a bad answer produced {document}")),
        }
    }

    /// A type whose `Serialize` fails, which no handler should return.
    struct NotSerializable;

    impl Serialize for NotSerializable {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            Err(serde::ser::Error::custom("this value has no JSON form"))
        }
    }
}
