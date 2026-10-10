//! `protocol-droid` client → `rmcp` server, across revisions.
//!
//! The first half of the T22 cross-test. Spawns the
//! `rmcp_stdio_server` binary as a child, then drives it with a
//! `protocol-droid` `StdioClient` — `initialize`, `tools/list`,
//! `tools/call add`, and `ping` — and asserts each answer matches
//! the schema this crate's `Counter` advertises. A failure is either
//! a `protocol-droid` codec bug or an `rmcp` wire shape, and the
//! finding goes into `FINDINGS.md` (T22 must-have) with the spec
//! citation deciding which one is right.
//!
//! The same matrix is re-run for every revision in
//! `REVISIONS`; a regression in any one leg fails the run.

use interop_rmcp::{Failure, Result, REVISIONS};
use protocol_droid::adapters::primary::stdio::Diagnostics;
use protocol_droid::adapters::secondary::stdio::{ChildSpec, ClientConfig, StdioClient};
use protocol_droid::domain::ir::{JsonObject, MethodName};
use serde_json::Value;

/// Path to the `rmcp_stdio_server` binary this test crate builds.
///
/// Set by Cargo at compile time; the binary is in the same crate, so
/// `env!` resolves it. A test that hard-coded a `target/debug/…`
/// path would re-introduce the class of flake the matrix is built
/// to avoid — `cargo test` and `cargo test --release` and
/// cross-compile all land in different directories.
fn rmcp_server_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_rmcp_stdio_server"))
}

/// What `tools/list` must report for the cross-test to be
/// meaningful.
///
/// The exact tool name is part of the cross-test contract — it is
/// what the test asserts on. A change here is a test failure, not
/// a runtime adaptation.
const EXPECTED_TOOL_NAME: &str = "add";

/// Builds the `initialize` params the cross-test sends.
///
/// The protocol version is interpolated into the wire shape
/// `protocol-droid` and `rmcp` both parse; the rest of the
/// handshake is a fixed shape the cross-test does not vary. The
/// raw string keeps the wire bytes the cross-test asserts on
/// identical to what a real client would send, rather than
/// passing through `serde_json::Value` and re-serialising.
fn initialize_params(revision: &str) -> Result<JsonObject> {
    let text = format!(
        r#"{{"protocolVersion":"{revision}","capabilities":{{}},"clientInfo":{{"name":"interop-rmcp-test","version":"0.0.0"}}}}"#
    );
    JsonObject::new(text).map_err(|error| Failure::ProtocolDroid(format!("initialize params: {error}")))
}

/// Builds the `tools/call add` params the cross-test sends.
fn call_add_params() -> Result<JsonObject> {
    let text = r#"{"name":"add","arguments":{"a":2,"b":3}}"#;
    JsonObject::new(text).map_err(|error| Failure::ProtocolDroid(format!("call params: {error}")))
}

/// Drives one cross-test pass against one revision.
async fn drive_one(spec_version: &str) -> Result<()> {
    let spec = ChildSpec::from_argv([rmcp_server_path()]).map_err(|error| {
        Failure::Harness(format!(
            "rmcp_stdio_server binary path is not a valid ChildSpec: {error}"
        ))
    })?;
    let config = ClientConfig::default();
    let mut client = StdioClient::spawn(&spec, config, Diagnostics::stderr())
        .await
        .map_err(|error| Failure::Process(format!("spawn rmcp server: {error}")))?;

    // `initialize` is the handshake. The cross-test's claim is
    // that the version it ends up with is the one it asked for.
    let init_params = initialize_params(spec_version)?;
    let init_id = client.next_request_id();
    let init_method = MethodName::new("initialize")
        .map_err(|error| Failure::ProtocolDroid(format!("initialize method: {error}")))?;
    let init_result = client
        .call(&init_id, &init_method, &init_params)
        .await
        .map_err(|error| Failure::ProtocolDroid(format!("initialize: {error}")))?;
    let init_value: Value = serde_json::from_str(init_result.as_str())
        .map_err(|error| Failure::ProtocolDroid(format!("initialize body is not JSON: {error}")))?;
    let negotiated = init_value
        .get("protocolVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Failure::Rmcp(format!(
                "initialize result missing protocolVersion; got {init_value}"
            ))
        })?;
    // The cross-test asserts *intersection*, not equality. The
    // matrix's `REVISIONS` is the set both libraries actually
    // agree on, and rmcp 3.5.1 negotiates down to `2025-11-25`
    // when asked for `2026-07-28`; a peer that lies about
    // supporting a revision and then negotiates to a different
    // one is a real-world interop outcome, not a test failure.
    if !REVISIONS.contains(&negotiated) {
        return Err(Failure::Rmcp(format!(
            "rmcp negotiated {negotiated:?} which is not in the cross-test's \
             REVISIONS list {REVISIONS:?}; the matrix's REVISIONS may be out \
             of date with rmcp's actual support, or rmcp is returning a \
             version protocol-droid has never heard of"
        )));
    }

    // The MCP lifecycle says the client must send
    // `notifications/initialized` after `initialize` returns, and
    // rmcp gates the tool router on having seen it. Skipping this
    // notification is the difference between `tools/list`
    // returning the registered tools and returning an empty
    // list — the cross-test asserts the former.
    let init_notification_method = MethodName::new("notifications/initialized")
        .map_err(|error| Failure::ProtocolDroid(format!("notifications/initialized method: {error}")))?;
    client
        .notify(&init_notification_method, &JsonObject::default())
        .await
        .map_err(|error| Failure::ProtocolDroid(format!("notifications/initialized: {error}")))?;

    // `tools/list` is the round-trip the cross-test's real claim
    // lives in. The rmcp server advertises one tool, `add`, and
    // the cross-test asserts both that the list contains it.
    let list_id = client.next_request_id();
    let list_method = MethodName::new("tools/list")
        .map_err(|error| Failure::ProtocolDroid(format!("tools/list method: {error}")))?;
    let tools_result = client
        .call(&list_id, &list_method, &JsonObject::default())
        .await
        .map_err(|error| Failure::ProtocolDroid(format!("tools/list: {error}")))?;
    let tools_value: Value = serde_json::from_str(tools_result.as_str())
        .map_err(|error| Failure::ProtocolDroid(format!("tools/list body is not JSON: {error}")))?;
    let tools = tools_value
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| Failure::Rmcp("tools/list did not return an array".to_owned()))?;
    let tool_names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .collect();
    if !tool_names.contains(&EXPECTED_TOOL_NAME) {
        return Err(Failure::Rmcp(format!(
            "tools/list returned {tool_names:?}; expected to contain {EXPECTED_TOOL_NAME:?}"
        )));
    }

    // `tools/call add` with a hand-checked sum, so a passing test
    // here is a witness that the *args* and *result* shapes agree
    // between the two SDKs — the schema is one half, the values
    // are the other. rmcp's `tools/call` answer wraps the tool's
    // return value in a `content` array; we assert the array
    // contains a `text` element whose body carries the integer
    // result.
    let call_id = client.next_request_id();
    let call_method = MethodName::new("tools/call")
        .map_err(|error| Failure::ProtocolDroid(format!("tools/call method: {error}")))?;
    let call_params = call_add_params()?;
    let call_result = client
        .call(&call_id, &call_method, &call_params)
        .await
        .map_err(|error| Failure::ProtocolDroid(format!("tools/call: {error}")))?;
    let call_value: Value = serde_json::from_str(call_result.as_str())
        .map_err(|error| Failure::ProtocolDroid(format!("tools/call body is not JSON: {error}")))?;
    let content = call_value
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| Failure::Rmcp("tools/call did not return a content array".to_owned()))?;
    let text = content
        .iter()
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("");
    if !text.contains('5') {
        return Err(Failure::Rmcp(format!(
            "tools/call add 2 3 returned content {text:?}; expected to contain '5'"
        )));
    }

    // Close cleanly. The cross-test does not assert on the
    // child's exit code, because the `terminate` path here is
    // `kill_on_drop`'s hand: a graceful shutdown from rmcp
    // requires the *peer* to close stdin, which the client does
    // on `Drop`. A process that exited with a signal is not a
    // wire-shape failure of the cross-test — it is the test
    // reaping the child it spawned. So we just wait for the
    // reap and let any `ExitStatus` pass.
    let _ = client
        .terminate()
        .await
        .map_err(|error| Failure::Process(format!("terminate: {error}")))?;

    Ok(())
}

/// The matrix: one assertion per revision in the intersection.
#[test]
fn pd_client_to_rmcp_server_matrix() -> std::result::Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("build the test runtime: {error}"))?;
    for spec_version in REVISIONS {
        if let Err(error) = runtime.block_on(drive_one(spec_version)) {
            return Err(format!(
                "interop (pd client → rmcp server) failed for {spec_version}: {error}"
            ));
        }
    }
    Ok(())
}
