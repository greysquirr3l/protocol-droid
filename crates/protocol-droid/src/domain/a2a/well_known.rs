//! The A2A `AgentCard` well-known URI.
//!
//! # Where this is
//!
//! A2A servers publish their [`AgentCard`](super::AgentCard) at a
//! standardised URI: `https://{agent-server-domain}/.well-known/agent-card.json`.
//! The path component is fixed by the spec; the host is the server's
//! domain. RFC 8615 governs the `.well-known` convention; the A2A
//! spec chooses the `agent-card.json` slot.
//!
//! # What this is not
//!
//! The well-known URI is the *primary* discovery mechanism, not the
//! only one. A2A also defines a tenant-scoped variant for multi-tenant
//! servers and a registry-based discovery for federated setups. This
//! module is the *root* well-known path; the tenant and registry
//! forms are T27 follow-up.
//!
//! Source: A2A v1.0 specification, §8 "Agent Discovery" and
//! `https://a2a-protocol.org/latest/topics/agent-discovery/`.

/// The path component of the well-known URI, with a leading slash.
///
/// The constant is the path *only* — no scheme, no host. A handler
/// matches an incoming request path against this string; the host the
/// client dialled is whatever the server's transport is bound to.
pub const AGENT_CARD_WELL_KNOWN_PATH: &str = "/.well-known/agent-card.json";

/// Returns `true` if `path` is the well-known agent-card path.
///
/// The match is exact — case-sensitive, no trailing slash, no query
/// string. A peer that asks for `/.well-known/agent-card.json/` is
/// asking for a different resource, and the well-known convention
/// says clients send the path as the spec writes it. A handler that
/// wants to be lenient can strip a trailing slash before calling.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::a2a::is_agent_card_well_known_path;
///
/// assert!(is_agent_card_well_known_path("/.well-known/agent-card.json"));
/// assert!(!is_agent_card_well_known_path("/.well-known/other"));
/// assert!(!is_agent_card_well_known_path("/agent-card.json"));
/// ```
#[must_use]
pub fn is_agent_card_well_known_path(path: &str) -> bool {
    // The match is a single load-immediate-and-compare on the hot
    // path: the agent-card matcher in the HTTP adapter is called
    // once per request, and the constant-length comparison lets
    // the compiler lower it to a memcmp on the literal. The
    // function is intentionally non-`const` because
    // `PartialEq<[u8]>` is not `const`-stable in 1.98; the
    // runtime cost is the same and the call site is not in a
    // const context.
    path.as_bytes() == AGENT_CARD_WELL_KNOWN_PATH.as_bytes()
}

#[cfg(test)]
mod tests {
    // The tests here are pure constant-folding on the match function;
    // no fallible calls, no expect/panic.

    use super::{AGENT_CARD_WELL_KNOWN_PATH, is_agent_card_well_known_path};

    #[test]
    fn the_constant_is_what_the_spec_names() {
        assert_eq!(AGENT_CARD_WELL_KNOWN_PATH, "/.well-known/agent-card.json");
    }

    #[test]
    fn matches_the_exact_path() {
        assert!(is_agent_card_well_known_path("/.well-known/agent-card.json"));
    }

    #[test]
    fn rejects_close_misses() {
        for near in [
            "/.well-known/agent-card",            // missing .json
            "/.well-known/agent-card.json/",      // trailing slash
            "/.well-known/Agent-Card.json",       // case mismatch
            "/.well-known/agent-card-json",       // dash, not dot
            "/.well-known/other",
            "/agent-card.json",                   // missing /.well-known
            "/.well-known/",                      // truncated
        ] {
            assert!(
                !is_agent_card_well_known_path(near),
                "{near:?} is not the well-known agent-card path"
            );
        }
    }

    #[test]
    fn rejects_empty_and_wrong_length() {
        assert!(!is_agent_card_well_known_path(""));
        assert!(!is_agent_card_well_known_path("/"));
        assert!(!is_agent_card_well_known_path(
            "/.well-known/agent-card.json/extra"
        ));
    }
}
