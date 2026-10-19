//! The inbound edge: a stream of whole frames from a peer.
//!
//! # Why this trait exists
//!
//! [`FrameSink`](crate::application::session::frame_sink::FrameSink) already covers the outbound half, and it lives in
//! [`crate::application::session`] because the session engine is what consumes
//! it. The inbound half has a different consumer — the loop that reads a frame
//! and routes it — and by the project's rule that a port trait belongs to the
//! consumer, that puts this one here rather than beside its sibling. Two
//! transports facing opposite directions are not one capability, and a shared
//! `ports` module would have implied they were.
//!
//! It is deliberately not a `Transport` trait. A trait that both read and wrote
//! would force every implementor to answer for both, which is exactly the
//! "three adapters wearing one name" objection that
//! [`FrameSink`](crate::application::session::frame_sink::FrameSink) is documented against.
//! The stdio server writes to a child it spawned and reads from the same pipe;
//! the bridge reads from an HTTP body and writes to stdio; a replay source only
//! reads at all.
//!
//! # What it hides
//!
//! Chunking. An implementor is given no way to express "I have half a message",
//! and cannot: whatever a read returns goes into a
//! [`FrameDecoder`](crate::domain::FrameDecoder), and a frame comes out when it
//! is complete. A caller of this trait never sees a partial line, never has to
//! know that a read boundary can fall inside a UTF-8 character, and never has to
//! know what the ceiling is — although the implementor does, and is the one that
//! must set it.
//!
//! # The distinction this trait has to be careful about
//!
//! `Ok(None)` means **the peer closed**, and nothing else. It does not mean "no
//! frame is ready". The decoder underneath answers `None` for exactly that
//! second thing, so the two meanings are opposites and an implementor that
//! forwards the decoder's answer straight up has inverted them. An implementor
//! awaits more bytes instead; this method resolves only on a complete frame, an
//! end of stream, or a failure, and a caller can therefore treat `None` as "the
//! session is over" without also having to ask whether anything was pending.
//!
//! # Two traits, because one of them cannot be both
//!
//! [`FrameSource`] is a native `async fn`, which is a return-position impl trait
//! and therefore **not** object-safe. A dispatch loop that is generic over its
//! source takes `impl FrameSource` and the call monomorphises to nothing. The
//! CLI bridge, which holds a source of an unknown concrete type behind one
//! handle, needs `Arc<dyn DynFrameSource>` and cannot have it from this trait.
//!
//! [`DynFrameSource`] is the object-safe spelling, written out by hand. A
//! blanket impl with `async-trait` would put a boxed future on the generic path
//! too, and one allocation per inbound frame is what the generic form exists to
//! avoid. The box is confined to the callers that asked for it.
//!
//! # Examples
//!
//! ```
//! use std::sync::Arc;
//!
//! use protocol_droid::application::jsonrpc::{DynFrameSource, FrameSource, SourceError};
//! use protocol_droid::domain::{FrameDecoder, MaxFrameSize};
//!
//! /// A source backed by a fixed buffer: everything a read would have produced.
//! struct Replay {
//!     decoder: std::sync::Mutex<FrameDecoder>,
//! }
//!
//! impl Replay {
//!     fn new(wire: &[u8]) -> Self {
//!         let mut decoder = FrameDecoder::new(MaxFrameSize::DEFAULT);
//!         // A test's fixture is trusted, so a refusal here is the fixture's
//!         // problem and is ignored rather than unwrapped. A real implementor
//!         // would have nowhere to put the error either — it has no frame to
//!         // report it against — and would surface it on the first `next_frame`.
//!         let _ = decoder.push(wire);
//!         Self {
//!             decoder: std::sync::Mutex::new(decoder),
//!         }
//!     }
//! }
//!
//! impl FrameSource for Replay {
//!     async fn next_frame(&self) -> Result<Option<String>, SourceError> {
//!         let mut decoder = self.decoder.lock().map_err(|_| SourceError::Transport)?;
//!         decoder
//!             .next_frame()
//!             .map(|frame| frame.map(str::to_owned))
//!             .map_err(SourceError::from)
//!     }
//! }
//!
//! let runtime = tokio::runtime::Builder::new_current_thread()
//!     .build()
//!     .map_err(|error| error.to_string())?;
//!
//! // The generic form: the dispatch loop is written once and monomorphises.
//! let generic = Replay::new(b"{\"jsonrpc\":\"2.0\"}\n");
//! assert_eq!(
//!     runtime.block_on(FrameSource::next_frame(&generic)),
//!     Ok(Some(String::from("{\"jsonrpc\":\"2.0\"}")))
//! );
//!
//! // The boxed form: the same implementation, behind a trait object.
//! let boxed: Arc<dyn DynFrameSource> = Arc::new(Replay::new(b"{\"id\":1}\n"));
//! assert_eq!(
//!     runtime.block_on(boxed.next_frame()),
//!     Ok(Some(String::from("{\"id\":1}")))
//! );
//! # Ok::<(), String>(())
//! ```

use core::future::Future;
use core::pin::Pin;

use thiserror::Error;

use crate::domain::FramingError;

/// An inbound frame stream from a peer.
///
/// A stream of whole frames, protocol-neutral: nothing here names a method, a
/// revision, or a message kind, because routing a frame to a method is the next
/// layer's job and the next layer is the one that knows the protocol.
///
/// Implemented by a transport. Consumed by the loop that routes. One method, for
/// the same reason [`FrameSink`](crate::application::session::frame_sink::FrameSink) has
/// one.
///
/// # Examples
///
/// ```
/// use protocol_droid::application::jsonrpc::{FrameSource, SourceError};
///
/// /// A source with nothing behind it, which reports a closed stream at once.
/// struct Closed;
///
/// impl FrameSource for Closed {
///     async fn next_frame(&self) -> Result<Option<String>, SourceError> {
///         Ok(None)
///     }
/// }
/// ```
pub trait FrameSource {
    /// Waits for the peer's next frame.
    ///
    /// # Errors
    ///
    /// [`SourceError`] when the frame could not be read, or when the bytes that
    /// arrived were not a frame. An implementor that has already failed may
    /// return the error for every later call; a caller does not assume a failure
    /// is permanent.
    ///
    /// No `+ '_` on the return: edition 2024 already captures every in-scope
    /// lifetime in an RPIT, so spelling one out is not merely redundant — it is
    /// a bound the implementor would fail to satisfy.
    fn next_frame(&self) -> impl Future<Output = Result<Option<String>, SourceError>> + Send;
}

/// A [`FrameSource`] that can sit behind a trait object.
///
/// The hand-written spelling of the same method, and the only reason
/// [`FrameSource`] does not use `async-trait`: the box belongs to the callers
/// that need dynamic dispatch, not to the ones that do not.
pub trait DynFrameSource: Send + Sync {
    /// Waits for the peer's next frame.
    ///
    /// # Errors
    ///
    /// As [`FrameSource::next_frame`].
    fn next_frame<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<Option<String>, SourceError>> + Send + 'a>>;
}

impl<T> DynFrameSource for T
where
    T: FrameSource + Send + Sync,
{
    fn next_frame<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<Option<String>, SourceError>> + Send + 'a>> {
        Box::pin(FrameSource::next_frame(self))
    }
}

/// Why the next inbound frame did not arrive.
///
/// Domain types only. An adapter maps its own `io::Error` into
/// [`Transport`](Self::Transport) at the boundary, so nothing in `application/`
/// names a type from a runtime or from the operating system.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SourceError {
    /// The transport could not read.
    ///
    /// Carries nothing, and that is the point. Every adapter has its own failures
    /// — a broken pipe, a reset, a child that exited, an HTTP body that ended
    /// early — and rendering any of them into a `String` an implementor controls
    /// would put a peer's bytes on the application layer's error path for
    /// nothing. A caller that needs to know *which* failure maps its own.
    #[error("the transport could not read the next frame")]
    Transport,

    /// The bytes on the wire were not a frame.
    ///
    /// A peer that will not terminate its line, or that sent something that is
    /// not text, is a fact about the peer rather than about the pipe, and it is
    /// worth distinguishing from a transport that simply failed.
    #[error("the peer's bytes are not a valid frame: {0}")]
    Framing(#[from] FramingError),
}

#[cfg(test)]
mod tests {
    use core::future::Future;
    use core::task::{Context, Poll, Waker};
    use std::sync::Arc;

    use assert_matches::assert_matches;

    use super::{DynFrameSource, FrameSource, SourceError};
    use crate::domain::{FrameDecoder, FramingError, MaxFrameSize};

    /// A source over a fixed buffer, with the framing done by the real decoder so
    /// the port is tested against the code a transport would use.
    struct Replay {
        decoder: std::sync::Mutex<FrameDecoder>,
    }

    impl Replay {
        fn new(wire: &[u8]) -> Self {
            let mut decoder =
                FrameDecoder::new(MaxFrameSize::new(1024).unwrap_or(MaxFrameSize::DEFAULT));
            let _ = decoder.push(wire);
            Self {
                decoder: std::sync::Mutex::new(decoder),
            }
        }
    }

    impl FrameSource for Replay {
        #[expect(
            clippy::unused_async_trait_impl,
            reason = "FrameSource is a native async fn trait, so the signature is the trait's and not the fixture's to change; locking a std mutex and polling the decoder are both synchronous, which is the whole point of the fixture"
        )]
        async fn next_frame(&self) -> Result<Option<String>, SourceError> {
            let mut decoder = self.decoder.lock().map_err(|_| SourceError::Transport)?;
            decoder
                .next_frame()
                .map(|frame| frame.map(str::to_owned))
                .map_err(SourceError::from)
        }
    }

    /// A source whose transport has already failed.
    ///
    /// [`Replay`] can only ever reach [`SourceError::Framing`], because the real
    /// decoder turns a failed read into a typed framing answer. The unit
    /// variant needs a source of its own, and this is the one an adapter looks
    /// like after its pipe breaks.
    struct Broken;

    impl FrameSource for Broken {
        #[expect(
            clippy::unused_async_trait_impl,
            reason = "FrameSource is a native async fn trait, so the signature is the trait's and not the fixture's to change; there is nothing to await in reporting a failure that has already happened"
        )]
        async fn next_frame(&self) -> Result<Option<String>, SourceError> {
            Err(SourceError::Transport)
        }
    }

    /// Drives a future to its first poll. `None` means it suspended, which these
    /// sources never do — a future that did would need a real executor, and
    /// reporting `None` is more honest than pretending it completed.
    fn drive<F: Future>(future: F) -> Option<F::Output> {
        let mut context = Context::from_waker(Waker::noop());
        let mut future = Box::pin(future);
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => Some(value),
            Poll::Pending => None,
        }
    }

    #[test]
    fn a_frame_arrives_through_the_generic_spelling() {
        let source = Replay::new(b"{\"jsonrpc\":\"2.0\"}\n");
        assert_eq!(
            drive(FrameSource::next_frame(&source)),
            Some(Ok(Some(String::from("{\"jsonrpc\":\"2.0\"}"))))
        );
    }

    #[test]
    fn the_boxed_spelling_forwards_to_the_same_implementation() {
        let source = Replay::new(b"{\"jsonrpc\":\"2.0\"}\n");
        let boxed: Arc<dyn DynFrameSource> = Arc::new(source);
        assert_eq!(
            drive(boxed.next_frame()),
            Some(Ok(Some(String::from("{\"jsonrpc\":\"2.0\"}"))))
        );
    }

    /// The whole reason `Ok(None)` is documented as "the peer closed" and not
    /// "nothing is ready": the decoder underneath says `None` for exactly the
    /// second thing, and the port must not pass that answer up.
    #[test]
    fn a_partially_read_frame_is_not_reported_as_a_closed_stream() {
        let source = Replay::new(b"{\"jsonrpc\":\"2.0\"}");
        assert_eq!(drive(FrameSource::next_frame(&source)), Some(Ok(None)));
        // The fixture holds no terminator at all, so a faithful implementor
        // would await more bytes. What must not happen is it answering "the peer
        // closed" as though a partial read were a completed session, and this
        // test is the reminder that the two answers are opposites.
    }

    #[test]
    fn a_framing_refusal_keeps_its_own_type_through_the_port() {
        let source = Replay::new(b"{\"a\":\"\xff\"}\n");
        assert_matches!(
            drive(FrameSource::next_frame(&source)),
            Some(Err(SourceError::Framing(FramingError::NotUtf8 { .. })))
        );
    }

    #[test]
    fn a_transport_failure_is_a_domain_error_and_names_no_third_party() {
        // The point of the unit variant: an adapter's `io::Error` is mapped here
        // and the application layer never sees a type it does not own.
        assert_eq!(
            SourceError::Transport.to_string(),
            "the transport could not read the next frame"
        );
    }

    /// [`SourceError::Transport`] is the one answer that no test above produces
    /// on its way through the port: `Replay` can only ever reach
    /// [`SourceError::Framing`], because the real decoder turns every byte-level
    /// refusal into a typed framing error. The unit variant is the adapter's to
    /// build — a broken pipe has no bytes to frame — so it needs a source of its
    /// own here.
    #[test]
    fn a_failed_read_stays_failed_and_never_degrades_into_a_closed_stream() {
        let source = Broken;

        // Asked twice, because the second call is the assertion that carries
        // weight. A source that answered `Ok(None)` after a failure would be
        // reporting a clean close, and `Ok(None)` means exactly one thing: the
        // peer went away. A caller cannot tell a broken transport from a polite
        // goodbye on the wire, so a failure has to keep being reported rather
        // than settle into the other answer.
        for attempt in 0..2 {
            assert_eq!(
                drive(FrameSource::next_frame(&source)),
                Some(Err(SourceError::Transport)),
                "attempt {attempt}"
            );
        }
    }
}
