//! The [`Authenticator`] that turns a bearer token into an identity.
//!
//! # What this is for
//!
//! The router's port asks one question — what may this request act as — and
//! until now the only implementations were a stand-in that knew one hardcoded
//! token and the [`Anonymous`](super::Anonymous) default that knows none. This
//! is the real one: it strips `Bearer`, asks a [`TokenSource`] what the token
//! says, applies the version's authorization model, and hands back a context.
//!
//! # Everything unusable becomes anonymous, and that is the point
//!
//! A request with no header, a request with a non-bearer scheme, a request
//! whose token the authorization server refuses, and a request whose token is
//! perfectly valid but names the wrong audience all arrive at the same place:
//! [`AuthContext::anonymous`]. They are not collapsed because they are
//! equivalent — they are collapsed because telling them apart would be an
//! oracle. RFC 6750 §3.1 defines `invalid_token` with an `error_description`
//! precisely so a client can react, and a server that obliged would hand anyone
//! with a token a way to learn which keys the authorization server has and
//! whether a given token is merely expired. The client loses a little
//! diagnosability; the attacker loses a probe. That is the trade, and it is
//! made in the port's signature rather than left to each implementor.
//!
//! What the caller gets instead is [`AuthPolicy`](super::AuthPolicy) reading
//! the resulting context: anonymous under a [`Require`](super::AuthPolicy) is
//! a `401`, which is RFC 6750's `invalid_token` status, and a context that
//! holds a principal but not the scope is a `403`, which is
//! `insufficient_scope`. The distinction that RFC 6750 actually cares about —
//! no credential, or an insufficient one — survives. Only the reason does not.
//!
//! # The resource identifier comes from the metadata document
//!
//! The audience a token must carry and the `resource_metadata` URL in the
//! challenge are the *same* fact about the server, so this takes one
//! [`ResourceMetadata`] rather than a resource and a metadata URL separately.
//! Configuring them twice is how a deployment ends up checking tokens against
//! one audience while telling clients to fetch metadata for another, which
//! fails only in production and only for clients that follow the link.
//!
//! # Nothing here logs
//!
//! The token is borrowed for the length of one call and never stored, cloned
//! into a message, or formatted. Every error from [`authenticate`] is discarded
//! into [`AuthContext::anonymous`], which is what makes that easy to keep: there
//! is no error value left anywhere for a log line to pick up.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::SystemTime;

use super::{AuthContext, Authenticator, Credentials};
use crate::application::oauth::{ScopePolicy, TokenPolicy, TokenSource, authenticate};
use crate::domain::ProtocolVersion;
use crate::domain::authorization_model;
use crate::domain::mcp::ResourceMetadata;

/// The wall clock `exp` and `nbf` are compared against.
///
/// Injected rather than called from [`SystemTime::now`] because the domain
/// rule is that time is a parameter, and a test that cannot say what "now" is
/// cannot pin an expiry boundary. Narrow, owned by the consumer, and one
/// method, which is what the port rule asks for.
pub trait WallClock: Send + Sync {
    /// The current instant.
    fn now(&self) -> SystemTime;
}

/// Reads the real clock.
///
/// The only implementation a deployment should use. Every other one is a test.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl WallClock for SystemClock {
    /// The current wall-clock instant.
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// The bearer credential a request presented, if it presented one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Presented<'a> {
    /// A well-formed `Bearer <token>` per RFC 6750 §2.1.
    Bearer(&'a str),
    /// No `Authorization` header, or one that is not a single bearer token.
    Nothing,
}

/// Reads the bearer token out of an `Authorization` header value.
///
/// RFC 6750 §2.1: the scheme is case-insensitive (RFC 7235 §2.1) and separated
/// from the token by whitespace. The token itself is a `b64token`, which cannot
/// contain a space, so splitting at the *first* space is correct and taking the
/// remainder whole is safe — there is no second token hiding in it to
/// misinterpret, and a header carrying one is a malformed request rather than a
/// stronger credential.
///
/// A scheme this does not recognise is [`Presented::Nothing`] rather than an
/// error, because it is not a bearer token this server can use: a client
/// authenticating with `Basic` has presented nothing that answers the question.
fn bearer(header: Option<&str>) -> Presented<'_> {
    let Some(header) = header else {
        return Presented::Nothing;
    };
    let Some((scheme, token)) = header.split_once(char::is_whitespace) else {
        return Presented::Nothing;
    };
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return Presented::Nothing;
    }
    let token = token.trim();
    if token.is_empty() {
        return Presented::Nothing;
    }
    Presented::Bearer(token)
}

/// Authenticates bearer tokens against a [`TokenSource`].
///
/// Generic over the source rather than holding a `dyn`, following
/// [`TokenSource`]'s own rule: the call site takes `impl TokenSource`, so this
/// monomorphises and the per-request cost is a direct call rather than a boxed
/// future behind a vtable. The scopes and the clock are `dyn` because a
/// deployment supplies them by value at construction and they are one call each.
pub struct OAuthAuthenticator<A> {
    /// What establishes authenticity.
    source: A,
    /// The resource identifier a token must be bound to, and the document a
    /// challenge points at.
    metadata: Arc<ResourceMetadata>,
    /// The revision this server negotiated, which decides what the model
    /// requires.
    version: ProtocolVersion,
    /// Whether tokens are required to name this server as their issuer.
    expected_issuer: Option<Arc<str>>,
    /// The scope vocabulary this deployment recognises.
    scopes: Arc<dyn ScopePolicy>,
    /// What "now" means.
    clock: Arc<dyn WallClock>,
}

impl<A> std::fmt::Debug for OAuthAuthenticator<A> {
    /// Prints the configuration only.
    ///
    /// Written by hand rather than derived, and not for the usual reason of a
    /// field that is awkward to format: the derive would demand
    /// `A: Debug` on the source — which for a JWT source would mean formatting
    /// its key set — and `dyn Debug` on two fields that are behaviour, not
    /// state. What is left is what an operator actually needs when a `401`
    /// shows up: which resource is protected, under which revision's model,
    /// and whether an issuer was pinned. The expected issuer is printed;
    /// nothing else on this type is secret, and no token ever reaches it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthAuthenticator")
            .field("resource", self.metadata.resource())
            .field("version", &self.version)
            .field("expected_issuer", &self.expected_issuer)
            .finish_non_exhaustive()
    }
}

impl<A> OAuthAuthenticator<A> {
    /// An authenticator for `source`, protecting `metadata`'s resource under
    /// the model `version` selects.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::adapters::primary::transport::http::{
    ///     OAuthAuthenticator, SystemClock,
    /// };
    /// use protocol_droid::application::oauth::NoScopes;
    /// use protocol_droid::domain::{ProtocolVersion, mcp::{ResourceMetadata, ResourceUri}};
    ///
    /// let metadata = ResourceMetadata::new(ResourceUri::new("https://example.com/mcp")?);
    /// let authenticator = OAuthAuthenticator::new(
    ///     (),                 // a real deployment passes a `TokenSource`
    ///     metadata,
    ///     ProtocolVersion::V2025_06_18,
    ///     NoScopes,
    ///     SystemClock,
    /// );
    /// # let _ = authenticator;
    /// # Ok::<(), protocol_droid::domain::mcp::ResourceUriError>(())
    /// ```
    #[must_use]
    pub fn new(
        source: A,
        metadata: ResourceMetadata,
        version: ProtocolVersion,
        scopes: impl ScopePolicy + 'static,
        clock: impl WallClock + 'static,
    ) -> Self {
        Self {
            source,
            metadata: Arc::new(metadata),
            version,
            expected_issuer: None,
            scopes: Arc::new(scopes),
            clock: Arc::new(clock),
        }
    }

    /// Requires tokens to name `issuer`, rather than accepting any.
    ///
    /// Off by default because RFC 9728 publishes a *list* of authorization
    /// servers this resource trusts, so a deployment fronted by more than one
    /// legitimately accepts more than one issuer. Turning it on is how a
    /// single-issuer deployment says so.
    #[must_use]
    pub fn with_expected_issuer(mut self, issuer: impl Into<Arc<str>>) -> Self {
        self.expected_issuer = Some(issuer.into());
        self
    }

    /// The resource this authenticator protects.
    #[must_use]
    pub fn metadata(&self) -> &ResourceMetadata {
        &self.metadata
    }

    /// The policy this authenticator applies, built fresh for one call.
    ///
    /// Rebuilt per request rather than stored because [`TokenPolicy`] borrows
    /// the resource and holds an `Option<Arc<str>>`, so a stored one would pin
    /// a lifetime onto the struct for the sake of two field copies.
    fn policy(&self) -> TokenPolicy<'_> {
        let mut policy =
            TokenPolicy::new(self.metadata.resource(), authorization_model(self.version));
        if let Some(issuer) = &self.expected_issuer {
            policy = policy.with_expected_issuer(Arc::clone(issuer));
        }
        policy
    }
}

impl<A> Authenticator for OAuthAuthenticator<A>
where
    A: TokenSource,
{
    fn authenticate<'a>(
        &'a self,
        presented: &'a Credentials<'_>,
    ) -> Pin<Box<dyn Future<Output = AuthContext> + Send + 'a>> {
        let token = match bearer(presented.authorization()) {
            Presented::Bearer(token) => token,
            Presented::Nothing => {
                return Box::pin(std::future::ready(AuthContext::anonymous()));
            }
        };
        let source = &self.source;
        let scopes = Arc::clone(&self.scopes);
        let policy = self.policy();
        let now = self.clock.now();

        Box::pin(async move {
            // Every failure collapses to the same answer, which is the module's
            // whole argument. `SourceError` is deliberately not examined:
            // distinguishing "the authorization server is unreachable" from "the
            // token was refused" would hand a caller a liveness probe for the
            // authorization server, and telling a bad token from a good one
            // would hand them a probe for its keys.
            match authenticate(source, &policy, scopes.as_ref(), token, now).await {
                Ok(context) => context,
                Err(_refusal) => AuthContext::anonymous(),
            }
        })
    }
}

#[cfg(test)]
#[path = "oauth/tests.rs"]
mod tests;
