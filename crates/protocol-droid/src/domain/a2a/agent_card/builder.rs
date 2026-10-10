//! The builder for [`AgentCard`].
//!
//! The builder is the only way to get an `AgentCard` once the
//! optional fields land. Every mutation happens on a builder;
//! the `build` method is the only way to read out a
//! value-object, and the only place the spec's required-field
//! invariants are checked.

use std::sync::Arc;

use crate::domain::a2a::agent_card::error::AgentCardError;
use crate::domain::a2a::agent_card::types::{
    AgentCapabilities, AgentCard, AgentInterface, AgentSkill,
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
    capabilities: AgentCapabilities,
    default_input_modes: Vec<Arc<str>>,
    default_output_modes: Vec<Arc<str>>,
    skills: Vec<AgentSkill>,
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

    /// Sets the capability set.
    #[must_use]
    pub const fn with_capabilities(mut self, capabilities: AgentCapabilities) -> Self {
        self.capabilities = capabilities;
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
        Ok(AgentCard {
            name,
            description,
            version,
            supported_interfaces: self.supported_interfaces,
            capabilities: self.capabilities,
            default_input_modes: self.default_input_modes,
            default_output_modes: self.default_output_modes,
            skills: self.skills,
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
}
