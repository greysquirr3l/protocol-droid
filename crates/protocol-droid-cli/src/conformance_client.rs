//! The client half of `conformance self`: this crate's reference client, run
//! against the suite's mock servers.
//!
//! The suite's client mode launches a program with the mock's URL, so the
//! program has to exist before it is named. It is built once, up front, and
//! the path cargo reports for it is what the suite is told to run — not
//! `cargo run`, which would make every scenario pay cargo's up-to-date check
//! and mix its output into the suite's.

use std::ffi::OsString;
use std::process::Stdio;

use anyhow::Context as _;

/// The scenarios a revision requires of a client, from the suite's own
/// requirements file for that revision, minus the OAuth ones this fixture does
/// not run yet.
///
/// A static table because the suite has no way to list "the client scenarios a
/// revision requires" short of reading a YAML file that ships inside the npm
/// package; the pin file is what makes this table checkable, and a bump to it
/// is a deliberate act that revisits this list.
#[must_use]
pub fn client_scenarios(spec_version: &str) -> &'static [&'static str] {
    match spec_version {
        "2025-11-25" => &[
            "initialize",
            "tools_call",
            "elicitation-sep1034-client-defaults",
            "sse-retry",
            "auth/metadata-default",
            "auth/metadata-var1",
            "auth/metadata-var2",
            "auth/metadata-var3",
            "auth/basic-cimd",
            "auth/scope-from-www-authenticate",
            "auth/scope-from-scopes-supported",
            "auth/scope-omitted-when-undefined",
            "auth/scope-step-up",
            "auth/scope-retry-limit",
            "auth/token-endpoint-auth-basic",
            "auth/token-endpoint-auth-post",
            "auth/token-endpoint-auth-none",
            "auth/pre-registration",
        ],
        "2025-06-18" => &["initialize", "tools_call"],
        "2026-07-28" => &[
            "tools_call",
            "request-metadata",
            "http-standard-headers",
            "http-custom-headers",
            "http-invalid-tool-headers",
            "sep-2322-client-request-state",
            "json-schema-ref-no-deref",
            "auth/metadata-default",
            "auth/metadata-var1",
            "auth/metadata-var2",
            "auth/metadata-var3",
            "auth/basic-cimd",
            "auth/scope-from-www-authenticate",
            "auth/scope-from-scopes-supported",
            "auth/scope-omitted-when-undefined",
            "auth/scope-step-up",
            "auth/scope-retry-limit",
            "auth/token-endpoint-auth-basic",
            "auth/token-endpoint-auth-post",
            "auth/token-endpoint-auth-none",
            "auth/pre-registration",
            "auth/resource-mismatch",
            "auth/offline-access-scope",
            "auth/offline-access-not-supported",
            "auth/authorization-server-migration",
            "auth/iss-supported",
            "auth/iss-not-advertised",
            "auth/iss-supported-missing",
            "auth/iss-wrong-issuer",
            "auth/iss-unexpected",
            "auth/iss-normalized",
            "auth/metadata-issuer-mismatch",
        ],
        // `2024-11-05` and `2025-03-26` have no client scenarios at the pinned
        // suite version, which `ALLOWLIST.md` records.
        _ => &[],
    }
}

/// Builds the reference client and returns the path to its executable.
///
/// # Errors
///
/// A build failure, or cargo reporting no executable for the example.
pub async fn build_reference_client() -> anyhow::Result<std::path::PathBuf> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let output = tokio::process::Command::new(cargo)
        .args(["build", "--quiet", "--message-format=json-render-diagnostics"])
        .arg("--manifest-path")
        .arg(super::conformance::workspace_manifest_path())
        .args(["-p", "protocol-droid", "--example", "conformance_reference_client"])
        .args(["--features", "http,oauth,random-ids"])
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .await
        .context("could not run cargo to build the reference client")?;
    if !output.status.success() {
        anyhow::bail!("the reference client did not build");
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|message| {
            message.pointer("/target/name").and_then(serde_json::Value::as_str)
                == Some("conformance_reference_client")
        })
        .filter_map(|message| {
            message
                .get("executable")
                .and_then(serde_json::Value::as_str)
                .map(std::path::PathBuf::from)
        })
        .next_back()
        .context("cargo built the reference client but reported no executable for it")
}

/// The suite's argv for one client scenario.
#[must_use]
pub fn client_argv(pinned: &str, client: &std::path::Path, spec_version: &str, scenario: &str) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec![
        "--yes".into(),
        format!("@modelcontextprotocol/conformance@{pinned}").into(),
        "client".into(),
        "--command".into(),
        client.as_os_str().to_owned(),
        "--spec-version".into(),
        spec_version.into(),
        "--scenario".into(),
        scenario.into(),
    ];
    argv.shrink_to_fit();
    argv
}

#[cfg(test)]
mod tests {
    use super::{client_argv, client_scenarios};

    #[test]
    fn every_stateful_revision_with_scenarios_runs_the_basics() {
        for version in ["2025-06-18", "2025-11-25"] {
            assert!(client_scenarios(version).contains(&"initialize"), "{version}");
        }
        assert!(client_scenarios("2024-11-05").is_empty());
    }

    #[test]
    fn the_stateless_revision_does_not_run_initialize() {
        assert!(!client_scenarios("2026-07-28").contains(&"initialize"));
    }

    #[test]
    fn argv_names_the_scenario_and_the_revision() {
        let argv = client_argv("1.2.3", std::path::Path::new("/bin/client"), "2025-11-25", "initialize");
        let text: Vec<String> = argv.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(text.windows(2).any(|w| w == ["--scenario", "initialize"]));
        assert!(text.windows(2).any(|w| w == ["--spec-version", "2025-11-25"]));
        assert!(text.iter().any(|a| a == "@modelcontextprotocol/conformance@1.2.3"));
    }
}
