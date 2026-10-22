//! The replay mode: re-emit a captured session against a target revision.
//!
//! One frame in, one verdict out. The verdict is the same
//! `forwarded` / `downgraded` / `refused` shape the `bridge --check-result`
//! probe returns for a single reply, applied independently to every frame in
//! the file.
//!
//! The decision itself is not made here. The `Translator` already answers
//! "what would the bridge do to this reply?" for one envelope, and a session
//! is many envelopes — so re-implementing the rules would be a second copy
//! that can disagree with the bridge. This module asks the translator and
//! reports what it said.

use super::client::run_as_client_async;
use super::diff::run_diff;
use super::serve::run_as_server;
use super::{RecordedFrame, ReplayArgs};
use std::io::{BufRead, Write};

use anyhow::Context as _;
use protocol_droid::adapters::secondary::codec::result::WireToolResults;
use protocol_droid::application::bridge::Translator;
use protocol_droid::domain::ir::lower::LowerError;
use protocol_droid::domain::{
    CapabilitySet, DegradePolicy, Leg, Negotiation, ProtocolVersion, TransportKind, resolve,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::check;

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
                out.write_all(b"\n")
                    .context("writing the verdict newline")?;
                counter.skipped += 1;
                continue;
            }
        };
        if explain {
            writeln!(summary, "frame {index}: {}", explain_verdict(&verdict))?;
        }
        serde_json::to_writer(&mut *out, &verdict).context("writing a per-frame verdict")?;
        out.write_all(b"\n")
            .context("writing the verdict newline")?;
        counter.record(&verdict);
    }

    counter.write_to(summary)?;
    Ok(counter)
}

/// One frame's verdict, or `None` when the line was empty.
pub(super) fn replay_one(
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
            return Err(anyhow::anyhow!(
                "not a `{{version, frame}}` object: {error}"
            ));
        }
    };
    let source: ProtocolVersion = recorded.version.parse().with_context(|| {
        format!(
            "the recorded version is not a known revision: {}",
            recorded.version
        )
    })?;
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
pub(super) fn leg_for(version: ProtocolVersion) -> Leg {
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
    pub(super) forwarded: usize,
    pub(super) downgraded: usize,
    pub(super) refused: usize,
    pub(super) skipped: usize,
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
///
/// When `--diff <other>` is set, runs the diff half instead: pairs
/// the two sessions by line index, writes per-line diffs to stdout
/// in JSONL, and a human summary to stderr. `--target` is ignored
/// in this mode; the diff is mechanical.
///
/// When `--as-server` is set, stands up a mock MCP server on
/// stdin/stdout that answers from the recorded `upstream→client`
/// frames, matched by the request's `id`.
///
/// When `--as-client` is set, drives a server process (passed as
/// positional `SERVER [args...]`) with the recorded
/// `client→upstream` lines, captures the responses, and diffs
/// them against the recorded `upstream→client` lines. The diff
/// is emitted in the same JSONL shape `--diff` writes.
pub async fn run(args: &ReplayArgs) -> anyhow::Result<()> {
    if let Some(right_path) = args.diff.as_deref() {
        return run_diff(&args.session, right_path);
    }
    if args.as_server {
        return run_as_server(&args.session);
    }
    if args.as_client {
        if args.server.is_empty() {
            anyhow::bail!(
                "--as-client requires a server command after the session, e.g. `replay --as-client session.jsonl -- python server.py`"
            );
        }
        return run_as_client_async(&args.session, &args.server).await;
    }
    let target: ProtocolVersion = args
        .target
        .as_deref()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "--target is required for the verdict mode; pass --diff or --as-server to skip it"
            )
        })?
        .parse()
        .with_context(|| {
            format!(
                "--target is not a known revision: {}",
                args.target.as_deref().unwrap_or("")
            )
        })?;
    let policy = if args.allow_downgrade {
        DegradePolicy::permissive()
    } else {
        DegradePolicy::strict()
    };
    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    let summary = run_on_path(
        &args.session,
        target,
        policy,
        args.explain,
        &mut stdout,
        &mut stderr,
    )?;
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

// ============================================================================
// Session-vs-session diff
//
// The diff half of dynamic replay: take two recorded sessions (the same
// capture tool wrote both), pair them by line index, and emit a per-line
// report. Same session = zero diffs (the property the T20 spec pins with
// "replay against the same server: zero diffs"). Mutated session = a
// precise diff at the line that changed.
//
// The pairing is by *line index*, not by frame id, because the same
// capture run against two different upstreams produces the same line
// ordering. A diff that aligned by id would be aligning across two
// unrelated request streams; an id that exists in only one of them
// would be reported as a deletion on the left and an insertion on the
// right, and a per-line pairing is what the operator running `replay
// --diff` actually wants to see.
// ============================================================================
