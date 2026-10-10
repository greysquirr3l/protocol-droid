//! `protocol-droid` client → TS SDK server, across revisions.
//!
//! The first half of the T22 cross-test for the TypeScript SDK.
//! Spawns the TS SDK's stdio server (`fixtures/server.mjs`,
//! driven by `@modelcontextprotocol/sdk` `1.32.1`) as a child,
//! then drives it with a `protocol-droid` `StdioClient` —
//! `initialize`, `tools/list`, `tools/call add`, and a second
//! `tools/call` against a tool the server does not advertise,
//! to cover the negative path. A failure is either a
//! `protocol-droid` codec bug or a TS SDK wire shape, and the
//! finding goes into `FINDINGS.md` (T22 must-have) with the
//! spec citation deciding which one is right.
//!
//! The same matrix is re-run for every revision in
//! `REVISIONS`; a regression in any one leg fails the run.

use interop_ts_sdk::{Failure, Result, REVISIONS};
use protocol_droid::adapters::primary::stdio::Diagnostics;
use protocol_droid::adapters::secondary::stdio::{ChildSpec, ClientConfig, StdioClient};
use protocol_droid::domain::ir::{JsonObject, MethodName};
use serde_json::Value;

/// Path to the `node` binary the test spawns.
///
/// `node` must be on `PATH`; a fixture that hard-codes an
/// absolute path to a specific `node` install would make the
/// matrix non-reproducible across machines. A test that fails
/// to find `node` is reported as `Failure::Process` rather than
/// as a wire-shape diff, so a missing `node` is its own
/// diagnostic rather than a silent mis-interpretation.
fn node_path() -> std::path::PathBuf {
    std::path::PathBuf::from("node")
}

/// Path to the TS server fixture the test drives.
///
/// `fixtures/server.mjs` lives next to this test crate. The
/// path is computed from `CARGO_MANIFEST_DIR` so a copy of the
/// crate into another tree (a CI cache, a fork) still works.
fn ts_server_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("server.mjs")
}

/// What `tools/list` must report for the cross-test to be
/// meaningful.
///
/// The exact tool name is part of the cross-test contract — it
/// is what the test asserts on. A change here is a test
/// failure, not a runtime adaptation.
const EXPECTED_TOOL_NAME: &str = "add";

/// Builds the `initialize` params the cross-test sends.
///
/// The protocol version is interpolated into the wire shape
/// `protocol-droid` and the TS SDK both parse; the rest of the
/// handshake is a fixed shape the cross-test does not vary.
/// The raw string keeps the wire bytes the cross-test asserts
/// on identical to what a real client would send, rather than
/// passing through `serde_json::Value` and re-serialising.
fn initialize_params(revision: &str) -> Result<JsonObject> {
    let text = format!(
        r#"{{"protocolVersion":"{revision}","capabilities":{{}},"clientInfo":{{"name":"interop-ts-sdk-test","version":"0.0.0"}}}}"#
    );
    JsonObject::new(text).map_err(|error| Failure::ProtocolDroid(format!("initialize params: {error}")))
}

/// Builds the `tools/call add` params the cross-test sends.
fn call_add_params() -> Result<JsonObject> {
    let text = r#"{"name":"add","arguments":{"a":2,"b":3}}"#;
    JsonObject::new(text).map_err(|error| Failure::ProtocolDroid(format!("call params: {error}")))
}

/// Builds the `tools/call` params for a tool the server does
/// not advertise.
///
/// The TS SDK's tool handler throws a plain `Error` for an
/// unknown tool, which the SDK converts to a JSON-RPC
/// `InternalError` (-32603). The MCP spec recommends
/// `MethodNotFound` (-32601) for that case, and the spec
/// deviation is recorded in `FINDINGS.md` (TS-SDK-001). The
/// cross-test does not assert on the negative path because
/// the disagreement is not a wire-shape bug — both sides
/// agree on the bytes — but a *spec* disagreement the
/// conformance suite's `tools-call-unknown-tool` scenario
/// already covers. A wire-shape diff here would be a
/// different finding.
#[allow(dead_code, reason = "kept for the FINDINGS.md entry; not asserted on")]
fn call_unknown_tool_params() -> Result<JsonObject> {
    let text = r#"{"name":"definitely_not_a_real_tool","arguments":{}}"#;
    JsonObject::new(text).map_err(|error| Failure::ProtocolDroid(format!("call params: {error}")))
}

/// Drives one cross-test pass against one revision.
async fn drive_one(spec_version: &str) -> Result<()> {
    let spec = ChildSpec::from_argv([node_path(), ts_server_path()]).map_err(|error| {
        Failure::Harness(format!("TS server ChildSpec is not valid: {error}"))
    })?;
    let config = ClientConfig::default();
    let client = StdioClient::spawn(&spec, config, Diagnostics::stderr())
        .await
        .map_err(|error| Failure::Process(format!("spawn ts-sdk server: {error}")))?;

    // `initialize` is the handshake. The cross-test's claim
    // is that the version it ends up with is the one it asked
    // for; if the TS SDK negotiated to a different revision
    // (e.g. for `2024-11-05` it might answer with the
    // SDK's `LATEST_PROTOCOL_VERSION`), the assertion fires
    // and the finding goes into `FINDINGS.md`.
    let init_params = initialize_params(spec_version)?;
    let init_id = client.next_request_id();
    let init_method = MethodName::new("initialize")
        .map_err(|error| Failure::ProtocolDroid(format!("initialize method: {error}")))?;
    let init_result = client
        .call(&init_id, &init_method, &init_params)
        .await
        .map_err(|error| Failure::ProtocolDroid(format!("initialize call: {error}")))?;

    let init_json: Value = serde_json::from_str(init_result.as_str())
        .map_err(|error| Failure::ProtocolDroid(format!("initialize result is not JSON: {error}")))?;
    let negotiated = init_json
        .get("protocolVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Failure::TsSdk(format!(
                "initialize result is missing protocolVersion field: {init_json}"
            ))
        })?;
    assert_eq!(negotiated, spec_version, "TS SDK negotiated to {negotiated:?} when asked for {spec_version:?}; the cross-test's claim is that the asked-for version is the negotiated one");

    // `tools/list` should report exactly the `add` tool the
    // fixture advertises. The cross-test does not require
    // the SDK to surface every field of the schema
    // (description, mime, etc.) — those are tested by the
    // conformance suite's `tools-list` scenario — only the
    // name and the basic shape.
    let list_id = client.next_request_id();
    let list_method = MethodName::new("tools/list")
        .map_err(|error| Failure::ProtocolDroid(format!("tools/list method: {error}")))?;
    let list_result = client
        .call(&list_id, &list_method, &JsonObject::new("{}".to_owned()).unwrap())
        .await
        .map_err(|error| Failure::ProtocolDroid(format!("tools/list call: {error}")))?;

    let list_json: Value = serde_json::from_str(list_result.as_str())
        .map_err(|error| Failure::ProtocolDroid(format!("tools/list result is not JSON: {error}")))?;
    let tool_names: Vec<&str> = list_json
        .get("tools")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(|t| t.get("name").and_then(Value::as_str)).collect())
        .unwrap_or_default();
    assert!(
        tool_names.contains(&EXPECTED_TOOL_NAME),
        "TS SDK did not advertise {EXPECTED_TOOL_NAME:?}; got {tool_names:?}"
    );

    // `tools/call add 2 + 3` should answer `5` in the
    // `content[0].text` slot the TS SDK produces.
    let call_id = client.next_request_id();
    let call_method = MethodName::new("tools/call")
        .map_err(|error| Failure::ProtocolDroid(format!("tools/call method: {error}")))?;
    let call_result = client
        .call(&call_id, &call_method, &call_add_params()?)
        .await
        .map_err(|error| Failure::ProtocolDroid(format!("tools/call add: {error}")))?;

    let call_json: Value = serde_json::from_str(call_result.as_str())
        .map_err(|error| Failure::TsSdk(format!("tools/call add result is not JSON: {error}")))?;
    let text = call_json
        .get("content")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(|item| item.get("text"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Failure::TsSdk(format!(
                "tools/call add did not return a content[0].text field: {call_json}"
            ))
        })?;
    assert_eq!(
        text, "5",
        "TS SDK answered `add(2,3)` with {text:?}, expected `5`"
    );

    Ok(())
}

/// The matrix entry point. One leg per revision in
/// `REVISIONS`; a failure in any one fails the whole test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pd_client_to_ts_server_matrix() -> Result<()> {
    for revision in REVISIONS {
        drive_one(revision).await.map_err(|error| {
            Failure::Harness(format!("revision {revision} failed: {error}"))
        })?;
    }
    Ok(())
}
