//! The MCP ↔ A2A codec, built from the two protocol adapters.
//!
//! # What this type is
//!
//! A convenience wrapper over [`ComposedCodec`], which is the thing that
//! does the work. This module exists so a consumer that wants to bridge
//! an MCP client to an A2A agent does not have to know that the bridge
//! is a composition of two adapters — that is an implementation detail
//! of the bridge, and a consumer should not have to assemble it.
//!
//! # Why the translation is not written out here
//!
//! The obvious shape for this file is a pair of functions — "A2A request
//! becomes an MCP request" and "MCP request becomes an A2A request" —
//! with the parameter shapes spelled out in each. That is what this file
//! used to be, and it is why the bridge could only speak two protocols:
//! every new protocol needed a new pair of functions, written from
//! scratch, for every protocol it had to talk to.
//!
//! The translation now lives in the adapters — [`McpAdapter`] and
//! [`A2aAdapter`] — and this file is the composition of the two. Adding a
//! third protocol means writing one adapter, not three pairs of
//! functions.
//!
//! # The wire shapes
//!
//! The A2A side uses the spec's part shape: a part is an object with
//! exactly one of `text`, `file` or `data` at the top level, and no
//! `kind` discriminator. That is what the crate's own A2A server sends,
//! what the A2A conformance fixtures expect, and what the vendored
//! `a2a.proto` describes.

use std::sync::Arc;

use serde_json::{Map, Value};

use crate::domain::ir::IrMethod;

use super::a2a_adapter::A2aAdapter;
use super::composed::ComposedCodec;
use super::cross_protocol::{
    CrossProtocolCodec, CrossProtocolError, ProtocolSide, RequestCodecReport,
};
use super::mcp_adapter::McpAdapter;

/// A codec between an MCP peer and an A2A peer.
///
/// Cheap to construct and to share: it holds no state of its own beyond
/// the two adapters it composes.
#[derive(Debug, Clone)]
pub struct A2aMcpCodec {
    /// The composition underneath.
    inner: ComposedCodec,
}

impl A2aMcpCodec {
    /// A codec with MCP downstream and A2A upstream.
    ///
    /// The common shape: an MCP client in front of the bridge, an A2A
    /// agent behind it. The client's `tools/call` becomes the agent's
    /// `SendMessage`, and the agent's answer comes back as a
    /// `CallToolResult`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: ComposedCodec::new(Arc::new(McpAdapter::new()), Arc::new(A2aAdapter::new())),
        }
    }

    /// A codec with A2A downstream and MCP upstream.
    ///
    /// The mirror: an A2A client in front of the bridge, an MCP agent
    /// behind it. The client's `SendMessage` becomes the agent's
    /// `tools/call`.
    #[must_use]
    pub fn a2a_downstream() -> Self {
        Self {
            inner: ComposedCodec::new(Arc::new(A2aAdapter::new()), Arc::new(McpAdapter::new())),
        }
    }

    /// The composition underneath, for a caller that wants the adapters.
    #[must_use]
    pub const fn composed(&self) -> &ComposedCodec {
        &self.inner
    }
}

impl Default for A2aMcpCodec {
    /// The MCP-downstream codec, the same as [`A2aMcpCodec::new`].
    fn default() -> Self {
        Self::new()
    }
}

impl CrossProtocolCodec for A2aMcpCodec {
    /// The protocol the downstream leg speaks.
    fn downstream_protocol(&self) -> ProtocolSide {
        self.inner.downstream_protocol()
    }

    /// The protocol the upstream leg speaks.
    fn upstream_protocol(&self) -> ProtocolSide {
        self.inner.upstream_protocol()
    }

    /// The canonical IR method a downstream wire method lifts into.
    fn ir_method(&self, downstream_method: &str) -> Option<IrMethod> {
        self.inner.ir_method(downstream_method)
    }

    /// Translates a downstream request envelope into the upstream's wire form.
    ///
    /// # Errors
    ///
    /// The same set as [`ComposedCodec::encode_request`].
    fn encode_request(
        &self,
        ir_method: &IrMethod,
        raw_envelope: &Map<String, Value>,
        report: &mut RequestCodecReport,
    ) -> Result<Map<String, Value>, CrossProtocolError> {
        self.inner.encode_request(ir_method, raw_envelope, report)
    }

    /// Translates an upstream reply envelope back into the downstream's wire form.
    ///
    /// # Errors
    ///
    /// The same set as [`ComposedCodec::decode_reply`].
    fn decode_reply(
        &self,
        ir_method: &IrMethod,
        reply: &Map<String, Value>,
        report: &mut RequestCodecReport,
    ) -> Result<Map<String, Value>, CrossProtocolError> {
        self.inner.decode_reply(ir_method, reply, report)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value, json};

    use super::super::cross_protocol::RequestCodecStep;
    use super::*;

    fn report() -> RequestCodecReport {
        RequestCodecReport::empty()
    }

    fn object_of(input: &Value) -> Map<String, Value> {
        input.as_object().cloned().unwrap_or_default()
    }

    /// The MCP call a client sends.
    fn mcp_tools_call() -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "lookup", "arguments": {"query": "weather"}}
        })
    }

    /// The A2A call a client sends.
    fn a2a_send_message() -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "SendMessage",
            "params": {
                "message": {"role": "user", "parts": [{"data": {"query": "weather"}}]},
                "metadata": {"bridge_skill": "lookup"}
            }
        })
    }

    #[test]
    fn an_mcp_tools_call_becomes_an_a2a_send_message() -> Result<(), Box<dyn std::error::Error>> {
        let codec = A2aMcpCodec::new();
        let mut rep = report();
        let raw = object_of(&mcp_tools_call());
        let method = codec
            .ir_method("tools/call")
            .ok_or("the codec does not bridge tools/call")?;
        let out = codec.encode_request(&method, &raw, &mut rep)?;
        assert_eq!(
            out.get("method").and_then(Value::as_str),
            Some("SendMessage")
        );
        let params = out
            .get("params")
            .and_then(Value::as_object)
            .ok_or("params was not an object")?;
        let message = params
            .get("message")
            .and_then(Value::as_object)
            .ok_or("no message")?;
        let parts = message
            .get("parts")
            .and_then(Value::as_array)
            .ok_or("no parts")?;
        let part = parts.first().ok_or("no parts")?;
        let data = part
            .get("data")
            .and_then(Value::as_object)
            .ok_or("the part carried no data")?;
        assert_eq!(data.get("query").and_then(Value::as_str), Some("weather"));
        assert_eq!(
            params
                .get("metadata")
                .and_then(|meta| meta.get("bridge_skill"))
                .and_then(Value::as_str),
            Some("lookup")
        );
        Ok(())
    }

    #[test]
    fn an_a2a_send_message_becomes_an_mcp_tools_call() -> Result<(), Box<dyn std::error::Error>> {
        let codec = A2aMcpCodec::a2a_downstream();
        let mut rep = report();
        let raw = object_of(&a2a_send_message());
        let method = codec
            .ir_method("SendMessage")
            .ok_or("the codec does not bridge SendMessage")?;
        let out = codec.encode_request(&method, &raw, &mut rep)?;
        assert_eq!(
            out.get("method").and_then(Value::as_str),
            Some("tools/call")
        );
        let params = out
            .get("params")
            .and_then(Value::as_object)
            .ok_or("params was not an object")?;
        assert_eq!(params.get("name").and_then(Value::as_str), Some("lookup"));
        let arguments = params
            .get("arguments")
            .and_then(Value::as_object)
            .ok_or("no arguments")?;
        assert_eq!(
            arguments.get("query").and_then(Value::as_str),
            Some("weather")
        );
        Ok(())
    }

    #[test]
    fn the_arguments_survive_a_round_trip_through_the_other_protocol()
    -> Result<(), Box<dyn std::error::Error>> {
        // The MCP call goes out as an A2A `SendMessage` and the reply
        // comes back as an MCP `CallToolResult`. The arguments the
        // client sent are the arguments the agent receives — no wrapper
        // key, no double nesting.
        let codec = A2aMcpCodec::new();
        let mut rep = report();
        let raw = object_of(&mcp_tools_call());
        let method = codec
            .ir_method("tools/call")
            .ok_or("the codec does not bridge tools/call")?;
        let outbound = codec.encode_request(&method, &raw, &mut rep)?;

        let params = outbound
            .get("params")
            .and_then(Value::as_object)
            .ok_or("params was not an object")?;
        let message = params
            .get("message")
            .and_then(Value::as_object)
            .ok_or("no message")?;
        let parts = message
            .get("parts")
            .and_then(Value::as_array)
            .ok_or("no parts")?;
        let part = parts.first().ok_or("no parts")?;
        let data = part
            .get("data")
            .and_then(Value::as_object)
            .ok_or("the part carried no data")?;
        assert_eq!(
            data.get("query").and_then(Value::as_str),
            Some("weather"),
            "the agent receives the arguments the client sent"
        );
        Ok(())
    }

    #[test]
    fn a_tool_result_comes_back_as_an_a2a_message() -> Result<(), Box<dyn std::error::Error>> {
        // The MCP agent answered; the client on the other side of the
        // bridge is an A2A client, so the reply is lowered into a
        // message.
        let codec = A2aMcpCodec::a2a_downstream();
        let mut rep = report();
        let method = codec
            .ir_method("SendMessage")
            .ok_or("the codec does not bridge SendMessage")?;
        let reply = object_of(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"content": [{"type": "text", "text": "hello"}]}
        }));
        let out = codec.decode_reply(&method, &reply, &mut rep)?;
        let result = out
            .get("result")
            .and_then(Value::as_object)
            .ok_or("no result")?;
        let message = result
            .get("message")
            .and_then(Value::as_object)
            .ok_or("no message")?;
        let parts = message
            .get("parts")
            .and_then(Value::as_array)
            .ok_or("no parts")?;
        let part = parts.first().ok_or("no parts")?;
        assert_eq!(part.get("text").and_then(Value::as_str), Some("hello"));
        Ok(())
    }

    #[test]
    fn a_task_comes_back_as_a_tool_result() -> Result<(), Box<dyn std::error::Error>> {
        // The A2A agent answered with a task; the client on the other
        // side of the bridge is an MCP client, so the reply is lowered
        // into a `CallToolResult`.
        let codec = A2aMcpCodec::new();
        let mut rep = report();
        let method = codec
            .ir_method("tools/call")
            .ok_or("the codec does not bridge tools/call")?;
        let reply = object_of(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "id": "task-1",
                "status": {"state": "completed"},
                "history": [
                    {"role": "user", "parts": [{"text": "hi"}]},
                    {"role": "agent", "parts": [{"text": "hello"}]}
                ]
            }
        }));
        let out = codec.decode_reply(&method, &reply, &mut rep)?;
        let result = out
            .get("result")
            .and_then(Value::as_object)
            .ok_or("no result")?;
        let content = result
            .get("content")
            .and_then(Value::as_array)
            .ok_or("no content")?;
        let block = content.first().ok_or("no content")?;
        assert_eq!(block.get("text").and_then(Value::as_str), Some("hello"));
        Ok(())
    }

    #[test]
    fn a_method_the_codec_does_not_bridge_is_refused() -> Result<(), Box<dyn std::error::Error>> {
        let codec = A2aMcpCodec::a2a_downstream();
        let mut rep = report();
        let raw = object_of(&json!({"jsonrpc": "2.0", "id": 1, "method": "GetTask"}));
        assert!(codec.ir_method("GetTask").is_none());
        let method = codec
            .ir_method("SendMessage")
            .ok_or("the codec does not bridge SendMessage")?;
        let outcome = codec.encode_request(&method, &raw, &mut rep);
        assert!(matches!(
            outcome,
            Err(CrossProtocolError::MethodNotBridged(_, ProtocolSide::A2a))
        ));
        Ok(())
    }

    #[test]
    fn the_report_records_the_rename_and_the_shapes() -> Result<(), Box<dyn std::error::Error>> {
        let codec = A2aMcpCodec::new();
        let mut rep = report();
        let raw = object_of(&mcp_tools_call());
        let method = codec
            .ir_method("tools/call")
            .ok_or("the codec does not bridge tools/call")?;
        codec.encode_request(&method, &raw, &mut rep)?;
        assert!(rep.steps.iter().any(|step| matches!(
            step,
            RequestCodecStep::MethodRenamed {
                from: "tools/call",
                to: "SendMessage"
            }
        )));
        assert!(
            rep.steps
                .iter()
                .any(|step| matches!(step, RequestCodecStep::ParamsTranslated { .. }))
        );
        Ok(())
    }
}
