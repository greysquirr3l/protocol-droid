//! The client-side decisions of the authorization flow that need no network.
//!
//! Three rules from the authorization page, each a pure function so it can be
//! tested without an authorization server:
//! <https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization#scope-selection-strategy>
//!
//! - **Initial scope**: use the `scope` from the `WWW-Authenticate` challenge;
//!   failing that, all of `scopes_supported`; failing that, send none.
//! - **Step-up**: re-authorize with the union of what was requested before and
//!   what the new challenge names, so earlier permissions are not lost.
//! - **Retry limit**: a few attempts, then a permanent failure.
//!
//! `offline_access` is added only when the server lists it in its own
//! `scopes_supported`, and a resource must never advertise it.

use crate::domain::oauth::challenge::BearerChallenge;

/// How many step-up authorizations are attempted for one operation before the
/// failure is treated as permanent. "No more than a few times".
pub const MAX_STEP_UP_ATTEMPTS: u32 = 3;

/// The `offline_access` scope, which asks for a refresh token.
pub const OFFLINE_ACCESS: &str = "offline_access";

/// Picks the scope for the first authorization request.
///
/// `None` means send no `scope` parameter at all, which is what the page asks
/// when neither the challenge nor `scopes_supported` names any.
#[must_use]
pub fn initial_scope(
    challenge: &BearerChallenge,
    resource_scopes_supported: Option<&[String]>,
) -> Option<String> {
    let challenged = challenge.scopes();
    if !challenged.is_empty() {
        return Some(challenged.join(" "));
    }
    resource_scopes_supported
        .filter(|scopes| !scopes.is_empty())
        .map(|scopes| scopes.join(" "))
}

/// The scope for a step-up: everything requested before, plus the challenge.
///
/// Order is kept and duplicates dropped, so the request is stable and does not
/// grow on a retry that asks for what it already has.
#[must_use]
pub fn step_up_scope(previous: Option<&str>, challenge: &BearerChallenge) -> Option<String> {
    let mut merged: Vec<&str> = Vec::new();
    for scope in previous
        .into_iter()
        .flat_map(str::split_ascii_whitespace)
        .chain(challenge.scopes())
    {
        if !merged.contains(&scope) {
            merged.push(scope);
        }
    }
    (!merged.is_empty()).then(|| merged.join(" "))
}

/// Adds `offline_access` to `scope` when the authorization server lists it.
///
/// A client that wants a refresh token "MAY add `offline_access`" only when the
/// server's metadata contains it in `scopes_supported`; asking for it
/// otherwise requests something the server never offered.
#[must_use]
pub fn with_offline_access(scope: Option<String>, server_scopes_supported: &[String]) -> Option<String> {
    if !server_scopes_supported.iter().any(|s| s == OFFLINE_ACCESS) {
        return scope;
    }
    match scope {
        Some(existing) if existing.split_ascii_whitespace().any(|s| s == OFFLINE_ACCESS) => {
            Some(existing)
        }
        Some(existing) => Some(format!("{existing} {OFFLINE_ACCESS}")),
        None => Some(OFFLINE_ACCESS.to_owned()),
    }
}

/// Whether another step-up may be attempted after `attempts` have been made.
#[must_use]
pub const fn may_step_up(attempts: u32) -> bool {
    attempts < MAX_STEP_UP_ATTEMPTS
}

#[cfg(test)]
mod tests {
    use super::{initial_scope, may_step_up, step_up_scope, with_offline_access};
    use crate::domain::oauth::challenge::parse_bearer;

    fn challenge(header: &str) -> crate::domain::oauth::challenge::BearerChallenge {
        parse_bearer(header).unwrap_or_default()
    }

    #[test]
    fn the_challenge_wins_over_scopes_supported() {
        let supported = vec!["x".to_owned()];
        assert_eq!(
            initial_scope(&challenge(r#"Bearer scope="a b""#), Some(&supported)),
            Some("a b".to_owned())
        );
    }

    #[test]
    fn scopes_supported_is_the_fallback_and_absence_sends_none() {
        let supported = vec!["x".to_owned(), "y".to_owned()];
        assert_eq!(
            initial_scope(&challenge("Bearer"), Some(&supported)),
            Some("x y".to_owned())
        );
        assert_eq!(initial_scope(&challenge("Bearer"), None), None);
    }

    #[test]
    fn step_up_keeps_what_was_granted() {
        assert_eq!(
            step_up_scope(Some("read"), &challenge(r#"Bearer scope="write read""#)),
            Some("read write".to_owned())
        );
    }

    #[test]
    fn offline_access_only_when_the_server_lists_it() {
        let listed = vec!["offline_access".to_owned()];
        assert_eq!(
            with_offline_access(Some("a".to_owned()), &listed),
            Some("a offline_access".to_owned())
        );
        assert_eq!(with_offline_access(Some("a".to_owned()), &[]), Some("a".to_owned()));
        assert_eq!(with_offline_access(None, &listed), Some("offline_access".to_owned()));
    }

    #[test]
    fn step_up_stops_after_a_few_attempts() {
        assert!(may_step_up(2));
        assert!(!may_step_up(3));
    }
}
