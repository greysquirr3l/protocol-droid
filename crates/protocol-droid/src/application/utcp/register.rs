//! Registering a manual: deciding which of its tools may be used.
//!
//! # Why registration is a step of its own
//!
//! A manual is a document. Reading it tells you what a provider *says* it
//! has; it says nothing about what the client should let you do. A provider
//! that serves a manual over HTTPS can still put a `cli` tool in it, and a
//! `cli` tool is a program to run on this machine.
//!
//! The specification's answer is the `allowed_communication_protocols`
//! field, and its default is deny: a manual may only register and call
//! tools that use its own protocol type. Registration is where that filter
//! is applied, and it is applied again before every call, because a tool
//! that was registered under one permission can be handed to a caller under
//! another.
//!
//! # Where the permission comes from
//!
//! From the registration, never from the document. The registration is the
//! call template the *user* wrote to say "register this manual"; the
//! document is what the provider served. If the document could widen its own
//! permission, the filter would be a speed bump rather than a control.
//!
//! # The two filters
//!
//! A tool is callable when both of these hold:
//!
//! 1. The registration's policy permits the tool's protocol type.
//! 2. The tool's own template permits it, when the tool's template names a
//!    list. An absent or empty list means the tool permits only its own
//!    protocol type — the same default, applied at the tool's level.
//!
//! The second filter can only narrow. A tool's template is part of the
//! document, so it is not allowed to widen anything.
//!
//! # What a refused tool reports
//!
//! The refusal names the protocol the tool wanted and what was permitted,
//! because the reader's next question is always "so what do I do instead?".
//! The specification's own warning says the same thing.

use crate::domain::utcp::call_template::CallTemplate;
use crate::domain::utcp::manual::{Manual, Tool};
use crate::domain::utcp::policy::ProtocolPolicy;

/// A manual that has been registered, and the tools that survived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    /// The manual's own version.
    pub manual_version: String,
    /// The protocol version the manual was written against.
    pub utcp_version: String,
    /// The permission the registration carries.
    pub policy: ProtocolPolicy,
    /// The tools that may be called.
    pub tools: Vec<Tool>,
    /// The tools that were excluded, and why.
    pub refused: Vec<RefusedTool>,
}

/// A tool that registration excluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedTool {
    /// The tool's name, as the manual names it.
    pub name: String,
    /// The protocol type the tool wanted to be called over.
    pub protocol: String,
    /// Why it was excluded.
    pub reason: RefusalReason,
}

/// Why a tool was excluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefusalReason {
    /// The registration does not permit this protocol type.
    NotPermitted {
        /// The protocol types the registration permits.
        allowed: Vec<String>,
    },
    /// The tool's own template does not permit itself.
    NotSelfPermitted {
        /// The protocol types the tool's template permits.
        allowed: Vec<String>,
    },
}

impl core::fmt::Display for RefusalReason {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotPermitted { allowed } => write!(
                formatter,
                "the registration permits {} only",
                if allowed.is_empty() {
                    "nothing".to_owned()
                } else {
                    allowed.join(", ")
                }
            ),
            Self::NotSelfPermitted { allowed } => write!(
                formatter,
                "the tool's own template permits {} only",
                if allowed.is_empty() {
                    "nothing".to_owned()
                } else {
                    allowed.join(", ")
                }
            ),
        }
    }
}

/// Registers a manual under a call template.
///
/// The template is the registration: it says how the manual was reached and
/// which protocols its tools may use. Its
/// `allowed_communication_protocols` decides the policy when it is present
/// and non-empty; otherwise the policy permits the template's own protocol
/// type and nothing else.
///
/// # Examples
///
/// ```
/// use protocol_droid::application::utcp::register::register;
/// use protocol_droid::domain::utcp::call_template::CallTemplate;
/// use protocol_droid::domain::utcp::HttpMethod;
///
/// # fn example(manual: &protocol_droid::domain::utcp::Manual) {
/// let template = CallTemplate::http(
///     "provider",
///     "https://api.example.com/utcp",
///     HttpMethod::Get,
/// );
/// let registration = register(&template, manual);
///
/// // The manual's tools use `http`, and the registration permits `http`.
/// assert!(registration.tools.iter().any(|tool| tool.name == "get_user"));
/// # }
/// ```
#[must_use]
pub fn register(template: &CallTemplate, manual: &Manual) -> Registration {
    let policy = ProtocolPolicy::for_registration(template);
    let mut tools = Vec::new();
    let mut refused = Vec::new();
    for tool in &manual.tools {
        match classify(tool, &policy) {
            Ok(()) => tools.push(tool.clone()),
            Err(reason) => refused.push(RefusedTool {
                name: tool.name.clone(),
                protocol: tool.tool_call_template.call_template_type().to_owned(),
                reason,
            }),
        }
    }
    Registration {
        manual_version: manual.manual_version.to_string(),
        utcp_version: manual.utcp_version.to_string(),
        policy,
        tools,
        refused,
    }
}

/// Whether a tool may be called under this policy.
fn classify(tool: &Tool, policy: &ProtocolPolicy) -> Result<(), RefusalReason> {
    let protocol = tool.tool_call_template.call_template_type();
    if !policy.permits(protocol) {
        return Err(RefusalReason::NotPermitted {
            allowed: policy.allowed(),
        });
    }
    let self_permitted = self_permitted(&tool.tool_call_template);
    if !self_permitted.iter().any(|allowed| allowed == protocol) {
        return Err(RefusalReason::NotSelfPermitted {
            allowed: self_permitted,
        });
    }
    Ok(())
}

/// The protocol types a tool's own template permits.
///
/// An absent or empty list means the tool permits only its own protocol
/// type, which is the specification's default applied at the tool's level.
fn self_permitted(template: &CallTemplate) -> Vec<String> {
    match &template.allowed_communication_protocols {
        Some(list) if !list.is_empty() => list.clone(),
        _ => vec![template.call_template_type().to_owned()],
    }
}

#[cfg(test)]
mod tests {
    use crate::domain::utcp::HttpMethod;
    use crate::domain::utcp::call_template::CallTemplate;
    use crate::domain::utcp::manual::Manual;

    use super::register;

    /// A registration that permits only HTTP tools.
    fn http_registration() -> CallTemplate {
        let mut template = CallTemplate::http(
            "provider",
            "https://api.example.com/utcp",
            HttpMethod::Get,
        );
        template.allowed_communication_protocols = None;
        template
    }

    /// A manual with one HTTP tool and one CLI tool.
    fn mixed_manual() -> Result<Manual, crate::domain::utcp::ManualError> {
        Manual::parse(
            r#"{
              "manual_version": "1.0.0",
              "utcp_version": "1.0.1",
              "tools": [
                {
                  "name": "get_user",
                  "description": "Fetch a user",
                  "inputs": {"type": "object"},
                  "tool_call_template": {
                    "name": "users",
                    "call_template_type": "http",
                    "url": "https://api.example.com/users",
                    "http_method": "GET"
                  }
                },
                {
                  "name": "run",
                  "description": "Run a command",
                  "inputs": {"type": "object"},
                  "tool_call_template": {
                    "name": "shell",
                    "call_template_type": "cli",
                    "command": "rm"
                  }
                }
              ]
            }"#,
        )
    }

    /// A tool the registration permits is kept.
    #[test]
    fn a_permitted_tool_is_kept() -> Result<(), crate::domain::utcp::ManualError> {
        let manual = mixed_manual()?;
        let registration = register(&http_registration(), &manual);
        assert_eq!(registration.tools.len(), 1);
        assert_eq!(registration.tools.first().map(|tool| tool.name.as_str()), Some("get_user"));
        Ok(())
    }

    /// A tool the registration does not permit is refused, and the refusal
    /// names what was permitted.
    #[test]
    fn a_tool_the_registration_does_not_permit_is_refused(
    ) -> Result<(), crate::domain::utcp::ManualError> {
        let manual = mixed_manual()?;
        let registration = register(&http_registration(), &manual);
        assert_eq!(registration.refused.len(), 1);
        let refused = registration.refused.first();
        assert_eq!(refused.map(|refused| refused.name.as_str()), Some("run"));
        assert_eq!(refused.map(|refused| refused.protocol.as_str()), Some("cli"));
        Ok(())
    }

    /// The manual's versions travel with the registration.
    #[test]
    fn the_manuals_versions_travel_with_the_registration(
    ) -> Result<(), crate::domain::utcp::ManualError> {
        let manual = mixed_manual()?;
        let registration = register(&http_registration(), &manual);
        assert_eq!(registration.manual_version, "1.0.0");
        assert_eq!(registration.utcp_version, "1.0.1");
        Ok(())
    }

    /// The registration's policy is the one the caller supplied.
    #[test]
    fn the_registrations_policy_is_the_callers() -> Result<(), crate::domain::utcp::ManualError> {
        let manual = mixed_manual()?;
        let registration = register(&http_registration(), &manual);
        assert!(registration.policy.permits("http"));
        assert!(!registration.policy.permits("cli"));
        Ok(())
    }
}
