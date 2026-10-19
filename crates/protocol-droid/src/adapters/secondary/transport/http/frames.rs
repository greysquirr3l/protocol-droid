//! Turning a byte stream into SSE frames, without deciding what they mean.
//!
//! [`SseEvent`] already knows how to parse one block of text into an event and
//! what its fields mean. It does not know that a block arrives in pieces, nor
//! where one block ends and the next begins on the wire. This module owns
//! exactly that gap, and nothing else — the frame grammar stays with the
//! server's codec, because a client with its own grammar is a client that
//! disagrees with the server on resume, and the disagreement shows up as a
//! duplicated event rather than as an error.
//!
//! The split is not cosmetic. A reader that re-joined frames on `\n` alone
//! splits a CRLF-terminated block into one block plus an empty one; a reader
//! that treated an empty block as an event delivers a keep-alive to the
//! protocol layer as if the server had said something. Both are silent: the
//! stream stays well-formed and the client just sees more events than there
//! were.

use crate::adapters::primary::transport::http::{SseError, SseEvent};

/// The largest single SSE block this reader will assemble.
///
/// A block is one event. The server writes each in one go and the grammar puts
/// no bound on one, but an unbounded buffer over a peer-supplied stream is a
/// memory cost with no ceiling. Exceeding this is an error rather than a
/// truncation: a truncated event handed upward is a lie about what the peer
/// said, and a lie with an event id attached is a lie a resume will repeat.
const MAX_BLOCK: usize = 8 * 1024;

/// Why a byte stream did not yield frames this client can hand on.
///
/// Separate from [`SseError`] on purpose. [`SseError`] is the wire grammar's
/// vocabulary and lives with the codec that both sides share; the length bound
/// is this reader's memory policy, not a rule a peer could observe if we chose
/// a different number.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// The peer sent more than `MAX_BLOCK` bytes without a block terminator.
    #[error("the peer sent {limit} bytes with no block terminator")]
    BlockTooLong {
        /// The bound that was exceeded.
        limit: usize,
    },

    /// The block was terminated but is not a frame.
    #[error("the block is not a frame this client can read: {source}")]
    Grammar {
        /// The shared codec's account of what was wrong.
        source: SseError,
    },
}

impl From<SseError> for FrameError {
    fn from(source: SseError) -> Self {
        Self::Grammar { source }
    }
}

/// Assembles SSE blocks out of arbitrarily chunked bytes.
///
/// Fed whatever the transport delivers; yields complete frames. The buffer is
/// retained between calls because a frame routinely spans two reads, and
/// dropping it per read is what turns a normal stream into a stream of one-byte
/// events.
#[derive(Debug, Default)]
pub struct FrameReader {
    buffer: Vec<u8>,
}

impl FrameReader {
    /// A reader with nothing buffered.
    #[must_use]
    pub const fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    /// How many bytes are held waiting for their terminator.
    #[must_use]
    pub const fn buffered(&self) -> usize {
        self.buffer.len()
    }

    /// Feeds bytes and yields every complete frame they finished.
    ///
    /// Yields in wire order and stops at the first partial block, because
    /// anything after an unterminated block is inside that block — the grammar
    /// has no way to say "and here is the next event".
    ///
    /// # Errors
    ///
    /// [`FrameError::BlockTooLong`] if the buffered block grows past
    /// `MAX_BLOCK` before a terminator arrives, or [`FrameError::Grammar`] if
    /// a completed block does not parse.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseEvent>, FrameError> {
        self.buffer.extend_from_slice(chunk);
        if self.buffer.len() > MAX_BLOCK {
            // Cleared before returning, so a peer that keeps sending does not
            // make the next call fail on a block this one already rejected.
            self.buffer.clear();
            return Err(FrameError::BlockTooLong { limit: MAX_BLOCK });
        }

        // An index rather than a repeated re-slice-and-reassign: the block
        // borrows the buffer it was found in, so consuming it as it goes means
        // one `drain` at the end instead of a borrow that ends and restarts
        // once per frame. On a stream that reads forever, that is the
        // difference between one copy per read and one copy per frame.
        let mut frames = Vec::new();
        let mut consumed = 0_usize;
        while let Some(tail) = self.buffer.get(consumed..) {
            let Some((block, used)) = split_block(tail) else {
                break;
            };
            if let Some(event) = SseEvent::dispatch(block)? {
                frames.push(event);
            }
            consumed = consumed.saturating_add(used);
        }
        if consumed > 0 {
            let _ = self.buffer.drain(..consumed);
        }
        Ok(frames)
    }

    /// Takes the bytes of a block that never got its terminator.
    ///
    /// `None` for a stream that ended cleanly — one whose trailing block is
    /// absent, or complete. `Some` is the difference between a connection that
    /// dropped mid-event and one that ended on a boundary, and it is what
    /// decides whether a resume has to guess.
    #[must_use]
    pub fn into_partial(self) -> Option<Vec<u8>> {
        if self.buffer.is_empty() {
            None
        } else {
            Some(self.buffer)
        }
    }
}

/// Finds the first complete block in `bytes`, and how long it is.
///
/// A block ends at a **blank line**, not at the first newline. That is the
/// whole framing: `id: 1\ndata: x\n\n` is one event, and a reader that stops
/// at the first newline hands the grammar a block with no blank line in it and
/// never dispatches anything at all. The block is returned **including** its
/// last line terminator, because that is the shape the encoder wrote and the
/// shape the decoder is written against — a payload's final line is empty, and
/// the trailing newline is what makes it there to be read.
///
/// The length is returned rather than a remainder because the block borrows
/// `bytes`: a caller that reassigns the buffer to make progress cannot do so
/// while that borrow lives.
///
/// # Panics
///
/// Never. The returned block is a subslice of `bytes` and `used` is its length.
#[must_use]
pub fn split_block(bytes: &[u8]) -> Option<(&str, usize)> {
    let mut start = 0_usize;
    loop {
        let end = line_end(bytes, start)?;
        // The line just read is the block's last if the line after it is empty.
        // An empty line is a terminator with nothing between it and this one,
        // and it is the only thing that dispatches an event.
        let blank = match bytes.get(end) {
            Some(b'\n') => Some(end.saturating_add(1)),
            Some(b'\r') => Some(if bytes.get(end + 1) == Some(&b'\n') {
                end.saturating_add(2)
            } else {
                end.saturating_add(1)
            }),
            // The stream ended on a line boundary with no blank line after it,
            // so the event was never dispatched. Reporting "no block" is what
            // lets the caller tell a cut-off frame from a finished one.
            _ => None,
        };
        if let Some(consumed) = blank {
            let block = bytes.get(..end)?;
            return Some((decode_lossy(block), consumed));
        }
        start = end;
    }
}

/// The index just past the terminator of the line starting at `from`.
///
/// All three terminators are accepted, because a peer that writes CRLF and a
/// peer that writes LF are both conforming and a reader that understands only
/// one of them fails on half its peers. A lone `CR` at the end returns `None`
/// rather than guessing: the `LF` may be in the next chunk, and resolving it
/// here is how a CRLF stream gets a phantom event whenever a chunk boundary
/// lands between the pair.
fn line_end(bytes: &[u8], from: usize) -> Option<usize> {
    let mut index = from;
    while index < bytes.len() {
        match bytes.get(index) {
            Some(b'\n') => return Some(index.saturating_add(1)),
            Some(b'\r') => {
                return Some(if bytes.get(index + 1) == Some(&b'\n') {
                    index.saturating_add(2)
                } else {
                    index.saturating_add(1)
                });
            }
            _ => index = index.saturating_add(1),
        }
    }
    None
}

/// Decodes a block that may have arrived split across a multi-byte character.
///
/// Lossy on purpose. A block cut short by a dropped connection can end inside a
/// multi-byte sequence, and the caller is about to discard those bytes anyway;
/// what reaches the grammar decoder is the valid prefix.
fn decode_lossy(bytes: &[u8]) -> &str {
    core::str::from_utf8(bytes).unwrap_or_default()
}
