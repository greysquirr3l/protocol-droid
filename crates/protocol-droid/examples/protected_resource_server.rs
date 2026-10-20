//! Serves an RFC 9728 protected-resource metadata document beside an MCP
//! endpoint, so a client that receives a `401` can find out where to get a
//! token.
//!
//! This is the smallest deployment that makes the OAuth discovery step work:
//! one well-known route merged into the app, and the MCP router mounted beside
//! it. Run it and ask for the document:
//!
//! ```text
//! cargo run --example protected_resource_server --features http
//! curl -i http://127.0.0.1:3000/.well-known/oauth-protected-resource/mcp
//! ```
//!
//! The `resource` field in the response is the audience a token must carry, so
//! it has to match the resource identifier the client asks for. That is why
//! the document is served at `/.well-known/oauth-protected-resource/mcp`
//! rather than at the origin root: two resources on one host each get their
//! own document without the two colliding.
//!
//! # No merge, and no second router
//!
//! `ServerConfig::with_resource_metadata` mounts the document on the MCP router
//! and points the `401` challenge at the same URL from the same value. There
//! is nothing to merge and nothing to keep in step.
//!
//! The document is reached before a client has a token, so the route
//! deliberately skips the authenticator — the escape route cannot require the
//! credential it explains how to obtain. It still passes the same `Host` and
//! rate-limit checks as every other route on the server.
//!
//! `serve_protected_resource` remains for serving the document from a different
//! origin or port than the MCP endpoint; this example does not need it.

use protocol_droid::adapters::primary::transport::http::ServerConfig;
use protocol_droid::domain::mcp::{ResourceMetadata, ResourceUri};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The canonical resource identifier. Everything else follows from it: the
    // path this document is served at, and the audience a presented token must
    // carry.
    let resource = ResourceUri::new("http://127.0.0.1:3000/mcp")?;

    let metadata = ResourceMetadata::new(resource.clone())
        .with_authorization_server("https://login.example.com")
        .with_scope("files:read")
        .with_name("Example MCP server");

    eprintln!(
        "protected_resource_server: metadata at /{}",
        metadata.resource().metadata_path()
    );
    eprintln!(
        "protected_resource_server: resource is {}",
        resource.as_str()
    );

    // This is the whole setup. The MCP router mounts the document at
    // `metadata_path()` and a `401` carries a challenge naming that URL, both
    // read from this one value.
    let config = ServerConfig::new()?.with_resource_metadata(metadata);
    eprintln!(
        "protected_resource_server: challenge {:?}",
        config.challenge()
    );
    let _ = config;
    Ok(())
}
