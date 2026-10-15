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
pub static GRPC: LazyLock<ProtocolBinding> = LazyLock::new(|| ProtocolBinding(Arc::from("GRPC")));

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

/// The service provider of the agent.
///
/// The proto marks both fields REQUIRED. The builder rejects an
/// empty `organization`; the `url` field is typed `url::Url`,
/// so an invalid URL is a `url::ParseError` at the call site
/// that constructs the value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProvider {
    /// A URL for the agent provider's website or relevant
    /// documentation. REQUIRED.
    pub url: Url,
    /// The name of the agent provider's organization. REQUIRED,
    /// non-empty.
    pub organization: Arc<str>,
}

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

/// A declaration of a protocol extension the agent supports.
///
/// A2A v1.0 §6 reserves a set of extension URIs for
/// protocol-level additions (e.g. `audio-input`, `video-input`,
/// agent-to-agent routing). An agent declares the extensions it
/// honours through [`AgentCapabilities::extensions`]; a client
/// can read the list to decide which features to surface.
///
/// The `params` field carries extension-specific configuration
/// — a `google.protobuf.Struct` is a JSON object, modelled here
/// as `serde_json::Map` for round-tripping without an additional
/// dep on the protobuf crate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentExtension {
    /// The unique URI identifying the extension. REQUIRED,
    /// non-empty.
    pub uri: Arc<str>,
    /// A human-readable description of how the agent uses the
    /// extension. REQUIRED, non-empty.
    pub description: Arc<str>,
    /// If `true`, the client **must** understand and comply
    /// with the extension's requirements. A client that does
    /// not understand a required extension must refuse to
    /// interact with the agent.
    #[serde(default)]
    pub required: bool,
    /// Extension-specific configuration parameters. `None`
    /// when the extension has no params; an empty object
    /// (`Some({})`) when the extension defines a params schema
    /// with no values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Map<String, serde_json::Value>>,
}

/// The capability set the agent advertises.
///
/// The three booleans are optional in the proto (their absence
/// is equivalent to `false`), and the `extensions` list is the
/// protocol-extension declaration. The struct itself is
/// required — every card has a `capabilities` field, even if
/// the value is the default "none of the optional things are
/// on".
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
    /// The protocol extensions the agent supports. The list is
    /// always serialised (the spec marks the field as part of
    /// `AgentCapabilities`); an empty list means the agent
    /// speaks the base A2A v1.0 spec with no extensions.
    #[serde(default)]
    pub extensions: Vec<AgentExtension>,
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
/// This is the required-fields view plus the four
/// single-value optional fields the proto names
/// (`provider`, `documentation_url`, `icon_url`, plus
/// the `security_schemes` map that lands in this commit).
/// The remaining optional fields (`security_requirements`,
/// `signatures`) land in follow-up commits — the former
/// is its own type, the latter is RFC 7515 JWS and wants
/// its own dependency footprint.
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
    /// The service provider of the agent. Optional in the
    /// proto, present here as `Option<AgentProvider>` so the
    /// field is omitted from the wire form when the agent
    /// does not advertise a provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<AgentProvider>,
    /// A URL providing additional documentation about the
    /// agent. Optional in the proto; absent by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documentation_url: Option<Url>,
    /// A URL to an icon for the agent. Optional in the
    /// proto; absent by default. UI clients surface this
    /// as the agent's avatar in a directory listing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<Url>,
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
    /// The security schemes the agent advertises, keyed by
    /// the caller's chosen scheme name. Optional in the
    /// spec — an agent that does not require authentication
    /// has no `securitySchemes` field on the wire. The map
    /// is `BTreeMap` so the wire form is deterministic
    /// (alphabetical by scheme name), which keeps test
    /// fixtures readable.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub security_schemes:
        std::collections::BTreeMap<Arc<str>, super::security_scheme::SecurityScheme>,
    /// The security requirements the agent publishes, in
    /// preference order. Optional in the spec — an agent
    /// with no `security_requirements` field on the wire
    /// defaults to "every operation requires no
    /// authentication", which is the same as publishing
    /// `[{ "schemes": {} }]`. The two are different signals
    /// (the former is implicit, the latter is explicit)
    /// and clients render them differently.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub security_requirements: Vec<super::security_requirement::SecurityRequirement>,
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
        // The three optional fields are absent by default —
        // a card with no provider / docs / icon should not
        // serialise `null` for them, and the absence is
        // the signal a client uses to render the agent
        // without an org tag, docs link, or avatar.
        for optional in ["provider", "documentationUrl", "iconUrl"] {
            assert!(
                !obj.contains_key(optional),
                "an unset optional must be absent (not null); got key {optional:?}"
            );
        }
        // No snake_case leakage.
        for leak in [
            "supported_interfaces",
            "default_input_modes",
            "default_output_modes",
            "documentation_url",
            "icon_url",
        ] {
            assert!(
                !obj.contains_key(leak),
                "the JSON object must not contain snake_case {leak:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn optional_fields_appear_when_set() -> Result<(), TestError> {
        // The wire form carries an optional field only when
        // it is set; the test pins each of the three so a
        // future refactor that always-serialises cannot
        // silently change the JSON.
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
            .with_provider(super::AgentProvider {
                url: url::Url::parse("https://example.com")?,
                organization: Arc::from("Example Co"),
            })
            .with_documentation_url(url::Url::parse("https://example.com/docs")?)
            .with_icon_url(url::Url::parse("https://example.com/icon.png")?)
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .build()?;
        let value = serde_json::to_value(&card)?;
        let obj = value
            .as_object()
            .expect("the card must serialise to a JSON object");

        let provider = obj.get("provider").and_then(serde_json::Value::as_object);
        let provider_url = provider
            .and_then(|p| p.get("url"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let provider_org = provider
            .and_then(|p| p.get("organization"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        assert_eq!(provider_url.as_deref(), Some("https://example.com/"));
        assert_eq!(provider_org.as_deref(), Some("Example Co"));

        let docs = obj
            .get("documentationUrl")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let icon = obj
            .get("iconUrl")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        assert_eq!(docs.as_deref(), Some("https://example.com/docs"));
        assert_eq!(icon.as_deref(), Some("https://example.com/icon.png"));
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
                extensions: vec![],
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
