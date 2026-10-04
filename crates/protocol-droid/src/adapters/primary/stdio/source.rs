//! Reading whole frames off a peer's stream.
//!
//! # The two `Ok(None)`s
//!
//! Two different `None`s meet in this file and they mean opposites.
//!
//! [`FrameDecoder::next_frame`] answers `Ok(None)` for **"not ready — the
//! terminator has not arrived"**. A pipe splits wherever it likes, so that
//! answer is the normal case: most reads return a partial line. It is not an
//! end condition, and a caller that treated it as one would drop every frame
//! that did not happen to land on a read boundary.
//!
//! [`FrameSource::next_frame`] answers `Ok(None)` for **"the peer closed"**, and
//! nothing else. It is the only signal the loop above has that the peer is
//! gone, so it must never be produced by a decoder that simply has nothing yet.
//!
//! Forwarding the decoder's answer straight up is therefore the one mistake
//! this module exists to not make, and it is a quiet one: a transport built
//! that way works perfectly against a peer that frames its writes to match the
//! read buffer, and silently truncates the conversation against one that does
//! not. So [`StdioSource::next_frame`] loops internally — it reads more bytes,
//! checks the decoder again, and only resolves when a frame is complete, the
//! stream has ended, or a read has failed. It never returns the decoder's
//! `None`.
//!
//! # A failure is latched
//!
//! Once a read fails, every later call answers
//! [`SourceError::Transport`] without touching the stream. The alternative —
//! retrying, and letting a later read return `Ok(0)` — would let a broken
//! connection quietly present as a clean close, and a caller that distinguishes
//! a peer that left from a peer that died would be told the wrong one. There is
//! no recovery path here because a pipe that failed mid-read is not
//! resynchronisable: the bytes already consumed are gone.
//!
//! # What the loop guarantees at end-of-stream
//!
//! End-of-stream is only ever reported once the decoder has nothing complete in
//! it, because the check for a complete frame runs before every read. A frame
//! the peer wrote and terminated is therefore never lost to a close that
//! arrived in the same breath. A frame the peer wrote and did *not* terminate is
//! dropped, which is the only defensible reading: there is no length on a
//! newline to wait for, and reporting the partial bytes as a message would hand
//! the next layer something the peer never sent.

use core::future::Future;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::Mutex;

use crate::application::jsonrpc::{FrameSource, SourceError};
use crate::domain::{FrameDecoder, MaxFrameSize};

use super::StdioConfig;

/// The mutable half of a [`StdioSource`], behind one lock.
///
/// Stream and decoder share a single lock rather than one each because they
/// are one resource seen from two angles: a read that returns bytes and a
/// decoder that consumes them are a single step, and two locks would let a
/// concurrent caller interleave between them. It also settles the question of
/// which state is authoritative — there is exactly one.
#[derive(Debug)]
struct Inner<R> {
    stream: R,
    decoder: FrameDecoder,
    /// Reused across reads so a long-lived session allocates its buffer once.
    buf: Vec<u8>,
    /// `read` returned zero: the peer is gone and this stays true.
    ended: bool,
    /// A read failed. Latched, so a failure never presents as a close.
    failed: bool,
}

/// A [`FrameSource`] over any `AsyncRead`.
///
/// # Examples
///
/// A frame split across two reads is reassembled, because the partial
/// `Ok(None)` from the decoder is a reason to read again rather than an answer:
///
/// ```
/// use std::pin::Pin;
/// use std::task::{Context, Poll};
///
/// use protocol_droid::adapters::primary::stdio::{StdioConfig, StdioError, StdioSource};
/// use protocol_droid::application::jsonrpc::FrameSource;
/// use protocol_droid::domain::MaxFrameSize;
/// use tokio::io::{AsyncRead, ReadBuf};
///
/// /// Hands over at most three bytes per read, so a frame cannot arrive whole.
/// struct Trickle {
///     data: Vec<u8>,
///     served: usize,
/// }
///
/// impl AsyncRead for Trickle {
///     fn poll_read(
///         mut self: Pin<&mut Self>,
///         _cx: &mut Context<'_>,
///         buf: &mut ReadBuf<'_>,
///     ) -> Poll<std::io::Result<()>> {
///         let end = (self.served + 3).min(self.data.len());
///         let Some(chunk) = self.data.get(self.served..end) else {
///             return Poll::Ready(Ok(())); // end of stream
///         };
///         buf.put_slice(chunk);
///         self.served = end;
///         Poll::Ready(Ok(()))
///     }
/// }
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let stream = Trickle { data: b"{\"jsonrpc\":\"2.0\"}\n".to_vec(), served: 0 };
/// let source = StdioSource::new(stream, StdioConfig::new(MaxFrameSize::new(1024)?));
///
/// assert_eq!(source.next_frame().await?, Some(r#"{"jsonrpc":"2.0"}"#.to_owned()));
/// # Ok(())
/// # }
/// # fn _unused(e: StdioError) {}
/// ```
#[derive(Debug)]
pub struct StdioSource<R> {
    inner: Mutex<Inner<R>>,
    config: StdioConfig,
}

impl<R> StdioSource<R> {
    /// Wraps a stream, decoding frames at the configured ceiling.
    #[must_use]
    pub fn new(stream: R, config: StdioConfig) -> Self {
        let buf = vec![0; config.read_chunk().get()];
        Self {
            inner: Mutex::new(Inner {
                stream,
                decoder: FrameDecoder::new(config.limit()),
                buf,
                ended: false,
                failed: false,
            }),
            config,
        }
    }

    /// The settings this source was built with.
    #[must_use]
    pub const fn config(&self) -> StdioConfig {
        self.config
    }

    /// The ceiling this source refuses frames above.
    #[must_use]
    pub const fn limit(&self) -> MaxFrameSize {
        self.config.limit()
    }
}

impl<R> StdioSource<R>
where
    R: AsyncRead + Unpin,
{
    /// Reads the peer's next frame, waiting for as many reads as that takes.
    ///
    /// This is the read loop, and the reason this type exists. Every exit from
    /// it is a decision, and the decoder's `None` is not one of them.
    ///
    /// # Errors
    ///
    /// [`SourceError::Transport`] when a read fails, which is latched, and
    /// [`SourceError::Framing`] when the peer's bytes are not a frame — most
    /// often [`crate::domain::FramingError::FrameTooLarge`], or bytes that are
    /// not UTF-8.
    ///
    /// `Ok(None)` is the peer closing and only that.
    ///
    /// The lock is held for the whole loop, which is the point of holding one:
    /// two callers must not interleave a read with the decoder that consumes
    /// what it returned. Every exit from the loop is still holding it, so the
    /// guard has no earlier last use to be tightened to.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the guard is the mutual exclusion for the entire read loop, and every path out of that loop is still holding it"
    )]
    pub async fn next_frame(&self) -> Result<Option<String>, SourceError> {
        let mut guard = self.inner.lock().await;
        // Destructured so the stream and the read buffer are separate mutable
        // borrows rather than two claims on the same guard.
        let Inner {
            stream,
            decoder,
            buf,
            ended,
            failed,
        } = &mut *guard;

        // Latched before anything else: a source that has already failed must
        // not go back to the stream and risk a later `Ok(0)` reading as a
        // clean close.
        if *failed {
            return Err(SourceError::Transport);
        }

        loop {
            // The decoder's `None` means "no terminator yet". Checking here, at
            // the top of every pass, is also what makes the end-of-stream path
            // below safe: the stream is only ever read once the decoder has been
            // asked and had nothing.
            if let Some(frame) = decoder.next_frame()? {
                return Ok(Some(frame.to_owned()));
            }

            // Already saw the close, and the decoder had nothing, so the peer
            // is gone and there is nothing left to wait for.
            if *ended {
                return Ok(None);
            }

            let Ok(read) = stream.read(buf.as_mut_slice()).await else {
                *failed = true;
                return Err(SourceError::Transport);
            };

            if read == 0 {
                *ended = true;
                return Ok(None);
            }

            let Some(bytes) = buf.get(..read) else {
                // `read` is bounded by the slice it was given, so this is
                // unreachable. Latching the failure anyway costs nothing and
                // keeps the alternative — indexing, or a silently dropped
                // frame — off the table.
                *failed = true;
                return Err(SourceError::Transport);
            };
            decoder.push(bytes)?;
        }
    }
}

impl<R> FrameSource for StdioSource<R>
where
    R: AsyncRead + Unpin + Send,
{
    /// Waits for the peer's next frame.
    ///
    /// The port's `Ok(None)` is the peer closing, so this never returns the
    /// decoder's "no terminator yet" — see the module docs for why the two
    /// `None`s are opposites and what inverting them looks like from the
    /// outside.
    ///
    /// A framing error is *not* latched: the decoder refused the bytes and kept
    /// its own buffer, so the next call is refused again by the same check
    /// rather than by a flag, and the stream stays open for the caller to shut
    /// down.
    ///
    /// # Errors
    ///
    /// As [`StdioSource::next_frame`].
    fn next_frame(&self) -> impl Future<Output = Result<Option<String>, SourceError>> + Send {
        Self::next_frame(self)
    }
}

#[cfg(test)]
mod tests {
    use core::pin::Pin;
    use core::sync::atomic::{AtomicUsize, Ordering};
    use core::task::{Context, Poll};
    use std::io;
    use std::sync::Arc;

    use assert_matches::assert_matches;

    use tokio::io::{AsyncRead, ReadBuf};

    use super::{StdioConfig, StdioSource};
    use crate::application::jsonrpc::{DynFrameSource, SourceError};
    use crate::domain::{FrameDecoder, FramingError, MaxFrameSize, MaxFrameSizeError};

    /// The `Failure` an `?` in a test can produce, so a fallible call whose
    /// error type is not the one under test still propagates instead of
    /// unwrapping. Without it every test below would need an `unwrap` at the
    /// boundary, which the project rules forbid in tests too.
    #[derive(Debug)]
    #[expect(dead_code, reason = "the payloads are read by the derived Debug")]
    enum Failure {
        /// A source refused to produce the frame the test expected.
        Source(SourceError),
        /// A limit the test tried to build was refused.
        Limit(MaxFrameSizeError),
    }

    impl From<SourceError> for Failure {
        fn from(error: SourceError) -> Self {
            Self::Source(error)
        }
    }

    impl From<MaxFrameSizeError> for Failure {
        fn from(error: MaxFrameSizeError) -> Self {
            Self::Limit(error)
        }
    }

    /// A reader that hands over at most `per_read` bytes at a time, can be made
    /// to fail on demand, and counts how many reads it served.
    ///
    /// A real pipe is the one thing a test cannot ask to split a particular
    /// way, and "split across two reads" is the whole property under test. This
    /// is the deterministic stand-in, and it is also how the read-failure case
    /// is produced without a socket that someone has to break.
    struct Trickle {
        data: Vec<u8>,
        served: usize,
        per_read: usize,
        /// Fail once this many bytes have been handed over.
        fail_after: Option<usize>,
        reads: Arc<AtomicUsize>,
    }

    impl Trickle {
        fn new(data: &[u8], per_read: usize) -> (Self, Arc<AtomicUsize>) {
            let reads = Arc::new(AtomicUsize::new(0));
            (
                Self {
                    data: data.to_vec(),
                    served: 0,
                    per_read,
                    fail_after: None,
                    reads: Arc::clone(&reads),
                },
                reads,
            )
        }

        fn failing(data: &[u8], per_read: usize) -> Self {
            Self {
                fail_after: Some(0),
                ..Self::new(data, per_read).0
            }
        }
    }

    impl AsyncRead for Trickle {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let this = self.get_mut();
            this.reads.fetch_add(1, Ordering::Relaxed);

            if this.fail_after.is_some_and(|limit| this.served >= limit) {
                return Poll::Ready(Err(io::Error::other("injected read failure")));
            }

            let remaining = this.data.len().saturating_sub(this.served);
            if remaining == 0 {
                // End of stream: leave the buffer empty so `read` answers zero.
                return Poll::Ready(Ok(()));
            }

            let end = this
                .served
                .saturating_add(this.per_read)
                .min(this.data.len());
            let Some(chunk) = this.data.get(this.served..end) else {
                return Poll::Ready(Ok(()));
            };
            buf.put_slice(chunk);
            this.served = end;
            Poll::Ready(Ok(()))
        }
    }

    /// A source over a [`Trickle`], plus the read counter for asserting that a
    /// frame really did span more than one read.
    fn trickling(
        wire: &[u8],
        per_read: usize,
        limit: u32,
    ) -> Result<(StdioSource<Trickle>, Arc<AtomicUsize>), Failure> {
        let (stream, reads) = Trickle::new(wire, per_read);
        let config = StdioConfig::new(MaxFrameSize::new(limit)?).with_read_chunk(per_read);
        Ok((StdioSource::new(stream, config), reads))
    }

    /// The property the module exists for: a frame that arrives in pieces is
    /// reassembled rather than truncated, because the decoder's "no terminator
    /// yet" is not forwarded up as an answer.
    #[tokio::test]
    async fn a_frame_split_across_several_reads_reassembles() -> Result<(), Failure> {
        let document = r#"{"jsonrpc":"2.0","id":1,"result":{}}"#;
        let mut wire = document.as_bytes().to_vec();
        wire.push(b'\n');

        // Four bytes at a time, so a 38-byte frame cannot arrive whole.
        let (source, reads) = trickling(&wire, 4, 1024)?;

        assert_eq!(source.next_frame().await?, Some(document.to_owned()));
        assert!(
            reads.load(Ordering::Relaxed) > 1,
            "the frame must have spanned more than one read for this test to mean anything"
        );
        Ok(())
    }

    /// Two frames in one read, then a close. The close must not cost the second
    /// frame, which is the case that a "read, then check" loop gets wrong when
    /// the end-of-stream check runs before the decoder check.
    #[tokio::test]
    async fn a_frame_buffered_before_the_close_is_still_delivered() -> Result<(), Failure> {
        let (source, _) = trickling(b"one\ntwo\n", 8, 1024)?;

        assert_eq!(source.next_frame().await?, Some("one".to_owned()));
        assert_eq!(source.next_frame().await?, Some("two".to_owned()));
        assert_eq!(source.next_frame().await?, None);
        Ok(())
    }

    /// End of stream is `Ok(None)`, and it stays `Ok(None)` on every later call
    /// rather than turning into an error or a second close of a different kind.
    #[tokio::test]
    async fn a_clean_end_of_stream_is_a_close_and_stays_one() -> Result<(), Failure> {
        let (source, _) = trickling(b"{\"jsonrpc\":\"2.0\"}\n", 64, 1024)?;

        assert!(source.next_frame().await?.is_some());
        assert_eq!(source.next_frame().await?, None);
        assert_eq!(source.next_frame().await?, None);
        Ok(())
    }

    /// A read failure is [`SourceError::Transport`], not a close. The two are
    /// opposites and the caller above is entitled to the difference.
    #[tokio::test]
    async fn a_read_failure_is_a_transport_error() -> Result<(), Failure> {
        let config = StdioConfig::new(MaxFrameSize::new(1024)?);
        let source = StdioSource::new(Trickle::failing(b"ignored\n", 8), config);

        assert_matches!(source.next_frame().await, Err(SourceError::Transport));
        Ok(())
    }

    /// The latch. A source that has failed keeps reporting the failure without
    /// touching the stream again, so a later `Ok(0)` can never present a dead
    /// connection as a peer that left politely.
    #[tokio::test]
    async fn a_failed_read_stays_failed_and_never_becomes_a_clean_close() -> Result<(), Failure> {
        let config = StdioConfig::new(MaxFrameSize::new(1024)?);
        let source = StdioSource::new(Trickle::failing(b"ignored\n", 8), config);

        for _ in 0..3 {
            assert_matches!(source.next_frame().await, Err(SourceError::Transport));
        }
        Ok(())
    }

    /// The ceiling applies to what a peer sends, reported as the framing error
    /// the domain already defines rather than as a transport failure.
    #[tokio::test]
    async fn an_oversized_inbound_frame_is_refused() -> Result<(), Failure> {
        // A 64-byte ceiling against a 200-byte line.
        let (source, _) = trickling(&[b'x'; 200], 16, 64)?;

        assert_matches!(
            source.next_frame().await,
            Err(SourceError::Framing(FramingError::FrameTooLarge { .. }))
        );
        Ok(())
    }

    /// A line the peer never terminated is not a frame, and reporting it as one
    /// would hand the next layer a message the peer did not send.
    #[tokio::test]
    async fn an_unterminated_trailing_line_is_not_a_frame() -> Result<(), Failure> {
        let (source, _) = trickling(b"{\"jsonrpc\":\"2.0\"}", 64, 1024)?;

        assert_eq!(source.next_frame().await?, None);
        Ok(())
    }

    /// Bytes that are not UTF-8 are refused rather than replaced, so the
    /// corruption surfaces as an encoding error instead of as a routing one.
    #[tokio::test]
    async fn non_utf8_bytes_are_refused() -> Result<(), Failure> {
        let (source, _) = trickling(&[0xFF, 0xFE, b'\n'], 8, 1024)?;

        assert_matches!(
            source.next_frame().await,
            Err(SourceError::Framing(FramingError::NotUtf8 { .. }))
        );
        Ok(())
    }

    /// The blanket impl in the port module covers the object-safe spelling, so
    /// a caller holding `Arc<dyn DynFrameSource>` gets a source without this
    /// adapter writing a second impl that would collide with it.
    #[tokio::test]
    async fn the_source_is_usable_behind_a_trait_object() -> Result<(), Failure> {
        let (source, _) = trickling(b"behind-a-box\n", 16, 1024)?;
        let boxed: Arc<dyn DynFrameSource> = Arc::new(source);

        assert_eq!(boxed.next_frame().await?, Some("behind-a-box".to_owned()));
        Ok(())
    }

    /// The decoder the loop drives is the real one, and a source that has read
    /// nothing holds no bytes — the two facts together are what "a `None` here
    /// means not-ready" is allowed to assume.
    #[test]
    fn the_framing_is_the_domain_decoder_not_a_local_copy() {
        let mut decoder =
            FrameDecoder::new(MaxFrameSize::new(1024).unwrap_or(MaxFrameSize::DEFAULT));
        assert!(decoder.is_empty());
        assert_matches!(decoder.next_frame(), Ok(None));
    }
}
