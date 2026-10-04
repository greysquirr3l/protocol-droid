//! Writing whole frames to a peer's stream.
//!
//! # The write half of the same ceiling
//!
//! A limit on what a server will *read* is half a protection. A handler that
//! answers with more than the peer is willing to receive is the mirror image of
//! a peer that sends more than the server will accept, and
//! [`StdioSink::send`] refuses it with
//! [`StdioError::ReplyTooLarge`](super::StdioError::ReplyTooLarge) rather than
//! writing a line the reader on the other end has to treat as malformed.
//!
//! The refusal happens before a single byte is written. That is the part worth
//! being explicit about: a write that fails halfway is a *truncated frame* on
//! the wire, and a truncated frame in a newline protocol is not an error the
//! peer can even detect, because the terminator simply never arrives and the
//! peer waits for more of a message that was already truncated. A ceiling
//! checked before the write turns that silent corruption into a typed error in
//! this process, where something can be done about it.
//!
//! # Why the framing is the domain's job
//!
//! [`encode_frame`] is the same function the read side's decoder is paired with,
//! so the terminator and the size check cannot drift between the two halves.
//! This module supplies the limit, the stream, and the mapping into
//! [`StdioError`] — and nothing about what a reply *says*, which belongs above
//! it.
//!
//! # Why a lock rather than a borrow
//!
//! [`FrameSink::send`] takes `&self`, so two replies can be in flight at once.
//! A line interleaved with another line is a corrupted stream, and a corrupted
//! stream on a transport whose whole framing is one byte of delimiter is
//! indistinguishable from a protocol bug. The lock spans the whole write for
//! that reason alone.

use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::Mutex;

use crate::application::session::{FrameSink, SinkError};
use crate::domain::{FramingError, MaxFrameSize, encode_frame};

use super::{StdioConfig, StdioError};

/// Writes framed documents to the peer, and to nothing else.
///
/// # Examples
///
/// ```
/// use protocol_droid::adapters::primary::stdio::{StdioConfig, StdioSink};
/// use protocol_droid::domain::MaxFrameSize;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let config = StdioConfig::new(MaxFrameSize::new(1024)?);
/// let sink = StdioSink::new(tokio::io::sink(), config);
///
/// // One line, one terminator, and nothing else on the stream.
/// sink.send(r#"{"jsonrpc":"2.0","id":1,"result":null}"#).await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct StdioSink<W> {
    stream: Mutex<W>,
    limit: MaxFrameSize,
}

impl<W> StdioSink<W> {
    /// Wraps a stream, refusing replies above the configured ceiling.
    #[must_use]
    pub fn new(stream: W, config: StdioConfig) -> Self {
        Self {
            stream: Mutex::new(stream),
            limit: config.limit(),
        }
    }

    /// The ceiling this sink refuses replies above.
    #[must_use]
    pub const fn limit(&self) -> MaxFrameSize {
        self.limit
    }
}

impl<W> StdioSink<W>
where
    W: AsyncWrite + Unpin,
{
    /// Frames `document` and writes it as one line.
    ///
    /// # Errors
    ///
    /// [`StdioError::ReplyTooLarge`] when the document is longer than the
    /// ceiling, in which case *nothing* is written — not a prefix, not a
    /// truncated line. [`StdioError::Transport`] when the stream refuses the
    /// bytes.
    pub async fn send(&self, document: &str) -> Result<(), StdioError> {
        // Encoded into a local first, so the size check and the terminator
        // happen before the stream is touched at all.
        let mut framed = Vec::with_capacity(document.len() + 1);
        encode_frame(document, &mut framed, self.limit).map_err(stdio_error)?;

        let mut stream = self.stream.lock().await;
        stream
            .write_all(&framed)
            .await
            .map_err(|_| StdioError::Transport)?;
        stream.flush().await.map_err(|_| StdioError::Transport)
    }
}

/// The domain's framing error, in the transport's vocabulary.
///
/// [`encode_frame`] only refuses on size — a `&str` is text by construction — so
/// the `NotUtf8` arm is unreachable from here. It is mapped rather than
/// `unreachable!()`d because the enum does not promise that to this caller, and
/// because both arms report the same thing: nothing reached the wire. A panic
/// on a path that runs for every reply a server ever sends is the wrong place
/// to be defending a hypothesis.
const fn stdio_error(error: FramingError) -> StdioError {
    match error {
        FramingError::FrameTooLarge { limit, at_least } => StdioError::ReplyTooLarge {
            limit,
            len: at_least,
        },
        FramingError::NotUtf8 { .. } => StdioError::Transport,
    }
}

impl<W> FrameSink for StdioSink<W>
where
    W: AsyncWrite + Unpin + Send,
{
    /// Writes one encoded frame.
    ///
    /// Collapses [`StdioError`] into the port's unit [`SinkError`], because the
    /// session engine is protocol-neutral and must not learn which transport it
    /// happens to be driving. That collapse is the mapping this boundary exists
    /// to perform — the specific reason was already decided by the time the
    /// engine asked, and a caller that needs it drives
    /// [`StdioSink::send`] directly.
    ///
    /// # Errors
    ///
    /// [`SinkError`] when the frame did not reach the peer.
    async fn send(&self, frame: &str) -> Result<(), SinkError> {
        Self::send(self, frame).await.map_err(|_| SinkError)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use assert_matches::assert_matches;

    use super::super::{StdioConfig, testing::Capture};
    use super::{StdioError, StdioSink, stdio_error};
    use crate::application::session::{DynFrameSink, FrameSink, SinkError};
    use crate::domain::{FramingError, MaxFrameSize, MaxFrameSizeError};

    /// The `Failure` an `?` in a test can produce, so a fallible call whose
    /// error type is not the one under test still propagates instead of
    /// unwrapping.
    #[derive(Debug)]
    #[expect(dead_code, reason = "the payloads are read by the derived Debug")]
    enum Failure {
        /// The sink refused a write the test expected to succeed.
        Stdio(StdioError),
        /// A limit the test tried to build was refused.
        Limit(MaxFrameSizeError),
        /// The port's unit error, for the call that goes through a trait object.
        Sink(SinkError),
    }

    impl From<StdioError> for Failure {
        fn from(error: StdioError) -> Self {
            Self::Stdio(error)
        }
    }

    impl From<MaxFrameSizeError> for Failure {
        fn from(error: MaxFrameSizeError) -> Self {
            Self::Limit(error)
        }
    }

    impl From<SinkError> for Failure {
        fn from(error: SinkError) -> Self {
            Self::Sink(error)
        }
    }

    /// A sink over a capture, so a test can assert on the bytes and not only on
    /// the return value. The "nothing was written" assertions are the point: a
    /// sink that returned the right error *and* leaked a truncated prefix would
    /// pass a test that only looked at the `Result`.
    fn capture_sink(limit: u32) -> Result<(StdioSink<Capture>, Capture), Failure> {
        let capture = Capture::shared();
        let config = StdioConfig::new(MaxFrameSize::new(limit)?);
        Ok((StdioSink::new(capture.clone(), config), capture))
    }

    /// A ceiling that is definitely not zero, for assertions that compare
    /// against a limit the test set itself.
    fn ceiling(bytes: u32) -> MaxFrameSize {
        MaxFrameSize::new(bytes).unwrap_or(MaxFrameSize::DEFAULT)
    }

    /// The happy path writes exactly one line: the document, then one `\n`.
    /// Anything else — two terminators, a trailing newline of its own, no
    /// terminator — produces a stream the peer's decoder reads differently.
    #[tokio::test]
    async fn a_reply_is_written_as_exactly_one_line() -> Result<(), Failure> {
        let (sink, capture) = capture_sink(1024)?;
        let document = r#"{"jsonrpc":"2.0","id":1,"result":null}"#;

        sink.send(document).await?;

        assert_eq!(capture.bytes(), format!("{document}\n").into_bytes());
        Ok(())
    }

    /// An oversized reply is a typed error, and the stream is untouched.
    ///
    /// This is the test the module exists for. Returning the right error while
    /// writing a truncated line would corrupt the stream in a way the peer
    /// cannot detect: in a newline protocol a truncated frame has no
    /// terminator, so the peer does not see a bad message, it sees the first
    /// half of one and then waits forever for the rest.
    #[tokio::test]
    async fn an_oversized_reply_is_typed_and_writes_nothing() -> Result<(), Failure> {
        let (sink, capture) = capture_sink(16)?;

        let result = sink
            .send("a reply well over the sixteen byte ceiling")
            .await;

        assert_matches!(
            result,
            Err(StdioError::ReplyTooLarge { limit, len }) => {
                assert_eq!(limit, ceiling(16));
                assert!(len > 16, "the reported length is the document's, not the limit's");
            }
        );
        assert!(
            capture.bytes().is_empty(),
            "an oversized reply must not put a single byte on the wire"
        );
        Ok(())
    }

    /// The ceiling counts the document, not the terminator, so a document of
    /// exactly `limit` bytes is legal on both halves of the transport. If it
    /// were not, the read side would accept a frame the write side refuses.
    #[tokio::test]
    async fn a_reply_of_exactly_the_ceiling_is_written() -> Result<(), Failure> {
        let (sink, capture) = capture_sink(16)?;
        let document = "0123456789abcdef";

        sink.send(document).await?;

        assert_eq!(
            capture.bytes().len(),
            17,
            "sixteen document bytes plus one terminator"
        );
        Ok(())
    }

    /// The port's spelling collapses the detail, and the frame still does not
    /// reach the stream. The collapse is deliberate — the session engine is
    /// protocol-neutral — so this asserts both halves of it.
    #[tokio::test]
    async fn the_port_trait_turns_an_oversized_reply_into_a_sink_error() -> Result<(), Failure> {
        let (sink, capture) = capture_sink(16)?;

        let result = FrameSink::send(&sink, "a reply well over the ceiling").await;

        assert_matches!(result, Err(SinkError));
        assert!(capture.bytes().is_empty());
        Ok(())
    }

    /// Two replies in flight at once must not interleave. The lock spans the
    /// whole write, so each line lands whole; without it a scheduler switch
    /// between the document and its terminator would put `half` and `whole` on
    /// the wire as two separate frames, which the peer's decoder reads as two
    /// malformed messages rather than one good reply and one bad one.
    ///
    /// The documents carry an embedded newline on purpose: without one, a
    /// mis-ordered pair of writes would still produce two well-formed frames
    /// and the test would pass against a broken sink.
    #[tokio::test]
    async fn concurrent_replies_do_not_interleave() -> Result<(), Failure> {
        let (sink, capture) = capture_sink(1024)?;
        let first = Arc::new(sink);
        let second = Arc::clone(&first);

        let (left, right) = tokio::join!(
            async move { first.send("{\"id\":1}\nembedded").await },
            async move { second.send("{\"id\":2}").await }
        );
        left?;
        right?;

        let written = capture.contents();
        for document in ["{\"id\":1}\nembedded", "{\"id\":2}"] {
            assert!(
                written.contains(&format!("{document}\n")),
                "each reply must land whole; the stream was {written:?}"
            );
        }
        Ok(())
    }

    /// The `FrameSink` impl needs no `DynFrameSink` impl: the session module
    /// already provides a blanket one for every `T: FrameSink + Send + Sync`,
    /// and a second here would collide with it at the coherence check. This
    /// test is the proof that the blanket impl really does apply.
    ///
    /// The call goes through [`DynFrameSink`], not [`FrameSink`], because the
    /// two are separate traits and the blanket impl runs in one direction only.
    /// A `dyn DynFrameSink` is not itself a `FrameSink`; asking for one is the
    /// mistake this test exists to make impossible.
    #[tokio::test]
    async fn the_sink_is_usable_behind_a_trait_object() -> Result<(), Failure> {
        let (sink, capture) = capture_sink(1024)?;
        let boxed: Arc<dyn DynFrameSink> = Arc::new(sink);

        DynFrameSink::send(boxed.as_ref(), "boxed").await?;

        assert_eq!(capture.contents(), "boxed\n");
        Ok(())
    }

    /// The unreachable arm of the mapping is mapped rather than panicked on,
    /// and it reports the same thing the reachable one does: nothing was
    /// written. A `&str` is text by construction, so the arm cannot be reached
    /// from `send` — but the enum does not promise that to a future caller, and
    /// a panic on the path that runs for every reply a server ever sends is the
    /// wrong place to be defending a hypothesis.
    #[test]
    fn a_not_utf8_framing_error_maps_rather_than_panics() {
        assert_eq!(
            stdio_error(FramingError::NotUtf8 { valid_up_to: 3 }),
            StdioError::Transport
        );
        assert_matches!(
            stdio_error(FramingError::FrameTooLarge {
                limit: ceiling(64),
                at_least: 65,
            }),
            StdioError::ReplyTooLarge { len: 65, .. }
        );
    }
}
