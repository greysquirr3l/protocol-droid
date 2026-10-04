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
//! # `initialize` is not answered yet, and that is a gap in the crate
//!
//! Sending this server an `initialize` returns:
//!
//! ```text
//! {"error":{"code":-32601,"message":"no such method"},"id":1,"jsonrpc":"2.0"}
//! ```
//!
//! That is not a mistake in this file. `Dispatcher::dispatch` matches the
//! method name against a closed set — `ping`, the four `*/list` methods, and
//! `tools/call` — and has no `initialize` arm, so the request is refused as an
//! unknown method. `ServeLoop` takes a concrete `Arc<Dispatcher>` rather than
//! a port trait, so an example cannot wrap the dispatch table to intercept the
//! handshake either.
//!
//! Every other method works, and `tools/call` in particular returns a real
//! result; see the module's closing notes. Until the crate gains a handshake
//! responder, the revision cannot be read from a peer, which is why it is the
//! constant [`NEGOTIATED`].
//!
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
//! `resources/templates/list` and `prompts/list` all answer, and the batch
//! policy is [`ProfileBatching`], so a batch frame is accepted only at a
//! revision whose profile carries `Feature::JsonRpcBatching` — which
//! [`NEGOTIATED`] does not, and which is why a `2025-06-18` client is told
//! `-32600` for one.
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

use std::sync::Arc;

use protocol_droid::adapters::primary::stdio::{Diagnostics, ServeLoop, StdioConfig};
use protocol_droid::application::jsonrpc::ProfileBatching;
use protocol_droid::application::server::{
    CallbackExchange, Dispatcher, ExchangeError, PeerSession, RequestContext,
};
use protocol_droid::application::session::{FrameSink, SinkError};
use protocol_droid::domain::feature::{Negotiation, resolve};
use protocol_droid::domain::ir::JsonObject;
use protocol_droid::domain::session::SessionId;
use protocol_droid::domain::{CapabilitySet, ProtocolVersion, TransportKind};

/// The revision this server answers on.
///
/// A real handshake settles this from the client's `initialize`. It cannot here:
/// nothing in the crate routes `initialize` to a handler, so the revision is a
/// constant this file chose rather than one a peer negotiated. Change it, and
/// everything gated behind the [`FeatureProfile`] moves with it — the profile is
/// derived from this value rather than asserted beside it.
///
/// `2025-06-18` because it is the newest revision this crate serves without
/// the example having to choose a codec and say so.
const NEGOTIATED: ProtocolVersion = ProtocolVersion::V2025_06_18;

/// The name this file calls itself by, in the run's closing line on stderr.
///
/// Not a protocol value: `initialize` is the only method that reports a server
/// name, and the crate does not answer it (see the module docs), so the only
/// place this string reaches a human is the summary written after the peer
/// closes stdin.
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

// ---------------------------------------------------------------------------
// The two ports a PeerSession needs
// ---------------------------------------------------------------------------

/// The callback port, wired to refuse.
///
/// A [`PeerSession`] needs somewhere to put a server-initiated round trip —
/// a sampling request, an elicitation. This server starts none, so the honest
/// implementation is the one that says so rather than a channel with no reader.
/// Refusing is not a stand-in for a timeout: a tool that cannot service a
/// callback fails, and an agent loop that hangs is worse than one that fails.
///
/// # Errors
///
/// Always [`ExchangeError`]. There is no path on which this returns `Ok`.
struct NoCallbacks;

impl CallbackExchange for NoCallbacks {
    fn exchange(
        &self,
        method: &str,
        _params: &JsonObject,
    ) -> impl std::future::Future<Output = Result<JsonObject, ExchangeError>> + Send {
        // The method name is this server's own, not a peer's, so naming it in
        // the error is safe — but it is deliberately not logged anywhere.
        let reason = format!("this server cannot service a {method} callback");
        std::future::ready(Err(ExchangeError::new(reason)))
    }
}

/// The notification port, wired to discard.
///
/// A [`PeerSession`] also needs somewhere to put a one-way notification such as
/// a `notifications/tools/list_changed`. This server registers nothing at
/// runtime, so it emits none, and a sink that counts what it was handed would
/// only prove that it was handed nothing.
///
/// # Errors
///
/// Never. Discarding a frame cannot fail, and reporting that it can would be a
/// failure mode this sink does not have.
struct NullSink;

impl FrameSink for NullSink {
    fn send(
        &self,
        _frame: &str,
    ) -> impl std::future::Future<Output = Result<(), SinkError>> + Send {
        std::future::ready(Ok(()))
    }
}

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

/// The negotiation this session settled on.
///
/// `server_caps` is every capability [`NEGOTIATED`] defines, which is what a
/// server that genuinely serves them would advertise. `client_caps` is empty
/// because a stdio peer that sent no capability block has advertised nothing,
/// and a profile resolved from a set the peer never sent would be stricter than
/// the session it is gating.
fn negotiation() -> Negotiation {
    Negotiation {
        version: NEGOTIATED,
        client_caps: CapabilitySet::empty(),
        server_caps: CapabilitySet::all_for(NEGOTIATED),
        transport: TransportKind::Stdio,
    }
}

/// The session every request on this connection is gated by.
///
/// Infallible by construction: [`resolve`] is a pure function over a
/// [`Negotiation`] this file builds itself, and [`PeerSession::new`] records
/// what it is given.
fn peer_session() -> PeerSession {
    let profile = resolve(negotiation());
    PeerSession::new(
        NEGOTIATED,
        profile,
        SessionId::new(1),
        Arc::new(NoCallbacks),
        Arc::new(NullSink),
    )
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
/// As [`main`]: a read, write, or diagnostics failure from [`ServeLoop`], or
/// a [`JsonObjectError`] from a tool schema that would not parse.
async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    let server = Greeter.into_server()?;
    let dispatcher = Arc::new(Dispatcher::new(Arc::new(server)));
    let peer = peer_session();

    let loop_ = ServeLoop::new(
        tokio::io::stdin(),
        tokio::io::stdout(),
        Diagnostics::stderr(),
        StdioConfig::default(),
        dispatcher,
        peer,
        ProfileBatching,
    );

    let report = loop_.serve().await?;

    // stderr, never stdout. The report is the one place a human looks for what
    // the run did, and stdout is the wire.
    eprintln!(
        "{}: {} frames, {} requests, {} replies",
        SERVER_NAME, report.frames, report.requests, report.replies
    );
    Ok(())
}
