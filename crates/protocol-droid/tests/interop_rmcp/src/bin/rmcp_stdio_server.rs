//! rmcp stdio server used as the `rmcp` half of the interop test.
//!
//! # Why this exists in the interop crate
//!
//! `rmcp` is a library, not a binary; the interop test that pairs a
//! `protocol-droid` `StdioClient` with an `rmcp` server has to *spawn*
//! a process that serves on stdin/stdout. The process has to be in a
//! crate that has `rmcp` in its dependency closure, which the workspace
//! itself does not, and adding `rmcp` to the workspace would pull
//! `oauth2`, `sse-stream`, and the rest of rmcp's tree into every
//! member's resolver. Putting a small binary in the non-workspace
//! interop crate is the cheaper answer.
//!
//! # What this server actually does
//!
//! One tool, `add`, taking two `i64`s and reporting their sum. That is
//! the smallest shape the cross-test can round-trip: a `tools/list`
//! that returns one entry, a `tools/call` whose arguments and result
//! are both well-typed, and a `ping`. The cross-test asserts each of
//! them; any additional surface (a prompt, a resource, a sampling
//! request) is a follow-up, not a T22 must-have.
//!
//! The shape mirrors rmcp's own calculator example so the cross-test
//! is comparing the two SDKs against the same wire shape, not
//! against a stylised variant that rmcp would never produce in
//! practice.

use std::sync::Arc;

use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{Implementation, ServerCapabilities, ServerConfig},
    schemars, tool, tool_handler, tool_router,
};
use tokio::sync::Mutex;
#[allow(unused_imports)]
type _Unused = (Arc<Mutex<i64>>, ToolRouter<Counter>);

/// Two-integer sum request.
///
/// `JsonSchema` is what `#[tool]` uses to derive the JSON schema the
/// cross-test reads back from `tools/list`; the field-level
/// `description` is what the protocol-droid client will see on the
/// wire. Putting the description on the *field* (not on the tool
/// method) is rmcp's documented pattern.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SumRequest {
    /// The first number to add.
    pub a: i64,
    /// The second number to add.
    pub b: i64,
}

/// The rmcp server the cross-test drives.
///
/// Holds a single tool (`add`) and the `#[tool_router]` machinery
/// `rmcp-macros` emits when it expands `#[tool_router]` on the
/// `Counter` `impl` below. The struct is empty of application state
/// on purpose: the cross-test exercises the *wire* shape of
/// `tools/call`, not a particular tool's semantics, and a stateless
/// `add` matches what rmcp's own `Calculator` example ships. Adding
/// state would be a small change but also a small flake surface.
#[derive(Clone, Default)]
pub struct Counter {}

#[tool_router]
impl Counter {
    /// A fresh counter.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds two 64-bit integers and reports the sum as a `String`.
    ///
    /// The cross-test asserts the body of the answer contains the
    /// decimal representation of the sum, so a `String` return
    /// matches what the rmcp example macros produce and keeps the
    /// `#[tool]` body free of `serde_json::Value` plumbing the
    /// cross-test would have to undo. The args are wrapped in
    /// `Parameters<SumRequest>` because rmcp's `#[tool]` only derives
    /// a schema for a struct, not for a bare parameter list.
    #[tool(description = "Add two 64-bit integers and return their sum as a string")]
    fn add(
        Parameters(SumRequest { a, b }): Parameters<SumRequest>,
    ) -> String {
        (a + b).to_string()
    }
}

/// `ServerHandler` is `async`-flavoured by rmcp's trait
/// definition, so the macro-expanded method bodies are
/// `async fn` even when they never `.await`. The trait's own
/// return-type discipline is the contract that keeps the
/// generated `serve(...)` plumbing sound, so suppressing the
/// lint is a deliberate allow for the macro, not a fix to the
/// generated code.
#[expect(
    clippy::unused_async_trait_impl,
    reason = "rmcp's `ServerHandler` trait declares `async fn`; the \
              macro-generated bodies do not `.await` because the \
              default `ServerHandler` impls are already synchronous, \
              but the trait shape is what the rest of rmcp's API \
              matches against. Suppressing here is the documented \
              pattern for `rmcp-macros` consumers."
)]
#[tool_handler]
impl ServerHandler for Counter {
    /// The server identity and capabilities the interop test reads
    /// back. Hard-coded rather than read from `CARGO_PKG_NAME`,
    /// because the test asserts the *exact* server name and a name
    /// that depends on a build setting would re-introduce the same
    /// class of test flake the matrix is built to avoid.
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_server_info(Implementation::new("interop-rmcp", "0.0.0"))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // rmcp's `serve(transport::io::stdio())` takes ownership of
    // stdin/stdout and blocks the task on incoming JSON-RPC frames.
    // The cross-test sends `initialize`, waits for the answer,
    // sends a few `tools/list` and `tools/call` requests, then
    // closes the pipe; `waiting()` then returns and the process
    // exits.
    let service = Counter::new().serve(rmcp::transport::io::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
