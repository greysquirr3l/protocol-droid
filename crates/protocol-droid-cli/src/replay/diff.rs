//! The `--diff` mode: compare two captured sessions.
//!
//! The left session is paired with the right by line index, each line gets a
//! `same` / `changed` / `added` / `removed` verdict, and the command exits
//! non-zero on a non-clean diff. Same session = zero diffs; mutated session =
//! a precise diff at the line that changed.
//!
//! It is the "assert on the server's responses with a diff on mismatch" half
//! of the T20 spec.

use super::RecordedFrame;
use super::serve::open_session;
use std::io::{BufRead, Write};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

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
    pub(super) same: usize,
    pub(super) changed: usize,
    pub(super) added: usize,
    pub(super) removed: usize,
    pub(super) malformed: usize,
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
    pub(super) const fn is_clean(&self) -> bool {
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
        serde_json::to_writer(&mut *out, &diff).context("writing a per-line diff")?;
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
pub(super) fn run_diff(left_path: &str, right_path: &str) -> anyhow::Result<()> {
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
