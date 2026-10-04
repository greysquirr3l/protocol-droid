//! Progress: the opaque token a request carries, and the cursor that guards it.
//!
//! # The token is the peer's, not ours
//!
//! The schema is unambiguous — `ProgressToken = string | number` in every
//! revision from 2024-11-05 to 2026-07-28, and the value is described as
//! opaque. This crate therefore never interprets it, never derives one, and
//! never substitutes a value of its own: it is carried through the request, out
//! onto each notification, and matched back. A token this side invented would
//! be a token the peer cannot correlate, which is the one outcome the mechanism
//! exists to prevent.
//!
//! # Why the cursor exists
//!
//! The specification says progress "should increase every time progress is
//! made, even if the total is unknown". That is a SHOULD, so this side does not
//! reject the request for violating it — it declines to emit the update. A peer
//! that renders progress as a bar would otherwise be able to walk backwards
//! because a handler recomputed a percentage, and the notification is
//! out-of-band, so nothing upstream would notice.

use core::fmt;
use std::borrow::Cow;
use std::sync::Arc;

use serde_json::Value;

/// The opaque handle a request carries to ask for progress updates.
///
/// # Why this is the second non-`Copy` newtype
///
/// [`crate::domain::RequestId`] is not `Copy` because it wraps an `Arc<str>`,
/// and this does too, for the same reason: a progress token is stored on the
/// in-flight entry when the request arrives and read again for every update
/// until it completes, so a notification re-uses the request's allocation
/// instead of making one. `RequestId`'s doc comment says it is the only such
/// newtype; this is the one exception, and it is the same reason rather than a
/// new one — a token duplicated by value is a correlation the peer did not ask
/// for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProgressToken {
    /// A numeric token.
    Number(i64),
    /// A string token.
    Text(Arc<str>),
}

impl ProgressToken {
    /// A numeric token.
    #[must_use]
    pub const fn number(value: i64) -> Self {
        Self::Number(value)
    }

    /// A string token.
    #[must_use]
    pub fn text(value: impl AsRef<str>) -> Self {
        Self::Text(Arc::from(value.as_ref()))
    }

    /// Reads a token out of the JSON a request carried.
    ///
    /// The one place the wire shape is known, and it is the shape the vendored
    /// schema states: a string or a number, nothing else. A boolean, an object
    /// or a fractional number is not a `ProgressToken`, and returning `None`
    /// for one is what lets the request proceed with no progress rather than
    /// failing — the token is a request for updates, not a requirement for the
    /// request to be valid.
    #[must_use]
    pub fn from_json(value: &Value) -> Option<Self> {
        match value {
            Value::String(text) => Some(Self::text(text)),
            Value::Number(number) => number.as_i64().map(Self::number),
            _ => None,
        }
    }

    /// The token as JSON, for a codec to write back.
    ///
    /// A string is quoted, so the result pastes into a document without the
    /// caller needing to know which arm it got. Borrowed rather than owned: the
    /// codec serialises it into a buffer it already holds.
    #[must_use]
    pub fn to_json(&self) -> Cow<'_, str> {
        match self {
            Self::Number(value) => Cow::Owned(value.to_string()),
            // `serde_json` rather than `{:?}`: the latter emits Rust escapes,
            // and a token containing a control character would produce a
            // document that is not JSON. The fallback is an empty JSON string
            // because a lossy answer is recoverable and malformed JSON is not.
            Self::Text(value) => {
                serde_json::to_string(value.as_ref()).map_or(Cow::Borrowed("\"\""), Cow::Owned)
            }
        }
    }
}

impl fmt::Display for ProgressToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(value) => write!(formatter, "{value}"),
            Self::Text(value) => formatter.write_str(value),
        }
    }
}

/// One progress update to put on the wire.
#[derive(Debug, Clone, PartialEq)]
pub struct ProgressUpdate {
    /// The token the request carried.
    pub token: ProgressToken,
    /// How far along it is. Increases monotonically.
    pub progress: f64,
    /// How far along it has to be, when the handler knows.
    pub total: Option<f64>,
    /// An optional description of the current step.
    pub message: Option<Cow<'static, str>>,
}

impl ProgressUpdate {
    /// An update with a progress value and nothing else.
    #[must_use]
    pub const fn new(token: ProgressToken, progress: f64) -> Self {
        Self {
            token,
            progress,
            total: None,
            message: None,
        }
    }

    /// Sets the total.
    #[must_use]
    pub const fn with_total(mut self, total: f64) -> Self {
        self.total = Some(total);
        self
    }

    /// Sets the message.
    #[must_use]
    pub fn with_message(mut self, message: impl Into<Cow<'static, str>>) -> Self {
        self.message = Some(message.into());
        self
    }
}

/// The furthest progress reported for one request so far.
///
/// One per in-flight request, held on the in-flight entry, so it is dropped
/// exactly when the request is — there is no table of stale cursors to age out.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ProgressCursor {
    furthest: Option<f64>,
}

impl ProgressCursor {
    /// A cursor that has reported nothing.
    #[must_use]
    pub const fn new() -> Self {
        Self { furthest: None }
    }

    /// Records an update and says whether it should go on the wire.
    ///
    /// `false` for an update that does not move the cursor forward. The first
    /// update always moves it, including one reporting `0.0`, because a request
    /// that said "tell me about this" is owed at least one notification and a
    /// zero-valued first update is how a handler says "started".
    ///
    /// `NaN` is refused rather than compared. It would fail every ordering
    /// test — including the `is_finite` one a reader expects to find here — and
    /// so would wedge the cursor at whatever it last reached, silently
    /// suppressing every later update. A value that is not a number is not
    /// progress.
    #[must_use]
    pub fn advance(&mut self, update: &ProgressUpdate) -> bool {
        let progress = update.progress;
        if !progress.is_finite() {
            return false;
        }
        if let Some(furthest) = self.furthest
            && progress <= furthest
        {
            return false;
        }
        self.furthest = Some(progress);
        true
    }

    /// The furthest value reported so far.
    #[must_use]
    pub const fn furthest(&self) -> Option<f64> {
        self.furthest
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{ProgressCursor, ProgressToken, ProgressUpdate};
    use crate::domain::RequestId;

    fn token() -> ProgressToken {
        ProgressToken::number(1)
    }

    #[test]
    fn the_first_update_always_moves_the_cursor() {
        let mut cursor = ProgressCursor::new();
        assert!(cursor.advance(&ProgressUpdate::new(token(), 0.0)));
        assert_eq!(cursor.furthest(), Some(0.0));
    }

    /// "This should increase every time progress is made, even if the total is
    /// unknown" — the clause the cursor exists to enforce.
    #[test]
    fn a_repeated_or_going_backwards_update_is_not_emitted() {
        let mut cursor = ProgressCursor::new();
        for (value, expected) in [
            (1.0, true),  // the first update
            (2.0, true),  // forward
            (2.0, false), // repeated
            (1.5, false), // backwards
            (2.0, false), // back to the furthest
            (3.0, true),  // forward past the furthest
        ] {
            let emitted = cursor.advance(&ProgressUpdate::new(token(), value));
            assert_eq!(emitted, expected, "progress {value}");
        }
        assert_eq!(cursor.furthest(), Some(3.0));
    }

    #[test]
    fn a_non_finite_progress_is_not_progress() {
        let mut cursor = ProgressCursor::new();
        assert!(!cursor.advance(&ProgressUpdate::new(token(), f64::NAN)));
        assert!(!cursor.advance(&ProgressUpdate::new(token(), f64::INFINITY)));
        assert_eq!(cursor.furthest(), None, "a refused value was recorded");
        // The cursor is still usable, which is the point of refusing rather
        // than wedging.
        assert!(cursor.advance(&ProgressUpdate::new(token(), 1.0)));
    }

    #[test]
    fn a_token_reads_out_of_every_shape_the_schema_allows() {
        assert_eq!(
            ProgressToken::from_json(&serde_json::json!("abc")),
            Some(ProgressToken::text("abc"))
        );
        assert_eq!(
            ProgressToken::from_json(&serde_json::json!(7)),
            Some(ProgressToken::number(7))
        );
    }

    /// Everything else is not a `ProgressToken`, and a request that carries one
    /// of these still has to work — it just gets no progress.
    #[test]
    fn every_other_shape_is_not_a_token() {
        for value in [
            serde_json::json!(true),
            serde_json::json!({ "a": 1 }),
            serde_json::json!([1]),
            serde_json::json!(null),
            // A number outside i64, and a fractional one. The schema says
            // `string | number`; a JSON number that will not fit the id type
            // this crate uses for the same concept is refused rather than
            // wrapped.
            serde_json::json!(1.5),
        ] {
            assert_eq!(ProgressToken::from_json(&value), None, "{value}");
        }
    }

    #[test]
    fn a_token_writes_back_as_the_json_it_arrived_as() {
        assert_eq!(ProgressToken::number(7).to_json(), "7");
        assert_eq!(ProgressToken::text("abc").to_json(), "\"abc\"");
    }

    /// The DEL case from T09, in a different field: `{:?}` is Rust syntax, not
    /// a legal JSON escape, and a token is written into a document verbatim.
    #[test]
    fn a_control_character_in_a_token_survives_into_json() {
        let written = ProgressToken::text("a\u{7f}b").to_json().into_owned();
        // DEL is legal unescaped inside a JSON string, so the document is not
        // escaped — it has to *parse*, and parse back to the same character.
        // A `{:?}`-based `to_json` would emit `a\u{7f}b`, which is Rust
        // syntax and not JSON at all.
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&written).ok(),
            Some(serde_json::json!("a\u{7f}b")),
            "the written document should be JSON carrying the same character: {written:?}"
        );
    }

    /// The two non-`Copy` newtypes are siblings for the same reason, and a
    /// clone of either has to be a refcount bump rather than a copy.
    #[test]
    fn a_cloned_token_shares_its_allocation() {
        let a = ProgressToken::text("shared");
        let b = a.clone();
        assert_matches::assert_matches!(
            (&a, &b),
            (ProgressToken::Text(left), ProgressToken::Text(right))
                if Arc::ptr_eq(left, right),
            "clone copied the string instead of sharing it"
        );
        // And it is usable as a key beside the other opaque handle.
        let mut seen = std::collections::HashMap::new();
        seen.insert(RequestId::text("req"), a);
        assert_eq!(seen.len(), 1);
    }

    #[test]
    fn total_and_message_are_optional_and_attach_once() {
        let update = ProgressUpdate::new(token(), 3.0)
            .with_total(10.0)
            .with_message(String::from("reading"));
        assert_eq!(update.total, Some(10.0));
        assert_eq!(update.message.as_deref(), Some("reading"));
    }
}
