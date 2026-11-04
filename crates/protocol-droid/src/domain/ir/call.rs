//! `IrCall` and `IrOutcome` — the protocol-neutral unit of work.
//!
//! # Why these two types exist
//!
//! The rest of the IR describes an MCP-shaped message: a JSON-RPC request
//! with a method name and an opaque bag of parameters. That is the right
//! shape for a peer speaking MCP, because MCP *is* JSON-RPC and the
//! parameter names are the spec's.
//!
//! It is the wrong shape for a bridge between protocols that both ride on
//! JSON-RPC but disagree about what a call *is*. MCP says a call is
//! `tools/call` with `name` and `arguments`. A2A says a call is
//! `SendMessage` with a `message` whose parts carry the payload and a
//! `metadata` entry naming the skill. Nothing in the MCP-shaped IR says
//! which is which — `IrParams` is an opaque bag, and a `tools/call`
//! `arguments` value is indistinguishable from a `SendMessage` `message`
//! field.
//!
//! So a bridge that wants to speak to both sides has to keep the *pair* in
//! its head: it holds a hand-written translator that knows "an A2A call
//! becomes an MCP call" and "an MCP call becomes an A2A call". That is
//! one translator per pair of protocols, and the number of pairs grows with
//! the square of the number of protocols.
//!
//! `IrCall` is the meeting point. It says what every one of these protocols
//! agrees on: **a named target, some arguments, and some content**. An
//! adapter lifts its own wire form into it and lowers it back out. Two
//! protocols bridge because their adapters meet at `IrCall`, not because
//! somebody wrote the pair.
//!
//! # What the types deliberately do not carry
//!
//! No method name from any protocol, and no wire shape. `IrMethod::Call` is
//! the only method today because `tools/call` and `SendMessage` are the only
//! pair the bridge bridges; adding `ListTools` or `ReadResource` is a
//! variant on this enum, not a new concept. Everything a protocol says
//! that the others do not — a skill name, a `_meta` bag, a task id — rides
//! in `extensions`, which is the passthrough bag the IR already has.

use crate::domain::ir::{Content, Extensions, JsonObject};

/// A protocol-neutral method identity.
///
/// The bridge routes on this rather than on a wire method name, which is
/// what lets a method mean the same thing on both sides of a bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IrMethod {
    /// A call to a named target: an MCP `tools/call`, an A2A `SendMessage`.
    ///
    /// The one method both protocols express, and the one the bridge
    /// translates. A protocol that cannot express a call at all has no
    /// adapter for this method and refuses the request rather than sending
    /// something the peer will reject with `-32601`.
    Call,
}

impl IrMethod {
    /// The protocol-neutral name of the method.
    ///
    /// Stable across revisions and protocols, so it can appear in a report
    /// without implying a wire vocabulary.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Call => "call",
        }
    }
}

impl core::fmt::Display for IrMethod {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// A protocol-neutral call: what a peer asked for, without a protocol's
/// vocabulary.
///
/// The three payload fields are the intersection of what MCP and A2A can
/// both express. A protocol that carries something else — a skill name, a
/// `_meta` bag — puts it in `extensions`, and the other side's adapter
/// reads it from there.
#[derive(Debug, Clone, PartialEq)]
pub struct IrCall {
    /// The method this call is.
    pub method: IrMethod,
    /// The name of the thing being called.
    ///
    /// An MCP tool name, an A2A skill name. Empty when the caller named
    /// no target, which is legal on the wire and is why this is a `String`
    /// and not an `Option`.
    pub target: String,
    /// The arguments for the target.
    ///
    /// `None` when the call carried no arguments at all, which is
    /// different from an empty object: MCP omits `arguments` for a tool
    /// that takes none, and A2A sends a message with no data part. An
    /// adapter that cannot tell those apart would send `{}` where the
    /// peer sent nothing.
    pub arguments: Option<JsonObject>,
    /// Fields the call carried that neither side modelled.
    pub extensions: Extensions,
}

impl IrCall {
    /// A call to `target` with `arguments` and nothing else.
    #[must_use]
    pub fn new(method: IrMethod, target: impl Into<String>, arguments: Option<JsonObject>) -> Self {
        Self {
            method,
            target: target.into(),
            arguments,
            extensions: Extensions::empty(),
        }
    }

    /// Attaches unmodelled fields.
    #[must_use]
    pub fn with_extensions(mut self, extensions: Extensions) -> Self {
        self.extensions = extensions;
        self
    }
}

/// A protocol-neutral outcome: what a call produced.
///
/// The counterpart to [`IrCall`]. A protocol's reply shape lifts into this
/// and lowers back out, so the reply path has the same property as the
/// request path: the translation lives in the per-protocol adapter.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct IrOutcome {
    /// The content the call produced.
    pub content: Vec<Content>,
    /// A structured payload, for a protocol that has one.
    ///
    /// MCP's `structuredContent` and nothing else today; `None` on a
    /// protocol with no such field.
    pub structured: Option<JsonObject>,
    /// Whether the call failed.
    ///
    /// MCP's `isError`, and an A2A task state of `failed` or `rejected`.
    pub is_error: bool,
    /// Fields the reply carried that neither side modelled.
    pub extensions: Extensions,
}

impl IrOutcome {
    /// An outcome with content and nothing else.
    #[must_use]
    pub const fn new(content: Vec<Content>) -> Self {
        Self {
            content,
            structured: None,
            is_error: false,
            extensions: Extensions::empty(),
        }
    }

    /// Attaches a structured payload.
    #[must_use]
    pub fn with_structured(mut self, structured: JsonObject) -> Self {
        self.structured = Some(structured);
        self
    }

    /// Marks the outcome as a failure.
    #[must_use]
    pub const fn with_error(mut self, is_error: bool) -> Self {
        self.is_error = is_error;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_starts_with_no_extensions() {
        let call = IrCall::new(IrMethod::Call, "search", None);
        assert!(call.extensions.is_empty());
        assert_eq!(call.target, "search");
        assert!(call.arguments.is_none());
    }

    #[test]
    fn no_arguments_is_not_the_same_as_an_empty_object() {
        let without = IrCall::new(IrMethod::Call, "ping", None);
        let empty = IrCall::new(IrMethod::Call, "ping", Some(JsonObject::default()));
        assert_ne!(without.arguments, empty.arguments);
    }

    #[test]
    fn the_method_name_is_protocol_neutral() {
        assert_eq!(IrMethod::Call.name(), "call");
        assert_eq!(IrMethod::Call.to_string(), "call");
    }

    #[test]
    fn an_outcome_defaults_to_a_success_with_no_content() {
        let outcome = IrOutcome::default();
        assert!(outcome.content.is_empty());
        assert!(outcome.structured.is_none());
        assert!(!outcome.is_error);
    }

    #[test]
    fn an_outcome_can_be_marked_failed_and_carry_a_structured_payload() {
        let outcome = IrOutcome::new(Vec::new())
            .with_structured(JsonObject::default())
            .with_error(true);
        assert!(outcome.is_error);
        assert!(outcome.structured.is_some());
    }
}
