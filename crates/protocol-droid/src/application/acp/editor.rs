//! The port an ACP handler uses to call the editor back.
//!
//! # Why a handler needs this
//!
//! ACP is bidirectional. The editor calls the agent
//! (`initialize`, `session/new`, `session/prompt`), and
//! the agent calls the editor back (`session/request_permission`,
//! `fs/read_text_file`, `terminal/create`).
//!
//! The call-back direction is not optional. A
//! `session/prompt` turn almost always needs it: the
//! agent wants to run a tool, so it asks the editor's
//! user for permission; the agent wants a file's
//! contents, so it asks the editor.
//!
//! # Why the handler cannot do it directly
//!
//! The handler runs while the serving loop is parked
//! awaiting the next request. The loop holds the peer;
//! the handler cannot borrow it. So the loop hands the
//! handler a *separate* handle — one that can write to
//! the editor but not read from it — and that handle is
//! what this trait describes.
//!
//! The trait lives in the application layer because the
//! handler is its consumer. The adapter implements it.
//!
//! # Errors
//!
//! A call fails when the editor does not answer: the
//! connection dropped, the editor sent a JSON-RPC
//! error, or the reply was not the shape the method
//! expects. [`AcpEditorError`] says which.

use core::pin::Pin;

use futures_core::Future;
use serde_json::Value;

use crate::domain::acp::method::ClientMethod;

/// The failure of an agent-to-editor call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AcpEditorError {
    /// The editor answered with a JSON-RPC error.
    ///
    /// The code and message are the editor's own — the
    /// agent did not invent them.
    #[error("the editor answered with an error: {code}: {message}")]
    JsonRpc {
        /// The JSON-RPC error code the editor sent.
        code: i64,
        /// The JSON-RPC error message the editor sent.
        message: String,
    },

    /// The editor's reply did not have the shape the
    /// method's response type expects.
    #[error("the editor's reply did not decode: {reason}")]
    Decode {
        /// Why the reply could not be decoded.
        reason: String,
    },

    /// The connection to the editor is gone.
    ///
    /// The editor closed its end of the wire, or the
    /// pipe broke.
    #[error("the editor is gone: {reason}")]
    Disconnected {
        /// What was observed when the connection failed.
        reason: String,
    },
}

/// The editor-facing port a handler uses to call the
/// editor back.
///
/// The trait is the consumer's — the handler owns the
/// capability, the adapter supplies it. A handler
/// receives one through the
/// [`AcpRequestContext`](super::AcpRequestContext).
///
/// # Example
///
/// A handler that asks the editor's user for permission
/// before running a tool:
///
/// ```no_run
/// use std::pin::Pin;
///
/// use futures_core::Future;
/// use serde_json::{json, Value};
///
/// use protocol_droid::application::acp::{
///     AcpEditorCaller, AcpEditorError, AcpError,
///     AcpRequestContext, AcpRequestHandler,
/// };
/// use protocol_droid::domain::acp::method::{
///     AgentMethod, ClientMethod,
/// };
///
/// struct PermissioningAgent;
///
/// impl AcpRequestHandler for PermissioningAgent {
///     fn dispatch<'a>(
///         &'a self,
///         method: AgentMethod,
///         _params: Option<Value>,
///         context: &'a AcpRequestContext,
///     ) -> Pin<
///         Box<dyn Future<Output = Result<Value, AcpError>> + Send + 'a>,
///     > {
///         Box::pin(async move {
///             match method {
///                 AgentMethod::SessionPrompt => {
///                     // Ask the editor's user.
///                     let reply = context
///                         .editor()
///                         .request(
///                             ClientMethod::SessionRequestPermission,
///                             json!({"toolCall": {}}),
///                         )
///                         .await
///                         .map_err(|error| AcpError::InternalError {
///                             message: error.to_string(),
///                         })?;
///                     Ok(reply)
///                 }
///                 _ => Err(AcpError::MethodNotImplemented { method }),
///             }
///         })
///     }
/// }
/// ```
pub trait AcpEditorCaller: Send + Sync + 'static {
    /// Call an editor method and await its reply.
    ///
    /// The eleven [`ClientMethod`]s are all issued this
    /// way except the two that are notifications on the
    /// wire (`session/update` and `elicitation/complete`)
    /// — those go through
    /// [`notify`](Self::notify) instead, because the
    /// editor sends no reply for them.
    ///
    /// # Errors
    ///
    /// - [`AcpEditorError::JsonRpc`] — the editor
    ///   answered with a JSON-RPC error.
    /// - [`AcpEditorError::Decode`] — the reply did not
    ///   decode into the method's response type.
    /// - [`AcpEditorError::Disconnected`] — the editor
    ///   is gone.
    fn request(
        &self,
        method: ClientMethod,
        params: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AcpEditorError>> + Send + '_>>;

    /// Send the editor a notification.
    ///
    /// The editor sends no reply, so this returns as
    /// soon as the bytes are queued. The two
    /// notification-shaped client methods are
    /// `session/update` (the agent streaming a turn
    /// update) and `elicitation/complete` (the agent
    /// signalling that an elicitation finished).
    ///
    /// # Errors
    ///
    /// - [`AcpEditorError::Disconnected`] — the editor
    ///   is gone.
    fn notify(
        &self,
        method: ClientMethod,
        params: Value,
    ) -> Pin<Box<dyn Future<Output = Result<(), AcpEditorError>> + Send + '_>>;
}
