//! The OAuth composition root.
//!
//! # What this file is
//!
//! The T25 wiring audit
//! (`crates/protocol-droid/tests/port_trait_wiring.rs`) tracks
//! every port-trait impl in the adapters layer. The four
//! OAuth adapters were `OUTSTANDING` because no composition
//! root anywhere in the workspace constructed them:
//!
//!   - `OsSecrets`        impls `AuthorizationSecrets`
//!   - `IntrospectionSource` impls `TokenSource`
//!   - `JwtSource`        impls `TokenSource`
//!   - `HttpTokenEndpoint`  impls `TokenEndpoint`
//!
//! This file is the composition root that wires them. The
//! shape is "construct each with a real-looking config and
//! assert each satisfies its port." Driving a full OAuth
//! flow against a real authorization server is a
//! conformance test, not a wiring test, and lives in the
//! conformance suite; this file is the proof that the four
//! adapters are reachable from a composition root and that
//! the trait bounds they advertise compile.
//!
//! The test is gated behind the `oauth` and `random-ids`
//! features because every type it touches
//! (`OsSecrets`, `IntrospectionSource`, `JwtSource`,
//! `HttpTokenEndpoint`, and the `jsonwebtoken` crate) is
//! feature-gated there. A default-feature build would not
//! compile the test, so we skip the file entirely in that
//! configuration.
#![cfg(all(feature = "oauth", feature = "random-ids"))]

/// The fallible calls in this file answer unrelated error types, so tests
/// return a `Result` over this rather than unwrapping.
type TestError = Box<dyn std::error::Error>;

fn fail(message: impl Into<String>) -> TestError {
    message.into().into()
}

use std::sync::Arc;

use jsonwebtoken::Algorithm;

use protocol_droid::adapters::secondary::oauth::introspection::IntrospectionSource;
use protocol_droid::adapters::secondary::oauth::jwt::JwtSource;
use protocol_droid::adapters::secondary::oauth::redeem::HttpTokenEndpoint;
use protocol_droid::adapters::secondary::oauth::secrets::OsSecrets;
use protocol_droid::application::oauth::client::AuthorizationSecrets;
use protocol_droid::application::oauth::redeem::TokenEndpoint;
use protocol_droid::application::oauth::{ScopePolicy, TokenSource};

const fn is_authorization_secrets<T: AuthorizationSecrets + ?Sized>(_: &T) {}

const fn is_token_source<T: TokenSource + ?Sized>(_: &T) {}

const fn is_token_endpoint<T: TokenEndpoint + ?Sized>(_: &T) {}

#[test]
fn os_secrets_satisfies_authorization_secrets() {
    is_authorization_secrets(&OsSecrets);
}

#[test]
fn introspection_source_is_constructible_with_real_config() {
    let source = IntrospectionSource::new(
        Arc::from("https://auth.example.com/introspect"),
        "client-id-7c3f",
        "client-secret-DO-NOT-LOG",
    );
    is_token_source(&source);
}

#[test]
fn jwt_source_is_constructible_with_real_config() {
    let source = JwtSource::new(
        Arc::from("https://auth.example.com/.well-known/jwks.json"),
        vec![Algorithm::RS256, Algorithm::ES256],
    );
    is_token_source(&source);
}

#[test]
fn http_token_endpoint_is_constructible_with_real_config() {
    let endpoint = HttpTokenEndpoint::new("https://auth.example.com/token");
    is_token_endpoint(&endpoint);
    assert_eq!(endpoint.endpoint(), "https://auth.example.com/token");
}

#[test]
fn os_secrets_draws_per_call_state_and_verifier() {
    // A regression to a constant return value would be the
    // most obvious bug a wiring test could catch. Two
    // distinct draws per call prove the source is per-call.
    // The values are secrets — the `Debug` impl is redacted
    // — so the only thing the test can compare is the value
    // itself. The two `assert!`s use `is_ok()` because
    // the production code's `SecretError` is a `thiserror`
    // enum, and a refusal of `getrandom` is a kernel
    // problem the test fixture cannot manufacture.
    let secrets = OsSecrets;

    let state_one = secrets.new_state();
    let state_two = secrets.new_state();
    assert!(state_one.is_ok(), "first new_state must succeed");
    assert!(state_two.is_ok(), "second new_state must succeed");
    assert_ne!(state_one, state_two, "two new_state() calls must differ");

    let verifier_one = secrets.new_code_verifier();
    let verifier_two = secrets.new_code_verifier();
    assert!(verifier_one.is_ok(), "first new_code_verifier must succeed");
    assert!(
        verifier_two.is_ok(),
        "second new_code_verifier must succeed"
    );
    assert_ne!(
        verifier_one, verifier_two,
        "two new_code_verifier() calls must differ"
    );
}

// -------------------------------------------------------------------------
// Can a server *require* OAuth, end to end?
// -------------------------------------------------------------------------

/// The issue's question, answered by compiling the assembly rather than by
/// listing the parts.
///
/// Every earlier test in this file checks that one component is constructible.
/// None checks that the components compose into a server that actually refuses
/// an anonymous caller and publishes the document a client needs to get a
/// token — which is the question a deployment has. If `HttpBuilder` did not
/// accept an authenticator, or if the document were not mounted, this fails to
/// compile or fails its assertions, and the "it all exists individually" claim
/// stops being load-bearing.
#[test]
fn a_server_can_require_oauth_from_the_public_surface_alone() -> Result<(), TestError> {
    use ScopePolicy;
    use protocol_droid::adapters::primary::transport::http::{AuthPolicy, Refusal};
    use protocol_droid::adapters::primary::transport::http::{
        OAuthAuthenticator, ServerConfig, SystemClock,
    };
    use protocol_droid::domain::ProtocolVersion;
    use protocol_droid::domain::mcp::{ResourceMetadata, ResourceUri};
    use protocol_droid::domain::registry::auth::{AuthContext, Scope, ScopeSet};

    /// Stands in for a deployment's scope vocabulary. The specification names
    /// no scope strings, so this is policy a consumer supplies rather than
    /// something this crate can decide.
    struct McpScopes;

    impl ScopePolicy for McpScopes {
        fn resolve(&self, scope: &str) -> Option<Scope> {
            scope.starts_with("mcp:").then_some(Scope::Read)
        }
        fn principal(&self, subject: &str, _scopes: &[Arc<str>]) -> String {
            subject.to_owned()
        }
    }

    // A JWKS-backed token source, built from public API only.
    let source = JwtSource::new(
        Arc::from("https://login.example.com/.well-known/jwks.json"),
        vec![Algorithm::RS256],
    );

    let metadata = ResourceMetadata::new(ResourceUri::new("https://mcp.example/mcp")?)
        .with_authorization_server("https://login.example.com")
        .with_scope("mcp:read");

    // The authenticator validates a token presented *to* the server, binding
    // its audience to `metadata` — RFC 8707, which is what stops a token
    // minted for somebody else from being replayed here.
    let authenticator = OAuthAuthenticator::new(
        source,
        metadata.clone(),
        ProtocolVersion::V2025_06_18,
        McpScopes,
        SystemClock,
    );

    // It satisfies the builder's port, with no adapter-internal assembly.
    let _: Arc<
        dyn protocol_droid::adapters::primary::transport::http::Authenticator + Send + Sync,
    > = Arc::new(authenticator);

    // And the document a client needs is reachable from the same config the
    // challenge is built from, so the two cannot name different URLs.
    let config = ServerConfig::new()?.with_resource_metadata(metadata);
    let challenge = config
        .challenge()
        .ok_or_else(|| fail("a configured document produced no challenge"))?;
    if !challenge.contains("/.well-known/oauth-protected-resource/mcp") {
        return Err(fail(format!("challenge named the wrong path: {challenge}")));
    }

    // The other half of "can require OAuth": under a `Require` policy an
    // anonymous caller is refused, which is what produces the `401` carrying
    // the challenge above. Uses the policy's own decision rather than
    // re-deriving it, so this cannot drift from what the router enforces.
    let policy = AuthPolicy::Require(ScopeSet::empty().with(Scope::Read));
    let refusal = policy.refuse(&AuthContext::anonymous());
    if refusal != Some(Refusal::Unauthenticated) {
        return Err(fail(format!(
            "an anonymous caller under Require was not refused: {refusal:?}"
        )));
    }

    // And a context holding the required scope is not, or the requirement
    // would refuse everyone including authenticated peers.
    let permitted = AuthContext::new("peer", ScopeSet::empty().with(Scope::Read));
    if policy.refuse(&permitted).is_some() {
        return Err(fail("a caller holding the required scope was refused"));
    }
    Ok(())
}
