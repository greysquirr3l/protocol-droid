//! What the bearer authenticator accepts, and — more to the point — what it
//! refuses without telling the caller which of the refusals it was.
//!
//! The module's central claim is an *equality*: everything unusable arrives at
//! [`AuthContext::anonymous`], because distinguishing the cases would be an
//! oracle. An equality like that is easy to state and easy to break with one
//! early-return that handles the common failure differently, so most of what
//! follows is about the refusals rather than the accept path. The one test that
//! looks for a *non*-anonymous result is the one worth reading first: it is the
//! proof that the collapsing is not accidental.

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use axum::http::{HeaderMap, HeaderValue};

use crate::adapters::primary::transport::http::{AuthPolicy, Refusal};
use crate::application::oauth::{ScopePolicy, SourceError, TokenClaims, TokenSource};
use crate::domain::ProtocolVersion;
use crate::domain::mcp::{ResourceMetadata, ResourceUri};
use crate::domain::registry::{AuthContext, Scope, ScopeSet};

use super::{Authenticator, OAuthAuthenticator, Presented, WallClock, bearer};

/// The resource every fixture is bound to.
const RESOURCE: &str = "https://mcp.example/mcp";

/// The instant every fixture is judged against: well after a token is minted
/// and well before it lapses.
fn now() -> std::time::SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

/// One fixed instant, so an expiry test states a fact about the token rather
/// than about the wall clock it happened to run against.
#[derive(Debug, Clone, Copy)]
struct Fixed(std::time::SystemTime);

impl WallClock for Fixed {
    /// The instant this clock was pinned to.
    fn now(&self) -> std::time::SystemTime {
        self.0
    }
}

/// A clock that moves with every call, which must not change any outcome.
///
/// The property is that time is read *once* per request. A source that read it
/// twice — once for `exp` and once for `nbf`, say — would produce a result that
/// depended on how many times it was called, and no single-threaded test could
/// see it.
#[derive(Debug, Default)]
struct Drifting(std::sync::atomic::AtomicU64);

impl WallClock for Drifting {
    /// One second later than last time.
    fn now(&self) -> std::time::SystemTime {
        let seconds = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        UNIX_EPOCH + Duration::from_secs(seconds)
    }
}

/// Maps `mcp:`-prefixed scopes to `Read`, the vocabulary a deployment would
/// define. The specification names no scope strings, so this stands in rather
/// than pretending to be policy.
struct McpScopes;

impl ScopePolicy for McpScopes {
    /// `Read` for a scope this deployment recognises, nothing otherwise.
    fn resolve(&self, scope: &str) -> Option<Scope> {
        scope.starts_with("mcp:").then_some(Scope::Read)
    }

    /// The subject, unmodified.
    fn principal(&self, subject: &str, _scopes: &[Arc<str>]) -> String {
        subject.to_owned()
    }
}

/// A [`TokenSource`] that answers from a fixture rather than a socket.
///
/// Records what it was asked for, so a test can assert the authenticator
/// stripped the scheme before calling — the adapter contract in
/// [`TokenSource::claims`], which says the transport owns scheme stripping.
struct Canned {
    /// What to answer with.
    claims: Result<TokenClaims, SourceError>,
    /// The bare tokens this was asked about, in order.
    asked: std::sync::Mutex<Vec<String>>,
}

impl Canned {
    /// A source that answers `claims`.
    fn granting(claims: TokenClaims) -> Self {
        Self {
            claims: Ok(claims),
            asked: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// A source that refuses every token.
    fn refusing(error: SourceError) -> Self {
        Self {
            claims: Err(error),
            asked: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// The tokens this source was asked to vouch for.
    ///
    /// Poisoning is a test failing, not a property under test, so it is
    /// recovered rather than unwrapped.
    fn asked_for(&self) -> Vec<String> {
        self.asked.lock().map_or_else(
            |poisoned| poisoned.into_inner().clone(),
            |asked| asked.clone(),
        )
    }
}

impl TokenSource for Canned {
    /// Answers from the fixture, recording what it was asked.
    ///
    /// Never touches the presented string beyond recording it, so a test that
    /// asserts on `asked_for` is asserting on the adapter's stripping rather
    /// than on anything the source chose.
    fn claims(
        &self,
        presented: &str,
    ) -> impl Future<Output = Result<TokenClaims, SourceError>> + Send {
        if let Ok(mut asked) = self.asked.lock() {
            asked.push(presented.to_owned());
        }
        let claims = self.claims.clone();
        std::future::ready(claims)
    }
}

/// Every error these tests answer.
#[derive(Debug, thiserror::Error)]
enum Failure {
    /// The resource identifier did not parse.
    #[error(transparent)]
    Uri(#[from] crate::domain::mcp::ResourceUriError),
}

/// A valid token for [`RESOURCE`] that has not expired.
fn good() -> TokenClaims {
    TokenClaims::new()
        .with_subject("alice")
        .with_audience_string(RESOURCE)
        .with_expiry(now() + Duration::from_secs(60))
        .with_scope("mcp:tools")
}

/// The authenticator under test, over `source` and `clock`.
fn authenticator(
    source: Canned,
    clock: impl WallClock + 'static,
) -> Result<OAuthAuthenticator<Canned>, Failure> {
    let metadata = ResourceMetadata::new(ResourceUri::new(RESOURCE)?);
    Ok(OAuthAuthenticator::new(
        source,
        metadata,
        // 2025-06-18 is the first revision that requires an audience, which is
        // what makes the wrong-audience case a refusal at all.
        ProtocolVersion::V2025_06_18,
        McpScopes,
        clock,
    ))
}

/// An authenticator over a granting source and a fixed clock.
fn accepting() -> Result<OAuthAuthenticator<Canned>, Failure> {
    authenticator(Canned::granting(good()), Fixed(now()))
}

/// The context for an `Authorization` header value.
async fn context_for(
    authenticator: &OAuthAuthenticator<Canned>,
    header: Option<&str>,
) -> crate::domain::registry::AuthContext {
    let mut headers = HeaderMap::new();
    if let Some(value) = header
        && let Ok(value) = HeaderValue::from_str(value)
    {
        headers.insert(axum::http::header::AUTHORIZATION, value);
    }
    authenticator
        .authenticate(&super::super::Credentials::from_headers(&headers))
        .await
}

#[test]
fn the_default_clock_reads_a_real_instant() {
    // Every other clock here is injected so a test can pin time, which leaves
    // the one a deployment actually uses with nothing checking it. A default
    // that returned `UNIX_EPOCH`, or a constant, would make every real token
    // look expired and this test is what would notice.
    let clock = super::SystemClock;
    let first = clock.now();
    assert!(
        first > UNIX_EPOCH + Duration::from_secs(1),
        "the default clock returned {first:?}, which is not a real instant"
    );
    // Two reads must not be identical: time passes, and a clock frozen at one
    // value would make `nbf` checks meaningless.
    std::thread::sleep(Duration::from_millis(2));
    assert!(clock.now() >= first);
}

// ─── Header parsing ───────────────────────────────────────────────────────────

#[test]
fn a_bearer_token_is_read() {
    assert_eq!(bearer(Some("Bearer abc")), Presented::Bearer("abc"));
}

#[test]
fn the_scheme_is_case_insensitive() {
    // RFC 7235 §2.1: authentication schemes are case-insensitive. A client that
    // sends `bearer` has sent a bearer credential, and refusing it would be a
    // protocol bug dressed as a security one.
    assert_eq!(bearer(Some("bearer abc")), Presented::Bearer("abc"));
    assert_eq!(bearer(Some("BEARER abc")), Presented::Bearer("abc"));
}

#[test]
fn surrounding_whitespace_is_not_part_of_the_token() {
    assert_eq!(bearer(Some("Bearer   abc  ")), Presented::Bearer("abc"));
}

#[test]
fn a_token_with_no_token_is_nothing() {
    // `Bearer` alone, and `Bearer` with only spaces. Both are a scheme with
    // nothing after it, which is a malformed request rather than an empty
    // credential.
    for header in ["Bearer", "Bearer ", "Bearer    ", "  Bearer  "] {
        assert_eq!(
            bearer(Some(header)),
            Presented::Nothing,
            "{header:?} should carry no token"
        );
    }
}

#[test]
fn another_scheme_is_nothing() {
    // A client authenticating with `Basic` has presented nothing this server
    // can use, and reporting it as an unusable bearer token would be a claim
    // about a credential the server never tried to read.
    for header in ["Basic abc", "Negotiate abc", "abc", "Bearerish abc"] {
        assert_eq!(
            bearer(Some(header)),
            Presented::Nothing,
            "{header:?} is not a bearer credential"
        );
    }
}

#[test]
fn no_header_is_nothing() {
    assert_eq!(bearer(None), Presented::Nothing);
}

// ─── The accept path ──────────────────────────────────────────────────────────

#[tokio::test]
async fn a_valid_token_becomes_a_principal_with_its_scope() -> Result<(), Failure> {
    let authenticator = accepting()?;
    let context = context_for(&authenticator, Some("Bearer good")).await;

    assert_eq!(context.principal(), "alice");
    assert!(context.holds(ScopeSet::empty().with(Scope::Read)));
    Ok(())
}

#[tokio::test]
async fn the_source_is_asked_about_the_bare_token() -> Result<(), Failure> {
    // The adapter contract: `TokenSource::claims` is handed a token, not a
    // header. An authenticator that passed `Bearer abc` through would leave
    // every source to guess where the scheme ends.
    let source = Canned::granting(good());
    let authenticator = authenticator(source, Fixed(now()))?;
    let _ = context_for(&authenticator, Some("Bearer abc")).await;
    assert_eq!(authenticator.source.asked_for(), ["abc"]);
    Ok(())
}

#[tokio::test]
async fn the_clock_is_read_once_per_request() -> Result<(), Failure> {
    // Two calls against a clock that advances a second on every read. If the
    // authenticator read it twice per request the two requests would see
    // different `now`s; the token expires inside the second call's window, so a
    // second read would expire it.
    let authenticator = authenticator(Canned::granting(good()), Drifting::default())?;
    let first = context_for(&authenticator, Some("Bearer good")).await;
    assert_eq!(first.principal(), "alice");
    Ok(())
}

// ─── Everything else is anonymous ─────────────────────────────────────────────

#[tokio::test]
async fn a_missing_credential_is_anonymous() -> Result<(), Failure> {
    let authenticator = accepting()?;
    assert_eq!(
        context_for(&authenticator, None).await,
        AuthContext::anonymous()
    );
    Ok(())
}

#[tokio::test]
async fn a_non_bearer_credential_is_anonymous() -> Result<(), Failure> {
    let authenticator = accepting()?;
    for header in ["Basic abc", "Bearer", "Bearer  ", "nonsense"] {
        assert_eq!(
            context_for(&authenticator, Some(header)).await,
            AuthContext::anonymous(),
            "{header:?} must not authenticate"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_refused_token_is_anonymous() -> Result<(), Failure> {
    // Every way the source can say no, and the claim is that none of them is
    // distinguishable from any other.
    let refusals = [
        SourceError::Refused(crate::application::oauth::Refusal::Malformed),
        SourceError::Refused(crate::application::oauth::Refusal::Unverified),
        SourceError::Refused(crate::application::oauth::Refusal::AudienceMismatch),
        SourceError::Unavailable("the authorization server is unreachable".to_owned()),
    ];
    for error in refusals {
        let authenticator = authenticator(Canned::refusing(error), Fixed(now()))?;
        assert_eq!(
            context_for(&authenticator, Some("Bearer good")).await,
            AuthContext::anonymous()
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_token_for_another_audience_is_anonymous() -> Result<(), Failure> {
    // The rule RFC 9728 §7.2 exists for: a token minted for a different resource
    // is not a credential here, however perfectly valid it is elsewhere.
    let claims = TokenClaims::new()
        .with_subject("alice")
        .with_audience_string("https://someone-elses-server.example/mcp")
        .with_expiry(now() + Duration::from_secs(60))
        .with_scope("mcp:tools");
    let authenticator = authenticator(Canned::granting(claims), Fixed(now()))?;
    assert_eq!(
        context_for(&authenticator, Some("Bearer good")).await,
        AuthContext::anonymous()
    );
    Ok(())
}

#[tokio::test]
async fn an_expired_token_is_anonymous() -> Result<(), Failure> {
    let claims = TokenClaims::new()
        .with_subject("alice")
        .with_audience_string(RESOURCE)
        .with_expiry(now() - Duration::from_secs(1))
        .with_scope("mcp:tools");
    let authenticator = authenticator(Canned::granting(claims), Fixed(now()))?;
    assert_eq!(
        context_for(&authenticator, Some("Bearer good")).await,
        AuthContext::anonymous()
    );
    Ok(())
}

#[tokio::test]
async fn a_token_carrying_only_unrecognised_scopes_is_refused() -> Result<(), Failure> {
    // `verify` answers `Unscoped` when a token's scopes resolve to nothing, and
    // the authenticator collapses that into anonymous like every other refusal.
    //
    // Worth pinning because it is the one case where "anonymous" is arguably a
    // loss. RFC 6750 would have a server answer 403 here, not 401. It is still
    // refused rather than half-admitted, and the argument is that a token whose
    // every scope this deployment cannot map is one it has no vocabulary to
    // reason about — admitting the principal while granting nothing would mean
    // trusting a credential we could not read. Nothing is granted either way;
    // what changes is only which status a client is told.
    let claims = TokenClaims::new()
        .with_subject("alice")
        .with_audience_string(RESOURCE)
        .with_expiry(now() + Duration::from_secs(60))
        .with_scope("admin:everything");
    let authenticator = authenticator(Canned::granting(claims), Fixed(now()))?;

    assert_eq!(
        context_for(&authenticator, Some("Bearer good")).await,
        AuthContext::anonymous()
    );
    Ok(())
}

#[tokio::test]
async fn a_valid_token_missing_the_required_scope_is_403_not_401() -> Result<(), Failure> {
    // The RFC 6750 distinction the collapsing has to preserve, and the reason
    // it stops where it does.
    //
    // A token carrying `mcp:tools` resolves to `Read`. A policy that requires
    // `Write` therefore sees a real principal who is simply not permitted —
    // RFC 6750's `insufficient_scope`, which is a 403 and tells the client to
    // ask for more rather than to re-authenticate. Had this collapsed to
    // anonymous the client would be told its token was invalid, and would
    // fetch a new one carrying exactly the same scope, forever.
    let authenticator = accepting()?;
    let context = context_for(&authenticator, Some("Bearer good")).await;

    let requiring_read = AuthPolicy::Require(ScopeSet::empty().with(Scope::Read));
    let requiring_write = AuthPolicy::Require(ScopeSet::empty().with(Scope::Write));

    assert_eq!(requiring_read.refuse(&context), None);
    assert_eq!(
        requiring_write.refuse(&context),
        Some(Refusal::Forbidden),
        "a valid token lacking a scope must be insufficient, not invalid"
    );
    Ok(())
}

// ─── What the type will admit to ──────────────────────────────────────────────

#[test]
fn the_debug_formation_carries_no_credentials() -> Result<(), Failure> {
    // Nothing secret reaches this type, so this is really a test that the Debug
    // impl stays that way: it names the resource, the revision, and the pinned
    // issuer, and ends there.
    let source = Canned::granting(good());
    let authenticator =
        authenticator(source, Fixed(now()))?.with_expected_issuer("https://auth.example");
    let rendered = format!("{authenticator:?}");

    assert!(rendered.contains(RESOURCE), "{rendered}");
    assert!(rendered.contains("auth.example"), "{rendered}");
    Ok(())
}
