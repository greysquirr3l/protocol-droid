//! A complete MCP server that speaks over stdin/stdout, in one runnable file.
//!
//! # What this is
//!
//! The smallest thing an external MCP client can actually spawn and drive. It
//! wires the four pieces a stdio server needs and then gets out of the way:
//!
//! 1. a `Server` carrying at least one real tool, built with the
//!    `#[server]` / `#[tool]` macros,
//! 2. a [`Dispatcher`] that routes a method name to that server,
//! 3. a [`PeerSession`] carrying the negotiated revision and the
//!    `FeatureProfile` every feature gate in the crate consults, and
//! 4. a [`ServeLoop`] that frames, decodes, routes, and writes the replies.
//!
//! # The one rule this file exists to honour
//!
//! **stdout carries protocol frames and nothing else.** Every JSON-RPC document
//! the process emits is written by [`StdioSink`] and nothing else; diagnostics
//! go to stderr through [`Diagnostics::stderr`], and the [`ServeReport`] is
//! returned rather than printed, because this transport's stdout is the wire.
//! A single stray `println!` corrupts the stream and the client's decoder will
//! blame the server for it.
//!
//! # How to run it
//!
//! ```text
//! cargo run -p protocol-droid --example stdio_server
//! ```
//!
//! It then reads frames until the peer closes stdin, so a one-shot probe is a
//! pipe:
//!
//! ```text
//! echo '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' \
//!   | cargo run -q -p protocol-droid --example stdio_server
//! ```
//!
//! # `initialize` is answered, and this file does not implement it
//!
//! `Dispatcher` has an `initialize` arm. It reads the peer's offer, settles a
//! revision, resolves a profile from that revision and from the capabilities
//! the peer actually sent, and narrows the session to it — so every request
//! after the handshake is gated on the revision *this peer* asked for, not on
//! one this file picked at startup.
//!
//! ```text
//! $ echo '{"jsonrpc":"2.0","id":1,"method":"initialize",
//!         "params":{"protocolVersion":"2024-11-05","capabilities":{},
//!                   "clientInfo":{"name":"probe","version":"1"}}}' \
//!   | cargo run -q -p protocol-droid --example stdio_server
//! {"id":1,"jsonrpc":"2.0","result":{"capabilities":{"logging":{},
//!  "prompts":{"listChanged":true},"resources":{"listChanged":true,
//!  "subscribe":true},"tools":{"listChanged":true}},
//!  "protocolVersion":"2024-11-05",
//!  "serverInfo":{"name":"stdio_server::Greeter","version":"0.6.0"}}}
//! ```
//!
//! Note what is *absent*: no `completions`, no `elicitation`. Neither exists on
//! `2024-11-05`, and asking for one over that session is refused with
//! `-32601` rather than answered. A peer newer than this build is not refused
//! at all — it is counter-offered the newest revision whose handshake exists,
//! which is `2025-11-25` and not `2026-07-28`, because SEP-2575 removed
//! `initialize` from that one.
//! # What does work, verified
//!
//! ```text
//! $ echo '{"jsonrpc":"2.0","id":3,"method":"tools/call",
//!         "params":{"name":"shout","arguments":{"text":"hello droid"}}}' \
//!   | cargo run -q -p protocol-droid --example stdio_server
//! {"id":3,"jsonrpc":"2.0","result":{"content":[{"text":
//! "{\"length\":11,\"text\":\"HELLO DROID\"}","type":"text"}],"isError":false,
//! "structuredContent":{"length":11,"text":"HELLO DROID"}}}
//! ```
//!
//! `ping`, `tools/list`, `tools/call`, `resources/list`,
//! `resources/templates/list` and `prompts/list` all answer. The batch policy
//! is [`ProfileBatching`], so a batch frame is accepted only at a revision
//! whose profile carries `Feature::JsonRpcBatching` — which is why a
//! `2024-11-05` client, having negotiated that revision, is told `-32600` for
//! one.
//!
//! # Errors
//!
//! `main` returns a `Box<dyn Error>`. A failure to read stdin, a frame larger
//! than the ceiling, a write to a closed pipe, and a failure to build the
//! server's tool schema are all reported rather than swallowed, and each names
//! itself on stderr on the way out.

#![expect(
    clippy::unused_async,
    reason = "the `#[tool]` macro generates an `.await` over the handler, so an infallible \
              handler is still an `async fn` with nothing to await"
)]
#![expect(
    clippy::unused_async_trait_impl,
    reason = "the `#[tool]` macro generates an `.await` over the handler, so an infallible \
              handler is still an `async fn` with nothing to await"
)]

use protocol_droid::application::server::RequestContext;

/// The name this file calls itself by, in the run's closing line on stderr.
///
/// Not a protocol value: `initialize` already reports it to the peer, so this
/// constant exists only so the same string reaches a human in the summary
/// written after the peer closes stdin.
const SERVER_NAME: &str = "stdio_server";

// ---------------------------------------------------------------------------
// The server
// ---------------------------------------------------------------------------

/// A server with two tools, chosen so `tools/call` has something to return.
///
/// Two rather than one: `shout` proves a tool with a required string argument
/// round-trips its schema and its result, and `negotiated` proves a handler
/// can read the session through a leading [`RequestContext`] — which is the
/// one parameter that does not become a schema property.
struct Greeter;

#[protocol_droid::server]
impl Greeter {
    /// Uppercases `text` and reports how many characters it had.
    ///
    /// The character count is the part worth having: it forces the handler to
    /// do real work on its argument, so a `tools/call` that comes back
    /// unchanged is visibly a bug rather than a plausible coincidence.
    #[protocol_droid::tool]
    async fn shout(text: String) -> Result<Shout, String> {
        Ok(Shout {
            text: text.to_uppercase(),
            length: text.chars().count(),
        })
    }

    /// The revision this session negotiated.
    ///
    /// A leading [`RequestContext`] is how a handler reads the session, and it
    /// is the one parameter that does not become a schema property.
    #[protocol_droid::tool]
    async fn negotiated(ctx: RequestContext) -> String {
        ctx.peer().negotiated().to_string()
    }
}

/// What [`Greeter::shout`] answers with.
#[derive(protocol_droid::serde::Serialize)]
struct Shout {
    /// `text`, uppercased.
    text: String,
    /// How many characters `text` had before uppercasing.
    length: usize,
}

/// Serves MCP on stdin/stdout until the peer closes stdin.
///
/// The current-thread runtime is deliberate. The loop is one task that reads a
/// frame, routes it, and writes a reply, and nothing in it ever blocks on
/// another task.
///
/// The runtime is built by hand rather than by `#[tokio::main]`, because that
/// macro defaults to the multi-thread flavour and `rt-multi-thread` is not
/// among the crate's `tokio` features — enabling it would mean editing the
/// manifest, and a thread pool for one serialised conversation would buy
/// nothing anyway.
///
/// # Errors
///
/// Propagates a runtime build failure, a
/// [`protocol_droid::adapters::primary::stdio::ServeError`] for a read, write,
/// or diagnostics failure, and a `JsonObjectError` for a tool whose schema
/// would not parse. All of them are reported on stderr by this function's own
/// `Error` chain, never on stdout.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(serve())
}

/// The body of the server, on a current-thread runtime.
///
/// # Errors
///
/// As [`main`]: a read, write, or diagnostics failure from the serve loop, or
/// a [`JsonObjectError`] from a tool schema that would not parse.
async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    // One line, and no version. Everything a consumer would otherwise have to
    // assemble — a dispatcher, a session, a profile resolved for some
    // revision, a batch policy, a framing ceiling — is behind `serve_stdio`,
    // and the revision comes from the peer's own `initialize`.
    let report = Greeter.into_server()?.serve_stdio().await?;

    // stderr, never stdout. The report is the one place a human looks for what
    // the run did, and stdout is the wire.
    eprintln!(
        "{}: {} frames, {} requests, {} replies",
        SERVER_NAME, report.frames, report.requests, report.replies
    );
    Ok(())
}
