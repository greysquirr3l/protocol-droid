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

use clap::Args;

use crate::record::Direction;

mod client;
mod diff;
mod report;
mod serve;

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "tests assert on fallible calls; .expect() is the standard test pattern"
)]
mod tests;

// The four modes are separate modules, and the items they publish are
// re-exported here so that `replay::FrameVerdict` still means what it meant
// when every mode shared one file. `run` is the command's entry point, and it
// lives in `report` because replaying a session frame by frame is the
// command's primary mode; the other three are the flags that change what
// replaying means.
pub use client::{CapturedTurn, ClientReport, drive_session};
pub use diff::{DiffSummary, LineDiff, LineDiffOutcome, diff_with_io};
pub use report::{FrameVerdict, RunSummary, VerdictOutcome, run, run_with_io};
use serde::Deserialize;
use serde_json::{Map, Value};
pub use serve::{ServeReport, serve_with_io};

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
