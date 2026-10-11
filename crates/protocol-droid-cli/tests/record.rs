//! Wire tests for the `record` subcommand.
//!
//! The unit tests in `src/record.rs` pin the per-frame logic; this
//! file exercises the public surface (`record::run` and the
//! `Recorder` / `JsonlRecorder` / `RecordingSource` / `RecordingSink`
//! types) so the lib's `pub` items are seen as used by the
//! dead-code lint and the CLI contract is checked the way a real
//! invocation would check it.
//!
//! Lints: tests assert on fallible calls, so `.expect()` is
//! unavoidable. Suppress at the file level.

#![expect(
    clippy::expect_used,
    reason = "tests assert on fallible calls; .expect() is the standard test pattern"
)]
#![expect(
    clippy::indexing_slicing,
    reason = "tests index into a fixed array of test inputs"
)]

use std::io::Write;
use std::sync::{Arc, Mutex};

use protocol_droid_cli::record::{
    JsonlRecorder, Recorder, RecordingSink, RecordingSource, redact_default,
};

/// A writer that hands every byte to a shared `Vec<u8>`.
struct Shared(Arc<Mutex<Vec<u8>>>);
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

#[test]
fn record_args_constructable() {
    // A smoke test for the CLI args. `RecordArgs` derives `Args`
    // (clap), so constructing it through `Default::default()` is
    // the cheapest way to exercise the derive without going
    // through the parser.
    let _ = format!("{:?}", protocol_droid_cli::record::RecordArgs {
        listen: "stdio:.".to_owned(),
        upstream: "stdio:./upstream".to_owned(),
        out: "-".to_owned(),
        allow_downgrade: false,
    });
}

#[test]
fn run_function_is_reachable_from_a_test() {
    // The test binary does not need to actually start a bridge —
    // `parse_spec` would refuse the spec, and that is enough to
    // prove the function is callable through its public name. The
    // type signature carries the integration value here: a future
    // refactor that drops `pub` from `run` would break this test.
    let _ = protocol_droid_cli::record::run;
}

#[test]
fn jsonl_recorder_round_trip_through_shared_writer() {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let writer = Shared(Arc::clone(&buf));
    let recorder = JsonlRecorder::new(writer, std::time::Instant::now());
    recorder.record_client_to_upstream(r#"{"id":1}"#);
    recorder.record_upstream_to_client(r#"{"id":1,"result":{}}"#);
    let out = String::from_utf8(buf.lock().expect("buffer").clone()).expect("utf-8");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("client→upstream"));
    assert!(lines[1].contains("upstream→client"));
    assert!(lines[0].contains("\"version\":\"pending\""));
}

#[test]
fn jsonl_recorder_set_negotiated_is_observable_in_subsequent_records() {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let writer = Shared(Arc::clone(&buf));
    let recorder = JsonlRecorder::new(writer, std::time::Instant::now());
    recorder.record_client_to_upstream("{}");
    recorder.set_negotiated("2025-06-18".to_owned());
    recorder.record_client_to_upstream("{}");
    let out = String::from_utf8(buf.lock().expect("buffer").clone()).expect("utf-8");
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].contains("\"version\":\"pending\""));
    assert!(lines[1].contains("\"version\":\"2025-06-18\""));
}

#[test]
fn redact_default_function_is_reachable() {
    let frame = r#"{"params":{"headers":{"Authorization":"Bearer xyz"}}}"#;
    let redacted = redact_default(frame);
    assert!(!redacted.contains("xyz"));
    assert!(redacted.contains("\"Authorization\":\"***\""));
}

#[test]
fn recording_source_is_constructable() {
    // The recording source wraps a `FrameSource`; building it
    // through the public API proves the type is reachable. The
    // unit tests in `src/record.rs` cover the runtime behaviour.
    struct Null;
    impl protocol_droid::application::jsonrpc::FrameSource for Null {
        fn next_frame(
            &self,
        ) -> impl std::future::Future<
            Output = Result<Option<String>, protocol_droid::application::jsonrpc::SourceError>,
        > + Send {
            std::future::ready(Ok(None))
        }
    }
    struct Counting;
    impl Recorder for Counting {
        fn record_client_to_upstream(&self, _frame: &str) {}
        fn record_upstream_to_client(&self, _frame: &str) {}
    }
    let recorder: Arc<dyn Recorder> = Arc::new(Counting);
    let _ = RecordingSource::new(Null, recorder);
}

#[test]
fn recording_sink_is_constructable() {
    struct Null;
    impl protocol_droid::application::session::FrameSink for Null {
        fn send(
            &self,
            _frame: &str,
        ) -> impl std::future::Future<
            Output = Result<(), protocol_droid::application::session::SinkError>,
        > + Send {
            std::future::ready(Ok(()))
        }
    }
    struct Counting;
    impl Recorder for Counting {
        fn record_client_to_upstream(&self, _frame: &str) {}
        fn record_upstream_to_client(&self, _frame: &str) {}
    }
    let recorder: Arc<dyn Recorder> = Arc::new(Counting);
    let _ = RecordingSink::new(Null, recorder);
}
