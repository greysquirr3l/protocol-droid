//! The A2A v1.0 ↔ MCP `SendMessage` ↔ `tools/call` codec.
//!
//! # What this codec handles
//!
//! One method pair, in both directions:
//!
//! - **A2A → MCP**: an MCP client calling `tools/call` for a tool that
//!   maps to an A2A skill becomes an A2A `SendMessage` whose
//!   `message.parts` carry the tool's name and arguments in a metadata
//!   field, and the A2A agent uses the metadata to dispatch to the
//!   right skill.
//! - **MCP → A2A**: an A2A client calling `SendMessage` (a skill on the
//!   advertised agent card) becomes an MCP `tools/call` whose `name`
//!   is the skill and whose `arguments` are the message parts.
//!
//! The reply path uses the same A2A `Task` → MCP `ToolResult` mapping
//! rules in [`crate::domain::bridge::a2a_mcp`] for both directions.
//!
//! # What this codec does *not* handle
//!
//! The other ten A2A v1.0 methods (`GetTask`, `ListTasks`, `CancelTask`,
//! `SubscribeToTask`, the four `*PushNotificationConfig` methods, and
//! `GetExtendedAgentCard`) are not modelled. A bridge that receives
//! one of them refuses with `CrossProtocolError::MethodNotBridged` and
//! the loop returns `-32601` to the client. The codec that adds them
//! is a follow-up commit; this one ships the shape of the integration
//! and the path for the rest to follow.
//!
//! # Why a metadata field carries the skill name on the A2A side
//!
//! A2A `SendMessage` has no `skill` parameter — the agent dispatches
//! the message to whichever skill it thinks fits. A bridge that wants
//! an MCP `tools/call` to land on a specific skill has to either inject
//! the skill name into the message and hope the A2A agent reads it,
//! or run a separate A2A server per skill and route by URL.
//!
//! The metadata-field approach is the cheaper of the two and matches
//! the A2A spec's own use of `metadata` for "per-request" hints. An
//! A2A agent that does not read it will still try to dispatch, and
//! will get a different answer than the bridge meant — but the report
//! will still show the bridge took a guess. The convention is
//! documented in the doc comment on [`bridge_skill_metadata_key`].

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Map, Value};

use crate::application::bridge::cross_protocol::{
    CrossProtocolCodec, CrossProtocolError, ProtocolSide, RequestCodecReport, RequestCodecStep,
};
use crate::domain::bridge::a2a_mcp::a2a_message_to_mcp_content;

/// The metadata key the A2A side of the bridge uses to name a skill.
///
/// An A2A agent that wants to be bridge-friendly reads this from the
/// `metadata` bag and dispatches to the named skill. An A2A agent that
/// does not will see the same message it would have seen without the
/// bridge.
const BRIDGE_SKILL_KEY: &str = "bridge_skill";

/// The codec that turns `SendMessage` ↔ `tools/call` into the bridge's
/// canonical envelopes.
///
/// Cheap to construct (no allocations beyond the struct itself), safe
/// to share behind an `Arc` — the trait's `Send + Sync` bound is
/// satisfied without any interior mutability, because the codec is a
/// pure function of its inputs.
#[derive(Debug, Default, Clone, Copy)]
pub struct A2aMcpCodec;

impl A2aMcpCodec {
    /// A codec the bridge holds by value.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl CrossProtocolCodec for A2aMcpCodec {
    fn downstream_protocol(&self) -> ProtocolSide {
        // The codec is symmetric — it is the *bridge* that picks which
        // direction to drive. A `SendMessage` from the downstream is
        // either the A2A client calling an A2A method, or the MCP
        // client calling `tools/call`. The codec's job is to handle
        // both, and the bridge calls the right method based on the
        // downstream leg's protocol.
        ProtocolSide::Mcp
    }

    fn encode_request(
        &self,
        downstream_side: ProtocolSide,
        ir_method: &str,
        raw_envelope: &Map<String, Value>,
        report: &mut RequestCodecReport,
    ) -> Result<Map<String, Value>, CrossProtocolError> {
        let Some(upstream_method) = downstream_side.wire_method(ir_method) else {
            return Err(CrossProtocolError::MethodNotBridged(
                ir_method.to_owned(),
                downstream_side,
            ));
        };

        let mut outbound = raw_envelope.clone();
        // The envelope is what the bridge forwards, and the bridge's
        // own `lower_envelope` is the right level for the JSON-RPC
        // scaffolding. The codec only changes the method name and the
        // params; it leaves `jsonrpc`, `id`, and the unmodelled
        // extensions alone.
        if let Some(params) = outbound.get_mut("params") {
            *params = translate_params_outbound(downstream_side, ir_method, params, report);
        } else if downstream_side != ir_method_side(ir_method) {
            // A `SendMessage` request from an MCP client has no `params`
            // — MCP `tools/call` is the only call this codec produces
            // for the MCP→A2A direction — so a missing-`params` case
            // is a caller mistake. An A2A `SendMessage` always has
            // `params`, so the same is true on the A2A→MCP side.
            return Err(CrossProtocolError::MethodNotBridged(
                ir_method.to_owned(),
                downstream_side,
            ));
        }

        if let Some(method) = outbound.get_mut("method") {
            *method = Value::String(upstream_method.to_owned());
        }

        report.record(RequestCodecStep::MethodRenamed {
            from: ir_method_static(ir_method),
            to: method_name_static(upstream_method),
        });
        Ok(outbound)
    }

    fn decode_reply(
        &self,
        downstream_side: ProtocolSide,
        ir_method: &str,
        reply: &Map<String, Value>,
        report: &mut RequestCodecReport,
    ) -> Result<Map<String, Value>, CrossProtocolError> {
        if downstream_side.wire_method(ir_method).is_none() {
            return Err(CrossProtocolError::MethodNotBridged(
                ir_method.to_owned(),
                downstream_side,
            ));
        }

        let mut outbound = reply.clone();
        let Some(result) = outbound.get_mut("result") else {
            // An error reply is not a result and needs no translation.
            // The bridge forwards it as-is.
            return Ok(outbound);
        };

        *result = match downstream_side {
            ProtocolSide::A2a => a2a_to_mcp_result(result),
            ProtocolSide::Mcp => mcp_to_a2a_result(result),
        };

        report.record(RequestCodecStep::ReplyTranslated {
            from: reply_label(downstream_side, "upstream"),
            to: reply_label(downstream_side, "downstream"),
        });

        Ok(outbound)
    }
}

fn ir_method_side(ir_method: &str) -> ProtocolSide {
    if ir_method == "SendMessage" {
        ProtocolSide::A2a
    } else if ir_method == "tools/call" {
        ProtocolSide::Mcp
    } else {
        // Unreachable: the codec only knows these two. The caller
        // already rejected other methods with `MethodNotBridged`.
        ProtocolSide::A2a
    }
}

fn ir_method_static(method: &str) -> &'static str {
    // The codec only knows two IR method names, and they are the
    // canonical ones the bridge tracks. Leaking them is the only way
    // to get a `&'static str` for the report step, and the lookup is
    // bounded to the two strings the codec actually emits.
    match method {
        "SendMessage" => "SendMessage",
        "tools/call" => "tools/call",
        // Unreachable: the codec refuses other methods.
        _ => "unknown",
    }
}

fn method_name_static(method: &str) -> &'static str {
    // Same as `ir_method_static`, but for the upstream wire name.
    match method {
        "SendMessage" => "SendMessage",
        "SendStreamingMessage" => "SendStreamingMessage",
        "GetTask" => "GetTask",
        "tools/call" => "tools/call",
        "tools/list" => "tools/list",
        // Other upstream names; safe to fall through to a marker.
        _ => "unknown",
    }
}

fn reply_label(side: ProtocolSide, direction: &str) -> &'static str {
    // The label names the shape in human terms, not the literal JSON
    // type, so an operator reading the report sees the bridge's
    // intent rather than its implementation.
    if direction == "upstream" {
        if side == ProtocolSide::A2a {
            "A2A Task/Message"
        } else {
            "MCP CallToolResult"
        }
    } else if side == ProtocolSide::A2a {
        "MCP CallToolResult"
    } else {
        "A2A Task/Message"
    }
}

fn translate_params_outbound(
    side: ProtocolSide,
    ir_method: &str,
    params: &Value,
    report: &mut RequestCodecReport,
) -> Value {
    match (side, ir_method) {
        (ProtocolSide::A2a, "SendMessage") => a2a_to_mcp_tools_call_params(params, report),
        (ProtocolSide::Mcp, "tools/call") => mcp_to_a2a_send_message_params(params, report),
        _ => params.clone(),
    }
}

fn mcp_to_a2a_send_message_params(params: &Value, report: &mut RequestCodecReport) -> Value {
    // MCP `tools/call` params: `{"name": "<skill>", "arguments": {...}}`.
    let Some(object) = params.as_object() else {
        return params.clone();
    };
    let skill = object.get("name").and_then(Value::as_str).unwrap_or("");
    let arguments = object.get("arguments").cloned().unwrap_or(Value::Null);
    let mut metadata = BTreeMap::new();
    if !skill.is_empty() {
        metadata.insert(BRIDGE_SKILL_KEY.to_owned(), Value::String(skill.to_owned()));
    }
    let mut message = serde_json::Map::new();
    message.insert("role".to_owned(), Value::String("user".to_owned()));
    if arguments.is_null() {
        message.insert("parts".to_owned(), Value::Array(Vec::new()));
    } else {
        let mut data_part = serde_json::Map::new();
        data_part.insert("data".to_owned(), arguments);
        let mut part = serde_json::Map::new();
        part.insert("kind".to_owned(), Value::String("data".to_owned()));
        part.insert("data".to_owned(), Value::Object(data_part));
        message.insert("parts".to_owned(), Value::Array(vec![Value::Object(part)]));
    }
    let mut out = serde_json::Map::new();
    out.insert("message".to_owned(), Value::Object(message));
    if !metadata.is_empty() {
        let meta: Map<String, Value> = metadata.into_iter().collect();
        out.insert("metadata".to_owned(), Value::Object(meta));
    }
    report.record(RequestCodecStep::ParamsTranslated {
        from: "MCP tools/call{name, arguments}",
        to: "A2A SendMessage{message, metadata}",
    });
    Value::Object(out)
}

fn a2a_to_mcp_tools_call_params(params: &Value, report: &mut RequestCodecReport) -> Value {
    // A2A `SendMessage` params: `{"message": {...}, "metadata": {...}}`.
    let Some(object) = params.as_object() else {
        return params.clone();
    };
    let Some(message) = object.get("message") else {
        return params.clone();
    };
    let skill = object
        .get("metadata")
        .and_then(Value::as_object)
        .and_then(|meta| meta.get(BRIDGE_SKILL_KEY))
        .and_then(Value::as_str)
        .unwrap_or("");
    let arguments = first_message_data(message);
    let mut out = serde_json::Map::new();
    if !skill.is_empty() {
        out.insert("name".to_owned(), Value::String(skill.to_owned()));
    }
    out.insert("arguments".to_owned(), arguments);
    report.record(RequestCodecStep::ParamsTranslated {
        from: "A2A SendMessage{message, metadata}",
        to: "MCP tools/call{name, arguments}",
    });
    Value::Object(out)
}

fn first_message_data(message: &Value) -> Value {
    let Some(parts) = message
        .as_object()
        .and_then(|object| object.get("parts"))
        .and_then(Value::as_array)
    else {
        return Value::Null;
    };
    for part in parts {
        if let Some(data) = part.as_object().and_then(|object| object.get("data")) {
            return data.clone();
        }
    }
    Value::Null
}

fn a2a_to_mcp_result(result: &Value) -> Value {
    // A2A reply is either `{"id": "...", "status": ...}` (a Task) or
    // `{"role": "...", "parts": [...]}` (a Message). The MCP side
    // expects a `CallToolResult` with `content`.
    let Some(object) = result.as_object() else {
        return result.clone();
    };
    let mut mapping_report = crate::domain::bridge::a2a_mcp::CrossProtocolReport::new();
    let mut content = if object.contains_key("id") {
        a2a_task_content(object, &mut mapping_report)
    } else {
        let message = clone_message(object);
        a2a_message_to_mcp_content(&message, &mut mapping_report)
    };
    if content.is_empty() {
        // An empty A2A Task with no history becomes a text content
        // block carrying the task state, so the MCP client sees *some*
        // answer rather than a 0-block result it has to special-case.
        if let Some(state) = object
            .get("status")
            .and_then(Value::as_object)
            .and_then(|status| status.get("state"))
            .and_then(Value::as_str)
        {
            content = vec![crate::domain::ir::Content::Text(
                crate::domain::ir::TextContent::new(state),
            )];
        }
    }
    let mut out = serde_json::Map::new();
    out.insert(
        "content".to_owned(),
        Value::Array(content_to_values(&content)),
    );
    let is_error = object
        .get("status")
        .and_then(Value::as_object)
        .and_then(|status| status.get("state"))
        .and_then(Value::as_str)
        .is_some_and(|state| state == "failed" || state == "rejected");
    if is_error {
        out.insert("isError".to_owned(), Value::Bool(true));
    }
    Value::Object(out)
}

fn a2a_task_content(
    object: &Map<String, Value>,
    report: &mut crate::domain::bridge::a2a_mcp::CrossProtocolReport,
) -> Vec<crate::domain::ir::Content> {
    // It's a Task; pick the most recent non-user message from history.
    let history = object
        .get("history")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let last_message: Option<crate::domain::a2a::message::Message> = history
        .iter()
        .rev()
        .find_map(|m| {
            let is_agent = m
                .as_object()
                .and_then(|obj| obj.get("role"))
                .and_then(Value::as_str)
                .is_some_and(|role| role != "user");
            if is_agent {
                serde_json::from_value(m.clone()).ok()
            } else {
                None
            }
        });
    last_message.map_or_else(Vec::new, |message| a2a_message_to_mcp_content(&message, report))
}

fn mcp_to_a2a_result(result: &Value) -> Value {
    // MCP `CallToolResult`: `{"content": [...], "isError": bool?}`.
    let Some(object) = result.as_object() else {
        return result.clone();
    };
    let content = object.get("content").and_then(Value::as_array);
    let parts = content.map_or_else(Vec::new, |items| items.iter().map(value_to_a2a_part_value).collect());
    let mut message = serde_json::Map::new();
    message.insert("role".to_owned(), Value::String("assistant".to_owned()));
    message.insert("parts".to_owned(), Value::Array(parts));
    let mut out = serde_json::Map::new();
    out.insert("message".to_owned(), Value::Object(message));
    if object.get("isError").and_then(Value::as_bool) == Some(true) {
        let mut status = serde_json::Map::new();
        status.insert("state".to_owned(), Value::String("failed".to_owned()));
        out.insert("status".to_owned(), Value::Object(status));
    }
    Value::Object(out)
}

fn value_to_a2a_part_value(value: &Value) -> Value {
    // A wire `Content` block carries `"type": "text" | "image" | ...`
    // and the rest of the block is the type-specific payload. A
    // bridge that does not recognise the type falls through to a
    // `DataPart` with the whole block, which is lossless and lets the
    // A2A agent read whatever it can.
    let Some(object) = value.as_object() else {
        return value.clone();
    };
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("data");
    match kind {
        "text" => {
            let text = object
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let mut part = serde_json::Map::new();
            part.insert("kind".to_owned(), Value::String("text".to_owned()));
            part.insert("text".to_owned(), Value::String(text));
            Value::Object(part)
        }
        "image" | "audio" | "resource_link" | "resource" => {
            let mut part = serde_json::Map::new();
            part.insert("kind".to_owned(), Value::String("file".to_owned()));
            part.insert("file".to_owned(), value.clone());
            Value::Object(part)
        }
        _ => {
            let mut part = serde_json::Map::new();
            part.insert("kind".to_owned(), Value::String("data".to_owned()));
            part.insert("data".to_owned(), value.clone());
            Value::Object(part)
        }
    }
}

fn clone_message(object: &Map<String, Value>) -> crate::domain::a2a::message::Message {
    // The mapping rule expects a typed `Message`; the A2A schema's
    // wire form is exactly that, so a typed parse is the simplest
    // path. A bridge that does not trust the wire form would lift
    // first; the codec is the right place to do it. A parse that
    // fails is a malformed reply from the upstream, which the bridge
    // already treats as an error: an empty default keeps the bridge
    // responsive rather than panic-prone.
    let value = Value::Object(object.clone());
    serde_json::from_value(value).unwrap_or_default()
}

fn content_to_values(content: &[crate::domain::ir::Content]) -> Vec<Value> {
    content.iter().map(content_to_value).collect()
}

fn content_to_value(content: &crate::domain::ir::Content) -> Value {
    use crate::domain::ir::Content;
    match content {
        Content::Text(text) => {
            let mut map = serde_json::Map::new();
            map.insert("type".to_owned(), Value::String("text".to_owned()));
            map.insert("text".to_owned(), Value::String(text.text.clone()));
            Value::Object(map)
        }
        Content::Image(image) => {
            let mut map = serde_json::Map::new();
            map.insert("type".to_owned(), Value::String("image".to_owned()));
            map.insert("data".to_owned(), Value::String(image.data.clone()));
            map.insert(
                "mimeType".to_owned(),
                Value::String(image.mime_type.clone()),
            );
            Value::Object(map)
        }
        Content::Audio(audio) => {
            let mut map = serde_json::Map::new();
            map.insert("type".to_owned(), Value::String("audio".to_owned()));
            map.insert("data".to_owned(), Value::String(audio.data.clone()));
            map.insert(
                "mimeType".to_owned(),
                Value::String(audio.mime_type.clone()),
            );
            Value::Object(map)
        }
        Content::ResourceLink(link) => {
            let mut map = serde_json::Map::new();
            map.insert(
                "type".to_owned(),
                Value::String("resource_link".to_owned()),
            );
            map.insert("uri".to_owned(), Value::String(link.uri.clone()));
            map.insert("name".to_owned(), Value::String(link.name.clone()));
            if let Some(description) = link.description.as_ref() {
                map.insert(
                    "description".to_owned(),
                    Value::String(description.clone()),
                );
            }
            if let Some(mime) = link.mime_type.as_ref() {
                map.insert("mimeType".to_owned(), Value::String(mime.clone()));
            }
            Value::Object(map)
        }
        Content::EmbeddedResource(resource) => {
            let mut map = serde_json::Map::new();
            map.insert(
                "type".to_owned(),
                Value::String("resource".to_owned()),
            );
            let mut resource_map = serde_json::Map::new();
            resource_map.insert("uri".to_owned(), Value::String(resource.uri.clone()));
            if let Some(text) = resource.text.as_ref() {
                resource_map.insert("text".to_owned(), Value::String(text.clone()));
            }
            if let Some(blob) = resource.blob.as_ref() {
                resource_map.insert("blob".to_owned(), Value::String(blob.clone()));
            }
            if let Some(mime) = resource.mime_type.as_ref() {
                resource_map.insert("mimeType".to_owned(), Value::String(mime.clone()));
            }
            map.insert("resource".to_owned(), Value::Object(resource_map));
            Value::Object(map)
        }
        Content::Unknown(unknown) => match serde_json::from_str(&unknown.raw.to_string()) {
            Ok(Value::Object(map)) => Value::Object(map),
            Ok(other) => other,
            Err(_) => Value::Null,
        },
    }
}

impl CrossProtocolCodec for Arc<A2aMcpCodec> {
    fn downstream_protocol(&self) -> ProtocolSide {
        (**self).downstream_protocol()
    }

    fn encode_request(
        &self,
        downstream_side: ProtocolSide,
        ir_method: &str,
        raw_envelope: &Map<String, Value>,
        report: &mut RequestCodecReport,
    ) -> Result<Map<String, Value>, CrossProtocolError> {
        (**self).encode_request(downstream_side, ir_method, raw_envelope, report)
    }

    fn decode_reply(
        &self,
        downstream_side: ProtocolSide,
        ir_method: &str,
        reply: &Map<String, Value>,
        report: &mut RequestCodecReport,
    ) -> Result<Map<String, Value>, CrossProtocolError> {
        (**self).decode_reply(downstream_side, ir_method, reply, report)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;

    fn report() -> RequestCodecReport {
        RequestCodecReport::empty()
    }

    fn object_of(input: &Value) -> Map<String, Value> {
        input.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn a2a_send_message_becomes_mcp_tools_call() -> Result<(), Box<dyn std::error::Error>> {
        let codec = A2aMcpCodec::new();
        let mut rep = report();
        let raw = object_of(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "SendMessage",
            "params": {
                "message": {
                    "role": "user",
                    "parts": [
                        { "kind": "data", "data": { "query": "weather" } }
                    ]
                },
                "metadata": { "bridge_skill": "lookup" }
            }
        }));
        let out = codec.encode_request(ProtocolSide::A2a, "SendMessage", &raw, &mut rep)?;
        assert_eq!(out.get("method").and_then(Value::as_str), Some("tools/call"));
        let params = out
            .get("params")
            .and_then(Value::as_object)
            .ok_or("missing params")?;
        assert_eq!(
            params.get("name").and_then(Value::as_str),
            Some("lookup")
        );
        assert_eq!(
            params.get("arguments"),
            Some(&json!({ "query": "weather" }))
        );
        Ok(())
    }

    #[test]
    fn mcp_tools_call_becomes_a2a_send_message() -> Result<(), Box<dyn std::error::Error>> {
        let codec = A2aMcpCodec::new();
        let mut rep = report();
        let raw = object_of(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "lookup",
                "arguments": { "query": "weather" }
            }
        }));
        let out = codec.encode_request(ProtocolSide::Mcp, "tools/call", &raw, &mut rep)?;
        assert_eq!(out.get("method").and_then(Value::as_str), Some("SendMessage"));
        let params = out
            .get("params")
            .and_then(Value::as_object)
            .ok_or("missing params")?;
        let metadata = params
            .get("metadata")
            .and_then(Value::as_object)
            .ok_or("missing metadata")?;
        assert_eq!(
            metadata.get("bridge_skill").and_then(Value::as_str),
            Some("lookup")
        );
        Ok(())
    }

    #[test]
    fn an_unbridged_method_is_refused() {
        let codec = A2aMcpCodec::new();
        let mut rep = report();
        let raw = object_of(&json!({ "jsonrpc": "2.0", "id": 1, "method": "GetTask" }));
        let outcome = codec.encode_request(ProtocolSide::A2a, "GetTask", &raw, &mut rep);
        assert!(matches!(
            outcome,
            Err(CrossProtocolError::MethodNotBridged(_, ProtocolSide::A2a))
        ));
    }

    #[test]
    fn a_task_reply_becomes_a_mcp_tool_result() -> Result<(), Box<dyn std::error::Error>> {
        let codec = A2aMcpCodec::new();
        let mut rep = report();
        let reply = object_of(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "id": "task-1",
                "status": { "state": "completed" },
                "history": [
                    { "role": "user", "parts": [{ "kind": "text", "text": "hi" }] },
                    { "role": "agent", "parts": [{ "kind": "text", "text": "hello" }] }
                ]
            }
        }));
        let out = codec.decode_reply(ProtocolSide::A2a, "SendMessage", &reply, &mut rep)?;
        let result = out
            .get("result")
            .and_then(Value::as_object)
            .ok_or("missing result")?;
        let content = result
            .get("content")
            .and_then(Value::as_array)
            .ok_or("missing content")?;
        assert!(!content.is_empty());
        Ok(())
    }

    #[test]
    fn an_mcp_tool_result_becomes_an_a2a_message() -> Result<(), Box<dyn std::error::Error>> {
        let codec = A2aMcpCodec::new();
        let mut rep = report();
        let reply = object_of(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "content": [{ "type": "text", "text": "hello" }],
                "isError": false
            }
        }));
        let out = codec.decode_reply(ProtocolSide::Mcp, "tools/call", &reply, &mut rep)?;
        let result = out
            .get("result")
            .and_then(Value::as_object)
            .ok_or("missing result")?;
        let message = result.get("message").ok_or("missing message")?;
        assert!(message.is_object());
        Ok(())
    }

    #[test]
    fn the_codec_is_send_and_sync_via_arc() {
        fn assert_send_sync<T: Send + Sync>(_: &T) {}
        let codec: Arc<A2aMcpCodec> = Arc::new(A2aMcpCodec::new());
        assert_send_sync(&codec);
    }
}
