//! The cross-protocol half of the bridge: request envelopes whose method
//! names and parameter shapes differ between the two legs.
//!
//! # Why this is a separate trait from `ToolResultCodec`
//!
//! `ToolResultCodec` is the per-revision *content* translator — the piece that
//! decides whether a `CallToolResult` needs to be downgraded before the
//! downstream client can read it. The shape of the *request* envelope is
//! identical at every MCP revision: `tools/call` is `tools/call`, `params`
//! carries the same keys. A bridge that only had `ToolResultCodec` could
//! forward MCP ↔ MCP, because nothing in the request needs renaming.
//!
//! A bridge between two *different* protocols cannot. A2A's
//! `SendMessage` has no MCP counterpart, and MCP's `tools/call` has no
//! A2A counterpart, and the bridge has to know the pair exists. That
//! knowledge does not belong in `ToolResultCodec` because it is about
//! *requests*, not about results. It does not belong in `BridgePeer`,
//! because the peer is a transport and does not know the two protocols
//! speak different languages. It is its own thing.
//!
//! # What this trait is *not*
//!
//! It is not the codec that lifts/lowers JSON-RPC envelopes. Both protocols
//! use the same JSON-RPC 2.0 wire envelope, so the bridge's `lift_envelope`
//! and `lower_envelope` are unchanged. This trait operates on the IR the
//! lift produced, with the protocol-agnostic method name (`MethodName`) the
//! bridge tracks.
//!
//! # Why the codec is asked per-message, not pre-loaded
//!
//! A bridge might speak MCP downstream and A2A upstream, or A2A downstream
//! and MCP upstream, or MCP on both legs (in which case this codec is
//! never called). The right question is "given *this* downstream leg and
//! *this* upstream envelope, what does the wire form look like?" — and
//! the answer depends on the leg's protocol, the upstream's protocol, and
//! the method being called. A pre-loaded table is a stale table the moment
//! a new leg arrives.

use serde_json::{Map, Value};

/// The two sides of a cross-protocol translation, recorded for the operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProtocolSide {
    /// A2A v1.0. The wire surface is JSON-RPC 2.0; the method names are
    /// `SendMessage`, `GetTask`, etc.
    A2a,
    /// MCP at any revision the bridge negotiated.
    Mcp,
}

impl ProtocolSide {
    /// The wire method name the bridge will hand to the peer on this side.
    ///
    /// Returns `None` when the bridge cannot express the method on this
    /// side at all — the caller refuses the call rather than sending a
    /// method name the peer will reject as `-32601`.
    ///
    /// The first arm is the row an A2A client hits: it called
    /// `SendMessage`, the bridge lifts that to the IR name `SendMessage`,
    /// and the codec now needs the wire name the *upstream* peer
    /// (the MCP agent) reads, which is `tools/call`. The second arm
    /// is the mirror case: an MCP client called `tools/call`, the
    /// bridge lifted to the IR name `tools/call`, and the upstream
    /// (an A2A agent) reads `SendMessage`.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::application::bridge::ProtocolSide;
    ///
    /// assert_eq!(
    ///     ProtocolSide::A2a.wire_method("SendMessage"),
    ///     Some("tools/call")
    /// );
    /// assert_eq!(
    ///     ProtocolSide::Mcp.wire_method("tools/call"),
    ///     Some("SendMessage")
    /// );
    /// ```
    #[must_use]
    pub fn wire_method(self, ir_method: &str) -> Option<&'static str> {
        match (self, ir_method) {
            (Self::A2a, "SendMessage") => Some("tools/call"),
            (Self::Mcp, "tools/call") => Some("SendMessage"),
            // The two protocols' other methods have no IR counterpart yet.
            // The bridge refuses them with `MethodNotBridged` rather than
            // forwarding a name the peer will reject.
            _ => None,
        }
    }
}

/// What translating a request and its reply produced.
///
/// Distinct from [`crate::domain::bridge::a2a_mcp::CrossProtocolReport`],
/// which records what one of the pure mapping functions did. This one
/// records what a single bridge call (one request envelope in, one reply
/// envelope out) cost the bridge.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RequestCodecReport {
    /// The steps the translation took, in order. An empty list means the
    /// call passed through with no recorded loss.
    pub steps: Vec<RequestCodecStep>,
}

impl RequestCodecReport {
    /// A report with no recorded steps.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Whether the translation did anything worth reporting.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// The number of recorded steps.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.steps.len()
    }

    /// Records `step`.
    pub fn record(&mut self, step: RequestCodecStep) {
        self.steps.push(step);
    }
}

/// One named step in a request/reply translation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestCodecStep {
    /// The method name was renamed to the upstream's vocabulary.
    MethodRenamed {
        /// The IR-side name the bridge carried in.
        from: &'static str,
        /// The wire-side name the bridge sent.
        to: &'static str,
    },
    /// The request params were translated. The shape change is recorded
    /// with enough detail for a reviewer to know what the bridge did.
    ParamsTranslated {
        /// What the IR shape is, in human terms.
        from: &'static str,
        /// What the wire shape is, in human terms.
        to: &'static str,
    },
    /// The reply was translated. Same `from`/`to` shape.
    ReplyTranslated {
        /// What the wire reply shape is, in human terms.
        from: &'static str,
        /// What the IR reply shape is, in human terms.
        to: &'static str,
    },
}

impl core::fmt::Display for RequestCodecStep {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::MethodRenamed { from, to } => {
                write!(formatter, "method {from} → {to}")
            }
            Self::ParamsTranslated { from, to } => {
                write!(formatter, "params {from} → {to}")
            }
            Self::ReplyTranslated { from, to } => {
                write!(formatter, "reply {from} → {to}")
            }
        }
    }
}

impl core::fmt::Display for RequestCodecReport {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for (i, step) in self.steps.iter().enumerate() {
            if i > 0 {
                formatter.write_str("; ")?;
            }
            core::fmt::Display::fmt(step, formatter)?;
        }
        Ok(())
    }
}

/// What a cross-protocol codec can refuse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CrossProtocolError {
    /// The downstream leg's protocol is not one this codec speaks.
    /// The bridge should never have called it; this is a wiring error.
    #[error("downstream leg speaks {0:?}, which is not bridged by this codec")]
    UnknownDownstream(ProtocolSide),
    /// The method on the IR is not one the codec has a rule for.
    /// A bridge that received this returns `-32601 Method not found` to
    /// the client, naming the method so the operator can see which call
    /// hit the gap.
    #[error("method {0:?} is not bridged between {1:?} and the upstream")]
    MethodNotBridged(String, ProtocolSide),
}

/// A codec that translates request and reply envelopes between the
/// downstream leg's protocol and the upstream's protocol.
///
/// The downstream leg's protocol is the one the client spoke; the upstream
/// is the one the bridge is calling out to. The two are usually different
/// in the cross-protocol case (an MCP client on one side, an A2A agent on
/// the other), and a `BridgePeer` for the upstream is a real connection
/// to that agent.
///
/// # Why the trait takes `Leg` not `ProtocolVersion`
///
/// The downstream leg's `ProtocolVersion` says which MCP revision the
/// client negotiated. The cross-protocol decision is bigger than that —
/// it is "is this MCP at all, or is it A2A?" — and `Leg` already carries
/// the protocol shape. A bridge that only sees one leg's protocol can
/// answer "MCP" or "A2A" and skip the per-revision question entirely,
/// which is what the per-revision `ToolResultCodec` is for on the way
/// back.
///
/// # Why the codec takes the raw downstream envelope, not just the IR
///
/// The IR is protocol-agnostic at the method-name level (it carries the
/// canonical name the bridge has assigned), but the *parameters* are
/// opaque: a `tools/call` `arguments` value is just an entry in
/// `IrParams`, indistinguishable from a `SendMessage` `message` field.
/// The codec needs the raw wire shape to know which entry is which, so
/// it re-reads `raw_envelope` rather than trying to re-derive the shape
/// from the IR. The bridge already has both — the IR for routing, the
/// raw envelope for translation.
pub trait CrossProtocolCodec: Send + Sync + core::fmt::Debug {
    /// The protocol this codec expects the downstream leg to speak.
    fn downstream_protocol(&self) -> ProtocolSide;

    /// Translates a downstream request envelope into the upstream's wire form.
    ///
    /// `downstream_side` is the protocol the downstream leg speaks
    /// (A2A or MCP). `ir_method` is the canonical method name the
    /// bridge has assigned (e.g. `SendMessage` for an A2A call,
    /// `tools/call` for an MCP call). `raw_envelope` is the
    /// downstream's original wire shape, so the codec can read the
    /// parameters in their native form.
    ///
    /// # Errors
    ///
    /// [`CrossProtocolError::MethodNotBridged`] if the method is one
    /// this codec has no rule for.
    fn encode_request(
        &self,
        downstream_side: ProtocolSide,
        ir_method: &str,
        raw_envelope: &Map<String, Value>,
        report: &mut RequestCodecReport,
    ) -> Result<Map<String, Value>, CrossProtocolError>;

    /// Translates an upstream reply envelope back into the downstream's
    /// wire form.
    ///
    /// `downstream_side` is the protocol the downstream leg speaks.
    /// `ir_method` is the canonical method name the request carried; the
    /// codec uses it to know which reply shape to expect (an A2A `Task`
    /// reply for a `SendMessage` request, a `Content` list for a
    /// `tools/call` request).
    ///
    /// # Errors
    ///
    /// The same set of errors as [`encode_request`](Self::encode_request).
    fn decode_reply(
        &self,
        downstream_side: ProtocolSide,
        ir_method: &str,
        reply: &Map<String, Value>,
        report: &mut RequestCodecReport,
    ) -> Result<Map<String, Value>, CrossProtocolError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_a2a_downstream_calling_send_message_lets_the_codec_route_to_tools_call() {
        assert_eq!(
            ProtocolSide::A2a.wire_method("SendMessage"),
            Some("tools/call")
        );
    }

    #[test]
    fn an_mcp_downstream_calling_tools_call_lets_the_codec_route_to_send_message() {
        assert_eq!(
            ProtocolSide::Mcp.wire_method("tools/call"),
            Some("SendMessage")
        );
    }

    #[test]
    fn an_unknown_method_returns_none_on_both_sides() {
        assert_eq!(ProtocolSide::Mcp.wire_method("GetTask"), None);
        assert_eq!(ProtocolSide::A2a.wire_method("resources/list"), None);
    }

    #[test]
    fn report_starts_empty_and_records_steps() {
        let mut report = RequestCodecReport::empty();
        assert!(report.is_empty());
        report.record(RequestCodecStep::MethodRenamed {
            from: "SendMessage",
            to: "tools/call",
        });
        assert_eq!(report.len(), 1);
    }

    #[test]
    fn report_display_lists_steps_in_order() {
        let mut report = RequestCodecReport::empty();
        report.record(RequestCodecStep::MethodRenamed {
            from: "SendMessage",
            to: "tools/call",
        });
        report.record(RequestCodecStep::ParamsTranslated {
            from: "A2A message+metadata",
            to: "MCP arguments",
        });
        let s = report.to_string();
        assert!(s.contains("SendMessage → tools/call"));
        assert!(s.contains("A2A message+metadata → MCP arguments"));
    }
}
