//! The reference MCP client the conformance suite exercises.
//!
//! The suite's client mode launches a program with the server's URL as its
//! argument and the scenario's name in `MCP_CONFORMANCE_SCENARIO`; the
//! scenario stands up a mock server and judges what this program sends. The
//! revision under test arrives in `MCP_CONFORMANCE_PROTOCOL_VERSION`, and
//! decides whether the stateful `initialize` lifecycle or the stateless
//! per-request one is spoken.
//!
//! Everything here goes through the library's own [`Client`] where the mock
//! allows it — that is the point of the fixture: a client conformance result is
//! only worth having if it measures the code a consumer links, not a script
//! written beside it.
//!
//! ```text
//! cargo run --example conformance_reference_client -- <server-url>
//! ```

pub mod auth_scenarios;
pub mod scenarios;

use std::process::ExitCode;

fn main() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("could not start a runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    let scenario = std::env::var("MCP_CONFORMANCE_SCENARIO").unwrap_or_default();
    let Some(url) = std::env::args().nth(1) else {
        eprintln!(
            "usage: MCP_CONFORMANCE_SCENARIO=<name> conformance_reference_client <server-url>"
        );
        return ExitCode::FAILURE;
    };
    match runtime.block_on(scenarios::run(&scenario, &url)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("scenario `{scenario}` failed: {error}");
            ExitCode::FAILURE
        }
    }
}
