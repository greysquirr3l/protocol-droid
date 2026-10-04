//! The process: spawned once, drained on stderr, killed on drop.
//!
//! # `kill_on_drop` is the whole point of this module
//!
//! `tokio::process::Child` does **not** kill the process it is dropped with.
//! That is not a detail of this crate's design; it is the default in `tokio`,
//! and it is the correct default for a `Child` whose whole job is to be waited
//! on. A stdio client is not that: it holds a server that has no other reason
//! to exist, so dropping the client has to take the server with it. A client
//! that failed halfway through a handshake and returned an error would
//! otherwise leave a process running with nobody reading its stdout — an
//! orphan holding a pipe, holding whatever the user asked it to do, for as
//! long as the machine stays up.
//!
//! So [`spawn`] sets `kill_on_drop(true)` before the `Command` is ever
//! executed, and it is the only function in the crate that spawns a child. One
//! place to look, one flag to audit.
//!
//! On Unix the flag alone leaves the killed child to be reaped, and that is
//! tokio's own business rather than this crate's: `process` compiles the
//! `SIGCHLD` machinery for a Unix target whether or not the public `signal`
//! feature is enabled (`cfg_signal_internal!` in tokio's `macros/cfg.rs` is
//! `any(feature = "signal", all(unix, feature = "process"))`), so the reaper is
//! always present here and the dependency stays at `process`.
//!
//! # Where the child's stderr goes
//!
//! A server logs to stderr because stdout is the conversation, and a client
//! that discarded that stream would deadlock: a child writing more than a pipe
//! buffer's worth of log output blocks on a full pipe and stops answering. So
//! [`forward_stderr`] drains it continuously and hands it to
//! [`Diagnostics`](crate::adapters::primary::stdio::Diagnostics), one line at
//! a time.
//!
//! One line at a time is the part worth arguing. A read from a pipe returns
//! whatever happens to be in the buffer, so forwarding the read directly would
//! write log lines in whatever sizes the kernel chose — a diagnostic channel
//! that interleaves its own lines with itself. Accumulating to the next
//! newline costs one `Vec` per child and makes the log the log rather than a
//! stream of fragments.
//!
//! Nothing here writes to the protocol. There is no path from this module's
//! stderr handling to a [`StdioSink`](crate::adapters::primary::stdio::StdioSink),
//! and there is not going to be one.
//!
//! # Examples
//!
//! The whole lifecycle, which is what a caller of
//! [`StdioClient::spawn`](super::StdioClient::spawn) is doing under the hood:
//!
//! ```no_run
//! use protocol_droid::adapters::secondary::stdio::{ChildSpec, ClientConfig, StdioClient};
//! use protocol_droid::adapters::primary::stdio::{Diagnostics, StdioConfig};
//! use protocol_droid::domain::MaxFrameSize;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let spec = ChildSpec::new("my-server")?;
//! let config = ClientConfig::new(StdioConfig::new(MaxFrameSize::new(1024 * 1024)?));
//!
//! let client = StdioClient::spawn(&spec, config, Diagnostics::stderr()).await?;
//! // Dropping `client` here kills the child: `kill_on_drop(true)` was set
//! // before the process was started, so the server does not outlive it.
//! drop(client);
//! # Ok(())
//! # }
//! ```

use std::process::Stdio;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWrite};
use tokio::process::{Child, ChildStderr};
use tokio::task::JoinHandle;

use crate::adapters::primary::stdio::Diagnostics;

use super::spec::ChildSpec;

/// How many bytes one read of the child's stderr may ask for.
///
/// A read from a pipe returns whatever is in the buffer, so this bounds the
/// copy rather than promising a size. Large enough that a chatty server is not
/// read in slivers, small enough that no child carries a kilobyte of log
/// buffer per connection.
const STDERR_READ_CHUNK: usize = 4 * 1024;

/// A freshly spawned child's process handle and the three pipes it was given.
#[derive(Debug)]
pub(crate) struct SpawnedChild {
    /// The process. Dropping it kills the process, because [`spawn`] said so.
    pub(crate) child: Child,
    /// The child's stdin, for writing frames to.
    pub(crate) stdin: Option<tokio::process::ChildStdin>,
    /// The child's stdout, for reading frames from.
    pub(crate) stdout: Option<tokio::process::ChildStdout>,
    /// The child's stderr, which is never a frame.
    pub(crate) stderr: Option<ChildStderr>,
}

/// Starts a child server, with all three of its standard streams piped.
///
/// The pipes are requested here rather than in [`ChildSpec::to_command`]
/// because a spec describes a program and a client describes a conversation,
/// and the conversation needs a stdin and a stdout to have a conversation on.
/// Both halves are taken out of the returned [`Child`] immediately, so the
/// process handle is left holding nothing but the handle.
///
/// # Errors
///
/// [`SpawnError::NotStarted`] when the program could not be started at all —
/// no such file, not executable, out of processes. The pipe requests cannot
/// fail, so this is the only way the call can.
pub(crate) fn spawn(spec: &ChildSpec) -> Result<SpawnedChild, SpawnError> {
    let mut command = spec.to_command();
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // The load-bearing line. See the module docs.
        .kill_on_drop(true);
    // `tokio::process::Command::spawn` is synchronous — the async surface of
    // the process API is the child's I/O and its `wait`, not the fork.
    let mut child = command.spawn().map_err(|_| SpawnError::NotStarted)?;
    Ok(SpawnedChild {
        stdin: child.stdin.take(),
        stdout: child.stdout.take(),
        stderr: child.stderr.take(),
        child,
    })
}

/// Drains the child's stderr into a diagnostics channel, and keeps draining it.
///
/// Returns the task's handle, which completes when the pipe reaches
/// end-of-file. That is worth having rather than discarding: end-of-file on
/// the child's stderr is the kernel's own account of the child exiting, and a
/// caller that needs to know the process is gone — a test proving a dropped
/// client took its server with it, say — can await this and be told by the
/// operating system rather than by a handle's `Drop` having run.
///
/// The handle is deliberately not awaited here. Nothing else has to happen
/// before the log is written, and a forwarder that blocked the spawn until the
/// child exited would be a child that had to exit before it could be used.
///
/// # Panics
///
/// Never. The task swallows the stream's own failures: a child's stderr that
/// cannot be read, or a diagnostics channel that refuses a write, ends the
/// forwarder. Neither is a fault in the conversation, and a task that could
/// not report them would take the runtime's panic handler with it.
pub(crate) fn forward_stderr<DW>(
    mut stderr: ChildStderr,
    diagnostics: Arc<Diagnostics<DW>>,
) -> JoinHandle<()>
where
    DW: AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buffer = vec![0; STDERR_READ_CHUNK];
        let mut pending: Vec<u8> = Vec::new();
        loop {
            let read = match stderr.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            let Some(chunk) = buffer.get(..read) else {
                break;
            };
            pending.extend_from_slice(chunk);
            // Whole lines only: a diagnostic interleaved with another
            // diagnostic is a corrupted log, and a corrupted log is the reason
            // people turn logging off.
            while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
                let line: Vec<u8> = pending.drain(..=newline).collect();
                if diagnostics.write_all(&line).await.is_err() {
                    return;
                }
            }
        }
        // Whatever the child wrote after its last newline still belongs in the
        // log. Dropping it would lose the last thing a server said before it
        // died, which is usually the reason anybody is reading.
        //
        // A failed write is discarded rather than returned on: this is the
        // forwarder's last act, so there is nothing a caller could do with the
        // failure and nowhere left to send it.
        if !pending.is_empty() {
            let _ = diagnostics.write_all(&pending).await;
        }
    })
}

/// Why a child could not be started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SpawnError {
    /// The program could not be started.
    ///
    /// Carries no `io::Error`, for the reason [`StdioError`] does: the caller
    /// in a position to tell "no such file" from "not executable" is the one
    /// that has already failed to spawn, and it can report the program's own
    /// refusal. A `String` here would put a path this process invented onto
    /// the error path of every caller above it.
    ///
    /// [`StdioError`]: crate::adapters::primary::stdio::StdioError
    #[error("the child process could not be started")]
    NotStarted,

    /// The child started, but the platform did not give it one of the three
    /// pipes.
    ///
    /// Not a failure the spawn can detect — `Stdio::piped()` succeeding and
    /// the handle coming back without the stream is not a thing that happens —
    /// and reported because the alternative on the only path that can observe
    /// it is an `unwrap`, and an `unwrap` on a path that runs for every client
    /// a caller ever spawns is a panic waiting for a platform nobody tested.
    #[error("the child process was started without a {stream} pipe")]
    NoPipe {
        /// Which of the three standard streams the platform withheld.
        stream: &'static str,
    },
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;

    use super::SpawnError;

    /// A failure this module reports is not the same failure twice, so a test
    /// that cannot tell them apart is testing nothing.
    #[test]
    fn the_two_spawn_failures_are_distinguishable() {
        assert_ne!(
            SpawnError::NotStarted,
            SpawnError::NoPipe { stream: "stdout" }
        );
    }

    /// The withheld-stream variant names the stream, because "a pipe" is not
    /// an answer a caller can act on and the three are not interchangeable.
    #[test]
    fn a_withheld_pipe_names_its_stream() {
        let error = SpawnError::NoPipe { stream: "stderr" };

        assert!(error.to_string().contains("stderr"), "{error}");
        assert_matches!(error, SpawnError::NoPipe { .. });
    }
}
