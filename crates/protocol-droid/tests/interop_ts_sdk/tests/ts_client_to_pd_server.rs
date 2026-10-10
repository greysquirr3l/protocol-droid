//! TS SDK client → `protocol-droid` server.
//!
//! The second half of the T22 cross-test for the TypeScript
//! SDK. Spawns the TS SDK's stdio client
//! (`fixtures/client.mjs`, driven by
//! `@modelcontextprotocol/sdk` `1.32.1`) as a child of the
//! Rust test, with the `protocol-droid` `stdio_server`
//! example as the *grandchild* of the test (the TS client's
//! own child). The TS client connects, runs `tools/list`
//! and a `tools/call shout` against the `shout` tool the
//! example advertises, and prints a single JSON summary to
//! stdout. The Rust test parses that summary and asserts on
//! its fields.
//!
//! # Why this direction is a single test, not a matrix
//!
//! The TS SDK's `Client` class always offers its
//! `LATEST_PROTOCOL_VERSION` (currently `2025-11-25` at the
//! pinned `1.32.1`) in the `initialize` it sends, and there
//! is no public API to override the offered version. The
//! `protocol-droid` server's negotiated version is therefore
//! `2025-11-25` for any run of this test. A matrix in this
//! direction would be one row, not four; the per-revision
//! variation lives in the other leg (`pd_client_to_ts_server_matrix`).
//!
//! A failure is either a `protocol-droid` codec bug or a TS
//! SDK wire shape, and the finding goes into `FINDINGS.md`
//! (T22 must-have) with the spec citation deciding which one
//! is right.

use interop_ts_sdk::{Failure, Result};
use serde_json::Value;

/// Path to the `node` binary the test spawns.
///
/// `node` must be on `PATH`; a fixture that hard-codes an
/// absolute path to a specific `node` install would make the
/// matrix non-reproducible across machines.
fn node_path() -> std::path::PathBuf {
    std::path::PathBuf::from("node")
}

/// Path to the TS client fixture the test spawns.
///
/// `fixtures/client.mjs` lives next to this test crate. The
/// path is computed from `CARGO_MANIFEST_DIR` so a copy of
/// the crate into another tree (a CI cache, a fork) still
/// works.
fn ts_client_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("client.mjs")
}

/// Path to the `protocol-droid` `stdio_server` example the
/// TS client drives.
///
/// The example is built by the `protocol-droid` workspace,
/// not by this crate, so a hard-coded `target/debug/…`
/// path would re-introduce the class of flake the matrix
/// is built to avoid.
///
/// An env override (`INTEROP_TS_SDK_PD_STDIO_SERVER`)
/// matches the rmcp interop crate's convention and lets
/// the CI workflow point at a freshly-built example
/// binary without re-substituting a path.
fn pd_stdio_server_path() -> std::path::PathBuf {
    if let Ok(override_path) = std::env::var("INTEROP_TS_SDK_PD_STDIO_SERVER") {
        return std::path::PathBuf::from(override_path);
    }
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .expect("walk up to the workspace root")
        .to_path_buf();
    let target = std::env::var("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| workspace.join("target"));
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".to_owned());
    target
        .join(profile)
        .join("examples")
        .join(if cfg!(windows) {
            "stdio_server.exe"
        } else {
            "stdio_server"
        })
}

/// Drives the cross-test: spawn the TS client, wait for it
/// to finish, parse the JSON it prints, and assert on the
/// fields the cross-test cares about.
///
/// # Errors
///
/// `Failure::Process` when the child could not be spawned or
/// exited non-zero. `Failure::TsSdk` when the child's output
/// is missing fields the TS SDK is supposed to produce, or
/// when those fields are not the shape the cross-test
/// expects. `Failure::ProtocolDroid` when the cross-test's
/// understanding of what `protocol-droid` advertises differs
/// from what the example actually exposes.
async fn drive_one() -> Result<()> {
    let server_path = pd_stdio_server_path();
    if !server_path.exists() {
        return Err(Failure::Harness(format!(
            "protocol-droid stdio_server example is not at {}; \
             run `cargo build --example stdio_server -p protocol-droid` \
             before this test, or set INTEROP_TS_SDK_PD_STDIO_SERVER",
            server_path.display()
        )));
    }

    let mut child = tokio::process::Command::new(node_path())
        .arg(ts_client_path())
        .arg(&server_path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| Failure::Process(format!("spawn ts-sdk client: {error}")))?;

    // The TS client prints a single JSON line on stdout and
    // exits. The pipe buffers that one line; the test reads
    // it back, parses it, and asserts.
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Failure::Process("ts-sdk client stdout was not piped".to_owned()))?;
    let mut stdout_buf = Vec::new();
    use tokio::io::AsyncReadExt;
    let mut stdout_reader = stdout;
    stdout_reader
        .read_to_end(&mut stdout_buf)
        .await
        .map_err(|error| Failure::Process(format!("read ts-sdk client stdout: {error}")))?;

    let status = child
        .wait()
        .await
        .map_err(|error| Failure::Process(format!("wait ts-sdk client: {error}")))?;
    if !status.success() {
        return Err(Failure::Process(format!(
            "ts-sdk client exited with {status}; stdout: {}",
            String::from_utf8_lossy(&stdout_buf)
        )));
    }

    let summary: Value = serde_json::from_slice(&stdout_buf).map_err(|error| {
        Failure::TsSdk(format!(
            "ts-sdk client stdout is not JSON: {error}; raw: {}",
            String::from_utf8_lossy(&stdout_buf)
        ))
    })?;

    // The TS SDK's `getServerVersion()` returns
    // `Implementation { name, version }`. The
    // `stdio_server` example uses the `#[protocol_droid::server]`
    // attribute without a `name = "..."` override, so the
    // default name is `<module_path>::<impl_name>` —
    // `stdio_server::Greeter` for this example. The
    // cross-test asserts that the TS SDK sees the same
    // default the example produces, not a different
    // string; the default-name behaviour is itself a
    // cross-test claim.
    let server_identity = summary
        .get("server_identity")
        .ok_or_else(|| Failure::TsSdk("client output is missing server_identity".to_owned()))?;
    let server_name = server_identity
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::TsSdk("server_identity is missing name".to_owned()))?;
    let server_version = server_identity
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::TsSdk("server_identity is missing version".to_owned()))?;
    assert_eq!(
        server_name, "stdio_server::Greeter",
        "TS SDK saw server name {server_name:?}, expected `stdio_server::Greeter`"
    );
    assert!(
        !server_version.is_empty(),
        "TS SDK saw empty server version"
    );

    // `tools/list` should include the `shout` tool the
    // example advertises. Other tools (e.g. `negotiated`)
    // may also appear; the cross-test only asserts on the
    // presence of `shout`, not on the absence of any other
    // tool.
    let tools = summary
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| Failure::TsSdk("client output is missing tools array".to_owned()))?;
    let tool_names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .collect();
    assert!(
        tool_names.contains(&"shout"),
        "TS SDK did not see `shout` in tools/list; got {tool_names:?}"
    );

    // `tools/call shout` should return a `content[0].text`
    // whose body is the JSON-serialised `Shout { text, length }`
    // the example emits. The cross-test asserts the shape
    // (a JSON object with `text` uppercased and a numeric
    // `length`) rather than the exact string, because the
    // serde-derived `Shout` could change field order
    // between revisions without breaking the wire.
    let call_result = summary
        .get("call_result")
        .ok_or_else(|| Failure::TsSdk("client output is missing call_result".to_owned()))?;
    let text = call_result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(|item| item.get("text"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Failure::TsSdk(format!(
                "call_result is missing content[0].text: {call_result}"
            ))
        })?;
    let shout: Value = serde_json::from_str(text).map_err(|error| {
        Failure::ProtocolDroid(format!(
            "shout result text is not JSON: {error}; raw: {text}"
        ))
    })?;
    let shouted_text = shout
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::ProtocolDroid("shout result is missing text field".to_owned()))?;
    let length = shout
        .get("length")
        .and_then(Value::as_u64)
        .ok_or_else(|| Failure::ProtocolDroid("shout result is missing length field".to_owned()))?;
    assert_eq!(
        shouted_text, "HELLO FROM TS-SDK",
        "shout did not uppercase; got {shouted_text:?}"
    );
    assert_eq!(
        length, 17,
        "shout reported length {length}, expected 17 for `hello from ts-sdk`"
    );

    Ok(())
}

/// The matrix entry point. A failure here fails the run;
/// the diff is the failing test's output.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ts_client_to_pd_server_matrix() -> Result<()> {
    drive_one().await
}
