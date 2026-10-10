//! Integration tests for the `protocol-droid` CLI's top-level
//! subcommands.
//!
//! The unit tests in `src/bridge.rs` and `src/conformance.rs`
//! exercise argument parsing in-process. This file exercises
//! the *binary* — the path a consumer of the CLI actually
//! takes — and asserts each subcommand is reachable, the
//! `--help` text renders, and the exit code is zero.
//!
//! # Why `--help`
//!
//! `--help` is the only subcommand-level invocation that is
//! non-destructive (it does not start a server, open a
//! socket, or run the conformance suite), and `clap` exits
//! with status 0 on a help request. A test that runs the
//! real binary with `--help` proves the binary builds, the
//! embedded SHA and date the build script bakes in are
//! well-formed, and the clap-derived arg parser accepts the
//! subcommand. The non-zero-cost subcommands (a real
//! `bridge` or `conformance` run) are exercised in the
//! conformance / interop CI workflows, not here; the cost
//! of those runs in a unit-test context is what kept the
//! per-subcommand integration test this small.
//!
//! # Path resolution
//!
//! The binary path is computed from `CARGO_BIN_EXE_protocol-droid`,
//! which Cargo sets for integration tests in the same crate.
//! A test that hard-codes `target/debug/protocol-droid` would
//! re-introduce the class of flake the interop crate's
//! `pd_stdio_server_path()` exists to avoid.

/// Resolves the path to the `protocol-droid` binary the test
/// should spawn.
fn protocol_droid_bin() -> &'static std::path::Path {
    std::path::Path::new(env!("CARGO_BIN_EXE_protocol-droid"))
}

/// Drives one subcommand's `--help` invocation end-to-end.
///
/// # Errors
///
/// Returns `Err` when the binary could not be spawned, the
/// spawned process exited non-zero, or stdout did not contain
/// the subcommand name the test was asserting on. The
/// `String` payload carries the raw stdout/stderr so a
/// failing run prints the binary's view of the world, not
/// just a "test failed".
fn assert_help_contains(subcommand: &str, expected_fragment: &str) -> Result<(), String> {
    let output = std::process::Command::new(protocol_droid_bin())
        .arg(subcommand)
        .arg("--help")
        .output()
        .map_err(|error| format!("failed to spawn `protocol-droid {subcommand} --help`: {error}"))?;

    let status = output.status;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !status.success() {
        return Err(format!(
            "`protocol-droid {subcommand} --help` exited with {status}; \
             stdout: {stdout}; stderr: {stderr}"
        ));
    }

    if !stdout.contains(expected_fragment) && !stderr.contains(expected_fragment) {
        return Err(format!(
            "`protocol-droid {subcommand} --help` did not contain {expected_fragment:?}; \
             stdout: {stdout}; stderr: {stderr}"
        ));
    }

    Ok(())
}

/// `bridge --help` reaches the help text and renders a
/// fragment that names the subcommand.
#[test]
fn bridge_help_renders() -> Result<(), String> {
    assert_help_contains("bridge", "bridge")
}

/// `conformance --help` reaches the help text and renders a
/// fragment that names the subcommand.
#[test]
fn conformance_help_renders() -> Result<(), String> {
    assert_help_contains("conformance", "conformance")
}

/// The binary's own `--version` text is rendered through
/// the build script's `version` arg, which sets the crate
/// version plus the embedded SHA. The assertion is on the
/// crate name (the SHA's exact value is a build-time
/// artefact, and asserting on it would flake on a CI
/// machine whose `git` produced a different short hash).
#[test]
fn version_text_is_rendered() -> Result<(), String> {
    let output = std::process::Command::new(protocol_droid_bin())
        .arg("--version")
        .output()
        .map_err(|error| format!("failed to spawn `protocol-droid --version`: {error}"))?;

    if !output.status.success() {
        return Err(format!(
            "`protocol-droid --version` exited with {}; \
             stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.contains("protocol-droid") {
        return Err(format!(
            "`protocol-droid --version` did not contain the crate name; stdout: {stdout}"
        ));
    }
    Ok(())
}
