//! The `--serve` mode: answer a replayed session from a local server.
//!
//! Instead of translating the recorded replies, this starts the recorded
//! server and replays the recorded requests against it, so the verdicts come
//! from a live peer rather than from the translator.

use super::RecordedFrame;
use std::collections::VecDeque;
use std::io::{BufRead, Write};

use anyhow::Context as _;
use serde_json::{Map, Value, json};



/// Opens a session file (or stdin for `-`).
pub(super) fn open_session(
    path: &str,
    label: &str,
) -> anyhow::Result<std::io::BufReader<Box<dyn std::io::Read + Send>>> {
    let reader: Box<dyn std::io::Read + Send> = if path == "-" {
        Box::new(std::io::stdin())
    } else {
        let file = std::fs::File::open(path)
            .with_context(|| format!("opening the {label} session file: {path}"))?;
        Box::new(file)
    };
    Ok(std::io::BufReader::new(reader))
}

// ============================================================================
// Dynamic replay: --as-server
//
// A mock MCP server that answers every JSON-RPC request with a recorded
// `upstream→client` frame from the session, matched by the request's `id`.
// The server speaks over stdio: it reads from the configured reader and
// writes to the configured writer. The recorded session is the contract —
// a test client can `pipe` a transcript to it and assert the responses.
//
// Matching is by `id`. A recorded session carries one `upstream→client`
// frame per request id, in the order the capture tool saw them, and the
// mock serves them in the same order. A request whose id is not in the
// recording is answered with a typed JSON-RPC `method not found` so the
// test client gets a refusal rather than a hang; a request whose id is
// already used is also a refusal (the recorded reply was already
// served). Notifications (no id) are acknowledged by silence, which is
// what an MCP server does for `notifications/initialized`.
// ============================================================================

/// What the mock reports for one served reply.
#[derive(Debug, Default)]
pub struct ServeReport {
    pub(super) answered: usize,
    pub(super) not_found: usize,
    pub(super) notifications: usize,
    pub(super) unreadable: usize,
}

impl ServeReport {
    /// Whether the mock answered at least one request.
    ///
    /// A typed refusal counts as "answered" — the test client
    /// sent a request and got a JSON-RPC reply back. A session
    /// that yielded zero replies and zero refusals is empty
    /// input; a session that yielded only refusals is a valid
    /// mock that the test client is asking the wrong things.
    pub const fn served_any(&self) -> bool {
        self.answered > 0 || self.not_found > 0
    }

    fn write_to(&self, sink: &mut impl Write) -> std::io::Result<()> {
        writeln!(
            sink,
            "{} requests served: {} answered, {} not-found, {} notifications, {} unreadable",
            self.answered + self.not_found,
            self.answered,
            self.not_found,
            self.notifications,
            self.unreadable,
        )
    }
}

/// Looks up the recorded reply for one request id, drawing from a
/// `VecDeque` of pre-parsed `upstream→client` frames.
fn take_reply_by_id(
    replies: &mut VecDeque<Map<String, Value>>,
    id: &Value,
) -> Option<Map<String, Value>> {
    for (index, reply) in replies.iter().enumerate() {
        if reply.get("id") == Some(id) {
            return replies.remove(index);
        }
    }
    None
}

/// The JSON-RPC error code the mock returns for a request that is
/// not in the recorded session.
///
/// `-32601 Method not found` is the spec's answer for a method the
/// server does not know, and "the recorded session did not capture
/// a reply for this id" is the closest analogue when the *server*
/// is a recording rather than a process. The error is honest in
/// both readings: the test client's request reached a server that
/// has no answer for it.
const METHOD_NOT_FOUND: i32 = -32_601;

/// A mock server that drains a `FrameSource`, matches each request
/// against a pre-loaded queue of recorded replies, and writes the
/// matches (or typed refusals) to a `FrameSink`.
///
/// Split out from the clap-driven `run_as_server` so the same
/// loop can be driven from a test with an in-process source and
/// sink, without going through stdin/stdout or a child process.
pub fn serve_with_io<R, W, S>(
    source: R,
    sink: &mut W,
    summary: &mut S,
    mut replies: VecDeque<Map<String, Value>>,
) -> anyhow::Result<ServeReport>
where
    R: BufRead,
    W: Write,
    S: Write,
{
    let mut report = ServeReport::default();
    for line in source.lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                writeln!(summary, "could not read a frame: {error}")?;
                report.unreadable += 1;
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let envelope: Map<String, Value> = match serde_json::from_str(&line) {
            Ok(envelope) => envelope,
            Err(error) => {
                writeln!(summary, "the incoming frame is not a JSON object: {error}")?;
                report.unreadable += 1;
                continue;
            }
        };
        let id = envelope.get("id").cloned();
        // Notifications have no `id`. The spec answers them with
        // silence; a mock that wrote a response would invent a
        // peer the capture never saw.
        let Some(id) = id else {
            report.notifications += 1;
            continue;
        };
        if let Some(reply) = take_reply_by_id(&mut replies, &id) {
            let serialised = serde_json::to_string(&Value::Object(reply))
                .context("serialising a recorded reply")?;
            writeln!(sink, "{serialised}").context("writing a recorded reply")?;
            report.answered += 1;
        } else {
            let refusal = json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": METHOD_NOT_FOUND,
                    "message": "this id is not in the recorded session",
                },
            });
            let serialised =
                serde_json::to_string(&refusal).context("serialising a not-found refusal")?;
            writeln!(sink, "{serialised}").context("writing a not-found refusal")?;
            report.not_found += 1;
        }
    }
    report.write_to(summary)?;
    Ok(report)
}

/// Loads a session, extracts the `upstream→client` frames in order,
/// and starts the mock server on stdin/stdout.
pub(super) fn run_as_server(session_path: &str) -> anyhow::Result<()> {
    let file = std::fs::File::open(session_path)
        .with_context(|| format!("opening the session file: {session_path}"))?;
    let reader = std::io::BufReader::new(file);
    let mut replies: VecDeque<Map<String, Value>> = VecDeque::new();
    // Only the upstream→client frames are answers. The recorded
    // client→upstream frames are the requests the mock is *expecting*,
    // not the answers; a mock that answered with the request itself
    // would be a peer that talks to itself. The recorded pair is
    // (client→upstream, upstream→client); the mock sees the
    // client→upstream half as fresh input and serves the
    // upstream→client half back.
    let mut total_recorded = 0usize;
    for line in std::io::BufRead::lines(reader) {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(recorded) = serde_json::from_str::<RecordedFrame>(&line) else {
            continue;
        };
        total_recorded += 1;
        if !looks_like_response(&recorded.frame) {
            continue;
        }
        replies.push_back(recorded.frame);
    }
    if replies.is_empty() {
        anyhow::bail!(
            "the session at {session_path} contains no `upstream→client` frames; the mock has nothing to serve (recorded {total_recorded} total frames)"
        );
    }
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut err = std::io::stderr().lock();
    let report = serve_with_io(stdin.lock(), &mut out, &mut err, replies)?;
    drop((out, err));
    // A mock that served nothing at all is a configuration
    // problem (the test client closed the stream before sending,
    // or the operator pointed at the wrong session file). A
    // mock that served a typed refusal is a *correct* answer to
    // a request that the recording did not capture; the test
    // client gets a refusal and the loop continues. The bail
    // fires only when the report is empty in every column.
    if report.answered == 0
        && report.not_found == 0
        && report.notifications == 0
        && report.unreadable == 0
    {
        anyhow::bail!(
            "the mock answered no requests; check the session file or the test client's request ids"
        );
    }
    Ok(())
}

/// Whether a recorded frame is a response (has `result` or `error`)
/// rather than a request (has `method`).
///
/// The recorded shape is a JSON-RPC envelope; the discriminator is
/// the presence of `result` or `error`. A notification also lacks
/// `result` / `error`, but a recorded session writes responses and
/// requests in matched pairs and the responses are the half the
/// mock needs.
pub(super) fn looks_like_response(frame: &Map<String, Value>) -> bool {
    frame.contains_key("result") || frame.contains_key("error")
}

// ============================================================================
// Dynamic replay: --as-client
//
// The other half of the dynamic replay. Reads a recorded session,
// spawns a server process the operator passes as a positional
// argument, and replays the recorded `client→upstream` frames
// against the live server, capturing each response and diffing it
// against the recorded `upstream→client` frame at the next
// position. Same server = zero diffs; mutated server = a precise
// diff at the request whose response changed.
//
// Pairing is by (request line, next response line), not by
// line index. The session is a single time-ordered stream: the
// request at index `2k` is the input the bridge saw, the
// response at index `2k+1` is the answer the bridge served.
// The recorded session is the contract; the as-client engine
// re-issues the requests and verifies the responses.
// ============================================================================

