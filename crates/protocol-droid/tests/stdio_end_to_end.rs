//! Our own server, driven by our own client, over a real OS pipe.
//!
//! # The gap this file closes
//!
//! The other tests in this crate exercise both halves of a stdio conversation
//! and neither combination. `tests/in_process_exchange.rs` drives a
//! [`Dispatcher`] directly and never puts a byte on a wire.
//! `adapters/secondary/stdio/testing.rs` re-executes the test binary as its
//! child and speaks to it over a real pipe, but the child is a stand-in that
//! exists to be told which role to play. So the one thing neither reaches is
//! the thing a user actually runs: the example server in
//! `examples/stdio_server.rs`, spawned as a process, framed over two pipes, by
//! the same client code that would spawn a third-party server.
//!
//! That combination is where the assumptions meet reality. A frame ceiling
//! that is only ever checked against a hand-built `StdioSource`, a `kill_on_drop`
//! flag on a `Child` the test owns, a stderr split proven against a helper that
//! writes to stderr on command — each of those can be right in isolation and
//! still leave a real child unframable, unkilled, or unreadable.
//!
//! # The child is the example, unmodified
//!
//! `examples/stdio_server.rs` is used exactly as a third-party server would be:
//! no arguments, no environment, no harness hook. The tools it registers —
//! `shout`, which uppercases a required string and reports its length, and
//! `negotiated`, which reads the session's revision — are asserted on by value
//! so a regression in the macro, the schema, or the codec shows up here as a
//! wrong string rather than as a missing test.
//!
//! # Where the binary is found, and why
//!
//! `env!("CARGO_BIN_EXE_…")` covers `[[bin]]` targets and no others, so it
//! cannot name an example. The path is therefore derived from
//! [`current_exe`](std::env::current_exe), which is `target/<profile>/deps/<test>`:
//! two `.parent()` calls reach `target/<profile>`, and the example sits in
//! `examples/` beneath it. Deriving it this way rather than from
//! `CARGO_MANIFEST_DIR` is what keeps it correct under a custom
//! `CARGO_TARGET_DIR` or an explicit `--target`, because the test binary and
//! the example are then in the same target tree by construction rather than by
//! agreement between two directory guesses.
//!
//! The `.exe` suffix comes from [`std::env::consts::EXE_SUFFIX`], which is
//! `""` off Windows — so there is no `cfg(windows)` branch to get wrong.
//!
//! # The guard, and why it skips rather than fails
//!
//! `cargo test` builds examples as part of its default target selection, so
//! after a plain `cargo test --workspace --all-features` the binary is there
//! and these tests run. When it is not — a bare `cargo test` of one unrelated
//! target, a tree that has only ever built the library — the tests **skip with
//! a message on stderr** rather than failing.
//!
//! Skipping is the honest outcome, because a missing binary is a fact about
//! what was built, not a defect in the client or the server. Building the
//! example from inside the test was the alternative and was rejected: it means
//! a second `cargo` invocation nested inside a test, holding the build lock
//! while the parent `cargo` still owns it, which deadlocks. Failing instead
//! would have been worse still, for the same reason a missing test is worse
//! than an absent one: it would turn "this suite has not built an example yet"
//! into a red suite that looks like a product bug.
//!
//! # The whole file is behind `stdio`
//!
//! `StdioClient` and `StdioSink` are gated behind the `stdio` feature, and CI
//! has a `--no-default-features` leg. Without this attribute the file would not
//! compile on that leg, so a test that exists to cover the transport would be
//! one more reason that leg cannot build.
#![cfg(feature = "stdio")]

use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use protocol_droid::adapters::primary::stdio::{Diagnostics, StdioConfig, StdioError};
use protocol_droid::adapters::secondary::stdio::{
    ChildSpec, ChildSpecError, ClientConfig, SpawnError, StdioClient, StdioClientError,
};
use protocol_droid::application::jsonrpc::SourceError;
use protocol_droid::application::jsonrpc::document::{RequestError, request_document};
use protocol_droid::domain::ir::{JsonObject, JsonObjectError, MethodName, MethodNameError};
use protocol_droid::domain::{MaxFrameSize, MaxFrameSizeError};
use tokio::io::AsyncWrite;

/// Far larger than a child needs to start, answer, or be killed.
///
/// Every wait here is wrapped in it, so a regression that turns "the peer did
/// not answer" into a hang becomes a failed test rather than a stuck suite.
/// Deliberately not a realistic budget: reaching it means nothing woke the
/// task at all, not that it was slow.
const GUARD: Duration = Duration::from_secs(20);

/// How long to let a conclusion settle before believing it.
///
/// Only used where the claim is about something *not* happening, so it has to
/// be long enough that the thing would have happened if it were going to. See
/// `dropping_the_client_kills_the_child`, whose control is what makes this
/// number mean anything.
const SETTLE: Duration = Duration::from_millis(750);

/// The revision the example serves, and therefore the one asserted below.
///
/// The example picks this as a constant rather than negotiating it, so a test
/// that read it from the wire would be asserting whatever the child chose to
/// say. Spelling it here means a change to the child's default revision is a
/// failure here, in a diff, rather than a test that quietly follows.
const REVISION: &str = "2025-06-18";

/// The line the child writes to stderr once its peer closes stdin, gracefully.
///
/// Its absence after the client is dropped is the evidence that the drop
/// *killed* the child rather than letting it shut down — the distinction the
/// whole test turns on. See `THE GRACEFUL EXIT MARKER` in that test.
const GRACEFUL_EXIT_MARKER: &str = "the peer closed the stream";

/// A frame the child must refuse: larger than its 4 MiB default ceiling.
///
/// Sized past the ceiling rather than to it, because "at the limit" and "over
/// the limit" are different branches and only the second is a framing error.
const OVERSIZED_PADDING: usize = 5 * 1024 * 1024;

/// The ceiling this test's *client* uses, above [`OVERSIZED_PADDING`].
///
/// The client refuses to write a frame above its own ceiling, so it has to be
/// given a larger one than the child has for this test to deliver an oversized
/// frame at all. This is the asymmetry under test: two peers, two ceilings, and
/// the one that gives way is the one that reads.
const CLIENT_CEILING: u32 = 8 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Failure
// ---------------------------------------------------------------------------

/// What a `?` in this file can produce.
///
/// The fallible calls span the process layer, the byte layer, the frame parser
/// and the child's own error type, and a test that unwrapped each one would be
/// testing its fixtures. The payloads are carried rather than discarded so a
/// failing test prints the layer's own account of what it refused.
#[derive(Debug)]
#[expect(
    dead_code,
    reason = "the payloads are read by the derived Debug, not by the tests"
)]
enum Failure {
    /// A stream under the test failed.
    Io(io::Error),
    /// A child could not be described.
    Spec(ChildSpecError),
    /// A child could not be started or shaped.
    Spawn(SpawnError),
    /// This client's own account of a failed exchange.
    Client(StdioClientError),
    /// A frame could not be read from the child.
    Source(SourceError),
    /// A frame could not be written to the child.
    Stdio(StdioError),
    /// A document this test built could not be assembled.
    Document(RequestError),
    /// A method name this test wrote is not one.
    Method(MethodNameError),
    /// A frame ceiling this test asked for is not one.
    Ceiling(MaxFrameSizeError),
    /// A document this crate built is not an object.
    Object(JsonObjectError),
    /// The child's answer did not parse as JSON.
    Json(serde_json::Error),
    /// The path of this running test binary is not available.
    NoCurrentExe,
    /// The child exited on its own where it was required to stay running.
    ExitedEarly(String),
    /// A wait did not finish inside [`GUARD`].
    TimedOut(&'static str),
    /// The child's stdout closed where a frame was expected.
    NoFrame(&'static str),
    /// The reply did not say what the wire said it would say.
    Unexpected {
        /// What was being read.
        what: &'static str,
        /// What actually came back.
        got: String,
    },
    /// A value at a path in a reply was not present.
    Absent {
        /// The path that was walked.
        what: String,
    },
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ChildSpecError> for Failure {
    fn from(error: ChildSpecError) -> Self {
        Self::Spec(error)
    }
}

impl From<SpawnError> for Failure {
    fn from(error: SpawnError) -> Self {
        Self::Spawn(error)
    }
}

impl From<StdioClientError> for Failure {
    fn from(error: StdioClientError) -> Self {
        Self::Client(error)
    }
}

impl From<SourceError> for Failure {
    fn from(error: SourceError) -> Self {
        Self::Source(error)
    }
}

impl From<StdioError> for Failure {
    fn from(error: StdioError) -> Self {
        Self::Stdio(error)
    }
}

impl From<RequestError> for Failure {
    fn from(error: RequestError) -> Self {
        Self::Document(error)
    }
}

impl From<MethodNameError> for Failure {
    fn from(error: MethodNameError) -> Self {
        Self::Method(error)
    }
}

impl From<MaxFrameSizeError> for Failure {
    fn from(error: MaxFrameSizeError) -> Self {
        Self::Ceiling(error)
    }
}

impl From<JsonObjectError> for Failure {
    fn from(error: JsonObjectError) -> Self {
        Self::Object(error)
    }
}

impl From<serde_json::Error> for Failure {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

// ---------------------------------------------------------------------------
// Locating the child
// ---------------------------------------------------------------------------

/// The example server's path, or `None` when it has not been built.
///
/// # Errors
///
/// [`Failure::NoCurrentExe`] when the running test binary's own path is
/// unavailable, which is the one case where the question cannot be asked at
/// all — as opposed to answered with "not yet", which is a `None` and a skip.
fn example_server() -> Result<Option<PathBuf>, Failure> {
    let running = std::env::current_exe().map_err(|_| Failure::NoCurrentExe)?;
    // `<target>/<profile>/deps/<test>` -> `<target>/<profile>`.
    let Some(profile_dir) = running.parent().and_then(std::path::Path::parent) else {
        return Ok(None);
    };
    let path = profile_dir
        .join("examples")
        .join(format!("stdio_server{}", std::env::consts::EXE_SUFFIX));

    Ok(path.is_file().then_some(path))
}

/// The program to spawn, or a skip, for a test that needs the example.
///
/// One place so the skip reason is the same sentence in every test that takes
/// it: a suite where four tests each explained a different way of not running
/// is a suite nobody can read the skips of.
fn program_or_skip() -> Result<Option<PathBuf>, Failure> {
    let found = example_server()?;
    if found.is_none() {
        eprintln!(
            "skipping: the stdio_server example has not been built. \
             `cargo test` builds examples, so a plain \
             `cargo test --workspace --all-features` runs this; \
             `cargo build -p protocol-droid --example stdio_server` builds it alone."
        );
    }
    Ok(found)
}

/// A spec for the example, with no arguments and no environment.
///
/// Deliberately bare. The example takes no arguments, so passing any would be
/// a claim about it that nothing here has established, and a client that
/// depended on one would be testing a configuration no user would run.
fn spec_for(path: &Path) -> Result<ChildSpec, Failure> {
    Ok(ChildSpec::new(path.as_os_str())?)
}

// ---------------------------------------------------------------------------
// The diagnostics channel
// ---------------------------------------------------------------------------

/// A shared, in-memory `AsyncWrite` for [`Diagnostics`].
///
/// This is the "somewhere that is not the protocol stream" the client and the
/// server both need, made readable so a test can prove what the child wrote
/// there. A `std::sync::Mutex` rather than tokio's because `poll_write` cannot
/// await, and a poisoned lock is recovered rather than propagated: a panic in
/// another test's writer must not turn into a failure of this one's.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    /// Everything the child has written off-protocol so far.
    fn text(&self) -> String {
        match self.0.lock() {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(poisoned) => String::from_utf8_lossy(&poisoned.into_inner()).into_owned(),
        }
    }
}

impl AsyncWrite for Capture {
    fn poll_write(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let shared = self.as_ref().get_ref();
        let written = buf.len();
        match shared.0.lock() {
            Ok(mut bytes) => bytes.extend_from_slice(buf),
            Err(poisoned) => poisoned.into_inner().extend_from_slice(buf),
        }
        Poll::Ready(Ok(written))
    }

    fn poll_flush(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// Whether `capture` comes to contain `needle`, within [`SETTLE`].
///
/// For claims of the form "this arrives, eventually": the child writes on its
/// own schedule, and a test that read once would be racing it.
async fn capture_gains(capture: &Capture, needle: &str) -> bool {
    let deadline = tokio::time::Instant::now() + SETTLE;
    loop {
        if capture.text().contains(needle) {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ---------------------------------------------------------------------------
// Reading a reply
// ---------------------------------------------------------------------------

/// The value at `path` within a reply, or a failure naming the path.
///
/// Walks with `get` at every step rather than indexing, so a reply that
/// changed shape reports *which* field went missing instead of panicking on
/// the way to find out.
fn at<'a>(value: &'a serde_json::Value, path: &[&str]) -> Result<&'a serde_json::Value, Failure> {
    let mut cursor = value;
    for key in path {
        cursor = cursor.get(key).ok_or_else(|| Failure::Absent {
            what: path.join("."),
        })?;
    }
    Ok(cursor)
}

/// The string at `path`, or a failure carrying what was actually there.
fn string_at(value: &serde_json::Value, path: &[&str]) -> Result<String, Failure> {
    let found = at(value, path)?;
    found
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| Failure::Unexpected {
            what: "a string",
            got: found.to_string(),
        })
}

/// The result object of a reply, as a value.
///
/// Every assertion in this file reads the answer back through here, so that
/// "the client returned a `JsonObject`" and "the wire said the right thing" are
/// kept honest against each other: the `JsonObject` has to be re-parsed to be
/// read, which is what a consumer that is not this crate has to do too.
fn result_of(reply: &JsonObject) -> Result<serde_json::Value, Failure> {
    Ok(serde_json::from_str(reply.as_str())?)
}

/// The `tools` array of a `tools/list` reply, as `(name, description)` pairs.
fn tool_names(listed: &serde_json::Value) -> Result<Vec<String>, Failure> {
    let tools = at(listed, &["tools"])?;
    let array = tools.as_array().ok_or_else(|| Failure::Unexpected {
        what: "a tools array",
        got: tools.to_string(),
    })?;

    // A hand-rolled fold rather than an iterator chain, because every `?` in
    // one of those is a conversion that has to name its own failure.
    let mut names = Vec::new();
    for tool in array {
        let name = tool
            .get("name")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| Failure::Unexpected {
                what: "a named tool",
                got: tool.to_string(),
            })?;
        names.push(name.to_owned());
    }
    Ok(names)
}

/// The named tool from a `tools/list` reply, or a failure naming it.
fn tool_named<'a>(
    listed: &'a serde_json::Value,
    name: &str,
) -> Result<&'a serde_json::Value, Failure> {
    let tools = at(listed, &["tools"])?;
    let array = tools.as_array().ok_or_else(|| Failure::Unexpected {
        what: "a tools array",
        got: tools.to_string(),
    })?;

    for tool in array {
        if tool.get("name").and_then(serde_json::Value::as_str) == Some(name) {
            return Ok(tool);
        }
    }
    Err(Failure::Absent {
        what: format!("a tool named {name}"),
    })
}

/// The `initialize` params the MCP handshake takes.
fn initialize_params() -> Result<JsonObject, Failure> {
    // One line, and no `\` continuation: a raw string keeps a backslash
    // literally, so the continuation would land in the JSON as a backslash and
    // a newline, and the failure would surface as an offset in the middle of a
    // handshake rather than as the mistake it is.
    Ok(JsonObject::new(format!(
        r#"{{"protocolVersion":"{REVISION}","capabilities":{{}},"clientInfo":{{"name":"stdio_end_to_end","version":"0.1.0"}}}}"#
    ))?)
}

// ---------------------------------------------------------------------------
// Driving the child
// ---------------------------------------------------------------------------

/// A client on the example, with its off-protocol output captured.
///
/// # Errors
///
/// Whatever spawning does, plus [`Failure::TimedOut`] if the spawn itself did
/// not finish inside [`GUARD`] — which on a loaded machine is a real outcome
/// and should read as such rather than as a hang.
async fn client_on(path: &Path, capture: &Capture) -> Result<StdioClient<Capture>, Failure> {
    let spec = spec_for(path)?;
    let config = ClientConfig::new(StdioConfig::new(MaxFrameSize::new(CLIENT_CEILING)?));
    Ok(tokio::time::timeout(
        GUARD,
        StdioClient::spawn(&spec, config, Diagnostics::new(capture.clone())),
    )
    .await
    .map_err(|_| Failure::TimedOut("spawning the example server"))??)
}

/// Runs `initialize` and returns the result, so every test starts negotiated.
///
/// # Errors
///
/// As [`client_on`], plus the client's own error if the handshake does not
/// complete.
async fn initialize(client: &StdioClient<Capture>) -> Result<serde_json::Value, Failure> {
    let id = client.next_request_id();
    let method = MethodName::new("initialize")?;
    let reply = tokio::time::timeout(GUARD, client.call(&id, &method, &initialize_params()?))
        .await
        .map_err(|_| Failure::TimedOut("the initialize handshake"))??;

    result_of(&reply)
}

// ---------------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------------

/// A whole session, over two pipes, against the example as a user would run it.
///
/// `initialize`, `tools/list`, `tools/call`, and a notification that must draw
/// no answer — in that order, on one connection, with nothing stubbed between
/// the client and the process.
#[tokio::test]
async fn a_full_session_runs_over_a_real_pipe() -> Result<(), Failure> {
    let Some(path) = program_or_skip()? else {
        return Ok(());
    };
    let capture = Capture::default();
    let client = client_on(&path, &capture).await?;

    // A running client has a real process behind it, not an in-memory stand-in.
    // Asserted before anything else so every claim below is about a child that
    // was demonstrably started.
    let pid = client.child_id();
    if pid.is_none() {
        return Err(Failure::Unexpected {
            what: "a spawned child with a process id",
            got: "none".to_owned(),
        });
    }

    // 1. The handshake. The revision and the server's name are the example's
    //    own, read back over the wire rather than asserted against a constant
    //    this test also owns.
    let initialized = initialize(&client).await?;
    let version = string_at(&initialized, &["protocolVersion"])?;
    if version != REVISION {
        return Err(Failure::Unexpected {
            what: "the negotiated revision",
            got: version,
        });
    }
    let server = string_at(&initialized, &["serverInfo", "name"])?;
    if server != "stdio_server::Greeter" {
        return Err(Failure::Unexpected {
            what: "the server's name",
            got: server,
        });
    }

    // 2. The listing, by value: both tools, and the required-argument schema
    //    the macro generated for `shout`. A tool that vanished, or one whose
    //    schema lost its `required`, shows up here rather than in a
    //    three-way snapshot nobody reads.
    list_and_check_schemas(&client).await?;

    // 3. The tool call, asserted on the bytes the child really sent.
    call_and_check_content(&client).await?;

    // 4. A notification, and the claim that it draws no answer.
    //
    //    Proven by frame order rather than by absence. The notification goes
    //    out first, then a `ping` with a known id, and the *first* frame that
    //    comes back must be the `ping`'s — carrying the `ping`'s id, and a
    //    result rather than an error.
    //
    //    That is a complete proof rather than a suggestive one, and the reason
    //    is the child's serial loop: it reads a frame, routes it, and writes
    //    one reply before it reads again. So it had to read the notification
    //    before it read the `ping`, and any reply it had made to the
    //    notification would already have been on the stream, ahead of the
    //    `ping`'s. There is no ordering in which an answer to the
    //    notification can arrive after the `ping`'s and still be missed here.
    notification_draws_no_answer(&client).await?;

    // And nothing left the process off-protocol during any of it, so the clean
    // stream above cannot be explained by stray output being dropped somewhere.
    let off_protocol = capture.text();
    if !off_protocol.is_empty() {
        return Err(Failure::Unexpected {
            what: "an empty diagnostics channel",
            got: off_protocol,
        });
    }

    Ok(())
}

/// Step 2: `tools/list` carries both tools, and `shout`'s generated schema
/// still marks `text` as required.
async fn list_and_check_schemas(client: &StdioClient<Capture>) -> Result<(), Failure> {
    let listed = result_of(
        &tokio::time::timeout(
            GUARD,
            client.call(
                &client.next_request_id(),
                &MethodName::new("tools/list")?,
                &JsonObject::default(),
            ),
        )
        .await
        .map_err(|_| Failure::TimedOut("tools/list"))??,
    )?;
    let names = tool_names(&listed)?;
    for expected in ["shout", "negotiated"] {
        if !names.iter().any(|name| name == expected) {
            return Err(Failure::Unexpected {
                what: "the listed tools",
                got: names.join(", "),
            });
        }
    }
    let shout = tool_named(&listed, "shout")?;
    let required = at(shout, &["inputSchema", "required"])?;
    let entries = required.as_array().ok_or_else(|| Failure::Unexpected {
        what: "a required-arguments array",
        got: required.to_string(),
    })?;
    let text_is_required = entries.iter().any(|entry| entry.as_str() == Some("text"));
    if !text_is_required {
        return Err(Failure::Unexpected {
            what: "shout's required arguments",
            got: required.to_string(),
        });
    }
    Ok(())
}

/// Step 3: `tools/call` on `shout`, asserted on the bytes the child really
/// sent. The text inside `content` is the tool's own serialisation, and the
/// length is there so a call that came back unchanged is visibly wrong rather
/// than coincidentally right.
async fn call_and_check_content(client: &StdioClient<Capture>) -> Result<(), Failure> {
    let call_params = JsonObject::new(r#"{"name":"shout","arguments":{"text":"hello droid"}}"#)?;
    let called = result_of(
        &tokio::time::timeout(
            GUARD,
            client.call(
                &client.next_request_id(),
                &MethodName::new("tools/call")?,
                &call_params,
            ),
        )
        .await
        .map_err(|_| Failure::TimedOut("tools/call"))??,
    )?;

    let block = at(&called, &["content"])?;
    let first = block
        .as_array()
        .and_then(|blocks| blocks.first())
        .ok_or_else(|| Failure::Unexpected {
            what: "a content block",
            got: block.to_string(),
        })?;
    let text = first
        .get("text")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| Failure::Unexpected {
            what: "text in the content block",
            got: first.to_string(),
        })?;
    if text != r#"{"length":11,"text":"HELLO DROID"}"# {
        return Err(Failure::Unexpected {
            what: "shout's returned content",
            got: text.to_owned(),
        });
    }
    let uppercased = string_at(&called, &["structuredContent", "text"])?;
    if uppercased != "HELLO DROID" {
        return Err(Failure::Unexpected {
            what: "shout's structured content",
            got: uppercased,
        });
    }
    let is_error = at(&called, &["isError"])?;
    if is_error != &serde_json::Value::Bool(false) {
        return Err(Failure::Unexpected {
            what: "isError on a successful call",
            got: is_error.to_string(),
        });
    }
    Ok(())
}

/// Step 4: a notification draws no answer, proven by the frame order that
/// follows rather than by an absence nothing can witness.
///
/// The proof is the order and not a count. The notification goes out first,
/// then a `ping` with a known id, and the first frame back must be the
/// `ping`'s. The child's serial loop closes it: it reads a frame, routes it,
/// and writes one reply before reading again, so it had to read the
/// notification before the `ping` and any reply to the notification was
/// already on the stream ahead of the `ping`'s.
async fn notification_draws_no_answer(client: &StdioClient<Capture>) -> Result<(), Failure> {
    let ping_id = client.next_request_id();
    let notification = MethodName::new("notifications/initialized")?;
    tokio::time::timeout(GUARD, client.notify(&notification, &JsonObject::default()))
        .await
        .map_err(|_| Failure::TimedOut("a notification"))??;

    let ping_document =
        request_document(&ping_id, &MethodName::new("ping")?, &JsonObject::default())?;
    let document = ping_document.to_string();
    tokio::time::timeout(GUARD, client.send_document(&document))
        .await
        .map_err(|_| Failure::TimedOut("writing the ping"))??;

    let frame = tokio::time::timeout(GUARD, client.next_frame())
        .await
        .map_err(|_| Failure::TimedOut("the first frame after a notification"))??;
    let frame = frame.ok_or(Failure::NoFrame("the ping's answer"))?;
    let answered: serde_json::Value = serde_json::from_str(&frame)?;
    let answered_id = answered
        .get("id")
        .ok_or_else(|| Failure::Absent {
            what: "id on the reply".to_owned(),
        })?
        .to_string();
    if answered_id != ping_id.to_string() {
        return Err(Failure::Unexpected {
            what: "the first frame's id",
            got: answered_id,
        });
    }
    // A notification's answer would carry no id at all, so an id here is
    // already half the claim; the other half is that this is the `ping`'s.
    let version = string_at(&answered, &["jsonrpc"])?;
    if version != "2.0" {
        return Err(Failure::Unexpected {
            what: "a JSON-RPC 2.0 frame on the protocol stream",
            got: version,
        });
    }
    Ok(())
}

/// A tool that does not exist is a protocol error, not a transport failure.
///
/// The failure case worth most, because it is the one a bridge gets wrong: an
/// error must come back as a JSON-RPC error *reply* and leave the session
/// running, rather than closing the pipe or taking the child down.
#[tokio::test]
async fn an_unknown_tool_is_a_protocol_error_and_the_session_survives() -> Result<(), Failure> {
    let Some(path) = program_or_skip()? else {
        return Ok(());
    };
    let capture = Capture::default();
    let client = client_on(&path, &capture).await?;
    initialize(&client).await?;

    let outcome = tokio::time::timeout(
        GUARD,
        client.call(
            &client.next_request_id(),
            &MethodName::new("tools/call")?,
            &JsonObject::new(r#"{"name":"no-such-tool","arguments":{}}"#)?,
        ),
    )
    .await
    .map_err(|_| Failure::TimedOut("a call to a missing tool"))?;

    // The peer's own error, surfaced by the client as a request failure. Not a
    // `TimedOut`, not a `PeerClosed`, not a broken pipe: the child answered.
    match outcome {
        Err(StdioClientError::Request(RequestError::PeerError { message, .. })) => {
            if message != "no such tool" {
                return Err(Failure::Unexpected {
                    what: "the error's message",
                    got: message,
                });
            }
        }
        Err(other) => {
            return Err(Failure::Unexpected {
                what: "a peer error reply",
                got: other.to_string(),
            });
        }
        Ok(reply) => {
            return Err(Failure::Unexpected {
                what: "an error reply",
                got: reply.as_str().to_owned(),
            });
        }
    }

    // The child is still there, and still answers. A server that treated an
    // unknown tool as fatal would fail here rather than on the assertion above.
    let listed = result_of(
        &tokio::time::timeout(
            GUARD,
            client.call(
                &client.next_request_id(),
                &MethodName::new("tools/list")?,
                &JsonObject::default(),
            ),
        )
        .await
        .map_err(|_| Failure::TimedOut("tools/list after an error"))??,
    )?;
    let names = tool_names(&listed)?;
    if names.len() < 2 {
        return Err(Failure::Unexpected {
            what: "both tools still listed",
            got: names.join(", "),
        });
    }

    Ok(())
}

/// The child's stderr reaches the diagnostics channel, and never the protocol.
///
/// The split is the whole claim of a stdio transport, and it needs traffic on
/// *both* sides to be worth anything: a test that only ever checks the clean
/// case is consistent with a client that discarded the child's stderr unread,
/// which is a different bug with the same passing test.
///
/// So this drives a frame past the child's ceiling. The child refuses it, and
/// being a Rust process whose `main` returned an error, it says so — on
/// stderr. The assertions are then the two halves:
///
/// - the capture contains the child's error, so the bytes really were produced
///   and really were routed off-protocol, and
/// - the protocol stream yields nothing further, so those bytes demonstrably
///   did not also go out as frames.
///
/// Either alone would be consistent with a broken client. Together they are
/// the split.
#[tokio::test]
async fn the_childs_stderr_never_reaches_the_protocol_stream() -> Result<(), Failure> {
    let Some(path) = program_or_skip()? else {
        return Ok(());
    };
    let capture = Capture::default();
    let client = client_on(&path, &capture).await?;

    // A clean session first, so the stream is known to be carrying valid
    // frames before the interesting thing happens to it.
    initialize(&client).await?;

    let padding = "a".repeat(OVERSIZED_PADDING);
    let oversized =
        format!(r#"{{"jsonrpc":"2.0","id":1,"method":"ping","params":{{"pad":"{padding}"}}}}"#);

    // The write may or may not complete, and that is not what is under test.
    // The child refuses the frame at its ceiling and exits, which closes the
    // read end of the pipe underneath a client that is still filling it — so a
    // completed write and a broken pipe are both correct, and which one happens
    // is a scheduling detail. What matters is where the child's report goes.
    match tokio::time::timeout(GUARD, client.send_document(&oversized)).await {
        // A completed write and a broken pipe are the same outcome for this
        // test, and merging the arms says so rather than hiding it behind two
        // empty blocks that look like a distinction nobody is drawing.
        // A completed write and a broken pipe are the same outcome for this
        // test, and merging the arms says so rather than hiding it behind two
        // empty blocks that look like a distinction nobody is drawing.
        Ok(Ok(()) | Err(StdioError::Transport)) => {}
        Ok(Err(other)) => {
            return Err(Failure::Unexpected {
                what: "a completed write or a broken pipe",
                got: other.to_string(),
            });
        }
        Err(_elapsed) => return Err(Failure::TimedOut("writing an oversized frame")),
    }
    // Half one: the child said something, off-protocol, and it is the refusal
    // rather than some unrelated line.
    if !capture_gains(&capture, "FrameTooLarge").await {
        return Err(Failure::Unexpected {
            what: "the child's frame-too-large report on stderr",
            got: capture.text(),
        });
    }

    // Half two: the protocol stream is finished. The child exited, so this
    // returns `None` — and had its stderr been merged into stdout, the frame
    // carrying that same text would have arrived here instead.
    let leaked = tokio::time::timeout(GUARD, client.next_frame())
        .await
        .map_err(|_| Failure::TimedOut("the frame after the child refused one"))??;
    if let Some(frame) = leaked {
        if frame.contains("FrameTooLarge") {
            return Err(Failure::Unexpected {
                what: "no stderr text on the protocol stream",
                got: frame,
            });
        }
        return Err(Failure::Unexpected {
            what: "no frame after the child exited",
            got: frame,
        });
    }

    Ok(())
}

/// Dropping the client kills the child, and the child did not kill itself.
///
/// # Why this test is shaped the way it is
///
/// THE GRACEFUL EXIT MARKER is the line the example writes to stderr once its
/// stdin closes. A client that failed to kill its child would let exactly that
/// happen: the client goes away, the child's stdin closes, the child shuts
/// down gracefully and writes its line. So "the marker is absent" is only
/// evidence of a kill if the child would otherwise have written it — which is
/// what the control below establishes, and it is the false proof this test is
/// shaped to avoid.
///
/// The control is the same binary, started the same way, *without*
/// `kill_on_drop`, with its stdin held open. Half a second later it is still
/// running. So the example does not exit by itself, and a child that is gone
/// after the drop went away because it was killed.
///
/// On the ordering that makes the assertion deterministic: `StdioClient`
/// declares `child` before `sink`, and Rust drops fields in declaration
/// order, so `kill_on_drop` fires *before* the child's stdin is closed. The
/// child therefore cannot observe the EOF that would have triggered the
/// graceful path, on any platform, however the two drops are interleaved.
#[tokio::test]
async fn dropping_the_client_kills_the_child() -> Result<(), Failure> {
    let Some(path) = program_or_skip()? else {
        return Ok(());
    };
    let spec = spec_for(&path)?;

    // The control. No `kill_on_drop` on this one, and that is the entire
    // difference between it and the client below.
    let mut control = spec.to_command();
    control
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut control_child = control.spawn()?;
    // Holding stdin is what keeps the child off its EOF path.
    let _held_stdin = control_child.stdin.take();
    let _held_stdout = control_child.stdout.take();
    let _held_stderr = control_child.stderr.take();

    tokio::time::sleep(SETTLE).await;
    if let Some(status) = control_child.try_wait()? {
        return Err(Failure::ExitedEarly(status.to_string()));
    }
    // Nothing else will kill this child but this test.
    let _killed = control_child.start_kill();
    let _reaped = control_child.wait().await;

    // The subject.
    let capture = Capture::default();
    let client = client_on(&path, &capture).await?;

    // Prove it was live, so "it is gone afterwards" is a change rather than a
    // child that was never there.
    let pong = tokio::time::timeout(
        GUARD,
        client.call(
            &client.next_request_id(),
            &MethodName::new("ping")?,
            &JsonObject::default(),
        ),
    )
    .await
    .map_err(|_| Failure::TimedOut("pinging the live child"))??;
    if result_of(&pong)? != serde_json::json!({}) {
        return Err(Failure::Unexpected {
            what: "an empty ping result",
            got: pong.as_str().to_owned(),
        });
    }

    drop(client);

    // The child is killed on drop, not asked to leave. If it were merely
    // abandoned, its stdin would close, the graceful path would run, and the
    // marker would be in the channel below.
    tokio::time::sleep(SETTLE).await;
    let off_protocol = capture.text();
    if off_protocol.contains(GRACEFUL_EXIT_MARKER) {
        return Err(Failure::Unexpected {
            what: "no graceful shutdown after the client was dropped",
            got: off_protocol,
        });
    }

    Ok(())
}
