//! The per-protocol half of a cross-protocol bridge.
//!
//! # Why an adapter and not a translator
//!
//! A translator that knows "A2A becomes MCP" is one translator per *pair*
//! of protocols. Three protocols need six of them, and the sixth is not a
//! composition of the first two — it is a third reading of the same two
//! vocabularies, written by whoever needed the pair.
//!
//! An adapter knows only about *one* protocol: how that protocol's
//! requests and replies lift into the IR, and how the IR lowers back out
//! into that protocol's wire form. Two adapters meet at
//! [`IrCall`](crate::domain::ir::IrCall) and
//! [`IrOutcome`](crate::domain::ir::IrOutcome), and a bridge between two
//! protocols is the composition of the two — no translation code of its
//! own.
//!
//! # Where the per-pair knowledge went
//!
//! The method names are the per-pair knowledge that used to live in a
//! table keyed by "downstream side". They live in the adapters now:
//! `McpAdapter` says `tools/call` is [`IrMethod::Call`], and `A2aAdapter`
//! says `SendMessage` is [`IrMethod::Call`]. Neither knows the other
//! exists, which is what makes the composition work for a pair nobody
//! wrote code for.

use serde_json::Value;

use crate::domain::ir::{IrCall, IrMethod, IrOutcome};

use super::cross_protocol::{CrossProtocolError, ProtocolSide, RequestCodecReport};

/// A request in a protocol's own vocabulary: a method name and its
/// parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireRequest {
    /// The method name, in the protocol's own vocabulary.
    pub method: String,
    /// The parameters.
    pub params: Value,
}

/// One protocol's half of a bridge.
///
/// Implementors lift their protocol's wire form into the IR and lower the
/// IR back out. Nothing here mentions another protocol, and nothing here
/// knows which side of a bridge the adapter is on: the composition in
/// [`super::composed`] decides that.
///
/// # Examples
///
/// An adapter for a protocol whose only bridged method is a call:
///
/// ```
/// use protocol_droid::application::bridge::{
///     CrossProtocolError, ProtocolAdapter, ProtocolSide, RequestCodecReport, WireRequest,
/// };
/// use protocol_droid::domain::ir::{IrCall, IrMethod, IrOutcome};
/// use serde_json::Value;
///
/// #[derive(Debug)]
/// struct Example;
///
/// impl ProtocolAdapter for Example {
///     fn side(&self) -> ProtocolSide {
///         ProtocolSide::Mcp
///     }
///
///     fn ir_method(&self, wire_method: &str) -> Option<IrMethod> {
///         (wire_method == "example/call").then_some(IrMethod::Call)
///     }
///
///     fn wire_method(&self, ir_method: &IrMethod) -> Option<&'static str> {
///         match ir_method {
///             IrMethod::Call => Some("example/call"),
///         }
///     }
///
///     fn request_shape(&self) -> &'static str {
///         "example call"
///     }
///
///     fn reply_shape(&self) -> &'static str {
///         "example reply"
///     }
///
///     fn lift_request(
///         &self,
///         _wire_method: &str,
///         _params: &Value,
///         _report: &mut RequestCodecReport,
///     ) -> Result<IrCall, CrossProtocolError> {
///         Err(CrossProtocolError::MethodNotBridged(
///             "example/call".to_owned(),
///             ProtocolSide::Mcp,
///         ))
///     }
///
///     fn lower_request(
///         &self,
///         _call: &IrCall,
///         _report: &mut RequestCodecReport,
///     ) -> Result<WireRequest, CrossProtocolError> {
///         Err(CrossProtocolError::MethodNotBridged(
///             "call".to_owned(),
///             ProtocolSide::Mcp,
///         ))
///     }
///
///     fn lift_reply(
///         &self,
///         _result: &Value,
///         _report: &mut RequestCodecReport,
///     ) -> Result<IrOutcome, CrossProtocolError> {
///         Err(CrossProtocolError::MethodNotBridged(
///             "example/call".to_owned(),
///             ProtocolSide::Mcp,
///         ))
///     }
///
///     fn lower_reply(
///         &self,
///         _outcome: &IrOutcome,
///         _report: &mut RequestCodecReport,
///     ) -> Result<Value, CrossProtocolError> {
///         Err(CrossProtocolError::MethodNotBridged(
///             "call".to_owned(),
///             ProtocolSide::Mcp,
///         ))
///     }
/// }
/// ```
pub trait ProtocolAdapter: Send + Sync + core::fmt::Debug {
    /// The protocol this adapter speaks.
    fn side(&self) -> ProtocolSide;

    /// The canonical IR method this adapter lifts a wire method into.
    ///
    /// `None` for a method this protocol has no IR counterpart for. The
    /// bridge refuses such a call with `-32601 Method not found` rather
    /// than sending a name the peer would reject with an error of its own.
    fn ir_method(&self, wire_method: &str) -> Option<IrMethod>;

    /// The wire method this adapter lowers an IR method to.
    ///
    /// `None` when this protocol cannot express the IR method at all.
    fn wire_method(&self, ir_method: &IrMethod) -> Option<&'static str>;

    /// A human-readable name for this protocol's call shape.
    ///
    /// It appears in the bridge's translation report, so an operator
    /// reading the report can tell which shape became which without
    /// knowing either protocol's wire format.
    fn request_shape(&self) -> &'static str;

    /// A human-readable name for this protocol's reply shape.
    ///
    /// The counterpart to [`request_shape`](Self::request_shape).
    fn reply_shape(&self) -> &'static str;

    /// Lifts a wire request into the IR.
    ///
    /// `report` is the call's translation report: an adapter that cannot
    /// carry something across records the loss there rather than dropping
    /// it silently.
    ///
    /// # Errors
    ///
    /// [`CrossProtocolError::MethodNotBridged`] when `wire_method` is not
    /// one this adapter lifts, and
    /// [`CrossProtocolError::MalformedPayload`] when the parameters do
    /// not fit the shape the protocol requires.
    fn lift_request(
        &self,
        wire_method: &str,
        params: &Value,
        report: &mut RequestCodecReport,
    ) -> Result<IrCall, CrossProtocolError>;

    /// Lowers an IR call back into this protocol's wire form.
    ///
    /// # Errors
    ///
    /// [`CrossProtocolError::MethodNotBridged`] when this protocol cannot
    /// express the call's method, and
    /// [`CrossProtocolError::MalformedPayload`] when a field of the call
    /// has no wire form here.
    fn lower_request(
        &self,
        call: &IrCall,
        report: &mut RequestCodecReport,
    ) -> Result<WireRequest, CrossProtocolError>;

    /// Lifts a wire reply into the IR.
    ///
    /// # Errors
    ///
    /// [`CrossProtocolError::MalformedPayload`] when the result does not
    /// fit the shape the protocol requires.
    fn lift_reply(
        &self,
        result: &Value,
        report: &mut RequestCodecReport,
    ) -> Result<IrOutcome, CrossProtocolError>;

    /// Lowers an IR outcome back into this protocol's wire form.
    ///
    /// # Errors
    ///
    /// [`CrossProtocolError::MalformedPayload`] when a field of the
    /// outcome has no wire form here.
    fn lower_reply(
        &self,
        outcome: &IrOutcome,
        report: &mut RequestCodecReport,
    ) -> Result<Value, CrossProtocolError>;
}
