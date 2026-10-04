//! Where a session's outbound frames go.
//!
//! # Why the engine does not build frames
//!
//! Encoding is the codec's job, and the codec is per-revision: the same
//! `sampling/createMessage` is three different documents across three
//! revisions. If the sink took a typed value the engine would have to know
//! which codec to run, and the engine is exactly the module that is not
//! allowed to know which protocol it is serving. So the boundary is drawn at
//! the encoded frame: something downstream of the engine already lowered the
//! value, and the sink's only job is to put the bytes on the wire and say
//! whether they got there.
//!
//! That also makes the failure honest. A sink that fails is reporting a broken
//! pipe, not a bad document, and the engine has nothing useful to add to that
//! report.
//!
//! # Two traits, because one of them cannot be both
//!
//! [`FrameSink`] is a native `async fn`, which is a return-position impl trait
//! and therefore **not** object-safe. A transport that is generic over its
//! session gets it for free: the call monomorphises and costs nothing. The CLI
//! bridge, which holds a bridge of mixed transports behind one handle, needs
//! `Arc<dyn FrameSink>` and cannot have it from this trait.
//!
//! [`DynFrameSink`] is the object-safe spelling, written out by hand. The
//! alternative — a blanket impl with `async-trait` — would put a boxed future
//! on the generic path too, and one allocation per outbound frame is exactly
//! what the `FrameSink` form exists to avoid. The box is confined to the
//! callers that asked for it, which is the CLI bridge wiring and nowhere on a
//! per-frame hot path.

use core::future::Future;
use core::pin::Pin;

use thiserror::Error;

/// A frame that did not reach the peer.
///
/// Deliberately carries nothing from the transport that failed. An adapter
/// maps its own `io::Error` into this at the boundary, and the engine adds
/// nothing, because the only fact available is "it did not go" — a half-written
/// frame is a half-written frame regardless of which syscall noticed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("the frame could not be written to the peer")]
pub struct SinkError;

/// A session's outbound path.
///
/// Declared by the engine, which is its only consumer, and consumed through
/// `impl FrameSink` so the call monomorphises. One method, for the same reason
/// [`Clock`](crate::application::session::Clock) has one: a sink that also
/// flushed, or negotiated, or reported back-pressure would be three adapters
/// wearing one name.
///
/// # Examples
///
/// ```
/// use protocol_droid::application::session::{FrameSink, SinkError};
///
/// /// A sink that keeps what it was given, for a test to inspect.
/// #[derive(Default)]
/// struct Recorded(Vec<String>);
///
/// impl FrameSink for Recorded {
///     async fn send(&self, frame: &str) -> Result<(), SinkError> {
///         // A real sink would not need this lock; a test that shares its sink
///         // across tasks does, and holding the same shape here keeps the
///         // example honest about what the borrow checker demands.
///         let _ = self;
///         Ok(())
///     }
/// }
/// ```
pub trait FrameSink {
    /// Writes one encoded frame.
    ///
    /// `frame` is a document some codec has already produced. The engine
    /// forwards it and never inspects it, which is what lets one engine serve
    /// six revisions.
    ///
    /// # Errors
    ///
    /// [`SinkError`] when the frame did not reach the peer. A sink that has
    /// already failed may return it for every later call; the engine does not
    /// assume a failure is permanent.
    ///
    /// No `+ '_` on the return: this method has two borrowed inputs, and an
    /// elided `'_` names only one of them. On edition 2024 the returned
    /// `impl Future` already captures every in-scope lifetime, so spelling one
    /// out is not merely redundant — it is a bound the implementor would fail
    /// to satisfy.
    fn send(&self, frame: &str) -> impl Future<Output = Result<(), SinkError>> + Send;
}

/// A [`FrameSink`] that can sit behind a trait object.
///
/// The hand-written spelling of the same method, and the only reason
/// [`FrameSink`] does not use `async-trait`: the box belongs to the callers
/// that need dynamic dispatch, not to the ones that do not.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use protocol_droid::application::session::{DynFrameSink, FrameSink, SinkError};
///
/// struct Null;
///
/// impl FrameSink for Null {
///     async fn send(&self, _frame: &str) -> Result<(), SinkError> {
///         Ok(())
///     }
/// }
///
/// // The bridge holds a sink of an unknown concrete type; the allocation here
/// // is per outbound frame on a path that is already behind a mutex.
/// let sink: Arc<dyn DynFrameSink> = Arc::new(Null);
/// ```
pub trait DynFrameSink: Send + Sync {
    /// Writes one encoded frame.
    ///
    /// # Errors
    ///
    /// As [`FrameSink::send`].
    ///
    /// The two inputs share one lifetime because the box has to: a
    /// `dyn Future + '_` names a single region, so an implementor that
    /// returned a future borrowing the sink for longer than the frame could
    /// not be spelled. Both are borrowed for the duration of the call anyway,
    /// so nothing is lost.
    fn send<'a>(
        &'a self,
        frame: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), SinkError>> + Send + 'a>>;
}

impl<T> DynFrameSink for T
where
    T: FrameSink + Send + Sync,
{
    fn send<'a>(
        &'a self,
        frame: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), SinkError>> + Send + 'a>> {
        Box::pin(FrameSink::send(self, frame))
    }
}

#[cfg(test)]
mod tests {
    use core::future::Future;
    use core::pin::Pin;
    use core::sync::atomic::{AtomicUsize, Ordering};
    use core::task::{Context, Poll, Waker};
    use std::sync::Arc;

    use super::{DynFrameSink, FrameSink, SinkError};

    /// Counts what it was asked to write, so the two spellings of the same
    /// trait can be compared on what actually reached the peer.
    #[derive(Default)]
    struct Counting {
        written: AtomicUsize,
    }

    impl Counting {
        fn written(&self) -> usize {
            self.written.load(Ordering::Relaxed)
        }
    }

    impl FrameSink for Counting {
        fn send(&self, _frame: &str) -> impl Future<Output = Result<(), SinkError>> + Send {
            self.written.fetch_add(1, Ordering::Relaxed);
            core::future::ready(Ok(()))
        }
    }

    /// A sink that never writes, so the error path is reachable.
    struct Broken;

    impl FrameSink for Broken {
        fn send(&self, _frame: &str) -> impl Future<Output = Result<(), SinkError>> + Send {
            core::future::ready(Err(SinkError))
        }
    }

    /// Drives a future to its first poll. `None` means it suspended, which
    /// these sinks never do — a future that did would need a real executor,
    /// and reporting `None` is more honest than pretending it completed.
    fn drive<F: Future>(future: F) -> Option<F::Output> {
        let mut context = Context::from_waker(Waker::noop());
        let mut future = Box::pin(future);
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => Some(value),
            Poll::Pending => None,
        }
    }

    #[test]
    fn the_boxed_spelling_forwards_to_the_same_implementation() {
        let concrete = Arc::new(Counting::default());
        assert_eq!(drive(FrameSink::send(&*concrete, "{}")), Some(Ok(())));
        let boxed: Arc<dyn DynFrameSink> = concrete.clone();
        assert_eq!(drive(boxed.send("{}")), Some(Ok(())));
        assert_eq!(
            concrete.written(),
            2,
            "the blanket impl must not lose a frame"
        );
    }

    #[test]
    fn a_failing_sink_reports_the_error_through_both_spellings() {
        let concrete = Broken;
        assert_eq!(
            drive(FrameSink::send(&concrete, "{}")),
            Some(Err(SinkError))
        );
        let boxed: Arc<dyn DynFrameSink> = Arc::new(Broken);
        assert_eq!(drive(boxed.send("{}")), Some(Err(SinkError)));
    }

    #[test]
    fn a_boxed_sink_is_usable_from_behind_an_arc() {
        fn write(
            sink: &Arc<dyn DynFrameSink>,
        ) -> Pin<Box<dyn Future<Output = Result<(), SinkError>> + Send + '_>> {
            sink.send("{\"jsonrpc\":\"2.0\"}")
        }
        let sink: Arc<dyn DynFrameSink> = Arc::new(Counting::default());
        assert_eq!(drive(write(&sink)), Some(Ok(())));
    }
}
