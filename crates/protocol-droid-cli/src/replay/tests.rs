//! The replay command's tests.
//!
//! Out of the command so that reading `replay` means reading the four modes
//! rather than the tests that sit underneath all of them.

use super::client::diff_turn;
use super::client::pair_recorded_turns;
use super::report::leg_for;
use super::report::replay_one;
use super::*;
use protocol_droid::adapters::secondary::codec::result::WireToolResults;
use protocol_droid::application::bridge::Translator;
use protocol_droid::domain::{DegradePolicy, ProtocolVersion};
use serde_json::json;
use std::collections::VecDeque;

fn policy() -> DegradePolicy {
    DegradePolicy::permissive()
}

fn target() -> ProtocolVersion {
    ProtocolVersion::V2024_11_05
}

/// A well-formed `ReplayArgs` is constructible. The clap
/// struct lives in the lib because the integration test
/// pins its shape here; without a lib-internal reference
/// the dead-code-pub-in-binary lint flags it for the lib
/// test target, which does not see the bin.
#[test]
fn clap_args_are_well_formed() {
    let args = ReplayArgs {
        session: "session.jsonl".to_owned(),
        server: Vec::new(),
        target: Some("2024-11-05".to_owned()),
        explain: true,
        allow_downgrade: true,
        diff: None,
        as_server: false,
        as_client: false,
    };
    assert_eq!(args.target.as_deref(), Some("2024-11-05"));
    assert!(args.explain);
    assert!(args.allow_downgrade);
    assert!(args.diff.is_none());
    assert!(!args.as_server);
    assert!(!args.as_client);
    assert!(args.server.is_empty());
}

/// `run` is the clap-driven entry point. It is exercised by
/// the binary at runtime; the lib test target does not see
/// the bin, so the lint flags the unused `pub`. Referenced
/// here to keep it reachable from the lib's own tests.
#[test]
fn run_is_reachable() {
    let args = ReplayArgs {
        session: "session.jsonl".to_owned(),
        server: Vec::new(),
        target: Some("2024-11-05".to_owned()),
        explain: false,
        allow_downgrade: true,
        diff: None,
        as_server: false,
        as_client: false,
    };
    // The function opens a real file (or stdin for `-`);
    // this test is a lint-pin only, so the path is bogus
    // and the call is expected to fail.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build the test runtime");
    let _ = runtime.block_on(run(&args));
}

/// A simple text result is forwardable to every revision.
#[test]
fn text_result_is_forwarded_to_2024_11_05() -> anyhow::Result<()> {
    let line = r#"{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}"}"#;
    let verdict = replay_one(
        line,
        1,
        target(),
        &leg_for(target()),
        policy(),
        &Translator::new(WireToolResults),
    )?
    .expect("a well-formed line produces a verdict");
    assert_eq!(verdict.outcome, VerdictOutcome::Forwarded);
    assert!(verdict.unchanged);
    Ok(())
}

/// A line whose `frame` is what `record` actually writes — a JSON
/// string, not a JSON object — is the regression test for the
/// format mismatch between `record` and `replay`. Without the
/// `deserialize_frame_string` adapter, this line is reported as a
/// type error and the verdict is `Refused`; with it, the verdict
/// is `Forwarded` and the bytes round-trip.
#[test]
fn record_string_frame_round_trips() -> anyhow::Result<()> {
    let line = r#"{"direction":"client→upstream","version":"2025-06-18","clock_ns":42,"frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}"}"#;
    let verdict = replay_one(
        line,
        1,
        target(),
        &leg_for(target()),
        policy(),
        &Translator::new(WireToolResults),
    )?
    .expect("a record-shape line produces a verdict");
    assert_eq!(verdict.outcome, VerdictOutcome::Forwarded);
    Ok(())
}

/// A line whose `frame` is a bare `null` is reported as
/// `Refused` with an error naming the parse failure. The
/// refactor to the `frame` deserialiser would have dropped
/// the error path silently if this test passed with a
/// `Forwarded` outcome.
#[test]
fn record_string_frame_with_null_is_refused() {
    let line = r#"{"version":"2025-06-18","frame":null}"#;
    let _verdict = replay_one(
        line,
        1,
        target(),
        &leg_for(target()),
        policy(),
        &Translator::new(WireToolResults),
    )
    .expect_err("a null frame is a parse error, not a verdict");
}

/// A line whose `frame` is a JSON string that does not parse
/// is reported as `Refused` with an error naming the parse
/// failure. The deserialiser forwards the parse error rather
/// than swallowing it.
#[test]
fn record_string_frame_with_unparseable_value_is_refused() {
    let line = r#"{"version":"2025-06-18","frame":"not even json"}"#;
    let _verdict = replay_one(
        line,
        1,
        target(),
        &leg_for(target()),
        policy(),
        &Translator::new(WireToolResults),
    )
    .expect_err("an unparseable frame is a parse error, not a verdict");
}

/// A 2025-06-18 `structuredContent` cannot reach 2024-11-05.
#[test]
fn structured_content_is_downgraded_when_replayed_to_2024_11_05() -> anyhow::Result<()> {
    let line = r#"{"version":"2025-06-18","frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}],\"structuredContent\":{\"key\":\"value\"}}}"}"#;
    let verdict = replay_one(
        line,
        1,
        target(),
        &leg_for(target()),
        policy(),
        &Translator::new(WireToolResults),
    )?
    .expect("a well-formed line produces a verdict");
    assert_eq!(verdict.outcome, VerdictOutcome::Downgraded);
    assert!(!verdict.unchanged);
    assert!(
        verdict
            .steps
            .iter()
            .any(|step| step.contains("StructuredContent")),
        "the downgrade step should name `StructuredContent`; got {:?}",
        verdict.steps,
    );
    Ok(())
}

/// An error envelope is forwarded (a refusal is the upstream's own
/// words about a request, not a result this layer has any opinion on).
#[test]
fn error_envelope_is_forwarded() -> anyhow::Result<()> {
    let line = r#"{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32601,\"message\":\"Method not found\"}}"}"#;
    let verdict = replay_one(
        line,
        1,
        target(),
        &leg_for(target()),
        policy(),
        &Translator::new(WireToolResults),
    )?
    .expect("a well-formed line produces a verdict");
    assert_eq!(verdict.outcome, VerdictOutcome::Forwarded);
    assert!(verdict.unchanged);
    Ok(())
}

/// A request envelope (no `result`, no `error`) is forwarded.
#[test]
fn request_envelope_is_forwarded() -> anyhow::Result<()> {
    let line =
        r#"{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}"}"#;
    let verdict = replay_one(
        line,
        1,
        target(),
        &leg_for(target()),
        policy(),
        &Translator::new(WireToolResults),
    )?
    .expect("a well-formed line produces a verdict");
    assert_eq!(verdict.outcome, VerdictOutcome::Forwarded);
    assert!(verdict.unchanged);
    Ok(())
}

/// A line that is not `{version, frame}` is reported and skipped.
#[test]
fn malformed_line_returns_err_so_caller_can_skip() {
    let line = r#"{"not":"a frame"}"#;
    let result = replay_one(
        line,
        1,
        target(),
        &leg_for(target()),
        policy(),
        &Translator::new(WireToolResults),
    );
    assert!(
        result.is_err(),
        "a malformed line should be an error, got Ok"
    );
}

/// An unknown recorded version is reported as `Refused` (the
/// build cannot lift the source frame, so the verdict is
/// unknowable). The bridge would forward the bytes untouched
/// at runtime, but `replay` answers the user's question
/// "what would happen?", and a `Forwarded` verdict would be
/// a lie the build cannot back up.
#[test]
fn unknown_recorded_version_is_refused_not_silently_forwarded() -> anyhow::Result<()> {
    let line = r#"{"version":"2099-01-01","frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}"}"#;
    let verdict = replay_one(
        line,
        1,
        target(),
        &leg_for(target()),
        policy(),
        &Translator::new(WireToolResults),
    )?
    .expect("a well-formed line produces a verdict");
    assert_eq!(verdict.outcome, VerdictOutcome::Refused);
    assert!(verdict.error.is_some());
    assert!(verdict.note.contains("not a revision this build can read"));
    Ok(())
}

/// A multi-line session produces a verdict per frame plus a
/// summary line, and the totals add up.
#[test]
fn run_with_io_totals_match_the_frames() -> anyhow::Result<()> {
    let input = "\
{\"version\":\"2024-11-05\",\"frame\":\"{\\\"jsonrpc\\\":\\\"2.0\\\",\\\"id\\\":1,\\\"result\\\":{\\\"content\\\":[{\\\"type\\\":\\\"text\\\",\\\"text\\\":\\\"hi\\\"}]}}\"}
{\"version\":\"2025-06-18\",\"frame\":\"{\\\"jsonrpc\\\":\\\"2.0\\\",\\\"id\\\":2,\\\"result\\\":{\\\"content\\\":[{\\\"type\\\":\\\"text\\\",\\\"text\\\":\\\"hi\\\"}],\\\"structuredContent\\\":{\\\"k\\\":\\\"v\\\"}}}\"}
{\"version\":\"2024-11-05\",\"frame\":\"{\\\"jsonrpc\\\":\\\"2.0\\\",\\\"id\\\":3,\\\"error\\\":{\\\"code\\\":-32601,\\\"message\\\":\\\"Method not found\\\"}}\"}
";
    let mut out: Vec<u8> = Vec::new();
    let mut summary: Vec<u8> = Vec::new();
    let counters = run_with_io(
        input.as_bytes(),
        target(),
        policy(),
        false,
        &mut out,
        &mut summary,
    )?;
    assert_eq!(counters.forwarded, 2);
    assert_eq!(counters.downgraded, 1);
    assert_eq!(counters.refused, 0);
    assert_eq!(counters.skipped, 0);
    let output = std::str::from_utf8(&out).expect("output is JSONL of utf-8");
    let lines = output.lines().count();
    assert_eq!(lines, 3, "one verdict per frame");
    let summary = std::str::from_utf8(&summary).expect("summary is utf-8");
    assert!(summary.contains("3 frames replayed"));
    Ok(())
}

/// A session with a bad line reports the line number, skips the
/// line, and continues with the rest.
#[test]
fn run_with_io_skips_a_malformed_line_and_keeps_going() -> anyhow::Result<()> {
    let input = "\
{\"version\":\"2024-11-05\",\"frame\":\"{\\\"jsonrpc\\\":\\\"2.0\\\",\\\"id\\\":1,\\\"result\\\":{\\\"content\\\":[{\\\"type\\\":\\\"text\\\",\\\"text\\\":\\\"hi\\\"}]}}\"}
this is not json
{\"version\":\"2024-11-05\",\"frame\":\"{\\\"jsonrpc\\\":\\\"2.0\\\",\\\"id\\\":2,\\\"error\\\":{\\\"code\\\":-32601,\\\"message\\\":\\\"Method not found\\\"}}\"}
";
    let mut out: Vec<u8> = Vec::new();
    let mut summary: Vec<u8> = Vec::new();
    let counters = run_with_io(
        input.as_bytes(),
        target(),
        policy(),
        false,
        &mut out,
        &mut summary,
    )?;
    assert_eq!(counters.forwarded, 2);
    assert_eq!(counters.skipped, 1);
    let output = std::str::from_utf8(&out).expect("output is JSONL of utf-8");
    let lines = output.lines().count();
    assert_eq!(lines, 3, "the malformed line still gets a verdict record");
    let summary = std::str::from_utf8(&summary).expect("summary is utf-8");
    assert!(
        summary.contains("line 2:"),
        "the bad line is named on stderr; got {summary:?}"
    );
    Ok(())
}

// -- Session-vs-session diff -------------------------------------------

/// Two identical sessions produce a `same` outcome for every line
/// and exit cleanly. This is the property the T20 spec calls
/// "replay against the same server: zero diffs".
#[test]
fn diff_with_io_reports_same_for_identical_sessions() -> anyhow::Result<()> {
    let session = r#"{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}"}
{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}"}
"#;
    let mut out: Vec<u8> = Vec::new();
    let mut summary: Vec<u8> = Vec::new();
    let totals = diff_with_io(
        session.as_bytes(),
        session.as_bytes(),
        &mut out,
        &mut summary,
    )?;
    assert_eq!(totals.same, 2);
    assert_eq!(totals.changed, 0);
    assert_eq!(totals.added, 0);
    assert_eq!(totals.removed, 0);
    assert!(totals.is_clean(), "a same-only diff is clean");
    let output = std::str::from_utf8(&out).expect("output is JSONL of utf-8");
    let lines: Vec<LineDiff> = output
        .lines()
        .map(|line| serde_json::from_str(line).expect("the diff is well-formed"))
        .collect();
    assert_eq!(lines.len(), 2);
    let first = lines.first().expect("at least one diff record");
    assert_eq!(first.outcome, LineDiffOutcome::Same);
    let second = lines.get(1).expect("the second line is present");
    assert_eq!(second.outcome, LineDiffOutcome::Same);
    Ok(())
}

/// A one-byte difference on one line is reported as `changed`
/// at that line and `same` everywhere else. The precise diff
/// the T20 spec wants to see.
#[test]
fn diff_with_io_reports_changed_for_a_one_byte_difference() -> anyhow::Result<()> {
    let left = r#"{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}"}
{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}"}
"#;
    let right = r#"{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}"}
{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"changed\":true}}"}
"#;
    let mut out: Vec<u8> = Vec::new();
    let mut summary: Vec<u8> = Vec::new();
    let totals = diff_with_io(left.as_bytes(), right.as_bytes(), &mut out, &mut summary)?;
    assert_eq!(totals.same, 1);
    assert_eq!(totals.changed, 1);
    assert_eq!(totals.added, 0);
    assert_eq!(totals.removed, 0);
    assert!(!totals.is_clean());
    let output = std::str::from_utf8(&out).expect("output is JSONL of utf-8");
    let lines: Vec<LineDiff> = output
        .lines()
        .map(|line| serde_json::from_str(line).expect("the diff is well-formed"))
        .collect();
    assert_eq!(lines.len(), 2);
    let first = lines.first().expect("at least one diff record");
    assert_eq!(first.outcome, LineDiffOutcome::Same);
    let second = lines.get(1).expect("the changed line is present");
    assert_eq!(second.outcome, LineDiffOutcome::Changed);
    assert!(second.left.is_some());
    assert!(second.right.is_some());
    assert_ne!(second.left, second.right);
    Ok(())
}

/// A line present on the left but not the right is `removed`;
/// a line present on the right but not the left is `added`.
/// A run that grew shorter on the right reports the
/// lost lines as removed, and vice versa.
#[test]
fn diff_with_io_reports_added_and_removed() -> anyhow::Result<()> {
    let left = r#"{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}"}
{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}"}
{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{}}"}
"#;
    let right = r#"{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}"}
{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":99,\"result\":{}}"}
"#;
    let mut out: Vec<u8> = Vec::new();
    let mut summary: Vec<u8> = Vec::new();
    let totals = diff_with_io(left.as_bytes(), right.as_bytes(), &mut out, &mut summary)?;
    assert_eq!(totals.same, 1);
    assert_eq!(totals.changed, 1);
    assert_eq!(totals.added, 0);
    assert_eq!(totals.removed, 1);
    let output = std::str::from_utf8(&out).expect("output is JSONL of utf-8");
    let lines: Vec<LineDiff> = output
        .lines()
        .map(|line| serde_json::from_str(line).expect("the diff is well-formed"))
        .collect();
    assert_eq!(lines.len(), 3, "one diff record per paired line up to max");
    let first = lines.first().expect("at least one diff record");
    assert_eq!(first.outcome, LineDiffOutcome::Same);
    let second = lines.get(1).expect("the changed line is present");
    assert_eq!(second.outcome, LineDiffOutcome::Changed);
    let third = lines.get(2).expect("the removed line is present");
    assert_eq!(third.outcome, LineDiffOutcome::Removed);
    Ok(())
}

/// A different recorded version on the same line is `changed`.
/// The diff is on the version, not just the bytes; a recording
/// tool that re-emitted a frame at a different negotiated
/// revision is a real diff.
#[test]
fn diff_with_io_reports_changed_on_version_difference() -> anyhow::Result<()> {
    let left = r#"{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}"}
"#;
    let right = r#"{"version":"2025-06-18","frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}"}
"#;
    let mut out: Vec<u8> = Vec::new();
    let mut summary: Vec<u8> = Vec::new();
    let totals = diff_with_io(left.as_bytes(), right.as_bytes(), &mut out, &mut summary)?;
    assert_eq!(totals.changed, 1);
    assert_eq!(totals.same, 0);
    Ok(())
}

/// Empty sessions produce no diff records and a clean summary.
/// The `for i in 0..0` loop terminates immediately; the only
/// output is the summary line.
#[test]
fn diff_with_io_handles_empty_sessions() -> anyhow::Result<()> {
    let mut out: Vec<u8> = Vec::new();
    let mut summary: Vec<u8> = Vec::new();
    let totals = diff_with_io(b"" as &[u8], b"" as &[u8], &mut out, &mut summary)?;
    assert_eq!(totals.same, 0);
    assert!(totals.is_clean());
    let summary_text = std::str::from_utf8(&summary).expect("summary is utf-8");
    assert!(summary_text.contains("0 lines diffed"));
    Ok(())
}

// -- Dynamic replay: --as-server ----------------------------------------

/// A request whose id matches a recorded reply gets the
/// recorded reply back. The mock answers in the same
/// order it sees ids, but the lookup is by id, not by
/// queue position — a client that issued requests 1, 2,
/// 3 in order gets answers 1, 2, 3 in order.
#[test]
fn serve_with_io_answers_matching_ids() -> anyhow::Result<()> {
    let replies: VecDeque<Map<String, Value>> = vec![
        json!({"jsonrpc":"2.0","id":1,"result":{"v":"one"}})
            .as_object()
            .cloned()
            .expect("an object"),
        json!({"jsonrpc":"2.0","id":2,"result":{"v":"two"}})
            .as_object()
            .cloned()
            .expect("an object"),
    ]
    .into();
    let input = "\
{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}
{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}
";
    let mut out: Vec<u8> = Vec::new();
    let mut summary: Vec<u8> = Vec::new();
    let report = serve_with_io(input.as_bytes(), &mut out, &mut summary, replies)?;
    assert_eq!(report.answered, 2);
    assert_eq!(report.not_found, 0);
    assert!(report.served_any());
    let output = std::str::from_utf8(&out).expect("output is utf-8");
    assert!(
        output.contains("\"v\":\"one\""),
        "first reply served; got {output:?}"
    );
    assert!(
        output.contains("\"v\":\"two\""),
        "second reply served; got {output:?}"
    );
    Ok(())
}

/// A request whose id is not in the recording gets a typed
/// JSON-RPC `method not found` error rather than a hang.
/// The mock never blocks the test client.
#[test]
fn serve_with_io_reports_method_not_found_for_unknown_id() -> anyhow::Result<()> {
    let replies: VecDeque<Map<String, Value>> = vec![
        json!({"jsonrpc":"2.0","id":1,"result":{}})
            .as_object()
            .cloned()
            .expect("an object"),
    ]
    .into();
    let input = "{\"jsonrpc\":\"2.0\",\"id\":99,\"method\":\"unknown\"}\n";
    let mut out: Vec<u8> = Vec::new();
    let mut summary: Vec<u8> = Vec::new();
    let report = serve_with_io(input.as_bytes(), &mut out, &mut summary, replies)?;
    assert_eq!(report.answered, 0);
    assert_eq!(report.not_found, 1);
    // A typed refusal still counts as "served" — the
    // test client sent a request and got a JSON-RPC
    // reply back. The binary uses this distinction to
    // bail only on an empty input, not on a wrong-id
    // request.
    assert!(report.served_any());
    let output = std::str::from_utf8(&out).expect("output is utf-8");
    assert!(
        output.contains("-32601"),
        "the not-found error code is reported; got {output:?}"
    );
    Ok(())
}

/// A notification (no `id`) is acknowledged by silence. The
/// mock does not write a reply, the test client does not
/// expect one, and the next request still finds its
/// recorded answer.
#[test]
fn serve_with_io_silently_consumes_notifications() -> anyhow::Result<()> {
    let replies: VecDeque<Map<String, Value>> = vec![
        json!({"jsonrpc":"2.0","id":2,"result":{}})
            .as_object()
            .cloned()
            .expect("an object"),
    ]
    .into();
    let input = "\
{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}
{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}
";
    let mut out: Vec<u8> = Vec::new();
    let mut summary: Vec<u8> = Vec::new();
    let report = serve_with_io(input.as_bytes(), &mut out, &mut summary, replies)?;
    assert_eq!(report.notifications, 1);
    assert_eq!(report.answered, 1);
    let output = std::str::from_utf8(&out).expect("output is utf-8");
    assert!(
        !output.contains("notifications/initialized"),
        "no reply to a notification; got {output:?}"
    );
    assert!(
        output.contains("\"id\":2"),
        "the recorded reply is still served; got {output:?}"
    );
    Ok(())
}

/// The id-mismatch is the spec-shaped answer for a request
/// the mock has no recording for. The recorded reply for
/// id 1 is served first, the unknown id 99 gets a
/// refusal, and the recorded reply for id 2 is served
/// last — the mock does not give up after the first
/// not-found.
#[test]
fn serve_with_io_keeps_serving_after_a_not_found() -> anyhow::Result<()> {
    let replies: VecDeque<Map<String, Value>> = vec![
        json!({"jsonrpc":"2.0","id":1,"result":{"v":"one"}})
            .as_object()
            .cloned()
            .expect("an object"),
        json!({"jsonrpc":"2.0","id":2,"result":{"v":"two"}})
            .as_object()
            .cloned()
            .expect("an object"),
    ]
    .into();
    let input = "\
{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"first\"}
{\"jsonrpc\":\"2.0\",\"id\":99,\"method\":\"unknown\"}
{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"second\"}
";
    let mut out: Vec<u8> = Vec::new();
    let mut summary: Vec<u8> = Vec::new();
    let report = serve_with_io(input.as_bytes(), &mut out, &mut summary, replies)?;
    assert_eq!(report.answered, 2);
    assert_eq!(report.not_found, 1);
    let output = std::str::from_utf8(&out).expect("output is utf-8");
    assert!(output.contains("\"v\":\"one\""));
    assert!(output.contains("\"v\":\"two\""));
    assert!(output.contains("-32601"));
    Ok(())
}

/// An empty recording plus an empty input is a clean no-op:
/// no answers, no not-found, the summary line is the only
/// output. A test client that never sent a request gets
/// the same answer as a test that closed the stream
/// immediately.
#[test]
fn serve_with_io_handles_empty_input() -> anyhow::Result<()> {
    let replies: VecDeque<Map<String, Value>> = VecDeque::new();
    let mut out: Vec<u8> = Vec::new();
    let mut summary: Vec<u8> = Vec::new();
    let report = serve_with_io(b"" as &[u8], &mut out, &mut summary, replies)?;
    assert_eq!(report.answered, 0);
    assert!(!report.served_any());
    Ok(())
}

/// A frame that is not a JSON object is reported on the
/// summary stream and skipped. The mock does not crash on
/// malformed input — a test client that wrote a partial
/// frame should not bring the mock down.
#[test]
fn serve_with_io_skips_unparseable_lines() -> anyhow::Result<()> {
    let replies: VecDeque<Map<String, Value>> = vec![
        json!({"jsonrpc":"2.0","id":1,"result":{}})
            .as_object()
            .cloned()
            .expect("an object"),
    ]
    .into();
    let input = "\
this is not json
{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"x\"}
";
    let mut out: Vec<u8> = Vec::new();
    let mut summary: Vec<u8> = Vec::new();
    let report = serve_with_io(input.as_bytes(), &mut out, &mut summary, replies)?;
    assert_eq!(report.unreadable, 1);
    assert_eq!(report.answered, 1);
    Ok(())
}

// -- Dynamic replay: --as-client ---------------------------------------

/// Builds a `RecordedFrame` for a test fixture.
///
/// The field list mirrors the `record` module's on-disk
/// shape; `serde_json::from_str` round-trips through the
/// same fields the production path sees. The helper lives
/// next to the tests because every as-client test needs
/// paired (request, response) frames and a hand-rolled
/// literal per test is noise.
fn recorded(direction: Direction, frame: Map<String, Value>, version: &str) -> RecordedFrame {
    RecordedFrame {
        direction: Some(direction),
        version: version.to_owned(),
        frame,
    }
}

/// The recorded session is a single time-ordered stream;
/// `pair_recorded_turns` returns one `CapturedTurn` per
/// `(c→u, u→c)` pair. This is the T20 spec's "same session
/// produces one turn per recorded request" property.
#[test]
fn pair_recorded_turns_pairs_requests_with_responses() {
    let lines = vec![
        recorded(
            Direction::ClientToUpstream,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize"})
                .as_object()
                .cloned()
                .expect("an object"),
            "2024-11-05",
        ),
        recorded(
            Direction::UpstreamToClient,
            json!({"jsonrpc":"2.0","id":1,"result":{"v":"one"}})
                .as_object()
                .cloned()
                .expect("an object"),
            "2024-11-05",
        ),
        recorded(
            Direction::ClientToUpstream,
            json!({"jsonrpc":"2.0","id":2,"method":"ping"})
                .as_object()
                .cloned()
                .expect("an object"),
            "2024-11-05",
        ),
        recorded(
            Direction::UpstreamToClient,
            json!({"jsonrpc":"2.0","id":2,"result":{}})
                .as_object()
                .cloned()
                .expect("an object"),
            "2024-11-05",
        ),
    ];
    let (turns, report) = pair_recorded_turns(&lines);
    assert_eq!(turns.len(), 2);
    let first = turns.first().expect("at least one turn");
    let second = turns.get(1).expect("the second turn");
    assert_eq!(first.request_index, 1);
    assert_eq!(second.request_index, 3);
    assert!(first.captured.is_none());
    assert_eq!(report.sent, 2);
    assert_eq!(report.unexpected, 0);
    assert_eq!(report.unanswered, 0);
    assert_eq!(report.malformed, 0);
}

/// A request without a following response is `unanswered`
/// and the loop stops — there is nothing more to pair.
#[test]
fn pair_recorded_turns_marks_a_trailing_request_as_unanswered() {
    let lines = vec![recorded(
        Direction::ClientToUpstream,
        json!({"jsonrpc":"2.0","id":1,"method":"x"})
            .as_object()
            .cloned()
            .expect("an object"),
        "2024-11-05",
    )];
    let (turns, report) = pair_recorded_turns(&lines);
    assert!(turns.is_empty());
    assert_eq!(report.unanswered, 1);
}

/// A response without a preceding request is `unexpected`
/// and the loop continues — the recording starts with a
/// request by construction, so a leading response is a
/// session shape problem, not a fatal one.
#[test]
fn pair_recorded_turns_marks_a_leading_response_as_unexpected() {
    let lines = vec![recorded(
        Direction::UpstreamToClient,
        json!({"jsonrpc":"2.0","id":1,"result":{}})
            .as_object()
            .cloned()
            .expect("an object"),
        "2024-11-05",
    )];
    let (turns, report) = pair_recorded_turns(&lines);
    assert!(turns.is_empty());
    assert_eq!(report.unexpected, 1);
}

/// A line without a `direction` field (the verdict-path
/// shape `{version, frame}`) is `malformed` and skipped.
/// The client driver requires direction; an old-shape
/// session is a user error, not a bug to silently
/// translate.
#[test]
fn pair_recorded_turns_marks_directionless_lines_as_malformed() {
    let lines = vec![RecordedFrame {
        direction: None,
        version: "2024-11-05".to_owned(),
        frame: json!({"jsonrpc":"2.0","id":1,"method":"x"})
            .as_object()
            .cloned()
            .expect("an object"),
    }];
    let (turns, report) = pair_recorded_turns(&lines);
    assert!(turns.is_empty());
    assert_eq!(report.malformed, 1);
}

/// `diff_turn` returns `Same` when the captured response
/// equals the recorded expected; `Changed` when it does
/// not. The captured response replaces the `Option<None>`
/// left in the test fixture, then `diff_turn` is the
/// shape of a per-turn diff entry.
#[test]
fn diff_turn_reports_same_when_captured_matches_expected() {
    let mut turn = CapturedTurn {
        request: json!({"jsonrpc":"2.0","id":1,"method":"x"})
            .as_object()
            .cloned()
            .expect("an object"),
        captured: None,
        expected: json!({"jsonrpc":"2.0","id":1,"result":{}})
            .as_object()
            .cloned()
            .expect("an object"),
        request_index: 1,
    };
    turn.captured = Some(turn.expected.clone());
    let diff = diff_turn(&turn).expect("a captured response produces a diff");
    assert_eq!(diff.outcome, LineDiffOutcome::Same);
}

/// A captured response that differs from the expected is
/// `Changed`. The per-turn diff is the operator's way to
/// see "this request returned something different from
/// the recording".
#[test]
fn diff_turn_reports_changed_when_captured_differs_from_expected() {
    let turn = CapturedTurn {
        request: json!({"jsonrpc":"2.0","id":1,"method":"x"})
            .as_object()
            .cloned()
            .expect("an object"),
        captured: Some(
            json!({"jsonrpc":"2.0","id":1,"result":{"mutated":true}})
                .as_object()
                .cloned()
                .expect("an object"),
        ),
        expected: json!({"jsonrpc":"2.0","id":1,"result":{}})
            .as_object()
            .cloned()
            .expect("an object"),
        request_index: 1,
    };
    let diff = diff_turn(&turn).expect("a captured response produces a diff");
    assert_eq!(diff.outcome, LineDiffOutcome::Changed);
    assert!(diff.note.contains("live response differs"));
}

/// `drive_session` takes the captured responses and
/// writes one `LineDiff` per turn to the sink. The
/// report's matched/diffed counts add up to the turn
/// count.
#[test]
fn drive_session_writes_one_line_per_turn() -> anyhow::Result<()> {
    let mut turns = vec![
        CapturedTurn {
            request: json!({"jsonrpc":"2.0","id":1,"method":"x"})
                .as_object()
                .cloned()
                .expect("an object"),
            captured: Some(
                json!({"jsonrpc":"2.0","id":1,"result":{}})
                    .as_object()
                    .cloned()
                    .expect("an object"),
            ),
            expected: json!({"jsonrpc":"2.0","id":1,"result":{}})
                .as_object()
                .cloned()
                .expect("an object"),
            request_index: 1,
        },
        CapturedTurn {
            request: json!({"jsonrpc":"2.0","id":2,"method":"y"})
                .as_object()
                .cloned()
                .expect("an object"),
            captured: Some(
                json!({"jsonrpc":"2.0","id":2,"result":{"mutated":true}})
                    .as_object()
                    .cloned()
                    .expect("an object"),
            ),
            expected: json!({"jsonrpc":"2.0","id":2,"result":{}})
                .as_object()
                .cloned()
                .expect("an object"),
            request_index: 3,
        },
    ];
    let captured = vec![
        turns.first().and_then(|t| t.captured.clone()),
        turns.get(1).and_then(|t| t.captured.clone()),
    ];
    let mut out: Vec<u8> = Vec::new();
    let report = drive_session(&mut turns, captured, &mut out)?;
    assert_eq!(report.sent, 2);
    assert_eq!(report.matched, 1);
    assert_eq!(report.diffed, 1);
    let output = std::str::from_utf8(&out).expect("output is utf-8");
    let lines: Vec<LineDiff> = output
        .lines()
        .map(|line| serde_json::from_str(line).expect("well-formed"))
        .collect();
    assert_eq!(lines.len(), 2);
    let first = lines.first().expect("at least one diff");
    assert_eq!(first.outcome, LineDiffOutcome::Same);
    let second = lines.get(1).expect("the second line");
    assert_eq!(second.outcome, LineDiffOutcome::Changed);
    Ok(())
}

/// `ClientReport::is_clean` is true when every sent turn
/// was matched and no turn diffed. The binary's exit-zero
/// decision reads from this flag.
#[test]
fn client_report_is_clean_only_when_every_turn_matched() {
    let mut report = ClientReport::default();
    assert!(report.is_clean(), "an empty report is vacuously clean");
    report.matched = 1;
    report.sent = 1;
    assert!(report.is_clean());
    report.diffed = 1;
    assert!(!report.is_clean(), "a diffed turn breaks clean");
}
