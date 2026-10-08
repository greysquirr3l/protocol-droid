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

use crate::domain::jsonrpc::WireId;

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

    /// This id from the wire form, where a message with no id has none.
    ///
    /// Lives here rather than in a transport adapter because it is a mapping
    /// between the two representations of one value, and the stdio loop and the
    /// HTTP router both need it. When it lived in the stdio adapter the HTTP
    /// path had two options and both were wrong: copy it, and the two copies
    /// drift; or reach into another adapter, and the layering breaks.
    ///
    /// `None` — a notification — becomes id `0`. That is not a claim the peer
    /// made; it exists because [`RequestContext`] insists on an id to
    /// correlate against, and a notification is given neither a progress token
    /// nor a cancellation token, so nothing it does can reach the peer and the
    /// value is unobservable. It is written back only if something is
    /// genuinely broken.
    ///
    /// [`RequestContext`]: crate::application::server::context::RequestContext
    ///
    /// # Examples
    ///
    /// ```
    /// # use protocol_droid::domain::RequestId;
    /// # use protocol_droid::domain::jsonrpc::WireId;
    /// let numeric = RequestId::from_wire(Some(&WireId::Number(7)));
    /// let text = RequestId::from_wire(Some(&WireId::Text(std::borrow::Cow::Borrowed("a-1"))));
    /// // A notification has no id, and gets the unobservable placeholder.
    /// let none = RequestId::from_wire(None);
    ///
    /// assert_eq!(numeric, RequestId::number(7));
    /// assert_eq!(text, RequestId::text("a-1"));
    /// assert_eq!(none, RequestId::number(0));
    /// ```
    #[must_use]
    pub fn from_wire(id: Option<&WireId<'_>>) -> Self {
        match id {
            Some(WireId::Number(value)) => Self::Number(*value),
            Some(WireId::Text(text)) => Self::text(text.as_ref()),
            None => Self::Number(0),
        }
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
            // Characters, not bytes. A byte index of 4 lands *inside* the
            // fourth character of any multi-byte id, and a request id is
            // whatever UTF-8 the peer put in a JSON string — `["ab€"]` is a
            // perfectly legal id, and slicing it by byte panics here rather
            // than redacting. `clippy::indexing_slicing` does not catch it
            // because the bound is not a literal.
            Self::Text(value) => {
                let head: String = value.chars().take(4).collect();
                format!("#{head}…{}", value.chars().count())
            }
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

    /// Redaction must not panic on an id that is not ASCII.
    ///
    /// A request id is whatever UTF-8 the peer put in a JSON string, so
    /// `["ab€"]` is a legal id — and it is five bytes, with a three-byte
    /// character straddling byte four. Taking `&value[..len.min(4)]` landed
    /// inside that character and panicked, which turned "log this id" into a
    /// crash the peer chose. Every non-ASCII id of length 4 to 6 does it.
    #[test]
    fn redacting_a_multi_byte_id_does_not_panic() {
        for id in ["ab€", "€€€", "工具调用", "café", "🎉ok", "a€b€"] {
            let redacted = RequestId::text(id).redacted();
            assert!(redacted.starts_with('#'), "{id:?} redacted to {redacted:?}");
            assert!(redacted.contains('…'), "{id:?} redacted to {redacted:?}");
        }
    }

    /// The length in a redaction is a character count, not a byte count.
    ///
    /// Stated because the obvious implementation reports bytes, and the two
    /// disagree for every id outside ASCII — which is what makes it worth a
    /// test rather than a comment.
    #[test]
    fn the_redacted_length_counts_characters() {
        assert_eq!(RequestId::text("🎉ok").redacted(), "#🎉ok…3");
        assert_eq!(RequestId::text("abcdef").redacted(), "#abcd…6");
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
