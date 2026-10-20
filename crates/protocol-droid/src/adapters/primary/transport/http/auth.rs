//! Turning a request's headers into the `AuthContext` every stateful path
//! takes.
//!
//! # Why this is a port and not a function
//!
//! Because the port belongs to the consumer, and the consumer is the router.
//! The seam is fixed so that an implementation is a constructor argument
//! rather than a change to the router's signature — and so that the *default*
//! is a decision someone made rather than an absence.
//!
//! Two implementations ship: [`Anonymous`], which is the default, and
//! [`OAuthAuthenticator`] behind the `oauth` feature, which validates a token
//! presented to this server against an external authorization server. This
//! module is the port and the anonymous default; the OAuth half lives in the
//! [`oauth`] submodule.
//!
//! # Why the default is anonymous and that is still fail-secure
//!
//! [`Anonymous`] returns [`AuthContext::anonymous`], which holds no scopes.
//! That looks like "no authentication by default", and the honest framing is
//! that authentication is opt-in — a consumer wanting it passes
//! [`OAuthAuthenticator`] or its own [`Authenticator`] to
//! [`HttpBuilder::with_authenticator`](super::HttpBuilder::with_authenticator)
//! and a policy to `with_policy`. What makes the default fail-secure rather
//! than fail-open is structural:
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

use std::future::Future;
use std::pin::Pin;

use axum::http::HeaderMap;

use crate::domain::registry::{AuthContext, ScopeSet};

/// What a request presented, for an authenticator to judge.
///
/// Borrowed rather than owned because the only field is a header value that is
/// already in the request, and copying an `Authorization` header into a
/// long-lived struct is exactly the kind of retention the redaction rule is
/// about.
///
/// `Debug` is written by hand below rather than derived. The derive would print
/// the raw `Authorization` header — that is, the bearer token — into any
/// `{:?}` this type meets: a panic message, a failed assertion, a log line
/// somebody adds while debugging. The field's own doc comment used to say this
/// crate never logs it, which was true and was not a property of anything.
#[derive(Clone, Copy)]
pub struct Credentials<'a> {
    /// The `Authorization` header, if the client sent one.
    authorization: Option<&'a str>,
}

impl core::fmt::Debug for Credentials<'_> {
    /// Prints a fingerprint, never the header.
    ///
    /// The same shape as
    /// [`RequestId::redacted`](crate::domain::RequestId::redacted): a short
    /// prefix and a length, so two log lines can be correlated without either
    /// carrying the credential. Four characters of a 256-bit token is not a
    /// meaningful reduction in the search space, and correlation is worth it.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Credentials")
            .field("authorization", &self.authorization.map(Fingerprint))
            .finish()
    }
}

/// A presented header value, rendered as a fingerprint.
///
/// Fingerprints the *token*, not the whole header: every bearer credential
/// starts `Bearer `, so a prefix of the header is the same four characters on
/// every request in the world and correlates nothing at all.
struct Fingerprint<'a>(&'a str);

impl core::fmt::Debug for Fingerprint<'_> {
    /// Renders `#prefix…length`.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let token = self
            .0
            .split_once(char::is_whitespace)
            .map_or(self.0, |(_, rest)| rest.trim());
        let head: String = token.chars().take(4).collect();
        write!(formatter, "#{head}…{}", token.chars().count())
    }
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
/// # Why this is async
///
/// Because establishing authenticity is network I/O. An introspecting source
/// asks the authorization server about every request, and a JWKS source asks
/// it for keys on rotation; neither can be answered from what is already in
/// the process. A synchronous signature here would have to be served by an
/// authenticator that cannot actually check a real token.
///
/// The boxed-future form is deliberate rather than an `async fn` in the trait.
/// [`McpRouter`](super::router::McpRouter) holds `Arc<dyn Authenticator>`, so
/// the port has to stay object safe, and native `async fn` on a trait is RPITIT
/// and is not. It is written out rather than pulled in through `async-trait`
/// for one method, and the allocation it adds is not what is being measured
/// here — this runs once per HTTP request, beside a session lookup and a JSON
/// parse, and the router was already dispatching through a `dyn`.
///
/// This is the "boxed futures are already the norm" case that the crate's own
/// rule carves out, and it is deliberately *not* what
/// [`TokenSource`](crate::application::oauth::TokenSource) does: that port takes
/// `impl TokenSource` at the call site and never pays a boxing.
///
/// # Implementing this
///
/// Return [`AuthContext::anonymous`] for a request you do not recognise. Do not
/// return a context with scopes for a credential you could not verify, and do
/// not treat a malformed `Authorization` value as a present-but-empty one: a
/// token that fails to parse is not a token.
///
/// Reporting *why* a credential failed is deliberately not in the signature.
/// Every unusable credential arrives as the same anonymous context, and the
/// policy turns that into one status code. An authenticator that could
/// distinguish "expired" from "unknown key" would hand a caller an oracle for
/// probing which tokens exist.
///
/// # Example
///
/// ```
/// use std::future::Future;
/// use std::pin::Pin;
///
/// use protocol_droid::adapters::primary::transport::http::{
///     Authenticator, Credentials,
/// };
/// use protocol_droid::domain::registry::{AuthContext, Scope, ScopeSet};
///
/// /// A stand-in for the real thing: it accepts exactly one well-known token
/// /// so the test has something to exercise. The real implementation is
/// /// [`OAuthAuthenticator`](super::auth::oauth::OAuthAuthenticator).
/// struct StaticToken;
///
/// impl Authenticator for StaticToken {
///     fn authenticate<'a>(
///         &'a self,
///         presented: &'a Credentials<'_>,
///     ) -> Pin<Box<dyn Future<Output = AuthContext> + Send + 'a>> {
///         let context = match presented.authorization() {
///             Some("Bearer test-token") => AuthContext::new(
///                 "test",
///                 ScopeSet::empty().with(Scope::Read),
///             ),
///             _ => AuthContext::anonymous(),
///         };
///         Box::pin(std::future::ready(context))
///     }
/// }
/// # let _ = StaticToken;
/// ```
pub trait Authenticator: Send + Sync {
    /// The context `presented` is allowed to act under.
    fn authenticate<'a>(
        &'a self,
        presented: &'a Credentials<'_>,
    ) -> Pin<Box<dyn Future<Output = AuthContext> + Send + 'a>>;
}

/// The default: no credential is accepted, and no scope is ever held.
///
/// The `_ =>` arm is the whole design. A header this does not recognise, a
/// header that is absent, and a header that is malformed all reach the same
/// place, because a half-check here would be a credential check that is wrong
/// in a way nobody can see. The future is already resolved, so awaiting it
/// costs a poll and nothing else.
#[derive(Debug, Clone, Copy, Default)]
pub struct Anonymous;

impl Authenticator for Anonymous {
    fn authenticate<'a>(
        &'a self,
        _presented: &'a Credentials<'_>,
    ) -> Pin<Box<dyn Future<Output = AuthContext> + Send + 'a>> {
        Box::pin(std::future::ready(AuthContext::anonymous()))
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
    /// `refuse` answers `None` and the policy behaves exactly
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

/// The bearer-token authenticator — the real implementation of
/// [`Authenticator`] behind T16's OAuth work.
///
/// `pub(super)` rather than `pub` or `pub(crate)`, and both of those are
/// lints' suggestions that do not work here. `pub` is
/// [`unreachable_pub`](clippy::unreachable_pub): the enclosing `auth` module is
/// private, so the module itself is not reachable from outside the crate.
/// `pub(crate)` is [`redundant_pub_crate`](clippy::redundant_pub_crate): the
/// enclosing module has already restricted it to the crate, so the `crate` in
/// `pub(crate)` says nothing the parent does not. What is actually needed is
/// exactly `http` — the module that re-exports these items — which is one level
/// up and no further.
pub(super) mod oauth;

pub use oauth::{OAuthAuthenticator, SystemClock, WallClock};

#[cfg(test)]
mod tests {
    /// A credential of the kind a real client sends.
    const SECRET: &str = "super-secret-bearer-token-value-0123456789";

    /// The redaction boundary, for a presented credential.
    ///
    /// The value is untouched — an authenticator has to be able to read the
    /// token to check it — and the *rendering* carries only a fingerprint. This
    /// is the shape `SessionStore`'s `Debug` already has and the one
    /// [`RequestId::redacted`](crate::domain::RequestId::redacted) serves.
    ///
    /// It is a test on `Debug` specifically rather than on `Display` because
    /// `Debug` is what a panic, an `assert_eq!`, and every future log line
    /// expand to. A type that is safe to hold and unsafe to print has no
    /// redaction at all.
    #[test]
    fn the_debug_form_redacts_but_the_value_does_not() -> Result<(), TestError> {
        let header = format!("Bearer {SECRET}");
        let headers = header_map_with(Some(header.as_str()))?;
        let credentials = Credentials::from_headers(&headers);
        let rendered = format!("{credentials:?}");

        assert!(!rendered.contains(SECRET), "{rendered}");
        assert!(!rendered.contains("secret"), "{rendered}");
        // The fingerprint still correlates: the first four characters of the
        // token, and its length.
        assert!(rendered.contains("#supe…42"), "{rendered}");
        // The half the authenticator needs is untouched.
        assert_eq!(credentials.authorization(), Some(header.as_str()));
        Ok(())
    }

    /// A fingerprint that included the scheme would correlate nothing.
    ///
    /// Every bearer header starts `Bearer `, so a prefix taken from the whole
    /// header is `Bear` on every request in the world. The test states the
    /// prefix is of the *token* so that an implementation cannot quietly move
    /// the split and still pass the assertion above.
    #[test]
    fn the_fingerprint_covers_the_token_and_not_the_scheme() -> Result<(), TestError> {
        let header = format!("Bearer {SECRET}");
        let headers = header_map_with(Some(header.as_str()))?;
        let rendered = format!("{:?}", Credentials::from_headers(&headers));

        assert!(rendered.contains("#supe…"), "{rendered}");
        assert!(!rendered.contains("Bear"), "{rendered}");
        Ok(())
    }

    /// A request that presented nothing is shown as having presented nothing.
    ///
    /// Redaction must not become "everything looks the same": a missing
    /// credential and a present one are different facts for a reader of a log,
    /// and both are safe to print.
    #[test]
    fn a_missing_credential_says_so() {
        let headers = HeaderMap::new();
        let rendered = format!("{:?}", Credentials::from_headers(&headers));

        assert!(rendered.contains("None"), "{rendered}");
    }
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
    #[tokio::test]
    async fn the_default_authenticator_holds_no_scopes_for_anything() -> Result<(), TestError> {
        for presented in [None, Some("Bearer test-token"), Some("Basic abc"), Some("")] {
            let headers = header_map_with(presented)?;
            let context = Anonymous
                .authenticate(&Credentials::from_headers(&headers))
                .await;
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
    #[tokio::test]
    async fn a_malformed_credential_is_refused_under_a_required_policy() -> Result<(), TestError> {
        let policy = AuthPolicy::Require(ScopeSet::empty().with(Scope::Read));
        for malformed in [Some(""), Some("Bearer"), Some("not-a-scheme x"), Some("??")] {
            let headers = header_map_with(malformed)?;
            let context = Anonymous
                .authenticate(&Credentials::from_headers(&headers))
                .await;
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
