//! Which `notifications/message` records a session has asked to receive.
//!
//! # Two revisions, two owners of the threshold
//!
//! Through `2025-11-25` the threshold belongs to the **session**:
//! `logging/setLevel` sets it once and every later record is filtered against
//! it. `2026-07-28` moved it onto the **request**: SEP-2575 replaced the RPC
//! with the `io.modelcontextprotocol/logLevel` key in `_meta`, and a request
//! that omits the key receives no log records at all — "the server MUST NOT
//! send any `notifications/message` notifications for this request".
//!
//! One type serves both because the question a handler asks is the same:
//! "will this record be read?". The gate is shared by every clone of a peer,
//! so a stateful session's `setLevel` reaches the requests that follow it, and
//! a stateless request's gate is built fresh from its own `_meta`, so no
//! request can inherit another's.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::domain::ir::LogLevel;

/// Every record passes.
const OPEN: u8 = 0;
/// No record passes.
const CLOSED: u8 = u8::MAX;

/// A session's or a request's log threshold.
///
/// Cloning shares the threshold, which is the point: a stateful session's
/// peer is cloned into every request it serves.
#[derive(Debug, Clone)]
pub struct LogGate {
    threshold: Arc<AtomicU8>,
}

impl LogGate {
    /// A gate every record passes: a stateful session before any `setLevel`.
    #[must_use]
    pub fn open() -> Self {
        Self::stored(OPEN)
    }

    /// A gate no record passes: a `2026-07-28` request with no `logLevel`.
    #[must_use]
    pub fn closed() -> Self {
        Self::stored(CLOSED)
    }

    /// A gate passing `level` and everything more severe.
    #[must_use]
    pub fn at(level: LogLevel) -> Self {
        Self::stored(encode(level))
    }

    /// The gate a `2026-07-28` request's `logLevel` key describes.
    #[must_use]
    pub fn for_request(level: Option<LogLevel>) -> Self {
        level.map_or_else(Self::closed, Self::at)
    }

    /// Moves the threshold, for `logging/setLevel`.
    pub fn set(&self, level: LogLevel) {
        self.threshold.store(encode(level), Ordering::Relaxed);
    }

    /// Whether a record at `level` should be sent.
    #[must_use]
    pub fn admits(&self, level: LogLevel) -> bool {
        match self.threshold.load(Ordering::Relaxed) {
            OPEN => true,
            CLOSED => false,
            threshold => encode(level) >= threshold,
        }
    }

    fn stored(value: u8) -> Self {
        Self {
            threshold: Arc::new(AtomicU8::new(value)),
        }
    }
}

impl Default for LogGate {
    fn default() -> Self {
        Self::open()
    }
}

/// `1..=8`, so `0` and `u8::MAX` stay free for the open and closed states.
fn encode(level: LogLevel) -> u8 {
    let index = LogLevel::ALL
        .iter()
        .position(|candidate| *candidate == level)
        .unwrap_or_default();
    u8::try_from(index).map_or(1, |index| index.saturating_add(1))
}

#[cfg(test)]
mod tests {
    use super::LogGate;
    use crate::domain::ir::LogLevel;

    #[test]
    fn an_open_gate_admits_everything() {
        assert!(LogGate::open().admits(LogLevel::Debug));
    }

    #[test]
    fn a_closed_gate_admits_nothing() {
        assert!(!LogGate::closed().admits(LogLevel::Emergency));
    }

    #[test]
    fn a_threshold_admits_its_level_and_above() {
        let gate = LogGate::at(LogLevel::Warning);
        assert!(!gate.admits(LogLevel::Info));
        assert!(gate.admits(LogLevel::Warning));
        assert!(gate.admits(LogLevel::Error));
    }

    #[test]
    fn a_request_without_a_level_is_closed() {
        assert!(!LogGate::for_request(None).admits(LogLevel::Emergency));
        assert!(LogGate::for_request(Some(LogLevel::Debug)).admits(LogLevel::Debug));
    }

    #[test]
    fn set_reaches_every_clone() {
        let gate = LogGate::open();
        let clone = gate.clone();
        gate.set(LogLevel::Error);
        assert!(!clone.admits(LogLevel::Warning));
        assert!(clone.admits(LogLevel::Error));
    }

    #[test]
    fn the_stored_order_agrees_with_the_level_order() {
        for (lower, higher) in LogLevel::ALL.iter().zip(LogLevel::ALL.iter().skip(1))
        {
            assert!(lower < higher, "{lower} sorts below {higher}");
            assert!(super::encode(*lower) < super::encode(*higher));
        }
    }
}
