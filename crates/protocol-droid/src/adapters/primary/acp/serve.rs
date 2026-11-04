//! Serving an ACP agent over the stdio transport.
//!
//! # What this is
//!
//! [`serve_acp_agent`] runs the agent side of the ACP
//! loop: it reads the editor's requests off the wire,
//! routes each one to the consumer's
//! [`AcpRequestHandler`], and writes the reply back.
//!
//! The consumer writes one thing — the handler — and
//! gets a running agent:
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! use protocol_droid::adapters::primary::acp::serve::serve_acp_agent;
//! use protocol_droid::adapters::secondary::acp::AcpAgentPeer;
//! use protocol_droid::application::acp::AcpRequestHandler;
//! use protocol_droid::domain::acp::method::AgentMethod;
//!
//! # struct MyAgent;
//! # impl AcpRequestHandler for MyAgent {
//! #     fn dispatch(
//! #         &self,
//! #         _: AgentMethod,
//! #         _: Option<serde_json::Value>,
//! #         _: &protocol_droid::application::acp::AcpRequestContext,
//! #     ) -> std::pin::Pin<Box<dyn futures_core::Future<
//! #         Output = Result<serde_json::Value,
//! #             protocol_droid::application::acp::AcpError>,
//! #     > + Send + '_>>
//! #     { Box::pin(async { Ok(serde_json::json!({})) }) }
//! # }
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let peer = AcpAgentPeer::from_stdio(tokio::io::stdin(), tokio::io::stdout());
//! serve_acp_agent(peer, Arc::new(MyAgent)).await?;
//! # Ok(())
//! # }
//! ```
//!
//! # The two halves
//!
//! ACP is bidirectional, and the two halves have
//! different shapes:
//!
//! - The **editor to agent** direction is a request /
//!   reply loop. The editor sends `initialize`,
//!   `session/new`, `session/prompt`; the agent
//!   answers. This is what [`serve_acp_agent`] drives.
//!
//! - The **agent to editor** direction is the agent
//!   calling back: `session/request_permission`,
//!   `fs/read_text_file`, `terminal/create`. The
//!   agent needs these mid-`session/prompt`, while
//!   the request loop is parked awaiting the next
//!   request.
//!
//! The call-back direction reaches the handler
//! through [`AcpRequestContext::editor`], which
//! carries the agent's outbound handle. The handler
//! calls the editor through it.
//!
//! # Why the sender is in the context and not the
//! handler
//!
//! A handler that owns its own sender would have to be
//! constructed with one, which means the consumer
//! wires the transport into the handler by hand. The
//! context already exists as the per-request carrier,
//! and the serving adapter is the only thing that
//! knows the transport — so the adapter puts the
//! sender there.
//!
//! # Errors
//!
//! [`serve_acp_agent`] returns [`AcpServeError`]. The
//! two cases are a wire the editor closed (not a
//! failure — the editor quit) and a write that failed
//! (the pipe broke).

use std::sync::Arc;

use serde_json::Value;

use crate::adapters::secondary::acp::{AcpAgentPeer, AcpAgentSender, AcpPeerError};
use crate::application::acp::dispatch::{AcpError, AcpRequestContext, AcpRequestHandler};
use crate::application::acp::editor::{AcpEditorCaller, AcpEditorError};
use crate::domain::acp::method::{AcpMethod, ClientMethod};
use crate::domain::acp::protocol_version::AcpProtocolVersion;

/// The error returned by [`serve_acp_agent`].
#[derive(Debug, thiserror::Error)]
pub enum AcpServeError {
    /// The editor closed its end of the wire. The
    /// agent's loop ends; this is not a failure.
    #[error("the editor closed the connection")]
    EditorClosed,

    /// Writing a reply to the editor failed. The pipe
    /// is broken and the agent cannot continue.
    #[error("writing to the editor failed: {0}")]
    Write(#[source] AcpPeerError),
}

/// Run the agent side of the ACP loop until the
/// editor closes the connection.
///
/// The function reads each request off the peer,
/// routes it to the handler, and writes the reply.
/// A handler that returns an [`AcpError`] produces a
/// JSON-RPC error reply with the matching code — the
/// handler never has to know the code.
///
/// The agent's outbound handle is placed in the
/// [`AcpRequestContext`], so the handler can call the
/// editor back (`session/request_permission`,
/// `fs/read_text_file`, …) while this loop is parked.
///
/// # Errors
///
/// - [`AcpServeError::EditorClosed`] — the editor
///   closed the connection. The loop ends normally in
///   this case; the error is returned so the caller
///   can distinguish "the editor quit" from "the
///   agent crashed".
/// - [`AcpServeError::Write`] — a reply could not be
///   written. The pipe is broken.
///
/// # Example
///
/// See the module docs for a complete runnable
/// shape.
pub async fn serve_acp_agent<H>(peer: AcpAgentPeer, handler: Arc<H>) -> Result<(), AcpServeError>
where
    H: AcpRequestHandler + ?Sized + 'static,
{
    serve_acp_agent_with_version(peer, handler, AcpProtocolVersion::CURRENT).await
}

/// Run the agent side of the ACP loop with a
/// caller-supplied protocol version.
///
/// [`serve_acp_agent`] is the front door; this is the
/// same loop with the version the caller wants to
/// hand the handler. A consumer that negotiates the
/// version itself — or that wants to pin a version
/// other than the current one — uses this.
///
/// # Errors
///
/// The same set as [`serve_acp_agent`].
pub async fn serve_acp_agent_with_version<H>(
    peer: AcpAgentPeer,
    handler: Arc<H>,
    protocol_version: AcpProtocolVersion,
) -> Result<(), AcpServeError>
where
    H: AcpRequestHandler + ?Sized + 'static,
{
    let context = AcpRequestContext::new(
        protocol_version,
        Arc::new(PeerEditorCaller {
            sender: peer.sender(),
        }),
    );
    serve_acp_agent_with_context(peer, handler, context).await
}

/// Run the agent side of the ACP loop with a
/// caller-supplied context.
///
/// The most general form: the caller supplies the
/// whole context, so it controls the protocol version
/// *and* the editor port. A consumer that wires its
/// own editor port — a test double, or a port that
/// records the calls the handler makes — uses this.
///
/// # Errors
///
/// The same set as [`serve_acp_agent`].
pub async fn serve_acp_agent_with_context<H>(
    mut peer: AcpAgentPeer,
    handler: Arc<H>,
    context: AcpRequestContext,
) -> Result<(), AcpServeError>
where
    H: AcpRequestHandler + ?Sized + 'static,
{
    loop {
        let Some(request) = peer.recv_request().await else {
            // The editor closed its end of the wire.
            return Err(AcpServeError::EditorClosed);
        };

        // The method name is the routing key. An
        // unknown name is a wire violation, not a
        // handler bug, so it is answered with
        // JSON-RPC `-32601` rather than dispatched.
        let Ok(AcpMethod::Agent(method)) = request.method.parse::<AcpMethod>() else {
            peer.send_error(
                request.id,
                i64::from(AcpError::MethodNotFound.code()),
                AcpError::MethodNotFound.message(),
            )
            .await
            .map_err(AcpServeError::Write)?;
            continue;
        };

        let result = handler
            .dispatch(method, Some(request.params), &context)
            .await;

        match result {
            Ok(value) => {
                peer.send_reply(request.id, value)
                    .await
                    .map_err(AcpServeError::Write)?;
            }
            Err(error) => {
                peer.send_error(request.id, i64::from(error.code()), error.message())
                    .await
                    .map_err(AcpServeError::Write)?;
            }
        }
    }
}

/// The [`AcpEditorCaller`] backed by the agent peer's
/// outbound half.
///
/// The adapter's side of the port. The application
/// layer declares the trait; this is the one
/// implementation the crate ships.
struct PeerEditorCaller {
    /// The agent's outbound handle. Cloned into the
    /// caller so the peer keeps its own copy — the
    /// serving loop holds the peer, and the caller
    /// has to be usable from inside a handler.
    sender: AcpAgentSender,
}

impl AcpEditorCaller for PeerEditorCaller {
    fn request(
        &self,
        method: ClientMethod,
        params: Value,
    ) -> std::pin::Pin<
        Box<dyn futures_core::Future<Output = Result<Value, AcpEditorError>> + Send + '_>,
    > {
        Box::pin(async move {
            let reply = self
                .sender
                .send_request(AcpMethod::Client(method), params)
                .await
                .map_err(map_peer_error)?;
            Ok(reply.result)
        })
    }

    fn notify(
        &self,
        method: ClientMethod,
        params: Value,
    ) -> std::pin::Pin<Box<dyn futures_core::Future<Output = Result<(), AcpEditorError>> + Send + '_>>
    {
        Box::pin(async move {
            self.sender
                .send_notification(method.as_wire_name(), params)
                .await
                .map_err(map_peer_error)
        })
    }
}

/// Map a transport failure onto the port's error type.
///
/// The application layer never names a transport error,
/// so the adapter translates at the boundary.
pub(super) fn map_peer_error(error: AcpPeerError) -> AcpEditorError {
    match error {
        AcpPeerError::JsonRpc { code, message } => AcpEditorError::JsonRpc { code, message },
        AcpPeerError::Decode(error) => AcpEditorError::Decode {
            reason: error.to_string(),
        },
        other => AcpEditorError::Disconnected {
            reason: other.to_string(),
        },
    }
}
