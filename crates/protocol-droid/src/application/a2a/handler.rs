//! The A2A request handler trait — the consumer's
//! extension point for A2A JSON-RPC methods.
//!
//! # What this is
//!
//! A consumer who runs an A2A server implements
//! [`A2aRequestHandler`] to define what each A2A
//! method does. The serving adapter
//! ([`serve_jsonrpc`](crate::adapters::primary::a2a::jsonrpc_serve::serve_jsonrpc))
//! decodes the JSON-RPC envelope, looks up the
//! method name in the [`A2aMethodTable`](crate::adapters::secondary::a2a::A2aMethodTable),
//! and calls the handler with the matched method,
//! the raw params, and the per-request context.
//! The handler returns the raw result (or an
//! [`A2aError`]); the serving adapter serialises
//! the result into a JSON-RPC response.
//!
//! # Why a single dispatch method
//!
//! The A2A v1.0 spec defines 11 methods. A trait
//! with 11 typed methods is exhaustive (a handler
//! must implement all 11) and locks the wire
//! shape of every method at the type level. A
//! single dispatch method lets a consumer implement
//! only the methods they care about and return
//! [`A2aError::MethodNotImplemented`] for the rest,
//! which is what the 0.3.x → 1.0 migration story
//! looks like in practice: a server starts with
//! the two or three methods it needs, and adds
//! the others as consumers ask.
//!
//! The trade-off is no per-method compile-time
//! type safety on the params. The params arrive
//! as [`serde_json::Value`]; the consumer is
//! responsible for decoding them. A future
//! per-method typed wrapper is a separate
//! commit, not a replacement for this trait.
//!
//! # Why this is object-safe
//!
//! The serving adapter is generic over a concrete
//! handler in the call site, but a consumer who
//! wants to swap handlers at runtime holds an
//! `Arc<dyn A2aRequestHandler>`. Object safety
//! keeps both options open. The trade-off is the
//! boxed future on the per-call path: the JSON-RPC
//! dispatch itself is the project's hot path, not
//! the handler invocation. The
//! `Pin<Box<dyn Future + Send>>` signature
//! matches [`RequestHandler`](crate::adapters::primary::transport::http::router::RequestHandler)
//! in spirit but is a single concrete shape,
//! not RPITIT, because `async fn` in a trait
//! does not make the resulting future
//! object-safe in 1.98.

use std::pin::Pin;

use futures_core::Future;

use crate::adapters::secondary::a2a::A2aMethod;
use crate::domain::a2a::version::A2aVersion;

/// An A2A-specific error.
///
/// The variants are the JSON-RPC error codes the
/// A2A spec names. The serving adapter maps each
/// variant to a code and message on the wire; the
/// consumer never has to know the codes.
///
/// # JSON-RPC error codes
///
/// - `-32601`: `MethodNotFound`
/// - `-32602`: `InvalidParams`
/// - `-32603`: `InternalError`
/// - `-32000`: `ContentTypeNotSupported`
/// - `-32001`: `AuthenticationRequired`
/// - `-32002`: `AuthenticationFailed`
///
/// The negative numbers are the JSON-RPC
/// standard's reserved range for "implementation-
/// defined server errors"; A2A v1.0 §5.3 fixes
/// these specific codes. The standard
/// JSON-RPC error codes (`-32700` `ParseError`,
/// `-32600` `Invalid Request`, `-32601`
/// `Method Not Found`, `-32602` `Invalid
/// Params`, `-32603` `Internal Error`) are
/// reserved by JSON-RPC itself; the A2A spec
/// assigns them and extends the
/// implementation-defined range to `-32000`
/// through `-32002`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum A2aError {
    /// The method name is not in the A2A
    /// vocabulary. Maps to JSON-RPC `-32601`.
    MethodNotFound,

    /// The method is known to A2A but the
    /// consumer's handler does not implement it.
    /// Maps to JSON-RPC `-32601` (the same code
    /// as `MethodNotFound`; the distinction is
    /// for the consumer's logs).
    MethodNotImplemented {
        /// The method the consumer did not
        /// implement.
        method: A2aMethod,
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
        /// The error message. The serving adapter
        /// surfaces this on the wire as the
        /// JSON-RPC `message` field.
        message: String,
    },

    /// The request requires authentication and
    /// the request did not carry credentials.
    /// Maps to JSON-RPC `-32001`.
    AuthenticationRequired,

    /// The request carried credentials and they
    /// were not accepted. Maps to JSON-RPC
    /// `-32002`.
    AuthenticationFailed {
        /// Why the credentials were rejected.
        reason: String,
    },

    /// The request's `Content-Type` is not
    /// `application/json`. Maps to JSON-RPC
    /// `-32000`.
    ContentTypeNotSupported {
        /// The `Content-Type` the request carried.
        received: String,
    },
}

impl A2aError {
    /// The JSON-RPC error code for this error.
    ///
    /// The codes are the A2A v1.0 spec's
    /// implementation-defined server-error range.
    /// The standard JSON-RPC error codes
    /// (`-32700` `ParseError`, `-32600` `Invalid`
    /// `Request`, `-32601` `Method` `Not` `Found`,
    /// `-32602` `Invalid` `Params`, `-32603`
    /// `Internal` `Error`) are reserved by
    /// JSON-RPC itself; the A2A spec assigns them
    /// and extends the implementation-defined
    /// range to `-32000` through `-32002`.
    #[must_use]
    pub const fn code(&self) -> i32 {
        match self {
            Self::MethodNotFound | Self::MethodNotImplemented { .. } => -32601,
            Self::InvalidParams { .. } => -32602,
            Self::InternalError { .. } => -32603,
            Self::ContentTypeNotSupported { .. } => -32000,
            Self::AuthenticationRequired => -32001,
            Self::AuthenticationFailed { .. } => -32002,
        }
    }

    /// The JSON-RPC error message for this error.
    ///
    /// A short, fixed string per the A2A spec;
    /// detailed context goes in the `data` field
    /// (the serving adapter's responsibility, not
    /// this one).
    #[must_use]
    pub const fn message(&self) -> &'static str {
        match self {
            Self::MethodNotFound => "Method not found",
            Self::MethodNotImplemented { .. } => "Method not implemented",
            Self::InvalidParams { .. } => "Invalid params",
            Self::InternalError { .. } => "Internal error",
            Self::ContentTypeNotSupported { .. } => "Content-Type not supported",
            Self::AuthenticationRequired => "Authentication required",
            Self::AuthenticationFailed { .. } => "Authentication failed",
        }
    }
}

/// The per-request context the A2A handler
/// receives.
///
/// Carries the negotiated protocol version (so
/// the handler can decode per-revision params)
/// and the auth context (so a handler that needs
/// to check identity has it without re-parsing
/// headers). Future fields land here as the
/// protocol grows: the request id, the request
/// deadline, the tenant id from the URL.
#[derive(Debug, Clone)]
pub struct A2aRequestContext {
    /// The negotiated A2A protocol version. The
    /// handler is responsible for decoding params
    /// against this version's wire shape.
    pub protocol_version: A2aVersion,
}

/// The consumer's extension point for A2A methods.
///
/// The serving adapter calls [`dispatch`](Self::dispatch)
/// once per JSON-RPC request. The handler receives
/// the method (already validated against the
/// A2A vocabulary) and the raw `serde_json::Value`
/// params. The handler returns the raw result
/// value, or an [`A2aError`].
///
/// # Object safety
///
/// The trait is object-safe. A consumer who wants
/// to swap handlers at runtime holds an
/// `Arc<dyn A2aRequestHandler>`; the serving
/// adapter accepts either an `Arc<dyn>` or a
/// concrete `H: A2aRequestHandler`.
///
/// # Default impls
///
/// None of the methods have a default impl. A
/// consumer who wants to leave a method
/// unimplemented can return
/// [`A2aError::MethodNotImplemented`] from
/// `dispatch`; the serving adapter maps that to
/// JSON-RPC `-32601` and the caller's client
/// knows the method is not supported.
pub trait A2aRequestHandler: Send + Sync + 'static {
    /// Handles a routed A2A method.
    ///
    /// The serving adapter guarantees:
    /// - `method` was looked up in the A2A
    ///   vocabulary (so it is one of the 11
    ///   methods, not a string);
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
    ///   `result` field) or an [`A2aError`]
    ///   (which becomes the `error` field).
    ///
    /// # Errors
    ///
    /// Any [`A2aError`]. The serving adapter
    /// answers with the right JSON-RPC code;
    /// the handler never has to know the code.
    fn dispatch(
        &self,
        method: A2aMethod,
        params: Option<serde_json::Value>,
        context: &A2aRequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, A2aError>> + Send + '_>>;
}

#[cfg(test)]
mod tests {
    use super::A2aError;
    use crate::adapters::secondary::a2a::A2aMethod;

    #[test]
    fn method_not_found_is_minus_32601() {
        assert_eq!(A2aError::MethodNotFound.code(), -32601);
    }

    #[test]
    fn method_not_implemented_is_minus_32601() {
        // Same code as `MethodNotFound`; the
        // distinction is in the message.
        let err = A2aError::MethodNotImplemented {
            method: A2aMethod::SendMessage,
        };
        assert_eq!(err.code(), -32601);
        assert_eq!(err.message(), "Method not implemented");
    }

    #[test]
    fn invalid_params_is_minus_32602() {
        let err = A2aError::InvalidParams {
            reason: "expected an object".to_owned(),
        };
        assert_eq!(err.code(), -32602);
    }

    #[test]
    fn internal_error_is_minus_32603() {
        let err = A2aError::InternalError {
            message: "database is down".to_owned(),
        };
        assert_eq!(err.code(), -32603);
    }

    #[test]
    fn content_type_not_supported_is_minus_32000() {
        let err = A2aError::ContentTypeNotSupported {
            received: "text/plain".to_owned(),
        };
        assert_eq!(err.code(), -32000);
    }

    #[test]
    fn authentication_required_is_minus_32001() {
        assert_eq!(A2aError::AuthenticationRequired.code(), -32001);
    }

    #[test]
    fn authentication_failed_is_minus_32002() {
        let err = A2aError::AuthenticationFailed {
            reason: "token expired".to_owned(),
        };
        assert_eq!(err.code(), -32002);
    }

    #[test]
    fn messages_are_stable_strings() {
        // The spec fixes the message text;
        // changing any of these is a wire break.
        assert_eq!(A2aError::MethodNotFound.message(), "Method not found");
        assert_eq!(
            A2aError::InternalError {
                message: "x".to_owned()
            }
            .message(),
            "Internal error"
        );
        assert_eq!(
            A2aError::ContentTypeNotSupported {
                received: "x".to_owned()
            }
            .message(),
            "Content-Type not supported"
        );
        assert_eq!(
            A2aError::AuthenticationRequired.message(),
            "Authentication required"
        );
    }
}
