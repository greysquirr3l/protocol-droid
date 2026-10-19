//! The two metadata documents a client reads before it can authorize.
//!
//! Only the members MCP's flow acts on are read; everything else in either
//! document is ignored rather than rejected, because both are extensible.
//!
//! - Protected resource metadata (RFC 9728): `resource`, `authorization_servers`,
//!   `scopes_supported`.
//! - Authorization server metadata (RFC 8414 / `OpenID` Connect Discovery):
//!   the endpoints, the advertised capabilities the flow branches on, and
//!   `issuer`, which "MUST be identical to the issuer identifier used to
//!   construct the well-known URL".
//!   <https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization/authorization-server-discovery>

use serde_json::Value;

/// Why a metadata document could not be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MetadataError {
    /// The body is not a JSON object.
    #[error("the {document} is not a JSON object")]
    NotAnObject {
        /// Which document.
        document: &'static str,
    },
    /// A required member is missing or not a string.
    #[error("the {document} has no usable `{member}`")]
    Missing {
        /// Which document.
        document: &'static str,
        /// Which member.
        member: &'static str,
    },
    /// The document's `issuer` is not the issuer it was fetched for.
    ///
    /// The mix-up defence: a document served from one issuer's well-known URL
    /// that claims to be another's must not be used.
    #[error("the metadata names issuer `{claimed}`, but it was fetched for `{expected}`")]
    IssuerMismatch {
        /// The issuer the URL was built from.
        expected: String,
        /// The issuer the document claims.
        claimed: String,
    },
}

/// What a client reads from protected resource metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedResource {
    /// The resource identifier the document describes.
    pub resource: String,
    /// The authorization servers, in the order listed.
    pub authorization_servers: Vec<String>,
    /// The scopes the resource lists, if it lists any.
    pub scopes_supported: Option<Vec<String>>,
}

impl ProtectedResource {
    /// Parses an RFC 9728 document.
    ///
    /// # Errors
    ///
    /// [`MetadataError`] for a body that is not an object, or that lacks
    /// `resource` or a non-empty `authorization_servers`.
    pub fn parse(body: &[u8]) -> Result<Self, MetadataError> {
        const DOCUMENT: &str = "protected resource metadata";
        let value: Value = serde_json::from_slice(body)
            .map_err(|_| MetadataError::NotAnObject { document: DOCUMENT })?;
        let object = value
            .as_object()
            .ok_or(MetadataError::NotAnObject { document: DOCUMENT })?;
        let resource = object
            .get("resource")
            .and_then(Value::as_str)
            .ok_or(MetadataError::Missing {
                document: DOCUMENT,
                member: "resource",
            })?
            .to_owned();
        let authorization_servers: Vec<String> = strings(object.get("authorization_servers"));
        if authorization_servers.is_empty() {
            return Err(MetadataError::Missing {
                document: DOCUMENT,
                member: "authorization_servers",
            });
        }
        let scopes_supported = object
            .get("scopes_supported")
            .filter(|value| value.is_array())
            .map(|value| strings(Some(value)));
        Ok(Self {
            resource,
            authorization_servers,
            scopes_supported,
        })
    }
}

/// What a client reads from authorization server metadata.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AuthorizationServer {
    /// The issuer identifier, verified against the one the URL was built for.
    pub issuer: String,
    /// Where the user is sent.
    pub authorization_endpoint: String,
    /// Where a code is redeemed.
    pub token_endpoint: String,
    /// Where RFC 7591 registration is offered, if it is.
    pub registration_endpoint: Option<String>,
    /// The scopes the server lists.
    pub scopes_supported: Vec<String>,
    /// The PKCE methods the server lists; `None` when the member is absent.
    pub code_challenge_methods_supported: Option<Vec<String>>,
    /// The token endpoint authentication methods the server lists.
    pub token_endpoint_auth_methods_supported: Vec<String>,
    /// Whether a URL-shaped `client_id` (a metadata document) is accepted.
    pub client_id_metadata_document_supported: bool,
    /// Whether the server includes `iss` in authorization responses.
    pub authorization_response_iss_parameter_supported: bool,
}

impl AuthorizationServer {
    /// Parses a metadata document fetched for `expected_issuer`.
    ///
    /// # Errors
    ///
    /// [`MetadataError::IssuerMismatch`] when the document claims a different
    /// issuer — compared by simple string equality, as RFC 8414 §3.3 asks —
    /// and [`MetadataError::Missing`] when an endpoint the flow needs is absent.
    pub fn parse(body: &[u8], expected_issuer: &str) -> Result<Self, MetadataError> {
        const DOCUMENT: &str = "authorization server metadata";
        let value: Value = serde_json::from_slice(body)
            .map_err(|_| MetadataError::NotAnObject { document: DOCUMENT })?;
        let object = value
            .as_object()
            .ok_or(MetadataError::NotAnObject { document: DOCUMENT })?;
        let text = |member: &'static str| {
            object
                .get(member)
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .ok_or(MetadataError::Missing {
                    document: DOCUMENT,
                    member,
                })
        };
        let issuer = text("issuer")?;
        if issuer != expected_issuer {
            return Err(MetadataError::IssuerMismatch {
                expected: expected_issuer.to_owned(),
                claimed: issuer,
            });
        }
        Ok(Self {
            issuer,
            authorization_endpoint: text("authorization_endpoint")?,
            token_endpoint: text("token_endpoint")?,
            registration_endpoint: text("registration_endpoint").ok(),
            scopes_supported: strings(object.get("scopes_supported")),
            code_challenge_methods_supported: object
                .get("code_challenge_methods_supported")
                .filter(|value| value.is_array())
                .map(|value| strings(Some(value))),
            token_endpoint_auth_methods_supported: strings(
                object.get("token_endpoint_auth_methods_supported"),
            ),
            client_id_metadata_document_supported: object
                .get("client_id_metadata_document_supported")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            authorization_response_iss_parameter_supported: object
                .get("authorization_response_iss_parameter_supported")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{AuthorizationServer, MetadataError, ProtectedResource};

    #[test]
    fn protected_resource_metadata_reads_what_the_flow_needs() -> Result<(), MetadataError> {
        let document = ProtectedResource::parse(
            br#"{"resource":"https://mcp.example.com/mcp","authorization_servers":["https://auth.example.com"],"scopes_supported":["a","b"]}"#,
        )?;
        assert_eq!(document.authorization_servers, ["https://auth.example.com"]);
        assert_eq!(
            document.scopes_supported,
            Some(vec!["a".to_owned(), "b".to_owned()])
        );
        Ok(())
    }

    #[test]
    fn absent_scopes_supported_is_none_not_empty() -> Result<(), MetadataError> {
        let document = ProtectedResource::parse(
            br#"{"resource":"https://mcp.example.com","authorization_servers":["https://auth.example.com"]}"#,
        )?;
        assert_eq!(document.scopes_supported, None);
        Ok(())
    }

    #[test]
    fn metadata_for_another_issuer_is_refused() {
        let outcome = AuthorizationServer::parse(
            br#"{"issuer":"https://honest.example","authorization_endpoint":"a","token_endpoint":"t"}"#,
            "https://attacker.example",
        );
        assert!(matches!(outcome, Err(MetadataError::IssuerMismatch { .. })));
    }

    #[test]
    fn the_issuer_is_compared_exactly() {
        let outcome = AuthorizationServer::parse(
            br#"{"issuer":"https://auth.example.com/","authorization_endpoint":"a","token_endpoint":"t"}"#,
            "https://auth.example.com",
        );
        assert!(outcome.is_err(), "a trailing slash is a different issuer");
    }
}
