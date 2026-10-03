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

impl From<serde_json::Value> for ExtensionValue {
    /// Converts a JSON value, total by construction.
    ///
    /// Every `serde_json::Value` has a matching variant, so this cannot fail —
    /// which is what makes [`Extensions::from_json_object`] infallible and lift
    /// total. A number keeps its source text rather than being reparsed, so a
    /// large integer or a `1.0` survives the crossing byte for byte.
    fn from(value: serde_json::Value) -> Self {
        match value {
            serde_json::Value::Null => Self::Null,
            serde_json::Value::Bool(boolean) => Self::Bool(boolean),
            serde_json::Value::Number(number) => Self::Number(number.to_string()),
            serde_json::Value::String(string) => Self::String(string),
            serde_json::Value::Object(object) => Self::Object(Extensions::from_json_object(object)),
            serde_json::Value::Array(items) => {
                Self::Array(items.into_iter().map(Self::from).collect())
            }
        }
    }
}

impl From<&ExtensionValue> for serde_json::Value {
    /// Converts back to JSON.
    ///
    /// `Number` round-trips through the stored text, and `Raw` is parsed with
    /// the raw text as a fallback so an unparseable value is still emitted
    /// rather than dropped. A value that reached `Raw` was already text that
    /// serde could not read as JSON, so the fallback is a string, which is what
    /// it came from.
    fn from(value: &ExtensionValue) -> Self {
        match value {
            ExtensionValue::Null => Self::Null,
            ExtensionValue::Bool(boolean) => Self::Bool(*boolean),
            ExtensionValue::Number(text) => {
                // `Number` holds the source text of a number that already was
                // one, so this always parses. If it somehow does not, emitting
                // the text as a string still forwards the bytes; substituting a
                // different number would quietly change the value.
                text.parse()
                    .map_or_else(|_| Self::String(text.clone()), Self::Number)
            }
            ExtensionValue::String(string) => Self::String(string.clone()),
            ExtensionValue::Object(object) => Self::Object(object.to_json_object()),
            ExtensionValue::Array(items) => Self::Array(items.iter().map(Self::from).collect()),
            ExtensionValue::Raw(text) => {
                text.parse().unwrap_or_else(|_| Self::String(text.clone()))
            }
        }
    }
}

impl Extensions {
    /// Builds a bag from a JSON object.
    #[must_use]
    pub fn from_json_object(object: serde_json::Map<String, serde_json::Value>) -> Self {
        Self {
            entries: object
                .into_iter()
                .map(|(key, value)| (key, ExtensionValue::from(value)))
                .collect(),
        }
    }

    /// Renders the bag back to a JSON object.
    #[must_use]
    pub fn to_json_object(&self) -> serde_json::Map<String, serde_json::Value> {
        self.entries
            .iter()
            .map(|(key, value)| (key.clone(), serde_json::Value::from(value)))
            .collect()
    }
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

/// Unmodelled fields, in `serde_json::Map` order.
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

#[cfg(test)]
mod json_tests {
    #![expect(
        clippy::expect_used,
        clippy::unwrap_used,
        reason = "tests assert on fallible conversions; the source is a literal object, so a failure means the literal changed"
    )]

    use super::{ExtensionValue, Extensions};
    use serde_json::Value;

    /// A bag converted to JSON and back is the bag it started as. The codec
    /// layer leans on this for every unmodelled field on every message, so a
    /// violation here is a field silently re-shaped on the way through a
    /// bridge.
    #[test]
    fn a_bag_survives_the_json_round_trip() {
        for source in [
            serde_json::json!({}),
            serde_json::json!({"a": 1}),
            serde_json::json!({"a": null, "b": true, "c": "text"}),
            serde_json::json!({"big": 9_007_199_254_740_993_u64}),
            serde_json::json!({"float": 1.0}),
            serde_json::json!({"nested": {"deep": {"deeper": [1, "two", null]}}}),
            serde_json::json!({"array": [1, 2, {"three": 3}]}),
        ] {
            let bag = Extensions::from_json_object(source.as_object().unwrap().clone());
            let back = bag.to_json_object();
            assert_eq!(Value::Object(back), source);
        }
    }

    /// A large integer is the case a `f64`-based conversion would mangle, which
    /// is why `Number` holds text rather than a parsed value.
    #[test]
    fn a_large_integer_is_not_rounded() {
        let bag = Extensions::from_json_object(
            serde_json::json!({ "n": 9_007_199_254_740_993_u64 })
                .as_object()
                .expect("object")
                .clone(),
        );
        let back = bag.to_json_object();
        assert_eq!(
            back.get("n").and_then(Value::as_u64),
            Some(9_007_199_254_740_993),
        );
    }

    /// Key order is `serde_json::Map`'s, which is sorted unless the
    /// `preserve_order` feature is enabled. JSON makes member order
    /// insignificant, so preserving it is not part of lossless — the same set of
    /// keys with the same values is. Enabling `preserve_order` to keep the byte
    /// order would be a dependency for something the format does not promise.
    #[test]
    fn keys_keep_their_set_regardless_of_order() {
        let source = serde_json::json!({"zebra": 1, "apple": 2, "mango": 3});
        let bag = Extensions::from_json_object(source.as_object().expect("object").clone());
        let mut keys: Vec<&str> = bag.iter().map(|(key, _)| key).collect();
        let mut want = vec!["zebra", "apple", "mango"];
        keys.sort_unstable();
        want.sort_unstable();
        assert_eq!(keys, want);
        // And the values follow their keys, which is the part that matters.
        let back = bag.to_json_object();
        assert_eq!(back.get("zebra").and_then(Value::as_i64), Some(1));
        assert_eq!(back.get("apple").and_then(Value::as_i64), Some(2));
        assert_eq!(back.get("mango").and_then(Value::as_i64), Some(3));
    }

    /// `Raw` exists so an unparseable value can be forwarded rather than
    /// dropped. It emits its text back.
    #[test]
    fn raw_text_is_forwarded_rather_than_dropped() {
        let bag = Extensions::empty().with("odd", ExtensionValue::Raw("not json".to_owned()));
        let back = bag.to_json_object();
        assert_eq!(back.get("odd"), Some(&Value::String("not json".to_owned())));
    }
}
