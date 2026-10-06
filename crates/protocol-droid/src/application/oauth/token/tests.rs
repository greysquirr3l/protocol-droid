//! The rules a presented token has to pass, and each way it can fail.
//!
//! The task's three required cases are named tests here — wrong audience,
//! expired, valid — alongside the branches a reviewer would otherwise have to
//! take on trust.

use std::assert_matches;
use std::future::{Future, ready};
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::time::{Duration, SystemTime};

use super::{
    NoScopes, Refusal, ScopePolicy, SourceError, TokenClaims, TokenPolicy, TokenSource,
    authenticate, verify,
};
use crate::domain::mcp::{ResourceUri, ResourceUriError};
use crate::domain::registry::{Scope, ScopeSet};
use crate::domain::{ProtocolVersion, authorization_model};

/// The two unrelated error types these tests answer, collected so `?` works
/// and a failure prints the engine's own account of what it refused.
#[derive(Debug, thiserror::Error)]
enum Failure {
    /// A resource identifier did not parse.
    #[error(transparent)]
    Uri(#[from] ResourceUriError),
    /// A token was refused.
    #[error(transparent)]
    Refusal(#[from] Refusal),
    /// No claims could be obtained at all — the seam tests' subject.
    #[error(transparent)]
    Source(#[from] SourceError),
}

/// The resource these tests are about.
const RESOURCE: &str = "https://example.com/mcp";
/// A second server, which a token minted for it must not be usable here.
const OTHER: &str = "https://elsewhere.example/mcp";
/// A sibling path on our own host.
const SIBLING: &str = "https://example.com/other";

/// A fixed instant, so every time comparison in this file is deterministic.
const NOW_SECS: u64 = 1_000;
const FUTURE_SECS: u64 = NOW_SECS + 60;

fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS)
}

fn in_future() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(FUTURE_SECS)
}

/// A deployment whose vocabulary this crate does not know: only the two scopes
/// below mean anything. The names are the test's, not the specification's —
/// the vendored spec defines no scope strings.
struct TwoScopes;

impl ScopePolicy for TwoScopes {
    fn resolve(&self, scope: &str) -> Option<Scope> {
        match scope {
            "mcp:read" => Some(Scope::Read),
            "mcp:write" => Some(Scope::Write),
            _ => None,
        }
    }
}

/// A token that passes everything, for the individual tests to break one field
/// of at a time.
fn valid_claims() -> Result<TokenClaims, ResourceUriError> {
    claims_for(RESOURCE, &["mcp:read"])
}

/// A token for exactly `audience` carrying exactly `scopes`.
///
/// Built from scratch rather than from [`valid_claims`] on purpose:
/// `with_audience` *appends*, so starting from a valid token and adding a wrong
/// audience leaves a token that legitimately contains both — which verifies,
/// and tests nothing about refusal.
fn claims_for(audience: &str, scopes: &[&str]) -> Result<TokenClaims, ResourceUriError> {
    let mut claims = TokenClaims::new()
        .with_subject("alice")
        .with_audience(ResourceUri::new(audience)?)
        .with_expiry(in_future());
    for scope in scopes {
        claims = claims.with_scope(*scope);
    }
    Ok(claims)
}

#[test]
fn a_valid_token_yields_a_context_with_its_scopes() -> Result<(), Failure> {
    let resource = ResourceUri::new(RESOURCE)?;
    let policy = TokenPolicy::new(&resource, authorization_model(ProtocolVersion::V2026_07_28));

    let context = verify(&valid_claims()?, &policy, &TwoScopes, now())?;

    assert_eq!(context.principal(), "alice");
    assert!(context.holds(ScopeSet::empty().with(Scope::Read)));
    Ok(())
}

#[test]
fn a_wrong_audience_is_refused() -> Result<(), Failure> {
    // The task's first required case: signed, unexpired, names a subject — and
    // is only for a different server.
    let resource = ResourceUri::new(RESOURCE)?;
    let policy = TokenPolicy::new(&resource, authorization_model(ProtocolVersion::V2026_07_28));
    let claims = TokenClaims::new()
        .with_subject("alice")
        .with_audience(ResourceUri::new(OTHER)?)
        .with_expiry(in_future())
        .with_scope("mcp:read");

    assert_eq!(
        verify(&claims, &policy, &TwoScopes, now()),
        Err(Refusal::AudienceMismatch),
    );
    Ok(())
}

#[test]
fn an_expired_token_is_refused() -> Result<(), Failure> {
    // The task's second required case.
    let resource = ResourceUri::new(RESOURCE)?;
    let policy = TokenPolicy::new(&resource, authorization_model(ProtocolVersion::V2026_07_28));
    let claims = valid_claims()?.with_expiry(now() - Duration::from_secs(1));

    assert_eq!(
        verify(&claims, &policy, &TwoScopes, now()),
        Err(Refusal::Expired),
    );
    Ok(())
}

#[test]
fn a_token_expiring_exactly_now_is_refused() -> Result<(), Failure> {
    // `exp` is exclusive, so equality is expiry. Off-by-one here is a token
    // accepted for one instant past its life.
    let resource = ResourceUri::new(RESOURCE)?;
    let policy = TokenPolicy::new(&resource, authorization_model(ProtocolVersion::V2026_07_28));
    let claims = valid_claims()?.with_expiry(now());

    assert_eq!(
        verify(&claims, &policy, &TwoScopes, now()),
        Err(Refusal::Expired),
    );
    Ok(())
}

#[test]
fn a_token_that_is_not_valid_yet_is_refused() -> Result<(), Failure> {
    let resource = ResourceUri::new(RESOURCE)?;
    let policy = TokenPolicy::new(&resource, authorization_model(ProtocolVersion::V2026_07_28));
    let claims = valid_claims()?.with_not_before(in_future());

    assert_eq!(
        verify(&claims, &policy, &TwoScopes, now()),
        Err(Refusal::NotYetValid),
    );
    Ok(())
}

#[test]
fn a_token_with_no_audience_is_refused() -> Result<(), Failure> {
    // A token that names no audience is what a captured-token replay carries.
    let resource = ResourceUri::new(RESOURCE)?;
    let policy = TokenPolicy::new(&resource, authorization_model(ProtocolVersion::V2026_07_28));
    let claims = TokenClaims::new()
        .with_subject("alice")
        .with_expiry(in_future())
        .with_scope("mcp:read");

    assert_eq!(
        verify(&claims, &policy, &TwoScopes, now()),
        Err(Refusal::AudienceMismatch),
    );
    Ok(())
}

#[test]
fn sharing_an_audience_array_with_this_server_is_enough() -> Result<(), Failure> {
    // RFC 7519 §4.1.3 allows `aud` to be an array. A token naming two servers is
    // legitimately usable at ours — a check requiring the array to be exactly
    // one element would wrongly refuse it.
    let resource = ResourceUri::new(RESOURCE)?;
    let policy = TokenPolicy::new(&resource, authorization_model(ProtocolVersion::V2026_07_28));
    // Two entries, ours and theirs — which is the whole point of the array
    // form, and the reason this cannot reuse [`claims_for`]'s single audience.
    let claims = claims_for(OTHER, &["mcp:read"])?.with_audience(resource.clone());

    let context = verify(&claims, &policy, &TwoScopes, now())?;
    assert_eq!(context.principal(), "alice");
    Ok(())
}

#[test]
fn a_sibling_path_on_the_same_host_is_a_different_resource() -> Result<(), Failure> {
    // The check this design turns on. A server mounted at a sibling path shares
    // a host with ours, so anything matching on host would accept its token.
    let resource = ResourceUri::new(RESOURCE)?;
    let policy = TokenPolicy::new(&resource, authorization_model(ProtocolVersion::V2026_07_28));
    let claims = TokenClaims::new()
        .with_subject("alice")
        .with_audience(ResourceUri::new(SIBLING)?)
        .with_expiry(in_future())
        .with_scope("mcp:read");

    assert_eq!(
        verify(&claims, &policy, &TwoScopes, now()),
        Err(Refusal::AudienceMismatch),
    );
    Ok(())
}

#[test]
fn an_unexpected_issuer_is_refused() -> Result<(), Failure> {
    let resource = ResourceUri::new(RESOURCE)?;
    let policy = TokenPolicy::new(&resource, authorization_model(ProtocolVersion::V2026_07_28))
        .with_expected_issuer("https://auth.example.com");
    let claims = valid_claims()?.with_issuer("https://elsewhere.example.com");

    assert_eq!(
        verify(&claims, &policy, &TwoScopes, now()),
        Err(Refusal::IssuerMismatch),
    );
    Ok(())
}

#[test]
fn an_absent_issuer_is_not_a_mismatch() -> Result<(), Failure> {
    // Only a *present, wrong* issuer is fatal. A token naming none is unusual,
    // but it is not evidence of anything.
    let resource = ResourceUri::new(RESOURCE)?;
    let policy = TokenPolicy::new(&resource, authorization_model(ProtocolVersion::V2026_07_28))
        .with_expected_issuer("https://auth.example.com");

    assert!(!policy.accepts_any_issuer());
    assert!(verify(&valid_claims()?, &policy, &TwoScopes, now()).is_ok());
    Ok(())
}

#[test]
fn a_token_naming_no_subject_is_refused() -> Result<(), Failure> {
    let resource = ResourceUri::new(RESOURCE)?;
    let policy = TokenPolicy::new(&resource, authorization_model(ProtocolVersion::V2026_07_28));
    let claims = TokenClaims::new()
        .with_audience(resource.clone())
        .with_expiry(in_future())
        .with_scope("mcp:read");

    assert_eq!(
        verify(&claims, &policy, &TwoScopes, now()),
        Err(Refusal::NoSubject),
    );
    Ok(())
}

#[test]
fn a_token_whose_scopes_map_to_nothing_is_refused() -> Result<(), Failure> {
    // Refused loudly as a configuration problem rather than accepted with no
    // permissions — the two are indistinguishable over the wire and very
    // different in a log.
    let resource = ResourceUri::new(RESOURCE)?;
    let policy = TokenPolicy::new(&resource, authorization_model(ProtocolVersion::V2026_07_28));
    let claims = claims_for(RESOURCE, &["some:scope:this:deployment:never:issued"])?;

    assert_eq!(
        verify(&claims, &policy, &TwoScopes, now()),
        Err(Refusal::Unscoped),
    );
    Ok(())
}

#[test]
fn an_unconfigured_deployment_refuses_every_token() -> Result<(), Failure> {
    // `NoScopes` maps nothing, so a deployment that never supplied a policy
    // denies everything — fail-secure by construction rather than by
    // remembering to check a flag.
    let resource = ResourceUri::new(RESOURCE)?;
    let policy = TokenPolicy::new(&resource, authorization_model(ProtocolVersion::V2026_07_28));

    assert_eq!(
        verify(&valid_claims()?, &policy, &NoScopes, now()),
        Err(Refusal::Unscoped),
    );
    Ok(())
}

#[test]
fn several_scopes_union_rather_than_replace() -> Result<(), Failure> {
    let resource = ResourceUri::new(RESOURCE)?;
    let policy = TokenPolicy::new(&resource, authorization_model(ProtocolVersion::V2026_07_28));
    let claims = claims_for(RESOURCE, &["mcp:read", "mcp:write", "unknown:scope"])?;

    let context = verify(&claims, &policy, &TwoScopes, now())?;
    assert!(context.holds(ScopeSet::empty().with(Scope::Read).with(Scope::Write)));
    Ok(())
}

#[test]
fn a_revision_without_a_framework_accepts_no_token() -> Result<(), Failure> {
    // `2024-11-05` has no authorization framework, so there is no audience to
    // bind against and no correct way to accept one.
    let resource = ResourceUri::new(RESOURCE)?;
    let policy = TokenPolicy::new(&resource, authorization_model(ProtocolVersion::V2024_11_05));

    assert_eq!(
        verify(&valid_claims()?, &policy, &TwoScopes, now()),
        Err(Refusal::AudienceMismatch),
    );
    Ok(())
}

#[test]
fn every_revision_with_a_framework_checks_the_audience() -> Result<(), Failure> {
    // Gated by `audience_bound`, which follows `specified` — so `2025-03-26`
    // gets the check even though it has no protected-resource metadata to
    // publish. A regression that made the gate follow the metadata instead would
    // fail here rather than going unnoticed.
    for version in ProtocolVersion::KNOWN {
        let resource = ResourceUri::new(RESOURCE)?;
        let policy = TokenPolicy::new(&resource, authorization_model(version));
        let claims = claims_for(OTHER, &["mcp:read"])?;

        let got = verify(&claims, &policy, &TwoScopes, now());
        assert_eq!(
            got,
            Err(Refusal::AudienceMismatch),
            "{version} must refuse a foreign-audience token",
        );
    }
    Ok(())
}

#[test]
fn expiry_is_reported_before_a_mismatch() -> Result<(), Failure> {
    // The order is deliberate: a stale token should not report an audience
    // problem, because the audience may well be fine and the caller would then
    // go looking in the wrong place.
    let resource = ResourceUri::new(RESOURCE)?;
    let policy = TokenPolicy::new(&resource, authorization_model(ProtocolVersion::V2026_07_28));
    let claims = TokenClaims::new()
        .with_subject("alice")
        .with_audience(ResourceUri::new(OTHER)?)
        .with_expiry(now() - Duration::from_secs(1))
        .with_scope("mcp:read");

    assert_eq!(
        verify(&claims, &policy, &TwoScopes, now()),
        Err(Refusal::Expired),
    );
    Ok(())
}

#[test]
fn scope_strings_reach_the_verifier_untranslated() -> Result<(), Failure> {
    // The claims keep the strings the authorization server sent. Mapping them to
    // `Scope` is the deployment's policy, so it happens in `ScopePolicy` and not
    // on the way in.
    let claims = valid_claims()?;
    assert_eq!(claims.scopes(), &[Arc::from("mcp:read")]);

    let resource = ResourceUri::new(RESOURCE)?;
    let policy = TokenPolicy::new(&resource, authorization_model(ProtocolVersion::V2026_07_28));
    assert!(verify(&claims, &policy, &TwoScopes, now()).is_ok());
    Ok(())
}

// ---------------------------------------------------------------------------
// `authenticate` — the seam, tested where no adapter is compiled
//
// These live here rather than beside the introspection adapter because
// `default-features = false` drops that adapter, and `authenticate` is
// application logic that a lean build must still be able to reach. With no
// implementor of `TokenSource` anywhere in the configuration, these are what
// keep it from being dead code in exactly the build that cannot afford it.
// ---------------------------------------------------------------------------

/// A [`TokenSource`] that answers from a fixed answer, standing in for an
/// authorization server the test does not run.
struct Fixed(Result<TokenClaims, SourceError>);

impl TokenSource for Fixed {
    fn claims(
        &self,
        _presented: &str,
    ) -> impl Future<Output = Result<TokenClaims, SourceError>> + Send {
        ready(self.0.clone())
    }
}

/// Drives a future to completion on this thread.
///
/// Awaiting a ready future still needs an executor, and tokio is not compiled
/// under `default-features = false`. Fifteen lines of standard library is
/// cheaper than gating these tests behind a feature that would leave the lean
/// build untested.
fn block_on<F: Future>(future: F) -> F::Output {
    struct ThreadWaker(std::thread::Thread);

    impl Wake for ThreadWaker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }

    let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = Box::pin(future);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
        std::thread::park();
    }
}

/// The policy every seam test below verifies against.
fn policy(resource: &ResourceUri) -> TokenPolicy<'_> {
    TokenPolicy::new(resource, authorization_model(ProtocolVersion::V2026_07_28))
}

#[test]
fn a_source_that_answers_produces_the_same_context_verifying_would_have() -> Result<(), Failure> {
    // `authenticate` must be exactly the seam it looks like: get claims, then
    // apply the policy. If it ever grew a rule of its own, this is the test
    // that would notice the two paths disagreeing.
    let resource = ResourceUri::new(RESOURCE)?;
    let source = Fixed(Ok(valid_claims()?));

    let context = block_on(authenticate(
        &source,
        &policy(&resource),
        &TwoScopes,
        "a-token",
        now(),
    ))?;

    assert_eq!(context.principal(), "alice");
    assert!(context.holds(ScopeSet::empty().with(Scope::Read)));
    Ok(())
}

#[test]
fn the_policy_still_gets_the_last_word_over_what_a_source_returned() -> Result<(), Failure> {
    // A source is trusted for authenticity, not for policy. One that hands back
    // a token minted for another server must still be refused here, or the
    // seam would be a way around the audience check.
    let resource = ResourceUri::new(RESOURCE)?;
    let source = Fixed(Ok(claims_for(OTHER, &["mcp:read"])?));

    let refusal = block_on(authenticate(
        &source,
        &policy(&resource),
        &TwoScopes,
        "a-token",
        now(),
    ));

    assert_matches!(
        refusal,
        Err(SourceError::Refused(Refusal::AudienceMismatch))
    );
    Ok(())
}

#[test]
fn a_source_that_says_no_is_not_re_written_as_an_unavailable_source() -> Result<(), Failure> {
    // The two failures are different acts. A refusal means the caller's token
    // is bad and re-authenticating may help; unavailable means we could not
    // find out. Collapsing them would send someone to re-authenticate while
    // the authorization server is down.
    let resource = ResourceUri::new(RESOURCE)?;
    let source = Fixed(Err(SourceError::Refused(Refusal::Expired)));

    let outcome = block_on(authenticate(
        &source,
        &policy(&resource),
        &TwoScopes,
        "a-token",
        now(),
    ));

    assert_matches!(outcome, Err(SourceError::Refused(Refusal::Expired)));
    Ok(())
}

#[test]
fn an_unreachable_source_stays_unavailable_and_never_becomes_a_refusal() -> Result<(), Failure> {
    let resource = ResourceUri::new(RESOURCE)?;
    let source = Fixed(Err(SourceError::Unavailable(
        "introspection responded 503".into(),
    )));

    let outcome = block_on(authenticate(
        &source,
        &policy(&resource),
        &TwoScopes,
        "a-token",
        now(),
    ));

    assert_matches!(&outcome, Err(SourceError::Unavailable(_)));
    // The status survives to the caller: it is what tells an operator the
    // authorization server is down rather than the token being bad.
    if let Err(SourceError::Unavailable(reason)) = &outcome {
        assert!(reason.contains("503"));
    }
    Ok(())
}
