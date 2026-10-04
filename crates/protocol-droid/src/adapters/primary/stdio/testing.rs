//! Streams a test can read back.
//!
//! # Why this is a module and not a type inside one test
//!
//! Three of the adapter's tests need the same thing: a writer that keeps what it
//! was given so a test can look at it afterwards. Filing it under whichever
//! module happened to need it first would make the other two reach sideways
//! into a sibling's `#[cfg(test)]` items, which is the arrangement that makes a
//! module look testable and then stops compiling the day a test moves.
//!
//! # Why the handle is separate from the writer
//!
//! [`Capture::shared`] hands back a cheap clone. The writer is consumed by
//! whichever type is under test — [`StdioSink`](super::StdioSink) or
//! [`Diagnostics`](super::Diagnostics) — and the test still needs to read what
//! it wrote. One buffer behind an `Arc`, read through a clone, is the whole
//! mechanism, and it costs nothing to take the handle before the writer is
//! handed away.
//!
//! # Reads do not consume
//!
//! Both accessors return a copy and leave the buffer alone. A drain would make
//! `contents()` and `bytes()` answer differently about the same state, and two
//! ways to ask one question is a way to get two answers.

use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};

use tokio::io::{AsyncWrite, ReadBuf};

/// An in-memory writer, and a handle on what it has been given.
///
/// Cheap to clone: the clone shares the buffer, so a clone taken before the
/// writer is handed to a type under test still sees everything written after.
#[derive(Clone, Default)]
pub(super) struct Capture {
    written: Arc<Mutex<Vec<u8>>>,
}

impl Capture {
    /// A fresh, empty capture.
    #[must_use]
    pub(super) fn shared() -> Self {
        Self::default()
    }

    /// Everything written so far, as bytes.
    #[must_use]
    pub(super) fn bytes(&self) -> Vec<u8> {
        self.written.lock().map_or_else(
            |poisoned| poisoned.into_inner().clone(),
            |written| written.clone(),
        )
    }

    /// Everything written so far, as text.
    ///
    /// Lossy on purpose: this exists so a human reads a sensible failure
    /// message. A test that asserts on the exact bytes calls [`bytes`](Self::bytes).
    #[must_use]
    pub(super) fn contents(&self) -> String {
        String::from_utf8_lossy(&self.bytes()).into_owned()
    }
}

impl AsyncWrite for Capture {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let mut written = this.written.lock().unwrap_or_else(PoisonError::into_inner);
        written.extend_from_slice(buf);
        // The lock goes back before the poll returns, so a caller writing again
        // on its next poll is not held off by this one.
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

/// A writer that refuses every byte.
///
/// The counterpart to [`Capture`]. Without it, "a write failure is reported"
/// can only be tested by breaking a real pipe, and a test that needs a real pipe
/// is a test that fails on a platform nobody remembered to check.
pub(super) struct Broken;

impl AsyncWrite for Broken {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Err(io::Error::other("injected write failure")))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// Kept so the `ReadBuf` in this module's signatures is the real one, and so a
/// future trait impl that needs a reader is not re-deriving the import.
const _: fn(&mut ReadBuf<'_>) = |_| {};

#[cfg(test)]
mod tests {
    use std::io;

    use tokio::io::AsyncWriteExt;

    use super::{Broken, Capture};
    use crate::adapters::primary::stdio::StdioError;

    /// The `Failure` an `?` in a test can produce, so a fallible call whose
    /// error type is not the one under test still propagates instead of
    /// unwrapping. The writes here raise `io::Error`; the channel-level error
    /// is named alongside it so a test driving one can use the same type.
    #[derive(Debug)]
    #[expect(dead_code, reason = "the payloads are read by the derived Debug")]
    enum Failure {
        /// A write failed where the test expected one to land.
        Stdio(StdioError),
        /// The writer itself refused the bytes.
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

    /// The clone is the mechanism, not a convenience: the writer is consumed by
    /// the type under test and the test still has to see the output.
    #[tokio::test]
    async fn a_handle_taken_before_the_write_still_sees_it() -> Result<(), Failure> {
        let capture = Capture::shared();
        let mut writer = capture.clone();

        writer.write_all(b"written").await?;

        assert_eq!(capture.contents(), "written");
        Ok(())
    }

    /// Both accessors are copies and agree. A drain would make `bytes()` and
    /// `contents()` answer different questions about the same state, which is
    /// how a test ends up asserting on nothing.
    #[tokio::test]
    async fn both_accessors_agree_and_neither_consumes() -> Result<(), Failure> {
        let capture = Capture::shared();
        let mut writer = capture.clone();

        writer.write_all(b"bytes").await?;

        assert_eq!(capture.bytes(), b"bytes".to_vec());
        assert_eq!(capture.bytes(), b"bytes".to_vec());
        assert_eq!(capture.contents(), "bytes");
        assert_eq!(capture.contents(), "bytes");
        Ok(())
    }

    /// Successive writes accumulate, because the sink and the diagnostics
    /// channel both rely on this and a capture that kept only the last write
    /// would hide an interleaving bug in the sink's own lock.
    #[tokio::test]
    async fn successive_writes_accumulate() -> Result<(), Failure> {
        let capture = Capture::shared();
        let mut writer = capture.clone();

        writer.write_all(b"one\n").await?;
        writer.write_all(b"two\n").await?;

        assert_eq!(capture.contents(), "one\ntwo\n");
        Ok(())
    }

    /// The injected failure is a real `io::Error`, so a test can tell a broken
    /// stream from a broken expectation.
    #[tokio::test]
    async fn the_broken_writer_refuses() {
        let mut writer = Broken;
        assert!(writer.write_all(b"anything").await.is_err());
    }
}
