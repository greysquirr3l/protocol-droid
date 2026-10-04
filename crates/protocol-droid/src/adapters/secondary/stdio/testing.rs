//! The two things this module's tests need that no other test can lend them.
//!
//! # A child process, on every platform
//!
//! [`child::spawn`](super::child) is the thing under test, so the child has to
//! come from somewhere real. What exists on every platform is
//! [`std::env::current_exe`], which in a test is the test binary — so the
//! helper below is an `#[ignore]`d test that the parent re-executes with an
//! environment variable set, and libtest runs exactly that one test because of
//! the filter passed alongside it.
//!
//! Nothing here shells out. `sh -c` does not exist on Windows and the project
//! rules rule it out anyway; a re-exec of `current_exe()` is one `Command`
//! with an absolute path, identical on all three targets. It also gives a
//! child that *is* this crate, so the request documents these tests put on the
//! wire are the ones the real client writes rather than a fixture standing in
//! for them.
//!
//! # The one thing this costs
//!
//! libtest prints `running 1 test` to stdout before the helper's body runs,
//! and the child's stdout *is* the protocol stream. So frame zero is harness
//! output, and `next_reply` in the client's tests is the single place that
//! accounts for it. The adapter itself stays strict: a frame that is not a
//! reply is still
//! [`RequestError::UnreadableFrame`](super::request::RequestError::UnreadableFrame)
//! to a caller, which is the right answer to a peer that puts something other
//! than JSON-RPC on a stdio stream.
//!
//! The consequence is worth stating plainly: a test here cannot prove that
//! *nothing* appears on the child's stdout, because something does. What it
//! can prove — and does — is that the child's stderr marker never does, which
//! is the leak that matters and the only one the forwarding code could cause.
//!
//! # The child is a blocking program
//!
//! The helper is a plain `#[test]`, not a `#[tokio::test]`. It reads a line,
//! writes a line, repeats, and has no reason to await anything; the runtime
//! `tokio::io::stdin()` would put behind it would move the read onto a
//! blocking thread and add a thread per child for no benefit.

use std::env;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use tokio::io::AsyncWrite;

use super::spec::ChildSpec;

/// The environment variable that turns a test binary into a child server.
const ROLE_ENV: &str = "PROTOCOL_DROID_STDIO_CHILD_ROLE";

/// The marker the child writes to stderr, so a test can prove where it went.
///
/// `pub(super)` because the assertion that matters is made from
/// `client.rs`'s tests, not from the helper's own: it is the *client* that must
/// keep the child's stderr off the protocol stream, so that is where the
/// marker has to be read back.
pub(super) const STDERR_MARKER: &str = "protocol-droid-child-stderr-marker";

/// The full name of the helper test, as libtest spells it.
///
/// Passed to the child as a filter rather than looked up, so a rename shows up
/// as a child that answers nothing rather than as a test that quietly stops
/// spawning a child at all.
const CHILD_TEST: &str = "adapters::secondary::stdio::testing::tests::stdio_child_server";

/// What the child does when it starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Role {
    /// Answer every request, then exit when stdin closes.
    Echo,
    /// Answer every request, writing each answer in two writes.
    Split,
    /// Answer the first request, then exit.
    Once,
    /// Read stdin, write nothing at all.
    Silent,
    /// Exit without reading anything.
    Exit,
    /// Write [`STDERR_MARKER`] to stderr, then answer every request.
    Stderr,
}

impl Role {
    /// This role's name, in both directions.
    ///
    /// One table for both halves, because a role that could be spelled one way
    /// in the environment and another in the child's answer would be a way for
    /// a test to look for `split` and find `echo`.
    fn name(self) -> &'static str {
        match self {
            Self::Echo => "echo",
            Self::Split => "split",
            Self::Once => "once",
            Self::Silent => "silent",
            Self::Exit => "exit",
            Self::Stderr => "stderr",
        }
    }

    /// Every role, so a test that has to cover them all does not keep its own
    /// copy of the list and let it go stale.
    pub(super) const fn all() -> [Self; 6] {
        [
            Self::Echo,
            Self::Split,
            Self::Once,
            Self::Silent,
            Self::Exit,
            Self::Stderr,
        ]
    }

    /// The role named by an environment value.
    ///
    /// `None` for anything unrecognised, which is also the answer for the
    /// variable being absent: an ordinary `cargo test` run reaches the helper
    /// with no role set and must do nothing at all.
    fn from_env() -> Option<Self> {
        let name = env::var(ROLE_ENV).ok()?;
        Self::all().into_iter().find(|role| role.name() == name)
    }
}

/// The child spec for a role: this test binary, told which role to be.
///
/// `current_exe` is the only path available on all three targets without
/// assuming a shell, a `node`, or a particular build layout.
///
/// Four of the arguments exist to keep libtest's own output off the wire,
/// and each one was found by the symptom it prevents:
///
/// - `--exact` — without it the filter is a substring match, and a rename
///   that left the prefix intact would run *every* test sharing it. The
///   child would then have several bodies writing frames.
/// - `--nocapture` — so the helper's own writes reach the pipe rather than
///   being captured for a libtest report nobody will read.
/// - `--quiet` — the load-bearing one. libtest prints `test <name> ... ` to
///   **stdout** as a test starts, and this child's stdout _is_ the protocol
///   stream. That line carries no newline, so it lands mid-frame and the
///   parent's decoder never yields it: the reply is written in full and the
///   call still times out. `--nocapture` alone does not suppress it.
/// - `--test-threads=1` — one body must be the only writer.
///
/// The leading `running 1 test` line and the closing summary survive
/// `--quiet` and cannot be removed, which is what `next_reply` in the
/// client's tests exists for.
pub(super) fn child_spec(role: Role) -> Result<ChildSpec, SpawnFailure> {
    let exe = env::current_exe().map_err(|_| SpawnFailure::NoCurrentExe)?;
    ChildSpec::new(exe)
        .and_then(|spec| {
            spec.with_args([
                "--ignored",
                "--exact",
                CHILD_TEST,
                "--nocapture",
                "--quiet",
                "--test-threads=1",
            ])
        })
        .and_then(|spec| spec.with_env(ROLE_ENV, role.name()))
        .map_err(|_| SpawnFailure::BadSpec)
}

/// Why a child process for a test could not be described.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SpawnFailure {
    /// The path of the running test binary is not available.
    NoCurrentExe,
    /// The path was there and would not make a spec, which cannot happen and
    /// is reported rather than unwrapped.
    BadSpec,
}

/// An in-memory writer, and a handle on what it has been given.
///
/// The same shape as the server transport's own capture, duplicated rather
/// than shared: it is a `#[cfg(test)]` item on the other side of the
/// hexagonal boundary, and a test helper with two parents is a test helper
/// neither parent can move.
#[derive(Clone, Default)]
pub(super) struct Capture {
    written: Arc<Mutex<Vec<u8>>>,
}

impl Capture {
    /// A fresh, empty capture.
    pub(super) fn shared() -> Self {
        Self::default()
    }

    /// Everything written so far, as text.
    ///
    /// A copy: a drain would make two accessors answer differently about the
    /// same state, and two ways to ask one question is a way to get two
    /// answers.
    pub(super) fn contents(&self) -> String {
        String::from_utf8_lossy(&self.bytes()).into_owned()
    }

    /// Everything written so far, as bytes.
    pub(super) fn bytes(&self) -> Vec<u8> {
        // Both arms have to produce a copy, not a guard. `PoisonError::into_inner`
        // unwraps the poison wrapper and hands the guard straight back, so using it
        // as the default arm leaves the poisoned case returning the guard the
        // success case had already copied out of.
        self.written.lock().map_or_else(
            |poisoned| Vec::clone(&poisoned.into_inner()),
            |guard| Vec::clone(&guard),
        )
    }
}

impl AsyncWrite for Capture {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        // The `mut` belongs here rather than on the pattern binding: it is the
        // guard held past this `match` that needs one, to reach `extend_from_slice`
        // through `DerefMut`. On the binding it would be both redundant (the arm
        // only moves the guard out) and not enough (the guard is not `mut` there).
        let mut written = match self.written.lock() {
            Ok(written) => written,
            // A poisoned lock means another thread panicked holding it. The
            // buffer is still a `Vec<u8>` and still the truth about what was
            // written, so the capture carries on rather than taking a test
            // down with a test.
            Err(poisoned) => poisoned.into_inner(),
        };
        written.extend_from_slice(buf);
        // Released here rather than at the end of the function, which is where
        // the binding's scope would otherwise put it: the bytes are copied and
        // the guard is not wanted again, so a writer that held the lock across
        // the return would be holding it while its caller decides what to do.
        drop(written);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// A stream split into whole lines, on whichever side needs them.
///
/// The child reads with this because a read from a pipe returns whatever is in
/// the buffer, and a request that arrives in two pieces is the same fact a
/// client has to reassemble. `next_line` keeps the terminator, so what it
/// yields is the frame the parent reads back.
#[derive(Debug, Default)]
struct LineReader {
    buffered: Vec<u8>,
}

impl LineReader {
    /// Takes the next whole line, if the buffer holds one.
    fn next_line(&mut self) -> Option<String> {
        let newline = self.buffered.iter().position(|byte| *byte == b'\n')?;
        let line: Vec<u8> = self.buffered.drain(..=newline).collect();
        String::from_utf8(line).ok()
    }

    /// Hands over whatever a read produced.
    fn push(&mut self, bytes: &[u8]) {
        self.buffered.extend_from_slice(bytes);
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Read as _, Write};

    use serde_json::Value;

    use tokio::io::AsyncWriteExt;

    use super::super::request::{reply_result, request_document};
    use super::{Capture, LineReader, Role, STDERR_MARKER, SpawnFailure, child_spec};
    use crate::adapters::secondary::stdio::RequestError;
    use crate::domain::RequestId;
    use crate::domain::ir::{JsonObject, MethodName, MethodNameError};

    /// What a `?` in a test here can produce.
    #[derive(Debug)]
    #[expect(dead_code, reason = "the payloads are read by the derived Debug")]
    enum Failure {
        /// A stream under the test failed.
        Io(io::Error),
        /// A child could not be described.
        Spec(SpawnFailure),
        /// A document this module built did not read back.
        Request(RequestError),
        /// A method name the IR would not admit.
        Method(MethodNameError),
    }

    impl From<io::Error> for Failure {
        fn from(error: io::Error) -> Self {
            Self::Io(error)
        }
    }

    impl From<SpawnFailure> for Failure {
        fn from(error: SpawnFailure) -> Self {
            Self::Spec(error)
        }
    }

    impl From<RequestError> for Failure {
        fn from(error: RequestError) -> Self {
            Self::Request(error)
        }
    }

    impl From<MethodNameError> for Failure {
        fn from(error: MethodNameError) -> Self {
            Self::Method(error)
        }
    }

    /// The child server, re-executed by the tests in `client.rs`.
    ///
    /// `#[ignore]`d so an ordinary `cargo test` skips it, and a no-op without
    /// `ROLE_ENV` so that running it by hand changes nothing. It leaves
    /// through [`exit`] rather than by returning, because libtest's closing
    /// lines land on the same stdout the parent reads as frames — and a test
    /// that had to filter harness output out of every frame would be filtering
    /// it out of the assertions too.
    #[test]
    #[ignore = "spawned as a child process by the stdio client tests"]
    fn stdio_child_server() {
        let Some(role) = Role::from_env() else {
            return;
        };
        if role == Role::Exit {
            exit();
        }
        if role == Role::Stderr {
            let mut stderr = std::io::stderr();
            let marker = format!("{STDERR_MARKER}\n");
            if stderr.write_all(marker.as_bytes()).is_err() || stderr.flush().is_err() {
                exit();
            }
        }

        let mut stdin = std::io::stdin().lock();
        let mut stdout = std::io::stdout().lock();
        let mut reader = LineReader::default();
        let mut buffer = [0_u8; 4 * 1024];
        loop {
            while let Some(line) = reader.next_line() {
                if role == Role::Silent {
                    // Reading is the point; writing is what this role refuses
                    // to do.
                    continue;
                }
                let Some(reply) = answer(&line, role) else {
                    continue;
                };
                if write_reply(&mut stdout, &reply, role).is_err() {
                    exit();
                }
                if role == Role::Once {
                    exit();
                }
            }
            match stdin.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let Some(chunk) = buffer.get(..read) else {
                        break;
                    };
                    reader.push(chunk);
                }
            }
        }
        exit();
    }

    /// Writes one reply, in one write or in two.
    ///
    /// The flush after the first half of a split reply is the whole reason
    /// the split is observable: `stdout` is line-buffered, so a half-reply
    /// with no newline would sit in the buffer and reach the parent as one
    /// write, testing nothing.
    fn write_reply(out: &mut impl Write, reply: &str, role: Role) -> io::Result<()> {
        if role != Role::Split {
            return out.write_all(reply.as_bytes()).and_then(|()| out.flush());
        }
        let half = reply.len() / 2;
        let (first, second) = reply.split_at(half);
        out.write_all(first.as_bytes())?;
        out.flush()?;
        out.write_all(second.as_bytes())?;
        out.flush()
    }

    /// The reply the child writes for one request line.
    ///
    /// Deliberately not this crate's own `reply` builders: a child that built
    /// its answers with the code under test would pass a test where the client
    /// and the server had agreed on a shape no peer would send. The id is
    /// carried through as a parsed value, which is also what makes a string id
    /// come back as the same string id.
    fn answer(line: &str, role: Role) -> Option<String> {
        let request: Value = serde_json::from_str(line).ok()?;
        let id = request.get("id")?.clone();
        let method = request.get("method").and_then(Value::as_str)?;
        let result = serde_json::json!({
            "child": true,
            "method": method,
            "role": role.name(),
        });
        let document = serde_json::json!({
            "id": id,
            "jsonrpc": "2.0",
            "result": result,
        });
        // The framing is newline-delimited, so the terminator is part of the
        // document rather than a courtesy the writer adds afterwards. Without
        // it the parent's decoder never yields this frame and the call waits
        // for a reply that has been written in full.
        Some(format!("{document}\n"))
    }

    /// Leaves without giving libtest a chance to write.
    fn exit() -> ! {
        std::process::exit(0)
    }

    /// The capture sees exactly what was written — including the marker the
    /// stderr tests search for, which is the assertion that it is a marker
    /// nobody writes by accident.
    #[tokio::test]
    async fn a_capture_sees_exactly_what_was_written() -> Result<(), Failure> {
        let capture = Capture::shared();
        let mut writer = capture.clone();

        writer.write_all(STDERR_MARKER.as_bytes()).await?;

        assert_eq!(capture.contents(), STDERR_MARKER);
        Ok(())
    }

    /// A line reader that split on the wrong byte would make every
    /// reassembly test here pass for the wrong reason.
    #[test]
    fn a_line_reader_only_yields_whole_lines() {
        let mut reader = LineReader::default();
        reader.push(b"first\nsec");

        assert_eq!(reader.next_line().as_deref(), Some("first\n"));
        assert_eq!(reader.next_line(), None);
    }

    /// Every role has to be reachable through the one filter that selects the
    /// child, so a role that could not be named would be a role no test could
    /// reach.
    #[test]
    fn every_role_becomes_a_child_spec() -> Result<(), Failure> {
        for role in Role::all() {
            child_spec(role)?;
        }
        Ok(())
    }

    /// A reply this crate can read back. If the child ever stopped writing a
    /// shape [`reply_result`] accepts, every test that reads a frame would
    /// fail for the same uninformative reason; asserting it here names the
    /// breakage.
    #[test]
    fn the_childs_answer_is_a_reply_this_crate_reads() -> Result<(), Failure> {
        let method = MethodName::new("tools/list")?;
        let id = RequestId::text("r-1");
        let request = request_document(&id, &method, &JsonObject::default())?.to_string();

        let reply = answer(&request, Role::Echo).ok_or(Failure::Spec(SpawnFailure::BadSpec))?;
        let result = reply_result(&reply, &id)?;

        assert_eq!(
            result.as_str(),
            r#"{"child":true,"method":"tools/list","role":"echo"}"#
        );
        Ok(())
    }
}
