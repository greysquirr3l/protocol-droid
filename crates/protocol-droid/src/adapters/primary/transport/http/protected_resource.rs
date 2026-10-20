//! HTTP serving for the RFC 9728 protected-resource metadata document.
//!
//! RFC 9728 §3.1 requires a resource server to publish its metadata at a
//! `GET` URL formed by inserting `/.well-known/oauth-protected-resource`
//! between the host and the path of its resource identifier. A server at the
//! origin root publishes at `/.well-known/oauth-protected-resource`; a server
//! mounted at `/mcp` publishes at `/.well-known/oauth-protected-resource/mcp`.
//! [`ResourceUri::metadata_path`](crate::domain::mcp::ResourceUri::metadata_path)
//! computes that path, so the route mounts
//! wherever the configured resource identifier says it belongs rather than at
//! a hard-coded guess.
//!
//! Without this route a client that received a `401` has no way to learn which
//! authorization servers can issue a token for this server — which is the whole
//! discovery step. A deployment can be correctly configured and still be
//! unreachable in practice.
//!
//! # How it is mounted
//!
//! [`ServerConfig::with_resource_metadata`](crate::adapters::primary::transport::http::ServerConfig::with_resource_metadata)
//! mounts the document on the MCP
//! router, from the same value that makes the `401` challenge name it. A
//! consumer configuring a server does nothing else, and the two halves cannot
//! drift.
//!
//! The router in this module is for what that cannot cover: a document served
//! from a different origin or port than the MCP endpoint, or one routed by a
//! proxy that sends `/.well-known/` elsewhere.
//!
//! # Why the route is mounted inside the MCP router despite needing no credential
//!
//! The document is fetched precisely *because* the client could not
//! authenticate, so [`serve_document`](super::router) skips the authenticator
//! and the policy. It still passes `enforce_origin`, `enforce_host` and the
//! rate limiter — the same checks every MCP request on that server faces, and
//! no more. `OriginPolicy::check` treats an absent `Origin` as the non-browser
//! client it is, and `check_host` demands only the `Host` a public deployment
//! already had to configure for its MCP requests to be answered at all.
//!
//! # Usually you do not call this
//!
//! # The router this builds merges into a consumer's own axum app:
//!
//! ```no_run
//! use axum::Router;
//! use protocol_droid::adapters::primary::transport::http::protected_resource::serve_protected_resource;
//! use protocol_droid::domain::mcp::{ResourceMetadata, ResourceUri};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let metadata = ResourceMetadata::new(ResourceUri::new("https://example.com/mcp")?)
//!     .with_authorization_server("https://login.example.com");
//!
//! let app: Router = Router::new().merge(serve_protected_resource(&metadata));
//! # let _ = app;
//! # Ok(())
//! # }
//! ```
//!
//! # Why this is not a route on the MCP router
//!
//! The metadata URL is *not* the MCP endpoint. It sits under `/.well-known/`,
//! it is reached before any token exists, and putting it behind the
//! authenticator would mean the client cannot read the document that tells it
//! how to authenticate. It also cannot live on the MCP endpoint, which is
//! POST-only.
//!
//! The route is nevertheless mounted on the MCP router rather than left to a
//! separate one, because the layers it then passes through are exactly the ones
//! it already faces as an MCP endpoint — and keeping the document beside its
//! challenge is what stops the two naming different URLs.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::domain::mcp::ResourceMetadata;

/// The content type RFC 9728 §3.2 requires of a metadata response.
const CONTENT_TYPE: &str = "application/json";

/// Returns an axum [`Router`] serving `metadata` at its well-known path.
///
/// Most consumers want [`with_resource_metadata`](crate::adapters::primary::transport::http::ServerConfig::with_resource_metadata)
/// instead, which mounts the same document on the MCP router itself. Reach for
/// this when the document has to be served from somewhere the MCP router is
/// not.
///
/// The route is `GET` only. A `POST` to the path gets axum's `405 Method Not
/// Allowed`, which is correct: §3.1 specifies a `GET` query, and the document
/// is read-only by construction — it describes this server rather than
/// changing it.
///
/// The document is held in an `Arc` so the router is cheap to clone, and it is
/// serialised per request rather than once at mount.
///
/// # Examples
///
/// ```
/// use protocol_droid::adapters::primary::transport::http::protected_resource::serve_protected_resource;
/// use protocol_droid::domain::mcp::{ResourceMetadata, ResourceUri};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// // The router mounts at the path the resource identifier implies.
/// let root = ResourceMetadata::new(ResourceUri::new("https://example.com")?);
/// assert_eq!(root.resource().metadata_path(), "/.well-known/oauth-protected-resource");
///
/// let mounted = ResourceMetadata::new(ResourceUri::new("https://example.com/mcp")?);
/// assert_eq!(
///     mounted.resource().metadata_path(),
///     "/.well-known/oauth-protected-resource/mcp",
/// );
///
/// // Both build without touching the filesystem or opening a socket.
/// let _router = serve_protected_resource(&root);
/// let _also = serve_protected_resource(&mounted);
/// # Ok(())
/// # }
/// ```
pub fn serve_protected_resource(metadata: &ResourceMetadata) -> Router {
    Router::new()
        .route(&metadata.resource().metadata_path(), get(get_metadata))
        .with_state(Arc::new(metadata.clone()))
}

async fn get_metadata(State(metadata): State<Arc<ResourceMetadata>>) -> Response {
    // A document that fails to serialise is a bug in the derive rather than in
    // the caller's configuration — every field is a string, an `Arc<str>` or a
    // `Vec` of those, so there is nothing a caller can put here that breaks it.
    // The client gets a bodiless 500, because the wire contract is the metadata
    // document and a serialisation error is not part of it. A consumer who
    // wants to see it in production can wrap the router in a logging layer.
    let Ok(body) = serde_json::to_vec(&*metadata) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    (StatusCode::OK, [(header::CONTENT_TYPE, CONTENT_TYPE)], body).into_response()
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use tower::ServiceExt;

    use super::{ResourceMetadata, serve_protected_resource};
    use crate::domain::mcp::ResourceUri;

    /// The fallible calls answer unrelated error types, so the tests return a
    /// `Result` over this rather than unwrapping. Each variant names its step,
    /// because "test failed" does not say which one.
    #[derive(Debug)]
    enum Failure {
        /// A literal in a test was not a legal resource identifier.
        Uri(crate::domain::mcp::ResourceUriError),
        /// The test fixture or the request builder did not hold.
        Fixture(&'static str),
    }

    impl core::fmt::Display for Failure {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::Uri(e) => write!(f, "not a legal resource identifier: {e}"),
                Self::Fixture(what) => write!(f, "fixture did not hold: {what}"),
            }
        }
    }

    impl std::error::Error for Failure {}

    fn mounted() -> Result<ResourceMetadata, Failure> {
        Ok(ResourceMetadata::new(
            ResourceUri::new("https://example.com/mcp").map_err(Failure::Uri)?,
        )
        .with_authorization_server("https://login.example.com")
        .with_scope("files:read"))
    }

    fn request(method: &str, uri: &str) -> Result<Request<Body>, Failure> {
        Request::builder()
            .method(method)
            .uri(uri)
            .body(Body::empty())
            .map_err(|_| Failure::Fixture("request builder rejected the method or uri"))
    }

    async fn send(
        metadata: &ResourceMetadata,
        request: Request<Body>,
    ) -> Result<axum::response::Response, Failure> {
        serve_protected_resource(metadata)
            .oneshot(request)
            .await
            .map_err(|_| Failure::Fixture("the router did not answer"))
    }

    #[tokio::test]
    async fn serves_the_document_with_the_content_type_the_rfc_requires() -> Result<(), Failure> {
        let metadata = mounted()?;
        let response = send(
            &metadata,
            request("GET", "/.well-known/oauth-protected-resource/mcp")?,
        )
        .await?;
        assert_eq!(response.status(), StatusCode::OK);

        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok());
        assert_eq!(content_type, Some("application/json"));

        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .map_err(|_| Failure::Fixture("could not read the response body"))?;
        let document: serde_json::Value = serde_json::from_slice(&body)
            .map_err(|_| Failure::Fixture("the response body was not JSON"))?;

        // `get` rather than indexing throughout: a wrong shape then fails the
        // assertion instead of panicking inside the test.
        assert_eq!(
            document.get("resource"),
            Some(&serde_json::json!("https://example.com/mcp"))
        );
        assert_eq!(
            document
                .get("authorization_servers")
                .and_then(serde_json::Value::as_array)
                .and_then(|values| values.first()),
            Some(&serde_json::json!("https://login.example.com"))
        );
        assert_eq!(
            document
                .get("scopes_supported")
                .and_then(serde_json::Value::as_array)
                .and_then(|values| values.first()),
            Some(&serde_json::json!("files:read"))
        );
        assert_eq!(
            document
                .get("bearer_methods_supported")
                .and_then(serde_json::Value::as_array)
                .and_then(|values| values.first()),
            Some(&serde_json::json!("header"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_post_is_refused_rather_than_answered() -> Result<(), Failure> {
        let metadata = mounted()?;
        let response = send(
            &metadata,
            request("POST", "/.well-known/oauth-protected-resource/mcp")?,
        )
        .await?;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        Ok(())
    }

    #[tokio::test]
    async fn a_root_resource_publishes_at_the_root_form() -> Result<(), Failure> {
        let metadata =
            ResourceMetadata::new(ResourceUri::new("https://example.com").map_err(Failure::Uri)?);
        let response = send(
            &metadata,
            request("GET", "/.well-known/oauth-protected-resource")?,
        )
        .await?;
        assert_eq!(response.status(), StatusCode::OK);
        Ok(())
    }

    #[tokio::test]
    async fn the_root_form_is_not_served_by_a_server_mounted_below_it() -> Result<(), Failure> {
        // A server at `/mcp` does not also answer at the root form: that
        // document would name a different resource, and publishing it is how a
        // client ends up holding a token for the wrong audience.
        let metadata = mounted()?;
        let response = send(
            &metadata,
            request("GET", "/.well-known/oauth-protected-resource")?,
        )
        .await?;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        Ok(())
    }

    #[tokio::test]
    async fn the_router_is_clonable_and_stays_mounted_where_it_was() -> Result<(), Failure> {
        // Cloning is how a consumer merges this alongside its own routes, and
        // the mount path has to survive it — a clone that mounts elsewhere
        // would silently 404 the discovery document.
        let metadata = mounted()?;
        let router = serve_protected_resource(&metadata);
        let clone = router.clone();
        let original = request("GET", "/.well-known/oauth-protected-resource/mcp")?;
        assert_eq!(send(&metadata, original).await?.status(), StatusCode::OK);

        // The clone answers the same request, which is the observable half of
        // "it stayed mounted".
        let cloned_response = clone
            .oneshot(request("GET", "/.well-known/oauth-protected-resource/mcp")?)
            .await
            .map_err(|_| Failure::Fixture("the cloned router did not answer"))?;
        assert_eq!(cloned_response.status(), StatusCode::OK);
        Ok(())
    }
}
