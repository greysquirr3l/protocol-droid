//! Turning a request's headers into the `AuthContext` every stateful path
//! takes.
//!
//! # Why this is a port and not a function
//!
//! Because the port belongs to the consumer, and the consumer is the router.
//! T16 implements OAuth; this module does not and does not pretend to. What it
//! does is fix the shape of the seam so that adding an implementation is a
//! constructor argument rather than a change to the router's signature — and
//! so that the *default* is a decision someone made rather than an absence.
//!
//! # Why the default is anonymous and that is still fail-secure
//!
//! [`Anonymous`] returns [`AuthContext::anonymous`], which holds no scopes.
//! That looks like "no authentication by default", and the honest framing is
//! that this adapter does not authenticate and T16 will. What makes it
//! fail-secure rather than fail-open is structural:
//!
//! - Every stateful route resolves an `AuthContext` before it looks at the
//!   session, the body, or the stream. There is no path that skips it, because
//!   the route handler's only way to get a session is through a `&AuthContext`
//!   it was handed.
//! - The resolved context is checked against
//!   [`ServerConfig::required_scopes`](super::ServerConfig::required_scopes)
//!   before anything else happens. A configuration that names a scope refuses
//!   every anonymous caller, and the refusal is `401`.
//! - [`AuthContext::anonymous`] cannot be widened after the fact. The only
//!   constructors that carry scopes are `new` and `unrestricted`, and neither
//!   is reachable from this module.
//!
//! So the risk of this module is not that it fails open — it is that a
//! deployment forgets to configure a scope. That is why [`AuthPolicy`] exists:
//! it is the one place a deployment states, out loud, which of "this endpoint
//! is public" and "this endpoint needs a credential" is true.

use axum::http::HeaderMap;

use crate::domain::registry::{AuthContext, ScopeSet};

/// What a request presented, for an authenticator to judge.
///
/// Borrowed rather than owned because the only field is a header value that is
/// already in the request, and copying an `Authorization` header into a
/// long-lived struct is exactly the kind of retention the redaction rule is
/// about.
#[derive(Debug, Clone, Copy)]
pub struct Credentials<'a> {
    /// The `Authorization` header, if the client sent one.
    authorization: Option<&'a str>,
}

impl<'a> Credentials<'a> {
    /// Reads the `Authorization` header out of a request.
    #[must_use]
    pub fn from_headers(headers: &'a HeaderMap) -> Self {
        Self {
            authorization: headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
        }
    }

    /// The raw `Authorization` value.
    ///
    /// Present for an implementor that has to parse it. This crate never logs
    /// it, and the doc comment on [`Credentials`] is the reason: the rule is
    /// "never log tokens", and the way that is kept is by this being the only
    /// path to the value and by nothing in this module formatting it.
    #[must_use]
    pub const fn authorization(&self) -> Option<&'a str> {
        self.authorization
    }
}

/// Resolves a request's credentials into the context its handlers run under.
///
/// # Implementing this
///
/// Return [`AuthContext::anonymous`] for a request you do not recognise. Do not
/// return a context with scopes for a credential you could not verify, and do
/// not treat a malformed `Authorization` value as a present-but-empty one: a
/// token that fails to parse is not a token.
///
/// # Example
///
/// ```
/// use protocol_droid::adapters::primary::transport::http::{
///     Authenticator, Credentials,
/// };
/// use protocol_droid::domain::registry::{AuthContext, Scope, ScopeSet};
///
/// /// A stand-in for the real thing: T16 replaces this with OAuth. It accepts
/// /// exactly one well-known token so the test has something to exercise.
/// struct StaticToken;
///
/// impl Authenticator for StaticToken {
///     fn authenticate(&self, presented: &Credentials<'_>) -> AuthContext {
///         match presented.authorization() {
///             Some("Bearer test-token") => AuthContext::new(
///                 "test",
///                 ScopeSet::empty().with(Scope::Read),
///             ),
///             _ => AuthContext::anonymous(),
///         }
///     }
/// }
/// # let _ = StaticToken;
/// ```
pub trait Authenticator: Send + Sync {
    /// The context `presented` is allowed to act under.
    fn authenticate(&self, presented: &Credentials<'_>) -> AuthContext;
}

/// The default: no credential is accepted, and no scope is ever held.
///
/// The `_ =>` arm is the whole design. A header this does not recognise, a
/// header that is absent, and a header that is malformed all reach the same
/// place, because a real credential check belongs to T16 and a half-check here
/// would be a credential check that is wrong in a way nobody can see.
#[derive(Debug, Clone, Copy, Default)]
pub struct Anonymous;

impl Authenticator for Anonymous {
    fn authenticate(&self, _presented: &Credentials<'_>) -> AuthContext {
        AuthContext::anonymous()
    }
}

/// What a deployment has decided about this endpoint's exposure.
///
/// Separate from [`Anonymous`] because "we have not configured anything yet"
/// and "this endpoint is deliberately public" are different states, and
/// conflating them is how a default becomes a decision nobody made. A
/// deployment that means "public" writes [`AuthPolicy::Public`]; a deployment
/// that means "authenticated" writes [`AuthPolicy::Require`] and names the
/// scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthPolicy {
    /// Every request is served under an anonymous context.
    ///
    /// Correct for a loopback stdio-equivalent and for a conformance runner,
    /// and wrong for anything reachable from another host. It is the default
    /// because a `Router` that cannot be constructed until authentication is
    /// wired is a router nobody constructs while T16 is unimplemented.
    #[default]
    Public,
    /// Every request must hold these scopes.
    ///
    /// Note what an *empty* `ScopeSet` does here: it requires nothing, so
    /// [`refuse`](Self::refuse) answers `None` and the policy behaves exactly
    /// like [`Public`](Self::Public). That is a footgun — "I require
    /// authentication" followed by nothing to require is a configuration
    /// mistake — and it is the kind that is best made visible by a test rather
    /// than by a runtime check, because there is no error to return from a
    /// `const` builder on a value that is otherwise perfectly well-formed.
    /// `an_empty_requirement_is_public_and_says_so` pins the behaviour so it
    /// cannot change silently: if a future dispatch wants `Require` to reject
    /// an empty set, that test is the thing that has to be updated on purpose.
    ///
    /// The same applies to
    /// [`ServerConfig::with_required_scopes`](super::ServerConfig::with_required_scopes):
    /// it stores what it is given and does not validate it.
    Require(ScopeSet),
}

impl AuthPolicy {
    /// The scopes a request must hold under this policy.
    #[must_use]
    pub const fn required(self) -> ScopeSet {
        match self {
            Self::Public => ScopeSet::empty(),
            Self::Require(scopes) => scopes,
        }
    }

    /// The refusal this policy produces for `context`, if any.
    ///
    /// The `401`-before-`403` order is the point. A caller that holds nothing
    /// is told to authenticate; a caller that holds a principal but lacks a
    /// scope is told its credential is insufficient. Answering `403` to a
    /// caller holding nothing tells the client to retry with more permission,
    /// which it cannot do — it has nothing to add.
    ///
    /// The test for "holds nothing" is [`AuthContext::scopes`], **not**
    /// [`AuthContext::principal`]. The domain's [`AuthContext::anonymous`] is
    /// a *named* principal — it is called `anonymous` rather than being the
    /// empty string, deliberately, so a trace can distinguish a peer that
    /// presented no credential from a named principal whose credential granted
    /// nothing. So a check on the principal being empty is not merely
    /// inelegant here, it is unreachable: it would classify the anonymous
    /// caller as authenticated-but-insufficient and answer `403` to a client
    /// that has no credential at all. The scopes are the thing the policy is
    /// actually about, and a caller holding none of them is the caller that
    /// needs to authenticate first.
    #[must_use]
    pub const fn refuse(&self, context: &AuthContext) -> Option<super::Refusal> {
        let required = self.required();
        if required.is_empty() {
            return None;
        }
        if context.scopes().is_empty() {
            return Some(super::Refusal::Unauthenticated);
        }
        if !context.holds(required) {
            return Some(super::Refusal::Forbidden);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, header};

    use crate::domain::registry::{AuthContext, Scope, ScopeSet};

    use super::{Anonymous, AuthPolicy, Authenticator, Credentials};
    use crate::adapters::primary::transport::http::Refusal;

    type TestError = Box<dyn std::error::Error>;

    fn header_map_with(authorization: Option<&str>) -> Result<HeaderMap, TestError> {
        let mut headers = HeaderMap::new();
        if let Some(value) = authorization {
            headers.insert(header::AUTHORIZATION, HeaderValue::from_str(value)?);
        }
        Ok(headers)
    }

    /// The default authenticator grants nothing, whatever the request says.
    ///
    /// The property under test is the *negative* one. A test that only showed
    /// "a good token works" would pass against an authenticator that also
    /// accepted `Bearer anything-else`, and the failure that matters is a
    /// credential check that is more permissive than it looks.
    #[test]
    fn the_default_authenticator_holds_no_scopes_for_anything() -> Result<(), TestError> {
        for presented in [None, Some("Bearer test-token"), Some("Basic abc"), Some("")] {
            let headers = header_map_with(presented)?;
            let context = Anonymous.authenticate(&Credentials::from_headers(&headers));
            assert_eq!(context.scopes(), ScopeSet::empty());
            // The claim is "this credential bought nothing", and the precise
            // spelling of that is equality with the domain's own anonymous
            // context. Asserting the principal is *empty* would be wrong: the
            // domain names the anonymous principal, on purpose, so a trace can
            // tell a peer that presented nothing from a named peer that
            // presented something worth nothing.
            assert_eq!(context, AuthContext::anonymous());
        }
        Ok(())
    }

    /// A malformed credential under a required policy is refused, not passed.
    ///
    /// The negative case the module docs promise and the one a naive
    /// implementation gets wrong: a header that is present but unparseable
    /// looks like "a credential was supplied", and an implementation that
    /// branches on presence rather than on what the credential proves will let
    /// it through to whatever the route does next.
    #[test]
    fn a_malformed_credential_is_refused_under_a_required_policy() -> Result<(), TestError> {
        let policy = AuthPolicy::Require(ScopeSet::empty().with(Scope::Read));
        for malformed in [Some(""), Some("Bearer"), Some("not-a-scheme x"), Some("??")] {
            let headers = header_map_with(malformed)?;
            let context = Anonymous.authenticate(&Credentials::from_headers(&headers));
            assert_eq!(
                policy.refuse(&context),
                Some(Refusal::Unauthenticated),
                "malformed credential {malformed:?} was not refused"
            );
        }
        Ok(())
    }

    /// A caller who presented a credential that granted nothing gets `401`.
    ///
    /// Separate from the anonymous case because the domain deliberately keeps
    /// the two apart: a named principal holding nothing presented credentials
    /// worth nothing, and a client that gets `403` for that is told to ask for
    /// permission it has no way to obtain. There is nothing to add, so the
    /// honest answer is "authenticate".
    #[test]
    fn a_credential_that_granted_nothing_is_unauthenticated() {
        let policy = AuthPolicy::Require(ScopeSet::empty().with(Scope::Read));
        let granted_nothing = AuthContext::new("alice", ScopeSet::empty());
        assert_eq!(
            policy.refuse(&granted_nothing),
            Some(Refusal::Unauthenticated),
        );
    }

    /// `Require` with an empty scope set requires nothing, and therefore
    /// behaves exactly like `Public`.
    ///
    /// Pinned deliberately. An empty requirement is a configuration mistake
    /// that is silent at every other layer, and the honest fix for that is to
    /// make the behaviour *stated* rather than to leave it emergent: this test
    /// is what a future dispatch has to update on purpose if it decides
    /// `Require` should reject an empty set.
    #[test]
    fn an_empty_requirement_is_public_and_says_so() {
        let policy = AuthPolicy::Require(ScopeSet::empty());
        // Equivalence is *behavioural*, not structural: the two are different
        // enum values and asserting they are equal would be asserting that
        // `Require` stops existing. What matters is that a caller cannot tell
        // the difference through the port, which is what these two lines say.
        assert!(policy.required().is_empty());
        assert_eq!(policy.refuse(&AuthContext::anonymous()), None);
        assert_eq!(policy.required(), AuthPolicy::Public.required());
        assert_eq!(
            policy.refuse(&AuthContext::anonymous()),
            AuthPolicy::Public.refuse(&AuthContext::anonymous()),
        );
    }

    /// The `Authorization` value is read from the request, not guessed.
    ///
    /// Without this, a test elsewhere that asserts "an unauthenticated request
    /// is refused" could be passing because the header was never read at all.
    #[test]
    fn the_credential_is_read_off_the_request() -> Result<(), TestError> {
        let with = header_map_with(Some("Bearer abc"))?;
        let without = header_map_with(None)?;
        assert_eq!(
            Credentials::from_headers(&with).authorization(),
            Some("Bearer abc"),
        );
        assert_eq!(Credentials::from_headers(&without).authorization(), None);
        Ok(())
    }

    /// A scope-bearing policy refuses an anonymous caller with `401`.
    ///
    /// This is the fail-secure claim the module docs make, and it is the one
    /// that distinguishes a transport that checks from one that merely has an
    /// `AuthContext` parameter nobody reads.
    #[test]
    fn a_requiring_policy_refuses_an_anonymous_caller() {
        let policy = AuthPolicy::Require(ScopeSet::empty().with(Scope::Read));
        let context = AuthContext::anonymous();
        assert_eq!(policy.refuse(&context), Some(Refusal::Unauthenticated));
    }

    /// A caller with a principal but too little scope gets `403`, not `401`.
    ///
    /// The two are different recoveries — "authenticate" against "you are
    /// authenticated but not allowed" — and collapsing them sends a client
    /// looking for a credential it already has.
    #[test]
    fn an_insufficient_scope_is_403_and_not_401() {
        let policy = AuthPolicy::Require(ScopeSet::empty().with(Scope::Write));
        let read_only = AuthContext::new("alice", ScopeSet::empty().with(Scope::Read));
        assert_eq!(policy.refuse(&read_only), Some(Refusal::Forbidden));
        // The same policy lets a caller with the scope through.
        let writer = AuthContext::new("alice", ScopeSet::empty().with(Scope::Write));
        assert_eq!(policy.refuse(&writer), None);
    }

    /// The public policy serves an anonymous caller.
    ///
    /// The counterpart to the refusal test, and the reason the enum exists
    /// rather than a bare `ScopeSet` field: "no scopes required" has to be
    /// something a deployment says on purpose.
    #[test]
    fn the_public_policy_serves_everyone() {
        let policy = AuthPolicy::Public;
        assert!(policy.required().is_empty());
        assert_eq!(policy.refuse(&AuthContext::anonymous()), None);
        assert_eq!(policy.required(), AuthPolicy::default().required());
    }
}
