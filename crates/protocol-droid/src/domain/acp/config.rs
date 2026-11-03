//! Session configuration options — the typed form of the
//! schema's `SessionConfigOption` family and the
//! `session/set_config_option` request/response pair.
//!
//! The schema models a configuration option as a flat
//! object whose *shape* is chosen by a `type`
//! discriminator: a `select` option carries a `currentValue`
//! and a list of choices, a `boolean` option carries a
//! `currentValue` and nothing else. Rust has no sum type
//! that flattens into a struct, so the typed form keeps
//! the discriminator in a nested [`SessionConfigKind`]
//! and flattens it on the wire — `#[serde(flatten)]` puts
//! the `type` tag and the variant's own fields back at
//! the top level of the JSON object, which is exactly the
//! shape the schema describes.
//!
//! The select variant has two shapes of its own: a flat
//! list of options, or a list of named groups each
//! holding its own options. The schema models that as an
//! `anyOf` over two array types, which is
//! [`SessionConfigSelectOptions`].
//!
//! # The `agent`-is-the-default rule
//!
//! `AuthMethod` has the same shape of ambiguity and the
//! same answer: the schema says an authentication method
//! with no `type` is an *agent* method. See
//! [`super::initialize::AuthMethod`].

use serde::{Deserialize, Serialize};

use std::collections::BTreeMap;

/// The unique id of a session configuration option.
///
/// The schema's `SessionConfigId` is `type: "string"` —
/// opaque to the protocol. The newtype makes the API
/// self-documenting and gives the future per-revision
/// codec a place to narrow the type without rewriting
/// every call site.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionConfigId(pub String);

impl SessionConfigId {
    /// A `SessionConfigId` from any string the schema
    /// accepts. The schema does not constrain the value
    /// beyond "opaque string"; the agent is the source
    /// of truth for what makes a valid id.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The inner string, borrowed.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for SessionConfigId {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(&self.0, formatter)
    }
}

/// The id of one value a `select` configuration option
/// can take.
///
/// Distinct from [`SessionConfigId`]: the option is the
/// setting, the value is one of its choices.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionConfigValueId(pub String);

impl SessionConfigValueId {
    /// A `SessionConfigValueId` from any string the schema
    /// accepts.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The inner string, borrowed.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for SessionConfigValueId {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(&self.0, formatter)
    }
}

/// The id of a group of `select` options.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionConfigGroupId(pub String);

impl SessionConfigGroupId {
    /// A `SessionConfigGroupId` from any string the schema
    /// accepts.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The inner string, borrowed.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for SessionConfigGroupId {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(&self.0, formatter)
    }
}

/// The category an option belongs to.
///
/// The schema names four categories and allows any other
/// string, so the typed form keeps the four named ones as
/// variants and carries the rest as
/// [`SessionConfigOptionCategory::Other`]. A consumer
/// that wants to react to a category the schema does not
/// name can still match on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionConfigOptionCategory {
    /// The session mode — `ask`, `plan`, and so on.
    Mode,
    /// The model in use.
    Model,
    /// A model-level tuning knob.
    ModelConfig,
    /// How much the agent thinks before answering.
    ThoughtLevel,
    /// A category the schema does not name. The wire
    /// string is preserved verbatim.
    #[serde(untagged)]
    Other(String),
}

/// One choice of a `select` configuration option.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigSelectOption {
    /// The value this choice sets.
    pub value: SessionConfigValueId,
    /// Human-readable name.
    pub name: String,
    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Per-implementation metadata.
    #[serde(default, rename = "_meta", skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, serde_json::Value>,
}

/// A named group of `select` options.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigSelectGroup {
    /// The group's id.
    pub group: SessionConfigGroupId,
    /// Human-readable name.
    pub name: String,
    /// The choices in this group.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<SessionConfigSelectOption>,
    /// Per-implementation metadata.
    #[serde(default, rename = "_meta", skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, serde_json::Value>,
}

/// The choices of a `select` configuration option.
///
/// The schema allows either a flat list of options or a
/// list of groups. `Ungrouped` is tried first on the way
/// in, so a payload of plain options reads as
/// `Ungrouped`; a payload of groups fails that arm (a
/// group has no `value`) and reads as `Grouped`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SessionConfigSelectOptions {
    /// A flat list of choices.
    Ungrouped(Vec<SessionConfigSelectOption>),
    /// Choices arranged into named groups.
    Grouped(Vec<SessionConfigSelectGroup>),
}

/// The payload of a `select` configuration option.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigSelect {
    /// The currently selected value.
    pub current_value: SessionConfigValueId,
    /// The set of selectable options.
    pub options: SessionConfigSelectOptions,
}

/// The payload of a `boolean` configuration option.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigBoolean {
    /// The current value of the boolean option.
    pub current_value: bool,
}

/// The payload of a configuration option, chosen by its
/// `type` discriminator.
///
/// Flattened into [`SessionConfigOption`] on the wire, so
/// the JSON object carries `type` and the variant's fields
/// side by side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionConfigKind {
    /// A single-value selector.
    Select(SessionConfigSelect),
    /// A boolean on/off toggle.
    Boolean(SessionConfigBoolean),
}

/// A session configuration option the agent advertises.
///
/// The shape of the option — selector or toggle — is
/// chosen by the `type` discriminator and lives in
/// [`SessionConfigOption::kind`]; the wire form keeps it
/// flat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigOption {
    /// The unique id of the option. The consumer
    /// references this id in
    /// [`SetSessionConfigOptionRequest`].
    pub id: SessionConfigId,
    /// Human-readable name.
    pub name: String,
    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The category the option belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<SessionConfigOptionCategory>,
    /// The option's payload — a selector or a toggle.
    #[serde(flatten)]
    pub kind: SessionConfigKind,
    /// Per-implementation metadata.
    #[serde(default, rename = "_meta", skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, serde_json::Value>,
}

/// The value a consumer sets on a configuration option.
///
/// The schema allows either a boolean or a value id, told
/// apart by a `type` discriminator that the value-id
/// form omits. The discriminator is therefore only present
/// on the boolean arm, and the value-id arm is the
/// fallback — the same "the simpler form is the default"
/// rule the schema states for `AuthMethod`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionConfigOptionValue {
    /// A boolean setting.
    Boolean {
        /// The value to set.
        value: bool,
    },
    /// A value id, chosen from a `select` option's list.
    #[serde(untagged)]
    ValueId {
        /// The value id to set.
        value: SessionConfigValueId,
    },
}

/// The `session/set_config_option` request body.
///
/// Sets one configuration option on one session. The
/// value is flattened into the request so the wire form
/// carries `value` (and, for a boolean, `type`) at the
/// top level alongside `sessionId` and `configId`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetSessionConfigOptionRequest {
    /// The session to change.
    pub session_id: super::session::SessionId,
    /// The option to change.
    pub config_id: SessionConfigId,
    /// The value to set.
    #[serde(flatten)]
    pub value: SessionConfigOptionValue,
    /// Per-implementation metadata.
    #[serde(default, rename = "_meta", skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, serde_json::Value>,
}

/// The `session/set_config_option` response body.
///
/// Carries the full set of configuration options after the
/// change, so the consumer can re-render its UI from the
/// agent's own view of the world.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetSessionConfigOptionResponse {
    /// The agent's configuration options after the change.
    pub config_options: Vec<SessionConfigOption>,
    /// Per-implementation metadata.
    #[serde(default, rename = "_meta", skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The test error type. `serde_json` calls are
    /// infallible for a well-formed value, but the
    /// test functions return `Result` to satisfy the
    /// project's no-`expect` rule.
    #[derive(Debug)]
    enum Failure {
        /// A `serde_json` call failed.
        Json(serde_json::Error),
        /// A fallible check on the deserialised value.
        /// The string is the human-readable reason.
        Other(String),
    }

    impl From<serde_json::Error> for Failure {
        fn from(error: serde_json::Error) -> Self {
            Self::Json(error)
        }
    }

    impl core::fmt::Display for Failure {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::Json(error) => write!(formatter, "json error: {error}"),
                Self::Other(message) => write!(formatter, "{message}"),
            }
        }
    }

    impl std::error::Error for Failure {}

    /// A `select` configuration option round-trips:
    /// the `type` discriminator and the payload sit
    /// side by side in one flat JSON object, exactly as
    /// the schema describes.
    #[test]
    fn a_select_option_round_trips() -> Result<(), Failure> {
        let value = json!({
            "id": "model",
            "name": "Model",
            "description": "The LLM to use",
            "category": "model",
            "type": "select",
            "currentValue": "opus",
            "options": [
                {"value": "opus", "name": "Opus"},
                {"value": "sonnet", "name": "Sonnet"}
            ]
        });
        let option: SessionConfigOption = serde_json::from_value(value.clone())?;
        let SessionConfigKind::Select(select) = &option.kind else {
            return Err(Failure::Other("expected the select arm".to_owned()));
        };
        assert_eq!(select.current_value, SessionConfigValueId::new("opus"));
        let SessionConfigSelectOptions::Ungrouped(choices) = &select.options else {
            return Err(Failure::Other("expected ungrouped options".to_owned()));
        };
        assert_eq!(choices.len(), 2);
        assert_eq!(option.category, Some(SessionConfigOptionCategory::Model));
        let back = serde_json::to_value(&option)?;
        assert_eq!(back, value);
        Ok(())
    }

    /// A `boolean` configuration option round-trips
    /// with the same flat shape.
    #[test]
    fn a_boolean_option_round_trips() -> Result<(), Failure> {
        let value = json!({
            "id": "brave",
            "name": "Brave mode",
            "type": "boolean",
            "currentValue": true
        });
        let option: SessionConfigOption = serde_json::from_value(value.clone())?;
        let SessionConfigKind::Boolean(toggle) = &option.kind else {
            return Err(Failure::Other("expected the boolean arm".to_owned()));
        };
        assert!(toggle.current_value);
        let back = serde_json::to_value(&option)?;
        assert_eq!(back, value);
        Ok(())
    }

    /// A `select` option whose choices are arranged
    /// into named groups round-trips as the grouped
    /// arm.
    #[test]
    fn a_grouped_select_option_round_trips() -> Result<(), Failure> {
        let value = json!({
            "id": "model",
            "name": "Model",
            "type": "select",
            "currentValue": "opus",
            "options": [
                {
                    "group": "frontier",
                    "name": "Frontier",
                    "options": [{"value": "opus", "name": "Opus"}]
                }
            ]
        });
        let option: SessionConfigOption = serde_json::from_value(value.clone())?;
        let SessionConfigKind::Select(select) = &option.kind else {
            return Err(Failure::Other("expected the select arm".to_owned()));
        };
        let SessionConfigSelectOptions::Grouped(groups) = &select.options else {
            return Err(Failure::Other("expected grouped options".to_owned()));
        };
        assert_eq!(groups.len(), 1);
        assert_eq!(
            groups.first().map(|g| g.group.clone()),
            Some(SessionConfigGroupId::new("frontier"))
        );
        let back = serde_json::to_value(&option)?;
        assert_eq!(back, value);
        Ok(())
    }

    /// The schema names four categories and allows any
    /// other string, so an unrecognised category is
    /// preserved verbatim rather than dropped.
    #[test]
    fn an_unnamed_category_is_preserved() -> Result<(), Failure> {
        let value = json!({
            "id": "x",
            "name": "X",
            "category": "custom",
            "type": "boolean",
            "currentValue": false
        });
        let option: SessionConfigOption = serde_json::from_value(value.clone())?;
        assert_eq!(
            option.category,
            Some(SessionConfigOptionCategory::Other("custom".to_owned()))
        );
        let back = serde_json::to_value(&option)?;
        assert_eq!(back, value);
        Ok(())
    }

    /// The `session/set_config_option` request carries
    /// a value id in the flat form — no `type`
    /// discriminator — and round-trips.
    #[test]
    fn a_set_config_option_request_with_a_value_id_round_trips() -> Result<(), Failure> {
        let value = json!({
            "sessionId": "s-1",
            "configId": "model",
            "value": "opus"
        });
        let request: SetSessionConfigOptionRequest = serde_json::from_value(value.clone())?;
        assert_eq!(
            request.session_id,
            super::super::session::SessionId::new("s-1")
        );
        assert_eq!(request.config_id, SessionConfigId::new("model"));
        let SessionConfigOptionValue::ValueId { value: chosen } = &request.value else {
            return Err(Failure::Other("expected the value-id arm".to_owned()));
        };
        assert_eq!(*chosen, SessionConfigValueId::new("opus"));
        let back = serde_json::to_value(&request)?;
        assert_eq!(back, value);
        Ok(())
    }

    /// The same request with a boolean value carries the
    /// `type` discriminator, and round-trips.
    #[test]
    fn a_set_config_option_request_with_a_boolean_round_trips() -> Result<(), Failure> {
        let value = json!({
            "sessionId": "s-1",
            "configId": "brave",
            "type": "boolean",
            "value": true
        });
        let request: SetSessionConfigOptionRequest = serde_json::from_value(value.clone())?;
        let SessionConfigOptionValue::Boolean { value: chosen } = &request.value else {
            return Err(Failure::Other("expected the boolean arm".to_owned()));
        };
        assert!(*chosen);
        let back = serde_json::to_value(&request)?;
        assert_eq!(back, value);
        Ok(())
    }

    /// The `session/set_config_option` response carries
    /// the agent's full option set, and round-trips.
    #[test]
    fn a_set_config_option_response_round_trips() -> Result<(), Failure> {
        let value = json!({
            "configOptions": [
                {
                    "id": "model",
                    "name": "Model",
                    "type": "select",
                    "currentValue": "opus",
                    "options": [{"value": "opus", "name": "Opus"}]
                }
            ]
        });
        let response: SetSessionConfigOptionResponse = serde_json::from_value(value.clone())?;
        assert_eq!(response.config_options.len(), 1);
        let back = serde_json::to_value(&response)?;
        assert_eq!(back, value);
        Ok(())
    }

    /// An option with no `description` and no
    /// `category` deserialises with both absent — the
    /// schema marks them optional with a
    /// default-on-error annotation, so a missing one is
    /// not an error.
    #[test]
    fn an_option_without_optional_fields_parses() -> Result<(), Failure> {
        let value = json!({
            "id": "x",
            "name": "X",
            "type": "boolean",
            "currentValue": false
        });
        let option: SessionConfigOption = serde_json::from_value(value)?;
        assert_eq!(option.description, None);
        assert_eq!(option.category, None);
        Ok(())
    }

    /// The `_meta` block survives a round trip on the
    /// option and on the request.
    #[test]
    fn the_meta_block_survives_a_round_trip() -> Result<(), Failure> {
        let value = json!({
            "id": "x",
            "name": "X",
            "type": "boolean",
            "currentValue": false,
            "_meta": {"origin": "test"}
        });
        let option: SessionConfigOption = serde_json::from_value(value.clone())?;
        assert_eq!(option.meta.get("origin"), Some(&json!("test")));
        let back = serde_json::to_value(&option)?;
        assert_eq!(back, value);
        Ok(())
    }
}
