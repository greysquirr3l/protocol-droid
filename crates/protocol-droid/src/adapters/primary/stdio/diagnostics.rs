//! The side channel that must never be confused for the protocol stream.
//!
//! # Why stderr and only stderr
//!
//! On stdio, **stdout is the protocol**. Every byte a peer reads from this
//! process is a frame that [`FrameDecoder`](crate::domain::FrameDecoder) has to
//! turn into a message, and a diagnostic line written there is not a frame the
//! peer can use — it is a parse error the peer will report against us, having no
//! way to tell that the thing which confused it was our log line rather than
//! its peer's.
//!
//! So this channel is bound to stderr and there is no constructor that points
//! it anywhere else. That is a stronger property than it looks: the generic
//! `Diagnostics<W>` shape exists so a *test* can hand it a buffer, and the only
//! way to reach the real process streams is [`Diagnostics::stderr`], which has
//! no parameter to get wrong.
//!
//! # What is written, and what is not
//!
//! A frame can carry a token, a session identifier, or an elicitation
//! response, so a caller hands this channel a line it has already composed
//! rather than a frame it wants logged. The bytes cross here and stop: a
//! diagnostic about this transport's bytes *is* the line, and there is no second
//! copy of it elsewhere for a reader to choose between.
//!
//! That is also why nothing underneath is an event-stream facade. A fact about
//! a session is a [`SessionEvent`](crate::application::session::SessionEvent)
//! and reaches an [`EventLog`](crate::application::session::EventLog); a fact
//! about this transport is a line on this channel. A third mechanism spanning
//! the second would carry no fact the first cannot, and the cost of it is not
//! only a dependency — it is a log that disagrees with itself.

use core::pin::Pin;
use core::task::{Context, Poll};

use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex, MutexGuard};

use super::StdioError;

/// A writer bound to one stream, safe to share.
///
/// Generic over the stream so a test can hand it a buffer and read the bytes
/// back. In production that buffer is stderr, and
/// [`Diagnostics::stderr`] is the only constructor that says so.
///
/// # Examples
///
/// ```
/// use protocol_droid::adapters::primary::stdio::Diagnostics;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// // The real channel: process stderr, and nothing else.
/// let diagnostics = Diagnostics::stderr();
/// diagnostics.write_all(b"listening on stdio\n").await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct Diagnostics<W> {
    writer: Mutex<W>,
}

impl Diagnostics<tokio::io::Stderr> {
    /// The real diagnostics channel: this process's stderr.
    ///
    /// The only constructor that names a process stream, and the only one that
    /// can reach stdout — which it cannot, because it takes no argument to say
    /// so.
    #[must_use]
    pub fn stderr() -> Self {
        Self::new(tokio::io::stderr())
    }
}

impl<W> Diagnostics<W> {
    /// Binds the channel to a stream.
    ///
    /// Public because a test needs it and a caller embedding this transport
    /// needs it; neither is a route to stdout by accident, since choosing the
    /// stream is the whole of what this function does.
    #[must_use]
    pub fn new(writer: W) -> Self {
        Self {
            writer: Mutex::new(writer),
        }
    }
}

impl<W> Diagnostics<W>
where
    W: AsyncWrite + Unpin,
{
    /// Writes a diagnostic line, holding the lock for the whole write.
    ///
    /// The lock spans the write rather than the frame boundary because a
    /// diagnostic interleaved with another diagnostic is a corrupted log, and a
    /// corrupted log is the failure mode that makes people turn logging off.
    ///
    /// # Errors
    ///
    /// [`StdioError::Transport`] when the underlying stream refuses the bytes.
    /// A diagnostic that cannot be written is never worth failing a request
    /// over, so callers that treat it as best-effort can discard this.
    pub async fn write_all(&self, bytes: &[u8]) -> Result<(), StdioError> {
        let mut writer = self.writer.lock().await;
        writer
            .write_all(bytes)
            .await
            .map_err(|_| StdioError::Transport)?;
        // Released here rather than at the end of the scope, which is where the
        // guard would otherwise live until the last statement. The channel is
        // shared and the write is the last thing that needs the lock, so a
        // caller writing a burst of diagnostics does not serialise each one
        // behind the previous one's scope.
        drop(writer);
        Ok(())
    }

    /// Borrows the channel as a plain [`AsyncWrite`], for a caller that already
    /// holds an `AsyncWrite` and would otherwise need a wrapper.
    ///
    /// The borrow is the point, and it is why this is not an `AsyncWrite` impl
    /// on `Diagnostics` itself. The `AsyncWrite` methods take `&mut self`, so
    /// implementing the trait on the shared type would mean the channel could
    /// only ever be written by one task at a time and could not be shared at
    /// all — and a diagnostics channel that cannot be shared is not one. The
    /// returned guard holds the lock for as long as the caller keeps it, which
    /// makes [`write_all`](Self::write_all) the same operation spelled
    /// differently rather than a different guarantee.
    ///
    /// A guard is the borrow check made visible: it will not compile if the
    /// caller tries to reach the channel a second time while holding it, which
    /// is the mistake a shared `AsyncWrite` implementation would have hidden
    /// behind a lock.
    pub async fn writer(&self) -> DiagnosticsWriter<'_, W> {
        DiagnosticsWriter {
            guard: self.writer.lock().await,
        }
    }
}

/// A held borrow of a [`Diagnostics`] channel, usable as an [`AsyncWrite`].
#[derive(Debug)]
pub struct DiagnosticsWriter<'a, W> {
    guard: MutexGuard<'a, W>,
}

impl<W> AsyncWrite for DiagnosticsWriter<'_, W>
where
    W: AsyncWrite + Unpin,
{
    /// Writes what it can and reports the short count, as `AsyncWrite` requires.
    ///
    /// A short write is not a corrupted log here for the same reason it is a
    /// normal condition in every `AsyncWrite`: the caller is required to
    /// re-present the remainder, and a diagnostic that gave up mid-line would
    /// report a truncation the caller can see for itself.
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut *self.get_mut().guard).poll_write(cx, buf)
    }

    /// Flushes the underlying stream.
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut *self.get_mut().guard).poll_flush(cx)
    }

    /// Shuts the underlying stream down. Called once, at the end of a session.
    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut *self.get_mut().guard).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use assert_matches::assert_matches;

    use tokio::io::AsyncWriteExt;

    use super::super::StdioError;
    use super::super::testing::{Broken, Capture};
    use super::Diagnostics;

    /// The `Failure` an `?` in a test can produce, so a fallible call whose
    /// error type is not the one under test still propagates instead of
    /// unwrapping. Two of them appear here: the channel's own error, and the
    /// `io::Error` that every `AsyncWriteExt` method raises.
    #[derive(Debug)]
    #[expect(dead_code, reason = "the payloads are read by the derived Debug")]
    enum Failure {
        /// The channel refused a write the test expected to succeed.
        Stdio(StdioError),
        /// The stream under the channel failed before the channel could map it.
        Io(io::Error),
    }

    impl From<StdioError> for Failure {
        fn from(error: StdioError) -> Self {
            Self::Stdio(error)
        }
    }

    impl From<io::Error> for Failure {
        fn from(error: io::Error) -> Self {
            Self::Io(error)
        }
    }

    /// The property the module is tested for: a diagnostic written through the
    /// channel lands in the buffer it was given, byte for byte. A diagnostics
    /// channel that cannot be observed is a channel nobody verifies.
    #[tokio::test]
    async fn a_diagnostic_lands_in_the_injected_buffer() -> Result<(), Failure> {
        let capture = Capture::shared();
        let diagnostics = Diagnostics::new(capture.clone());

        diagnostics
            .write_all(b"peer announced version 2025-06-18\n")
            .await?;

        assert_eq!(capture.contents(), "peer announced version 2025-06-18\n");
        Ok(())
    }

    /// The real channel is constructible and writable. Every other test in this
    /// module hands it a buffer, which leaves this the only exercise of the one
    /// constructor with a process stream behind it — and a break in it would
    /// otherwise reach production unobserved. It writes to stderr, where the
    /// test harness captures it, so the line is noise nobody has to read.
    #[tokio::test]
    async fn the_real_channel_is_constructible_and_writable() -> Result<(), Failure> {
        let diagnostics = Diagnostics::stderr();

        diagnostics
            .write_all(b"diagnostics: the stderr channel is live\n")
            .await?;

        Ok(())
    }

    /// Several writes accumulate rather than replace. A channel that kept only
    /// the last line would pass the single-write test above and lose the history
    /// that made it worth reading.
    #[tokio::test]
    async fn successive_diagnostics_accumulate() -> Result<(), Failure> {
        let capture = Capture::shared();
        let diagnostics = Diagnostics::new(capture.clone());

        diagnostics.write_all(b"first\n").await?;
        diagnostics.write_all(b"second\n").await?;

        assert_eq!(capture.contents(), "first\nsecond\n");
        Ok(())
    }

    /// The same channel is reachable through a borrowed [`AsyncWrite`], so a
    /// caller that already holds an `AsyncWrite` — a logger, a codec's error
    /// sink — can use it without a wrapper. The bytes have to arrive through
    /// that path too, or the guard is a claim the tests never checked.
    #[tokio::test]
    async fn the_channel_also_works_as_a_plain_async_writer() -> Result<(), Failure> {
        let capture = Capture::shared();
        let diagnostics = Diagnostics::new(capture.clone());

        let mut borrowed = diagnostics.writer().await;
        borrowed.write_all(b"through the trait\n").await?;
        borrowed.flush().await?;
        drop(borrowed);

        assert_eq!(capture.contents(), "through the trait\n");
        Ok(())
    }

    /// A short write is not a failure. `AsyncWrite` permits one, the caller is
    /// required to resume from it, and a channel that treated every short
    /// count as a broken stream would report truncation on a healthy pipe.
    #[tokio::test]
    async fn a_short_write_is_reported_as_a_count_not_an_error() -> Result<(), Failure> {
        let capture = Capture::shared();
        let diagnostics = Diagnostics::new(capture.clone());
        let mut borrowed = diagnostics.writer().await;

        borrowed
            .write_all(b"a longer line than the limit allows")
            .await?;

        assert_eq!(capture.contents(), "a longer line than the limit allows");
        Ok(())
    }

    /// A stream that refuses the bytes is a typed error, not a panic and not a
    /// silent drop. `Broken` exists precisely so this needs no real pipe, and
    /// this is the path a closed or full stderr takes in production — where
    /// the caller is told the line did not land, and is free to decide that a
    /// lost diagnostic is not worth failing a request over.
    #[tokio::test]
    async fn a_refused_diagnostic_is_a_typed_error() {
        let diagnostics = Diagnostics::new(Broken);

        let result = diagnostics.write_all(b"never lands\n").await;

        assert_matches!(result, Err(StdioError::Transport));
    }
}
