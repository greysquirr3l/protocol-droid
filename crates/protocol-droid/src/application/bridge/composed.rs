//! The composition that makes a bridge out of two adapters.
//!
//! # What this is
//!
//! A [`CrossProtocolCodec`] built from a downstream adapter and an upstream
//! adapter. The translation is `lift` on the downstream side, `lower` on
//! the upstream side, and the IR is what sits between them.
//!
//! # Why the composition has no logic in it
//!
//! Everything that used to be per-pair — the method names, the parameter
//! shapes, the reply shapes — lives in the adapters now. What is left here
//! is the order of operations, which is the same for every pair: lift on
//! the way out, lower on the way back.
//!
//! That is the whole point of the shape. A bridge between MCP and A2A is
//! two adapters and this file. A bridge between MCP and a third protocol
//! is one new adapter and this file again — there is no second translator
//! to write, and no table of "which method means what on which side" that
//! has to grow a row per pair.

use std::sync::Arc;

use serde_json::{Map, Value};

use crate::domain::ir::IrMethod;

use super::cross_protocol::{
    CrossProtocolCodec, CrossProtocolError, ProtocolSide, RequestCodecReport, RequestCodecStep,
};
use super::protocol_adapter::ProtocolAdapter;

/// A codec built from a downstream adapter and an upstream adapter.
#[derive(Debug, Clone)]
pub struct ComposedCodec {
    /// The adapter for the leg the client speaks to.
    downstream: Arc<dyn ProtocolAdapter>,
    /// The adapter for the leg the bridge calls out to.
    upstream: Arc<dyn ProtocolAdapter>,
}

impl ComposedCodec {
    /// Composes a codec from the two sides of a bridge.
    ///
    /// The downstream adapter is the one the client speaks to; the
    /// upstream adapter is the one the bridge calls out to. Neither is
    /// checked against the other here: a codec that cannot bridge a
    /// method refuses the *call*, with `-32601`, rather than refusing to
    /// be built — the same method can be bridged in one direction and not
    /// the other.
    #[must_use]
    pub fn new(downstream: Arc<dyn ProtocolAdapter>, upstream: Arc<dyn ProtocolAdapter>) -> Self {
        Self {
            downstream,
            upstream,
        }
    }

    /// The downstream adapter.
    #[must_use]
    pub fn downstream(&self) -> &Arc<dyn ProtocolAdapter> {
        &self.downstream
    }

    /// The upstream adapter.
    #[must_use]
    pub fn upstream(&self) -> &Arc<dyn ProtocolAdapter> {
        &self.upstream
    }
}

impl CrossProtocolCodec for ComposedCodec {
    /// The protocol the downstream leg speaks.
    fn downstream_protocol(&self) -> ProtocolSide {
        self.downstream.side()
    }

    /// The protocol the upstream leg speaks.
    fn upstream_protocol(&self) -> ProtocolSide {
        self.upstream.side()
    }

    /// The canonical IR method a downstream wire method lifts into.
    fn ir_method(&self, downstream_method: &str) -> Option<IrMethod> {
        self.downstream.ir_method(downstream_method)
    }

    /// Translates a downstream request envelope into the upstream's wire form.
    ///
    /// # Errors
    ///
    /// The same set as the downstream adapter's `lift_request`, plus
    /// [`CrossProtocolError::MethodNotBridged`] when the upstream cannot
    /// express the call the downstream asked for.
    fn encode_request(
        &self,
        ir_method: &IrMethod,
        raw_envelope: &Map<String, Value>,
        report: &mut RequestCodecReport,
    ) -> Result<Map<String, Value>, CrossProtocolError> {
        let method = raw_envelope
            .get("method")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                CrossProtocolError::MethodNotBridged(
                    ir_method.name().to_owned(),
                    self.downstream.side(),
                )
            })?;
        if self.downstream.ir_method(method) != Some(*ir_method) {
            return Err(CrossProtocolError::MethodNotBridged(
                method.to_owned(),
                self.downstream.side(),
            ));
        }
        let params = raw_envelope
            .get("params")
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));

        let call = self.downstream.lift_request(method, &params, report)?;
        let wire = self.upstream.lower_request(&call, report)?;

        // The envelope is what the bridge forwards, and the bridge's own
        // `lower_envelope` is the right level for the JSON-RPC
        // scaffolding. The codec only changes the method name and the
        // params; it leaves `jsonrpc`, `id`, and the unmodelled
        // extensions alone.
        let mut outbound = raw_envelope.clone();
        outbound.insert("method".to_owned(), Value::String(wire.method.clone()));
        outbound.insert("params".to_owned(), wire.params);

        if let (Some(from), Some(to)) = (
            self.downstream.wire_method(ir_method),
            self.upstream.wire_method(ir_method),
        ) {
            report.record(RequestCodecStep::MethodRenamed { from, to });
        }
        report.record(RequestCodecStep::ParamsTranslated {
            from: self.downstream.request_shape(),
            to: self.upstream.request_shape(),
        });
        Ok(outbound)
    }

    /// Translates an upstream reply envelope back into the downstream's
    /// wire form.
    ///
    /// # Errors
    ///
    /// The same set as the upstream adapter's `lift_reply`, plus
    /// [`CrossProtocolError::MethodNotBridged`] when the downstream
    /// cannot express the reply.
    fn decode_reply(
        &self,
        ir_method: &IrMethod,
        reply: &Map<String, Value>,
        report: &mut RequestCodecReport,
    ) -> Result<Map<String, Value>, CrossProtocolError> {
        if self.downstream.wire_method(ir_method).is_none()
            || self.upstream.wire_method(ir_method).is_none()
        {
            return Err(CrossProtocolError::MethodNotBridged(
                ir_method.name().to_owned(),
                self.downstream.side(),
            ));
        }

        let mut outbound = reply.clone();
        let Some(result) = outbound.get_mut("result") else {
            // An error reply is not a result and needs no translation.
            // The bridge forwards it as-is.
            return Ok(outbound);
        };

        // The reply arrived in the upstream's language, so it lifts with
        // the upstream adapter and lowers with the downstream one.
        let outcome = self.upstream.lift_reply(result, report)?;
        *result = self.downstream.lower_reply(&outcome, report)?;

        report.record(RequestCodecStep::ReplyTranslated {
            from: self.upstream.reply_shape(),
            to: self.downstream.reply_shape(),
        });
        Ok(outbound)
    }
}
