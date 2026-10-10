//! The reference MCP server the conformance suite exercises.
//!
//! # What this is
//!
//! One runnable, fully-formed MCP server exposing a representative slice of
//! what the official `@modelcontextprotocol/conformance` suite's
//! server-scenarios want from a peer: a `ping`, a couple of `tools`,
//! `prompts` with descriptions and arguments, and `resources`. The names,
//! descriptions, and shapes are the ones a conforming client reads back.
//!
//! The server is intentionally **not** the project's own example server.
//! `stdio_server.rs` proves a tiny server is enough to round-trip a
//! handshake; this one proves the rest of the spec the conformance suite
//! reads is also wired. The two examples would diverge the moment either
//! shape changes, and that divergence is the test failure mode the
//! conformance suite catches.
//!
//! # What this is not
//!
//! Not a profile. The suite runs every scenario for the chosen revision
//! against this server, and a profile that hides a feature behind a
//! downgrade does the suite no favours — it would be the project's own
//! server that fails the matrix leg, not the feature. This server exposes
//! everything the `#[server]` macro supports today, and the
//! `conformance self` command points the suite at it.
//!
//! # Running
//!
//! ```text
//! cargo run --example conformance_reference_server -- --addr 127.0.0.1:8080
//! ```
//!
//! `stdout` carries nothing — this server runs on Streamable HTTP, where
//! stdout is not the wire. The conformance suite connects to
//! `http://<addr>/mcp`, runs every server scenario for the chosen
//! revision, and reports its findings through the wrapper.

use std::net::SocketAddr;

// `#[tool]` is called once per request rather than once per server, and
// the function is `async fn` because the call site `.await`s it. A body
// that did no real work would still hand back a future, and the
// compiler would warn about an unused-async body. `tokio::task::yield_now`
// is the one-byte future the lint accepts: it costs one task queue
// turn and changes nothing else, so a fixture whose tools are hot-paths
// in a benchmark does not regress here.
async fn yield_to_runtime() {
    tokio::task::yield_now().await;
}

use protocol_droid::application::server::Server;
use protocol_droid::domain::registry::{Prompt, PromptArgument, Resource};
use protocol_droid::serde::Serialize;

/// The name this server reports in its `initialize` reply.
///
/// Surfaced here rather than passed in, because a fixture server's identity
/// is part of its contract with the conformance suite: the suite reads it
/// from the wire and asserts on it, and a fixture whose name is whatever
/// happened to be in `argv` would not be a fixture any more. The literal
/// is also inlined into the `#[server(name = "...")]` attribute, where the
/// macro only accepts string literals — the constant and the literal are
/// the same fact, and keeping them in one block is the rule.
const SERVER_NAME: &str = "conformance-reference-server";

/// A tool result for `echo` and `add`, with a small structured payload.
///
/// `protocol-droid`'s `Result<T, E>` types are typed on the wire: `Ok(T)`
/// produces a `content` array of text/image entries plus a typed
/// `structuredContent`, `Err(E)` produces an `isError: true` reply with
/// the error's `Display`. The conformance suite's tools scenario asserts
/// the `isError` flag and the `content` shape, and a type that is a
/// string is enough for the two tools the suite needs.
#[derive(Serialize)]
struct EchoResult {
    /// The text the tool returned, verbatim.
    text: String,
    /// How many characters the input had — a counter proves the handler
    /// ran rather than short-circuited on an empty input.
    length: usize,
}

/// A tool result for `add`, with the same shape `EchoResult` uses.
///
/// Two concrete result types rather than one generic one because the
/// conformance suite's `tools/call` scenario checks that a structured
/// result's fields are *the* fields the tool described in its schema —
/// and a generic `Value` would let a regression in field naming slip
/// through the schema check.
#[derive(Serialize)]
struct AddResult {
    /// The sum, which a peer reads out of `structuredContent`.
    total: i64,
}

/// The reference server, in one `#[server]` block.
///
/// `into_server` registers the three marks against one builder, in source
/// order, and the test for the macro is that they all appear in
/// `tools/list`, `prompts/list`, and `resources/list` afterwards. A
/// registration that compiles but never lands in the registry would be
/// a passing test that exercises nothing, and the conformance suite
/// would not see the server as having a `prompts/list` reply at all.
struct ConformanceFixture;

#[protocol_droid::server(name = "conformance-reference-server")]
impl ConformanceFixture {
    /// Uppercases `text` and reports its length.
    ///
    /// The pair is the smallest one that proves a tool with a required
    /// string argument round-trips both its schema (the string is
    /// `required: ["text"]`) and its result (the JSON has a `text` and a
    /// `length` field). The conformance suite's tools scenario asserts
    /// both, and one tool that returns `42` would not.
    #[protocol_droid::tool]
    async fn echo(text: String) -> Result<EchoResult, String> {
        yield_to_runtime().await;
        Ok(EchoResult {
            length: text.chars().count(),
            text: text.to_uppercase(),
        })
    }

    /// Adds two integers and reports the sum.
    ///
    /// Two-number tool so the `tools/call` scenario has at least one
    /// tool whose arguments are typed integers. The conformance suite
    /// does not assert the *value* of the sum — it asserts the schema
    /// and that `isError` is `false` for a well-formed call.
    #[protocol_droid::tool]
    async fn add(a: i64, b: i64) -> Result<AddResult, String> {
        yield_to_runtime().await;
        Ok(AddResult { total: a + b })
    }

    /// A prompt that asks for a code review, with two arguments.
    ///
    /// The conformance suite's `prompts/list` and `prompts/get` scenarios
    /// require a prompt with a description and at least one argument, so
    /// a prompt without `arguments` would not exercise the schema path
    /// the suite reads. The two arguments — `diff` (required) and
    /// `focus` (optional) — are the smallest shape that covers both
    /// `required: [...]` and `arguments[i].required: true`.
    #[protocol_droid::prompt]
    fn review() -> Prompt {
        Prompt {
            description: Some(
                "Asks for a review of a diff against an optional focus area.".to_owned(),
            ),
            arguments: vec![
                PromptArgument {
                    name: "diff".to_owned(),
                    description: Some("The diff to review.".to_owned()),
                    title: None,
                    required: true,
                },
                PromptArgument {
                    name: "focus".to_owned(),
                    description: Some(
                        "What to look at (e.g. `security`, `performance`).".to_owned(),
                    ),
                    title: None,
                    required: false,
                },
            ],
            ..Prompt::new("review")
        }
    }

    /// A prompt without arguments, so `prompts/get` is exercised against
    /// at least one prompt whose `arguments` is the empty array.
    #[protocol_droid::prompt]
    fn summarize() -> Prompt {
        Prompt {
            description: Some("Summarizes whatever the caller hands in.".to_owned()),
            ..Prompt::new("summarize")
        }
    }

    /// A static readme resource, with a `text/` MIME type the conformance
    /// suite's `resources/list` and `resources/read` scenarios both read.
    #[protocol_droid::resource]
    fn readme() -> Resource {
        Resource {
            description: Some("The reference server's own readme.".to_owned()),
            mime_type: Some("text/markdown".to_owned()),
            ..Resource::new("file:///conformance/readme.md", "readme")
        }
    }

    /// A second resource so the suite sees a `resources/list` reply that
    /// is not a single-element array. A regression that registers the
    /// first resource twice would still pass a one-resource test.
    #[protocol_droid::resource]
    fn changelog() -> Resource {
        Resource {
            description: Some("The reference server's own changelog.".to_owned()),
            mime_type: Some("text/markdown".to_owned()),
            ..Resource::new("file:///conformance/CHANGELOG.md", "changelog")
        }
    }
}

/// Parses `--addr <ipv4:port>` from argv, falling back to loopback on
/// port 8080. A single flag because the conformance wrapper is the one
/// that picks the address and forwards it; a fixture that took
/// eleven flags would not be a fixture.
///
/// Returns a `Result` rather than panicking on a malformed
/// `--addr <value>`, so the error path is the same chain the
/// `axum::Error` / `Bind` errors report.
fn parse_addr() -> Result<SocketAddr, Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--addr" && let Some(value) = args.next() {
            return Ok(value.parse()?);
        }
    }
    Ok("127.0.0.1:8080".parse()?)
}

/// Builds the server, then serves it on the chosen address.
///
/// # Errors
///
/// A `Box<dyn Error>` is the smallest type the surface needs: a
/// `JsonObjectError` for a tool whose schema would not parse, an
/// `io::Error` for a bind failure, an `axum::Error` for an accept
/// failure. All are reported on the way out with their own chain.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(serve())
}

async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    let addr = parse_addr()?;
    // `Server::serve_http` is the one-call facade, but it does not let the
    // caller pick the address — the fixture is started by a wrapper that
    // does, and an address baked into the fixture would make that wrapper's
    // job harder. The `HttpBuilder` is the explicit form that takes an
    // address, and the only piece of the suite of choices the fixture
    // actually needs to make.
    let server: Server = ConformanceFixture.into_server()?;
    let builder =
        protocol_droid::adapters::primary::transport::http::HttpBuilder::new(server).addr(addr);
    let bound = builder.serve().await?;
    eprintln!("{}: listening on {}", SERVER_NAME, bound.addr);
    Ok(())
}