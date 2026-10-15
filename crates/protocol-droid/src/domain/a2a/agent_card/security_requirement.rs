//! `SecurityRequirement` — a list of schemes an operation requires.
//!
//! # What this is
//!
//! A `SecurityRequirement` is a single rule of the form
//! "this operation needs scheme X with scopes A, B, C,
//! AND scheme Y". The agent publishes a list of these
//! rules on its [`AgentCard`](super::AgentCard); a client
//! matches a request to one of the rules to decide what
//! credentials to send.
//!
//! In `OpenAPI 3.x` (which the A2A spec follows), the
//! wire form is:
//!
//! ```json
//! {
//!   "securityRequirements": [
//!     { "apiKey": [] },
//!     { "oauth2": ["read", "write"] }
//!   ]
//! }
//! ```
//!
//! Each entry is a single object whose keys are scheme
//! names and whose values are lists of required scopes.
//! A client must satisfy **all** the schemes in one
//! entry to use the operation; **any** of the entries
//! being satisfied is enough.
//!
//! # Why the proto shape is different
//!
//! The proto's `SecurityRequirement` is
//! `map<string, StringList> schemes` — a single
//! `StringList` wrapper per value, producing:
//!
//! ```json
//! { "schemes": { "apiKey": { "list": [] } } }
//! ```
//!
//! The `StringList` is the proto's own wire shape, not
//! the `OpenAPI` one. The A2A spec follows `OpenAPI` for
//! the JSON form; the proto is a separate definition.
//! We model the proto's shape (a single `StringList`
//! per value) so the wire form round-trips with the
//! rest of the A2A JSON ecosystem — the spec's JSON
//! example, where it exists, will use the
//! `StringList` shape the proto produces.
//!
//! # Empty requirements are meaningful
//!
//! In `OpenAPI 3.x`, a `securityRequirement: [{}]`
//! explicitly says "this operation requires no
//! authentication" — it overrides the agent's
//! default-deny. A card with no
//! `security_requirements` field defaults to
//! "every operation requires no authentication".
//! The two are different signals and the type
//! allows both.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// A single security requirement — one rule an
/// operation must satisfy.
///
/// The wire form is the proto's: a `schemes` map
/// from scheme name to a [`StringList`] of
/// required scopes. An entry with an empty `schemes`
/// map explicitly says "this operation requires no
/// authentication" (`OpenAPI 3.x` semantics).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecurityRequirement {
    /// The schemes this requirement demands, keyed by
    /// the scheme name (the same name as the key in
    /// [`AgentCard::security_schemes`](super::AgentCard::security_schemes)).
    /// The list of scopes is per-scheme; for schemes
    /// that don't take scopes (API key, HTTP, mTLS)
    /// the list is empty.
    #[serde(default)]
    pub schemes: BTreeMap<Arc<str>, StringList>,
}

impl SecurityRequirement {
    /// Starts a new empty requirement — equivalent to
    /// `OpenAPI 3.x`'s `securityRequirement: [{}]`,
    /// "no authentication required".
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a scheme the requirement demands. The
    /// `scopes` list is the scopes the operation
    /// requires; for non-OAuth schemes the list
    /// should be empty.
    #[must_use]
    pub fn with_scheme(
        mut self,
        name: impl Into<Arc<str>>,
        scopes: impl IntoIterator<Item = impl Into<Arc<str>>>,
    ) -> Self {
        self.schemes.insert(
            name.into(),
            StringList {
                list: scopes.into_iter().map(Into::into).collect(),
            },
        );
        self
    }
}

/// A list of strings — the proto's wire shape for a
/// `repeated string` value that lives inside a `map`.
///
/// The proto's `StringList` is a one-field message
/// wrapping `repeated string list = 1`; the wire form
/// is `{"list": [...]}`. The struct exists so
/// [`SecurityRequirement`]'s `schemes` map value
/// serialises with the same shape the proto produces.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StringList {
    /// The string values.
    pub list: Vec<Arc<str>>,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{SecurityRequirement, StringList};

    type TestError = Box<dyn std::error::Error>;

    #[test]
    fn empty_requirement_serialises_as_openapi_no_auth() -> Result<(), TestError> {
        // OpenAPI's "no authentication required" is
        // `securityRequirement: [{}]`. The proto's
        // shape serialises this as
        // `{"schemes": {}}` — the `schemes` field is
        // present but empty, distinguishing it from a
        // missing field.
        let req = SecurityRequirement::new();
        let value = serde_json::to_value(&req)?;
        let Some(obj) = value.as_object() else {
            return Err("a SecurityRequirement must serialise to a JSON object"
                .to_owned()
                .into());
        };
        let Some(schemes) = obj.get("schemes").and_then(serde_json::Value::as_object) else {
            return Err("schemes must be a JSON object".to_owned().into());
        };
        assert!(schemes.is_empty());
        Ok(())
    }

    #[test]
    fn requirement_with_api_key_serialises_with_proto_shape() -> Result<(), TestError> {
        let req = SecurityRequirement::new().with_scheme("apiKey", std::iter::empty::<Arc<str>>());
        let value = serde_json::to_value(&req)?;
        let Some(obj) = value.as_object() else {
            return Err("a SecurityRequirement must serialise to a JSON object"
                .to_owned()
                .into());
        };
        let Some(schemes) = obj.get("schemes").and_then(serde_json::Value::as_object) else {
            return Err("schemes must be a JSON object".to_owned().into());
        };
        let Some(api_key) = schemes.get("apiKey").and_then(serde_json::Value::as_object) else {
            return Err("apiKey must be a JSON object".to_owned().into());
        };
        let list = api_key.get("list").and_then(serde_json::Value::as_array);
        let list = list.map(std::vec::Vec::as_slice).unwrap_or_default();
        assert!(list.is_empty());
        Ok(())
    }

    #[test]
    fn requirement_with_oauth_scopes_round_trips() -> Result<(), TestError> {
        let req = SecurityRequirement::new()
            .with_scheme("oauth2", [Arc::from("read"), Arc::from("write")]);
        let json = serde_json::to_string(&req)?;
        let parsed: SecurityRequirement = serde_json::from_str(&json)?;
        assert_eq!(parsed, req);
        let value = serde_json::to_value(&req)?;
        let Some(obj) = value.as_object() else {
            return Err("a SecurityRequirement must serialise to a JSON object"
                .to_owned()
                .into());
        };
        let Some(schemes) = obj.get("schemes").and_then(serde_json::Value::as_object) else {
            return Err("schemes must be a JSON object".to_owned().into());
        };
        let Some(oauth2) = schemes.get("oauth2").and_then(serde_json::Value::as_object) else {
            return Err("oauth2 must be a JSON object".to_owned().into());
        };
        let list = oauth2
            .get("list")
            .and_then(serde_json::Value::as_array)
            .map(std::vec::Vec::as_slice)
            .unwrap_or_default();
        let scope_strs: Vec<&str> = list.iter().filter_map(serde_json::Value::as_str).collect();
        assert_eq!(scope_strs, ["read", "write"]);
        Ok(())
    }

    #[test]
    fn string_list_round_trips() -> Result<(), TestError> {
        let list = StringList {
            list: vec![Arc::from("a"), Arc::from("b")],
        };
        let json = serde_json::to_string(&list)?;
        let parsed: StringList = serde_json::from_str(&json)?;
        assert_eq!(parsed, list);
        Ok(())
    }
}
