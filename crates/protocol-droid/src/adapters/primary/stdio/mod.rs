//! The stdio transport's byte layer.
//!
//! # What this is
//!
//! Three adapters and a shared error type, and none of them knows what a
//! JSON-RPC message is. [`StdioSource`] turns a byte stream into whole frames,
//! [`StdioSink`] turns documents into framed bytes, and [`Diagnostics`] is the
//! side channel that must never be confused for either. The framing itself is
//! [`crate::domain::FrameDecoder`] and [`crate::domain::encode_frame`] — this
//! module supplies the stream, the ceiling, and the errors, and delegates the
//! rest, so the two halves of the transport cannot disagree about where a
//! frame ends or how long one may be.
//!
//! # The ceiling is one value, not two
//!
//! [`StdioConfig`] is handed to both halves. That is the point: a limit applied
//! only to reads protects the server from a hostile peer but not from a
//! handler that answers with more than the peer will read, and a limit applied
//! only to writes is the same argument with the roles swapped. One
//! [`MaxFrameSize`], both directions, so a reply is refused with a typed error
//! instead of being written as a line the peer's own decoder has to treat as a
//! malformed message.
//!
//! # Why the transport is generic over its stream
//!
//! [`StdioSource<R>`] and [`StdioSink<W>`] are generic rather than hard-wired to
//! a process's stdin and stdout. The transport boundary is where a
//! `tokio::process::Child` becomes a byte stream, and everything below that line
//! is identical whether the bytes arrive from a child, a pipe, a socket, or a
//! test. Naming the concrete stream here would make every one of those cases a
//! second implementation, and the framing logic — the part with the security
//! properties — would be duplicated along with it.
//!
//! # What is deliberately not here
//!
//! No spawning, no lifecycle, and no JSON-RPC. This is a byte layer; the loop
//! that reads a frame, routes it, and answers lives above it, and the process
//! management that gives it a stream in the first place is a separate concern.
//! A single unterminated line has no framing error, because the framing layer
//! cannot know whether the peer is mid-message or has gone away.

mod diagnostics;
mod sink;
mod source;

#[cfg(test)]
mod testing;

pub use diagnostics::{Diagnostics, DiagnosticsWriter};
pub use sink::StdioSink;
pub use source::StdioSource;

use core::num::NonZeroUsize;

use thiserror::Error;

use crate::domain::MaxFrameSize;

/// How many bytes a single read from the peer's stream is allowed to ask for.
///
/// A pipe hands over whatever happens to be in the buffer, so this is an upper
/// bound rather than a promise: a large frame is still assembled from as many
/// reads as it takes. It is sized for a pipe rather than a socket because a
/// pipe read cannot be made to wait for a full buffer, so an 8 KiB ask is
/// satisfied quickly and keeps the syscall count down without holding a frame's
/// worth of memory per connection.
const DEFAULT_READ_CHUNK: NonZeroUsize = match NonZeroUsize::new(8 * 1024) {
    Some(chunk) => chunk,
    None => NonZeroUsize::MIN,
};

/// The settings both halves of the transport are built from.
///
/// Cloned into each adapter rather than shared by reference, so a
/// [`StdioSource`] and a [`StdioSink`] on the same session cannot be pointed at
/// different ceilings after the fact. The value is three words; the invariant is
/// worth more than the copy.
///
/// # Examples
///
/// ```
/// use protocol_droid::adapters::primary::stdio::StdioConfig;
/// use protocol_droid::domain::MaxFrameSize;
///
/// # fn example() -> Result<(), Box<dyn std::error::Error>> {
/// // The default ceiling, with a smaller read buffer.
/// let config = StdioConfig::new(MaxFrameSize::new(64 * 1024)?).with_read_chunk(4096);
/// assert_eq!(config.read_chunk().get(), 4096);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StdioConfig {
    limit: MaxFrameSize,
    read_chunk: NonZeroUsize,
}

impl StdioConfig {
    /// The default configuration: a [`MaxFrameSize::DEFAULT`] ceiling and a
    /// [`DEFAULT_READ_CHUNK`] read buffer.
    #[must_use]
    pub const fn new(limit: MaxFrameSize) -> Self {
        Self {
            limit,
            read_chunk: DEFAULT_READ_CHUNK,
        }
    }

    /// Caps how many bytes one read from the peer's stream may ask for.
    ///
    /// Zero is treated as one byte rather than refused. A zero-length buffer
    /// would make `read` return `Ok(0)`, which every reader in the `AsyncRead`
    /// contract reserves for end-of-stream — so honouring it would silently
    /// turn the peer into a closed stream on the first poll. Coercing to one
    /// byte keeps the loop making progress and costs nothing but syscalls.
    #[must_use]
    pub const fn with_read_chunk(mut self, bytes: usize) -> Self {
        self.read_chunk = match NonZeroUsize::new(bytes) {
            Some(chunk) => chunk,
            None => NonZeroUsize::MIN,
        };
        self
    }

    /// The largest frame this transport will read or write.
    #[must_use]
    pub const fn limit(self) -> MaxFrameSize {
        self.limit
    }

    /// How many bytes a single read may ask for.
    #[must_use]
    pub const fn read_chunk(self) -> NonZeroUsize {
        self.read_chunk
    }
}

impl Default for StdioConfig {
    /// The 4 MiB [`MaxFrameSize::DEFAULT`] ceiling.
    ///
    /// Not a const impl, because `Default::default` is not a const fn and the
    /// project's rules prefer the trait over a hand-written constructor that
    /// only exists to be called from another one.
    fn default() -> Self {
        Self::new(MaxFrameSize::DEFAULT)
    }
}

/// Why the stdio transport could not carry a frame.
///
/// Deliberately not [`crate::application::jsonrpc::SourceError`] or
/// [`crate::application::session::SinkError`]. Those are the *ports*, and they
/// carry no detail on purpose: the application layer is protocol-neutral and
/// must not learn that a broken pipe and an oversized reply are different
/// failures. This is the transport's own account, and it is what a caller
/// driving [`StdioSource`] or [`StdioSink`] directly actually learns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum StdioError {
    /// The peer's stream refused a read or a write.
    ///
    /// Carries no `io::Error` on purpose, for the reason the ports do not: a
    /// caller in a position to do something about *which* failure it was
    /// inspects its own transport, and one that is not has nothing useful to do
    /// with the difference.
    #[error("the peer's stream could not be read or written")]
    Transport,

    /// A reply was larger than the transport is configured to write.
    ///
    /// Terminal, and not a truncated write. The document does not fit a frame,
    /// so there is no frame to write, and writing the bytes anyway would put a
    /// line on the wire that the peer's decoder must then reject as malformed —
    /// a protocol error caused by our own answer, reported as a protocol error
    /// from the peer's side, with nothing in this process to connect the two.
    #[error("a reply of {len} bytes exceeds the configured maximum of {limit}")]
    ReplyTooLarge {
        /// The ceiling the caller configured.
        limit: MaxFrameSize,
        /// The length of the reply that was refused, in bytes.
        len: usize,
    },
}
