//! `Implementation` — the `clientInfo` / `agentInfo` block
//! the editor and the agent exchange in `initialize`.
//!
//! The schema's source of truth is
//! `crates/protocol-droid-schema/spec/acp/v1.25.0/schema.json`
//! under `$defs.Implementation`. The shape:
//!
//! - `name`: programmatic identifier (required)
//! - `title`: human-readable display name (optional)
//! - `version`: semver string (required)
//! - `_meta`: per-implementation metadata (optional,
//!   open shape, kept as a JSON `Value` for forward
//!   compatibility)
//!
//! # Why a `BTreeMap` for `_meta`
//!
//! The schema's `_meta` is an open object with
//! `additionalProperties: true`. A typed
//! `BTreeMap<String, serde_json::Value>` is the closest
//! the Rust type system can get to the wire form
//! without losing the JSON's "anything goes" semantics.
//! The map keeps the wire form deterministic
//! (alphabetical by key) so test fixtures stay readable.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The `clientInfo` / `agentInfo` block. The same shape
/// on both sides of the wire, mirrored by the schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Implementation {
    /// Programmatic identifier. Required, non-empty.
    /// Convention is reverse-DNS, e.g. `com.example.client`.
    pub name: String,
    /// Human-readable display name. Optional; UI surfaces
    /// fall back to `name` when `title` is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Semver-form version string. Required, non-empty.
    /// The schema does not validate the semver shape; the
    /// build's own `protocol_droid::VERSION` is the
    /// canonical source for protocol-droid itself.
    pub version: String,
    /// Per-implementation metadata. The schema's `_meta`
    /// is open (`additionalProperties: true`), so the
    /// typed form is a JSON `Value` map. The map keeps
    /// the wire form deterministic.
    #[serde(default, rename = "_meta", skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, serde_json::Value>,
}

impl Implementation {
    /// A `protocol-droid`-flavored `Implementation` for the
    /// build the consumer is running. The version string
    /// is the crate's own `CARGO_PKG_VERSION`; the name
    /// is `protocol-droid-client` / `protocol-droid-agent`
    /// so a wire log line carries the implementation.
    pub fn protocol_droid(name: &str, version: &str) -> Self {
        Self {
            name: name.to_owned(),
            title: Some("protocol-droid".to_owned()),
            version: version.to_owned(),
            meta: BTreeMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The test error type. The `serde_json` calls
    /// below are infallible for a well-formed value
    /// but the test function must return `Result` to
    /// satisfy the project's no-`expect` rule.
    #[derive(Debug)]
    #[expect(
        dead_code,
        reason = "the test only constructs the error variant on failure; the field is read by the Debug derive"
    )]
    enum Failure {
        Json(serde_json::Error),
    }

    impl From<serde_json::Error> for Failure {
        fn from(error: serde_json::Error) -> Self {
            Self::Json(error)
        }
    }

    /// The wire form is `camelCase` for `clientInfo`
    /// and `agentInfo` themselves but the inner
    /// `name`/`title`/`version`/`_meta` keys are
    /// the schema's own (no `camelCase` rewrite
    /// because they are already lowercase). The
    /// rename rule applies to the *struct*'s
    /// field renames, not the inner field names.
    #[test]
    fn the_inner_field_names_are_already_lowercase() -> Result<(), Failure> {
        let info = Implementation::protocol_droid("test-client", "0.1.0");
        let value = serde_json::to_value(&info)?;
        let name = value.get("name").and_then(serde_json::Value::as_str);
        let title = value.get("title").and_then(serde_json::Value::as_str);
        let version = value.get("version").and_then(serde_json::Value::as_str);
        assert_eq!(name, Some("test-client"));
        assert_eq!(title, Some("protocol-droid"));
        assert_eq!(version, Some("0.1.0"));
        assert!(value.get("_meta").is_none(), "empty _meta is omitted from the wire");
        Ok(())
    }

    /// A round-trip through `Serialize` →
    /// `Deserialize` produces an equal value. The
    /// `_meta` field carries JSON `Value`s, so the
    /// round-trip is by-value, not by-reference.
    #[test]
    fn serialize_then_deserialize_round_trips() -> Result<(), Failure> {
        let mut info = Implementation::protocol_droid("test", "0.1.0");
        info.meta.insert("k".to_owned(), serde_json::json!({"nested": 42}));
        let json = serde_json::to_string(&info)?;
        let round_tripped: Implementation = serde_json::from_str(&json)?;
        assert_eq!(round_tripped, info);
        Ok(())
    }
}
