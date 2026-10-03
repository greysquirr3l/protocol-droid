//! `RequestId` — a JSON-RPC request id.
//!
//! # Why it is the one newtype that is not `Copy`
//!
//! Every other newtype in this crate is `Copy` because all its fields are.
//! `RequestId` wraps an `Arc<str>` for the string form, and duplicating a
//! string id is not a thing anyone means to do: two "copies" of id `7` are the
//! same request, and a function that took one by value when it only needed to
//! read it should take a reference. The asymmetry is the signal.
//!
//! # Why an `Arc<str>` rather than a `String`
//!
//! A request id is read at least twice on every message — once to match a
//! result back to its request, once to log or redact it — and on the JSON-RPC
//! hot path that allocation is the expensive part. `Arc<str>` makes the
//! long-lived case (store on the in-flight map, read when the response lands)
//! a refcount bump instead of a copy, and it is one allocation at construction
//! rather than one per read.

use std::fmt;
use std::sync::Arc;

/// A JSON-RPC request id: a number or a string.
///
/// Deliberately *not* `Copy` — see the module docs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RequestId {
    /// A numeric id, which the spec says must not contain fractional parts.
    Number(i64),
    /// A string id.
    Text(Arc<str>),
}

impl RequestId {
    /// A numeric id.
    #[must_use]
    pub const fn number(value: i64) -> Self {
        Self::Number(value)
    }

    /// A string id.
    #[must_use]
    pub fn text(value: impl AsRef<str>) -> Self {
        Self::Text(Arc::from(value.as_ref()))
    }

    /// The id as it appears on the wire, as JSON.
    ///
    /// A string id is written with its surrounding quotes so the result can be
    /// pasted into a JSON document without the caller having to know which arm
    /// it got.
    #[must_use]
    pub fn to_json(&self) -> String {
        match self {
            Self::Number(value) => value.to_string(),
            Self::Text(value) => format!("{value:?}"),
        }
    }

    /// A short label for logs, which never gets the full id.
    ///
    /// Ids are redacted for the same reason tokens are: they are peer-chosen
    /// and can carry anything. This gives correlation without disclosure.
    #[must_use]
    pub fn redacted(&self) -> String {
        match self {
            Self::Number(value) => format!("#{value}"),
            Self::Text(value) => format!("#{}…{}", &value[..value.len().min(4)], value.len()),
        }
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(value) => write!(formatter, "{value}"),
            Self::Text(value) => formatter.write_str(value),
        }
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::panic,
        reason = "tests panic on the failing precondition, which is the message worth having"
    )]

    use super::RequestId;
    use std::sync::Arc;

    #[test]
    fn numbers_and_texts_compare_by_value() {
        assert_eq!(RequestId::number(7), RequestId::number(7));
        assert_ne!(RequestId::number(7), RequestId::number(8));
        assert_eq!(RequestId::text("a"), RequestId::text("a"));
        assert_ne!(RequestId::number(7), RequestId::text("7"));
        // Two separately-allocated strings are still equal, which is what
        // makes the id usable as a map key.
        assert_eq!(RequestId::text("abc"), RequestId::text(String::from("abc")));
    }

    #[test]
    fn text_ids_are_refcounted_not_copied() {
        let a = RequestId::text("shared");
        let b = a.clone();
        match (&a, &b) {
            (RequestId::Text(left), RequestId::Text(right)) => {
                assert!(Arc::ptr_eq(left, right), "clone copied the string");
            }
            _ => panic!("expected text ids"),
        }
    }

    #[test]
    fn to_json_quotes_only_string_ids() {
        assert_eq!(RequestId::number(7).to_json(), "7");
        assert_eq!(RequestId::number(-1).to_json(), "-1");
        assert_eq!(RequestId::text("abc").to_json(), r#""abc""#);
    }

    /// Ids go into logs, so the redacted form has to correlate without
    /// disclosing a peer-chosen string.
    #[test]
    fn redaction_correlates_without_disclosing() {
        assert_eq!(RequestId::number(7).redacted(), "#7");
        let redacted = RequestId::text("secret-token-value").redacted();
        assert!(redacted.starts_with("#sec"), "{redacted}");
        assert!(!redacted.contains("secret-token-value"), "{redacted}");
        // A short id still works rather than panicking on a short slice.
        assert!(RequestId::text("ab").redacted().starts_with("#ab"));
    }

    #[test]
    fn display_is_the_bare_value() {
        assert_eq!(RequestId::number(7).to_string(), "7");
        assert_eq!(RequestId::text("abc").to_string(), "abc");
    }

    #[test]
    fn ids_sort_deterministically() {
        // Numbers before texts, so a map iteration is reproducible.
        let mut ids = vec![
            RequestId::text("b"),
            RequestId::number(2),
            RequestId::text("a"),
            RequestId::number(1),
        ];
        ids.sort();
        assert_eq!(
            ids,
            vec![
                RequestId::number(1),
                RequestId::number(2),
                RequestId::text("a"),
                RequestId::text("b"),
            ]
        );
    }
}
