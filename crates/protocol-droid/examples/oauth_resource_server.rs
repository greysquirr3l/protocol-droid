//! A resource server that requires OAuth, end to end.
//!
//! This file is the single source for the assembly shown in
//! `docs/guide/src/security.md`. The guide includes it with `{{#include}}`
//! rather than restating it, because a hand-copied snippet drifts: this one is
//! compiled by `cargo check --all-targets --all-features` on every commit, so a
//! renamed import or a changed signature fails the build instead of failing in
//! a reader's terminal.
//!
//! Run it with:
//!
//! ```text
//! cargo run --example oauth_resource_server --features http,oauth,random-ids
//! ```
//!
//! It binds nothing. The point is the *shape* of the setup — the two builders
//! that have to agree — so it stops after constructing the server and prints
//! the challenge a `401` would carry.

use std::sync::Arc;

use jsonwebtoken::Algorithm;
use protocol_droid::adapters::primary::transport::http::{
    AuthPolicy, HttpBuilder, OAuthAuthenticator, ServerConfig, SystemClock,
};
use protocol_droid::adapters::secondary::oauth::jwt::JwtSource;
use protocol_droid::application::oauth::NoScopes;
use protocol_droid::application::server::Server;
use protocol_droid::domain::ProtocolVersion;
use protocol_droid::domain::mcp::{ResourceMetadata, ResourceUri};
use protocol_droid::domain::registry::auth::{Scope, ScopeSet};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Where tokens presented to this server are verified against. A real
    // deployment points this at its own JWKS; `IntrospectionSource` is the
    // opaque-token alternative.
    let source = JwtSource::new(
        Arc::from("https://login.example.com/.well-known/jwks.json"),
        vec![Algorithm::RS256],
    );

    // RFC 9728's protected-resource metadata: which resource this is, and who
    // may issue tokens for it. One value serves both purposes below, so the
    // document that is served and the challenge that points at it cannot name
    // different URLs.
    let metadata = ResourceMetadata::new(ResourceUri::new("https://mcp.example/mcp")?)
        .with_authorization_server("https://login.example.com")
        .with_scope("mcp:read");

    // Read before the metadata moves into the config. This is the exact string
    // a `401` carries in `WWW-Authenticate`, verbatim.
    match metadata.challenge() {
        Ok(challenge) => println!("a 401 would carry: {challenge}"),
        Err(error) => println!("no challenge: {error}"),
    }

    // ANCHOR: assembly
    // Half one: what validates a token presented *to* this server. The audience
    // is bound to `metadata`, which is what stops a token minted for a
    // different resource being replayed here.
    let authenticator = Arc::new(OAuthAuthenticator::new(
        source,
        metadata.clone(),
        ProtocolVersion::V2025_06_18,
        NoScopes,
        SystemClock,
    ));

    // Half two: what says the endpoint needs one at all. These are separate
    // because "nothing is configured yet" and "this endpoint is deliberately
    // public" are different states, and conflating them is how a default
    // becomes a decision nobody made.
    //
    // Wiring an authenticator without a policy is the silent fail-open it looks
    // like, so `into_router` refuses to build the route table instead of
    // serving the endpoint unprotected.
    let policy = AuthPolicy::Require(ScopeSet::empty().with(Scope::Read));

    let config = ServerConfig::new()?.with_resource_metadata(metadata);

    // `Server` is what a `#[server]` declaration produces through
    // `.into_server()`; building one directly here keeps the example runnable
    // without a full tool declaration.
    let server = Server::builder("mcp").build();

    // `.serve().await` binds the address and starts listening. This example
    // stops short of that so it exits and prints instead of hanging.
    let _builder = HttpBuilder::new(server)
        .config(config)
        .with_authenticator(authenticator)
        .with_policy(policy);
    // ANCHOR_END: assembly

    Ok(())
}
