//! Newline-delimited framing: bytes in, whole messages out.
//!
//! # What this is for
//!
//! The stdio transport carries one JSON-RPC message per line, and the line is
//! the only framing the specification defines for it. Everything else in that
//! transport — the pipe, the child process, `tokio::process` — is I/O, and this
//! module is not: it is a state machine over a byte buffer, and a test can
//! drive it with `&[u8]` and no process anywhere. That separation is the whole
//! reason the framing is worth having as its own stage.
//!
//! # The limit is the point
//!
//! [`FrameDecoder`] takes a [`MaxFrameSize`] from its caller and refuses a frame
//! larger than it, with [`FramingError::FrameTooLarge`]. Two properties of *how*
//! it refuses are the security-relevant part, and both are load-bearing:
//!
//! 1. **The check runs while the bytes are being read, never after.** A decoder
//!    that accumulates a line and measures it once the `\n` arrives has already
//!    paid for the memory it was trying to avoid — and the peer controls how
//!    much that costs, because it controls whether a `\n` ever arrives. This one
//!    counts against the limit inside [`FrameDecoder::push`], before the
//!    offending bytes are appended, and a refusal appends nothing at all.
//! 2. **The retained buffer does not grow with the size of a hostile frame.** It
//!    is bounded by the configured limit plus whatever the caller pushed, and
//!    not by the frame. A peer that streams gigabytes without a terminator is
//!    cut off after `limit` bytes, and the decoder is holding at most that.
//!    [`FrameDecoder::buffered`] exists so a test can assert this rather than
//!    take the claim on trust.
//!
//! # What is deliberately not here
//!
//! - **No MCP.** Framing delimits a JSON-RPC message; it does not know one from
//!   another, and nothing in this module names a method or a revision.
//! - **No I/O.** Nothing here opens, reads or writes anything. The transport
//!   adapter supplies bytes and takes documents.
//! - **No interpretation.** A `\r` before the `\n` is content, not a line ending
//!   to be trimmed, and an empty line is an empty frame rather than a skipped
//!   one. Trimming would be the same "helpful" rewriting the project forbids
//!   elsewhere: a decoder that edits a peer's bytes cannot forward them
//!   unchanged, and lossless passthrough is a property the bridge depends on.
//! - **No default limit.** [`FrameDecoder::new`] takes a [`MaxFrameSize`] and
//!   there is no way to build one without it, so a transport cannot acquire a
//!   ceiling by accident. [`MaxFrameSize::DEFAULT`] exists for a caller who wants
//!   a starting point and will still name it.
//!
//! # Examples
//!
//! A frame split across two reads, which is the case hand-rolled framing gets
//! wrong most often:
//!
//! ```
//! use protocol_droid::domain::{FrameDecoder, MaxFrameSize};
//!
//! let limit = MaxFrameSize::new(64)?;
//! let mut decoder = FrameDecoder::new(limit);
//!
//! decoder.push(b"{\"jsonrpc\":\"2.0\",")?;
//! assert_eq!(decoder.next_frame()?, None, "no terminator yet");
//!
//! decoder.push(b"\"id\":1}\n")?;
//! assert_eq!(decoder.next_frame()?, Some(r#"{"jsonrpc":"2.0","id":1}"#));
//! assert_eq!(decoder.next_frame()?, None, "the frame was consumed");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use core::num::NonZeroU32;

use thiserror::Error;

/// The byte that terminates a frame.
const TERMINATOR: u8 = b'\n';

/// Above this many consumed bytes the buffer is compacted rather than left to
/// drift.
///
/// The decoder reads out of the front of a `Vec` by advancing a cursor rather
/// than by draining, because draining the front of a buffer is linear in what
/// remains and a long-lived connection would make that quadratic. A cursor means
/// the consumed prefix sits in the allocation until the buffer is emptied, so it
/// is reclaimed once reclaiming it is worth the copy.
const COMPACT_AT: usize = 8 * 1024;

/// The largest frame a decoder will accept, in bytes, excluding the terminator.
///
/// A caller sets this. There is no constant a decoder falls back to, because a
/// ceiling nobody chose is a ceiling nobody has checked against the largest
/// message the application actually sends.
///
/// The inner type is [`NonZeroU32`]: a limit of zero would accept no frame at
/// all, including the empty one, which is a configuration mistake rather than a
/// policy, and refusing it at construction is the last point at which the caller
/// can still see what they did.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::{MaxFrameSize, MaxFrameSizeError};
///
/// let limit = MaxFrameSize::new(8 * 1024 * 1024)?;
/// assert_eq!(limit.bytes(), 8 * 1024 * 1024);
///
/// assert_eq!(MaxFrameSize::new(0), Err(MaxFrameSizeError::Zero));
/// assert!(MaxFrameSize::new(1).is_ok());
/// # Ok::<(), MaxFrameSizeError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MaxFrameSize(NonZeroU32);

impl MaxFrameSize {
    /// A starting point, for a caller who wants one and will still name it.
    ///
    /// Four mebibytes comfortably holds a `tools/list` of a large catalogue
    /// while still bounding a hostile peer.
    pub const DEFAULT: Self = Self(match NonZeroU32::new(4 * 1024 * 1024) {
        Some(inner) => inner,
        // Unreachable for the literal above, and `NonZeroU32::MIN` keeps this a
        // `const` without a `panic!`, which the project's clippy profile denies
        // across all targets.
        None => NonZeroU32::MIN,
    });

    /// Builds a limit.
    ///
    /// # Errors
    ///
    /// [`MaxFrameSizeError::Zero`] when `bytes` is zero. See the type's docs for
    /// why that is refused rather than accepted.
    pub const fn new(bytes: u32) -> Result<Self, MaxFrameSizeError> {
        match NonZeroU32::new(bytes) {
            Some(inner) => Ok(Self(inner)),
            None => Err(MaxFrameSizeError::Zero),
        }
    }

    /// The limit, in bytes.
    pub const fn bytes(self) -> u32 {
        self.0.get()
    }

    /// The limit as a `usize`, for the comparisons a buffer length needs.
    ///
    /// Not `From<u32>`: `usize` may be narrower than `u32` in principle, so std
    /// declines the infallible conversion and this is where the project chooses
    /// to make one. `TryFrom` is not const-stable, so neither is this.
    fn as_usize(self) -> usize {
        usize::try_from(self.0.get()).unwrap_or(usize::MAX)
    }
}

impl core::fmt::Display for MaxFrameSize {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} bytes", self.0)
    }
}

/// Why a [`MaxFrameSize`] could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MaxFrameSizeError {
    /// A limit of zero was requested.
    ///
    /// It would reject every frame including the empty one, so it encodes a
    /// caller mistake rather than a policy.
    #[error("a maximum frame size of zero would reject every frame, including an empty one")]
    Zero,
}

/// A frame that could not be framed.
///
/// A domain error: the framing layer knows about sizes and encodings and nothing
/// else, so nothing in it is an `io::Error` or a `serde_json::Error`, and an
/// adapter that hits a broken pipe maps that at its own boundary instead.
///
/// Every field is `Copy`, so the whole error is: it is a size and an offset,
/// and there is nothing in it worth owning twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum FramingError {
    /// A frame exceeded the decoder's configured limit.
    ///
    /// Raised while the bytes are still being read, so the decoder is holding at
    /// most `at_least` bytes of the offending frame and the peer cannot make
    /// that number grow by declining to send a terminator.
    #[error("frame exceeds the configured maximum of {limit}")]
    FrameTooLarge {
        /// The limit the caller configured.
        limit: MaxFrameSize,
        /// How many bytes of this frame had arrived when the refusal happened.
        ///
        /// A lower bound, not a length. The frame has no known length — that is
        /// the situation — so this is what was buffered, one byte past the limit.
        at_least: usize,
    },
    /// A frame's bytes were not valid UTF-8.
    ///
    /// Refused rather than replaced. A lossy decode would hand the next layer a
    /// message the peer did not send, and JSON-RPC ids are compared as text, so
    /// the corruption would surface as a routing bug rather than as an encoding
    /// one.
    #[error("frame is not valid UTF-8 (first invalid byte at offset {valid_up_to})")]
    NotUtf8 {
        /// Offset within the frame at which decoding stopped being possible.
        valid_up_to: usize,
    },
}

/// Reassembles newline-delimited frames from arbitrarily chopped-up bytes.
///
/// Feed it whatever a read returned with [`push`](Self::push), then take
/// complete frames out with [`next_frame`](Self::next_frame) until it says
/// `None`, which means "not yet" rather than "nothing more".
///
/// The cursor ([`buffered`](Self::buffered)) matters for reading these two in
/// order. A caller that pushes without draining accumulates complete frames as
/// well as the partial one, and the buffer is then bounded by the limit plus the
/// chunk it was given — never by the peer's frame size. The stdio adapter reads
/// a chunk, pushes it, and drains it before reading again.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::{FrameDecoder, MaxFrameSize};
///
/// let mut decoder = FrameDecoder::new(MaxFrameSize::new(1024)?);
/// decoder.push(b"first\nsecond\n")?;
///
/// assert_eq!(decoder.next_frame()?, Some("first"));
/// assert_eq!(decoder.next_frame()?, Some("second"));
/// assert_eq!(decoder.next_frame()?, None);
/// assert!(decoder.is_empty());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone)]
pub struct FrameDecoder {
    limit: MaxFrameSize,
    /// The read buffer. `0..start` is consumed; the rest is pending.
    buffer: Vec<u8>,
    start: usize,
}

impl FrameDecoder {
    /// A decoder that refuses frames larger than `limit`.
    ///
    /// No default: see the module docs.
    pub const fn new(limit: MaxFrameSize) -> Self {
        Self {
            limit,
            buffer: Vec::new(),
            start: 0,
        }
    }

    /// The limit this decoder was built with.
    pub const fn limit(&self) -> MaxFrameSize {
        self.limit
    }

    /// Bytes read and not yet handed out, including a partially-read frame.
    ///
    /// Exposed so a caller — or a test — can see the decoder's own footprint. It
    /// is bounded by the limit plus the size of the last chunk pushed, and is not
    /// a function of how large a frame the peer chose to send.
    pub const fn buffered(&self) -> usize {
        self.buffer.len() - self.start
    }

    /// Whether every frame pushed so far has been taken.
    pub const fn is_empty(&self) -> bool {
        self.buffered() == 0
    }

    /// Adds bytes read from the peer.
    ///
    /// The frame in progress may be split anywhere, including inside a
    /// multi-byte UTF-8 sequence — nothing here decodes until a frame is
    /// complete, so a character is never truncated by a read boundary.
    ///
    /// # Errors
    ///
    /// [`FramingError::FrameTooLarge`] when the bytes after the last terminator
    /// would push the frame in progress past the limit. The check runs before
    /// those bytes are buffered, so the decoder's footprint is unaffected and
    /// the decoder is left exactly as it was.
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), FramingError> {
        // Count the frame in progress before appending, so the whole check runs
        // against `bytes` plus what is already held and nothing is written until
        // the scan has passed. This ordering is the type's security property: a
        // refusal that had already appended would have allocated exactly the
        // memory it exists to avoid.
        let limit = self.limit.as_usize();
        let mut frame_len = self.buffered();
        for byte in bytes {
            if *byte == TERMINATOR {
                frame_len = 0;
            } else {
                frame_len += 1;
                if frame_len > limit {
                    return Err(FramingError::FrameTooLarge {
                        limit: self.limit,
                        at_least: frame_len,
                    });
                }
            }
        }

        self.buffer.extend_from_slice(bytes);
        Ok(())
    }

    /// Takes the next complete frame, or `None` if the terminator has not
    /// arrived.
    ///
    /// The returned `&str` borrows the read buffer and is valid until the next
    /// `push` or `next_frame`. A caller that needs to keep a frame past that
    /// point copies it; the copy is then at the call site, where it is visible,
    /// rather than hidden inside a type that allocates for everyone.
    ///
    /// # Errors
    ///
    /// [`FramingError::NotUtf8`] when the frame's bytes are not UTF-8. The
    /// offending frame is dropped, so a caller that chooses to carry on reads
    /// the *next* frame instead of being handed the same bad one for ever. A
    /// transport will normally close on this error, because a peer that emits a
    /// non-UTF-8 frame has said something about itself; the discard is here so
    /// that a caller which decides to continue is not trapped in a loop.
    pub fn next_frame(&mut self) -> Result<Option<&str>, FramingError> {
        // Compact before borrowing: compaction can reallocate, and a borrow
        // taken first would be invalidated by it.
        self.compact();

        let Some(pending) = self.buffer.get(self.start..) else {
            // `start` is only ever advanced to at most `buffer.len()`, so this
            // cannot happen. It is still handled rather than asserted, because
            // the project denies `panic!` and `unreachable!`, and a read that
            // cannot yield a frame is the honest answer if the invariant is
            // ever broken.
            return Ok(None);
        };
        let Some(newline) = pending.iter().position(|byte| *byte == TERMINATOR) else {
            return Ok(None);
        };

        let end = self.start + newline;
        let frame = self.buffer.get(self.start..end).unwrap_or_default();
        // Past the terminator, and set before validating so that the error path
        // below discards the offending frame as documented.
        self.start = end + 1;

        match core::str::from_utf8(frame) {
            Ok(text) => Ok(Some(text)),
            Err(error) => Err(FramingError::NotUtf8 {
                valid_up_to: error.valid_up_to(),
            }),
        }
    }

    /// Reclaims the consumed prefix once it outweighs its own size.
    fn compact(&mut self) {
        if self.start == self.buffer.len() {
            self.buffer.clear();
            self.start = 0;
        } else if self.start >= COMPACT_AT {
            self.buffer.drain(..self.start);
            self.start = 0;
        }
    }
}

/// Appends one frame — the document, then the terminator — to `out`.
///
/// The write half of the same framing, so a server and a client cannot disagree
/// about where a frame ends. `out` is taken rather than returned so a transport
/// writing into a reused buffer pays no allocation per frame.
///
/// The document is neither validated nor escaped: it is a document some codec
/// already produced, and a frame is a container, not a parser. What *is* checked
/// is the size, because a frame this process emits has to satisfy the same
/// ceiling a peer's does, and learning that at the write is far better than
/// learning it as a truncated read on the other side.
///
/// # Errors
///
/// [`FramingError::FrameTooLarge`] when `document` is longer than `limit`. The
/// limit counts the document's bytes and not the terminator, so a document of
/// exactly `limit` bytes is a legal frame.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::{MaxFrameSize, encode_frame};
///
/// let mut wire = Vec::new();
/// encode_frame("{\"jsonrpc\":\"2.0\"}", &mut wire, MaxFrameSize::new(64)?)?;
///
/// assert_eq!(wire, b"{\"jsonrpc\":\"2.0\"}\n");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn encode_frame(
    document: &str,
    out: &mut Vec<u8>,
    limit: MaxFrameSize,
) -> Result<(), FramingError> {
    let len = document.len();
    if len > limit.as_usize() {
        return Err(FramingError::FrameTooLarge {
            limit,
            at_least: len,
        });
    }
    out.extend_from_slice(document.as_bytes());
    out.push(TERMINATOR);
    Ok(())
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;

    use super::{FrameDecoder, FramingError, MaxFrameSize, MaxFrameSizeError, encode_frame};

    /// The `Failure` an `?` in a test can produce, so a fallible call whose error
    /// type is not the one under test still propagates instead of unwrapping.
    #[derive(Debug)]
    #[expect(dead_code, reason = "the payloads are read by the derived Debug")]
    enum Failure {
        /// A frame the test tried to push or take was refused.
        Framing(FramingError),
        /// A limit the test tried to build was refused.
        Limit(MaxFrameSizeError),
    }

    impl From<FramingError> for Failure {
        fn from(error: FramingError) -> Self {
            Self::Framing(error)
        }
    }

    impl From<MaxFrameSizeError> for Failure {
        fn from(error: MaxFrameSizeError) -> Self {
            Self::Limit(error)
        }
    }

    /// A decoder with a roomy limit, for the cases that are not about size.
    fn roomy() -> Result<FrameDecoder, MaxFrameSizeError> {
        Ok(FrameDecoder::new(MaxFrameSize::new(1024)?))
    }

    /// `u32` to `usize` for the size assertions. std has no `From<u32> for
    /// usize` because `usize` may be narrower, so the cast is spelled once here
    /// rather than repeated.
    const fn limit_in_usize(limit: MaxFrameSize) -> usize {
        limit.bytes() as usize
    }

    #[test]
    fn one_frame_round_trips_through_encode_and_decode() -> Result<(), Failure> {
        let document = r#"{"jsonrpc":"2.0","id":1,"result":{}}"#;
        let mut wire = Vec::new();
        encode_frame(document, &mut wire, MaxFrameSize::new(1024)?)?;

        let mut decoder = roomy()?;
        decoder.push(&wire)?;
        assert_eq!(decoder.next_frame()?, Some(document));
        assert_eq!(decoder.next_frame()?, None);
        Ok(())
    }

    #[test]
    fn several_frames_in_one_read_come_back_in_order() -> Result<(), Failure> {
        let mut decoder = roomy()?;
        decoder.push(b"one\ntwo\nthree\n")?;

        for expected in ["one", "two", "three"] {
            assert_eq!(decoder.next_frame()?, Some(expected));
        }
        assert_eq!(decoder.next_frame()?, None);
        assert!(decoder.is_empty());
        Ok(())
    }

    #[test]
    fn several_frames_one_per_read_come_back_in_order() -> Result<(), Failure> {
        let mut decoder = roomy()?;
        for chunk in [b"one\n".as_slice(), b"two\n", b"three\n"] {
            decoder.push(chunk)?;
        }

        for expected in ["one", "two", "three"] {
            assert_eq!(decoder.next_frame()?, Some(expected));
        }
        assert_eq!(decoder.next_frame()?, None);
        Ok(())
    }

    #[test]
    fn a_frame_split_across_reads_is_reassembled() -> Result<(), Failure> {
        let document = r#"{"jsonrpc":"2.0","method":"ping"}"#;
        let bytes = document.as_bytes();
        let split = document.len() / 2;

        let mut decoder = roomy()?;
        decoder.push(bytes.get(..split).unwrap_or_default())?;
        assert_eq!(
            decoder.next_frame()?,
            None,
            "a frame with no terminator is not yet a frame"
        );

        decoder.push(bytes.get(split..).unwrap_or_default())?;
        decoder.push(b"\n")?;
        assert_eq!(decoder.next_frame()?, Some(document));
        Ok(())
    }

    /// Every single-byte split of a two-frame stream, which is where hand-rolled
    /// framing usually breaks: the terminator landing alone in its own read, and
    /// a frame boundary coinciding with one.
    #[test]
    fn any_single_byte_split_of_a_two_frame_stream_decodes() -> Result<(), Failure> {
        let stream = b"{\"a\":1}\n{\"b\":2}\n";

        for split in 0..=stream.len() {
            let mut decoder = roomy()?;
            decoder.push(stream.get(..split).unwrap_or_default())?;
            decoder.push(stream.get(split..).unwrap_or_default())?;

            assert_eq!(decoder.next_frame()?, Some(r#"{"a":1}"#), "split {split}");
            assert_eq!(decoder.next_frame()?, Some(r#"{"b":2}"#), "split {split}");
            assert_eq!(decoder.next_frame()?, None, "split {split}");
        }
        Ok(())
    }

    #[test]
    fn a_multi_byte_character_split_across_reads_is_not_truncated() -> Result<(), Failure> {
        // "…" is three bytes. Splitting between them is the case a decoder that
        // validated eagerly would corrupt, and it is why validation happens on a
        // complete frame and never on a chunk.
        let document = r#"{"emoji":"…"}"#;
        let bytes = document.as_bytes();
        let split = bytes
            .windows(3)
            .position(|window| window == "…".as_bytes())
            .map_or(0, |at| at + 1);

        let mut decoder = roomy()?;
        decoder.push(bytes.get(..split).unwrap_or_default())?;
        assert_eq!(decoder.next_frame()?, None);

        decoder.push(bytes.get(split..).unwrap_or_default())?;
        decoder.push(b"\n")?;
        assert_eq!(decoder.next_frame()?, Some(document));
        Ok(())
    }

    #[test]
    fn a_frame_at_exactly_the_limit_is_accepted() -> Result<(), Failure> {
        let limit = MaxFrameSize::new(8)?;
        let document = "12345678";
        let mut wire = Vec::new();
        encode_frame(document, &mut wire, limit)?;

        let mut decoder = FrameDecoder::new(limit);
        decoder.push(&wire)?;
        assert_eq!(decoder.next_frame()?, Some(document));
        Ok(())
    }

    #[test]
    fn a_frame_one_byte_over_the_limit_is_refused() -> Result<(), Failure> {
        let limit = MaxFrameSize::new(8)?;
        let mut wire = Vec::new();
        assert_matches!(
            encode_frame("123456789", &mut wire, limit),
            Err(FramingError::FrameTooLarge { limit: got, .. }) if got == limit
        );
        assert!(wire.is_empty(), "a refused frame must write nothing");
        Ok(())
    }

    /// The security property, asserted rather than asserted-in-prose: a peer
    /// that never sends a terminator is cut off at the limit, and the decoder's
    /// footprint is a function of the limit rather than of what the peer sent.
    #[test]
    fn an_unterminated_frame_cannot_grow_the_buffer_past_the_limit() -> Result<(), Failure> {
        let limit = MaxFrameSize::new(64)?;
        let mut decoder = FrameDecoder::new(limit);

        // Sixteen kilobytes of a frame that never ends: 256x the limit.
        assert_matches!(
            decoder.push(&vec![b'x'; 16 * 1024]),
            Err(FramingError::FrameTooLarge { limit: got, at_least })
                if got == limit && at_least == limit_in_usize(limit) + 1
        );

        assert!(
            decoder.buffered() <= limit_in_usize(limit),
            "held {} bytes for a {limit} limit",
            decoder.buffered()
        );
        Ok(())
    }

    #[test]
    fn a_refusal_leaves_the_decoder_exactly_as_it_was() -> Result<(), Failure> {
        let limit = MaxFrameSize::new(16)?;
        let mut decoder = FrameDecoder::new(limit);
        decoder.push(b"short\npartial")?;
        let before = decoder.buffered();

        assert_matches!(
            decoder.push(&[b'y'; 64]),
            Err(FramingError::FrameTooLarge { .. })
        );
        assert_eq!(decoder.buffered(), before, "the refusal buffered bytes");
        assert_eq!(decoder.next_frame()?, Some("short"));
        Ok(())
    }

    /// The limit is per frame, not per connection: a stream of frames that are
    /// each under the limit is a stream of legal frames however many there are.
    #[test]
    fn a_completed_frame_does_not_count_against_the_next_one() -> Result<(), Failure> {
        let limit = MaxFrameSize::new(8)?;
        let mut decoder = FrameDecoder::new(limit);

        for _ in 0..100 {
            decoder.push(b"12345678\n")?;
            assert_eq!(decoder.next_frame()?, Some("12345678"));
        }
        assert!(decoder.is_empty());
        Ok(())
    }

    #[test]
    fn invalid_utf8_is_a_typed_error_rather_than_a_lossy_decode() -> Result<(), Failure> {
        let mut decoder = roomy()?;
        // 0xFF is not a legal UTF-8 byte in any position; here it is the sixth.
        decoder.push(b"{\"a\":\"\xff\"}\n")?;

        assert_matches!(
            decoder.next_frame(),
            Err(FramingError::NotUtf8 { valid_up_to: 6 })
        );

        // The bad frame was dropped, so a caller that continues is not trapped.
        decoder.push(b"next\n")?;
        assert_eq!(decoder.next_frame()?, Some("next"));
        Ok(())
    }

    #[test]
    fn an_empty_frame_is_a_frame_rather_than_a_skipped_line() -> Result<(), Failure> {
        let mut decoder = roomy()?;
        decoder.push(b"\n\n")?;

        assert_eq!(decoder.next_frame()?, Some(""));
        assert_eq!(decoder.next_frame()?, Some(""));
        assert_eq!(decoder.next_frame()?, None);
        Ok(())
    }

    /// A `\r` is content. Trimming it would mean the decoder edits a peer's
    /// bytes, and the bytes the peer sent are the bytes the bridge forwards.
    #[test]
    fn a_carriage_return_is_kept_as_content() -> Result<(), Failure> {
        let mut decoder = roomy()?;
        decoder.push(b"{\"a\":1}\r\n")?;
        assert_eq!(decoder.next_frame()?, Some("{\"a\":1}\r"));
        Ok(())
    }

    #[test]
    fn a_carriage_return_before_a_newline_counts_against_the_limit() -> Result<(), Failure> {
        let limit = MaxFrameSize::new(4)?;
        let mut decoder = FrameDecoder::new(limit);
        assert_matches!(
            decoder.push(b"1234\r"),
            Err(FramingError::FrameTooLarge { .. })
        );
        Ok(())
    }

    #[test]
    fn the_limit_is_configurable_and_readable_back() -> Result<(), Failure> {
        let limit = MaxFrameSize::new(4096)?;
        let decoder = FrameDecoder::new(limit);
        assert_eq!(decoder.limit(), limit);
        assert_eq!(decoder.limit().bytes(), 4096);
        Ok(())
    }

    #[test]
    fn a_zero_limit_is_refused_at_construction() {
        assert_eq!(MaxFrameSize::new(0), Err(MaxFrameSizeError::Zero));
        assert!(MaxFrameSize::new(1).is_ok());
    }

    #[test]
    fn the_default_limit_is_an_ordinary_value_rather_than_a_fallback() {
        // The point of the type having no `Default`: a decoder is always built
        // from a limit somebody chose. `DEFAULT` is available, and it is nothing
        // more than a value.
        assert_eq!(MaxFrameSize::DEFAULT.bytes(), 4 * 1024 * 1024);
    }

    /// The buffer is compacted rather than growing to hold a whole connection's
    /// history. `COMPACT_AT` is private, so the test drives past it by pushing a
    /// great many frames and then checks the footprint is back to the live
    /// bytes.
    #[test]
    fn a_long_connection_does_not_retain_every_frame_it_has_seen() -> Result<(), Failure> {
        let limit = MaxFrameSize::new(64)?;
        let mut decoder = FrameDecoder::new(limit);

        for _ in 0..4096 {
            decoder.push(b"a-frame\n")?;
            assert_eq!(decoder.next_frame()?, Some("a-frame"));
        }
        assert!(
            decoder.buffered() <= limit_in_usize(limit),
            "retained {} bytes of history",
            decoder.buffered()
        );
        assert!(decoder.is_empty());
        Ok(())
    }
}
