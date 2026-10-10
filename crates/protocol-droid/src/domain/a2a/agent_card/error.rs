//! Errors the [`AgentCardBuilder`](super::AgentCardBuilder) can raise.
//!
//! A single enum in its own file so a `match` is greppable and a
//! new variant does not touch the value-object file.

use std::sync::Arc;

use thiserror::Error;

/// Errors a builder can raise.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AgentCardError {
    /// `name` was empty.
    #[error("AgentCard.name must be non-empty")]
    EmptyName,
    /// `description` was empty.
    #[error("AgentCard.description must be non-empty")]
    EmptyDescription,
    /// `version` was empty.
    #[error("AgentCard.version must be non-empty")]
    EmptyVersion,
    /// `supported_interfaces` was empty.
    #[error("AgentCard.supported_interfaces must contain at least one interface")]
    NoSupportedInterfaces,
    /// `default_input_modes` was empty.
    #[error("AgentCard.default_input_modes must contain at least one mode")]
    NoInputModes,
    /// `default_output_modes` was empty.
    #[error("AgentCard.default_output_modes must contain at least one mode")]
    NoOutputModes,
    /// A skill was given an empty id, name, or description.
    #[error("AgentCard.skills[{index}].{field} must be non-empty")]
    EmptySkillField {
        /// The skill's position in `skills`.
        index: usize,
        /// The empty field, one of `id`, `name`, `description`.
        field: &'static str,
    },
    /// The provider's organization was empty.
    #[error("AgentCard.provider.organization must be non-empty")]
    EmptyProviderOrganization,
    /// A protocol extension was given an empty uri or description.
    #[error("AgentCard.capabilities.extensions[{index}].{field} must be non-empty")]
    EmptyExtensionField {
        /// The extension's position in `capabilities.extensions`.
        index: usize,
        /// The empty field, one of `uri`, `description`.
        field: &'static str,
    },
    /// A security scheme was registered with an empty name.
    #[error("AgentCard.security_schemes key must be non-empty")]
    EmptySecuritySchemeName,
    /// An API key security scheme has an empty `name` (the
    /// parameter name, not the scheme key).
    #[error("AgentCard.security_schemes[{name}].name must be non-empty")]
    EmptyAPIKeyParameterName {
        /// The scheme key in the `security_schemes` map.
        name: Arc<str>,
    },
    /// An HTTP security scheme has an empty `scheme` (the
    /// IANA-registered auth scheme, e.g. `Bearer`).
    #[error("AgentCard.security_schemes[{name}].scheme must be non-empty")]
    EmptyHTTPAuthScheme {
        /// The scheme key in the `security_schemes` map.
        name: Arc<str>,
    },
}
