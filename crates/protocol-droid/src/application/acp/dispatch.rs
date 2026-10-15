//! The ACP request handler trait — the consumer's
//! extension point for ACP JSON-RPC methods.
//!
//! # What this is
//!
//! A consumer who runs an ACP agent implements
//! [`AcpRequestHandler`] to define what each ACP
//! method does. The serving adapter
//! (the agent's `AcpAgentPeer` paired with a
//! matching JSON-RPC dispatcher) decodes the
//! JSON-RPC envelope, looks up the method name
//! in the [`AgentMethod`](crate::domain::acp::method::AgentMethod) table, and calls the
//! handler with the matched method, the raw
//! params, and the per-request context. The
//! handler returns the raw result (or an
//! [`AcpError`]); the serving adapter serialises
//! the result into a JSON-RPC response.
//!
//! # Why a single dispatch method
//!
//! The ACP v1.25.0 spec defines 13 agent-side
//! methods. A trait with 13 typed methods is
//! exhaustive (a handler must implement all
//! 13) and locks the wire shape of every method
//! at the type level. A single dispatch method
//! lets a consumer implement only the methods
//! they care about and return
//! [`AcpError::MethodNotImplemented`] for the
//! rest, which is what the early-adopter story
//! looks like in practice: an agent starts with
//! the three or four methods it needs (`initialize`,
//! `session/new`, `session/prompt`, `session/cancel`),
//! and adds the others as clients ask.
//!
//! The trade-off is no per-method compile-time
//! type safety on the params. The params arrive
//! as [`serde_json::Value`]; the consumer is
//! responsible for decoding them. A future
//! per-method typed wrapper is a separate
//! commit (the [`super::typed_dispatch`] module),
//! not a replacement for this trait.
//!
//! # Why this is object-safe
//!
//! The serving adapter is generic over a concrete
//! handler in the call site, but a consumer who
//! wants to swap handlers at runtime holds an
//! `Arc<dyn AcpRequestHandler>`. Object safety
//! keeps both options open. The trade-off is the
//! boxed future on the per-call path: the JSON-RPC
//! dispatch itself is the project's hot path, not
//! the handler invocation. The
//! `Pin<Box<dyn Future + Send>>` signature
//! matches [`A2aRequestHandler`](super::super::a2a::A2aRequestHandler)
//! in spirit but is a single concrete shape,
//! not RPITIT, because `async fn` in a trait
//! does not make the resulting future
//! object-safe in 1.98.

use core::fmt;
use core::pin::Pin;

use futures_core::Future;
use serde_json::Value;

use crate::domain::acp::method::AgentMethod;

/// An ACP-specific error.
///
/// The variants are the JSON-RPC error codes the
/// ACP spec names. The serving adapter maps each
/// variant to a code and message on the wire; the
/// consumer never has to know the codes.
///
/// # JSON-RPC error codes
///
/// - `-32601`: `MethodNotFound` / `MethodNotImplemented`
/// - `-32602`: `InvalidParams`
/// - `-32603`: `InternalError`
///
/// The negative numbers are the JSON-RPC
/// standard's reserved range. ACP v1.25.0
/// does not extend the implementation-defined
/// `-32000..=-32099` range, so this enum has
/// the same surface as the JSON-RPC standard's
/// own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpError {
    /// The method name is not in the ACP
    /// vocabulary. Maps to JSON-RPC `-32601`.
    MethodNotFound,

    /// The method is known to ACP but the
    /// consumer's handler does not implement it.
    /// Maps to JSON-RPC `-32601` (the same code
    /// as `MethodNotFound`; the distinction is
    /// for the consumer's logs).
    MethodNotImplemented {
        /// The method the consumer did not
        /// implement.
        method: AgentMethod,
    },

    /// The params could not be decoded into the
    /// shape the handler expects. Maps to JSON-RPC
    /// `-32602`.
    InvalidParams {
        /// Why the params are wrong. The serving
        /// adapter surfaces this on the wire as
        /// the JSON-RPC `data` field.
        reason: String,
    },

    /// The handler failed in a way that is not
    /// the caller's fault. Maps to JSON-RPC
    /// `-32603`.
    InternalError {
        /// A short, human-readable description of
        /// what went wrong. Goes on the wire as
        /// the JSON-RPC `message` field.
        message: String,
    },
}

impl AcpError {
    /// The JSON-RPC error code for this error.
    ///
    /// The codes are the JSON-RPC standard's
    /// reserved range; ACP v1.25.0 does not
    /// extend the implementation-defined
    /// `-32000..=-32099` range.
    #[must_use]
    pub const fn code(&self) -> i32 {
        match self {
            Self::MethodNotFound | Self::MethodNotImplemented { .. } => -32601,
            Self::InvalidParams { .. } => -32602,
            Self::InternalError { .. } => -32603,
        }
    }

    /// The JSON-RPC error message for this error.
    ///
    /// A short, fixed string per the JSON-RPC
    /// standard; detailed context goes in the
    /// `data` field (the serving adapter's
    /// responsibility, not this one).
    #[must_use]
    pub const fn message(&self) -> &'static str {
        match self {
            Self::MethodNotFound => "Method not found",
            Self::MethodNotImplemented { .. } => "Method not implemented",
            Self::InvalidParams { .. } => "Invalid params",
            Self::InternalError { .. } => "Internal error",
        }
    }
}

impl fmt::Display for AcpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MethodNotFound => formatter.write_str("method not found"),
            Self::MethodNotImplemented { method } => {
                // The wire form of the method is
                // what the consumer's log line
                // wants — `session/prompt`, not
                // `SessionPrompt`. `AgentMethod`
                // does not implement `Display` (the
                // canonical wire form is the
                // `as_wire_name` method), so we
                // call it here.
                write!(
                    formatter,
                    "method not implemented: {}",
                    method.as_wire_name()
                )
            }
            Self::InvalidParams { reason } => {
                write!(formatter, "invalid params: {reason}")
            }
            Self::InternalError { message } => {
                write!(formatter, "internal error: {message}")
            }
        }
    }
}

impl std::error::Error for AcpError {}

/// The per-request context the ACP handler
/// receives.
///
/// Carries the negotiated protocol version (so
/// the handler can decode per-revision params
/// against it) and the auth context (so a
/// handler that needs to check identity has it
/// without re-parsing the wire envelope). Future
/// fields land here as the protocol grows: the
/// request id, the request deadline, the session
/// id extracted from the params (where it is
/// always present), the cancellation handle for
/// the in-flight prompt.
#[derive(Debug, Clone)]
pub struct AcpRequestContext {
    /// The negotiated ACP protocol version. The
    /// handler is responsible for decoding params
    /// against this version's wire shape.
    pub protocol_version: crate::domain::acp::protocol_version::AcpProtocolVersion,
}

/// The consumer's extension point for ACP
/// methods.
///
/// The serving adapter calls [`dispatch`](Self::dispatch)
/// once per JSON-RPC request. The handler receives
/// the method (already validated against the
/// ACP vocabulary) and the raw `serde_json::Value`
/// params. The handler returns the raw result
/// value, or an [`AcpError`].
///
/// # Object safety
///
/// The trait is object-safe. A consumer who wants
/// to swap handlers at runtime holds an
/// `Arc<dyn AcpRequestHandler>`; the serving
/// adapter accepts either an `Arc<dyn>` or a
/// concrete `H: AcpRequestHandler`.
///
/// # Default impls
///
/// None of the methods have a default impl. A
/// consumer who wants to leave a method
/// unimplemented can return
/// [`AcpError::MethodNotImplemented`] from
/// `dispatch`; the serving adapter maps that to
/// JSON-RPC `-32601` and the caller's client
/// knows the method is not supported.
///
/// # Example
///
/// A minimal handler that implements only
/// `initialize` and returns
/// `AcpError::MethodNotImplemented` for the
/// other twelve methods. The full reference
/// impl is in a future `acp_agent` example.
///
/// ```no_run
/// use std::pin::Pin;
/// use std::sync::Arc;
///
/// use futures_core::Future;
/// use serde_json::{json, Value};
///
/// use protocol_droid::domain::acp::method::AgentMethod;
/// use protocol_droid::application::acp::{
///     AcpError, AcpRequestContext, AcpRequestHandler,
/// };
///
/// struct InitializeOnlyAgent;
///
/// impl AcpRequestHandler for InitializeOnlyAgent {
///     fn dispatch(
///         &self,
///         method: AgentMethod,
///         _params: Option<Value>,
///         _context: &AcpRequestContext,
///     ) -> Pin<
///         Box<dyn Future<Output = Result<Value, AcpError>> + Send + '_>,
///     > {
///         Box::pin(async move {
///             match method {
///                 AgentMethod::Initialize => {
///                     // Run the agent's initialize
///                     // handshake, return the
///                     // response as JSON.
///                     Ok(json!({"protocolVersion": 1, "agentInfo": {}}))
///                 }
///                 other => Err(AcpError::MethodNotImplemented {
///                     method: other,
///                 }),
///             }
///         })
///     }
/// }
///
/// let _handler: Arc<dyn AcpRequestHandler> = Arc::new(InitializeOnlyAgent);
/// ```
pub trait AcpRequestHandler: Send + Sync + 'static {
    /// Handles a routed ACP method.
    ///
    /// The serving adapter guarantees:
    /// - `method` was looked up in the ACP
    ///   vocabulary (so it is one of the 13
    ///   agent methods, not a string);
    /// - `params` is the raw JSON-RPC params
    ///   object, or `None` if the request had no
    ///   params.
    ///
    /// The handler is responsible for:
    /// - Decoding `params` into the per-method
    ///   type;
    /// - Running the method's logic;
    /// - Returning either the raw result value
    ///   (which the serving adapter puts in the
    ///   `result` field) or an [`AcpError`]
    ///   (which becomes the `error` field).
    ///
    /// # Errors
    ///
    /// Any [`AcpError`]. The serving adapter
    /// answers with the right JSON-RPC code;
    /// the handler never has to know the code.
    fn dispatch(
        &self,
        method: AgentMethod,
        params: Option<Value>,
        context: &AcpRequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AcpError>> + Send + '_>>;
}

#[cfg(test)]
mod tests {
    use super::AcpError;
    use crate::domain::acp::method::AgentMethod;

    #[test]
    fn method_not_found_is_minus_32601() {
        assert_eq!(AcpError::MethodNotFound.code(), -32601);
        assert_eq!(AcpError::MethodNotFound.message(), "Method not found");
    }

    #[test]
    fn method_not_implemented_is_minus_32601() {
        // Same code as `MethodNotFound`; the
        // distinction is in the message.
        let err = AcpError::MethodNotImplemented {
            method: AgentMethod::SessionPrompt,
        };
        assert_eq!(err.code(), -32601);
        assert_eq!(err.message(), "Method not implemented");
    }

    #[test]
    fn invalid_params_is_minus_32602() {
        let err = AcpError::InvalidParams {
            reason: "expected an object".to_owned(),
        };
        assert_eq!(err.code(), -32602);
        assert_eq!(err.message(), "Invalid params");
    }

    #[test]
    fn internal_error_is_minus_32603() {
        let err = AcpError::InternalError {
            message: "session store is down".to_owned(),
        };
        assert_eq!(err.code(), -32603);
        assert_eq!(err.message(), "Internal error");
    }

    #[test]
    fn display_distinguishes_the_two_minus_32601_variants() {
        let not_found = AcpError::MethodNotFound;
        let not_impl = AcpError::MethodNotImplemented {
            method: AgentMethod::SessionPrompt,
        };
        // The two are the same on the wire; the
        // distinction is the consumer's log
        // line.
        assert_ne!(not_found.to_string(), not_impl.to_string());
    }
}
