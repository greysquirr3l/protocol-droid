//! The JSON `resources/read` and `prompts/get` answer with.
//!
//! # Why these documents live here and not in the IR
//!
//! `GetPromptResult` and `ResourceContents` are pure data with no
//! serialisation of their own, exactly like `ToolResult`. Assembly is an
//! application concern because it depends on which revision the session
//! negotiated — a fact the domain layer is not allowed to know — and a
//! `to_json` on the IR type would have to take the revision as an argument
//! anyway, which puts it back where it started.
//!
//! # Why a prompt's content goes through the tool codec
//!
//! A `prompts/get` message carries a `ContentBlock`, and that union is spelled
//! differently per revision — `resource_link` arrives in `2025-06-18`, audio
//! in `2025-03-26`. Rather than answer "how is a content block spelled at this
//! revision" a second time, this wraps the block in the one-block `ToolResult`
//! the tool path already lowers and reads the block back out of the answer.
//!
//! That looks roundabout and is not. The alternative is a second copy of the
//! per-revision codecs answering the same question, and the two would agree
//! until the next revision added a field to one of them and not the other.

use crate::adapters::secondary::codec::result::verbatim;
use crate::application::server::result::OutcomeError;
use crate::domain::ir::{
    Content, GetPromptResult, JsonObject, ResourceBody, ResourceContents, ToolResult,
};
use crate::domain::ProtocolVersion;

/// The `ReadResourceResult` for `contents`.
///
/// # Errors
///
/// [`OutcomeError::NotAnObject`] when an assembled entry is not a JSON object.
pub fn read_contents(contents: &[ResourceContents]) -> Result<JsonObject, OutcomeError> {
    let mut entries = Vec::with_capacity(contents.len());
    for entry in contents {
        let mut object = serde_json::Map::new();
        object.insert("uri".to_owned(), entry.uri.clone().into());
        match &entry.body {
            ResourceBody::Text(text) => {
                object.insert("text".to_owned(), text.clone().into());
            }
            ResourceBody::Blob(data) => {
                object.insert("blob".to_owned(), data.clone().into());
            }
        }
        if let Some(mime_type) = &entry.mime_type {
            object.insert("mimeType".to_owned(), mime_type.clone().into());
        }
        entries.push(serde_json::Value::Object(object));
    }

    JsonObject::from_value(serde_json::json!({ "contents": entries }))
        .map_err(OutcomeError::NotAnObject)
}

/// The `GetPromptResult` for `result`, with content blocks spelled for `version`.
///
/// # Errors
///
/// [`OutcomeError::NotLowerable`] for a block the negotiated revision cannot
/// express, and [`OutcomeError::NotAnObject`] when the assembled document is
/// not an object.
pub fn prompt_messages(
    result: &GetPromptResult,
    version: ProtocolVersion,
) -> Result<JsonObject, OutcomeError> {
    let mut messages = Vec::with_capacity(result.messages.len());
    for message in &result.messages {
        let block = one_block(&message.content, version)?;
        // `block` is a validated JSON object held as a string, and `json!`
        // will not take it without a serde impl it does not need. Re-parsing
        // here is one parse per prompt message on a path that is not hot.
        let block: serde_json::Value = serde_json::from_str(block.as_str())
            .map_err(|error| OutcomeError::Answer(error.to_string()))?;
        messages.push(serde_json::json!({
            "role": message.role.as_str(),
            "content": block,
        }));
    }

    let mut document = serde_json::json!({ "messages": messages });
    if let (serde_json::Value::Object(map), Some(description)) =
        (&mut document, result.description.as_deref())
    {
        map.insert("description".to_string(), description.into());
    }
    JsonObject::from_value(document).map_err(OutcomeError::NotAnObject)
}

/// One content block, lowered for `version` and unwrapped from its result.
///
/// The wrapping is invisible on the wire and cannot fail for a reason the
/// caller could act on differently, so it does not get an error variant of its
/// own.
fn one_block(content: &Content, version: ProtocolVersion) -> Result<JsonObject, OutcomeError> {
    let document = verbatim(version, &ToolResult::new(vec![content.clone()]))?;
    let parsed: serde_json::Value = serde_json::from_str(document.as_str())
        .map_err(|error| OutcomeError::Answer(error.to_string()))?;
    let block = parsed
        .get("content")
        .and_then(serde_json::Value::as_array)
        .and_then(|blocks| blocks.first())
        .cloned()
        .ok_or_else(|| OutcomeError::Answer("the lowered result carried no block".to_owned()))?;
    JsonObject::from_value(block).map_err(OutcomeError::NotAnObject)
}
