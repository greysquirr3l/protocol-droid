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

// --- the refresh request on the wire ---------------------------------------

/// A token endpoint that records the one form it is sent and answers a token.
async fn capturing() -> Result<
    (
        String,
        std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>,
    ),
    Box<dyn std::error::Error>,
> {
    use std::sync::{Arc, Mutex};

    let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let app = axum::Router::new().route(
        "/token",
        axum::routing::post(move |headers: axum::http::HeaderMap, body: String| {
            let sink = Arc::clone(&sink);
            async move {
                if let Ok(mut seen) = sink.lock() {
                    let auth = headers
                        .get("authorization")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_owned();
                    seen.push((auth, body));
                }
                axum::Json(serde_json::json!({
                    "access_token": "new", "token_type": "Bearer", "refresh_token": "rotated"
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok((format!("http://{addr}/token"), seen))
}

#[tokio::test]
async fn a_refresh_posts_the_refresh_grant_with_the_resource()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::application::oauth::redeem::RefreshRequest;
    use crate::domain::mcp::resource_metadata::ResourceUri;
    use crate::domain::oauth::registration::{ClientId, ClientSecret, RegisteredClient};
    use crate::domain::oauth::resource::ResourceParameters;

    let (url, seen) = capturing().await?;
    let client = RegisteredClient::dynamic(
        "https://auth.example.com",
        ClientId::opaque("the-client"),
        Some(ClientSecret::issued("the-secret")),
    )?;
    let resource = ResourceParameters::single(&ResourceUri::new("https://mcp.example.com/mcp")?);
    let request = RefreshRequest::new("the-refresh", &client, &resource, None);

    let issued = HttpTokenEndpoint::new(url).refresh(request).await?;
    assert_eq!(issued.access_token().as_str(), "new");
    assert_eq!(
        issued
            .refresh_token()
            .map(crate::domain::oauth::issued_token::RefreshToken::as_str),
        Some("rotated")
    );

    let posted = seen.lock().map_err(|_| "poisoned")?.clone();
    let (auth, body) = posted.first().ok_or("nothing was posted")?;
    assert!(body.contains("grant_type=refresh_token"), "{body}");
    assert!(body.contains("refresh_token=the-refresh"), "{body}");
    assert!(
        body.contains("resource=https%3A%2F%2Fmcp.example.com%2Fmcp"),
        "{body}"
    );
    assert!(
        !body.contains("the-secret"),
        "a basic-auth client keeps its secret in the header: {body}"
    );
    assert!(auth.starts_with("Basic "), "{auth}");
    Ok(())
}
