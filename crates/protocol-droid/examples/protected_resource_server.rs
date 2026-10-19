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
//! # Why the two routers are separate
//!
//! `serve_protected_resource` builds its own [`Router`] and this example
//! merges it. The MCP router cannot serve the document itself, because the
//! document is reached *before* a client has a token — putting it behind the
//! authenticator would mean the document that explains how to authenticate is
//! itself unreachable.
//!
//! `http_server.rs` shows mounting an MCP router the same way; a full server
//! wires both together and binds one listener.

use axum::Router;
use protocol_droid::adapters::primary::transport::http::protected_resource::serve_protected_resource;
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

    let app: Router = Router::new().merge(serve_protected_resource(&metadata));

    // A full server merges an MCP router here and binds a listener:
    // `McpRouter::new(config, sessions, handler).into_router()?` for the MCP
    // routes, `axum::serve` for the socket. Building the app is enough here so
    // the example runs anywhere without claiming a port or needing a shutdown
    // signal.
    let _ = app;
    Ok(())
}
