//! Tests for the ACP serving adapter.
//!
//! The adapter's job is a round trip: the editor's
//! request arrives, the handler answers, the reply goes
//! back on the wire. So the tests drive a real duplex
//! pair and assert on the bytes that come out — an
//! IR-level assertion would pass even when the wire form
//! is wrong, which is the failure mode that matters here.
//!
//! The second thing the adapter has to get right is the
//! agent-to-editor direction: the handler calls the
//! editor back *while the serving loop is parked*. That
//! is a deadlock waiting to happen if the two halves
//! share a lock, so the test that covers it is the one
//! that matters most.

use std::pin::Pin;
use std::sync::Arc;

use futures_core::Future;
use serde_json::Value;
use tokio::io::duplex;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::serve::{AcpServeError, map_peer_error, serve_acp_agent, serve_acp_agent_with_context};
use crate::adapters::secondary::acp::{AcpAgentPeer, AcpPeerError};
use crate::application::acp::dispatch::{AcpError, AcpRequestContext, AcpRequestHandler};
use crate::application::acp::editor::{AcpEditorCaller, AcpEditorError};
use crate::domain::acp::method::{AgentMethod, ClientMethod};
use crate::domain::acp::protocol_version::AcpProtocolVersion;

/// The failure modes the tests can produce.
#[derive(Debug)]
enum Failure {
    /// A test double read a line that was not the
    /// JSON the test expected.
    BadLine(String),
    /// The editor end of the duplex could not be
    /// written to or read from.
    Io(String),
    /// A test double saw an outcome it did not
    /// expect.
    WrongOutcome(String),
    /// The serving task itself failed.
    Join(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadLine(line) => write!(formatter, "unexpected line: {line}"),
            Self::Io(reason) => write!(formatter, "io failure: {reason}"),
            Self::WrongOutcome(reason) => write!(formatter, "wrong outcome: {reason}"),
            Self::Join(reason) => write!(formatter, "the serving task failed: {reason}"),
        }
    }
}

impl std::error::Error for Failure {}

impl From<std::io::Error> for Failure {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

impl From<tokio::task::JoinError> for Failure {
    fn from(error: tokio::task::JoinError) -> Self {
        Self::Join(error.to_string())
    }
}

/// Read one line from the editor end of the duplex and
/// parse it as JSON.
///
/// Returns `None` when the peer closed its end without
/// writing a line.
async fn read_envelope(
    reader: &mut BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
) -> Result<Option<Value>, Failure> {
    let mut line = String::new();
    let read = reader.read_line(&mut line).await?;
    if read == 0 {
        return Ok(None);
    }
    let trimmed = line.trim_end_matches('\n');
    let value = serde_json::from_str(trimmed)
        .map_err(|error| Failure::BadLine(format!("{trimmed}: {error}")))?;
    Ok(Some(value))
}

/// A handler that answers `initialize` with a fixed
/// object and refuses everything else.
struct InitializeOnly;

impl AcpRequestHandler for InitializeOnly {
    fn dispatch<'a>(
        &'a self,
        method: AgentMethod,
        _params: Option<Value>,
        _context: &'a AcpRequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AcpError>> + Send + 'a>> {
        Box::pin(async move {
            match method {
                AgentMethod::Initialize => Ok(serde_json::json!({"protocolVersion": 1})),
                other => Err(AcpError::MethodNotImplemented { method: other }),
            }
        })
    }
}

/// A handler that calls the editor back — asking for
/// permission — and then answers with the editor's
/// reply.
struct CallsTheEditor;

impl AcpRequestHandler for CallsTheEditor {
    fn dispatch<'a>(
        &'a self,
        method: AgentMethod,
        _params: Option<Value>,
        context: &'a AcpRequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AcpError>> + Send + 'a>> {
        Box::pin(async move {
            match method {
                AgentMethod::SessionPrompt => {
                    let reply = context
                        .editor()
                        .request(
                            ClientMethod::SessionRequestPermission,
                            serde_json::json!({"toolCall": {"name": "read"}}),
                        )
                        .await
                        .map_err(|error| AcpError::InternalError {
                            message: error.to_string(),
                        })?;
                    Ok(reply)
                }
                other => Err(AcpError::MethodNotImplemented { method: other }),
            }
        })
    }
}

/// The editor end of the duplex, split so the test can
/// read the agent's replies while writing requests.
type EditorRead = BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>;

/// The agent's reply to the editor's `initialize`
/// request reaches the editor, with the result the
/// handler produced.
#[tokio::test]
async fn the_reply_reaches_the_editor() -> Result<(), Failure> {
    let (editor_end, agent_end) = duplex(4096);
    let (agent_read, agent_write) = tokio::io::split(agent_end);
    let (editor_read, editor_write) = tokio::io::split(editor_end);
    let mut editor_write = editor_write;
    let mut editor_read: EditorRead = BufReader::new(editor_read);

    let peer = AcpAgentPeer::from_stdio(agent_read, agent_write);
    let serving = tokio::spawn(serve_acp_agent(peer, Arc::new(InitializeOnly)));

    editor_write
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"initialize\",\"params\":{}}\n")
        .await?;

    let envelope = read_envelope(&mut editor_read)
        .await?
        .ok_or_else(|| Failure::WrongOutcome("no reply arrived".to_owned()))?;

    assert_eq!(envelope.get("id"), Some(&serde_json::json!(7)));
    assert_eq!(
        envelope.get("result"),
        Some(&serde_json::json!({"protocolVersion": 1}))
    );

    // The loop keeps running after the reply, so the
    // test ends it by dropping the editor's write end.
    drop(editor_write);
    drop(editor_read);
    let outcome = serving.await?;
    assert!(
        matches!(outcome, Err(AcpServeError::EditorClosed)),
        "the loop should end when the editor closes the wire"
    );
    Ok(())
}

/// A handler that refuses a method produces a JSON-RPC
/// error reply carrying the method's code — the handler
/// never names the code itself.
#[tokio::test]
async fn a_refused_method_becomes_a_json_rpc_error() -> Result<(), Failure> {
    let (editor_end, agent_end) = duplex(4096);
    let (agent_read, agent_write) = tokio::io::split(agent_end);
    let (editor_read, editor_write) = tokio::io::split(editor_end);
    let mut editor_write = editor_write;
    let mut editor_read: EditorRead = BufReader::new(editor_read);

    let peer = AcpAgentPeer::from_stdio(agent_read, agent_write);
    let serving = tokio::spawn(serve_acp_agent(peer, Arc::new(InitializeOnly)));

    editor_write
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"session/new\",\"params\":{}}\n")
        .await?;

    let envelope = read_envelope(&mut editor_read)
        .await?
        .ok_or_else(|| Failure::WrongOutcome("no reply arrived".to_owned()))?;

    assert_eq!(envelope.get("id"), Some(&serde_json::json!(3)));
    let error = envelope
        .get("error")
        .ok_or_else(|| Failure::WrongOutcome("no error member".to_owned()))?;
    assert_eq!(error.get("code"), Some(&serde_json::json!(-32601)));

    drop(editor_write);
    drop(editor_read);
    let _ = serving.await?;
    Ok(())
}

/// A method the ACP vocabulary does not name is
/// answered with `-32601` rather than dispatched — a
/// wire violation is not a handler bug.
#[tokio::test]
async fn an_unknown_method_is_rejected_without_dispatch() -> Result<(), Failure> {
    let (editor_end, agent_end) = duplex(4096);
    let (agent_read, agent_write) = tokio::io::split(agent_end);
    let (editor_read, editor_write) = tokio::io::split(editor_end);
    let mut editor_write = editor_write;
    let mut editor_read: EditorRead = BufReader::new(editor_read);

    let peer = AcpAgentPeer::from_stdio(agent_read, agent_write);
    let serving = tokio::spawn(serve_acp_agent(peer, Arc::new(InitializeOnly)));

    editor_write
        .write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"session/teleport\",\"params\":{}}\n",
        )
        .await?;

    let envelope = read_envelope(&mut editor_read)
        .await?
        .ok_or_else(|| Failure::WrongOutcome("no reply arrived".to_owned()))?;

    assert_eq!(envelope.get("id"), Some(&serde_json::json!(9)));
    let error = envelope
        .get("error")
        .ok_or_else(|| Failure::WrongOutcome("no error member".to_owned()))?;
    assert_eq!(error.get("code"), Some(&serde_json::json!(-32601)));

    drop(editor_write);
    drop(editor_read);
    let _ = serving.await?;
    Ok(())
}

/// The agent-to-editor direction works while the serving
/// loop is parked: the handler calls the editor back, the
/// editor answers, and the reply comes back out of the
/// serving loop.
///
/// This is the test that would hang if the two halves
/// shared a lock — the loop holds the peer while the
/// handler needs to write to it.
#[tokio::test]
async fn the_handler_can_call_the_editor_back() -> Result<(), Failure> {
    let (editor_end, agent_end) = duplex(4096);
    let (agent_read, agent_write) = tokio::io::split(agent_end);
    let (editor_read, editor_write) = tokio::io::split(editor_end);
    let mut editor_write = editor_write;
    let mut editor_read: EditorRead = BufReader::new(editor_read);

    let peer = AcpAgentPeer::from_stdio(agent_read, agent_write);
    let serving = tokio::spawn(serve_acp_agent(peer, Arc::new(CallsTheEditor)));

    editor_write
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"session/prompt\",\"params\":{}}\n")
        .await?;

    // The agent's outbound request arrives at the
    // editor first, before the reply to the prompt.
    let outbound = read_envelope(&mut editor_read)
        .await?
        .ok_or_else(|| Failure::WrongOutcome("no outbound request arrived".to_owned()))?;

    assert_eq!(
        outbound.get("method"),
        Some(&serde_json::json!("session/request_permission"))
    );
    let outbound_id = outbound
        .get("id")
        .cloned()
        .ok_or_else(|| Failure::WrongOutcome("no id on the outbound request".to_owned()))?;

    // The editor answers the agent's request.
    editor_write
        .write_all(
            format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":{outbound_id},\"result\":{{\"outcome\":{{\"outcome\":\"selected\"}}}}}}\n"
            )
            .as_bytes(),
        )
        .await?;

    // And now the reply to the prompt comes back, with
    // the editor's answer in it.
    let reply = read_envelope(&mut editor_read)
        .await?
        .ok_or_else(|| Failure::WrongOutcome("no reply arrived".to_owned()))?;

    assert_eq!(reply.get("id"), Some(&serde_json::json!(1)));
    assert_eq!(
        reply.get("result"),
        Some(&serde_json::json!({"outcome": {"outcome": "selected"}}))
    );

    drop(editor_write);
    drop(editor_read);
    let _ = serving.await?;
    Ok(())
}

/// The editor port the adapter builds is reachable from
/// the handler, and a handler that never calls it still
/// works — the two halves are independent.
#[tokio::test]
async fn a_handler_that_never_calls_back_still_answers() -> Result<(), Failure> {
    let (editor_end, agent_end) = duplex(4096);
    let (agent_read, agent_write) = tokio::io::split(agent_end);
    let (editor_read, editor_write) = tokio::io::split(editor_end);
    let mut editor_write = editor_write;
    let mut editor_read: EditorRead = BufReader::new(editor_read);

    let peer = AcpAgentPeer::from_stdio(agent_read, agent_write);
    let serving = tokio::spawn(serve_acp_agent(peer, Arc::new(InitializeOnly)));

    editor_write
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":11,\"method\":\"initialize\",\"params\":{}}\n")
        .await?;

    let envelope = read_envelope(&mut editor_read)
        .await?
        .ok_or_else(|| Failure::WrongOutcome("no reply arrived".to_owned()))?;
    assert_eq!(envelope.get("id"), Some(&serde_json::json!(11)));

    drop(editor_write);
    drop(editor_read);
    let _ = serving.await?;
    Ok(())
}

/// A transport failure is mapped onto the port's error
/// type, not leaked as a transport error — the
/// application layer never names one.
#[test]
fn a_transport_failure_maps_onto_the_port_error() {
    let mapped = map_peer_error(AcpPeerError::JsonRpc {
        code: -32000,
        message: "the editor said no".to_owned(),
    });
    assert!(matches!(
        mapped,
        AcpEditorError::JsonRpc { code: -32000, .. }
    ));

    let mapped = map_peer_error(AcpPeerError::IllShapedReply("no result".to_owned()));
    assert!(matches!(mapped, AcpEditorError::Disconnected { .. }));
}

/// The editor port is reachable from a caller-supplied
/// context, so a consumer can substitute its own.
#[tokio::test]
async fn a_caller_supplied_context_is_used() -> Result<(), Failure> {
    let (editor_end, agent_end) = duplex(4096);
    let (agent_read, agent_write) = tokio::io::split(agent_end);
    let (editor_read, editor_write) = tokio::io::split(editor_end);
    let mut editor_write = editor_write;
    let mut editor_read: EditorRead = BufReader::new(editor_read);

    let peer = AcpAgentPeer::from_stdio(agent_read, agent_write);
    let context = AcpRequestContext::new(AcpProtocolVersion::CURRENT, Arc::new(RefusingEditor));
    let serving = tokio::spawn(serve_acp_agent_with_context(
        peer,
        Arc::new(InitializeOnly),
        context,
    ));

    editor_write
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"initialize\",\"params\":{}}\n")
        .await?;

    let envelope = read_envelope(&mut editor_read)
        .await?
        .ok_or_else(|| Failure::WrongOutcome("no reply arrived".to_owned()))?;
    assert_eq!(envelope.get("id"), Some(&serde_json::json!(2)));

    drop(editor_write);
    drop(editor_read);
    let _ = serving.await?;
    Ok(())
}

/// An editor port that refuses every call. Used to show
/// that the caller's context is the one the handler
/// sees.
struct RefusingEditor;

impl AcpEditorCaller for RefusingEditor {
    fn request(
        &self,
        _method: ClientMethod,
        _params: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AcpEditorError>> + Send + '_>> {
        Box::pin(async {
            Err(AcpEditorError::Disconnected {
                reason: "the test refused the call".to_owned(),
            })
        })
    }

    fn notify(
        &self,
        _method: ClientMethod,
        _params: Value,
    ) -> Pin<Box<dyn Future<Output = Result<(), AcpEditorError>> + Send + '_>> {
        Box::pin(async {
            Err(AcpEditorError::Disconnected {
                reason: "the test refused the call".to_owned(),
            })
        })
    }
}
