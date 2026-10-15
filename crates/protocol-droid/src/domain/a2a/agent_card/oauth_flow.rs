//! OAuth 2.0 security scheme and the four flow types the
//! A2A v1.0 spec inherits from `OpenAPI 3.x`.
//!
//! # What this is
//!
//! An [`OAuth2SecurityScheme`] declares how a client
//! authenticates against the agent using OAuth 2.0. The
//! scheme carries:
//!
//! - A [`OAuthFlows`] object with one of the three
//!   spec-supported flows: authorization code, client
//!   credentials, or device code. The proto's
//!   `oneof flow` and the `OpenAPI 3.x` wire form both
//!   say "at least one flow is set".
//! - An optional `oauth2MetadataUrl` per RFC 8414 — the
//!   URL the client uses to discover the authorization
//!   server's metadata (token endpoint, supported scopes,
//!   PKCE requirements, etc.).
//!
//! # Why a struct, not an enum
//!
//! The proto declares [`OAuthFlows`] as a `oneof flow`
//! with five variants (two deprecated). The
//! `OpenAPI 3.x` wire form, which the A2A spec
//! follows, is a JSON object with each flow as an
//! optional field. The Rust shape that round-trips
//! with the wire form is a struct with three
//! `Option<...>` fields; the builder enforces
//! "at least one is set" so the proto's oneof
//! invariant survives the round-trip.
//!
//! # What this commit ships
//!
//! - [`OAuth2SecurityScheme`] (the scheme itself)
//! - [`OAuthFlows`] (the aggregator)
//! - [`AuthorizationCodeOAuthFlow`],
//!   [`ClientCredentialsOAuthFlow`],
//!   [`DeviceCodeOAuthFlow`] (the three spec-named flows)
//!
//! The two deprecated flows the proto names
//! ([`ImplicitOAuthFlow`], [`PasswordOAuthFlow`]) are
//! dropped per the proto's `[deprecated = true]`.
//! A future client that needs to consume a card
//! with one of those flows can use
//! `#[serde(other)]` to round-trip without binding
//! to a typed shape — the cost of modelling them
//! now is a maintenance burden the spec actively
//! disclaims.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use url::Url;

/// An OAuth 2.0 authentication scheme, as defined in
/// `OpenAPI 3.x` and RFC 6749.
///
/// The wire form is a single JSON object with
/// `type: "oauth2"`, a `flows` object (one of the three
/// spec-named flows), and an optional
/// `oauth2MetadataUrl` per RFC 8414.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuth2SecurityScheme {
    /// An optional human-readable description of the
    /// scheme.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Arc<str>>,
    /// The OAuth flows the agent supports. The
    /// `OAuthFlows` value must have at least one flow
    /// set; the builder rejects an empty aggregator.
    pub flows: OAuthFlows,
    /// The URL the client uses to fetch the
    /// authorization server's metadata, per
    /// RFC 8414. Optional in the spec; absent on the
    /// wire when the agent does not advertise it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth2_metadata_url: Option<Url>,
}

/// The OAuth flows the agent supports.
///
/// The wire form is `OpenAPI 3.x`'s: a JSON object
/// with three optional fields, at least one of which
/// must be set. The proto's `oneof flow` is the same
/// constraint — a `OAuthFlows` value with no flow is
/// not a valid OAuth 2.0 configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthFlows {
    /// The OAuth 2.0 authorization code flow (RFC 6749
    /// §4.1). The recommended flow for user-agent
    /// clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_code: Option<AuthorizationCodeOAuthFlow>,
    /// The OAuth 2.0 client credentials flow (RFC 6749
    /// §4.4). The recommended flow for service-to-service
    /// authentication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_credentials: Option<ClientCredentialsOAuthFlow>,
    /// The OAuth 2.0 device authorization flow (RFC 8628).
    /// The recommended flow for input-constrained devices
    /// like `TV`s, `IoT`, and `CLI` tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_code: Option<DeviceCodeOAuthFlow>,
}

impl OAuthFlows {
    /// `true` when at least one flow is set. The
    /// builder calls this to enforce the proto's
    /// `oneof` invariant at construction.
    #[must_use]
    pub const fn has_any(&self) -> bool {
        self.authorization_code.is_some()
            || self.client_credentials.is_some()
            || self.device_code.is_some()
    }
}

/// Configuration for the OAuth 2.0 authorization code
/// flow (RFC 6749 §4.1).
///
/// The recommended flow for user-agent clients, and the
/// only flow that supports `PKCE` (RFC 7636).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizationCodeOAuthFlow {
    /// The authorization URL. REQUIRED.
    pub authorization_url: Url,
    /// The token URL. REQUIRED.
    pub token_url: Url,
    /// The refresh URL. Optional in the spec; absent
    /// on the wire when the agent does not advertise
    /// a separate refresh endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_url: Option<Url>,
    /// The available scopes, mapped from scope name
    /// to a human-readable description. REQUIRED
    /// (the field is always present on the wire; an
    /// empty map is valid for an agent that exposes
    /// no scope checks).
    #[serde(default)]
    pub scopes: BTreeMap<Arc<str>, Arc<str>>,
    /// Whether `PKCE` (RFC 7636) is required. The
    /// spec recommends `true` for public clients and
    /// in fact every modern client should use it.
    #[serde(default)]
    pub pkce_required: bool,
}

/// Configuration for the OAuth 2.0 client credentials
/// flow (RFC 6749 §4.4).
///
/// The recommended flow for service-to-service
/// authentication where the client is itself the
/// resource owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCredentialsOAuthFlow {
    /// The token URL. REQUIRED.
    pub token_url: Url,
    /// The refresh URL. Optional in the spec; absent
    /// on the wire when the agent does not advertise
    /// a separate refresh endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_url: Option<Url>,
    /// The available scopes, mapped from scope name
    /// to a human-readable description. REQUIRED
    /// (the field is always present on the wire; an
    /// empty map is valid for an agent that exposes
    /// no scope checks).
    #[serde(default)]
    pub scopes: BTreeMap<Arc<str>, Arc<str>>,
}

/// Configuration for the OAuth 2.0 device authorization
/// flow (RFC 8628).
///
/// The recommended flow for input-constrained devices
/// like `TV`s, `IoT`, and `CLI` tools where the user
/// authorizes the device from a separate browser session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCodeOAuthFlow {
    /// The device authorization endpoint URL. REQUIRED.
    pub device_authorization_url: Url,
    /// The token URL. REQUIRED.
    pub token_url: Url,
    /// The refresh URL. Optional in the spec; absent
    /// on the wire when the agent does not advertise
    /// a separate refresh endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_url: Option<Url>,
    /// The available scopes, mapped from scope name
    /// to a human-readable description. REQUIRED
    /// (the field is always present on the wire; an
    /// empty map is valid for an agent that exposes
    /// no scope checks).
    #[serde(default)]
    pub scopes: BTreeMap<Arc<str>, Arc<str>>,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use super::{
        AuthorizationCodeOAuthFlow, ClientCredentialsOAuthFlow, DeviceCodeOAuthFlow,
        OAuth2SecurityScheme, OAuthFlows,
    };
    use crate::domain::a2a::agent_card::security_scheme::SecurityScheme;

    type TestError = Box<dyn std::error::Error>;

    // --- `OAuthFlows` invariants --------------------------------------

    #[test]
    fn oauth_flows_has_any_is_false_for_default() {
        // A `OAuthFlows::default()` has no flow set. The
        // builder calls `has_any()` to reject this; the
        // test pins the helper's behaviour.
        let flows = OAuthFlows::default();
        assert!(!flows.has_any());
    }

    #[test]
    fn oauth_flows_has_any_is_true_when_authorization_code_set() -> Result<(), TestError> {
        let mut scopes = BTreeMap::new();
        scopes.insert(Arc::from("read"), Arc::from("Read access"));
        let flows = OAuthFlows {
            authorization_code: Some(AuthorizationCodeOAuthFlow {
                authorization_url: url::Url::parse("https://auth.example.com/authorize")?,
                token_url: url::Url::parse("https://auth.example.com/token")?,
                refresh_url: None,
                scopes: scopes.clone(),
                pkce_required: true,
            }),
            client_credentials: None,
            device_code: None,
        };
        assert!(flows.has_any());
        Ok(())
    }

    #[test]
    fn oauth_flows_serialises_with_camel_case_keys() -> Result<(), TestError> {
        // The wire form is OpenAPI's: `authorizationCode`,
        // `clientCredentials`, `deviceCode`. A typo in
        // the `rename_all` would compile but break the
        // wire form.
        let mut scopes = BTreeMap::new();
        scopes.insert(Arc::from("read"), Arc::from("Read access"));
        let flows = OAuthFlows {
            authorization_code: Some(AuthorizationCodeOAuthFlow {
                authorization_url: url::Url::parse("https://auth.example.com/authorize")?,
                token_url: url::Url::parse("https://auth.example.com/token")?,
                refresh_url: None,
                scopes: scopes.clone(),
                pkce_required: true,
            }),
            client_credentials: None,
            device_code: Some(DeviceCodeOAuthFlow {
                device_authorization_url: url::Url::parse("https://auth.example.com/device")?,
                token_url: url::Url::parse("https://auth.example.com/token")?,
                refresh_url: None,
                scopes,
            }),
        };
        let value = serde_json::to_value(&flows)?;
        let Some(obj) = value.as_object() else {
            return Err("OAuthFlows must serialise to a JSON object"
                .to_owned()
                .into());
        };
        assert!(obj.contains_key("authorizationCode"));
        assert!(!obj.contains_key("clientCredentials"));
        assert!(obj.contains_key("deviceCode"));
        Ok(())
    }

    // --- `OAuth2SecurityScheme` wire form ----------------------------

    #[test]
    fn oauth2_security_scheme_serialises_with_openapi_wire_form() -> Result<(), TestError> {
        // Wire form: `{"type": "oauth2", "flows": {...},
        // "oauth2MetadataUrl": "..."}`.
        let mut scopes = BTreeMap::new();
        scopes.insert(Arc::from("read"), Arc::from("Read access"));
        let scheme = SecurityScheme::OAuth2(Box::new(OAuth2SecurityScheme {
            description: Some(Arc::from("OAuth 2.0 with authorization code.")),
            flows: OAuthFlows {
                authorization_code: Some(AuthorizationCodeOAuthFlow {
                    authorization_url: url::Url::parse("https://auth.example.com/authorize")?,
                    token_url: url::Url::parse("https://auth.example.com/token")?,
                    refresh_url: None,
                    scopes: scopes.clone(),
                    pkce_required: true,
                }),
                client_credentials: None,
                device_code: None,
            },
            oauth2_metadata_url: Some(url::Url::parse(
                "https://auth.example.com/.well-known/oauth-authorization-server",
            )?),
        }));
        let value = serde_json::to_value(&scheme)?;
        let Some(obj) = value.as_object() else {
            return Err("a SecurityScheme must serialise to a JSON object"
                .to_owned()
                .into());
        };
        assert_eq!(
            obj.get("type").and_then(serde_json::Value::as_str),
            Some("oauth2")
        );
        let Some(flows_obj) = obj.get("flows").and_then(serde_json::Value::as_object) else {
            return Err("flows must be a JSON object".to_owned().into());
        };
        let Some(ac) = flows_obj
            .get("authorizationCode")
            .and_then(serde_json::Value::as_object)
        else {
            return Err("authorizationCode must be a JSON object".to_owned().into());
        };
        assert_eq!(
            ac.get("authorizationUrl")
                .and_then(serde_json::Value::as_str),
            Some("https://auth.example.com/authorize")
        );
        assert_eq!(
            ac.get("tokenUrl").and_then(serde_json::Value::as_str),
            Some("https://auth.example.com/token")
        );
        assert_eq!(
            ac.get("pkceRequired").and_then(serde_json::Value::as_bool),
            Some(true)
        );
        let Some(scopes_obj) = ac.get("scopes").and_then(serde_json::Value::as_object) else {
            return Err("scopes must be a JSON object".to_owned().into());
        };
        assert_eq!(
            scopes_obj.get("read").and_then(serde_json::Value::as_str),
            Some("Read access")
        );
        assert_eq!(
            obj.get("oauth2MetadataUrl")
                .and_then(serde_json::Value::as_str),
            Some("https://auth.example.com/.well-known/oauth-authorization-server")
        );
        Ok(())
    }

    #[test]
    fn oauth2_security_scheme_round_trips_through_serde() -> Result<(), TestError> {
        let mut scopes = BTreeMap::new();
        scopes.insert(Arc::from("read"), Arc::from("Read access"));
        scopes.insert(Arc::from("write"), Arc::from("Write access"));
        let scheme = SecurityScheme::OAuth2(Box::new(OAuth2SecurityScheme {
            description: None,
            flows: OAuthFlows {
                authorization_code: None,
                client_credentials: Some(ClientCredentialsOAuthFlow {
                    token_url: url::Url::parse("https://auth.example.com/token")?,
                    refresh_url: Some(url::Url::parse("https://auth.example.com/refresh")?),
                    scopes,
                }),
                device_code: None,
            },
            oauth2_metadata_url: None,
        }));
        let json = serde_json::to_string(&scheme)?;
        let parsed: SecurityScheme = serde_json::from_str(&json)?;
        assert_eq!(parsed, scheme);
        Ok(())
    }
}
