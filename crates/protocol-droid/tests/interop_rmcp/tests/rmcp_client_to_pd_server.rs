//! `rmcp` client → `protocol-droid` server, across revisions.
//!
//! The second half of the T22 cross-test. Spawns the
//! `protocol-droid` `stdio_server` example as a child, then drives
//! it with an `rmcp` client — `initialize`, `tools/list`,
//! `tools/call shout` — and asserts each answer matches the
//! shape protocol-droid advertises. A failure is either an
//! `rmcp` codec bug or a `protocol-droid` wire shape, and the
//! finding goes into `FINDINGS.md` (T22 must-have) with the spec
//! citation deciding which one is right.

use std::process::Stdio;

use interop_rmcp::{Failure, Result, REVISIONS};
use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, ClientConfig, Implementation, ProtocolVersion},
    object,
    transport::{ConfigureCommandExt, TokioChildProcess},
};
use tokio::process::Command;

/// Path to the `protocol-droid` `stdio_server` example this test
/// crate drives.
///
/// The example is built by the `protocol-droid` workspace, not
/// by this crate, so a hard-coded `target/debug/…` path would
/// re-introduce the class of flake the matrix is built to
/// avoid. `cargo build --example stdio_server -p protocol-droid`
/// before the test runs is the contract; CI does that as part
/// of the workflow's `build` step.
///
/// The crate this test lives in is at
/// `crates/protocol-droid/tests/interop_rmcp/`, so the
/// `CARGO_MANIFEST_DIR`'s fourth ancestor is the workspace
/// root. That is where `cargo build` writes the example's
/// binary when the workspace is its only resolver.
///
/// `INTEROP_RMCP_PD_STDIO_SERVER` overrides the resolved path
/// so a debugging wrapper (a script that tees stdin through a
/// file before execing the real binary, for example) can be
/// substituted without recompiling.
fn pd_stdio_server_path() -> std::path::PathBuf {
    if let Ok(override_path) = std::env::var("INTEROP_RMCP_PD_STDIO_SERVER") {
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

/// The tool this cross-test calls.
///
/// `protocol-droid`'s `stdio_server` example registers a tool
/// named `shout` (it lower-cases the `shout` method name in the
/// macro). The `text` argument is the single string the example
/// expects, and the return is a `Shout` struct the test asserts
/// on. Picking `shout` over an invented tool means a
/// regression in either SDK fails the test rather than the
/// fixture.
const EXPECTED_TOOL_NAME: &str = "shout";

/// Drives one cross-test pass against one revision.
async fn drive_one(spec_version: &str) -> Result<()> {
    let server_path = pd_stdio_server_path();
    if !server_path.exists() {
        return Err(Failure::Process(format!(
            "protocol-droid `stdio_server` example binary not found at {server_path:?}; \
             run `cargo build --example stdio_server -p protocol-droid` first"
        )));
    }

    // Spawn the protocol-droid stdio server as a child of the
    // rmcp transport. The `serve` call below drives the
    // handshake and returns a service handle whose high-level
    // methods (`list_all_tools`, `call_tool`) write JSON-RPC
    // frames and read the answers.
    //
    // For debugging, a wrapper that tees stdin through a file
    // can be substituted: `Command::new("/tmp/tee_server.sh")`
    // when `RUST_LOG=trace` is set, so the failing request is
    // visible without changing the test code.
    let command = Command::new(&server_path).configure(|cmd| {
        // The stdio_server example reads the negotiated
        // protocol version from the client's `initialize`
        // params; we do not pass a `--revision` flag, so the
        // matrix's `spec_version` is exactly what the wire
        // shape asserts.
        cmd.stdout(Stdio::piped())
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .env("RUST_LOG", "warn");
    });

    // `rmcp`'s default `()` handler picks the newest protocol
    // version it knows about (`2026-07-28`), which
    // `protocol-droid`'s `stdio_server` example refuses because
    // SEP-2575 removed `initialize` from that revision.
    //
    // The cross-test's matrix only exercises revisions that
    // both libraries agree on, and the test we *can* drive
    // requires a client that explicitly asks for one of those
    // revisions. `ClientConfig::new(...).with_protocol_version(...)`
    // is the documented override: rmcp's blanket `ClientHandler
    // for ClientConfig` impl reads the version out of the
    // config and sends it in `initialize`.
    let version = match spec_version {
        "2024-11-05" => ProtocolVersion::V_2024_11_05,
        "2025-03-26" => ProtocolVersion::V_2025_03_26,
        "2025-06-18" => ProtocolVersion::V_2025_06_18,
        "2025-11-25" => ProtocolVersion::V_2025_11_25,
        "2026-07-28" => ProtocolVersion::V_2026_07_28,
        other => {
            return Err(Failure::Harness(format!(
                "spec_version {other:?} has no corresponding ProtocolVersion constant"
            )));
        }
    };
    let client_config = ClientConfig::new(
        rmcp::model::ClientCapabilities::default(),
        Implementation::new("interop-rmcp", "0.0.0"),
    )
    .with_protocol_version(version);

    let client = client_config
        .serve(TokioChildProcess::new(command).map_err(|error| {
            Failure::Process(format!("spawn protocol-droid stdio_server: {error}"))
        })?)
        .await
        .map_err(|error| Failure::Rmcp(format!("rmcp handshake: {error}")))?;

    // The peer info is rmcp's readback of `initialize`. The
    // cross-test asserts the negotiated revision is in the
    // matrix's `REVISIONS` list, for the same reason the
    // inverse direction does: an SDK that returns a revision
    // neither library claims is a real-world interop outcome
    // we want to record, not paper over.
    let peer_info = client
        .peer_info()
        .ok_or_else(|| Failure::Rmcp("rmcp peer_info was not set after handshake".to_owned()))?;
    let negotiated = peer_info.protocol_version.to_string();
    if !REVISIONS.contains(&negotiated.as_str()) {
        return Err(Failure::ProtocolDroid(format!(
            "protocol-droid negotiated {negotiated:?} which is not in the cross-test's \
             REVISIONS list {REVISIONS:?}; either the matrix is out of date with \
             protocol-droid's actual support, or the stdio_server example returned a \
             version the library does not advertise"
        )));
    }

    // The cross-test's first wire claim: `tools/list` must
    // contain `shout`. The exact name is part of the contract.
    let tools = client
        .list_all_tools()
        .await
        .map_err(|error| Failure::Rmcp(format!("list_all_tools: {error}")))?;
    let tool_names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    if !tool_names.contains(&EXPECTED_TOOL_NAME) {
        return Err(Failure::ProtocolDroid(format!(
            "protocol-droid's tools/list returned {tool_names:?}; expected to contain \
             {EXPECTED_TOOL_NAME:?}"
        )));
    }

    // The second claim: `tools/call shout` with a known input
    // returns the uppercased string. rmcp's `call_tool`
    // returns a `CallToolResult` whose `content` array mirrors
    // what the protocol-droid server wrote; the cross-test
    // asserts the text contains the uppercased form.
    let tool_result = client
        .call_tool(CallToolRequestParams::new(EXPECTED_TOOL_NAME).with_arguments(
            object!({ "text": "hello droid" }),
        ))
        .await
        .map_err(|error| Failure::Rmcp(format!("call_tool shout: {error}")))?;
    let content = tool_result
        .content
        .iter()
        .filter_map(|item| item.as_text())
        .map(|t| t.text.as_str())
        .collect::<Vec<_>>()
        .join("");
    if !content.contains("HELLO DROID") {
        return Err(Failure::ProtocolDroid(format!(
            "tools/call shout 'hello droid' returned content {content:?}; \
             expected to contain 'HELLO DROID'"
        )));
    }

    // Tear the rmcp client down cleanly. `cancel()` is the
    // documented way to close the JSON-RPC session; a dropped
    // client would also close the pipe, but `cancel` lets the
    // server see the `notifications/cancelled` and clean up
    // its own state.
    let _ = spec_version;
    client
        .cancel()
        .await
        .map_err(|error| Failure::Rmcp(format!("cancel: {error}")))?;

    Ok(())
}

/// The matrix: one assertion per revision in the intersection.
#[test]
fn rmcp_client_to_pd_server_matrix() -> std::result::Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("build the test runtime: {error}"))?;
    for spec_version in REVISIONS {
        if let Err(error) = runtime.block_on(drive_one(spec_version)) {
            return Err(format!(
                "interop (rmcp client → pd server) failed for {spec_version}: {error}"
            ));
        }
    }
    Ok(())
}
