//! Authentication and authorization schemes the agent declares.
//!
//! A2A v1.0 §5.4 specifies that [`AgentCard::security_schemes`]
//! is a map from scheme name (the caller's chosen handle) to
//! the scheme itself. The wire form follows the `OpenAPI 3.x`
//! security scheme convention: each scheme is a JSON object
//! with a `type` field that discriminates the variant.
//!
//! # Why `serde(tag = "type")`
//!
//! The proto declares a `oneof scheme { ... }` with five
//! variants, but the spec's wire form is `OpenAPI`'s, which
//! tags the variant by a string in the `type` field
//! (`"apiKey"`, `"http"`, `"oauth2"`, `"openIdConnect"`,
//! `"mutualTls"`). Serde's `#[serde(tag = "type")]` is the
//! right shape: a single JSON object per scheme, with the
//! `type` field set to the variant name and the rest of the
//! fields filled in by the chosen variant.
//!
//! # Out of scope this commit
//!
//! - `OAuth2SecurityScheme` and the four `OAuthFlows`
//!   sub-types — substantial, get their own commit.
//! - `OpenIdConnectSecurityScheme` — small, but depends on
//!   the `OpenID Connect` URL parsing rules.
//! - `MutualTlsSecurityScheme` — marker type, also its own
//!   commit so the `type: "mutualTls"` wire form gets a
//!   dedicated test.
//!
//! This commit ships the [`SecurityScheme`] enum and the
//! two simple variants — API key and HTTP — so the
//! consumer-side happy path is unblocked while the larger
//! work lands.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use url::Url;

/// An authentication or authorization scheme the agent
/// declares.
///
/// The wire form is `OpenAPI 3.x`: a single JSON object
/// with a `type` field that names the variant. The five
/// variants map to the proto's `oneof scheme`; this
/// commit ships the final two, [`OpenIdConnect`](Self::OpenIdConnect)
/// and [`MutualTls`](Self::MutualTls), completing the
/// set the spec names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SecurityScheme {
    /// API key authentication. The key is carried in a
    /// query parameter, a header, or a cookie, named by
    /// [`APIKeySecurityScheme::name`].
    #[serde(rename = "apiKey")]
    ApiKey(APIKeySecurityScheme),
    /// HTTP authentication, as defined in RFC 7235. The
    /// most common value is `Bearer` (RFC 6750); `Basic`
    /// is also standard.
    #[serde(rename = "http")]
    Http(HTTPAuthSecurityScheme),
    /// OAuth 2.0 authentication (RFC 6749). The variant
    /// carries the [`OAuth2SecurityScheme`] payload
    /// (flows + RFC 8414 metadata URL) boxed to keep the
    /// enum's total size down — the OAuth 2 sub-types
    /// are several hundred bytes, and the `ApiKey` /
    /// `Http` variants are forty.
    #[serde(rename = "oauth2")]
    OAuth2(Box<super::oauth_flow::OAuth2SecurityScheme>),
    /// `OpenID Connect` authentication (the discovery
    /// URL is the entry point the client fetches to
    /// learn the OAuth 2 endpoints the OIDC provider
    /// supports).
    #[serde(rename = "openIdConnect")]
    OpenIdConnect(OpenIdConnectSecurityScheme),
    /// Mutual TLS authentication (RFC 8705). The agent
    /// authenticates the client by validating the
    /// client's X.509 certificate against a configured
    /// trust anchor; no per-request credentials are
    /// sent.
    #[serde(rename = "mutualTls")]
    MutualTls(MutualTlsSecurityScheme),
}

/// An `OpenID Connect` discovery scheme.
///
/// The wire form is `OpenAPI 3.x`'s: a JSON object with
/// `type: "openIdConnect"` and an `openIdConnectUrl`
/// the client uses to discover the provider's
/// configuration (per the
/// `OpenID Connect Discovery 1.0` spec).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenIdConnectSecurityScheme {
    /// An optional human-readable description of the
    /// scheme.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Arc<str>>,
    /// The `OpenID Connect` discovery URL. REQUIRED.
    pub open_id_connect_url: Url,
}

/// A mutual-TLS authentication scheme (RFC 8705).
///
/// The wire form is `OpenAPI 3.x`'s: a JSON object with
/// `type: "mutualTls"` and an optional description.
/// The client presents an X.509 certificate at TLS
/// handshake; the agent validates the certificate
/// against its configured trust anchors. No per-request
/// `Authorization` header is sent — the TLS handshake
/// is the authentication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MutualTlsSecurityScheme {
    /// An optional human-readable description of the
    /// scheme.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Arc<str>>,
}

/// An API-key-based authentication scheme.
///
/// The wire form is `OpenAPI`'s: a JSON object with `type:
/// "apiKey"`, `in: <location>`, and `name: <parameter>`.
/// The `in` field is `OpenAPI`'s name for what the proto
/// calls `location`; the rename is in [`APIKeySecurityScheme::in_`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct APIKeySecurityScheme {
    /// An optional human-readable description of the
    /// scheme.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Arc<str>>,
    /// Where the API key is carried. Wire form is
    /// `OpenAPI`'s `in`; the Rust field is `in_` because
    /// `in` is a reserved keyword.
    #[serde(rename = "in")]
    pub in_: APIKeyLocation,
    /// The name of the header, query parameter, or cookie
    /// the key is carried in. REQUIRED, non-empty.
    pub name: Arc<str>,
}

/// Where an API key is carried in a request.
///
/// The wire form is `OpenAPI`'s: one of `"query"`, `"header"`,
/// or `"cookie"`. The proto's `string location` accepts the
/// same three values; the enum rejects anything else at
/// the call site that constructs the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum APIKeyLocation {
    /// The key is a query parameter
    /// (e.g. `?api_key=...`).
    Query,
    /// The key is an HTTP request header
    /// (e.g. `X-API-Key: ...`).
    Header,
    /// The key is a cookie
    /// (e.g. `Cookie: session=...`).
    Cookie,
}

/// An HTTP authentication scheme, as defined in RFC 7235.
///
/// The most common value is `Bearer` (RFC 6750); `Basic`
/// is also standard. The `scheme` field is the
/// `IANA`-registered value the agent expects in the
/// `Authorization` header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HTTPAuthSecurityScheme {
    /// An optional human-readable description of the
    /// scheme.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Arc<str>>,
    /// The IANA-registered authentication scheme name
    /// (e.g. `Bearer`, `Basic`). REQUIRED, non-empty.
    pub scheme: Arc<str>,
    /// A hint to the client about how the bearer token
    /// is formatted (e.g. `JWT`). Primarily for
    /// documentation; clients do not parse it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearer_format: Option<Arc<str>>,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        APIKeyLocation, APIKeySecurityScheme, HTTPAuthSecurityScheme, MutualTlsSecurityScheme,
        OpenIdConnectSecurityScheme, SecurityScheme,
    };

    type TestError = Box<dyn std::error::Error>;

    // --- API key -----------------------------------------------------

    #[test]
    fn api_key_serialises_with_openapi_wire_form() -> Result<(), TestError> {
        let scheme = SecurityScheme::ApiKey(APIKeySecurityScheme {
            description: None,
            in_: APIKeyLocation::Header,
            name: Arc::from("X-API-Key"),
        });
        let value = serde_json::to_value(&scheme)?;
        let Some(obj) = value.as_object() else {
            return Err("a SecurityScheme must serialise to a JSON object".to_owned().into());
        };
        assert_eq!(
            obj.get("type").and_then(serde_json::Value::as_str),
            Some("apiKey"),
            "the type field must be the OpenAPI literal \"apiKey\""
        );
        assert_eq!(
            obj.get("in").and_then(serde_json::Value::as_str),
            Some("header"),
            "the in field must be the OpenAPI name for location"
        );
        assert_eq!(
            obj.get("name").and_then(serde_json::Value::as_str),
            Some("X-API-Key")
        );
        assert!(
            !obj.contains_key("description"),
            "an absent description must not serialise as null"
        );
        Ok(())
    }

    #[test]
    fn api_key_round_trips_through_serde() -> Result<(), TestError> {
        let scheme = SecurityScheme::ApiKey(APIKeySecurityScheme {
            description: Some(Arc::from("API key for the public read endpoint.")),
            in_: APIKeyLocation::Query,
            name: Arc::from("api_key"),
        });
        let json = serde_json::to_string(&scheme)?;
        let parsed: SecurityScheme = serde_json::from_str(&json)?;
        assert_eq!(parsed, scheme);
        Ok(())
    }

    #[test]
    fn api_key_location_serialises_as_openapi_literal() {
        // OpenAPI's three valid values: "query", "header",
        // "cookie". A typo in the enum's `rename_all` would
        // pass the enum tests but break the wire form.
        let query = serde_json::to_value(APIKeyLocation::Query).ok();
        let header = serde_json::to_value(APIKeyLocation::Header).ok();
        let cookie = serde_json::to_value(APIKeyLocation::Cookie).ok();
        assert_eq!(query.as_ref().and_then(|v| v.as_str()), Some("query"));
        assert_eq!(header.as_ref().and_then(|v| v.as_str()), Some("header"));
        assert_eq!(cookie.as_ref().and_then(|v| v.as_str()), Some("cookie"));
    }

    // --- HTTP --------------------------------------------------------

    #[test]
    fn http_bearer_serialises_with_openapi_wire_form() -> Result<(), TestError> {
        let scheme = SecurityScheme::Http(HTTPAuthSecurityScheme {
            description: None,
            scheme: Arc::from("Bearer"),
            bearer_format: Some(Arc::from("JWT")),
        });
        let value = serde_json::to_value(&scheme)?;
        let Some(obj) = value.as_object() else {
            return Err("a SecurityScheme must serialise to a JSON object".to_owned().into());
        };
        assert_eq!(
            obj.get("type").and_then(serde_json::Value::as_str),
            Some("http")
        );
        assert_eq!(
            obj.get("scheme").and_then(serde_json::Value::as_str),
            Some("Bearer")
        );
        assert_eq!(
            obj.get("bearerFormat").and_then(serde_json::Value::as_str),
            Some("JWT")
        );
        Ok(())
    }

    #[test]
    fn http_basic_round_trips_through_serde() -> Result<(), TestError> {
        let scheme = SecurityScheme::Http(HTTPAuthSecurityScheme {
            description: Some(Arc::from("HTTP Basic auth.")),
            scheme: Arc::from("Basic"),
            bearer_format: None,
        });
        let json = serde_json::to_string(&scheme)?;
        let parsed: SecurityScheme = serde_json::from_str(&json)?;
        assert_eq!(parsed, scheme);
        Ok(())
    }

    // --- `OpenIdConnect` ---------------------------------------------

    #[test]
    fn open_id_connect_serialises_with_openapi_wire_form() -> Result<(), TestError> {
        let scheme = SecurityScheme::OpenIdConnect(OpenIdConnectSecurityScheme {
            description: Some(Arc::from("Login with the corporate OIDC provider.")),
            open_id_connect_url: url::Url::parse(
                "https://auth.example.com/.well-known/openid-configuration",
            )?,
        });
        let value = serde_json::to_value(&scheme)?;
        let Some(obj) = value.as_object() else {
            return Err("a SecurityScheme must serialise to a JSON object"
                .to_owned()
                .into());
        };
        assert_eq!(
            obj.get("type").and_then(serde_json::Value::as_str),
            Some("openIdConnect"),
            "the type field must be the OpenAPI literal \"openIdConnect\""
        );
        assert_eq!(
            obj.get("openIdConnectUrl").and_then(serde_json::Value::as_str),
            Some("https://auth.example.com/.well-known/openid-configuration")
        );
        Ok(())
    }

    #[test]
    fn open_id_connect_round_trips_through_serde() -> Result<(), TestError> {
        let scheme = SecurityScheme::OpenIdConnect(OpenIdConnectSecurityScheme {
            description: None,
            open_id_connect_url: url::Url::parse("https://auth.example.com/.well-known/openid-configuration")?,
        });
        let json = serde_json::to_string(&scheme)?;
        let parsed: SecurityScheme = serde_json::from_str(&json)?;
        assert_eq!(parsed, scheme);
        Ok(())
    }

    // --- `MutualTls` -------------------------------------------------

    #[test]
    fn mutual_tls_serialises_with_openapi_wire_form() -> Result<(), TestError> {
        let scheme = SecurityScheme::MutualTls(MutualTlsSecurityScheme {
            description: Some(Arc::from("Client certs validated against the corporate CA.")),
        });
        let value = serde_json::to_value(&scheme)?;
        let Some(obj) = value.as_object() else {
            return Err("a SecurityScheme must serialise to a JSON object"
                .to_owned()
                .into());
        };
        assert_eq!(
            obj.get("type").and_then(serde_json::Value::as_str),
            Some("mutualTls"),
            "the type field must be the OpenAPI literal \"mutualTls\""
        );
        assert_eq!(
            obj.get("description").and_then(serde_json::Value::as_str),
            Some("Client certs validated against the corporate CA.")
        );
        Ok(())
    }

    #[test]
    fn mutual_tls_round_trips_through_serde() -> Result<(), TestError> {
        let scheme = SecurityScheme::MutualTls(MutualTlsSecurityScheme { description: None });
        let json = serde_json::to_string(&scheme)?;
        let parsed: SecurityScheme = serde_json::from_str(&json)?;
        assert_eq!(parsed, scheme);
        Ok(())
    }
}
