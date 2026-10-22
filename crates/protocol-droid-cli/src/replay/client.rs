//! The `--client` mode: drive a recorded session against a live server.
//!
//! The other half of `--serve`. Where `serve` replays requests and compares
//! the replies, this replays the recorded turns as a client would — pairing
//! each recorded request with its recorded reply — so a session can be
//! checked against a server that is running now rather than one that was
//! running then.

use super::RecordedFrame;
use super::diff::{LineDiff, LineDiffOutcome};
use std::io::Write;

use anyhow::Context as _;
use serde_json::{Map, Value};

use crate::record::Direction;

/// What the client driver reports for one replayed turn.
#[derive(Debug, Default)]
pub struct ClientReport {
    pub(super) sent: usize,
    pub(super) matched: usize,
    pub(super) diffed: usize,
    pub(super) malformed: usize,
    /// A recorded request that the live server did not answer.
    /// The server is hung, or the request was malformed, or the
    /// server crashed mid-flight. Either way, the diff cannot
    /// continue past this turn.
    pub(super) unanswered: usize,
    /// A line that arrived on the wire that was not paired with
    /// a recorded request. The server wrote more than the
    /// recording expected.
    pub(super) unexpected: usize,
}

impl ClientReport {
    /// Whether the diff found no differences and matched every
    /// sent request with a recorded response.
    pub(super) const fn is_clean(&self) -> bool {
        self.matched == self.sent
            && self.diffed == 0
            && self.unexpected == 0
            && self.unanswered == 0
    }

    fn write_to(&self, sink: &mut impl Write) -> std::io::Result<()> {
        writeln!(
            sink,
            "{} turns replayed: {} sent, {} matched, {} diffed, {} unanswered, {} unexpected, {} malformed",
            self.sent,
            self.sent,
            self.matched,
            self.diffed,
            self.unanswered,
            self.unexpected,
            self.malformed,
        )
    }
}

/// What one paired turn looks like after the client driver has
/// collected a response.
#[derive(Debug)]
pub struct CapturedTurn {
    /// The recorded request that was sent.
    pub request: Map<String, Value>,
    /// The response the live server returned, re-serialised to
    /// the canonical wire shape.
    pub captured: Option<Map<String, Value>>,
    /// The recorded expected response.
    pub expected: Map<String, Value>,
    /// 1-indexed line in the session where the request was.
    pub request_index: usize,
}

/// Pairs the recorded lines in the order the bridge saw them.
///
/// The recorded session is a single time-ordered stream: the
/// request at index `2k` is the input, the response at index
/// `2k+1` is the answer. A response without a preceding
/// request is "unexpected"; a request without a following
/// response is "unanswered". Notifications (requests with no
/// `id`) are recorded as `client→upstream` and skipped by the
/// client driver because the bridge's answer is silence and
/// silence has nothing to diff against.
pub(super) fn pair_recorded_turns(lines: &[RecordedFrame]) -> (Vec<CapturedTurn>, ClientReport) {
    let mut report = ClientReport::default();
    let mut turns: Vec<CapturedTurn> = Vec::new();
    let mut iter = lines.iter().enumerate();
    while let Some((request_idx, frame)) = iter.next() {
        let Some(direction) = frame.direction else {
            // A line without a direction is the old
            // verdict-path shape (`{version, frame}` only). The
            // client driver needs direction; skip with a
            // malformed count and continue.
            report.malformed += 1;
            continue;
        };
        if direction != Direction::ClientToUpstream {
            // A response without a preceding request is
            // unexpected. The recording starts with a
            // request by construction, so a leading response
            // is a malformed session.
            report.unexpected += 1;
            continue;
        }
        let request = frame.frame.clone();
        let next = iter.next();
        let Some((_response_idx, response_frame)) = next else {
            // A request with no following response: the
            // recording ended mid-turn. Mark as unanswered
            // and stop — there is nothing more to pair.
            report.unanswered += 1;
            break;
        };
        let response_direction = response_frame.direction;
        if response_direction != Some(Direction::UpstreamToClient) {
            // The line after a request was not a response.
            // Either it was another request (the recording
            // interleaved notifications or the capture
            // missed a turn) or it has no direction at all.
            // Either way, the pairing breaks and the loop
            // cannot continue usefully.
            report.unexpected += 1;
            report.malformed += 1;
            continue;
        }
        let request_index = request_idx + 1;
        let expected = response_frame.frame.clone();
        turns.push(CapturedTurn {
            request,
            captured: None,
            expected,
            request_index,
        });
    }
    report.sent = turns.len();
    (turns, report)
}

/// Compares a captured response with the recorded expected
/// response and returns a per-line diff.
pub(super) fn diff_turn(turn: &CapturedTurn) -> Option<LineDiff> {
    let captured = turn.captured.as_ref()?;
    Some(LineDiff {
        index: turn.request_index,
        outcome: if captured == &turn.expected {
            LineDiffOutcome::Same
        } else {
            LineDiffOutcome::Changed
        },
        left: Some(turn.request.clone()),
        right: Some(captured.clone()),
        left_version: None,
        right_version: None,
        note: if captured == &turn.expected {
            "identical".to_owned()
        } else {
            "the live response differs from the recorded reply".to_owned()
        },
    })
}

/// The full client-driver turn-by-turn diff.
pub fn drive_session(
    turns: &mut [CapturedTurn],
    captured: Vec<Option<Map<String, Value>>>,
    out: &mut impl Write,
) -> anyhow::Result<ClientReport> {
    if captured.len() != turns.len() {
        anyhow::bail!(
            "the client driver captured {} responses for {} recorded turns; an off-by-one here means a request that the live server never answered",
            captured.len(),
            turns.len(),
        );
    }
    let mut report = ClientReport::default();
    for (turn, captured) in turns.iter_mut().zip(captured) {
        report.sent += 1;
        turn.captured = captured;
        if let Some(diff) = diff_turn(turn) {
            match diff.outcome {
                LineDiffOutcome::Same => report.matched += 1,
                LineDiffOutcome::Changed => report.diffed += 1,
                _ => {}
            }
            serde_json::to_writer(&mut *out, &diff).context("writing a per-turn diff")?;
            out.write_all(b"\n").context("writing the diff newline")?;
        }
    }
    Ok(report)
}

/// The async client driver.
///
/// `main` is the only caller; it dispatches the `replay` future
/// from inside the tokio runtime it built. The integration tests
/// (which run inside a `#[tokio::test]`) reuse the same entry
/// without re-entering a runtime.
pub(super) async fn run_as_client_async(
    session_path: &str,
    server: &[String],
) -> anyhow::Result<()> {
    use protocol_droid::adapters::primary::stdio::Diagnostics;
    use protocol_droid::adapters::secondary::stdio::{ChildSpec, ClientConfig, StdioClient};

    let (program, args) = server
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("--as-client needs at least one positional (the program); the trailing_var_arg was empty"))?;
    let spec = ChildSpec::new(program.as_str())
        .with_context(|| format!("building the child spec for {program}"))?;
    let spec = spec
        .with_args(args.iter().map(std::ffi::OsString::from))
        .with_context(|| format!("building the child args for {program}"))?;
    let config = ClientConfig::default();
    let diagnostics = Diagnostics::stderr();
    let client = StdioClient::spawn(&spec, config, diagnostics)
        .await
        .with_context(|| format!("spawning the server process {program}"))?;

    let file = std::fs::File::open(session_path)
        .with_context(|| format!("opening the session file: {session_path}"))?;
    let reader = std::io::BufReader::new(file);
    let lines: Vec<RecordedFrame> = {
        let mut collected: Vec<RecordedFrame> = Vec::new();
        for line in std::io::BufRead::lines(reader) {
            let Ok(line) = line else { continue };
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(recorded) = serde_json::from_str::<RecordedFrame>(&line) {
                collected.push(recorded);
            }
        }
        collected
    };
    let (mut turns, mut report) = pair_recorded_turns(&lines);
    if turns.is_empty() {
        anyhow::bail!(
            "the session at {session_path} contains no client->upstream/upstream->client pairs; the client driver has nothing to replay"
        );
    }

    // Drive the recorded turns through the live server,
    // capturing each response.
    let mut captured_responses: Vec<Option<Map<String, Value>>> = Vec::with_capacity(turns.len());
    for turn in &turns {
        let document = serde_json::to_string(&Value::Object(turn.request.clone()))
            .context("re-serialising a recorded request")?;
        if let Err(error) = client.send_document(&document).await {
            eprintln!(
                "the live server refused the request for turn {}: {error}",
                turn.request_index
            );
            report.unanswered += 1;
            captured_responses.push(None);
            continue;
        }
        captured_responses
            .push(capture_one_response(&client, turn.request_index, &mut report).await);
    }

    let mut stdout = std::io::stdout().lock();
    let inner_report = drive_session(&mut turns, captured_responses, &mut stdout)?;
    report.matched = inner_report.matched;
    report.diffed = inner_report.diffed;
    drop(stdout);
    report.write_to(&mut std::io::stderr().lock())?;
    if !report.is_clean() {
        anyhow::bail!(
            "the live server diverged from the recording: {} diffed, {} unanswered, {} unexpected",
            report.diffed,
            report.unanswered,
            report.unexpected,
        );
    }
    Ok(())
}

/// Reads the next frame from the live server, parses
/// it as a JSON object, and updates the report on
/// the four ways a turn can fail to produce a
/// captured response (the server closes stdout,
/// returns a non-object frame, errors on `next_frame`,
/// or — handled by the caller — refuses the request
/// the loop just sent). Returned `Some` carries a
/// parsed response; `None` is recorded against the
/// `unanswered` / `malformed` counters in the report.
///
/// Generic over the `StdioClient<DW>` sink type so
/// the helper does not lock the caller into a
/// specific writer — replay is the only caller
/// today, and it always uses `SinkDefault` (the
/// default type parameter of `StdioClient`).
///
/// The `DW` bound mirrors the one on
/// `StdioClient<DW>::next_frame`, which is the
/// only method the helper calls. `next_frame` is
/// in the `where DW: AsyncWrite + Unpin + Send +
/// 'static` block; the helper inherits the bound.
async fn capture_one_response<DW>(
    client: &protocol_droid::adapters::secondary::stdio::StdioClient<DW>,
    request_index: usize,
    report: &mut ClientReport,
) -> Option<Map<String, Value>>
where
    DW: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    match client.next_frame().await {
        Ok(Some(reply)) => match serde_json::from_str(&reply) {
            Ok(parsed) => Some(parsed),
            Err(error) => {
                eprintln!(
                    "the live server returned a frame that is not a JSON object for turn {request_index}: {error}"
                );
                report.malformed += 1;
                None
            }
        },
        Ok(None) => {
            eprintln!("the live server closed its stdout before answering turn {request_index}");
            report.unanswered += 1;
            None
        }
        Err(error) => {
            eprintln!(
                "the live server did not answer the request for turn {request_index}: {error}"
            );
            report.unanswered += 1;
            None
        }
    }
}
