//! `protocol-droid replay` — re-emit a captured session against a target revision.
//!
//! ## The shape of the command
//!
//! A `replay` reads a JSONL session file (one frame per line) and writes one
//! per-frame verdict per line. The verdict is the same
//! `forwarded` / `downgraded` / `refused` shape the `bridge --check-result`
//! probe returns for a single reply, applied independently to every frame in
//! the file. That is the whole product in one command: a developer hands
//! `replay` a session, names a target revision, and gets a per-frame report
//! of what would change.
//!
//! ## Why the existing translator is the right thing to call
//!
//! The `Translator` already answers "what would the bridge do to this reply?"
//! for one envelope. A session is many envelopes, and the per-frame report is
//! the same question asked in a loop. Re-implementing the decision here would
//! be a second copy of the rules that can disagree with the bridge; the
//! `check` module pins that this is not the case for a single reply, and
//! `replay` extends the same pin to a session.
//!
//! ## Two sinks, one serialiser
//!
//! The per-frame records are the JSON, on stdout. The human summary
//! ("12 frames: 10 forwarded, 2 downgraded, 0 refused, 1 skipped") is on
//! stderr. There is no `--json` flag because the output is always JSONL
//! — replay is multi-record by construction, and a flag that toggled the
//! per-frame format would be a flag with no second value.
//!
//! ## Skipping bad lines, not failing on them
//!
//! A session file with a malformed line is reported on stderr (with the
//! line number), the line is skipped, and replay continues. The skipped
//! count is part of the summary. The alternative — aborting on the first
//! bad line — turns a 10k-line session into a 200-byte debug trip, which
//! is the wrong direction. The session file is the user's; the user's
//! session file being broken is theirs to fix, and a `replay` that
//! refused to look at the rest is a `replay` they cannot use to find
//! the broken lines.
//!
//! ## `record` is out of scope
//!
//! This command reads sessions. It does not write them. The matching
//! `record` is a separate piece of work — see `T20-cli-record-replay.md` —
//! and is deferred per the strategy document's Phase 1 scope. A user
//! without `record` can hand-write a session file: the format is
//! `{version, frame}` per line, and a few lines of `jq` plus a recorded
//! transcript is enough to drive `replay` for the first demo.

use std::io::{BufRead, Write};

use anyhow::Context as _;
use clap::Args;
use protocol_droid::adapters::secondary::codec::result::WireToolResults;
use protocol_droid::application::bridge::Translator;
use protocol_droid::domain::ir::lower::LowerError;
use protocol_droid::domain::{
    CapabilitySet, DegradePolicy, Leg, Negotiation, ProtocolVersion, TransportKind, resolve,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::check;

/// What the operator asked `replay` to do.
#[derive(Debug, Args)]
pub struct ReplayArgs {
    /// The session file, JSONL of `{version, frame}` records.
    ///
    /// Use `-` to read from stdin. Empty lines are skipped, malformed
    /// lines are reported on stderr and skipped with a non-fatal
    /// summary, and a missing file is the only fatal read error.
    #[arg(value_name = "SESSION")]
    pub session: String,

    /// The revision to replay the session against.
    ///
    /// Every frame's verdict is computed as "what would the bridge do
    /// forwarding this frame's `result` to a client that negotiated
    /// this revision?". The frame's own recorded version is its
    /// `source`; this is the `target`.
    #[arg(long, value_name = "VERSION")]
    pub target: String,

    /// Also write a human-readable per-frame summary to stderr.
    ///
    /// The JSONL output on stdout is always one record per frame; this
    /// flag adds a second stream beside the totals, so a developer
    /// running `replay` interactively sees what is happening without
    /// having to pipe stdout through `jq`.
    #[arg(long)]
    pub explain: bool,

    /// Allow downgrades; the default refuses them and exits non-zero
    /// on the first `Refused` frame.
    ///
    /// Mirrors `--allow-downgrade` on the bridge probe, and exists for
    /// the same reason: a session that the bridge *could* translate
    /// would otherwise look broken to `replay`.
    #[arg(long)]
    pub allow_downgrade: bool,
}

/// One line of the input session file.
#[derive(Debug, Deserialize)]
struct RecordedFrame {
    /// The revision the frame was captured at.
    ///
    /// Parsed once, on the way in. An unknown revision is the
    /// caller's data being wrong, and the wrong line is reported
    /// and skipped, not fatal.
    version: String,
    /// The JSON-RPC envelope. Any shape (request, response, notification).
    frame: Map<String, Value>,
}

/// One line of the output JSONL stream.
#[derive(Debug, Serialize, Deserialize)]
pub struct FrameVerdict {
    /// The 1-indexed line in the input file.
    pub index: usize,
    /// The version the frame was recorded at.
    pub source_version: String,
    /// The version `replay` was asked to forward to.
    pub target_version: String,
    /// The forward/downgrade/refuse verdict.
    pub outcome: VerdictOutcome,
    /// Whether the frame's bytes came back identical to the input.
    pub unchanged: bool,
    /// Every downgrade rule the translator applied, named.
    pub steps: Vec<String>,
    /// The frame the bridge would forward.
    pub reply: Map<String, Value>,
    /// Why, in a sentence.
    pub note: String,
    /// Present when the line could not be replayed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The three verdicts `replay` can return per frame.
///
/// Mirrors [`check::Outcome`] so the bridge probe and the replay
/// output are the same shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerdictOutcome {
    /// The client can read it; the frame is handed on untouched.
    Forwarded,
    /// Part of it could not be expressed at the target revision,
    /// and a defined fallback was applied.
    Downgraded,
    /// The client cannot read it and no fallback is defined.
    Refused,
}

impl From<check::Outcome> for VerdictOutcome {
    fn from(outcome: check::Outcome) -> Self {
        match outcome {
            check::Outcome::Forwarded => Self::Forwarded,
            check::Outcome::Downgraded => Self::Downgraded,
            check::Outcome::Refused => Self::Refused,
        }
    }
}

impl FrameVerdict {
    /// The verdict a successful run reports.
    fn from_translation(
        index: usize,
        source: ProtocolVersion,
        target: ProtocolVersion,
        input: &Map<String, Value>,
        translated_reply: Map<String, Value>,
        steps: Vec<String>,
    ) -> Self {
        let unchanged = &translated_reply == input;
        let (outcome, note) = if steps.is_empty() {
            (
                VerdictOutcome::Forwarded,
                "the target can read this as the source wrote it",
            )
        } else {
            (
                VerdictOutcome::Downgraded,
                "the target cannot read part of this; each rule below was applied",
            )
        };
        Self {
            index,
            source_version: source.to_string(),
            target_version: target.to_string(),
            outcome,
            unchanged,
            steps,
            reply: translated_reply,
            note: note.to_string(),
            error: None,
        }
    }

    /// A frame that came back as a `LowerError` rather than a verdict.
    fn from_error(
        index: usize,
        source: ProtocolVersion,
        target: ProtocolVersion,
        input: Map<String, Value>,
        error: &LowerError,
    ) -> Self {
        Self {
            index,
            source_version: source.to_string(),
            target_version: target.to_string(),
            outcome: VerdictOutcome::Refused,
            unchanged: false,
            steps: Vec::new(),
            reply: input,
            note: format!("the target cannot read this and no fallback is defined: {error}"),
            error: Some(error.to_string()),
        }
    }

    /// A frame that could not even be parsed.
    fn malformed(index: usize, message: String) -> Self {
        Self {
            index,
            source_version: String::new(),
            target_version: String::new(),
            outcome: VerdictOutcome::Refused,
            unchanged: false,
            steps: Vec::new(),
            reply: Map::new(),
            note: message.clone(),
            error: Some(message),
        }
    }
}

/// Runs `replay` to completion, writing the JSONL verdicts to `out`
/// and the human summary to `summary`.
///
/// Split out from the clap-driven `run` so the same logic can be
/// driven from a test (or a future subcommand) without going through
/// the filesystem and stdio plumbing.
pub fn run_with_io<R, W, S>(
    session: R,
    target: ProtocolVersion,
    policy: DegradePolicy,
    explain: bool,
    out: &mut W,
    summary: &mut S,
) -> anyhow::Result<RunSummary>
where
    R: BufRead,
    W: Write,
    S: Write,
{
    let leg = leg_for(target);
    let translator = Translator::new(WireToolResults);
    let mut counter = RunSummary::default();

    for (index_0, line) in session.lines().enumerate() {
        let index = index_0 + 1;
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                writeln!(summary, "line {index}: could not be read: {error}")?;
                counter.skipped += 1;
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let verdict = match replay_one(&line, index, target, &leg, policy, &translator) {
            Ok(Some(verdict)) => verdict,
            Ok(None) => {
                counter.skipped += 1;
                continue;
            }
            Err(error) => {
                writeln!(summary, "line {index}: {error}")?;
                let verdict = FrameVerdict::malformed(index, error.to_string());
                serde_json::to_writer(&mut *out, &verdict)
                    .context("writing a malformed-line verdict")?;
                out.write_all(b"\n").context("writing the verdict newline")?;
                counter.skipped += 1;
                continue;
            }
        };
        if explain {
            writeln!(summary, "frame {index}: {}", explain_verdict(&verdict))?;
        }
        serde_json::to_writer(&mut *out, &verdict)
            .context("writing a per-frame verdict")?;
        out.write_all(b"\n").context("writing the verdict newline")?;
        counter.record(&verdict);
    }

    counter.write_to(summary)?;
    Ok(counter)
}

/// One frame's verdict, or `None` when the line was empty.
fn replay_one(
    line: &str,
    index: usize,
    target: ProtocolVersion,
    leg: &Leg,
    policy: DegradePolicy,
    translator: &Translator<WireToolResults>,
) -> anyhow::Result<Option<FrameVerdict>> {
    let recorded: RecordedFrame = match serde_json::from_str(line) {
        Ok(recorded) => recorded,
        Err(error) => {
            return Err(anyhow::anyhow!("not a `{{version, frame}}` object: {error}"));
        }
    };
    let source: ProtocolVersion = recorded
        .version
        .parse()
        .with_context(|| format!("the recorded version is not a known revision: {}", recorded.version))?;
    // The bridge treats an unvendored version as "pass through", because
    // a client on the other side might still read it. `replay` answers
    // the user's question "what would happen?", and a build that cannot
    // lift the source frame cannot answer that question with a
    // `forwarded` verdict — the user needs to know. Mark the frame
    // as `Refused` with the recorded version named.
    if matches!(source, ProtocolVersion::Unknown(_)) {
        let note = format!(
            "the recorded version `{}` is not a revision this build can read; \
             record the session at a known revision or upgrade the build",
            recorded.version
        );
        return Ok(Some(FrameVerdict {
            index,
            source_version: source.to_string(),
            target_version: target.to_string(),
            outcome: VerdictOutcome::Refused,
            unchanged: false,
            steps: Vec::new(),
            reply: recorded.frame,
            note: note.clone(),
            error: Some(note),
        }));
    }
    let verdict = translator.translate(recorded.frame.clone(), source, leg, policy);
    let verdict = match verdict {
        Ok(translated) => {
            let steps = translated
                .report
                .steps()
                .iter()
                .map(|step| format!("{:?} at {}", step.rule, step.target))
                .collect::<Vec<_>>();
            FrameVerdict::from_translation(
                index,
                source,
                target,
                &recorded.frame,
                translated.reply,
                steps,
            )
        }
        Err(error) => FrameVerdict::from_error(index, source, target, recorded.frame, &error),
    };
    Ok(Some(verdict))
}

/// The human one-line summary of a verdict, used by `--explain`.
fn explain_verdict(verdict: &FrameVerdict) -> String {
    format!(
        "{:?} ({} -> {}){}",
        verdict.outcome,
        verdict.source_version,
        verdict.target_version,
        if verdict.steps.is_empty() {
            String::new()
        } else {
            format!(" [{}]", verdict.steps.join(", "))
        }
    )
}

/// The `Leg` a client at `version` ends up with when nothing is withheld.
///
/// Same definition the `bridge --check-result` probe uses, so the
/// two commands agree on what the client can read.
fn leg_for(version: ProtocolVersion) -> Leg {
    let caps = CapabilitySet::all_for(version);
    Leg::new(
        version,
        resolve(Negotiation {
            version,
            client_caps: caps,
            server_caps: caps,
            transport: TransportKind::Stdio,
        }),
    )
}

/// The totals `replay` reports on stderr.
#[derive(Debug, Default)]
pub struct RunSummary {
    forwarded: usize,
    downgraded: usize,
    refused: usize,
    skipped: usize,
}

impl RunSummary {
    const fn record(&mut self, verdict: &FrameVerdict) {
        match verdict.outcome {
            VerdictOutcome::Forwarded => self.forwarded += 1,
            VerdictOutcome::Downgraded => self.downgraded += 1,
            VerdictOutcome::Refused => self.refused += 1,
        }
    }

    fn write_to(&self, sink: &mut impl Write) -> std::io::Result<()> {
        writeln!(
            sink,
            "{} frames replayed: {} forwarded, {} downgraded, {} refused, {} skipped",
            self.forwarded + self.downgraded + self.refused,
            self.forwarded,
            self.downgraded,
            self.refused,
            self.skipped,
        )
    }
}

/// The clap-driven entry point.
///
/// Reads the session file (or stdin for `-`), runs every frame
/// through the translator, writes JSONL verdicts to stdout and the
/// human summary to stderr. Exits non-zero when at least one frame
/// was `Refused` and downgrades were not allowed, mirroring the
/// bridge probe's contract.
pub fn run(args: &ReplayArgs) -> anyhow::Result<()> {
    let target: ProtocolVersion = args
        .target
        .parse()
        .with_context(|| format!("--target is not a known revision: {}", args.target))?;
    let policy = if args.allow_downgrade {
        DegradePolicy::permissive()
    } else {
        DegradePolicy::strict()
    };
    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    let summary = run_on_path(&args.session, target, policy, args.explain, &mut stdout, &mut stderr)?;
    drop((stdout, stderr));
    if summary.refused > 0 && !args.allow_downgrade {
        anyhow::bail!(
            "{} frame(s) refused; pass --allow-downgrade to accept them as errors instead of failures",
            summary.refused
        );
    }
    Ok(())
}

/// Reads the session from `path` (or stdin for `-`) and delegates.
fn run_on_path<W, S>(
    path: &str,
    target: ProtocolVersion,
    policy: DegradePolicy,
    explain: bool,
    stdout: &mut W,
    stderr: &mut S,
) -> anyhow::Result<RunSummary>
where
    W: Write,
    S: Write,
{
    if path == "-" {
        let stdin = std::io::stdin();
        let reader = stdin.lock();
        run_with_io(reader, target, policy, explain, stdout, stderr)
    } else {
        let file = std::fs::File::open(path)
            .with_context(|| format!("opening the session file: {path}"))?;
        let reader = std::io::BufReader::new(file);
        run_with_io(reader, target, policy, explain, stdout, stderr)
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "tests assert on fallible calls; .expect() is the standard test pattern"
)]
mod tests {
    use super::*;

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
            target: "2024-11-05".to_owned(),
            explain: true,
            allow_downgrade: true,
        };
        assert_eq!(args.target, "2024-11-05");
        assert!(args.explain);
        assert!(args.allow_downgrade);
    }

    /// `run` is the clap-driven entry point. It is exercised by
    /// the binary at runtime; the lib test target does not see
    /// the bin, so the lint flags the unused `pub`. Referenced
    /// here to keep it reachable from the lib's own tests.
    #[test]
    fn run_is_reachable() {
        let args = ReplayArgs {
            session: "session.jsonl".to_owned(),
            target: "2024-11-05".to_owned(),
            explain: false,
            allow_downgrade: true,
        };
        // The function opens a real file (or stdin for `-`);
        // this test is a lint-pin only, so the path is bogus
        // and the call is expected to fail.
        let _ = run(&args);
    }

    /// A simple text result is forwardable to every revision.
    #[test]
    fn text_result_is_forwarded_to_2024_11_05() -> anyhow::Result<()> {
        let line = r#"{"version":"2024-11-05","frame":{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"hi"}]}}}"#;
        let verdict = replay_one(line, 1, target(), &leg_for(target()), policy(), &Translator::new(WireToolResults))?
            .expect("a well-formed line produces a verdict");
        assert_eq!(verdict.outcome, VerdictOutcome::Forwarded);
        assert!(verdict.unchanged);
        Ok(())
    }

    /// A 2025-06-18 `structuredContent` cannot reach 2024-11-05.
    #[test]
    fn structured_content_is_downgraded_when_replayed_to_2024_11_05() -> anyhow::Result<()> {
        let line = r#"{"version":"2025-06-18","frame":{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"hi"}],"structuredContent":{"key":"value"}}}}"#;
        let verdict = replay_one(line, 1, target(), &leg_for(target()), policy(), &Translator::new(WireToolResults))?
            .expect("a well-formed line produces a verdict");
        assert_eq!(verdict.outcome, VerdictOutcome::Downgraded);
        assert!(!verdict.unchanged);
        assert!(
            verdict.steps.iter().any(|step| step.contains("StructuredContent")),
            "the downgrade step should name `StructuredContent`; got {:?}",
            verdict.steps,
        );
        Ok(())
    }

    /// An error envelope is forwarded (a refusal is the upstream's own
    /// words about a request, not a result this layer has any opinion on).
    #[test]
    fn error_envelope_is_forwarded() -> anyhow::Result<()> {
        let line = r#"{"version":"2024-11-05","frame":{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}}"#;
        let verdict = replay_one(line, 1, target(), &leg_for(target()), policy(), &Translator::new(WireToolResults))?
            .expect("a well-formed line produces a verdict");
        assert_eq!(verdict.outcome, VerdictOutcome::Forwarded);
        assert!(verdict.unchanged);
        Ok(())
    }

    /// A request envelope (no `result`, no `error`) is forwarded.
    #[test]
    fn request_envelope_is_forwarded() -> anyhow::Result<()> {
        let line = r#"{"version":"2024-11-05","frame":{"jsonrpc":"2.0","id":1,"method":"ping"}}"#;
        let verdict = replay_one(line, 1, target(), &leg_for(target()), policy(), &Translator::new(WireToolResults))?
            .expect("a well-formed line produces a verdict");
        assert_eq!(verdict.outcome, VerdictOutcome::Forwarded);
        assert!(verdict.unchanged);
        Ok(())
    }

    /// A line that is not `{version, frame}` is reported and skipped.
    #[test]
    fn malformed_line_returns_err_so_caller_can_skip() {
        let line = r#"{"not":"a frame"}"#;
        let result = replay_one(line, 1, target(), &leg_for(target()), policy(), &Translator::new(WireToolResults));
        assert!(result.is_err(), "a malformed line should be an error, got Ok");
    }

    /// An unknown recorded version is reported as `Refused` (the
    /// build cannot lift the source frame, so the verdict is
    /// unknowable). The bridge would forward the bytes untouched
    /// at runtime, but `replay` answers the user's question
    /// "what would happen?", and a `Forwarded` verdict would be
    /// a lie the build cannot back up.
    #[test]
    fn unknown_recorded_version_is_refused_not_silently_forwarded() -> anyhow::Result<()> {
        let line = r#"{"version":"2099-01-01","frame":{"jsonrpc":"2.0","id":1,"result":{}}}"#;
        let verdict = replay_one(line, 1, target(), &leg_for(target()), policy(), &Translator::new(WireToolResults))?
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
{\"version\":\"2024-11-05\",\"frame\":{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}}
{\"version\":\"2025-06-18\",\"frame\":{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}],\"structuredContent\":{\"k\":\"v\"}}}}
{\"version\":\"2024-11-05\",\"frame\":{\"jsonrpc\":\"2.0\",\"id\":3,\"error\":{\"code\":-32601,\"message\":\"Method not found\"}}}
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
{\"version\":\"2024-11-05\",\"frame\":{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}}
this is not json
{\"version\":\"2024-11-05\",\"frame\":{\"jsonrpc\":\"2.0\",\"id\":2,\"error\":{\"code\":-32601,\"message\":\"Method not found\"}}}
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
        assert!(summary.contains("line 2:"), "the bad line is named on stderr; got {summary:?}");
        Ok(())
    }
}
