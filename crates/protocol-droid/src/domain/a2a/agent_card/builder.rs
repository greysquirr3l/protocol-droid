//! The builder for [`AgentCard`].
//!
//! The builder is the only way to get an `AgentCard` once the
//! optional fields land. Every mutation happens on a builder;
//! the `build` method is the only way to read out a
//! value-object, and the only place the spec's required-field
//! invariants are checked.

use std::collections::BTreeMap;
use std::sync::Arc;

use url::Url;

use crate::domain::a2a::agent_card::error::AgentCardError;
use crate::domain::a2a::agent_card::security_requirement::SecurityRequirement;
use crate::domain::a2a::agent_card::security_scheme::SecurityScheme;
use crate::domain::a2a::agent_card::types::{
    AgentCapabilities, AgentCard, AgentExtension, AgentInterface, AgentProvider, AgentSkill,
};

/// The builder for [`AgentCard`].
///
/// The builder is a sequence of `with_*` calls the caller makes
/// consciously; the [`build`](Self::build) method is the only
/// way to get an `AgentCard`, and it checks the invariants the
/// spec mandates at the end.
#[derive(Debug, Default)]
pub struct AgentCardBuilder {
    name: Option<Arc<str>>,
    description: Option<Arc<str>>,
    version: Option<Arc<str>>,
    supported_interfaces: Vec<AgentInterface>,
    provider: Option<AgentProvider>,
    documentation_url: Option<Url>,
    icon_url: Option<Url>,
    capabilities: AgentCapabilities,
    default_input_modes: Vec<Arc<str>>,
    default_output_modes: Vec<Arc<str>>,
    skills: Vec<AgentSkill>,
    security_schemes: BTreeMap<Arc<str>, SecurityScheme>,
    security_requirements: Vec<SecurityRequirement>,
}

impl AgentCardBuilder {
    /// Starts a new builder with no fields set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the agent's human-readable name.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<Arc<str>>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Sets the agent's human-readable description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<Arc<str>>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Sets the agent's version string.
    #[must_use]
    pub fn with_version(mut self, version: impl Into<Arc<str>>) -> Self {
        self.version = Some(version.into());
        self
    }

    /// Adds an interface the agent publishes. The first call is
    /// the preferred interface; subsequent calls are fallbacks
    /// in the order the spec reads.
    #[must_use]
    pub fn with_interface(mut self, interface: AgentInterface) -> Self {
        self.supported_interfaces.push(interface);
        self
    }

    /// Sets the service provider of the agent. Optional in the
    /// spec; a builder that does not call this method produces
    /// a card with no provider field on the wire.
    #[must_use]
    pub fn with_provider(mut self, provider: AgentProvider) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Sets the URL providing additional documentation about the
    /// agent. Optional in the spec; absent on the wire unless
    /// this method is called.
    #[must_use]
    pub fn with_documentation_url(mut self, url: Url) -> Self {
        self.documentation_url = Some(url);
        self
    }

    /// Sets the URL to an icon for the agent. Optional in the
    /// spec; absent on the wire unless this method is called.
    /// UI clients surface this as the agent's avatar in a
    /// directory listing.
    #[must_use]
    pub fn with_icon_url(mut self, url: Url) -> Self {
        self.icon_url = Some(url);
        self
    }

    /// Sets the capability set.
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: AgentCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Adds a single protocol extension the agent supports.
    /// Convenience for the common "one extension at a time"
    /// pattern; a caller that wants to set several at once
    /// builds the [`AgentCapabilities`] directly and uses
    /// [`with_capabilities`](Self::with_capabilities).
    #[must_use]
    pub fn with_extension(mut self, extension: AgentExtension) -> Self {
        self.capabilities.extensions.push(extension);
        self
    }

    /// Registers a security scheme under `name`. The same
    /// name is the key in
    /// [`AgentCard::security_schemes`](super::AgentCard::security_schemes)
    /// and the key a client uses in
    /// [`SecurityRequirement`](super::SecurityRequirement)
    /// to demand that scheme. The wire form is `OpenAPI 3.x`.
    #[must_use]
    pub fn with_security_scheme(
        mut self,
        name: impl Into<Arc<str>>,
        scheme: SecurityScheme,
    ) -> Self {
        self.security_schemes.insert(name.into(), scheme);
        self
    }

    /// Adds a security requirement to the agent's
    /// published list. The requirements are tried in
    /// the order they are added; a client picks the
    /// first one it can satisfy.
    #[must_use]
    pub fn with_security_requirement(mut self, requirement: SecurityRequirement) -> Self {
        self.security_requirements.push(requirement);
        self
    }

    /// Adds a default input mode.
    #[must_use]
    pub fn with_input_mode(mut self, mode: impl Into<Arc<str>>) -> Self {
        self.default_input_modes.push(mode.into());
        self
    }

    /// Adds a default output mode.
    #[must_use]
    pub fn with_output_mode(mut self, mode: impl Into<Arc<str>>) -> Self {
        self.default_output_modes.push(mode.into());
        self
    }

    /// Adds a skill the agent advertises.
    #[must_use]
    pub fn with_skill(mut self, skill: AgentSkill) -> Self {
        self.skills.push(skill);
        self
    }

    /// Builds the card, checking the spec's required-field
    /// invariants.
    ///
    /// # Errors
    ///
    /// [`AgentCardError`] on the first invariant failure. The
    /// error names the field so the caller can fix the input.
    pub fn build(self) -> Result<AgentCard, AgentCardError> {
        let name = self.name.ok_or(AgentCardError::EmptyName)?;
        if name.is_empty() {
            return Err(AgentCardError::EmptyName);
        }
        let description = self.description.ok_or(AgentCardError::EmptyDescription)?;
        if description.is_empty() {
            return Err(AgentCardError::EmptyDescription);
        }
        let version = self.version.ok_or(AgentCardError::EmptyVersion)?;
        if version.is_empty() {
            return Err(AgentCardError::EmptyVersion);
        }
        if self.supported_interfaces.is_empty() {
            return Err(AgentCardError::NoSupportedInterfaces);
        }
        if self.default_input_modes.is_empty() {
            return Err(AgentCardError::NoInputModes);
        }
        if self.default_output_modes.is_empty() {
            return Err(AgentCardError::NoOutputModes);
        }
        for (index, skill) in self.skills.iter().enumerate() {
            if skill.id.is_empty() {
                return Err(AgentCardError::EmptySkillField { index, field: "id" });
            }
            if skill.name.is_empty() {
                return Err(AgentCardError::EmptySkillField { index, field: "name" });
            }
            if skill.description.is_empty() {
                return Err(AgentCardError::EmptySkillField {
                    index,
                    field: "description",
                });
            }
        }
        if let Some(provider) = &self.provider
            && provider.organization.is_empty()
        {
            return Err(AgentCardError::EmptyProviderOrganization);
        }
        for (index, extension) in self.capabilities.extensions.iter().enumerate() {
            if extension.uri.is_empty() {
                return Err(AgentCardError::EmptyExtensionField { index, field: "uri" });
            }
            if extension.description.is_empty() {
                return Err(AgentCardError::EmptyExtensionField {
                    index,
                    field: "description",
                });
            }
        }
        for (name, scheme) in &self.security_schemes {
            if name.is_empty() {
                return Err(AgentCardError::EmptySecuritySchemeName);
            }
            match scheme {
                SecurityScheme::ApiKey(api_key) => {
                    if api_key.name.is_empty() {
                        return Err(AgentCardError::EmptyAPIKeyParameterName {
                            name: Arc::clone(name),
                        });
                    }
                }
                SecurityScheme::Http(http) => {
                    if http.scheme.is_empty() {
                        return Err(AgentCardError::EmptyHTTPAuthScheme {
                            name: Arc::clone(name),
                        });
                    }
                }
                SecurityScheme::OAuth2(oauth2) => {
                    if !oauth2.flows.has_any() {
                        return Err(AgentCardError::EmptyOAuthFlows {
                            name: Arc::clone(name),
                        });
                    }
                }
                SecurityScheme::OpenIdConnect(_)
                | SecurityScheme::MutualTls(_) => {
                    // OpenIdConnectSecurityScheme has no
                    // builder-time invariants beyond the
                    // required `open_id_connect_url`, which
                    // is enforced by `url::Url::parse` at the
                    // call site. MutualTlsSecurityScheme has
                    // no required fields at all. Both are
                    // accepted as-is.
                }
            }
        }
        Ok(AgentCard {
            name,
            description,
            version,
            supported_interfaces: self.supported_interfaces,
            provider: self.provider,
            documentation_url: self.documentation_url,
            icon_url: self.icon_url,
            capabilities: self.capabilities,
            default_input_modes: self.default_input_modes,
            default_output_modes: self.default_output_modes,
            skills: self.skills,
            security_schemes: self.security_schemes,
            security_requirements: self.security_requirements,
        })
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::expect_used,
        reason = "tests assert on fallible calls with named failure messages; propagating errors would replace a clear failure with noise"
    )]

    use std::sync::Arc;

    use super::{AgentCard, AgentCardBuilder};
    use crate::domain::a2a::agent_card::error::AgentCardError;
    use crate::domain::a2a::agent_card::types::{AgentSkill, JSONRPC};
    use crate::domain::a2a::A2aVersion;

    type TestError = Box<dyn std::error::Error>;

    fn sample_interface() -> Result<crate::domain::a2a::agent_card::types::AgentInterface, TestError> {
        Ok(crate::domain::a2a::agent_card::types::AgentInterface {
            url: url::Url::parse("https://example.com/a2a/v1")?,
            protocol_binding: JSONRPC.clone(),
            protocol_version: A2aVersion::V1_0,
            tenant: None,
        })
    }

    fn sample_skill() -> AgentSkill {
        AgentSkill {
            id: Arc::from("research"),
            name: Arc::from("Research"),
            description: Arc::from("Look things up."),
            tags: vec![],
        }
    }

    fn sample_card() -> Result<AgentCard, TestError> {
        Ok(AgentCardBuilder::new()
            .with_name("Research Agent")
            .with_description("Looks things up on the web.")
            .with_version("1.0.0")
            .with_interface(sample_interface()?)
            .with_capabilities(crate::domain::a2a::agent_card::types::AgentCapabilities::default())
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .with_skill(sample_skill())
            .build()?)
    }

    // --- builder invariants ------------------------------------------

    #[test]
    fn empty_name_is_rejected() -> Result<(), TestError> {
        let err = AgentCardBuilder::new()
            .with_name("")
            .with_description("d")
            .with_version("1.0.0")
            .with_interface(sample_interface()?)
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .build()
            .expect_err("an empty name must be refused");
        assert_eq!(err, AgentCardError::EmptyName);
        Ok(())
    }

    #[test]
    fn missing_name_is_rejected() -> Result<(), TestError> {
        let err = AgentCardBuilder::new()
            .with_description("d")
            .with_version("1.0.0")
            .with_interface(sample_interface()?)
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .build()
            .expect_err("a missing name must be refused");
        assert_eq!(err, AgentCardError::EmptyName);
        Ok(())
    }

    #[test]
    fn empty_description_is_rejected() -> Result<(), TestError> {
        let err = AgentCardBuilder::new()
            .with_name("n")
            .with_description("")
            .with_version("1.0.0")
            .with_interface(sample_interface()?)
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .build()
            .expect_err("an empty description must be refused");
        assert_eq!(err, AgentCardError::EmptyDescription);
        Ok(())
    }

    #[test]
    fn empty_version_is_rejected() -> Result<(), TestError> {
        let err = AgentCardBuilder::new()
            .with_name("n")
            .with_description("d")
            .with_version("")
            .with_interface(sample_interface()?)
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .build()
            .expect_err("an empty version must be refused");
        assert_eq!(err, AgentCardError::EmptyVersion);
        Ok(())
    }

    #[test]
    fn no_supported_interfaces_is_rejected() {
        let err = AgentCardBuilder::new()
            .with_name("n")
            .with_description("d")
            .with_version("1.0.0")
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .build()
            .expect_err("zero interfaces must be refused");
        assert_eq!(err, AgentCardError::NoSupportedInterfaces);
    }

    #[test]
    fn no_input_modes_is_rejected() -> Result<(), TestError> {
        let err = AgentCardBuilder::new()
            .with_name("n")
            .with_description("d")
            .with_version("1.0.0")
            .with_interface(sample_interface()?)
            .with_output_mode("text/plain")
            .build()
            .expect_err("zero input modes must be refused");
        assert_eq!(err, AgentCardError::NoInputModes);
        Ok(())
    }

    #[test]
    fn no_output_modes_is_rejected() -> Result<(), TestError> {
        let err = AgentCardBuilder::new()
            .with_name("n")
            .with_description("d")
            .with_version("1.0.0")
            .with_interface(sample_interface()?)
            .with_input_mode("text/plain")
            .build()
            .expect_err("zero output modes must be refused");
        assert_eq!(err, AgentCardError::NoOutputModes);
        Ok(())
    }

    #[test]
    fn empty_skill_id_is_rejected() -> Result<(), TestError> {
        let bad = AgentSkill {
            id: Arc::from(""),
            name: Arc::from("Research"),
            description: Arc::from("Look things up."),
            tags: vec![],
        };
        let err = AgentCardBuilder::new()
            .with_name("n")
            .with_description("d")
            .with_version("1.0.0")
            .with_interface(sample_interface()?)
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .with_skill(bad)
            .build()
            .expect_err("an empty skill id must be refused");
        assert_eq!(
            err,
            AgentCardError::EmptySkillField { index: 0, field: "id" }
        );
        Ok(())
    }

    #[test]
    fn valid_card_builds() -> Result<(), TestError> {
        let card = sample_card()?;
        assert_eq!(card.name.as_ref(), "Research Agent");
        assert_eq!(card.supported_interfaces.len(), 1);
        assert_eq!(card.skills.len(), 1);
        Ok(())
    }

    // --- optional fields ---------------------------------------------

    #[test]
    fn valid_provider_builds() -> Result<(), TestError> {
        let card = AgentCardBuilder::new()
            .with_name("n")
            .with_description("d")
            .with_version("1.0.0")
            .with_interface(sample_interface()?)
            .with_provider(super::AgentProvider {
                url: url::Url::parse("https://example.com")?,
                organization: Arc::from("Example Co"),
            })
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .build()?;
        let provider = card
            .provider
            .as_ref()
            .expect("a card built with_provider must carry the provider");
        assert_eq!(provider.organization.as_ref(), "Example Co");
        assert_eq!(provider.url.as_str(), "https://example.com/");
        Ok(())
    }

    #[test]
    fn empty_provider_organization_is_rejected() -> Result<(), TestError> {
        let err = AgentCardBuilder::new()
            .with_name("n")
            .with_description("d")
            .with_version("1.0.0")
            .with_interface(sample_interface()?)
            .with_provider(super::AgentProvider {
                url: url::Url::parse("https://example.com")?,
                organization: Arc::from(""),
            })
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .build()
            .expect_err("an empty provider organization must be refused");
        assert_eq!(err, AgentCardError::EmptyProviderOrganization);
        Ok(())
    }

    #[test]
    fn unset_provider_carries_none() -> Result<(), TestError> {
        // A card that does not call `with_provider` must
        // serialise without a `provider` field. The test
        // pins both the in-memory shape (`None`) and the
        // wire shape (the field is absent).
        let card = sample_card()?;
        assert!(card.provider.is_none());
        let value = serde_json::to_value(&card)?;
        let obj = value
            .as_object()
            .expect("the card must serialise to a JSON object");
        assert!(!obj.contains_key("provider"));
        Ok(())
    }

    #[test]
    fn documentation_url_round_trips() -> Result<(), TestError> {
        let url = url::Url::parse("https://example.com/docs")?;
        let card = AgentCardBuilder::new()
            .with_name("n")
            .with_description("d")
            .with_version("1.0.0")
            .with_interface(sample_interface()?)
            .with_documentation_url(url.clone())
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .build()?;
        assert_eq!(card.documentation_url.as_ref(), Some(&url));
        let json = serde_json::to_string(&card)?;
        let parsed: AgentCard = serde_json::from_str(&json)?;
        assert_eq!(parsed, card);
        Ok(())
    }

    #[test]
    fn icon_url_round_trips() -> Result<(), TestError> {
        let url = url::Url::parse("https://example.com/icon.png")?;
        let card = AgentCardBuilder::new()
            .with_name("n")
            .with_description("d")
            .with_version("1.0.0")
            .with_interface(sample_interface()?)
            .with_icon_url(url.clone())
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .build()?;
        assert_eq!(card.icon_url.as_ref(), Some(&url));
        let json = serde_json::to_string(&card)?;
        let parsed: AgentCard = serde_json::from_str(&json)?;
        assert_eq!(parsed, card);
        Ok(())
    }

    // --- `AgentCapabilities::extensions` ------------------------------

    #[test]
    fn valid_extension_builds() -> Result<(), TestError> {
        let card = AgentCardBuilder::new()
            .with_name("n")
            .with_description("d")
            .with_version("1.0.0")
            .with_interface(sample_interface()?)
            .with_extension(super::AgentExtension {
                uri: Arc::from("https://example.com/extension/audio-input"),
                description: Arc::from("Accepts audio input on the SendMessage payload."),
                required: false,
                params: None,
            })
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .build()?;
        assert_eq!(card.capabilities.extensions.len(), 1);
        let ext = card
            .capabilities
            .extensions
            .first()
            .expect("the card carries exactly one extension");
        assert_eq!(ext.uri.as_ref(), "https://example.com/extension/audio-input");
        assert!(!ext.required);
        assert!(ext.params.is_none());
        Ok(())
    }

    #[test]
    fn empty_extension_uri_is_rejected() -> Result<(), TestError> {
        let err = AgentCardBuilder::new()
            .with_name("n")
            .with_description("d")
            .with_version("1.0.0")
            .with_interface(sample_interface()?)
            .with_extension(super::AgentExtension {
                uri: Arc::from(""),
                description: Arc::from("d"),
                required: false,
                params: None,
            })
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .build()
            .expect_err("an empty extension uri must be refused");
        assert_eq!(
            err,
            AgentCardError::EmptyExtensionField { index: 0, field: "uri" }
        );
        Ok(())
    }

    #[test]
    fn empty_extension_description_is_rejected() -> Result<(), TestError> {
        let err = AgentCardBuilder::new()
            .with_name("n")
            .with_description("d")
            .with_version("1.0.0")
            .with_interface(sample_interface()?)
            .with_extension(super::AgentExtension {
                uri: Arc::from("https://example.com/ext"),
                description: Arc::from(""),
                required: false,
                params: None,
            })
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .build()
            .expect_err("an empty extension description must be refused");
        assert_eq!(
            err,
            AgentCardError::EmptyExtensionField {
                index: 0,
                field: "description"
            }
        );
        Ok(())
    }

    #[test]
    fn extension_params_round_trip_as_json_object() -> Result<(), TestError> {
        // `params` is `google.protobuf.Struct` in the proto, a
        // JSON object. The test pins the round-trip via
        // `serde_json::Map`: set a value, serialise, parse,
        // assert equality.
        let mut params = serde_json::Map::new();
        params.insert(
            "mimeType".to_owned(),
            serde_json::Value::String("audio/wav".to_owned()),
        );
        let card = AgentCardBuilder::new()
            .with_name("n")
            .with_description("d")
            .with_version("1.0.0")
            .with_interface(sample_interface()?)
            .with_extension(super::AgentExtension {
                uri: Arc::from("https://example.com/ext"),
                description: Arc::from("d"),
                required: false,
                params: Some(params.clone()),
            })
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .build()?;
        let value = serde_json::to_value(
            card.capabilities
                .extensions
                .first()
                .expect("the card carries exactly one extension"),
        )?;
        let obj = value
            .as_object()
            .expect("extension must serialise to a JSON object");
        let mime = obj
            .get("params")
            .and_then(serde_json::Value::as_object)
            .and_then(|p| p.get("mimeType"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        assert_eq!(mime.as_deref(), Some("audio/wav"));
        Ok(())
    }

    // --- security schemes --------------------------------------------

    #[test]
    fn api_key_security_scheme_round_trips() -> Result<(), TestError> {
        use super::super::security_scheme::{APIKeyLocation, APIKeySecurityScheme, SecurityScheme};

        let card = AgentCardBuilder::new()
            .with_name("n")
            .with_description("d")
            .with_version("1.0.0")
            .with_interface(sample_interface()?)
            .with_security_scheme(
                "publicRead",
                SecurityScheme::ApiKey(APIKeySecurityScheme {
                    description: Some(Arc::from("Read-only API key.")),
                    in_: APIKeyLocation::Header,
                    name: Arc::from("X-API-Key"),
                }),
            )
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .build()?;
        let scheme = card
            .security_schemes
            .get("publicRead")
            .expect("the registered scheme must be present in the map");
        let json = serde_json::to_value(scheme)?;
        let obj = json
            .as_object()
            .expect("the scheme must serialise to a JSON object");
        assert_eq!(
            obj.get("type").and_then(serde_json::Value::as_str),
            Some("apiKey")
        );
        assert_eq!(
            obj.get("in").and_then(serde_json::Value::as_str),
            Some("header")
        );
        assert_eq!(
            obj.get("name").and_then(serde_json::Value::as_str),
            Some("X-API-Key")
        );
        Ok(())
    }

    #[test]
    fn http_security_scheme_round_trips() -> Result<(), TestError> {
        use super::super::security_scheme::{HTTPAuthSecurityScheme, SecurityScheme};

        let card = AgentCardBuilder::new()
            .with_name("n")
            .with_description("d")
            .with_version("1.0.0")
            .with_interface(sample_interface()?)
            .with_security_scheme(
                "bearerAuth",
                SecurityScheme::Http(HTTPAuthSecurityScheme {
                    description: None,
                    scheme: Arc::from("Bearer"),
                    bearer_format: Some(Arc::from("JWT")),
                }),
            )
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .build()?;
        let scheme = card
            .security_schemes
            .get("bearerAuth")
            .expect("the registered scheme must be present in the map");
        let json = serde_json::to_value(scheme)?;
        let obj = json
            .as_object()
            .expect("the scheme must serialise to a JSON object");
        assert_eq!(
            obj.get("type").and_then(serde_json::Value::as_str),
            Some("http")
        );
        assert_eq!(
            obj.get("scheme").and_then(serde_json::Value::as_str),
            Some("Bearer")
        );
        assert_eq!(
            obj.get("bearerFormat").and_then(serde_json::Value::as_str),
            Some("JWT")
        );
        Ok(())
    }

    #[test]
    fn empty_security_scheme_name_is_rejected() -> Result<(), TestError> {
        use super::super::security_scheme::{APIKeyLocation, APIKeySecurityScheme, SecurityScheme};

        let err = AgentCardBuilder::new()
            .with_name("n")
            .with_description("d")
            .with_version("1.0.0")
            .with_interface(sample_interface()?)
            .with_security_scheme(
                "",
                SecurityScheme::ApiKey(APIKeySecurityScheme {
                    description: None,
                    in_: APIKeyLocation::Header,
                    name: Arc::from("X-API-Key"),
                }),
            )
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .build()
            .expect_err("an empty scheme name must be refused");
        assert_eq!(err, AgentCardError::EmptySecuritySchemeName);
        Ok(())
    }

    #[test]
    fn empty_api_key_parameter_name_is_rejected() -> Result<(), TestError> {
        use super::super::security_scheme::{APIKeyLocation, APIKeySecurityScheme, SecurityScheme};

        let err = AgentCardBuilder::new()
            .with_name("n")
            .with_description("d")
            .with_version("1.0.0")
            .with_interface(sample_interface()?)
            .with_security_scheme(
                "publicRead",
                SecurityScheme::ApiKey(APIKeySecurityScheme {
                    description: None,
                    in_: APIKeyLocation::Header,
                    name: Arc::from(""),
                }),
            )
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .build()
            .expect_err("an empty API key parameter name must be refused");
        assert_eq!(
            err,
            AgentCardError::EmptyAPIKeyParameterName {
                name: Arc::from("publicRead"),
            }
        );
        Ok(())
    }

    #[test]
    fn empty_http_auth_scheme_is_rejected() -> Result<(), TestError> {
        use super::super::security_scheme::{HTTPAuthSecurityScheme, SecurityScheme};

        let err = AgentCardBuilder::new()
            .with_name("n")
            .with_description("d")
            .with_version("1.0.0")
            .with_interface(sample_interface()?)
            .with_security_scheme(
                "bearerAuth",
                SecurityScheme::Http(HTTPAuthSecurityScheme {
                    description: None,
                    scheme: Arc::from(""),
                    bearer_format: None,
                }),
            )
            .with_input_mode("text/plain")
            .with_output_mode("text/plain")
            .build()
            .expect_err("an empty HTTP auth scheme must be refused");
        assert_eq!(
            err,
            AgentCardError::EmptyHTTPAuthScheme {
                name: Arc::from("bearerAuth"),
            }
        );
        Ok(())
    }

    #[test]
    fn unset_security_schemes_carries_empty_map() -> Result<(), TestError> {
        // A card that does not call `with_security_scheme`
        // must serialise without a `securitySchemes` field.
        // The test pins both the in-memory shape (an empty
        // `BTreeMap`) and the wire shape (the field is
        // absent — `BTreeMap` is empty so
        // `skip_serializing_if` drops it).
        let card = sample_card()?;
        assert!(card.security_schemes.is_empty());
        let value = serde_json::to_value(&card)?;
        let obj = value
            .as_object()
            .expect("the card must serialise to a JSON object");
        assert!(!obj.contains_key("securitySchemes"));
        Ok(())
    }
}
