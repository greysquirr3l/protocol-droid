//! Turning a presented access token into the [`AuthContext`] a request runs under.
//!
//! The security-relevant part of T16 lives here and is a pure function:
//! [`verify`] takes claims an adapter has already authenticated (signature
//! checked, issuer confirmed), the resource the request arrived at, and a clock,
//! and it either produces an [`AuthContext`] or a [`Refusal`] that names why.
//! It reads no clock and opens no socket, so every branch in it — wrong audience,
//! expired, not yet valid, wrong issuer — is testable without a network.
//!
//! # Two rules that are worth stating plainly
//!
//! **The audience must be exactly this server.** RFC 8707 resource indicators
//! exist so a token minted for one resource cannot be presented at another, and
//! `2025-06-18` changelog §4 (PR #734) makes them a client `MUST`. Comparing
//! for "contains" rather than equality would defeat the whole mechanism: a token
//! with a two-element `aud` naming a malicious server and ours would pass.
//!
//! **No token means no context.** [`Refusal`] has no variant that means "let it
//! through unprivileged", because the route's only way to reach a session is
//! through the `&AuthContext` it was handed, and a context nobody can widen is
//! the fail-secure default.
//!
//! # On scope names
//!
//! The claims carry whatever scope strings the authorization server put there,
//! as strings. The vendored spec names none of them — the only scope sentence in
//! any revision's changelog is `2025-11-25` §3 (SEP-835) on incremental consent
//! via `WWW-Authenticate` — so there is no specified vocabulary to map onto
//! [`Scope`](crate::domain::Scope). Turning `mcp:tools` into
//! [`Read`](crate::domain::Scope::Read) is a *deployment's* policy, so it lives
//! behind [`ScopePolicy`] as configuration rather than being hardcoded here as
//! if the specification had said so.

use std::sync::Arc;
use std::time::SystemTime;

use crate::domain::mcp::ResourceUri;
use crate::domain::mcp::authorization::AuthorizationModel;
use crate::domain::registry::{AuthContext, Scope, ScopeSet};

/// What an adapter established about a presented token, before this crate's
/// own rules ran.
///
/// "Established" means the adapter verified the signature (JWT) or asked the
/// authorization server (introspection). It is deliberately *not* a type that can
/// be built from a raw string in this crate: there is no path from an
/// `Authorization` header to a `TokenClaims` that skips the adapter, because
/// that is the property that keeps a decode from becoming a verify.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TokenClaims {
    subject: Option<Arc<str>>,
    issuer: Option<Arc<str>>,
    audience: Vec<ResourceUri>,
    expires_at: Option<SystemTime>,
    not_before: Option<SystemTime>,
    scopes: Vec<Arc<str>>,
}

impl TokenClaims {
    /// Claims with nothing in them, for an adapter to fill in.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the subject — who the token is about.
    #[must_use]
    pub fn with_subject(mut self, subject: impl Into<Arc<str>>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    /// Sets the issuer that minted the token.
    #[must_use]
    pub fn with_issuer(mut self, issuer: impl Into<Arc<str>>) -> Self {
        self.issuer = Some(issuer.into());
        self
    }

    /// Adds an audience entry.
    ///
    /// A builder rather than a setter because RFC 7519 §4.1.3 allows `aud` to
    /// be either a single string or an array, and both have to land in the same
    /// place.
    #[must_use]
    pub fn with_audience(mut self, audience: ResourceUri) -> Self {
        self.audience.push(audience);
        self
    }

    /// Sets when the token stops being valid.
    #[must_use]
    pub const fn with_expiry(mut self, at: SystemTime) -> Self {
        self.expires_at = Some(at);
        self
    }

    /// Sets when the token starts being valid.
    #[must_use]
    pub const fn with_not_before(mut self, at: SystemTime) -> Self {
        self.not_before = Some(at);
        self
    }

    /// Declares a scope the token carries.
    #[must_use]
    pub fn with_scope(mut self, scope: impl Into<Arc<str>>) -> Self {
        self.scopes.push(scope.into());
        self
    }

    /// The subject, if the token named one.
    #[must_use]
    pub fn subject(&self) -> Option<&str> {
        self.subject.as_deref()
    }

    /// The issuer, if the token named one.
    #[must_use]
    pub fn issuer(&self) -> Option<&str> {
        self.issuer.as_deref()
    }

    /// Every audience entry, already canonicalised.
    #[must_use]
    pub fn audience(&self) -> &[ResourceUri] {
        &self.audience
    }

    /// The scope strings, untranslated.
    #[must_use]
    pub fn scopes(&self) -> &[Arc<str>] {
        &self.scopes
    }

    /// Whether the token is bound to `resource`.
    ///
    /// Exact equality against a canonicalised [`ResourceUri`], and equality
    /// against *any* element of `aud` — which is what RFC 7519 §4.1.3 means by
    /// an array audience. "Contains the resource" is the same predicate; what it
    /// is *not* is "contains the resource's host", which is the check that would
    /// let a sibling server's token through.
    #[must_use]
    pub fn is_audience(&self, resource: &ResourceUri) -> bool {
        self.audience.iter().any(|audience| audience == resource)
    }
}

/// Why a presented token was refused.
///
/// Every variant is a distinct reason a `401` might be correct, and the HTTP
/// layer does not get to choose between them on the way out: an expired token
/// and a wrong-audience token produce the same status and the same challenge,
/// because telling a caller *which* failed is telling it something about the
/// token it should not learn.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// The token had expired by `now`.
    #[error("the access token has expired")]
    Expired,

    /// The token's `nbf` is in the future.
    #[error("the access token is not valid yet")]
    NotYetValid,

    /// The token is bound to some other resource.
    ///
    /// The one refusal worth naming in a log rather than in a response body.
    #[error("the access token is not bound to this resource")]
    AudienceMismatch,

    /// The token named an issuer, and it is not the one expected.
    ///
    /// Only reachable when the deployment configured an expected issuer.
    #[error("the access token was issued by an unexpected issuer")]
    IssuerMismatch,

    /// The token had no subject, so it names nobody to attribute an action to.
    #[error("the access token names no subject")]
    NoSubject,

    /// The token carried no scope this deployment recognises.
    ///
    /// A refusal rather than an empty [`ScopeSet`], so that "valid token, no
    /// permissions" and "valid token, permissions the operator forgot to map"
    /// are distinguishable in a log even though both answer `401`.
    #[error("the access token carries no recognised scope")]
    Unscoped,
}

/// What a deployment decides its own OAuth scope strings mean.
///
/// Not in `domain/`, and deliberately not a hardcoded table: the vendored spec
/// names no scope strings, so any mapping written here would be this crate
/// inventing a vocabulary and presenting it as policy. A deployment supplies the
/// vocabulary it actually issues, and the default maps nothing — which means an
/// unconfigured deployment refuses every token rather than granting one a scope
/// nobody chose.
pub trait ScopePolicy: Send + Sync {
    /// The scope one authorization-server string confers, if any.
    fn resolve(&self, scope: &str) -> Option<Scope>;

    /// The principal name to record, given the token's subject and every scope
    /// it carries.
    ///
    /// Separate from [`Self::resolve`] because the two are separate decisions:
    /// which scopes a token has is the authorization server's statement, while
    /// what to call the principal is entirely local. A deployment that
    /// authenticates every user as `"user"` is not contradicting the token.
    fn principal(&self, subject: &str, scopes: &[Arc<str>]) -> String {
        let _ = scopes;
        subject.to_owned()
    }
}

/// The fail-secure default: no scope string means anything.
///
/// Every token is refused with [`Refusal::Unscoped`] until a deployment supplies
/// a real policy. That is the whole design — a missing configuration must deny,
/// and it must deny *loudly* with a reason that says it was a configuration
/// rather than a forged token.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoScopes;

impl ScopePolicy for NoScopes {
    fn resolve(&self, _scope: &str) -> Option<Scope> {
        None
    }
}

/// What the request is allowed to be, given the resource it reached and the
/// revision it negotiated.
///
/// Two deployment facts and one spec fact, all of which change whether a token
/// is checked at all. Bundled because passing three arguments to every call site
/// invites two of them being forgotten.
#[derive(Debug, Clone)]
pub struct TokenPolicy<'a> {
    /// The resource identifier a token must be bound to.
    resource: &'a ResourceUri,

    /// The revision's authorization model, from
    /// [`authorization_model`](crate::domain::authorization_model).
    model: AuthorizationModel,

    /// The one issuer this server accepts, if it has been configured with one.
    expected_issuer: Option<Arc<str>>,
}

impl<'a> TokenPolicy<'a> {
    /// A policy that accepts any issuer.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::domain::{ProtocolVersion, authorization_model};
    /// use protocol_droid::domain::mcp::ResourceUri;
    /// use protocol_droid::application::oauth::TokenPolicy;
    ///
    /// let resource = ResourceUri::new("https://example.com/mcp")?;
    /// let policy = TokenPolicy::new(&resource, authorization_model(ProtocolVersion::V2026_07_28));
    /// assert!(policy.accepts_any_issuer());
    /// # Ok::<(), protocol_droid::domain::mcp::ResourceUriError>(())
    /// ```
    #[must_use]
    pub const fn new(resource: &'a ResourceUri, model: AuthorizationModel) -> Self {
        Self {
            resource,
            model,
            expected_issuer: None,
        }
    }

    /// Requires tokens to name `issuer`, refusing any minted by anyone else.
    #[must_use]
    pub fn with_expected_issuer(mut self, issuer: impl Into<Arc<str>>) -> Self {
        self.expected_issuer = Some(issuer.into());
        self
    }

    /// Whether an issuer check will be made.
    #[must_use]
    pub const fn accepts_any_issuer(&self) -> bool {
        self.expected_issuer.is_none()
    }
}

/// Turns claims an adapter authenticated into an [`AuthContext`], or refuses.
///
/// # Errors
///
/// The [`Refusal`] naming the first rule `claims` fails. Order is deliberate:
/// time is checked before audience so a stale token does not report a mismatch,
/// and audience before issuer because the audience is the binding that keeps a
/// token from crossing between servers, which is the one this crate must not get
/// wrong.
///
/// # Examples
///
/// ```
/// use std::time::{Duration, SystemTime};
///
/// use protocol_droid::application::oauth::{ScopePolicy, Scope, TokenClaims, TokenPolicy, verify};
/// use protocol_droid::domain::mcp::ResourceUri;
/// use protocol_droid::domain::{ProtocolVersion, ScopeSet, authorization_model};
///
/// struct Permissive;
/// impl ScopePolicy for Permissive {
///     fn resolve(&self, scope: &str) -> Option<Scope> {
///         match scope {
///             "mcp:read" => Some(Scope::Read),
///             _ => None,
///         }
///     }
/// }
///
/// let resource = ResourceUri::new("https://example.com/mcp")?;
/// let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
/// let policy = TokenPolicy::new(&resource, authorization_model(ProtocolVersion::V2026_07_28));
///
/// let good = TokenClaims::new()
///     .with_subject("alice")
///     .with_audience(resource.clone())
///     .with_expiry(now + Duration::from_secs(60))
///     .with_scope("mcp:read");
///
/// let context = verify(good, &policy, &Permissive, now)?;
/// assert_eq!(context.principal(), "alice");
/// assert!(context.holds(ScopeSet::empty().with(Scope::Read)));
///
/// // A token minted for a different server is refused.
/// let elsewhere = TokenClaims::new()
///     .with_subject("alice")
///     .with_audience(ResourceUri::new("https://evil.example/mcp")?)
///     .with_expiry(now + Duration::from_secs(60));
/// assert!(verify(elsewhere, &policy, &Permissive, now).is_err());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn verify(
    claims: TokenClaims,
    policy: &TokenPolicy<'_>,
    scopes: &dyn ScopePolicy,
    now: SystemTime,
) -> Result<AuthContext, Refusal> {
    // A revision with no authorization framework has no audience concept to
    // bind against, so there is no correct way to accept a token for it. Refuse
    // rather than accept one that cannot be checked.
    if !policy.model.audience_bound() {
        return Err(Refusal::AudienceMismatch);
    }

    if claims.expires_at.is_some_and(|expiry| expiry <= now) {
        return Err(Refusal::Expired);
    }
    if claims.not_before.is_some_and(|start| start > now) {
        return Err(Refusal::NotYetValid);
    }
    if !claims.is_audience(policy.resource) {
        return Err(Refusal::AudienceMismatch);
    }
    if let Some(expected) = &policy.expected_issuer
        && claims
            .issuer()
            .is_some_and(|issuer| issuer != expected.as_ref())
    {
        return Err(Refusal::IssuerMismatch);
    }

    let subject = claims.subject().ok_or(Refusal::NoSubject)?;

    // Union, not intersection: RFC 8693-style tokens can carry several scope
    // strings and a deployment may map more than one onto the same `Scope`.
    let mut resolved = ScopeSet::empty();
    for scope in claims.scopes() {
        if let Some(scope) = scopes.resolve(scope) {
            resolved = resolved.with(scope);
        }
    }
    if resolved.is_empty() {
        return Err(Refusal::Unscoped);
    }

    Ok(AuthContext::new(
        scopes.principal(subject, &claims.scopes),
        resolved,
    ))
}

#[cfg(test)]
#[path = "token/tests.rs"]
mod tests;
