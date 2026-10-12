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
use protocol_droid_cli::replay::{self, FrameVerdict, ReplayArgs, VerdictOutcome};

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
    let dir = std::env::temp_dir();
    let path = dir.join(format!("protocol-droid-replay-{name}.jsonl"));
    let mut file = std::fs::File::create(&path).expect("create session file");
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
        target: "2024-11-05".to_owned(),
        explain: true,
        allow_downgrade: true,
    };
    assert_eq!(args.target, "2024-11-05");
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

