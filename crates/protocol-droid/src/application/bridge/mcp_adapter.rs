//! The MCP half of a cross-protocol bridge.
//!
//! # What this adapter knows
//!
//! MCP's call is `tools/call` with `name` and `arguments`; MCP's reply is
//! a `CallToolResult` with `content`, an optional `structuredContent` and
//! an optional `isError`. That is the whole of it. The adapter does not
//! know what A2A is, and it does not need to: the composition in
//! [`super::composed`] puts this adapter on one side and the other
//! protocol's adapter on the other, and the IR is what they share.
//!
//! # The revision question
//!
//! MCP's *shape* for a call and a result is the same at every revision —
//! `tools/call` is `tools/call` at `2024-11-05` and at `2026-07-28`. What
//! changes between revisions is which content blocks a result may carry,
//! and that is the per-revision `ToolResultCodec`'s job on the downstream
//! leg. This adapter is revision-agnostic on purpose.

use serde_json::{Map, Value};

use crate::domain::ir::{
    AudioContent, Content, EmbeddedResource, Extensions, ImageContent, IrCall, IrMethod, IrOutcome,
    JsonObject, ResourceLinkContent, TextContent, UnknownContent,
};

use super::cross_protocol::{
    CrossProtocolError, ProtocolSide, RequestCodecReport, RequestCodecStep,
};
use super::protocol_adapter::{ProtocolAdapter, WireRequest};

/// The MCP method this adapter bridges.
const TOOLS_CALL: &str = "tools/call";

/// The params key MCP reserves for implementation-specific data.
const META: &str = "_meta";

/// The MCP half of a bridge.
///
/// Cheap to construct and to share: it holds no state, so the `Send +
/// Sync` bound the trait requires is satisfied by the type itself.
#[derive(Debug, Default, Clone, Copy)]
pub struct McpAdapter;

impl McpAdapter {
    /// The adapter.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl ProtocolAdapter for McpAdapter {
    /// MCP at any revision the bridge negotiated.
    fn side(&self) -> ProtocolSide {
        ProtocolSide::Mcp
    }

    /// `tools/call` is the one MCP method the bridge translates.
    fn ir_method(&self, wire_method: &str) -> Option<IrMethod> {
        (wire_method == TOOLS_CALL).then_some(IrMethod::Call)
    }

    /// The wire method for an IR call.
    fn wire_method(&self, ir_method: &IrMethod) -> Option<&'static str> {
        match ir_method {
            IrMethod::Call => Some(TOOLS_CALL),
        }
    }

    /// The shape of an MCP call, for the translation report.
    fn request_shape(&self) -> &'static str {
        "MCP tools/call{name, arguments}"
    }

    /// The shape of an MCP reply, for the translation report.
    fn reply_shape(&self) -> &'static str {
        "MCP CallToolResult{content, isError}"
    }

    /// Lifts `tools/call` parameters into the IR.
    ///
    /// The `name` becomes the call's target and the `arguments` object
    /// becomes its arguments. A `_meta` object rides along in the
    /// extensions bag, because MCP reserves that key for exactly this:
    /// data the protocol does not model but the peer may still want.
    ///
    /// # Errors
    ///
    /// [`CrossProtocolError::MethodNotBridged`] for any method but
    /// `tools/call`, and [`CrossProtocolError::MalformedPayload`] when the
    /// parameters are not an object or the `arguments` value is not one.
    fn lift_request(
        &self,
        wire_method: &str,
        params: &Value,
        _report: &mut RequestCodecReport,
    ) -> Result<IrCall, CrossProtocolError> {
        if wire_method != TOOLS_CALL {
            return Err(CrossProtocolError::MethodNotBridged(
                wire_method.to_owned(),
                ProtocolSide::Mcp,
            ));
        }
        let Some(object) = params.as_object() else {
            return Err(CrossProtocolError::MalformedPayload {
                side: ProtocolSide::Mcp,
                detail: "params is not an object".to_owned(),
            });
        };
        let target = object
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let arguments = match object.get("arguments") {
            None | Some(Value::Null) => None,
            Some(value) => Some(JsonObject::from_value(value.clone()).map_err(|error| {
                CrossProtocolError::MalformedPayload {
                    side: ProtocolSide::Mcp,
                    detail: format!("arguments is not a JSON object: {error}"),
                }
            })?),
        };
        let extensions = extensions_from_meta(object.get(META));
        Ok(IrCall::new(IrMethod::Call, target, arguments).with_extensions(extensions))
    }

    /// Lowers an IR call into `tools/call` parameters.
    ///
    /// # Errors
    ///
    /// Never in practice: every field of an [`IrCall`] has an MCP form.
    /// The error variant is here because the trait promises one, and a
    /// future MCP revision could break the promise.
    fn lower_request(
        &self,
        call: &IrCall,
        _report: &mut RequestCodecReport,
    ) -> Result<WireRequest, CrossProtocolError> {
        let mut params = Map::new();
        if !call.target.is_empty() {
            params.insert("name".to_owned(), Value::String(call.target.clone()));
        }
        if let Some(arguments) = call.arguments.as_ref() {
            params.insert(
                "arguments".to_owned(),
                value_of(arguments).map_err(|detail| CrossProtocolError::MalformedPayload {
                    side: ProtocolSide::Mcp,
                    detail,
                })?,
            );
        }
        let meta = call.extensions.to_json_object();
        if !meta.is_empty() {
            params.insert(META.to_owned(), Value::Object(meta));
        }
        Ok(WireRequest {
            method: TOOLS_CALL.to_owned(),
            params: Value::Object(params),
        })
    }

    /// Lifts a `CallToolResult` into the IR.
    ///
    /// # Errors
    ///
    /// [`CrossProtocolError::MalformedPayload`] when the result is not an
    /// object.
    fn lift_reply(
        &self,
        result: &Value,
        report: &mut RequestCodecReport,
    ) -> Result<IrOutcome, CrossProtocolError> {
        let Some(object) = result.as_object() else {
            return Err(CrossProtocolError::MalformedPayload {
                side: ProtocolSide::Mcp,
                detail: "result is not an object".to_owned(),
            });
        };
        let mut content = Vec::new();
        if let Some(blocks) = object.get("content").and_then(Value::as_array) {
            for block in blocks {
                if let Some(block) = value_to_content(block) {
                    content.push(block);
                } else {
                    report.record(RequestCodecStep::Dropped {
                        what: "a content block that is not an object",
                        count: 1,
                    });
                }
            }
        }
        let structured = match object.get("structuredContent") {
            None | Some(Value::Null) => None,
            Some(value) => Some(JsonObject::from_value(value.clone()).map_err(|error| {
                CrossProtocolError::MalformedPayload {
                    side: ProtocolSide::Mcp,
                    detail: format!("structuredContent is not a JSON object: {error}"),
                }
            })?),
        };
        let is_error = object
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let extensions = extensions_from_meta(object.get(META));
        Ok(IrOutcome {
            content,
            structured,
            is_error,
            extensions,
        })
    }

    /// Lowers an IR outcome into a `CallToolResult`.
    ///
    /// # Errors
    ///
    /// Never in practice: every field of an [`IrOutcome`] has an MCP form.
    fn lower_reply(
        &self,
        outcome: &IrOutcome,
        _report: &mut RequestCodecReport,
    ) -> Result<Value, CrossProtocolError> {
        let mut result = Map::new();
        result.insert(
            "content".to_owned(),
            Value::Array(outcome.content.iter().map(content_to_value).collect()),
        );
        if let Some(structured) = outcome.structured.as_ref() {
            result.insert(
                "structuredContent".to_owned(),
                value_of(structured).map_err(|detail| CrossProtocolError::MalformedPayload {
                    side: ProtocolSide::Mcp,
                    detail,
                })?,
            );
        }
        if outcome.is_error {
            result.insert("isError".to_owned(), Value::Bool(true));
        }
        let meta = outcome.extensions.to_json_object();
        if !meta.is_empty() {
            result.insert(META.to_owned(), Value::Object(meta));
        }
        Ok(Value::Object(result))
    }
}

/// The `_meta` bag as extensions, or an empty bag.
fn extensions_from_meta(meta: Option<&Value>) -> Extensions {
    match meta {
        Some(Value::Object(object)) => Extensions::from_json_object(object.clone()),
        _ => Extensions::empty(),
    }
}

/// A [`JsonObject`] back as a [`Value`].
///
/// The IR stores the validated text rather than a parsed tree, so putting
/// one back on the wire costs a parse. The bridge is not the hot path —
/// the JSON-RPC envelope router is — and this keeps the IR free of the
/// serializer.
fn value_of(object: &JsonObject) -> Result<Value, String> {
    serde_json::from_str(object.as_str()).map_err(|error| error.to_string())
}

/// An MCP content block as a [`Value`].
fn content_to_value(content: &Content) -> Value {
    match content {
        Content::Text(text) => {
            let mut map = Map::new();
            map.insert("type".to_owned(), Value::String("text".to_owned()));
            map.insert("text".to_owned(), Value::String(text.text.clone()));
            Value::Object(map)
        }
        Content::Image(image) => {
            let mut map = Map::new();
            map.insert("type".to_owned(), Value::String("image".to_owned()));
            map.insert("data".to_owned(), Value::String(image.data.clone()));
            map.insert(
                "mimeType".to_owned(),
                Value::String(image.mime_type.clone()),
            );
            Value::Object(map)
        }
        Content::Audio(audio) => {
            let mut map = Map::new();
            map.insert("type".to_owned(), Value::String("audio".to_owned()));
            map.insert("data".to_owned(), Value::String(audio.data.clone()));
            map.insert(
                "mimeType".to_owned(),
                Value::String(audio.mime_type.clone()),
            );
            Value::Object(map)
        }
        Content::ResourceLink(link) => {
            let mut map = Map::new();
            map.insert("type".to_owned(), Value::String("resource_link".to_owned()));
            map.insert("uri".to_owned(), Value::String(link.uri.clone()));
            map.insert("name".to_owned(), Value::String(link.name.clone()));
            if let Some(description) = link.description.as_ref() {
                map.insert("description".to_owned(), Value::String(description.clone()));
            }
            if let Some(mime_type) = link.mime_type.as_ref() {
                map.insert("mimeType".to_owned(), Value::String(mime_type.clone()));
            }
            Value::Object(map)
        }
        Content::EmbeddedResource(resource) => {
            let mut inner = Map::new();
            inner.insert("uri".to_owned(), Value::String(resource.uri.clone()));
            if let Some(text) = resource.text.as_ref() {
                inner.insert("text".to_owned(), Value::String(text.clone()));
            }
            if let Some(blob) = resource.blob.as_ref() {
                inner.insert("blob".to_owned(), Value::String(blob.clone()));
            }
            if let Some(mime_type) = resource.mime_type.as_ref() {
                inner.insert("mimeType".to_owned(), Value::String(mime_type.clone()));
            }
            let mut map = Map::new();
            map.insert("type".to_owned(), Value::String("resource".to_owned()));
            map.insert("resource".to_owned(), Value::Object(inner));
            Value::Object(map)
        }
        Content::Unknown(unknown) => match serde_json::from_str(unknown.raw.as_str()) {
            Ok(Value::Object(map)) => Value::Object(map),
            Ok(other) => other,
            Err(_) => Value::Null,
        },
    }
}

/// An MCP content block as a [`Content`].
///
/// A block whose `type` this build does not know becomes
/// [`Content::Unknown`], which keeps the block whole so it can be
/// forwarded unchanged. A block that is not an object at all has no
/// forwardable form, and the caller records the loss.
fn value_to_content(value: &Value) -> Option<Content> {
    let object = value.as_object()?;
    let type_name = object.get("type").and_then(Value::as_str).unwrap_or("text");
    match type_name {
        "text" => Some(Content::Text(TextContent::new(
            object
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ))),
        "image" => Some(Content::Image(ImageContent::new(
            object
                .get("data")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            object
                .get("mimeType")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ))),
        "audio" => Some(Content::Audio(AudioContent::new(
            object
                .get("data")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            object
                .get("mimeType")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ))),
        "resource_link" => Some(Content::ResourceLink(ResourceLinkContent::new(
            object
                .get("uri")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            object
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ))),
        "resource" => {
            let inner = object.get("resource").and_then(Value::as_object)?;
            Some(Content::EmbeddedResource(EmbeddedResource {
                text: inner
                    .get("text")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                blob: inner
                    .get("blob")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                uri: inner
                    .get("uri")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                mime_type: inner
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                annotations: None,
                extensions: Extensions::empty(),
            }))
        }
        other => Some(Content::Unknown(UnknownContent {
            type_name: other.to_owned(),
            raw: JsonObject::from_value(value.clone()).ok()?,
            extensions: Extensions::empty(),
        })),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn report() -> RequestCodecReport {
        RequestCodecReport::empty()
    }

    #[test]
    fn a_tools_call_lifts_into_a_call_with_its_target_and_arguments()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut rep = report();
        let adapter = McpAdapter::new();
        let call = adapter.lift_request(
            "tools/call",
            &json!({"name": "lookup", "arguments": {"query": "weather"}}),
            &mut rep,
        )?;
        assert_eq!(call.method, IrMethod::Call);
        assert_eq!(call.target, "lookup");
        let arguments = call.arguments.ok_or("the call carried no arguments")?;
        let value = value_of(&arguments)?;
        assert_eq!(value.get("query").and_then(Value::as_str), Some("weather"));
        Ok(())
    }

    #[test]
    fn a_call_with_no_arguments_is_not_a_call_with_an_empty_object()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut rep = report();
        let adapter = McpAdapter::new();
        let call = adapter.lift_request("tools/call", &json!({"name": "ping"}), &mut rep)?;
        assert!(call.arguments.is_none());
        Ok(())
    }

    #[test]
    fn arguments_that_are_not_an_object_are_refused() {
        let mut rep = report();
        let adapter = McpAdapter::new();
        let outcome = adapter.lift_request(
            "tools/call",
            &json!({"name": "x", "arguments": 7}),
            &mut rep,
        );
        assert!(matches!(
            outcome,
            Err(CrossProtocolError::MalformedPayload {
                side: ProtocolSide::Mcp,
                ..
            })
        ));
    }

    #[test]
    fn a_method_that_is_not_a_call_is_not_lifted() {
        let mut rep = report();
        let adapter = McpAdapter::new();
        let outcome = adapter.lift_request("tools/list", &json!({}), &mut rep);
        assert!(matches!(
            outcome,
            Err(CrossProtocolError::MethodNotBridged(_, ProtocolSide::Mcp))
        ));
    }

    #[test]
    fn a_call_lowers_back_to_the_params_it_came_from() -> Result<(), Box<dyn std::error::Error>> {
        let mut rep = report();
        let adapter = McpAdapter::new();
        let params = json!({"name": "lookup", "arguments": {"query": "weather"}});
        let call = adapter.lift_request("tools/call", &params, &mut rep)?;
        let wire = adapter.lower_request(&call, &mut rep)?;
        assert_eq!(wire.method, "tools/call");
        assert_eq!(wire.params, params);
        Ok(())
    }

    #[test]
    fn a_call_with_no_arguments_lowers_without_the_key() -> Result<(), Box<dyn std::error::Error>> {
        let mut rep = report();
        let adapter = McpAdapter::new();
        let call = adapter.lift_request("tools/call", &json!({"name": "ping"}), &mut rep)?;
        let wire = adapter.lower_request(&call, &mut rep)?;
        let object = wire.params.as_object().ok_or("params was not an object")?;
        assert!(!object.contains_key("arguments"));
        Ok(())
    }

    #[test]
    fn a_tool_result_lifts_into_an_outcome_with_its_content_and_flag()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut rep = report();
        let adapter = McpAdapter::new();
        let outcome = adapter.lift_reply(
            &json!({
                "content": [{"type": "text", "text": "hello"}],
                "isError": true,
                "structuredContent": {"rows": 2}
            }),
            &mut rep,
        )?;
        assert!(outcome.is_error);
        assert!(outcome.structured.is_some());
        let first = outcome.content.first().ok_or("no content")?;
        assert!(matches!(first, Content::Text(text) if text.text == "hello"));
        Ok(())
    }

    #[test]
    fn a_tool_result_round_trips() -> Result<(), Box<dyn std::error::Error>> {
        let mut rep = report();
        let adapter = McpAdapter::new();
        let result = json!({
            "content": [{"type": "text", "text": "hello"}],
            "isError": false
        });
        let outcome = adapter.lift_reply(&result, &mut rep)?;
        let wire = adapter.lower_reply(&outcome, &mut rep)?;
        // `isError` is optional and defaults to false, so the lowered
        // result leaves it out instead of writing it out and back. The
        // content — the part a client reads — is unchanged.
        assert_eq!(
            wire,
            json!({"content": [{"type": "text", "text": "hello"}]})
        );
        Ok(())
    }

    #[test]
    fn a_content_block_this_build_does_not_know_is_kept_whole()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut rep = report();
        let adapter = McpAdapter::new();
        let outcome = adapter.lift_reply(
            &json!({"content": [{"type": "hologram", "frames": 3}]}),
            &mut rep,
        )?;
        let first = outcome.content.first().ok_or("no content")?;
        assert!(matches!(
            first,
            Content::Unknown(unknown) if unknown.type_name == "hologram"
        ));
        let wire = adapter.lower_reply(&outcome, &mut rep)?;
        let blocks = wire
            .get("content")
            .and_then(Value::as_array)
            .ok_or("no content")?;
        let block = blocks.first().ok_or("no content")?;
        assert_eq!(block.get("frames").and_then(Value::as_u64), Some(3));
        Ok(())
    }
}
