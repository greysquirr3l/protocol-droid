//! `JsonObject` — a validated JSON object, and the only way arbitrary JSON
//! enters the IR.
//!
//! # Why a newtype rather than `serde_json::Value`
//!
//! The domain layer takes no `serde_json::Value` in its signatures. That rule
//! is what keeps the domain free of the serializer, and it is worth restating
//! *why* it exists: a `Value` in a signature means every layer above can hand
//! the domain an arbitrary tree, so every domain function has to re-validate it
//! and the type system stops saying where the shape was checked. `JsonObject`
//! inverts that — by the time a caller has one, the value is known to be a JSON
//! object, checked once, at the boundary.
//!
//! # What it stores, and why
//!
//! The original text, not a re-serialised tree. Three reasons:
//!
//! - **Losslessness.** Round-tripping through a typed tree can reorder keys and
//!   change number formatting, and the bridge's contract is that a value it
//!   did not understand comes out the other side byte-identical. Storing the
//!   text makes that the default rather than something to test for.
//! - **Forward compatibility.** A JSON schema in `2026-07-28` may nest shapes
//!   this build has never heard of. Storing text means an unknown shape is not
//!   a problem, which is the same reason the rest of the IR carries an
//!   extension bag.
//! - **The hot path.** Construction parses once to validate and then holds a
//!   `&str`. Nothing downstream re-parses, which is what the no-double-parse
//!   rule is protecting.
//!
//! The cost is that you cannot read a field out of a `JsonObject` without
//! parsing. That is the right trade: callers who need structure go through the
//! codecs, which parse at the transport boundary, and everything else treats
//! the object as an opaque value to forward.

use std::fmt;

/// A JSON object, validated on construction.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JsonObject {
    text: String,
}

impl JsonObject {
    /// Validates `text` as a JSON *object*.
    ///
    /// # Errors
    ///
    /// [`JsonObjectError::InvalidJson`] when the text is not well-formed JSON,
    /// and [`JsonObjectError::NotAnObject`] when it parses but is not an
    /// object — an array or a bare scalar is a different thing, and `2025-06-18`
    /// `structuredContent` is specifically an object.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::domain::ir::JsonObject;
    ///
    /// let json = JsonObject::new(r#"{"temperature": 0.7}"#)?;
    /// assert_eq!(json.as_str(), r#"{"temperature": 0.7}"#);
    /// # Ok::<(), protocol_droid::domain::ir::JsonObjectError>(())
    /// ```
    pub fn new(text: impl Into<String>) -> Result<Self, JsonObjectError> {
        let text = text.into();
        // `serde_json` is used here and nowhere else in the IR: this is the
        // boundary check, and its output never escapes.
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(serde_json::Value::Object(_)) => Ok(Self { text }),
            Ok(_) => Err(JsonObjectError::NotAnObject),
            Err(error) => Err(JsonObjectError::InvalidJson {
                offset: error.column(),
            }),
        }
    }

    /// The text, exactly as it was validated.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Validates a `serde_json::Value` and stores it.
    ///
    /// A tool handler's result is built from data — the argument echoed back,
    /// a computed value, an error message composed from a failure. Doing that
    /// with `new` means `format!`ing a JSON document by hand, and a handler
    /// that interpolates a peer's string into a JSON literal without escaping
    /// it is an injection bug. This is the constructor that makes that mistake
    /// unavailable.
    ///
    /// The value is serialised rather than kept, so a `JsonObject` is still one
    /// validated `String` and still owns no borrowed peer's data.
    ///
    /// # Errors
    ///
    /// [`JsonObjectError::NotAnObject`] when the value is not an object.
    /// [`JsonObjectError::InvalidJson`] when the value cannot be serialised,
    /// which for a `Value` cannot happen — but the error type is the honest
    /// one, and inventing a second error for an unreachable case would be a
    /// promise this function cannot keep.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::domain::ir::JsonObject;
    ///
    /// let value = serde_json::json!({ "echoed": "quotes \" and \\ survive" });
    /// let json = JsonObject::from_value(value)?;
    /// assert_eq!(json.as_str(), r#"{"echoed":"quotes \" and \\ survive"}"#);
    /// # Ok::<(), protocol_droid::domain::ir::JsonObjectError>(())
    /// ```
    #[expect(
        clippy::needless_pass_by_value,
        reason = "the caller hands over a Value it built and does not want back. \
                  Almost every call site is `JsonObject::from_value(json!({..}))`, \
                  and by reference would force each one to bind a temporary \
                  first — the lint is right that the body only reads it, and \
                  taking it is still the signature a caller wants."
    )]
    pub fn from_value(value: serde_json::Value) -> Result<Self, JsonObjectError> {
        if !value.is_object() {
            return Err(JsonObjectError::NotAnObject);
        }
        serde_json::to_string(&value)
            .map(|text| Self { text })
            .map_err(|error| JsonObjectError::InvalidJson {
                offset: error.column(),
            })
    }

    /// Consumes the object, yielding its text.
    #[must_use]
    pub fn into_string(self) -> String {
        self.text
    }
}

impl Default for JsonObject {
    /// The empty object, `{}`.
    ///
    /// A tool handler returns a `JsonObject` and cannot return a `Result`,
    /// because MCP reports a tool failure as a successful call carrying
    /// `isError: true`. So a handler that cannot build the result it meant to
    /// build needs somewhere to fall back to, and it needs to be infallible.
    /// `{}` is that place: it is a valid document under every revision of the
    /// protocol, so no fallback can itself be a parse error.
    ///
    /// Not `""`, which is the default a `String`-backed type reaches for and
    /// which is not a JSON object at all.
    fn default() -> Self {
        Self {
            text: String::from("{}"),
        }
    }
}

impl fmt::Debug for JsonObject {
    /// Prints the text, since a `JsonObject` has no other meaningful state and
    /// the text is what a caller debugging a lossless round trip needs to see.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("JsonObject")
            .field(&self.text)
            .finish()
    }
}

impl fmt::Display for JsonObject {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)
    }
}

/// Why a string is not a [`JsonObject`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum JsonObjectError {
    /// The text is not well-formed JSON.
    #[error("not well-formed JSON, first problem at column {offset}")]
    InvalidJson {
        /// The column the parser stopped at.
        offset: usize,
    },

    /// The text parses, but as something other than an object.
    #[error("parsed as JSON, but not as an object")]
    NotAnObject,
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "tests assert on fallible calls and unwrap with the failing precondition in the message"
    )]

    use super::{JsonObject, JsonObjectError};

    #[test]
    fn a_well_formed_object_is_accepted() {
        let json = JsonObject::new(r#"{"a":1}"#).unwrap();
        assert_eq!(json.as_str(), r#"{"a":1}"#);
        // The empty object is an object.
        assert!(JsonObject::new("{}").is_ok());
        // Nesting is fine, and is exactly what a structured result is.
        assert!(JsonObject::new(r#"{"a":{"b":[{"c":null}]}}"#).is_ok());
    }

    /// Losslessness is the whole reason this stores text, so it is asserted
    /// rather than assumed.
    #[test]
    fn the_text_survives_unchanged() {
        // Whitespace, key order and number formatting all preserved exactly.
        for text in [
            r#"{ "b" : 2 , "a" : 1 }"#,
            r#"{"n":1.0}"#,
            r#"{"n":1e10}"#,
            r#"{"s":"üñí çödé 😀"}"#,
            r#"{"escaped":"line\nbreak"}"#,
        ] {
            let json = JsonObject::new(text).unwrap();
            assert_eq!(json.as_str(), text, "{text}");
            assert_eq!(json.clone().into_string(), text, "{text}");
        }
    }

    #[test]
    fn a_non_object_is_rejected() {
        for text in ["[]", r#""a string""#, "1", "true", "null"] {
            assert_eq!(
                JsonObject::new(text).unwrap_err(),
                JsonObjectError::NotAnObject,
                "{text}",
            );
        }
    }

    #[test]
    fn malformed_json_is_rejected() {
        for text in ["{", r#"{"a":}"#, "", "{'a':1}", r#"{"a":1,}"#] {
            assert!(
                matches!(
                    JsonObject::new(text),
                    Err(JsonObjectError::InvalidJson { .. })
                ),
                "{text} should be invalid",
            );
        }
    }

    #[test]
    fn equality_and_hashing_use_the_text() {
        use std::collections::HashSet;
        let a = JsonObject::new(r#"{"x":1}"#).unwrap();
        let b = JsonObject::new(r#"{"x":1}"#).unwrap();
        let c = JsonObject::new(r#"{ "x" : 1 }"#).unwrap();
        // Semantically equal but textually different, so *not* equal. This is
        // the deliberate consequence of storing text: equality is textual.
        assert_eq!(a, b);
        assert_ne!(a, c);
        let mut set = HashSet::new();
        assert!(set.insert(a));
        assert!(!set.insert(b));
        assert!(set.insert(c));
    }

    #[test]
    fn debug_and_display_show_the_text() {
        let json = JsonObject::new(r#"{"k":"v"}"#).unwrap();
        assert_eq!(json.to_string(), r#"{"k":"v"}"#);
        // Debug quotes the text, so it is escaped — but it is still there, and
        // naming the type is what makes a log line readable.
        let debug = format!("{json:?}");
        assert!(debug.starts_with("JsonObject("), "{debug}");
        assert!(debug.contains('k'), "{debug}");
    }
}
