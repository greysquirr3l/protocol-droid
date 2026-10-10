//! The value objects and `LazyLock` constants for [`AgentCard`].
//!
//! Every type here is a Rust shape the spec's wire form lowers
//! into. The builder in [`super::builder`] is the only way to
//! get an [`AgentCard`]; the value objects themselves are
//! constructable by the builder and by tests.

use std::sync::{Arc, LazyLock};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::domain::a2a::A2aVersion;

// --- `ProtocolBinding` and the three spec-named constants -----------

/// The JSON-RPC binding — the v1.0 default.
///
/// A `LazyLock<ProtocolBinding>` because `Arc::from` is not
/// `const` in 1.98, and an associated constant of type `Self`
/// is not allowed on stable. The constant lives at module scope
/// so callers reach it as `agent_card::JSONRPC`.
pub static JSONRPC: LazyLock<ProtocolBinding> =
    LazyLock::new(|| ProtocolBinding(Arc::from("JSONRPC")));

/// The gRPC binding.
pub static GRPC: LazyLock<ProtocolBinding> =
    LazyLock::new(|| ProtocolBinding(Arc::from("GRPC")));

/// The HTTP+JSON binding (REST-style POSTs of JSON-RPC envelopes).
pub static HTTP_JSON: LazyLock<ProtocolBinding> =
    LazyLock::new(|| ProtocolBinding(Arc::from("HTTP+JSON")));

/// The protocol binding an [`AgentInterface`] advertises.
///
/// The spec lists `JSONRPC`, `GRPC`, and `HTTP+JSON` as the
/// officially supported values, but the field is an "open form
/// string" so a deployment can advertise a custom binding (e.g.
/// `HTTP+SSE+JSON`). The newtype is a typed boundary;
/// comparisons go through `as_str`, the `custom` constructor is
/// the escape hatch for forward compatibility, and the three
/// spec-named values are pre-built [`LazyLock`] constants in this
/// module (see [`JSONRPC`], [`GRPC`], [`HTTP_JSON`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProtocolBinding(Arc<str>);

impl ProtocolBinding {
    /// A custom binding the spec does not name, for forward
    /// compatibility.
    #[must_use]
    pub fn custom(value: impl Into<Arc<str>>) -> Self {
        Self(value.into())
    }

    /// The wire form.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ProtocolBinding {
    fn default() -> Self {
        // The default is JSON-RPC, the v1.0 primary binding. An
        // agent builder that does not pick a binding gets a
        // JSON-RPC card. The clone is cheap (one refcount bump)
        // and avoids a `LazyLock` deref in a hot path.
        JSONRPC.clone()
    }
}

// --- Sub-messages ---------------------------------------------------

/// One protocol binding the agent publishes, with the URL it is
/// reachable at and the A2A version it speaks.
///
/// The first entry in `AgentCard::supported_interfaces` is the
/// preferred binding per the spec; the rest are fallbacks. The
/// order in the `Vec` is the order the spec reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInterface {
    /// The URL the binding is reachable at. The spec recommends
    /// `https://` in production; an `http://` URL is permitted
    /// for development.
    pub url: Url,
    /// The protocol binding advertised at this URL.
    pub protocol_binding: ProtocolBinding,
    /// The A2A version this interface speaks. `Known` for a
    /// version the build knows; `Unknown` for a
    /// forward-compatible version the build has not seen.
    pub protocol_version: A2aVersion,
    /// The tenant ID for tenant-scoped routing, when the server
    /// publishes a per-tenant interface. Optional in the spec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<Arc<str>>,
}

/// The capability set the agent advertises.
///
/// All three booleans are optional in the proto (their absence
/// is equivalent to `false`), but the struct itself is required
/// — every card has a `capabilities` field, even if the value is
/// the default "none of the optional things are on".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    /// The agent supports `SendStreamingMessage` /
    /// `SubscribeToTask`. The client can subscribe to
    /// incremental updates.
    #[serde(default)]
    pub streaming: bool,
    /// The agent accepts
    /// `CreateTaskPushNotificationConfig` and will POST to the
    /// registered webhook when a task changes. The actual
    /// delivery uses the [`SsrfDenylist`](crate::adapters::secondary::a2a::SsrfDenylist)
    /// the push-notification port consults.
    #[serde(default)]
    pub push_notifications: bool,
    /// The agent publishes an *extended* card behind
    /// authentication. Per the spec, if this is `false` or
    /// absent, calls to `GetExtendedAgentCard` return
    /// `ExtendedAgentCardNotConfiguredError` (or
    /// `UnsupportedOperationError`, depending on the protocol
    /// binding).
    #[serde(default)]
    pub extended_agent_card: bool,
}

/// One skill the agent advertises — a focused capability the
/// agent can describe more concretely than the agent as a whole.
///
/// A `Skill` is largely descriptive: the `id` is the addressable
/// handle a client can use to route a request, the `tags` are
/// keywords a discovery index can search on, the `examples` are
/// prompts a UI can surface as quick-pick chips.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSkill {
    /// A unique identifier for the skill within the card. The
    /// spec marks this REQUIRED; the builder rejects an empty
    /// id.
    pub id: Arc<str>,
    /// A human-readable name. REQUIRED.
    pub name: Arc<str>,
    /// A detailed description. REQUIRED.
    pub description: Arc<str>,
    /// A set of keywords describing the skill. REQUIRED — the
    /// field is always present on the wire (an empty array is
    /// valid), so the builder allows it to be empty but the
    /// field is not optional.
    #[serde(default)]
    pub tags: Vec<Arc<str>>,
}

// --- `AgentCard` ----------------------------------------------------

/// A self-describing manifest for an agent.
///
/// This is the required-fields view. The full proto has
/// additional optional fields (`provider`, `documentation_url`,
/// `icon_url`, `security_schemes`, `security_requirements`,
/// `signatures`, `extensions`) that a follow-up commit adds; the
/// builder is the place those land when the wire codec work
/// pulls them in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCard {
    /// A human-readable name. REQUIRED, non-empty.
    pub name: Arc<str>,
    /// A human-readable description. REQUIRED, non-empty.
    pub description: Arc<str>,
    /// The version of the agent, in semver form. REQUIRED,
    /// non-empty. The spec uses this for `ETag` derivation in
    /// caching; the builder does not parse the string.
    pub version: Arc<str>,
    /// The protocol bindings the agent publishes, in preference
    /// order. REQUIRED, non-empty. An agent with no interface
    /// has no protocol to speak.
    pub supported_interfaces: Vec<AgentInterface>,
    /// The capability set. REQUIRED — the field is always
    /// present on the wire.
    pub capabilities: AgentCapabilities,
    /// The default input media types. REQUIRED, non-empty. The
    /// spec recommends `text/plain` as the universal default.
    pub default_input_modes: Vec<Arc<str>>,
    /// The default output media types. REQUIRED, non-empty.
    pub default_output_modes: Vec<Arc<str>>,
    /// The skills the agent advertises. REQUIRED (the field is
    /// always present; an empty list is valid).
    pub skills: Vec<AgentSkill>,
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::expect_used,
        reason = "tests assert on fallible calls with named failure messages; propagating errors would replace a clear failure with noise"
    )]

    use std::sync::Arc;

    use super::{AgentCapabilities, AgentCard, JSONRPC};
    use crate::domain::a2a::AgentCardBuilder;

    type TestError = Box<dyn std::error::Error>;

    fn sample_card() -> Result<AgentCard, TestError> {
        // The builder is in the sibling module; the
        // `sample_interface` and `sample_skill` constructors
        // are the same as the builder's tests. Duplicating
        // them here keeps the value-object tests independent
        // of the builder's invariants.
        Ok(AgentCardBuilder::new()
            .with_name("Research Agent")
            .with_description("Looks things up on the web.")
            .with_version("1.0.0")
            .with_interface(super::AgentInterface {
                url: url::Url::parse("https://example.com/a2a/v1")?,
                protocol_binding: JSONRPC.clone(),
                protocol_version: crate::domain::a2a::A2aVersion::V1_0,
                tenant: None,
            })
            .with_capabilities(AgentCapabilities::default())
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .with_skill(super::AgentSkill {
                id: Arc::from("research"),
                name: Arc::from("Research"),
                description: Arc::from("Look things up."),
                tags: vec![],
            })
            .build()?)
    }

    // --- `ProtocolBinding` constants and methods ---------------------

    #[test]
    fn protocol_binding_jsonrpc_constant_has_the_spec_wire_form() {
        // The wire form is the A2A spec's literal; a typo in
        // the `LazyLock` body would be a protocol break.
        assert_eq!(super::JSONRPC.as_str(), "JSONRPC");
    }

    #[test]
    fn protocol_binding_grpc_constant_has_the_spec_wire_form() {
        assert_eq!(super::GRPC.as_str(), "GRPC");
    }

    #[test]
    fn protocol_binding_http_json_constant_has_the_spec_wire_form() {
        assert_eq!(super::HTTP_JSON.as_str(), "HTTP+JSON");
    }

    #[test]
    fn protocol_binding_custom_carries_the_callers_value() {
        let binding = super::ProtocolBinding::custom("HTTP+SSE+JSON");
        assert_eq!(binding.as_str(), "HTTP+SSE+JSON");
    }

    #[test]
    fn protocol_binding_default_is_json_rpc() {
        // The default is the v1.0 primary binding — an
        // agent builder that does not pick a binding gets a
        // JSON-RPC card. The test pins the implicit default,
        // so a future refactor that changes it fails loudly.
        assert_eq!(super::ProtocolBinding::default().as_str(), "JSONRPC");
    }

    #[test]
    fn protocol_binding_constants_are_distinct() {
        // Three distinct `LazyLock`s with three distinct
        // values; a typo that produced the same string for
        // two of them would still serialise to a working
        // card but would route incorrectly at the wire
        // layer.
        assert_ne!(super::JSONRPC.as_str(), super::GRPC.as_str());
        assert_ne!(super::JSONRPC.as_str(), super::HTTP_JSON.as_str());
        assert_ne!(super::GRPC.as_str(), super::HTTP_JSON.as_str());
    }

    // --- JSON shape --------------------------------------------------

    #[test]
    fn serialises_in_camel_case() -> Result<(), TestError> {
        // The spec's wire form is camelCase; a snake_case
        // serialisation would be unreadable to every other
        // A2A implementation. This test pins the field
        // names.
        let card = sample_card()?;
        let value = serde_json::to_value(&card)?;
        let obj = value
            .as_object()
            .expect("the card must serialise to a JSON object");
        for required in [
            "name",
            "description",
            "version",
            "supportedInterfaces",
            "capabilities",
            "defaultInputModes",
            "defaultOutputModes",
            "skills",
        ] {
            assert!(
                obj.contains_key(required),
                "the JSON object must contain {required:?}; got keys {:?}",
                obj.keys().collect::<Vec<_>>()
            );
        }
        // No snake_case leakage.
        for leak in [
            "supported_interfaces",
            "default_input_modes",
            "default_output_modes",
        ] {
            assert!(
                !obj.contains_key(leak),
                "the JSON object must not contain snake_case {leak:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn capabilities_serialise_with_correct_field_names() -> Result<(), TestError> {
        let card = AgentCardBuilder::new()
            .with_name("n")
            .with_description("d")
            .with_version("1.0.0")
            .with_interface(super::AgentInterface {
                url: url::Url::parse("https://example.com/a2a/v1")?,
                protocol_binding: JSONRPC.clone(),
                protocol_version: crate::domain::a2a::A2aVersion::V1_0,
                tenant: None,
            })
            .with_capabilities(AgentCapabilities {
                streaming: true,
                push_notifications: true,
                extended_agent_card: true,
            })
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .build()?;
        let value = serde_json::to_value(&card.capabilities)?;
        let obj = value.as_object().expect("capabilities is a JSON object");
        assert!(obj.contains_key("streaming"));
        assert!(obj.contains_key("pushNotifications"));
        assert!(obj.contains_key("extendedAgentCard"));
        Ok(())
    }

    #[test]
    fn round_trips_through_serde() -> Result<(), TestError> {
        let original = sample_card()?;
        let json = serde_json::to_string(&original)?;
        let parsed: AgentCard = serde_json::from_str(&json)?;
        assert_eq!(original, parsed);
        Ok(())
    }

    #[test]
    fn matches_the_spec_example_shape() -> Result<(), TestError> {
        // The A2A v1.0 spec's §4.4 example JSON is a fixture
        // a hand-written wire codec must reproduce. This
        // test pins the field shape against that fixture,
        // with values adjusted for the test's identity.
        let card = sample_card()?;
        let value = serde_json::to_value(&card)?;
        let obj = value.as_object().expect("card serialises to an object");

        let name = obj
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        assert_eq!(
            name,
            Some("Research Agent".to_owned()),
            "name must be the wire form the spec names"
        );

        let first_interface = obj
            .get("supportedInterfaces")
            .and_then(serde_json::Value::as_array)
            .and_then(|arr| arr.first())
            .and_then(serde_json::Value::as_object);
        let url = first_interface
            .and_then(|i| i.get("url"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let binding = first_interface
            .and_then(|i| i.get("protocolBinding"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let version = first_interface
            .and_then(|i| i.get("protocolVersion"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        assert_eq!(url.as_deref(), Some("https://example.com/a2a/v1"));
        assert_eq!(binding.as_deref(), Some("JSONRPC"));
        assert_eq!(version.as_deref(), Some("1.0"));

        let first_input = obj
            .get("defaultInputModes")
            .and_then(serde_json::Value::as_array)
            .and_then(|arr| arr.first())
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let first_output = obj
            .get("defaultOutputModes")
            .and_then(serde_json::Value::as_array)
            .and_then(|arr| arr.first())
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        assert_eq!(first_input.as_deref(), Some("text/plain"));
        assert_eq!(first_output.as_deref(), Some("text/plain"));
        Ok(())
    }
}
