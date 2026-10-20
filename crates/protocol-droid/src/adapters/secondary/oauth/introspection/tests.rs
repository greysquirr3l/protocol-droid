//! What an introspection response is allowed to say, and what it is not.
//!
//! Every test parses a literal response rather than standing up a server,
//! because the rules worth testing are in how a response is *read* — and a
//! round trip would only add a place for them to hide.

use std::future::{Future, ready};
use std::time::{Duration, UNIX_EPOCH};

use super::{Audience, IntrospectionResponse, IntrospectionSource, claims_from, system_time};
use crate::application::oauth::{
    Refusal, ScopePolicy, SourceError, TokenClaims, TokenPolicy, TokenSource, authenticate,
};
use crate::domain::mcp::{ResourceUri, ResourceUriError};
use crate::domain::registry::{Scope, ScopeSet};
use crate::domain::{ProtocolVersion, authorization_model};

/// The error types these tests answer, collected so `?` works and a failure
/// prints the engine's own account of what it refused.
#[derive(Debug, thiserror::Error)]
enum Failure {
    /// A resource identifier did not parse.
    #[error(transparent)]
    Uri(#[from] ResourceUriError),
    /// A token was refused, or could not be established.
    #[error(transparent)]
    Source(#[from] SourceError),
}

/// Parses a literal the way the HTTP path does, so a test and the adapter cannot
/// drift apart on how a field arrives.
fn parse(body: &str) -> Result<IntrospectionResponse, serde_json::Error> {
    serde_json::from_str(body)
}

/// Parses and interprets, which is what a request actually does.
///
/// The two failure kinds stay separate rather than folded into one error type:
/// a body that is not JSON and a token the server called inactive are not the
/// same event, and a log line that cannot tell them apart is one nobody can act
/// on.
fn interpret(body: &str) -> Result<TokenClaims, SourceError> {
    match parse(body) {
        Ok(response) => claims_from(response),
        Err(failure) => Err(SourceError::Unavailable(failure.to_string())),
    }
}

#[test]
fn an_active_token_becomes_claims() -> Result<(), Failure> {
    let claims = interpret(
        r#"{"active":true,"sub":"alice","aud":"https://example.com/mcp",
            "scope":"mcp:read mcp:write","exp":2000000000,"iss":"https://auth.example.com"}"#,
    )?;

    assert_eq!(claims.subject(), Some("alice"));
    assert_eq!(claims.issuer(), Some("https://auth.example.com"));
    assert_eq!(claims.scopes().len(), 2);
    assert_eq!(claims.audience().len(), 1);
    assert_eq!(
        claims
            .audience()
            .first()
            .map(crate::application::oauth::Audience::as_str),
        Some("https://example.com/mcp"),
    );
    Ok(())
}

#[test]
fn an_inactive_token_is_refused_whatever_else_it_says() {
    // The case RFC 7662 §2.2 forbids a server from producing, and the one a
    // client must be built to survive: a response shaped for an implementation
    // that reads `aud` and `scope` before it reads `active`. Every field here is
    // one that would make the token look usable.
    let parsed = interpret(
        r#"{"active":false,"sub":"alice","aud":"https://example.com/mcp",
            "scope":"mcp:read","exp":2000000000}"#,
    );

    assert_eq!(parsed, Err(SourceError::Refused(Refusal::Unscoped)));
}

#[test]
fn a_response_that_never_says_whether_the_token_is_active_is_unavailable() {
    // Not a refusal: the token is not known to be bad, and telling the caller it
    // is sends it to re-authenticate against a server that is merely broken.
    for body in [
        r#"{"sub":"alice","aud":"https://example.com/mcp"}"#,
        r#"{"scope":"mcp:read"}"#,
        r"{}",
    ] {
        assert_eq!(
            interpret(body),
            Err(SourceError::Unavailable(
                "the introspection response has no `active` field".to_owned(),
            )),
            "{body}",
        );
    }
}

#[test]
fn scopes_are_space_delimited_not_comma_delimited() -> Result<(), Failure> {
    // RFC 6749 §3.3. Splitting on commas would turn two scopes the server never
    // issued into one that looks legitimate.
    let claims = interpret(r#"{"active":true,"scope":"mcp:read mcp:write"}"#)?;
    assert_eq!(claims.scopes(), &["mcp:read".into(), "mcp:write".into()]);

    let commas = interpret(r#"{"active":true,"scope":"mcp:read,mcp:write"}"#)?;
    assert_eq!(
        commas.scopes(),
        &["mcp:read,mcp:write".into()],
        "a comma is part of one scope, not a separator",
    );
    Ok(())
}

#[test]
fn repeated_and_trailing_spaces_do_not_invent_empty_scopes() -> Result<(), Failure> {
    let claims = interpret(r#"{"active":true,"scope":"  mcp:read   mcp:write  "}"#)?;
    assert_eq!(claims.scopes(), &["mcp:read".into(), "mcp:write".into()]);
    Ok(())
}

#[test]
fn an_audience_may_be_one_string_or_many() -> Result<(), Failure> {
    // RFC 7519 §4.1.3 allows either. Both have to land in the same place, or a
    // token issued for two servers works against one and not the other.
    let one = interpret(r#"{"active":true,"aud":"https://example.com/mcp"}"#)?;
    assert_eq!(one.audience().len(), 1);

    let many = interpret(
        r#"{"active":true,"aud":["https://example.com/mcp","https://other.example/mcp"]}"#,
    )?;
    assert_eq!(many.audience().len(), 2);
    Ok(())
}

#[test]
fn an_audience_that_is_not_an_absolute_uri_is_retained_rather_than_dropped() -> Result<(), Failure>
{
    // Retained, for the same reason as the JWT path: an `aud` that is not a URI
    // is still an identifier the authorization server minted, and a deployment
    // may have configured exactly it. See issue #46, and
    // `audience_that_is_not_a_uri_is_kept_rather_than_dropped` in the JWT tests
    // for the behaviour this replaced.
    let claims = interpret(r#"{"active":true,"aud":["not-a-uri","https://example.com/mcp"]}"#)?;

    assert_eq!(claims.audience().len(), 2);
    assert_eq!(
        claims
            .audience()
            .first()
            .map(crate::application::oauth::Audience::as_str),
        Some("not-a-uri"),
    );
    Ok(())
}

#[test]
fn username_is_the_subject_when_sub_is_absent() -> Result<(), Failure> {
    // RFC 7662 §2.2 lists both; `username` is the older spelling and some
    // servers send only that.
    let claims = interpret(r#"{"active":true,"username":"alice"}"#)?;
    assert_eq!(claims.subject(), Some("alice"));
    Ok(())
}

#[test]
fn sub_wins_over_username() -> Result<(), Failure> {
    let claims = interpret(r#"{"active":true,"username":"old","sub":"alice"}"#)?;
    assert_eq!(claims.subject(), Some("alice"));
    Ok(())
}

#[test]
fn an_active_token_may_omit_every_optional_field() -> Result<(), Failure> {
    // RFC 7662 §2.2 permits it. Whether that is *enough* is `verify`'s
    // question, not this stage's.
    let claims = interpret(r#"{"active":true}"#)?;
    assert_eq!(claims.subject(), None);
    assert!(claims.scopes().is_empty());
    Ok(())
}

#[test]
fn an_absent_expiry_leaves_the_token_unbounded() -> Result<(), Failure> {
    // Not this module's call to make up. `verify` has no expiry either, so an
    // authorization server that omits `exp` issued a token that does not expire,
    // and inventing a limit here would be a policy this crate does not own.
    let _ = interpret(r#"{"active":true,"sub":"alice"}"#)?;
    Ok(())
}

#[test]
fn unix_seconds_become_system_times() {
    assert!(system_time(0).is_some_and(|at| {
        at.duration_since(UNIX_EPOCH)
            .is_ok_and(|d| d.as_secs() == 0)
    }),);
    assert!(system_time(1_700_000_000).is_some());
    // `u64::MAX` seconds is beyond any representable duration, and must not wrap
    // into a time near the epoch — which would silently un-expire a token.
    assert!(system_time(u64::MAX).is_none());
}

#[test]
fn an_out_of_range_expiry_is_dropped_rather_than_wrapping() -> Result<(), Failure> {
    let _ = interpret(r#"{"active":true,"sub":"alice","exp":18446744073709551615}"#)?;
    Ok(())
}

#[test]
fn a_malformed_body_is_unavailable_and_says_so() {
    // A 200 with a body that is not an object is a malfunctioning server, not a
    // bad token.
    for body in [r"[]", r#""active""#, r#"{"active":"yes"}"#, "not json"] {
        assert!(
            matches!(interpret(body), Err(SourceError::Unavailable(_))),
            "{body} should be unavailable",
        );
    }
}

#[test]
fn the_wire_shapes_are_what_this_module_reads() -> Result<(), Failure> {
    // Pins the two shapes the parse depends on: `active` optional-but-honoured,
    // and `aud` untagged. If either changes, the tests above change meaning.
    let one = parse(r#"{"active":true,"aud":"https://example.com"}"#)
        .map_err(|failure| SourceError::Unavailable(failure.to_string()))?;
    assert_eq!(one.active, Some(true));
    assert!(matches!(one.aud, Some(Audience::One(_))));

    let many = parse(r#"{"active":false,"aud":["a"]}"#)
        .map_err(|failure| SourceError::Unavailable(failure.to_string()))?;
    assert!(matches!(many.aud, Some(Audience::Many(_))));
    Ok(())
}

/// A source that already knows what a token says.
///
/// Stands in for the HTTP one so the seam — `TokenSource`, `authenticate`, the
/// policy — is exercised without a socket. What the HTTP path adds is the round
/// trip, not the composition. Returning `ready` rather than an `async` block
/// says what it is: the answer is already in hand.
struct Fixed(Result<TokenClaims, SourceError>);

impl TokenSource for Fixed {
    fn claims(
        &self,
        _presented: &str,
    ) -> impl Future<Output = Result<TokenClaims, SourceError>> + Send {
        ready(self.0.clone())
    }
}

/// Maps `mcp:read` to [`Scope::Read`] and nothing else.
struct OneScope;

impl ScopePolicy for OneScope {
    fn resolve(&self, scope: &str) -> Option<Scope> {
        match scope {
            "mcp:read" => Some(Scope::Read),
            _ => None,
        }
    }
}

/// A token built from scratch, so a test cannot accidentally add a wrong field
/// to a token that is already valid.
///
/// The builder appends rather than replaces, so growing a valid token until it
/// fails verifies nothing — it produces a token holding *both* audiences, which
/// passes. Each case here starts empty.
fn claims_for(resource: &str, subject: &str, scope: &str) -> Result<TokenClaims, ResourceUriError> {
    Ok(TokenClaims::new()
        .with_subject(subject)
        .with_audience(ResourceUri::new(resource)?)
        // One year past the unix epoch, so the expiry is not the thing
        // under test. `u64::MAX / 2` seconds would be ~146M years past
        // epoch — past the Windows FILETIME ceiling of 30828 AD and
        // overflowing `SystemTime + Duration` on that target. A year is
        // far enough out that no test in this file races the expiry.
        .with_expiry(UNIX_EPOCH + Duration::from_hours(8_760))
        .with_scope(scope))
}

#[tokio::test]
async fn a_source_feeds_authenticate_end_to_end() -> Result<(), Failure> {
    // The composition the port exists for: claims from an adapter, authorization
    // from this crate, one context out. Reaching it through `authenticate` rather
    // than calling `verify` directly is what proves there is no step between
    // them that can be skipped.
    let resource = ResourceUri::new("https://example.com/mcp")?;
    let policy = TokenPolicy::new(&resource, authorization_model(ProtocolVersion::V2026_07_28));
    let source = Fixed(Ok(claims_for(resource.as_str(), "alice", "mcp:read")?));
    let now = UNIX_EPOCH + Duration::from_secs(1_000);

    let context = authenticate(&source, &policy, &OneScope, "a-token", now).await?;
    assert_eq!(context.principal(), "alice");
    assert!(context.holds(ScopeSet::empty().with(Scope::Read)));
    Ok(())
}

#[tokio::test]
async fn a_refusal_from_the_source_is_not_swallowed_by_the_policy() -> Result<(), Failure> {
    // The two kinds stay distinct across the boundary: a source that refused
    // stays a refusal, rather than arriving as a policy failure that reads like
    // this crate's own rules rejected it.
    let resource = ResourceUri::new("https://example.com/mcp")?;
    let policy = TokenPolicy::new(&resource, authorization_model(ProtocolVersion::V2026_07_28));
    let source = Fixed(Err(SourceError::Refused(Refusal::Expired)));
    let now = UNIX_EPOCH + Duration::from_secs(1_000);

    assert_eq!(
        authenticate(&source, &policy, &OneScope, "a-token", now).await,
        Err(SourceError::Refused(Refusal::Expired)),
    );
    Ok(())
}

#[tokio::test]
async fn an_unreachable_server_is_not_a_refusal() -> Result<(), Failure> {
    // The distinction a caller cannot otherwise recover from: being told your
    // token is bad sends you to re-authenticate, and being told the server is
    // unreachable sends you to wait.
    let resource = ResourceUri::new("https://example.com/mcp")?;
    let policy = TokenPolicy::new(&resource, authorization_model(ProtocolVersion::V2026_07_28));
    let source = Fixed(Err(SourceError::Unavailable(
        "connection refused".to_owned(),
    )));
    let now = UNIX_EPOCH + Duration::from_secs(1_000);

    assert_eq!(
        authenticate(&source, &policy, &OneScope, "a-token", now).await,
        Err(SourceError::Unavailable("connection refused".to_owned())),
    );
    Ok(())
}

#[test]
fn constructing_a_source_opens_nothing_and_remembers_its_endpoint() {
    // A deployment discovers the introspection endpoint from a metadata
    // document, so a typo in it has to be visible without a request — and
    // constructing one must not reach the network.
    let source = IntrospectionSource::new(
        "https://auth.example.com/introspect",
        "client-id",
        "client-secret",
    );
    assert_eq!(source.endpoint(), "https://auth.example.com/introspect");

    let replaced = source.with_http_client(reqwest::Client::new());
    assert_eq!(
        replaced.endpoint(),
        "https://auth.example.com/introspect",
        "swapping the client must not lose the endpoint",
    );
}
