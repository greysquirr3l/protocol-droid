//! A minimal MCP server that speaks over Streamable HTTP.
//!
//! The stdio counterpart is
//! [`stdio_server.rs`](protocol_droid::adapters::primary::stdio::serve_stdio);
//! this file shows the **HTTP** counterpart — the same `Server`
//! built with the `#[server]` / `#[tool]` macros, but mounted
//! on a `Router` by [`HttpBuilder`] and bound to a TCP listener
//! instead of being driven over a child's standard streams.
//!
//! # What this is
//!
//! The smallest thing an external MCP client can actually
//! reach over HTTP. It wires the four pieces a Streamable HTTP
//! server needs:
//!
//! 1. a `Server` carrying at least one real tool, built with
//!    the `#[server]` / `#[tool]` macros,
//! 2. a [`Dispatcher`] that routes a method name to that server,
//! 3. a [`PeerSession`] carrying the negotiated revision and
//!    the `FeatureProfile` every feature gate in the crate
//!    consults, and
//! 4. an [`HttpBuilder`] that assembles the `Router`, binds
//!    the listener, and runs the per-session loop.
//!
//! # What this is not
//!
//! The
//! [`conformance_reference_server.rs`](https://github.com/greysquirr3l/protocol-droid/blob/main/crates/protocol-droid/examples/conformance_reference_server.rs)
//! example is the conformance target — it exposes every
//! feature the suite reads and is wired to the `conformance
//! self` subcommand. This file is the consumer-facing
//! counterpart: one tool, one route, no conformance surface.
//! A real consumer who needs the full feature set copies
//! the conformance target; a consumer who needs a focused
//! HTTP service starts here.
//!
//! # How to run it
//!
//! ```text
//! cargo run -p protocol-droid --example http_server --features http
//! ```
//!
//! A one-shot probe, after the server is running:
//!
//! ```text
//! curl -X POST -H 'content-type: application/json' \
//!   -H 'accept: application/json, text/event-stream' \
//!   -d '{"jsonrpc":"2.0","id":1,"method":"initialize",
//!        "params":{"protocolVersion":"2025-06-18",
//!                  "capabilities":{},
//!                  "clientInfo":{"name":"curl","version":"1.0"}}}' \
//!   http://127.0.0.1:3000/mcp
//! ```
//!
//! Override the bind address with `MCP_ADDR=0.0.0.0:8080`.
//! A bad value is reported on stderr and the default
//! (`127.0.0.1:3000`) is used.

use std::net::SocketAddr;

use protocol_droid::adapters::primary::transport::http::HttpBuilder;
use serde::Serialize;

// `#[tool]` is called once per request rather than once per server, and the
// function is `async fn` because the call site `.await`s it. A body that
// did no real work would still hand back a future, and the compiler would
// warn about an unused-async body. `tokio::task::yield_now` is the
// one-byte future the lint accepts: it costs one task queue turn and
// changes nothing else, so a fixture whose tools are hot-paths in a
// benchmark does not regress here.
async fn yield_to_runtime() {
    tokio::task::yield_now().await;
}

#[derive(Serialize)]
struct Shout {
    /// The text, uppercased.
    text: String,
    /// The character count of the original `text`.
    length: usize,
}

struct Greeter;

#[protocol_droid::server]
impl Greeter {
    /// Uppercases `text` and reports how many characters it had.
    ///
    /// The character count is the part worth having: it forces
    /// the handler to do real work and the response to carry a
    /// second field. A tool that just returned `text` would
    /// pass every assertion, including the "the response is
    /// empty" one, and the test would not catch the bug.
    #[protocol_droid::tool]
    async fn shout(text: String) -> Result<Shout, String> {
        yield_to_runtime().await;
        Ok(Shout {
            length: text.chars().count(),
            text: text.to_uppercase(),
        })
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = bind_addr()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(serve(addr))
}

async fn serve(addr: SocketAddr) -> Result<(), Box<dyn std::error::Error>> {
    let server = Greeter.into_server()?;
    let report = HttpBuilder::new(server).addr(addr).serve().await?;
    eprintln!("http_server: served {report:?}");
    Ok(())
}

/// Reads the bind address from the `MCP_ADDR` env var,
/// or falls back to `127.0.0.1:3000`. A malformed
/// `MCP_ADDR` is reported on stderr and the default
/// is used.
fn bind_addr() -> Result<SocketAddr, Box<dyn std::error::Error>> {
    let raw = std::env::var("MCP_ADDR").unwrap_or_else(|_| "127.0.0.1:3000".to_owned());
    match raw.parse() {
        Ok(addr) => Ok(addr),
        Err(error) => {
            eprintln!("http_server: MCP_ADDR={raw:?} is not a valid socket address: {error}");
            Ok("127.0.0.1:3000".parse()?)
        }
    }
}
