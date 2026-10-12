//! Wire tests for the `replay` subcommand.
//!
//! The unit tests in `src/replay.rs` pin the per-frame logic; this
//! file exercises the public surface (`replay::run_with_io`)
//! end-to-end against a hand-written session file, so the `pub`
//! items are seen as used by the lib's dead-code lint and the
//! CLI contract is checked the way a real invocation would check
//! it.
//!
//! Lints: tests assert on fallible calls, so `.expect()` and
//! `panic!` are unavoidable. Suppress at the file level.

#![expect(
    clippy::expect_used,
    reason = "tests assert on fallible calls; .expect() is the standard test pattern"
)]
#![expect(
    clippy::panic,
    reason = "tests panic on a failure to keep the assertion message close to the call"
)]
#![expect(
    clippy::indexing_slicing,
    reason = "tests index into a fixed array of test inputs"
)]

use std::io::{BufRead as _, BufReader, Write as _};

use protocol_droid::domain::DegradePolicy;
use protocol_droid_cli::replay::{self, FrameVerdict, LineDiff, LineDiffOutcome, ReplayArgs, VerdictOutcome};

/// Resolves the path to the `protocol-droid` binary.
fn protocol_droid_bin() -> &'static std::path::Path {
    std::path::Path::new(env!("CARGO_BIN_EXE_protocol-droid"))
}

/// Reads a session from `path` and returns the JSONL verdicts as
/// `FrameVerdict` plus the human summary.
fn replay_to_memory(session_path: &str, target: &str) -> (Vec<FrameVerdict>, String) {
    let file = std::fs::File::open(session_path)
        .unwrap_or_else(|error| panic!("opening {session_path}: {error}"));
    let mut out: Vec<u8> = Vec::new();
    let mut summary: Vec<u8> = Vec::new();
    replay::run_with_io(
        BufReader::new(file),
        target.parse().expect("a known target revision"),
        DegradePolicy::permissive(),
        false,
        &mut out,
        &mut summary,
    )
    .expect("replay completes without error");
    let verdicts: Vec<FrameVerdict> = out
        .lines()
        .map(|line| line.unwrap_or_else(|error| panic!("reading a verdict line: {error}")))
        .map(|line| serde_json::from_str(&line).expect("each line is a FrameVerdict"))
        .collect();
    let summary = String::from_utf8(summary).expect("summary is utf-8");
    (verdicts, summary)
}

/// Writes a session file in a tempdir and returns the path.
fn write_session(name: &str, body: &str) -> String {
    // The path includes the process id and an atomic counter so two
    // tests running in parallel with the same `name` get distinct
    // files. A path that two tests share would let one test's
    // `File::create` truncate the other test's bytes while it is
    // still being written, and the reader on the other side would
    // see a half-empty file. The counter is per-process because
    // parallel test threads share an address space.
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir();
    let path = dir.join(format!(
        "protocol-droid-replay-{}-{}-{}.jsonl",
        name,
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst),
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .expect("create session file");
    file.write_all(body.as_bytes())
        .expect("write session file");
    path.to_string_lossy().into_owned()
}

/// A 2024-11-05 session with a plain text result, replayed
/// against the same revision, is forwarded. The bytes come back
/// unchanged.
#[test]
fn forwarded_text_result_against_same_revision() {
    let session = write_session(
        "forwarded",
        r#"{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}"}"#,
    );
    let (verdicts, summary) = replay_to_memory(&session, "2024-11-05");
    assert_eq!(verdicts.len(), 1);
    assert_eq!(verdicts[0].outcome, VerdictOutcome::Forwarded);
    assert!(verdicts[0].unchanged);
    assert!(verdicts[0].steps.is_empty());
    assert!(summary.contains("1 frames replayed"));
}

/// A 2025-06-18 `structuredContent` cannot reach 2024-11-05
/// without a downgrade. The verdict is `Downgraded`, the bytes
/// are not unchanged, and the step is named.
#[test]
fn structured_content_is_downgraded_to_2024_11_05() {
    let session = write_session(
        "downgraded",
        r#"{"version":"2025-06-18","frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}],\"structuredContent\":{\"key\":\"value\"}}}"}"#,
    );
    let (verdicts, summary) = replay_to_memory(&session, "2024-11-05");
    assert_eq!(verdicts.len(), 1);
    assert_eq!(verdicts[0].outcome, VerdictOutcome::Downgraded);
    assert!(!verdicts[0].unchanged);
    assert!(
        verdicts[0].steps.iter().any(|s| s.contains("StructuredContent")),
        "expected a `StructuredContent` step, got {:?}",
        verdicts[0].steps,
    );
    assert!(summary.contains("1 downgraded"));
}

/// A 2024-11-05 result, replayed against 2026-07-28 (a
/// forward-compatible target) is forwarded unchanged.
#[test]
fn legacy_result_replayed_against_modern_target_is_forwarded() {
    let session = write_session(
        "forwarded-against-modern",
        r#"{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}"}"#,
    );
    let (verdicts, _summary) = replay_to_memory(&session, "2026-07-28");
    assert_eq!(verdicts.len(), 1);
    assert_eq!(verdicts[0].outcome, VerdictOutcome::Forwarded);
    assert!(verdicts[0].unchanged);
}

/// A multi-frame session reports per-frame verdicts and a
/// summary that names the totals.
#[test]
fn multi_frame_session_totals_match() {
    let session = write_session(
        "multi",
        "\
{\"version\":\"2024-11-05\",\"frame\":\"{\\\"jsonrpc\\\":\\\"2.0\\\",\\\"id\\\":1,\\\"result\\\":{\\\"content\\\":[{\\\"type\\\":\\\"text\\\",\\\"text\\\":\\\"hi\\\"}]}}\"}
{\"version\":\"2025-06-18\",\"frame\":\"{\\\"jsonrpc\\\":\\\"2.0\\\",\\\"id\\\":2,\\\"result\\\":{\\\"content\\\":[{\\\"type\\\":\\\"text\\\",\\\"text\\\":\\\"hi\\\"}],\\\"structuredContent\\\":{\\\"k\\\":\\\"v\\\"}}}\"}
{\"version\":\"2024-11-05\",\"frame\":\"{\\\"jsonrpc\\\":\\\"2.0\\\",\\\"id\\\":3,\\\"error\\\":{\\\"code\\\":-32601,\\\"message\\\":\\\"Method not found\\\"}}\"}
",
    );
    let (verdicts, summary) = replay_to_memory(&session, "2024-11-05");
    assert_eq!(verdicts.len(), 3);
    assert_eq!(verdicts[0].outcome, VerdictOutcome::Forwarded);
    assert_eq!(verdicts[1].outcome, VerdictOutcome::Downgraded);
    assert_eq!(verdicts[2].outcome, VerdictOutcome::Forwarded);
    assert!(summary.contains("3 frames replayed"));
    assert!(summary.contains("2 forwarded"));
    assert!(summary.contains("1 downgraded"));
}

/// A session that contains a malformed line still produces a
/// verdict for every input line, and the malformed one is
/// reported as refused with the original line number.
#[test]
fn malformed_line_is_reported_and_replay_continues() {
    let session = write_session(
        "malformed",
        "\
{\"version\":\"2024-11-05\",\"frame\":\"{\\\"jsonrpc\\\":\\\"2.0\\\",\\\"id\\\":1,\\\"result\\\":{\\\"content\\\":[{\\\"type\\\":\\\"text\\\",\\\"text\\\":\\\"hi\\\"}]}}\"}
this is not json
{\"version\":\"2024-11-05\",\"frame\":\"{\\\"jsonrpc\\\":\\\"2.0\\\",\\\"id\\\":2,\\\"error\\\":{\\\"code\\\":-32601,\\\"message\\\":\\\"x\\\"}}\"}
",
    );
    let (verdicts, summary) = replay_to_memory(&session, "2024-11-05");
    assert_eq!(verdicts.len(), 3, "every input line gets a verdict, even malformed ones");
    assert_eq!(verdicts[1].outcome, VerdictOutcome::Refused);
    assert_eq!(verdicts[1].index, 2);
    assert!(verdicts[1].error.is_some());
    assert!(summary.contains("line 2:"));
    assert!(summary.contains("1 skipped"));
}

/// A session that contains a frame with an unknown recorded
/// version produces a `Refused` verdict (the build cannot lift
/// the source frame, so the answer to "what would happen?" is
/// unknowable) and continues with the rest of the session.
#[test]
fn unknown_recorded_version_is_reported_and_replay_continues() {
    let session = write_session(
        "unknown-version",
        "\
{\"version\":\"2099-01-01\",\"frame\":\"{\\\"jsonrpc\\\":\\\"2.0\\\",\\\"id\\\":1,\\\"result\\\":{}}\"}
{\"version\":\"2024-11-05\",\"frame\":\"{\\\"jsonrpc\\\":\\\"2.0\\\",\\\"id\\\":2,\\\"result\\\":{\\\"content\\\":[{\\\"type\\\":\\\"text\\\",\\\"text\\\":\\\"hi\\\"}]}}\"}
",
    );
    let (verdicts, summary) = replay_to_memory(&session, "2024-11-05");
    assert_eq!(verdicts.len(), 2);
    assert_eq!(verdicts[0].outcome, VerdictOutcome::Refused);
    assert!(verdicts[0].error.is_some());
    assert!(verdicts[0].note.contains("not a revision this build can read"));
    assert_eq!(verdicts[1].outcome, VerdictOutcome::Forwarded);
    assert!(summary.contains("1 refused"));
    assert!(summary.contains("1 forwarded"));
}

/// The clap-derived `ReplayArgs` carries the four fields the
/// subcommand takes. Pinned here so a `clap` upgrade that
/// silently changes the contract shows up as a compile error in
/// the test, not a runtime surprise.
#[test]
fn clap_args_are_well_formed() {
    let args = ReplayArgs {
        session: "session.jsonl".to_owned(),
        target: Some("2024-11-05".to_owned()),
        explain: true,
        allow_downgrade: true,
        diff: None,
        as_server: false,
    };
    assert_eq!(args.target.as_deref(), Some("2024-11-05"));
    assert!(args.explain);
    assert!(args.allow_downgrade);
}

/// The binary's `replay` subcommand, invoked as a subprocess,
/// produces the same per-frame JSONL the in-process tests do.
/// Pinned here so a clap-derive change that alters the wire
/// surface (e.g. field rename) shows up as a test failure, not
/// a silently-broken CLI.
#[test]
fn binary_replay_subcommand_emits_jsonl_verdicts() {
    let session = write_session(
        "binary-roundtrip",
        r#"{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}"}"#,
    );
    let output = std::process::Command::new(protocol_droid_bin())
        .args(["replay", &session, "--target", "2026-07-28"])
        .output()
        .expect("spawn the protocol-droid binary");
    assert!(
        output.status.success(),
        "binary exited non-zero ({})\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let verdicts: Vec<FrameVerdict> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("each line is a FrameVerdict"))
        .collect();
    assert_eq!(verdicts.len(), 1);
    assert_eq!(verdicts[0].outcome, VerdictOutcome::Forwarded);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("1 frames replayed"),
        "human summary on stderr; got {stderr:?}"
    );
}

/// The binary, invoked with a session that contains a frame
/// that needs a downgrade, exits non-zero (without
/// `--allow-downgrade`) and reports the refusal on stderr.
#[test]
fn binary_replay_exits_non_zero_on_refused_frame() {
    let session = write_session(
        "binary-refused",
        r#"{"version":"2025-06-18","frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}],\"structuredContent\":{\"k\":\"v\"}}}"}"#,
    );
    let output = std::process::Command::new(protocol_droid_bin())
        .args(["replay", &session, "--target", "2024-11-05"])
        .output()
        .expect("spawn the protocol-droid binary");
    assert!(
        !output.status.success(),
        "binary should exit non-zero on a Refused frame; got {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("frame(s) refused"));
}

// -- Session-vs-session diff integration tests ---------------------------

/// Writes two session files and runs `replay --diff` against them,
/// returning the parsed `LineDiff` list and the human summary.
fn diff_two_sessions(left: &str, right: &str) -> (Vec<LineDiff>, String) {
    let left_path = write_session("diff-left", left);
    let right_path = write_session("diff-right", right);
    let left_file = std::fs::File::open(&left_path)
        .unwrap_or_else(|error| panic!("opening left: {error}"));
    let right_file = std::fs::File::open(&right_path)
        .unwrap_or_else(|error| panic!("opening right: {error}"));
    let mut out: Vec<u8> = Vec::new();
    let mut summary: Vec<u8> = Vec::new();
    replay::diff_with_io(
        BufReader::new(left_file),
        BufReader::new(right_file),
        &mut out,
        &mut summary,
    )
    .expect("diff runs end-to-end on a well-formed pair");
    let output = String::from_utf8(out).expect("output is utf-8");
    let lines: Vec<LineDiff> = output
        .lines()
        .map(|line| serde_json::from_str(line).expect("the diff is well-formed JSONL"))
        .collect();
    let summary_text = String::from_utf8(summary).expect("summary is utf-8");
    (lines, summary_text)
}

/// `replay --diff` against two identical sessions produces one
/// `same` line per input line and exits cleanly. The T20 spec's
/// "replay against the same server: zero diffs" property.
#[test]
fn diff_with_io_round_trips_through_files() {
    let session = r#"{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}"}
{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}"}
"#;
    let (lines, summary) = diff_two_sessions(session, session);
    assert_eq!(lines.len(), 2);
    let first = lines.first().expect("at least one diff record");
    let second = lines.get(1).expect("the second line is present");
    assert_eq!(first.outcome, LineDiffOutcome::Same);
    assert_eq!(second.outcome, LineDiffOutcome::Same);
    assert!(summary.contains("2 lines diffed"));
    assert!(summary.contains("2 same"));
}

/// `replay --diff` against a mutated session produces a `changed`
/// line at the index of the mutation, `same` everywhere else.
/// The T20 spec's "replay against a mutated server: a precise diff".
#[test]
fn diff_with_io_pinpoints_a_single_line_change() {
    let left = r#"{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}"}
{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}"}
"#;
    let right = r#"{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}"}
{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"mutated\":true}}"}
"#;
    let (lines, _summary) = diff_two_sessions(left, right);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].outcome, LineDiffOutcome::Same);
    assert_eq!(lines[1].outcome, LineDiffOutcome::Changed);
}

/// The binary's `replay --diff` subcommand, invoked as a subprocess,
/// produces per-line JSONL the same way the in-process tests do.
/// Pinned here so a clap-derive change that alters the wire surface
/// shows up as a test failure, not a silently-broken CLI.
#[test]
fn binary_replay_diff_subcommand_emits_per_line_jsonl() {
    let session = r#"{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}"}
"#;
    let left_path = write_session("bin-diff-left", session);
    let right_path = write_session("bin-diff-right", session);
    let output = std::process::Command::new(protocol_droid_bin())
        .args(["replay", &left_path, "--diff", &right_path])
        .output()
        .expect("spawn the protocol-droid binary");
    assert!(
        output.status.success(),
        "binary should exit zero on a clean diff; got {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<LineDiff> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("the diff is well-formed JSONL"))
        .collect();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].outcome, LineDiffOutcome::Same);
}

/// The binary's `replay --diff` exits non-zero when the two
/// sessions differ, mirroring the per-frame refusal contract.
#[test]
fn binary_replay_diff_exits_non_zero_on_mutation() {
    let left = r#"{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}"}
"#;
    let right = r#"{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"mutated\":true}}"}
"#;
    let left_path = write_session("bin-diff-mut-left", left);
    let right_path = write_session("bin-diff-mut-right", right);
    let output = std::process::Command::new(protocol_droid_bin())
        .args(["replay", &left_path, "--diff", &right_path])
        .output()
        .expect("spawn the protocol-droid binary");
    assert!(
        !output.status.success(),
        "binary should exit non-zero on a mutated diff; got {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("the two sessions differ"));
}

// -- --as-server integration tests ----------------------------------------

/// The binary's `replay --as-server`, driven as a subprocess with
/// a small transcript on stdin, answers the requests with the
/// recorded replies by id. Pinned here so a clap-derive change
/// that alters the wire surface (e.g. flag rename) shows up as
/// a test failure, not a silently-broken CLI.
#[test]
fn binary_replay_as_server_answers_recorded_replies() {
    let session = r#"{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"name\":\"mock\"}}"}
{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}"}
"#;
    let session_path = write_session("bin-as-server", session);
    let transcript = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n";
    let mut child = std::process::Command::new(protocol_droid_bin())
        .args(["replay", &session_path, "--as-server"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn the protocol-droid binary");
    child
        .stdin
        .as_mut()
        .expect("piped stdin")
        .write_all(transcript.as_bytes())
        .expect("write transcript");
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("wait on the binary");
    assert!(
        output.status.success(),
        "binary should exit zero on a clean mock; got {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"id\":1"), "the recorded reply for id 1 is served; got {stdout:?}");
    assert!(stdout.contains("\"id\":2"), "the recorded reply for id 2 is served; got {stdout:?}");
    assert!(stdout.contains("\"name\":\"mock\""), "the recorded result is byte-identical; got {stdout:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("2 answered"), "the summary reports the served count; got {stderr:?}");
}

/// A request whose id is not in the recording gets a typed
/// JSON-RPC `method not found` error back, not a hang.
#[test]
fn binary_replay_as_server_returns_method_not_found_for_unknown_id() {
    let session = r#"{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}"}
"#;
    let session_path = write_session("bin-as-server-404", session);
    let transcript = "{\"jsonrpc\":\"2.0\",\"id\":99,\"method\":\"unknown\"}\n";
    let mut child = std::process::Command::new(protocol_droid_bin())
        .args(["replay", &session_path, "--as-server"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn the protocol-droid binary");
    child
        .stdin
        .as_mut()
        .expect("piped stdin")
        .write_all(transcript.as_bytes())
        .expect("write transcript");
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("wait on the binary");
    // The mock answered at least one request (the not-found
    // refusal) and exits zero, mirroring the per-frame refusal
    // contract: typed refusal, not a hang.
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("-32601"), "the spec's method-not-found code; got {stdout:?}");
    assert!(stdout.contains("\"id\":99"), "the unknown id is echoed back; got {stdout:?}");
}

