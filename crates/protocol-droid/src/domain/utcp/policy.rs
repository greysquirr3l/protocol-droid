//! Which protocols a manual's tools may be called over.
//!
//! # The rule the specification sets
//!
//! UTCP v1.1 added `allowed_communication_protocols` to every call template,
//! and the default is **deny**:
//!
//! > If not specified, a manual can only register and call tools that use its
//! > own protocol type.
//!
//! The specification gives the reason plainly: *"This prevents unexpected
//! behavior where a seemingly HTTP-only manual could execute CLI commands."*
//!
//! # Why this is a security control and not a convenience
//!
//! The user input to a UTCP client is a URL. Everything else — which tools
//! exist, what protocol each one uses, what command a `cli` template runs —
//! comes out of the document the URL served. A document that says "call this
//! one with `cli`" is asking the client to run a program, on the machine, on
//! the user's behalf.
//!
//! So the filter is not about tidiness. A manual served over HTTP that names
//! an `mcp` template whose server entry carries a `command` is a manual
//! asking to be run as a local process, and without this filter that is
//! exactly what a client would do with it.
//!
//! # Where the list comes from
//!
//! From the *registration*, not from the document. The specification puts
//! `allowed_communication_protocols` on the manual's own call template — the
//! one the user wrote to say "register this manual" — and the tools are
//! filtered against it at registration and again at call time. A document
//! cannot widen its own permission: it can only be refused.
//!
//! # What the default is when the list is absent
//!
//! The manual's own protocol type. A manual registered over HTTP may call
//! HTTP tools; a manual registered over MCP may call MCP tools. An empty
//! list means the same thing as an absent one — the specification's table
//! says so explicitly, and treating an empty list as "allow everything"
//! would make the field's absence weaker than its presence.

use std::collections::BTreeSet;

use crate::domain::utcp::call_template::CallTemplate;

/// Which protocols a registered manual's tools may use.
///
/// Built once, at registration, from the manual's own call template. The
/// same value is consulted again before every call, because a tool that was
/// registered under one permission may be handed to a caller under another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolPolicy {
    /// The protocol types this policy permits.
    allowed: BTreeSet<String>,
}

impl ProtocolPolicy {
    /// The policy a registration implies, read from its call template.
    ///
    /// The template's `allowed_communication_protocols` decides when it is
    /// present and non-empty. When it is absent or empty the policy permits
    /// the template's own `call_template_type` and nothing else.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::domain::utcp::HttpMethod;
    /// use protocol_droid::domain::utcp::call_template::CallTemplate;
    /// use protocol_droid::domain::utcp::policy::ProtocolPolicy;
    ///
    /// let mut template = CallTemplate::http(
    ///     "provider",
    ///     "https://api.example.com/utcp",
    ///     HttpMethod::Get,
    /// );
    /// template.allowed_communication_protocols = None;
    ///
    /// let policy = ProtocolPolicy::for_registration(&template);
    /// assert!(policy.permits("http"));
    /// assert!(!policy.permits("cli"));
    /// ```
    #[must_use]
    pub fn for_registration(template: &CallTemplate) -> Self {
        match &template.allowed_communication_protocols {
            Some(list) if !list.is_empty() => Self::allowing(list.iter().cloned()),
            _ => Self::allowing(std::iter::once(template.call_template_type().to_owned())),
        }
    }

    /// A policy permitting exactly these protocol types.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::domain::utcp::policy::ProtocolPolicy;
    ///
    /// let policy = ProtocolPolicy::allowing(["http".to_owned(), "cli".to_owned()]);
    /// assert!(policy.permits("cli"));
    /// assert!(!policy.permits("mcp"));
    /// ```
    #[must_use]
    pub fn allowing<I, S>(protocols: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            allowed: protocols.into_iter().map(Into::into).collect(),
        }
    }

    /// A policy permitting nothing at all.
    ///
    /// The fail-secure default for a caller that has not said what it
    /// allows. Every tool is refused, and the refusal names what was
    /// permitted so the reader can see why.
    #[must_use]
    pub const fn deny_all() -> Self {
        Self {
            allowed: BTreeSet::new(),
        }
    }

    /// Whether a tool using this protocol type may be called.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::domain::utcp::policy::ProtocolPolicy;
    ///
    /// let policy = ProtocolPolicy::allowing(["http".to_owned()]);
    /// assert!(policy.permits("http"));
    /// assert!(!policy.permits("cli"));
    /// ```
    #[must_use]
    pub fn permits(&self, call_template_type: &str) -> bool {
        self.allowed.contains(call_template_type)
    }

    /// The protocol types this policy permits, in order.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::domain::utcp::policy::ProtocolPolicy;
    ///
    /// let policy = ProtocolPolicy::allowing(["http".to_owned()]);
    /// assert_eq!(policy.allowed(), ["http".to_owned()]);
    /// ```
    #[must_use]
    pub fn allowed(&self) -> Vec<String> {
        self.allowed.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::ProtocolPolicy;
    use crate::domain::utcp::HttpMethod;
    use crate::domain::utcp::call_template::CallTemplate;

    /// An HTTP template that names the protocols it permits.
    fn template_with(permitted: Option<&[&str]>) -> CallTemplate {
        let mut template = CallTemplate::http("provider", "https://api.example.com/utcp", HttpMethod::Get);
        template.allowed_communication_protocols = permitted.map(|list| {
            list.iter().map(|name| (*name).to_owned()).collect()
        });
        template
    }

    /// An absent list permits the template's own protocol type.
    #[test]
    fn an_absent_list_permits_only_the_templates_own_protocol() {
        let policy = ProtocolPolicy::for_registration(&template_with(None));
        assert!(policy.permits("http"));
        assert!(!policy.permits("cli"));
        assert!(!policy.permits("mcp"));
    }

    /// An empty list means the same thing as an absent one.
    #[test]
    fn an_empty_list_permits_only_the_templates_own_protocol() {
        let policy = ProtocolPolicy::for_registration(&template_with(Some(&[])));
        assert!(policy.permits("http"));
        assert!(!policy.permits("cli"));
    }

    /// A present list is the whole permission, and it replaces the default.
    #[test]
    fn a_present_list_replaces_the_default() {
        let policy = ProtocolPolicy::for_registration(&template_with(Some(&["cli"])));
        assert!(policy.permits("cli"));
        assert!(!policy.permits("http"));
    }

    /// The default is deny: a policy that names nothing permits nothing.
    #[test]
    fn denying_everything_permits_nothing() {
        let policy = ProtocolPolicy::deny_all();
        assert!(!policy.permits("http"));
        assert!(!policy.permits("mcp"));
        assert!(!policy.permits("cli"));
        assert!(policy.allowed().is_empty());
    }

    /// The permitted list comes back in a stable order.
    #[test]
    fn the_permitted_list_is_ordered() {
        let policy = ProtocolPolicy::allowing(["mcp".to_owned(), "cli".to_owned(), "http".to_owned()]);
        assert_eq!(policy.allowed(), ["cli".to_owned(), "http".to_owned(), "mcp".to_owned()]);
    }
}
