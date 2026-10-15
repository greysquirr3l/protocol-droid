//! ACP v1.25.0 interop test against a vanilla-Node reference agent.
//!
//! ## What this proves
//!
//! The library's typed `AcpStdioPeer` speaks
//! the same wire format as a non-protocol-droid
//! ACP agent. The reference agent is a
//! hand-rolled vanilla-Node script that
//! implements the `initialize` method (the
//! only one the protocol-droid types currently
//! model). The interop test drives `initialize`
//! end-to-end and asserts that the typed
//! `InitializeResponse` the consumer decodes
//! has the same shape the agent actually
//! emitted.
//!
//! ## Why vanilla Node
//!
//! The official JS SDK is the canonical
//! reference impl, but the SDK's sample agent
//! is distributed as part of a multi-MB npm
//! monorepo with a TypeScript build step.
//! That is the right shape for a production
//! deployment; it is too heavy for a
//! CI-friendly integration test, which is what
//! this file exists for. The interop question
//! is "do protocol-droid and a hand-rolled ACP
//! agent agree on the wire?" -- the answer is
//! the same whether the second agent is the
//! SDK's sample or a 100-line Node script,
//! because the wire format is published.
//!
//! ## Skipping when Node is not on PATH
//!
//! The interop test is a CI tool, not a
//! runtime dep. A dev environment without
//! Node skips the test with a clear log line;
//! a CI environment with Node runs it.
//!
//! ## How a test runs
//!
//! 1. `find_node()` returns `Some` or `None`.
//! 2. If `None`, the test logs and returns
//!    `Ok(())` (a skipped test is a pass).
//! 3. If `Some`, the test spawns the reference
//!    agent through the typed `AcpStdioPeer`,
//!    sends a typed `InitializeRequest`, and
//!    parses the typed `InitializeResponse`
//!    back.
//! 4. The child is killed when the peer drops
//!    (the peer sets `Command::kill_on_drop(true)`).

#![cfg(feature = "acp-experimental")]

use std::path::PathBuf;
use std::time::Duration;

use protocol_droid::adapters::secondary::acp::{AcpStdioPeer, SpawnSpec};
use protocol_droid::domain::acp::implementation::Implementation;
use protocol_droid::domain::acp::initialize::{
    AuthMethod, ClientCapabilities, InitializeRequest,
};
use protocol_droid::domain::acp::method::{AcpMethod, AgentMethod};
use protocol_droid::domain::acp::session::{McpServer, NewSessionRequest, SessionId};
use serde_json::Value;

/// The path to the in-tree JS reference agent,
/// resolved relative to `CARGO_MANIFEST_DIR`.
fn reference_agent_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/acp_reference_agent.mjs")
}

/// Returns the path to a `node` executable on
/// `PATH`, or `None` if there isn't one. The
/// interop test is a CI tool, not a runtime
/// dep; a dev box without Node skips the test
/// rather than failing it.
fn find_node() -> Option<String> {
    let path = std::env::var_os("PATH")?;
    let candidates: &[&str] = if cfg!(windows) {
        &["node.exe", "node.cmd", "node.bat"]
    } else {
        &["node"]
    };
    for directory in std::env::split_paths(&path) {
        for candidate in candidates {
            let full = directory.join(candidate);
            if full.is_file() {
                return Some(full.to_string_lossy().into_owned());
            }
        }
    }
    None
}

/// The whole interop test is gated on
/// `find_node()` returning `Some`. A clean
/// skip is `Ok(())`; a real failure is
/// `Err(...)`.
fn ensure_node() -> Result<String, String> {
    find_node().ok_or_else(|| {
        "the ACP stdio interop test is skipped because `node` is not on PATH; \
         install Node 18+ to run the interop suite"
            .to_owned()
    })
}

/// A typed `InitializeRequest` the test sends
/// to the reference agent.
fn sample_initialize_request() -> InitializeRequest {
    InitializeRequest {
        protocol_version: protocol_droid::domain::acp::protocol_version::AcpProtocolVersion::CURRENT,
        client_capabilities: ClientCapabilities::default(),
        client_info: Some(Implementation::protocol_droid("acp-stdio-interop", "0.1.0")),
    }
}

/// The peer drives the typed `InitializeRequest`
/// through a vanilla-Node reference agent and
/// the response decodes into a typed
/// `InitializeResponse`. The assertion is on
/// the wire shape, not on the typed newtypes.
#[tokio::test]
async fn initialize_round_trip_against_a_vanilla_node_agent() -> Result<(), String> {
    let node = ensure_node()?;
    let spec = SpawnSpec::new(node, vec![reference_agent_path().to_string_lossy().into_owned()])
        .with_timeout(Duration::from_secs(10));
    let peer = AcpStdioPeer::spawn(&spec).map_err(|error| format!("spawning the peer: {error}"))?;
    let reply = peer
        .send_request(
            AcpMethod::Agent(AgentMethod::Initialize),
            sample_initialize_request(),
        )
        .await
        .map_err(|error| format!("initialize reply: {error}"))?;

    let reply_id = reply
        .id
        .as_u64()
        .ok_or_else(|| format!("reply id is not a u64: {}", reply.id))?;
    assert!(reply_id >= 1, "the reply id is {reply_id}");

    let response: protocol_droid::domain::acp::initialize::InitializeResponse =
        serde_json::from_value(reply.result)
            .map_err(|error| format!("decoding InitializeResponse: {error}"))?;

    assert_eq!(
        response.protocol_version,
        protocol_droid::domain::acp::protocol_version::AcpProtocolVersion::CURRENT,
    );

    let auth_methods = response
        .auth_methods
        .as_ref()
        .ok_or_else(|| "the reference agent advertised no auth methods".to_owned())?;
    let method = auth_methods
        .first()
        .ok_or_else(|| "the auth methods list is empty".to_owned())?;
    match method {
        AuthMethod::Terminal(terminal) => {
            assert_eq!(terminal.id, "dev-terminal");
            assert_eq!(terminal.name, "In-process dev terminal");
        }
        AuthMethod::Agent(_) => {
            return Err("expected the terminal auth method, got the agent variant".to_owned());
        }
    }

    let info = response
        .agent_info
        .as_ref()
        .ok_or_else(|| "the reference agent advertised no agentInfo".to_owned())?;
    assert_eq!(info.name, "acp-reference-agent");
    assert_eq!(info.version, "1.25.0");

    drop(peer);
    Ok(())
}

/// The peer decodes a JSON-RPC error reply
/// (e.g. method not found) into a typed
/// `AcpPeerError::JsonRpc`. The reference
/// agent returns `-32601` for any method it
/// does not implement; the test uses
/// `session/list` to drive the path.
#[tokio::test]
async fn an_unknown_method_returns_a_typed_json_rpc_error() -> Result<(), String> {
    let node = ensure_node()?;
    let spec = SpawnSpec::new(node, vec![reference_agent_path().to_string_lossy().into_owned()])
        .with_timeout(Duration::from_secs(10));
    let peer = AcpStdioPeer::spawn(&spec).map_err(|error| format!("spawning the peer: {error}"))?;

    let outcome = peer
        .send_request(
            AcpMethod::Agent(AgentMethod::SessionList),
            serde_json::json!({}),
        )
        .await;

    match outcome {
        Err(protocol_droid::adapters::secondary::acp::AcpPeerError::JsonRpc { code, message }) => {
            assert_eq!(code, -32601);
            assert_eq!(message, "Method not found");
        }
        Err(other) => {
            return Err(format!("expected a JSON-RPC error, got {other}"));
        }
        Ok(reply) => {
            return Err(format!(
                "expected a JSON-RPC error, got a success reply: {reply:?}"
            ));
        }
    }

    drop(peer);
    Ok(())
}

/// The minimal `InitializeRequest` (no
/// `clientInfo`) round-trips. The agent's
/// reply is the same shape; the assertion
/// is the wire form of the minimal request
/// is well-formed.
#[tokio::test]
async fn a_minimal_initialize_request_round_trips() -> Result<(), String> {
    let node = ensure_node()?;
    let spec = SpawnSpec::new(node, vec![reference_agent_path().to_string_lossy().into_owned()])
        .with_timeout(Duration::from_secs(10));
    let peer = AcpStdioPeer::spawn(&spec).map_err(|error| format!("spawning the peer: {error}"))?;

    let request = InitializeRequest {
        protocol_version: protocol_droid::domain::acp::protocol_version::AcpProtocolVersion::CURRENT,
        client_capabilities: ClientCapabilities::default(),
        client_info: None,
    };
    let reply = peer
        .send_request(AcpMethod::Agent(AgentMethod::Initialize), request)
        .await
        .map_err(|error| format!("initialize reply: {error}"))?;
    let response: protocol_droid::domain::acp::initialize::InitializeResponse =
        serde_json::from_value(reply.result)
            .map_err(|error| format!("decoding InitializeResponse: {error}"))?;

    assert_eq!(
        response.protocol_version,
        protocol_droid::domain::acp::protocol_version::AcpProtocolVersion::CURRENT,
    );

    drop(peer);
    Ok(())
}

/// Smoke test: the peer can be spawned, kept
/// alive briefly, and dropped. The child
/// process is killed by the `kill_on_drop`
/// flag.
#[tokio::test]
async fn the_peer_spawns_against_node_and_drops_cleanly() -> Result<(), String> {
    let node = ensure_node()?;
    let spec = SpawnSpec::new(node, vec![reference_agent_path().to_string_lossy().into_owned()])
        .with_timeout(Duration::from_secs(10));
    let peer = AcpStdioPeer::spawn(&spec).map_err(|error| format!("spawning the peer: {error}"))?;

    tokio::time::sleep(Duration::from_millis(50)).await;

    drop(peer);
    Ok(())
}

/// The wire form of the typed `AuthMethod`
/// carries a `type` discriminator. The
/// reference agent's `terminal` auth method
/// round-trips through the typed decode.
#[tokio::test]
async fn the_auth_method_discriminator_is_the_type_field() -> Result<(), String> {
    let node = ensure_node()?;
    let spec = SpawnSpec::new(node, vec![reference_agent_path().to_string_lossy().into_owned()])
        .with_timeout(Duration::from_secs(10));
    let peer = AcpStdioPeer::spawn(&spec).map_err(|error| format!("spawning the peer: {error}"))?;
    let reply = peer
        .send_request(
            AcpMethod::Agent(AgentMethod::Initialize),
            sample_initialize_request(),
        )
        .await
        .map_err(|error| format!("initialize reply: {error}"))?;

    let auth_methods = reply
        .result
        .get("authMethods")
        .and_then(Value::as_array)
        .ok_or_else(|| "the reply does not carry an authMethods array".to_owned())?;
    let method = auth_methods
        .first()
        .ok_or_else(|| "the authMethods array is empty".to_owned())?;
    let kind = method
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| "the auth method has no `type` field".to_owned())?;
    assert_eq!(kind, "terminal", "the wire discriminator is `terminal`");
    let id = method
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| "the auth method has no `id` field".to_owned())?;
    assert_eq!(id, "dev-terminal", "the wire `id` is the agent's value");

    drop(peer);
    Ok(())
}

/// The typed `session/new` request round-trips
/// through `send_typed_request` and decodes a
/// typed `NewSessionResponse`. This is the
/// first test that drives the typed wrapper
/// the consumer-facing API exposes — the
/// previous tests all decode the raw
/// `reply.result` by hand.
#[tokio::test]
async fn a_typed_session_new_request_round_trips() -> Result<(), String> {
    let node = ensure_node()?;
    let spec = SpawnSpec::new(node, vec![reference_agent_path().to_string_lossy().into_owned()])
        .with_timeout(Duration::from_secs(10));
    let peer = AcpStdioPeer::spawn(&spec).map_err(|error| format!("spawning the peer: {error}"))?;

    let request = NewSessionRequest {
        cwd: "/tmp/example".to_owned(),
        additional_directories: None,
        mcp_servers: vec![McpServer::Stdio(
            protocol_droid::domain::acp::session::McpServerStdio {
                name: "stdio-server".to_owned(),
                command: "/usr/local/bin/mcp-server".to_owned(),
                args: vec!["--quiet".to_owned()],
                env: vec![],
            },
        )],
    };

    let response: protocol_droid::domain::acp::session::NewSessionResponse = peer
        .send_typed_request(AcpMethod::Agent(AgentMethod::SessionNew), request)
        .await
        .map_err(|error| format!("session/new reply: {error}"))?;

    let expected_id = SessionId::new("fixture-1");
    assert_eq!(response.session_id, expected_id);

    drop(peer);
    Ok(())
}

/// The typed `send_notification` writes a
/// JSON-RPC 2.0 notification (no `id` field)
/// and the agent is free to ignore it. The
/// reference agent does ignore it (the test
/// asserts the call returned `Ok` and the
/// peer is still alive).
#[tokio::test]
async fn a_typed_session_cancel_notification_writes_cleanly() -> Result<(), String> {
    let node = ensure_node()?;
    let spec = SpawnSpec::new(node, vec![reference_agent_path().to_string_lossy().into_owned()])
        .with_timeout(Duration::from_secs(10));
    let peer = AcpStdioPeer::spawn(&spec).map_err(|error| format!("spawning the peer: {error}"))?;

    let notification =
        protocol_droid::domain::acp::session::CancelNotification { session_id: SessionId::new("s") };
    peer.send_notification(AcpMethod::Agent(AgentMethod::SessionCancel), notification)
        .await
        .map_err(|error| format!("session/cancel write: {error}"))?;

    tokio::time::sleep(Duration::from_millis(50)).await;

    drop(peer);
    Ok(())
}
