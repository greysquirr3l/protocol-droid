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

use std::sync::Arc;

use jsonwebtoken::Algorithm;

use protocol_droid::adapters::secondary::oauth::introspection::IntrospectionSource;
use protocol_droid::adapters::secondary::oauth::jwt::JwtSource;
use protocol_droid::adapters::secondary::oauth::redeem::HttpTokenEndpoint;
use protocol_droid::adapters::secondary::oauth::secrets::OsSecrets;
use protocol_droid::application::oauth::TokenSource;
use protocol_droid::application::oauth::client::AuthorizationSecrets;
use protocol_droid::application::oauth::redeem::TokenEndpoint;

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
