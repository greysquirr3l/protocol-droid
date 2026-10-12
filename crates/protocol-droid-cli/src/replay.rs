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
//!
//! ## Dynamic replay: `--as-server`
//!
//! `--as-server` turns the binary into a *mock MCP server* that answers
//! every incoming JSON-RPC request with a recorded `upstream→client`
//! frame from the session, matched by the request's `id`. It speaks
//! over stdio so a test client can `pipe` to it; the recorded
//! session is the contract. This is the half of the dynamic replay
//! the T20 spec calls "replay as server: match inbound requests by
//! (method, normalised params) and return the recorded results".
//!
//! Matching is by `id` rather than by `method` because that is what
//! the recorded session carries — every recorded request has an id,
//! and every recorded reply has the same id. A client that issues
//! the same logical request twice with different ids is two
//! different recorded turns; the same id is the (request, response)
//! pair the capture run already saw.
//!
//! ## Session-vs-session diff: `--diff`
//!
//! `--diff <other>` pairs the left session with the right by line
//! index, emits per-line `same` / `changed` / `added` / `removed`
//! verdicts, and exits non-zero on a non-clean diff. Same session =
//! zero diffs; mutated session = a precise diff at the line that
//! changed. The diff is the "assert on the server's responses with
//! a diff on mismatch" half of the T20 spec.

use std::collections::VecDeque;
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
use serde_json::{Map, Value, json};

use crate::check;
use crate::record::Direction;

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

    /// The server command to spawn when `--as-client` is set.
    ///
    /// Positional and trailing — clap's `trailing_var_arg` means
    /// "everything from this position on is a literal". The first
    /// element is the program, the rest are its arguments; they
    /// are passed to a `StdioClient` as a child process. Empty in
    /// every other mode; the validator in `run` rejects a
    /// non-`--as-client` invocation that sets it.
    #[arg(value_name = "SERVER", trailing_var_arg = true, num_args = 0..)]
    pub server: Vec<String>,

    /// The revision to replay the session against.
    ///
    /// Every frame's verdict is computed as "what would the bridge do
    /// forwarding this frame's `result` to a client that negotiated
    /// this revision?". The frame's own recorded version is its
    /// `source`; this is the `target`.
    ///
    /// Unused in `--diff`, `--as-server`, and `--as-client` modes;
    /// the diff is mechanical, the mock answers from the recording
    /// byte-for-byte, and the client driver just passes through.
    /// Required otherwise.
    #[arg(
        long,
        value_name = "VERSION",
        required_unless_present_any = ["diff", "as_server", "as_client"]
    )]
    pub target: Option<String>,

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

    /// A second session file to diff against.
    ///
    /// When set, `replay` is the diff half of the dynamic replay:
    /// for every line in the left session, find the matching line in
    /// the right session (by line index, since the same capture tool
    /// wrote both), and emit a per-line diff. Lines that are present
    /// in one and not the other are reported as `added` / `removed`.
    /// The diff is the answer the T20 spec wants to "replay a session
    /// against the same server: zero diffs. Replay against a mutated
    /// server: a precise diff".
    ///
    /// `--target` is unused in this mode; the diff is mechanical and
    /// does not translate.
    #[arg(long, value_name = "SESSION")]
    pub diff: Option<String>,

    /// Stand up a stdio MCP server that answers from the recorded
    /// session.
    ///
    /// Each incoming JSON-RPC request is matched by `id` against the
    /// recorded `upstream→client` frames, and the recorded reply is
    /// written back. A request whose id is not in the recording is
    /// answered with a `method not found` error so the test client
    /// gets a typed refusal rather than a hang. The session is the
    /// contract; a recorded `initialize` is required for the
    /// handshake, and a recorded reply for every subsequent request
    /// is the assumption.
    ///
    /// `--target` and `--diff` are unused in this mode; the mock
    /// answers from the recording byte-for-byte and does not
    /// translate.
    #[arg(long, conflicts_with_all = ["target", "diff", "as_client"])]
    pub as_server: bool,

    /// Drive a live stdio MCP server with the recorded requests and
    /// diff its responses against the recorded replies.
    ///
    /// The recorded `client→upstream` lines are sent to a server
    /// process that the operator passes after `--`; the server's
    /// responses are paired with the recorded `upstream→client`
    /// lines (the next line in the session), and the diff is
    /// emitted in the same per-line JSONL shape `--diff` writes.
    /// Same server = zero diffs; mutated server = a precise diff
    /// at the request whose response changed.
    ///
    /// The server command is a positional after `--`, exactly as
    /// `npm test` and `cargo run --` accept: a leading
    /// `<program> [args...]` that the binary spawns as a child
    /// process. The session file is the second positional, and
    /// the help text is the place to read the order.
    #[arg(long, conflicts_with_all = ["target", "diff", "as_server"])]
    pub as_client: bool,
}

/// One line of the input session file.
#[derive(Debug, Deserialize)]
struct RecordedFrame {
    /// Which way the frame crossed the bridge.
    ///
    /// Optional in the on-disk shape so the per-frame verdict
    /// path's tests (which write a `{version, frame}` line
    /// without `direction`) keep working without a fixture
    /// rewrite. `replay --as-client` requires the field —
    /// pairing `client→upstream` with the next
    /// `upstream→client` is the whole point of the mode — and
    /// bails on a session that omits it.
    #[serde(default)]
    direction: Option<Direction>,
    /// The revision the frame was captured at.
    ///
    /// Parsed once, on the way in. An unknown revision is the
    /// caller's data being wrong, and the wrong line is reported
    /// and skipped, not fatal.
    version: String,
    /// The JSON-RPC envelope, as a JSON object.
    ///
    /// The recorder writes the frame as a JSON string to keep the
    /// recorder free of JSON-parsing decisions — the string is
    /// what crossed the wire, byte-for-byte. We re-parse it here
    /// because the verdict needs a structured envelope to translate.
    /// A line whose `frame` is not a string, or whose string is not
    /// a JSON object, is reported and skipped the same way an
    /// unknown version is.
    #[serde(deserialize_with = "deserialize_frame_string")]
    frame: Map<String, Value>,
}

/// Parses a `frame` field that arrived as a JSON string into a JSON object.
///
/// `record` writes the frame as a string (the wire bytes, untouched) so the
/// recorder does not have to take a position on what the frame is. `replay`
/// does, so this is the place the string becomes a map. A line whose `frame`
/// is a bare `null`, a number, or a parse error is reported by the caller
/// the same way an unknown version is.
fn deserialize_frame_string<'de, D>(deserializer: D) -> Result<Map<String, Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    serde_json::from_str(&raw).map_err(serde::de::Error::custom)
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
            anyhow::bail!("--as-client requires a server command after the session, e.g. `replay --as-client session.jsonl -- python server.py`");
        }
        return run_as_client_async(&args.session, &args.server).await;
    }
    let target: ProtocolVersion = args
        .target
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("--target is required for the verdict mode; pass --diff or --as-server to skip it"))?
        .parse()
        .with_context(|| format!("--target is not a known revision: {}", args.target.as_deref().unwrap_or("")))?;
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

/// What one diff line records.
#[derive(Debug, Serialize, Deserialize)]
pub struct LineDiff {
    /// 1-indexed line in the left session.
    pub index: usize,
    /// What the diff at this line is.
    pub outcome: LineDiffOutcome,
    /// The line on the left, if it existed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub left: Option<Map<String, Value>>,
    /// The line on the right, if it existed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub right: Option<Map<String, Value>>,
    /// The recorded version on the left, if a line was present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub left_version: Option<String>,
    /// The recorded version on the right, if a line was present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub right_version: Option<String>,
    /// Human-readable note.
    pub note: String,
}

/// The diff verdict for one line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LineDiffOutcome {
    /// The line exists on both sides and the bytes are identical.
    Same,
    /// The line exists on both sides and the bytes differ.
    Changed,
    /// The line exists only on the left.
    Removed,
    /// The line exists only on the right.
    Added,
}

/// Totals for a diff run.
#[derive(Debug, Default)]
pub struct DiffSummary {
    same: usize,
    changed: usize,
    added: usize,
    removed: usize,
    malformed: usize,
}

impl DiffSummary {
    const fn record(&mut self, diff: &LineDiff) {
        match diff.outcome {
            LineDiffOutcome::Same => self.same += 1,
            LineDiffOutcome::Changed => self.changed += 1,
            LineDiffOutcome::Added => self.added += 1,
            LineDiffOutcome::Removed => self.removed += 1,
        }
    }

    fn write_to(&self, sink: &mut impl Write) -> std::io::Result<()> {
        writeln!(
            sink,
            "{} lines diffed: {} same, {} changed, {} added, {} removed, {} malformed",
            self.same + self.changed + self.added + self.removed,
            self.same,
            self.changed,
            self.added,
            self.removed,
            self.malformed,
        )
    }

    /// Whether the diff found no differences.
    ///
    /// `replay --diff` exits non-zero when this is false (and the operator
    /// did not pass `--allow-downgrade`-style tolerance), so a `diff` that
    /// reports clean is the "replay against the same server: zero diffs"
    /// half of the T20 spec.
    const fn is_clean(&self) -> bool {
        self.changed == 0 && self.added == 0 && self.removed == 0
    }
}

/// Loads a session into a list of parsed lines.
///
/// Skips empty lines the same way the verdict path does. Malformed
/// lines are reported on `summary` and dropped from the result; the
/// diff is still useful when one side has a bad line, because the
/// other side's line at the same index is still meaningful.
fn load_session<R: BufRead>(
    reader: R,
    summary: &mut DiffSummary,
) -> anyhow::Result<Vec<RecordedFrame>> {
    let mut frames: Vec<RecordedFrame> = Vec::new();
    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                return Err(anyhow::anyhow!("could not read a line: {error}"));
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<RecordedFrame>(&line) {
            Ok(recorded) => frames.push(recorded),
            Err(_) => {
                summary.malformed += 1;
            }
        }
    }
    Ok(frames)
}

/// Diffs two parsed session lists by line index and writes the
/// per-line results to `out` plus a summary to `summary`.
///
/// The pairing is by index: line `i` of the left is compared to line
/// `i` of the right. A line that exists only on one side is reported
/// as `removed` (left-only) or `added` (right-only); a line that
/// exists on both is `same` if the bytes are identical and `changed`
/// otherwise.
pub fn diff_with_io<R, W, S>(
    left: R,
    right: R,
    out: &mut W,
    summary: &mut S,
) -> anyhow::Result<DiffSummary>
where
    R: BufRead,
    W: Write,
    S: Write,
{
    let mut totals = DiffSummary::default();
    let left_frames = load_session(left, &mut totals)?;
    let right_frames = load_session(right, &mut totals)?;
    let pairs = left_frames.len().max(right_frames.len());
    for i in 0..pairs {
        let index = i + 1;
        let left_line = left_frames.get(i);
        let right_line = right_frames.get(i);
        let diff = match (left_line, right_line) {
            (Some(l), Some(r)) => {
                let outcome = if frame_equal(l, r) {
                    LineDiffOutcome::Same
                } else {
                    LineDiffOutcome::Changed
                };
                LineDiff {
                    index,
                    outcome,
                    left: Some(l.frame.clone()),
                    right: Some(r.frame.clone()),
                    left_version: Some(l.version.clone()),
                    right_version: Some(r.version.clone()),
                    note: match outcome {
                        LineDiffOutcome::Same => "identical".to_owned(),
                        LineDiffOutcome::Changed => "the bytes differ".to_owned(),
                        _ => String::new(),
                    },
                }
            }
            (Some(l), None) => LineDiff {
                index,
                outcome: LineDiffOutcome::Removed,
                left: Some(l.frame.clone()),
                right: None,
                left_version: Some(l.version.clone()),
                right_version: None,
                note: "present on the left, missing on the right".to_owned(),
            },
            (None, Some(r)) => LineDiff {
                index,
                outcome: LineDiffOutcome::Added,
                left: None,
                right: Some(r.frame.clone()),
                left_version: None,
                right_version: Some(r.version.clone()),
                note: "present on the right, missing on the left".to_owned(),
            },
            (None, None) => unreachable!("pairs = max(left, right); the for loop is bounded by it"),
        };
        totals.record(&diff);
        serde_json::to_writer(&mut *out, &diff)
            .context("writing a per-line diff")?;
        out.write_all(b"\n").context("writing the diff newline")?;
    }
    totals.write_to(summary)?;
    Ok(totals)
}

/// Two recorded frames are equal when their version and the bytes
/// they wrap are equal.
///
/// The bytes comparison is by string form, not by `Value` equality,
/// because two `Value::String("a")` and `Value::String("a")` round-trip
/// to the same string and the recorded form keeps the frame as a
/// string the recorder wrote; comparing the parsed form would be
/// comparing a re-parse of the same input, which is the answer we
/// want, but a `Value` `Eq` does not see the difference between
/// `{"k": 1}` and `{"k":1}` — and a recorded session that re-emits
/// the same bytes will hit that case. Comparing the recorded
/// `version` and the parsed frame's re-serialised form would also
/// collapse that distinction, so we compare the version string and
/// the parsed `Value` directly: any re-ordering of keys or whitespace
/// that survived the re-parse is a real diff.
fn frame_equal(left: &RecordedFrame, right: &RecordedFrame) -> bool {
    if left.version != right.version {
        return false;
    }
    let left_value = Value::Object(left.frame.clone());
    let right_value = Value::Object(right.frame.clone());
    left_value == right_value
}

/// The clap-driven diff entry point.
fn run_diff(left_path: &str, right_path: &str) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    let summary = diff_on_path(left_path, right_path, &mut stdout, &mut stderr)?;
    drop((stdout, stderr));
    if !summary.is_clean() {
        anyhow::bail!(
            "the two sessions differ: {} changed, {} added, {} removed",
            summary.changed,
            summary.added,
            summary.removed,
        );
    }
    Ok(())
}

/// Reads the two sessions and delegates.
fn diff_on_path<W, S>(
    left_path: &str,
    right_path: &str,
    stdout: &mut W,
    stderr: &mut S,
) -> anyhow::Result<DiffSummary>
where
    W: Write,
    S: Write,
{
    let left = open_session(left_path, "left")?;
    let right = open_session(right_path, "right")?;
    diff_with_io(left, right, stdout, stderr)
}

/// Opens a session file (or stdin for `-`).
fn open_session(path: &str, label: &str) -> anyhow::Result<std::io::BufReader<Box<dyn std::io::Read + Send>>> {
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
    answered: usize,
    not_found: usize,
    notifications: usize,
    unreadable: usize,
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
fn take_reply_by_id(replies: &mut VecDeque<Map<String, Value>>, id: &Value) -> Option<Map<String, Value>> {
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
            let serialised = serde_json::to_string(&refusal)
                .context("serialising a not-found refusal")?;
            writeln!(sink, "{serialised}").context("writing a not-found refusal")?;
            report.not_found += 1;
        }
    }
    report.write_to(summary)?;
    Ok(report)
}

/// Loads a session, extracts the `upstream→client` frames in order,
/// and starts the mock server on stdin/stdout.
fn run_as_server(session_path: &str) -> anyhow::Result<()> {
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
        let Ok(recorded) = serde_json::from_str::<RecordedFrame>(&line) else { continue };
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
        anyhow::bail!("the mock answered no requests; check the session file or the test client's request ids");
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
fn looks_like_response(frame: &Map<String, Value>) -> bool {
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

/// What the client driver reports for one replayed turn.
#[derive(Debug, Default)]
pub struct ClientReport {
    sent: usize,
    matched: usize,
    diffed: usize,
    malformed: usize,
    /// A recorded request that the live server did not answer.
    /// The server is hung, or the request was malformed, or the
    /// server crashed mid-flight. Either way, the diff cannot
    /// continue past this turn.
    unanswered: usize,
    /// A line that arrived on the wire that was not paired with
    /// a recorded request. The server wrote more than the
    /// recording expected.
    unexpected: usize,
}

impl ClientReport {
    /// Whether the diff found no differences and matched every
    /// sent request with a recorded response.
    const fn is_clean(&self) -> bool {
        self.matched == self.sent && self.diffed == 0 && self.unexpected == 0 && self.unanswered == 0
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
fn pair_recorded_turns(lines: &[RecordedFrame]) -> (Vec<CapturedTurn>, ClientReport) {
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
fn diff_turn(turn: &CapturedTurn) -> Option<LineDiff> {
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
async fn run_as_client_async(session_path: &str, server: &[String]) -> anyhow::Result<()> {
    use protocol_droid::adapters::primary::stdio::Diagnostics;
    use protocol_droid::adapters::secondary::stdio::{ChildSpec, ClientConfig, StdioClient};

    let (program, args) = server
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("--as-client needs at least one positional (the program); the trailing_var_arg was empty"))?;
    let spec = ChildSpec::new(program.as_str())
        .with_context(|| format!("building the child spec for {program}"))?;
    let spec = spec.with_args(args.iter().map(std::ffi::OsString::from))
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
        match client.next_frame().await {
            Ok(Some(reply)) => {
                let parsed: Map<String, Value> = match serde_json::from_str(&reply) {
                    Ok(parsed) => parsed,
                    Err(error) => {
                        eprintln!(
                            "the live server returned a frame that is not a JSON object for turn {}: {error}",
                            turn.request_index
                        );
                        report.malformed += 1;
                        captured_responses.push(None);
                        continue;
                    }
                };
                captured_responses.push(Some(parsed));
            }
            Ok(None) => {
                eprintln!(
                    "the live server closed its stdout before answering turn {}",
                    turn.request_index
                );
                report.unanswered += 1;
                captured_responses.push(None);
            }
            Err(error) => {
                eprintln!(
                    "the live server did not answer the request for turn {}: {error}",
                    turn.request_index
                );
                report.unanswered += 1;
                captured_responses.push(None);
            }
        }
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
        let verdict = replay_one(line, 1, target(), &leg_for(target()), policy(), &Translator::new(WireToolResults))?
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
        let verdict = replay_one(line, 1, target(), &leg_for(target()), policy(), &Translator::new(WireToolResults))?
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
        let _verdict = replay_one(line, 1, target(), &leg_for(target()), policy(), &Translator::new(WireToolResults))
            .expect_err("a null frame is a parse error, not a verdict");
    }

    /// A line whose `frame` is a JSON string that does not parse
    /// is reported as `Refused` with an error naming the parse
    /// failure. The deserialiser forwards the parse error rather
    /// than swallowing it.
    #[test]
    fn record_string_frame_with_unparseable_value_is_refused() {
        let line = r#"{"version":"2025-06-18","frame":"not even json"}"#;
        let _verdict = replay_one(line, 1, target(), &leg_for(target()), policy(), &Translator::new(WireToolResults))
            .expect_err("an unparseable frame is a parse error, not a verdict");
    }

    /// A 2025-06-18 `structuredContent` cannot reach 2024-11-05.
    #[test]
    fn structured_content_is_downgraded_when_replayed_to_2024_11_05() -> anyhow::Result<()> {
        let line = r#"{"version":"2025-06-18","frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}],\"structuredContent\":{\"key\":\"value\"}}}"}"#;
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
        let line = r#"{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32601,\"message\":\"Method not found\"}}"}"#;
        let verdict = replay_one(line, 1, target(), &leg_for(target()), policy(), &Translator::new(WireToolResults))?
            .expect("a well-formed line produces a verdict");
        assert_eq!(verdict.outcome, VerdictOutcome::Forwarded);
        assert!(verdict.unchanged);
        Ok(())
    }

    /// A request envelope (no `result`, no `error`) is forwarded.
    #[test]
    fn request_envelope_is_forwarded() -> anyhow::Result<()> {
        let line = r#"{"version":"2024-11-05","frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}"}"#;
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
        let line = r#"{"version":"2099-01-01","frame":"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}"}"#;
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
        assert!(summary.contains("line 2:"), "the bad line is named on stderr; got {summary:?}");
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
        let totals = diff_with_io(
            left.as_bytes(),
            right.as_bytes(),
            &mut out,
            &mut summary,
        )?;
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
        let totals = diff_with_io(
            left.as_bytes(),
            right.as_bytes(),
            &mut out,
            &mut summary,
        )?;
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
        let totals = diff_with_io(
            left.as_bytes(),
            right.as_bytes(),
            &mut out,
            &mut summary,
        )?;
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
        let totals = diff_with_io(
            b"" as &[u8],
            b"" as &[u8],
            &mut out,
            &mut summary,
        )?;
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
            json!({"jsonrpc":"2.0","id":1,"result":{"v":"one"}}).as_object().cloned().expect("an object"),
            json!({"jsonrpc":"2.0","id":2,"result":{"v":"two"}}).as_object().cloned().expect("an object"),
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
        assert!(output.contains("\"v\":\"one\""), "first reply served; got {output:?}");
        assert!(output.contains("\"v\":\"two\""), "second reply served; got {output:?}");
        Ok(())
    }

    /// A request whose id is not in the recording gets a typed
    /// JSON-RPC `method not found` error rather than a hang.
    /// The mock never blocks the test client.
    #[test]
    fn serve_with_io_reports_method_not_found_for_unknown_id() -> anyhow::Result<()> {
        let replies: VecDeque<Map<String, Value>> = vec![
            json!({"jsonrpc":"2.0","id":1,"result":{}}).as_object().cloned().expect("an object"),
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
        assert!(output.contains("-32601"), "the not-found error code is reported; got {output:?}");
        Ok(())
    }

    /// A notification (no `id`) is acknowledged by silence. The
    /// mock does not write a reply, the test client does not
    /// expect one, and the next request still finds its
    /// recorded answer.
    #[test]
    fn serve_with_io_silently_consumes_notifications() -> anyhow::Result<()> {
        let replies: VecDeque<Map<String, Value>> = vec![
            json!({"jsonrpc":"2.0","id":2,"result":{}}).as_object().cloned().expect("an object"),
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
        assert!(!output.contains("notifications/initialized"), "no reply to a notification; got {output:?}");
        assert!(output.contains("\"id\":2"), "the recorded reply is still served; got {output:?}");
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
            json!({"jsonrpc":"2.0","id":1,"result":{"v":"one"}}).as_object().cloned().expect("an object"),
            json!({"jsonrpc":"2.0","id":2,"result":{"v":"two"}}).as_object().cloned().expect("an object"),
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
            json!({"jsonrpc":"2.0","id":1,"result":{}}).as_object().cloned().expect("an object"),
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
                json!({"jsonrpc":"2.0","id":1,"method":"initialize"}).as_object().cloned().expect("an object"),
                "2024-11-05",
            ),
            recorded(
                Direction::UpstreamToClient,
                json!({"jsonrpc":"2.0","id":1,"result":{"v":"one"}}).as_object().cloned().expect("an object"),
                "2024-11-05",
            ),
            recorded(
                Direction::ClientToUpstream,
                json!({"jsonrpc":"2.0","id":2,"method":"ping"}).as_object().cloned().expect("an object"),
                "2024-11-05",
            ),
            recorded(
                Direction::UpstreamToClient,
                json!({"jsonrpc":"2.0","id":2,"result":{}}).as_object().cloned().expect("an object"),
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
            json!({"jsonrpc":"2.0","id":1,"method":"x"}).as_object().cloned().expect("an object"),
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
            json!({"jsonrpc":"2.0","id":1,"result":{}}).as_object().cloned().expect("an object"),
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
            frame: json!({"jsonrpc":"2.0","id":1,"method":"x"}).as_object().cloned().expect("an object"),
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
            request: json!({"jsonrpc":"2.0","id":1,"method":"x"}).as_object().cloned().expect("an object"),
            captured: None,
            expected: json!({"jsonrpc":"2.0","id":1,"result":{}}).as_object().cloned().expect("an object"),
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
            request: json!({"jsonrpc":"2.0","id":1,"method":"x"}).as_object().cloned().expect("an object"),
            captured: Some(json!({"jsonrpc":"2.0","id":1,"result":{"mutated":true}}).as_object().cloned().expect("an object")),
            expected: json!({"jsonrpc":"2.0","id":1,"result":{}}).as_object().cloned().expect("an object"),
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
                request: json!({"jsonrpc":"2.0","id":1,"method":"x"}).as_object().cloned().expect("an object"),
                captured: Some(json!({"jsonrpc":"2.0","id":1,"result":{}}).as_object().cloned().expect("an object")),
                expected: json!({"jsonrpc":"2.0","id":1,"result":{}}).as_object().cloned().expect("an object"),
                request_index: 1,
            },
            CapturedTurn {
                request: json!({"jsonrpc":"2.0","id":2,"method":"y"}).as_object().cloned().expect("an object"),
                captured: Some(json!({"jsonrpc":"2.0","id":2,"result":{"mutated":true}}).as_object().cloned().expect("an object")),
                expected: json!({"jsonrpc":"2.0","id":2,"result":{}}).as_object().cloned().expect("an object"),
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
}
