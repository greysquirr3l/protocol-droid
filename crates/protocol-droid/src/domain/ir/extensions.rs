//! `Extensions` — the passthrough bag that makes translation lossless.
//!
//! # The problem this solves
//!
//! A bridge sees every message from both sides, and it does not understand all
//! of them. A `2026-07-28` client can send a `_meta` key this build has never
//! heard of; a `draft` peer can define a capability with no entry in the
//! feature matrix. If the IR is a closed set of fields, that data is dropped at
//! lift time and the peer on the other side of the bridge silently loses it.
//!
//! That is the failure mode this crate is built against, so it gets a first-class
//! type rather than a comment. Anything the IR does not model is captured here,
//! in order, and re-emitted on the other side. The bridge's contract becomes
//! checkable: `lift(v, lower(v, ir)) == ir` is only a meaningful claim if
//! unmodelled fields survive the round trip.
//!
//! # Why an ordered bag and not a map
//!
//! A `BTreeMap<String, ExtensionValue>` would be the obvious choice, and it is
//! worse for this job for two reasons.
//!
//! **Order.** JSON object member order is not semantically significant, but a
//! byte-identical round trip is the goal, and a map re-emits in its own order.
//! Keeping insertion order means the common case — a peer whose fields the IR
//! modelled fine, plus a couple it did not — comes back out looking the way it
//! went in.
//!
//! **Duplicates.** JSON allows a repeated key, and a peer that sends one is
//! telling you something (a merge artefact, or a buggy encoder). A map silently
//! keeps the last. A bag keeps both, and the conflict is visible.
//!
//! # What is in it
//!
//! [`ExtensionValue`], which is deliberately closed: the six JSON shapes, plus
//! [`ExtensionValue::Raw`](ExtensionValue::Raw) for text that failed to parse.
//! `Raw` exists so lift can never lose data to a parse failure — an unparseable
//! value is forwarded as the bytes that were received rather than dropped.

use std::fmt;

/// The value of an unmodelled field.
#[derive(Debug, Clone, PartialEq)]
pub enum ExtensionValue {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number, kept as text so a large integer or a `1.0` survives exactly.
    Number(String),
    /// A string.
    String(String),
    /// A nested object, as a bag in its own right.
    Object(Extensions),
    /// An array, in order.
    Array(Vec<Self>),
    /// Text that did not parse, kept so a parse failure cannot drop data.
    ///
    /// Lowering emits it back verbatim. This is the escape hatch that makes
    /// lift total: there is no input for which the bridge has to throw data
    /// away, which is what "lossless" has to mean to be worth claiming.
    Raw(String),
}

impl ExtensionValue {
    /// A short name for the shape, for logs and error messages.
    #[must_use]
    pub const fn shape(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "bool",
            Self::Number(_) => "number",
            Self::String(_) => "string",
            Self::Object(_) => "object",
            Self::Array(_) => "array",
            Self::Raw(_) => "raw",
        }
    }
}

/// Unmodelled fields, in the order they arrived.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Extensions {
    entries: Vec<(String, ExtensionValue)>,
}

impl Extensions {
    /// No extensions. The common case: most messages have none.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Adds an entry, keeping insertion order and allowing duplicate keys.
    #[must_use]
    pub fn with(mut self, key: impl Into<String>, value: ExtensionValue) -> Self {
        self.entries.push((key.into(), value));
        self
    }

    /// Adds an entry in place.
    pub fn insert(&mut self, key: impl Into<String>, value: ExtensionValue) {
        self.entries.push((key.into(), value));
    }

    /// The first value for `key`, or `None`.
    ///
    /// "First" because duplicates are preserved, and the first is the one
    /// serde would have used.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&ExtensionValue> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// Every value for `key`, in order.
    pub fn get_all<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a ExtensionValue> {
        self.entries
            .iter()
            .filter(move |(k, _)| k == key)
            .map(|(_, v)| v)
    }

    /// Whether `key` is present at all.
    #[must_use]
    pub fn contains(&self, key: &str) -> bool {
        self.entries.iter().any(|(k, _)| k == key)
    }

    /// The entries, in order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &ExtensionValue)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Whether there are none.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// How many entries there are, counting duplicates separately.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// Consumes the bag, yielding its entries in order.
    #[must_use]
    pub fn into_entries(self) -> Vec<(String, ExtensionValue)> {
        self.entries
    }
}

impl fmt::Display for Extensions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for (key, value) in &self.entries {
            if !first {
                formatter.write_str(", ")?;
            }
            first = false;
            write!(formatter, "{key}={}", value.shape())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::panic,
        clippy::unwrap_used,
        reason = "tests assert on fallible calls and unwrap with the failing precondition in the message"
    )]

    use super::{ExtensionValue, Extensions};

    #[test]
    fn an_empty_bag_has_nothing() {
        let bag = Extensions::empty();
        assert!(bag.is_empty());
        assert_eq!(bag.len(), 0);
        assert!(!bag.contains("x"));
        assert!(bag.get("x").is_none());
        assert_eq!(bag.iter().count(), 0);
    }

    /// Order is preserved, because a byte-identical round trip depends on it.
    #[test]
    fn insertion_order_is_preserved() {
        let bag = Extensions::empty()
            .with("z", ExtensionValue::Number("1".into()))
            .with("a", ExtensionValue::Null)
            .with("m", ExtensionValue::Bool(true));
        let keys: Vec<&str> = bag.iter().map(|(k, _)| k).collect();
        assert_eq!(keys, vec!["z", "a", "m"]);
    }

    /// A repeated key is preserved, and `get` reports the first — which is the
    /// one serde would have used.
    #[test]
    fn duplicate_keys_are_kept() {
        let bag = Extensions::empty()
            .with("k", ExtensionValue::String("first".into()))
            .with("k", ExtensionValue::String("second".into()));
        assert_eq!(bag.len(), 2);
        assert_eq!(bag.get_all("k").count(), 2);
        assert_eq!(bag.get("k"), Some(&ExtensionValue::String("first".into())));
        assert_eq!(
            bag.get_all("k").collect::<Vec<_>>(),
            vec![
                &ExtensionValue::String("first".into()),
                &ExtensionValue::String("second".into()),
            ],
        );
    }

    #[test]
    fn nested_shapes_round_trip_by_construction() {
        let inner = Extensions::empty().with("deep", ExtensionValue::Bool(false));
        let value = ExtensionValue::Object(inner);
        let bag = Extensions::empty().with("outer", value);
        assert_eq!(bag.get("outer").unwrap().shape(), "object");
        assert_eq!(
            bag.get("outer"),
            Some(&ExtensionValue::Object(
                Extensions::empty().with("deep", ExtensionValue::Bool(false)),
            )),
        );
    }

    #[test]
    fn arrays_keep_their_order() {
        let value = ExtensionValue::Array(vec![
            ExtensionValue::Number("1".into()),
            ExtensionValue::String("two".into()),
            ExtensionValue::Null,
        ]);
        let bag = Extensions::empty().with("a", value.clone());
        assert_eq!(bag.get("a"), Some(&value));
    }

    /// `Raw` is the reason lift can be total. An unparseable value is data the
    /// bridge has to forward, not data it can drop.
    #[test]
    fn raw_carries_text_that_did_not_parse() {
        let bag = Extensions::empty().with("junk", ExtensionValue::Raw("{oops".into()));
        assert_eq!(bag.get("junk").unwrap().shape(), "raw");
        assert_eq!(bag.get("junk"), Some(&ExtensionValue::Raw("{oops".into())),);
    }

    #[test]
    fn numbers_are_kept_as_text() {
        // Formatting that a typed f64 round trip would destroy.
        for text in ["1.0", "1e10", "10000000000000000000", "0.1", "-0"] {
            let bag = Extensions::empty().with("n", ExtensionValue::Number(text.into()));
            assert_eq!(
                bag.get("n"),
                Some(&ExtensionValue::Number(text.into())),
                "{text}"
            );
        }
    }

    #[test]
    fn into_entries_preserves_order_and_duplicates() {
        let bag = Extensions::empty()
            .with("a", ExtensionValue::Null)
            .with("b", ExtensionValue::Null)
            .with("a", ExtensionValue::Bool(true));
        let entries = bag.into_entries();
        assert_eq!(entries.len(), 3);
        let (Some(first), Some(last)) = (entries.first(), entries.last()) else {
            panic!("expected three entries");
        };
        assert_eq!(first.0, "a");
        assert_eq!(last.0, "a");
    }

    #[test]
    fn display_summarises_shapes_for_logs() {
        let bag = Extensions::empty()
            .with("a", ExtensionValue::String("x".into()))
            .with("b", ExtensionValue::Object(Extensions::empty()));
        assert_eq!(bag.to_string(), "a=string, b=object");
        assert_eq!(Extensions::empty().to_string(), "");
    }
}
