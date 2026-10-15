//! MCP interop test against a vanilla-Node reference server.
//!
//! ## What this proves
//!
//! The library's typed `McpStdioPeer` speaks
//! the same wire format as a
//! non-protocol-droid MCP server. The
//! reference server is a hand-rolled
//! vanilla-Node script that implements
//! the `initialize` + `tools/list` +
//! `tools/call` round-trip. The interop
//! test drives all three end-to-end and
//! asserts that the typed shapes the
//! consumer decodes match what the
//! server actually emitted.
//!
//! ## Why vanilla Node
//!
//! The official TS SDK is the canonical
//! reference impl, but the SDK's sample
//! server is distributed as part of a
//! multi-MB npm monorepo with a
//! TypeScript build step. That is the
//! right shape for a production
//! deployment; it is too heavy for a
//! CI-friendly integration test, which
//! is what this file exists for. The
//! interop question is "do protocol-droid
//! and a hand-rolled MCP server agree on
//! the wire?" -- the answer is the same
//! whether the second server is the
//! SDK's sample or a ~100-line Node
//! script, because the wire format is
//! published.

use std::path::PathBuf;
use std::time::Duration;

use protocol_droid::adapters::secondary::mcp::{
    CallToolRequest, ClientCapabilities, ContentBlock, Implementation, InitializeRequest,
    InitializeResult, ListToolsResult, McpStdioPeer, SpawnSpec,
};

/// The pinned revision this fixture
/// speaks. The `McpStdioPeer` does
/// not pin the revision itself
/// (the consumer picks one); the
/// fixture and the test agree on
/// this value.
const PINNED_REVISION: &str = "2025-06-18";

/// Return the path to the
/// vanilla-Node reference server.
fn reference_server_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mcp_reference_server.mjs")
}

/// Return the path to the
/// `node` executable, or `None`
/// if `node` is not on `PATH`.
/// The test skips with a clear
/// log line when `node` is not
/// available.
fn find_node() -> Option<String> {
    let output = std::process::Command::new("which")
        .arg("node")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8(output.stdout).ok()?;
    let trimmed = path.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// The test runs the
/// `initialize` +
/// `tools/list` + `tools/call`
/// round-trip end to end.
/// Asserts:
///
///   - The `initialize` reply
///     decodes into a typed
///     `InitializeResult` with
///     the pinned revision and
///     a `tools` capability.
///   - The `tools/list` reply
///     decodes into a typed
///     `ListToolsResult` with
///     one tool, `echo`, whose
///     `name` and
///     `description` match the
///     fixture's hand-built
///     values.
///   - The `tools/call` reply
///     decodes into a typed
///     `CallToolResult` with
///     the arguments echoed
///     back as a `Text`
///     content block.
#[tokio::test]
async fn initialize_tools_list_and_call_round_trip() -> Result<(), String> {
    let Some(node) = find_node() else {
        eprintln!("skipping: `node` is not on PATH");
        return Ok(());
    };
    let spec = SpawnSpec::new(
        node,
        vec![reference_server_path().to_string_lossy().into_owned()],
    )
    .with_timeout(Duration::from_secs(10));
    let peer = McpStdioPeer::spawn(&spec).map_err(|error| format!("spawning the peer: {error}"))?;

    // `initialize`. The pinned
    // revision matches the
    // fixture's hand-built
    // reply.
    let initialize_request = InitializeRequest {
        protocol_version: PINNED_REVISION.to_owned(),
        capabilities: ClientCapabilities::default(),
        client_info: Implementation {
            name: "protocol-droid-mcp-client".to_owned(),
            title: Some("protocol-droid MCP client".to_owned()),
            version: "0.1.0".to_owned(),
        },
    };
    let initialize: InitializeResult = peer
        .send_typed_request("initialize", initialize_request)
        .await
        .map_err(|error| format!("initialize reply: {error}"))?;
    assert_eq!(initialize.protocol_version, PINNED_REVISION);
    assert_eq!(initialize.server_info.name, "mcp-reference-server");
    assert!(
        initialize.capabilities.tools.is_some(),
        "the server must advertise the `tools` capability",
    );

    // `tools/list`. The
    // fixture returns one
    // tool, `echo`.
    let tools: ListToolsResult = peer
        .send_typed_request("tools/list", serde_json::json!({}))
        .await
        .map_err(|error| format!("tools/list reply: {error}"))?;
    assert_eq!(tools.tools.len(), 1, "the fixture advertises one tool");
    let Some(echo) = tools.tools.first() else {
        return Err("the fixture must advertise at least one tool".to_owned());
    };
    assert_eq!(echo.name, "echo");
    assert_eq!(
        echo.description.as_deref(),
        Some("Echoes the call arguments back as text content."),
    );
    assert!(echo.input_schema.get("properties").is_some());

    // `tools/call`. The
    // fixture echoes the
    // `message` argument
    // back as text content.
    let call = CallToolRequest {
        name: "echo".to_owned(),
        arguments: serde_json::json!({ "message": "hello" }),
    };
    let result: protocol_droid::adapters::secondary::mcp::CallToolResult = peer
        .send_typed_request("tools/call", call)
        .await
        .map_err(|error| format!("tools/call reply: {error}"))?;
    assert!(!result.is_error);
    assert_eq!(result.content.len(), 1);
    let Some(first) = result.content.first() else {
        return Err("the call result must contain at least one content block".to_owned());
    };
    match first {
        ContentBlock::Text { text } => assert_eq!(text, "echo: hello"),
        other => {
            return Err(format!(
                "expected a `Text` content block, got: {other:?}"
            ))
        }
    }
    Ok(())
}

/// `ping` round-trip. A small
/// follow-up: the server returns
/// `{}` and the client decodes
/// the reply as a JSON object.
#[tokio::test]
async fn ping_round_trips() -> Result<(), String> {
    let Some(node) = find_node() else {
        eprintln!("skipping: `node` is not on PATH");
        return Ok(());
    };
    let spec = SpawnSpec::new(
        node,
        vec![reference_server_path().to_string_lossy().into_owned()],
    )
    .with_timeout(Duration::from_secs(10));
    let peer = McpStdioPeer::spawn(&spec).map_err(|error| format!("spawning the peer: {error}"))?;

    // `ping` is the
    // liveness probe;
    // some servers gate
    // it on `initialize`
    // being completed
    // first, so do an
    // `initialize` then a
    // `ping`.
    let _initialize: InitializeResult = peer
        .send_typed_request(
            "initialize",
            InitializeRequest {
                protocol_version: PINNED_REVISION.to_owned(),
                capabilities: ClientCapabilities::default(),
                client_info: Implementation {
                    name: "protocol-droid-mcp-client".to_owned(),
                    title: None,
                    version: "0.1.0".to_owned(),
                },
            },
        )
        .await
        .map_err(|error| format!("initialize reply: {error}"))?;
    let ping: serde_json::Value = peer
        .send_typed_request("ping", serde_json::json!({}))
        .await
        .map_err(|error| format!("ping reply: {error}"))?;
    assert_eq!(ping, serde_json::json!({}));
    Ok(())
}

/// An unknown method returns
/// a JSON-RPC error. The
/// client's typed wrapper
/// surfaces the error code
/// and message, not a
/// transport error.
#[tokio::test]
async fn an_unknown_method_returns_a_json_rpc_error() -> Result<(), String> {
    let Some(node) = find_node() else {
        eprintln!("skipping: `node` is not on PATH");
        return Ok(());
    };
    let spec = SpawnSpec::new(
        node,
        vec![reference_server_path().to_string_lossy().into_owned()],
    )
    .with_timeout(Duration::from_secs(10));
    let peer = McpStdioPeer::spawn(&spec).map_err(|error| format!("spawning the peer: {error}"))?;
    let result: Result<serde_json::Value, _> = peer
        .send_typed_request("definitely/not/a/real/method", serde_json::json!({}))
        .await;
    let error = result
        .err()
        .ok_or_else(|| "the unknown method should have errored".to_owned())?;
    match error {
        protocol_droid::adapters::secondary::mcp::McpPeerError::JsonRpc { code, .. } => {
            assert_eq!(code, -32601, "the server should return `Method not found`");
        }
        other => return Err(format!("expected a `JsonRpc` error, got: {other:?}")),
    }
    Ok(())
}
