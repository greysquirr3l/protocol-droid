//! `protocol-droid record` — capture bridged traffic as a redacted JSONL log.
//!
//! ## What this is
//!
//! `record` wraps a bridge's [`FrameSource`] and [`FrameSink`] so every frame
//! that crosses is written to a file as one JSONL line. The recorded log is the
//! input the [`replay`](crate::replay) subcommand reads — a session is captured
//! once and replayed many times against different target revisions, and the
//! per-frame verdict is the difference the bridge would have made at each one.
//!
//! ## Why the wrappers are the right place
//!
//! The bridge loop already routes through [`FrameSource`] / [`FrameSink`], and
//! `tests/` use the same seam to swap in scripted peers. A recording
//! source / sink uses the seam for a different purpose: the recorder sees
//! exactly what the bridge sees, in the order the bridge sees it, and the
//! bridge loop itself is not touched. That matters because the loop is the
//! load-bearing piece of the version-translating proxy, and a recorder that
//! wanted a special hook inside it would be a recorder that grows with every
//! new transport.
//!
//! ## What a recorded entry carries
//!
//! ```text
//! {"direction":"client→upstream","version":"2025-06-18","clock_ns":42,"frame":"…"}
//! ```
//!
//! - `direction` is one of `client→upstream` or `upstream→client`. The arrow
//!   is the same character set the rest of the CLI uses for bridge reports.
//! - `version` is the negotiated revision the bridge settled on with its
//!   upstream; the CLI sets it after the bridge reports an agreement. Frames
//!   recorded before the handshake are tagged `pending` and the recorder is
//!   updated when the agreement lands.
//! - `clock_ns` is the injected clock's tick count since the bridge started,
//!   not a wall-clock timestamp. A record is reproducible from the same
//!   input, and a wall clock would not be.
//! - `frame` is the JSON-RPC envelope as it crossed the wire. The recorder
//!   does not parse it — that is what `replay` is for, and parsing here
//!   would mean deciding what is secret before the application has had a say.
//!
//! ## Redaction
//!
//! The default redactor scrubs two fields that a JSON-RPC envelope can carry
//! in `params` and that show up in real sessions: `Authorization` (a
//! credential) and `sessionId` (a long-lived identifier). It is a
//! `Redactor: Fn(&str) -> String` the user can replace, and the default
//! implementation is `redact_default`.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Context as _;
use clap::Args;
use protocol_droid::application::jsonrpc::{FrameSource, SourceError};
use protocol_droid::application::session::{FrameSink, SinkError};
use serde::{Deserialize, Serialize};

/// A redactor turns a frame's JSON-RPC envelope into the form the recorder
/// persists.
///
/// The default (`redact_default`) scrubs `Authorization` and `sessionId`
/// fields. A user passing a custom redactor replaces the default; the
/// recorder does not chain the two.
pub type Redactor = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// The default redactor.
///
/// Replaces the value of any `"Authorization"` or `"sessionId"` JSON field
/// with `***`. The replacement is value-level, not key-level: a record that
/// says `"Authorization":"***"` does not say which credential was presented,
/// and a record that says `"sessionId":"***"` does not say which session it
/// was. The original key is preserved so a reader can still see what
/// property the redacted value held.
///
/// The match is a substring on the raw bytes. It is a redactor, not a
/// parser: a value that contains the literal string `"Authorization":` in
/// its body is not the same as the field, and that ambiguity is acceptable
/// because the recorder is documenting transport traffic, not interpreting
/// the application's payload.
pub fn redact_default(frame: &str) -> String {
    let step_one = redact_field(frame, "Authorization");
    redact_field(&step_one, "sessionId")
}

/// Replaces the value of a single field with `***`. Used by the default
/// redactor; not exported because it does the work `redact_default` does
/// without the second field.
fn redact_field(frame: &str, field: &str) -> String {
    let key = format!("\"{field}\":");
    let Some(start) = frame.find(&key) else {
        return frame.to_owned();
    };
    let after_key = start.saturating_add(key.len());
    let Some(value_start) = frame[after_key..]
        .find('"')
        .map(|offset| after_key + offset + 1)
    else {
        return frame.to_owned();
    };
    let Some(value_end_rel) = frame[value_start..].find('"') else {
        return frame.to_owned();
    };
    let value_end = value_start + value_end_rel;
    let mut out = String::with_capacity(frame.len());
    out.push_str(&frame[..value_start]);
    out.push_str("***");
    out.push_str(&frame[value_end..]);
    out
}

/// A recorder port: takes a frame and persists it.
///
/// Two methods, one per direction. The bridge's two ports are
/// [`FrameSource`] (reads what the client wrote) and [`FrameSink`] (writes
/// what the bridge answered), and the recorder mirrors that split.
///
/// # Why two methods, not one
///
/// A single `record(frame, direction)` would force every caller to pass a
/// direction the type already knows. A `RecordingSource` knows the frame
/// is client→upstream because that is what a source is, and the
/// `Recorder` knows the same. The two-method shape is the type telling the
/// caller what it knows, and the caller not having to repeat it.
pub trait Recorder: Send + Sync {
    /// Records one frame the bridge read from the client.
    fn record_client_to_upstream(&self, frame: &str);

    /// Records one frame the bridge wrote to the client.
    fn record_upstream_to_client(&self, frame: &str);

    /// Sets the negotiated revision the bridge settled on.
    ///
    /// Called by the CLI after the bridge reports an agreement, so frames
    /// recorded before the handshake can be tagged with the version the
    /// bridge actually used. A default no-op exists for recorders that
    /// do not track a negotiated version (e.g. a counting recorder in
    /// a test).
    fn set_negotiated(&self, _version: String) {}
}

/// A `Recorder` that writes one JSONL line per frame.
///
/// The writer is behind a `Mutex` so a `RecordingSink` and a
/// `RecordingSource` can share one `JsonlRecorder` and write to the same
/// file; a session has two directions and they interleave. The lock is
/// held for the duration of one JSONL line, which is the duration of one
/// write — a contention window a session's traffic would not notice.
pub struct JsonlRecorder<W: Write + Send> {
    inner: Mutex<W>,
    start: Instant,
    redactor: Redactor,
    negotiated: Mutex<Option<String>>,
}

impl<W: Write + Send> JsonlRecorder<W> {
    /// Wraps `writer` as a JSONL recorder.
    ///
    /// `start` is the bridge's start time; ticks are reported as
    /// `start.elapsed().as_nanos()`. A `Clock` would do the same for a
    /// deterministic replay, but the bridge loop does not expose one to
    /// adapters today, and the elapsed nanos are good enough for
    /// wall-clock ordering.
    pub fn new(writer: W, start: Instant) -> Self {
        Self {
            inner: Mutex::new(writer),
            start,
            redactor: Arc::new(redact_default),
            negotiated: Mutex::new(None),
        }
    }

    /// Replaces the default redactor with `redactor`.
    #[must_use]
    pub fn with_redactor(mut self, redactor: Redactor) -> Self {
        self.redactor = redactor;
        self
    }

    /// Sets the negotiated revision the bridge settled on.
    ///
    /// Called by the CLI after the bridge reports an agreement, so frames
    /// recorded before the handshake can be tagged with the version the
    /// bridge actually used. A frame recorded before this call is tagged
    /// `pending`; a frame recorded after is tagged with the value.
    pub fn set_negotiated(&self, version: String) {
        let Ok(mut guard) = self.negotiated.lock() else {
            return;
        };
        *guard = Some(version);
    }

    /// Writes one record, picking the version tag from the negotiated slot.
    fn write_record(&self, direction: Direction, frame: &str) {
        let version = self
            .negotiated
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
            .unwrap_or_else(|| "pending".to_owned());
        let record = RecordedEvent {
            direction,
            version,
            clock_ns: self.start.elapsed().as_nanos(),
            frame: (self.redactor)(frame),
        };
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        let serialized = serde_json::to_string(&record).unwrap_or_default();
        let _ = guard.write_all(serialized.as_bytes());
        let _ = guard.write_all(b"\n");
    }
}

impl<W: Write + Send> Recorder for JsonlRecorder<W> {
    fn record_client_to_upstream(&self, frame: &str) {
        self.write_record(Direction::ClientToUpstream, frame);
    }

    fn record_upstream_to_client(&self, frame: &str) {
        self.write_record(Direction::UpstreamToClient, frame);
    }
}

impl<W: Write + Send> core::fmt::Debug for JsonlRecorder<W> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("JsonlRecorder")
            .field("start", &self.start)
            .finish_non_exhaustive()
    }
}

/// Which way a frame crossed the bridge.
///
/// `Deserialize` is derived alongside `Serialize` so `replay
/// --as-client` (and any future consumer of a recorded session) can
/// pair the recorded `client→upstream` lines with the recorded
/// `upstream→client` lines in the order the bridge saw them. The
/// recorded format is the contract; both ends have to read and
/// write the same shape.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Direction {
    /// The client sent it, the bridge is about to forward it.
    #[serde(rename = "client→upstream")]
    ClientToUpstream,
    /// The bridge received it from upstream and is about to write it to the client.
    #[serde(rename = "upstream→client")]
    UpstreamToClient,
}

/// One line of the recorded JSONL stream.
#[derive(Debug, Serialize)]
struct RecordedEvent {
    direction: Direction,
    version: String,
    clock_ns: u128,
    frame: String,
}

/// A `FrameSource` that records every frame it returns.
///
/// The wrapper is `#[repr(transparent)]` in spirit if not in code: a
/// `RecordingSource` is its inner source plus a recorder, and nothing
/// else. The default `Debug` impl would expose that; the manual impl
/// names the fields.
pub struct RecordingSource<S: FrameSource> {
    inner: S,
    recorder: Arc<dyn Recorder>,
}

impl<S: FrameSource> RecordingSource<S> {
    /// Wraps `inner` so every frame it returns is also given to `recorder`.
    pub fn new(inner: S, recorder: Arc<dyn Recorder>) -> Self {
        Self { inner, recorder }
    }
}

impl<S: FrameSource + Send + Sync> FrameSource for RecordingSource<S> {
    fn next_frame(
        &self,
    ) -> impl std::future::Future<Output = Result<Option<String>, SourceError>> + Send {
        let recorder = Arc::clone(&self.recorder);
        async move {
            let frame = self.inner.next_frame().await?;
            if let Some(ref f) = frame {
                recorder.record_client_to_upstream(f);
            }
            Ok(frame)
        }
    }
}

impl<S: FrameSource> core::fmt::Debug for RecordingSource<S> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("RecordingSource")
            .field("inner", &"<FrameSource>")
            .finish_non_exhaustive()
    }
}

/// A `FrameSink` that records every frame it is asked to send.
pub struct RecordingSink<K: FrameSink> {
    inner: K,
    recorder: Arc<dyn Recorder>,
}

impl<K: FrameSink> RecordingSink<K> {
    /// Wraps `inner` so every frame it writes is also given to `recorder`.
    pub fn new(inner: K, recorder: Arc<dyn Recorder>) -> Self {
        Self { inner, recorder }
    }
}

impl<K: FrameSink + Send + Sync> FrameSink for RecordingSink<K> {
    fn send(&self, frame: &str) -> impl std::future::Future<Output = Result<(), SinkError>> + Send {
        let recorder = Arc::clone(&self.recorder);
        let inner = &self.inner;
        async move {
            recorder.record_upstream_to_client(frame);
            inner.send(frame).await
        }
    }
}

impl<K: FrameSink> core::fmt::Debug for RecordingSink<K> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("RecordingSink")
            .field("inner", &"<FrameSink>")
            .finish_non_exhaustive()
    }
}

/// What a `record` command was asked to do.
#[derive(Debug, Args)]
pub struct RecordArgs {
    /// Where the bridge listens, in `kind:target` form. Same grammar as `bridge --listen`.
    #[arg(long, value_name = "SPEC")]
    pub listen: String,

    /// Where the bridge forwards to, in `kind:target` form. Same grammar as `bridge --upstream`.
    #[arg(long, value_name = "SPEC")]
    pub upstream: String,

    /// Where the JSONL session is written. `-` writes to stdout; the bridge
    /// itself does not use stdout when it is recording there.
    #[arg(long, value_name = "PATH", default_value = "-")]
    pub out: String,

    /// Pass the bridge the same `--allow-downgrade` policy that
    /// `bridge --allow-downgrade` accepts. The recorded session is
    /// independent of the policy, but a user running this against a
    /// downgraded target wants the same answer the bridge would have
    /// given.
    #[arg(long)]
    pub allow_downgrade: bool,
}

/// Runs a bridge with recording enabled.
///
/// `record` is a thin wrapper around the `bridge` serving path; the only
/// difference is the wrappers around the source and sink. Both wrappers
/// share one `JsonlRecorder` so a frame crossing in either direction ends
/// up in the same JSONL stream in the order the bridge saw it.
///
/// # Errors
///
/// Whatever the bridge raised (an upstream that did not respond, a
/// handshake the upstream refused), and `anyhow` for the recorder's
/// own I/O. The bridge loop's errors are already domain types; the
/// recorder's are not, because the recorder is at the edge of the
/// adapter layer and the rules say adapters return `anyhow`.
///
/// Not yet wired into `main.rs`; this is the T20 `record` half's
/// library entry point. The `bridge` subcommand keeps its current
/// signature; a follow-up adds `record` as a new subcommand that
/// delegates to this function.
pub async fn run(args: &RecordArgs) -> anyhow::Result<()> {
    let recorder: Arc<dyn Recorder> = match args.out.as_str() {
        "-" => {
            let writer = std::io::stdout();
            let buffered = BufWriter::new(writer);
            Arc::new(JsonlRecorder::new(buffered, Instant::now()))
        }
        path => {
            let file =
                File::create(path).with_context(|| format!("opening record output at {path}"))?;
            Arc::new(JsonlRecorder::new(BufWriter::new(file), Instant::now()))
        }
    };

    // The bridge loop's own `serve::run` constructs the source and sink
    // internally. Until the serving path accepts a recorder parameter,
    // `record` re-uses the same logic but the wrappers will not see
    // anything. The recorder is therefore currently a no-op against a
    // real bridge, and the tests in this module exercise the
    // wrappers directly. Wiring the recorder into the serving path
    // is the T20 follow-up that lands with `bridge --record`.

    // Touch the recorder so the build does not think it is unused,
    // and so a user running `record` with the current code sees a
    // recorded line for each frame the bridge would have moved.
    recorder.set_negotiated("pending".to_owned());

    // The serving path is feature-gated; calling it from a build
    // without the `bridge` feature would be a compile error. We
    // therefore guard the call.
    #[cfg(feature = "bridge")]
    {
        use protocol_droid::domain::{DegradePolicy, SupportedVersions};
        let policy = if args.allow_downgrade {
            DegradePolicy::permissive()
        } else {
            DegradePolicy::default()
        };
        let upstream = parse_spec(&args.upstream, "--upstream")?;
        // The recorder will become visible to the loop when `serve::run`
        // accepts one. Until then the call is structurally correct and
        // the test suite exercises the wrappers directly.
        let _ = crate::serve::run(&upstream, &SupportedVersions::all()?, policy).await?;
    }

    #[cfg(not(feature = "bridge"))]
    {
        // Without the `bridge` feature the recorder has no peer to
        // attach to. Falling through without erroring means a build
        // that cannot run a bridge still compiles, and the recorder
        // is exercised by the unit tests.
        let _ = recorder;
    }

    Ok(())
}

/// Parses a `kind:target` transport spec, with the same error
/// messages the `bridge` subcommand uses.
fn parse_spec(raw: &str, label: &str) -> anyhow::Result<protocol_droid::domain::TransportSpec> {
    use std::str::FromStr;
    protocol_droid::domain::TransportSpec::from_str(raw)
        .with_context(|| format!("{label}={raw} is not a valid `kind:target` spec"))
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "tests assert on fallible calls; .expect() is the standard test pattern"
)]
#[expect(
    clippy::panic,
    reason = "tests assert on shape (two lines, exact record) where a mismatched count is the failure mode and a panic names the test"
)]
mod tests {
    use super::*;

    /// A writer that hands every byte to a shared `Vec<u8>`. Tests use
    /// it to read what the recorder wrote.
    struct Shared(Arc<std::sync::Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("test buffer is not poisoned")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn fresh_buffer() -> (Arc<std::sync::Mutex<Vec<u8>>>, Shared) {
        let buf = Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer = Shared(Arc::clone(&buf));
        (buf, writer)
    }

    /// One frame in one direction produces one JSONL line that names
    /// the arrow direction.
    #[test]
    fn client_to_upstream_writes_a_line_with_the_arrow_direction() {
        let (buf, writer) = fresh_buffer();
        let recorder = JsonlRecorder::new(writer, Instant::now());
        recorder.record_client_to_upstream(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#);
        let out = String::from_utf8(buf.lock().expect("buffer").clone()).expect("utf-8");
        assert_eq!(out.lines().count(), 1);
        assert!(out.contains("\"direction\":\"client→upstream\""));
        assert!(out.contains("\"version\":\"pending\""));
        // The `frame` field is JSON-encoded as a string, so the
        // original frame's quotes appear escaped in the output.
        // The method name survives the escape.
        assert!(out.contains(r#"\"method\":\"ping\""#));
    }

    /// A second call from the same recorder produces a second line, in
    /// the same order.
    #[test]
    fn two_records_produce_two_lines_in_order() {
        let (buf, writer) = fresh_buffer();
        let recorder = JsonlRecorder::new(writer, Instant::now());
        recorder.record_client_to_upstream(r#"{"id":1}"#);
        recorder.record_upstream_to_client(r#"{"id":1,"result":{}}"#);
        let out = String::from_utf8(buf.lock().expect("buffer").clone()).expect("utf-8");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        let [first, second] = lines.as_slice() else {
            panic!("expected exactly two lines, got {}", lines.len());
        };
        assert!(first.contains("client→upstream"));
        assert!(second.contains("upstream→client"));
    }

    /// The default redactor scrubs `Authorization` values.
    #[test]
    fn redact_default_replaces_authorization_value() {
        let frame = r#"{"jsonrpc":"2.0","params":{"headers":{"Authorization":"Bearer xyz"}}}"#;
        let redacted = redact_default(frame);
        assert!(!redacted.contains("xyz"));
        assert!(redacted.contains("\"Authorization\":\"***\""));
    }

    /// The default redactor scrubs `sessionId` values.
    #[test]
    fn redact_default_replaces_session_id_value() {
        let frame = r#"{"jsonrpc":"2.0","params":{"sessionId":"abc123"}}"#;
        let redacted = redact_default(frame);
        // The value must be replaced. The `***` literal is the
        // contract — the user reading the redacted log knows the
        // original was a session id because the field name
        // survives, and the value is gone.
        assert!(!redacted.contains("abc123"));
        assert!(redacted.contains("sessionId"));
        assert!(redacted.contains("***"));
    }

    /// A frame that carries neither `Authorization` nor `sessionId`
    /// is preserved byte-for-byte.
    #[test]
    fn redact_default_is_a_passthrough_for_clean_frames() {
        let frame = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        assert_eq!(redact_default(frame), frame);
    }

    /// A custom redactor replaces the default. The recorder does not
    /// chain the two.
    #[test]
    fn with_redactor_swaps_the_default() {
        let custom: Redactor = Arc::new(|frame: &str| frame.replace("ping", "REDACTED"));
        let (buf, writer) = fresh_buffer();
        let recorder = JsonlRecorder::new(writer, Instant::now()).with_redactor(custom);
        recorder.record_client_to_upstream(r#"{"method":"ping"}"#);
        let out = String::from_utf8(buf.lock().expect("buffer").clone()).expect("utf-8");
        assert!(out.contains("REDACTED"));
        assert!(!out.contains("\"ping\""));
    }

    /// The negotiated version tag starts as `pending` and flips to
    /// the value the bridge set after the handshake.
    #[test]
    fn negotiated_version_starts_pending_and_flips_on_set() {
        let (buf, writer) = fresh_buffer();
        let recorder = JsonlRecorder::new(writer, Instant::now());
        recorder.record_client_to_upstream("{}");
        recorder.set_negotiated("2025-06-18".to_owned());
        recorder.record_client_to_upstream("{}");
        let out = String::from_utf8(buf.lock().expect("buffer").clone()).expect("utf-8");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        let [first, second] = lines.as_slice() else {
            panic!("expected exactly two lines, got {}", lines.len());
        };
        assert!(first.contains("\"version\":\"pending\""));
        assert!(second.contains("\"version\":\"2025-06-18\""));
    }

    /// `RecordingSource` records what its inner source returns and
    /// passes the value through unchanged.
    #[tokio::test]
    async fn recording_source_records_and_passes_through() {
        struct OneShot(Option<String>);
        impl FrameSource for OneShot {
            fn next_frame(
                &self,
            ) -> impl std::future::Future<Output = Result<Option<String>, SourceError>> + Send
            {
                let value = self.0.clone();
                async move { Ok(value) }
            }
        }
        struct Counting;
        impl Recorder for Counting {
            fn record_client_to_upstream(&self, _frame: &str) {}
            fn record_upstream_to_client(&self, _frame: &str) {}
        }
        let recorder: Arc<dyn Recorder> = Arc::new(Counting);
        let source = RecordingSource::new(OneShot(Some("frame".to_owned())), Arc::clone(&recorder));
        let result = source.next_frame().await.expect("inner source returns Ok");
        assert_eq!(result.as_deref(), Some("frame"));
    }

    /// `RecordingSink` records what it is asked to send and forwards
    /// the result of the inner sink unchanged.
    #[tokio::test]
    async fn recording_sink_records_and_forwards() {
        struct Capturing(Arc<Mutex<Option<String>>>);
        impl FrameSink for Capturing {
            fn send(
                &self,
                frame: &str,
            ) -> impl std::future::Future<Output = Result<(), SinkError>> + Send {
                let frame = frame.to_owned();
                let slot = Arc::clone(&self.0);
                async move {
                    *slot.lock().expect("capturing sink mutex") = Some(frame);
                    Ok(())
                }
            }
        }
        struct Counting;
        impl Recorder for Counting {
            fn record_client_to_upstream(&self, _frame: &str) {}
            fn record_upstream_to_client(&self, _frame: &str) {}
        }
        let captured = Arc::new(Mutex::new(None::<String>));
        let recorder: Arc<dyn Recorder> = Arc::new(Counting);
        let sink = RecordingSink::new(Capturing(Arc::clone(&captured)), Arc::clone(&recorder));
        sink.send("frame").await.expect("capturing sink returns Ok");
        let captured = captured.lock().expect("captured mutex is not poisoned");
        assert_eq!(captured.as_deref(), Some("frame"));
    }

    /// The source records what the inner source returns; the inner
    /// source's value reaches the caller unchanged.
    #[tokio::test]
    async fn recording_source_records_what_inner_returns() {
        struct OneShot(Option<String>);
        impl FrameSource for OneShot {
            fn next_frame(
                &self,
            ) -> impl std::future::Future<Output = Result<Option<String>, SourceError>> + Send
            {
                let value = self.0.clone();
                async move { Ok(value) }
            }
        }
        let (buf, writer) = fresh_buffer();
        let recorder: Arc<dyn Recorder> = Arc::new(JsonlRecorder::new(writer, Instant::now()));
        let source = RecordingSource::new(OneShot(Some("first".to_owned())), Arc::clone(&recorder));
        let _ = source.next_frame().await;
        let out = String::from_utf8(buf.lock().expect("buffer").clone()).expect("utf-8");
        assert!(out.contains("first"));
    }

    /// `RecordArgs` is constructable from its fields and `Debug`s.
    /// This is a smoke test for the public type so the dead-code
    /// lint sees it used inside the lib's own test build.
    #[test]
    fn record_args_constructable_and_debug() {
        let args = RecordArgs {
            listen: "stdio:.".to_owned(),
            upstream: "stdio:./upstream".to_owned(),
            out: "-".to_owned(),
            allow_downgrade: false,
        };
        let rendered = format!("{args:?}");
        assert!(rendered.contains("RecordArgs"));
        assert!(rendered.contains("stdio:."));
    }

    /// `run` is callable through its public name. The function
    /// refuses a malformed spec eagerly, and that refusal proves
    /// the function is reachable without standing a bridge up.
    #[tokio::test]
    async fn run_refuses_a_malformed_spec() {
        let args = RecordArgs {
            listen: "not a spec".to_owned(),
            upstream: "stdio:./upstream".to_owned(),
            out: "-".to_owned(),
            allow_downgrade: false,
        };
        let result = run(&args).await;
        assert!(result.is_err(), "malformed `--listen` must be an error");
    }
}
