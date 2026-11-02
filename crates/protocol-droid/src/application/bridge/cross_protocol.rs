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

use crate::domain::ir::IrMethod;

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
    /// The side's name, for a report or an error message.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::A2a => "a2a",
            Self::Mcp => "mcp",
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
    /// Something in the payload had no counterpart on the other side and
    /// was left behind.
    ///
    /// A loss is recorded rather than passed over in silence: the bridge
    /// cannot always carry every part of a message across, and an operator
    /// reading the report has to be able to see that it did not.
    Dropped {
        /// What was left behind, in human terms.
        what: &'static str,
        /// How many.
        count: usize,
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
            Self::Dropped { what, count } => {
                write!(formatter, "dropped {count} × {what}")
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
    /// The wire shape did not fit the protocol it arrived on.
    ///
    /// A peer sent something the adapter cannot read — a `tools/call`
    /// whose `arguments` is not an object, an A2A message whose `parts`
    /// is not an array. The bridge reports it as a bad request rather
    /// than forwarding a shape the upstream would reject with an error
    /// of its own.
    #[error("the {side:?} payload did not fit its shape: {detail}")]
    MalformedPayload {
        /// The protocol whose shape was not met.
        side: ProtocolSide,
        /// What was wrong, in the adapter's words.
        detail: String,
    },
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
/// # Why the codec is a pair of adapters rather than a translator
///
/// The obvious shape for this trait is "a translator that knows how to
/// turn protocol A's request into protocol B's". That shape does not
/// scale past two protocols: A-to-B, B-to-A, A-to-C, C-to-A, B-to-C, C-to-B
/// is six translators for three protocols, and each one is hand-written
/// from the same two vocabularies.
///
/// This trait is instead a *composition*. A codec is built from two
/// [`ProtocolAdapter`]s — one per side — and the translation is
/// `lift` on the downstream side, `lower` on the upstream side, with the
/// IR's [`IrCall`] in between. The per-protocol knowledge lives in the
/// adapter, so the number of pieces of translation code is the number of
/// protocols, not the number of pairs. The composition itself is a struct
/// of two references and no logic.
///
/// # Why the method is an `IrMethod` and not a string
///
/// The bridge routes on a protocol-neutral method. `IrMethod::Call` is
/// what an MCP `tools/call` and an A2A `SendMessage` both are; the wire
/// names are the adapters' business. A string parameter would put the
/// protocol's vocabulary back into the seam, which is the thing this
/// trait exists to remove.
pub trait CrossProtocolCodec: Send + Sync + core::fmt::Debug {
    /// The protocol the downstream leg speaks.
    fn downstream_protocol(&self) -> ProtocolSide;

    /// The protocol the upstream leg speaks.
    fn upstream_protocol(&self) -> ProtocolSide;

    /// The canonical IR method a downstream wire method lifts into.
    ///
    /// Returns `None` when this codec cannot bridge the method — the
    /// bridge refuses the call with `-32601 Method not found` rather than
    /// forwarding a name the upstream would reject with an error of its
    /// own.
    fn ir_method(&self, downstream_method: &str) -> Option<IrMethod>;

    /// Translates a downstream request envelope into the upstream's wire form.
    ///
    /// `ir_method` is the canonical method the bridge assigned, which
    /// `ir_method` resolved from the downstream's wire name.
    /// `raw_envelope` is the downstream's original wire shape, so the
    /// codec can read the parameters in their native form.
    ///
    /// # Errors
    ///
    /// [`CrossProtocolError::MethodNotBridged`] if the method is one
    /// this codec has no rule for, and
    /// [`CrossProtocolError::MalformedPayload`] if the downstream's
    /// payload does not fit the shape its protocol requires.
    fn encode_request(
        &self,
        ir_method: &IrMethod,
        raw_envelope: &Map<String, Value>,
        report: &mut RequestCodecReport,
    ) -> Result<Map<String, Value>, CrossProtocolError>;

    /// Translates an upstream reply envelope back into the downstream's
    /// wire form.
    ///
    /// `ir_method` is the canonical method the request carried; the
    /// codec uses it to know which reply shape to expect (an A2A `Task`
    /// reply for a `SendMessage` request, a `Content` list for a
    /// `tools/call` request).
    ///
    /// # Errors
    ///
    /// The same set of errors as [`encode_request`](Self::encode_request).
    fn decode_reply(
        &self,
        ir_method: &IrMethod,
        reply: &Map<String, Value>,
        report: &mut RequestCodecReport,
    ) -> Result<Map<String, Value>, CrossProtocolError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_side_names_itself_for_a_report() {
        assert_eq!(ProtocolSide::A2a.name(), "a2a");
        assert_eq!(ProtocolSide::Mcp.name(), "mcp");
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
