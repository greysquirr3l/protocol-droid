//! The session aggregate: lifecycle, in-flight requests, cancellation, progress.
//!
//! # Everything here is a pure transition
//!
//! No clock, no runtime, no I/O. Time arrives as an [`Instant`] read from a
//! port the application layer owns; a request becomes in-flight because
//! [`RequestRegistry::register`] was called, and stops being in-flight because
//! something completed or cancelled it. That is the whole of the domain's
//! contribution, and keeping it to that is what makes the state machine
//! testable by a property test rather than by a mock.
//!
//! # The `std::sync` in [`cancellation`]
//!
//! [`cancellation::CancellationToken`] holds a `Mutex<Vec<Waker>>` and a
//! `OnceLock`, which is more machinery than a value object. It is here rather
//! than in `application` for one reason: the in-flight registry has to hold
//! the token, and a domain aggregate may not reach outward to borrow one. The
//! state it holds — *has this request been cancelled, and what did the peer
//! say* — is domain state; the waker list is the standard-library plumbing that
//! publishes it to whoever is awaiting. No `Waker` is invented by this crate,
//! and no executor is named: the token is polled by whoever waits on it, so it
//! works on any runtime, including a test's.
//!
//! # Why there is no timer
//!
//! The domain knows a request's *budget* and can say when it is spent, because
//! that is arithmetic over injected instants. What wakes a future when a
//! deadline passes belongs to the runtime, and the runtime belongs to the
//! transport adapter. The engine therefore computes expiry and the adapter
//! races it — the same division that keeps a transaction inside its adapter.

pub mod cancellation;
pub mod progress;
pub mod registry;
pub mod state;

pub use cancellation::Cancelled;
pub use cancellation::{AbortReason, CancelReason, CancellationToken};
pub use progress::{ProgressCursor, ProgressToken, ProgressUpdate};
pub use registry::{
    CancelOutcome, DuplicateRequest, InFlight, RequestOrigin, RequestRegistry, UnknownRequest,
};
pub use state::{Instant, LifecycleError, LifecycleEvent, Session, SessionId, SessionState};
