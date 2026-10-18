//! Where an MCP client looks for metadata, in the order it must look.
//!
//! Two lists and one identity rule, all from the published discovery page:
//! <https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization/authorization-server-discovery>
//!
//! - Protected resource metadata: the `resource_metadata` URL from the
//!   challenge when there is one; otherwise the well-known URI at the MCP
//!   endpoint's path, then at the root.
//! - Authorization server metadata, for an issuer with a path: OAuth with path
//!   insertion, `OpenID` Connect with path insertion, `OpenID` Connect with path
//!   appending; without a path: OAuth, then `OpenID` Connect.
//! - The canonical resource URI of the server, sent as RFC 8707 `resource`:
//!   no fragment, and no trailing slash unless it is the whole path.

use url::Url;

/// The well-known URIs to try for a server's protected resource metadata.
///
/// For `https://example.com/public/mcp`: the path form
/// `…/.well-known/oauth-protected-resource/public/mcp`, then the root form.
/// A server at the root has only the root form.
#[must_use]
pub fn protected_resource_metadata_urls(server: &Url) -> Vec<String> {
    let mut urls = Vec::with_capacity(2);
    let path = server.path().trim_end_matches('/');
    if !path.is_empty() {
        urls.push(format!(
            "{}/.well-known/oauth-protected-resource{path}",
            origin(server)
        ));
    }
    urls.push(format!("{}/.well-known/oauth-protected-resource", origin(server)));
    urls
}

/// The well-known URIs to try for an issuer's metadata, in priority order.
#[must_use]
pub fn authorization_server_metadata_urls(issuer: &Url) -> Vec<String> {
    let base = origin(issuer);
    let path = issuer.path().trim_end_matches('/');
    if path.is_empty() {
        return vec![
            format!("{base}/.well-known/oauth-authorization-server"),
            format!("{base}/.well-known/openid-configuration"),
        ];
    }
    vec![
        format!("{base}/.well-known/oauth-authorization-server{path}"),
        format!("{base}/.well-known/openid-configuration{path}"),
        format!("{base}{path}/.well-known/openid-configuration"),
    ]
}

/// The canonical URI of an MCP server, as RFC 8707 `resource`.
///
/// Lowercase scheme and host (the `url` crate already does that), no fragment,
/// no query, and no trailing slash on a non-root path.
#[must_use]
pub fn canonical_resource(server: &Url) -> String {
    let path = server.path();
    let path = if path == "/" { "" } else { path.trim_end_matches('/') };
    format!("{}{path}", origin(server))
}

/// `scheme://host[:port]`, with the port only when it is not the default.
fn origin(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default();
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    url.port().map_or_else(
        || format!("{}://{host}", url.scheme()),
        |port| format!("{}://{host}:{port}", url.scheme()),
    )
}

#[cfg(test)]
mod tests {
    use url::Url;

    use super::{
        authorization_server_metadata_urls, canonical_resource, protected_resource_metadata_urls,
    };

    type TestResult = Result<(), url::ParseError>;

    #[test]
    fn resource_metadata_tries_the_path_then_the_root() -> TestResult {
        assert_eq!(
            protected_resource_metadata_urls(&Url::parse("https://example.com/public/mcp")?),
            [
                "https://example.com/.well-known/oauth-protected-resource/public/mcp",
                "https://example.com/.well-known/oauth-protected-resource",
            ]
        );
        assert_eq!(
            protected_resource_metadata_urls(&Url::parse("https://example.com")?),
            ["https://example.com/.well-known/oauth-protected-resource"]
        );
        Ok(())
    }

    #[test]
    fn an_issuer_with_a_path_tries_three_endpoints_in_order() -> TestResult {
        assert_eq!(
            authorization_server_metadata_urls(&Url::parse("https://auth.example.com/tenant1")?),
            [
                "https://auth.example.com/.well-known/oauth-authorization-server/tenant1",
                "https://auth.example.com/.well-known/openid-configuration/tenant1",
                "https://auth.example.com/tenant1/.well-known/openid-configuration",
            ]
        );
        Ok(())
    }

    #[test]
    fn an_issuer_without_a_path_tries_two() -> TestResult {
        assert_eq!(
            authorization_server_metadata_urls(&Url::parse("http://localhost:9000")?),
            [
                "http://localhost:9000/.well-known/oauth-authorization-server",
                "http://localhost:9000/.well-known/openid-configuration",
            ]
        );
        Ok(())
    }

    #[test]
    fn the_canonical_resource_drops_slash_query_and_fragment() -> TestResult {
        assert_eq!(
            canonical_resource(&Url::parse("https://MCP.example.com/mcp/?a=1#f")?),
            "https://mcp.example.com/mcp"
        );
        assert_eq!(
            canonical_resource(&Url::parse("https://mcp.example.com/")?),
            "https://mcp.example.com"
        );
        assert_eq!(
            canonical_resource(&Url::parse("http://127.0.0.1:8443/mcp")?),
            "http://127.0.0.1:8443/mcp"
        );
        Ok(())
    }
}
