//! What the redemption adapter sends, and how it reads what comes back.
//!
//! Every test here parses a literal rather than standing up a token endpoint,
//! for the reason the introspection tests give: the rules worth testing are in
//! how a *response* is read, and a socket in the way only adds a place for them
//! to hide. The request side is covered by the use-case tests, which can read
//! the assembled [`TokenRequest`] directly.

use std::assert_matches;

use super::{HttpTokenEndpoint, TokenEndpoint};
use crate::domain::oauth::issued_token::{IssuedToken, TokenResponseError};

const ENDPOINT: &str = "https://auth.example.com/token";

/// An endpoint that is constructed but never asked, so the constructors are
/// covered by something in this target.
///
/// `dead_code_pub_in_binary` counts a `pub` item as dead in the lib's *test*
/// build, where nothing outside the crate can call it — an integration test in
/// `tests/` does not satisfy it. The introspection adapter covers this the same
/// way.
#[test]
fn the_endpoint_is_constructible_and_knows_where_it_posts() {
    let endpoint = HttpTokenEndpoint::new(ENDPOINT);

    assert_eq!(endpoint.endpoint(), ENDPOINT);
}

/// The escape hatch for a caller that needs a timeout or a proxy. Asserted
/// because it is the only way to bound this call, and a deployment that cannot
/// bound it is holding a request that may never return.
#[test]
fn the_http_client_can_be_replaced() {
    let endpoint = HttpTokenEndpoint::new(ENDPOINT).with_http_client(reqwest::Client::new());

    assert_eq!(endpoint.endpoint(), ENDPOINT);
}

/// The trait is RPITIT and therefore not dyn compatible — the deliberate trade
/// documented on it. Asserting it as a generic bound records that the adapter
/// satisfies its port without requiring a vtable the design chose not to have.
#[test]
fn the_adapter_satisfies_its_port() {
    fn is_a_port<T: TokenEndpoint + ?Sized>(_: &T) {}
    is_a_port(&HttpTokenEndpoint::new(ENDPOINT));
}

// --- how a response is read -------------------------------------------------
//
// `IssuedToken::parse` is exercised in depth in the domain module. What is
// worth pinning *here* is that this adapter reaches it on every path, and that
// nothing in the adapter's own error handling converts a refusal into a token.

#[test]
fn a_success_body_is_handed_to_the_domain_parser() {
    let outcome =
        IssuedToken::parse(br#"{"access_token":"at-1","token_type":"Bearer","expires_in":3600}"#);

    assert_eq!(
        outcome
            .as_ref()
            .map(|issued| issued.access_token().as_str()),
        Ok("at-1")
    );
}

/// The distinction the adapter's status branch depends on: a §5.2 body must be
/// recognised as a refusal no matter what status accompanied it, because a 200
/// with `invalid_grant` is the case that would otherwise become a token.
#[test]
fn an_error_body_is_a_refusal_regardless_of_status() {
    let outcome = IssuedToken::parse(br#"{"error":"invalid_grant"}"#);

    assert_matches!(
        outcome,
        Err(TokenResponseError::AuthorizationServer { error, .. })
            if error == "invalid_grant"
    );
}
