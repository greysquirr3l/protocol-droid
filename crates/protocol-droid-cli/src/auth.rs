//! Signing in to an OAuth-protected MCP server from the command line.
//!
//! `--oauth` turns on the library's OAuth 2.1 client. The first request that
//! comes back `401` opens the system browser at the authorization server, and
//! the redirect is received on a loopback port. A person is part of that
//! exchange, so this is opt-in: without the flag a `401` is an error.
//!
//! A client registered ahead of time is named through the environment, never
//! the command line, so a secret does not land in shell history or a process
//! listing: `PROTOCOL_DROID_OAUTH_CLIENT_ID` and, for a confidential client,
//! `PROTOCOL_DROID_OAUTH_CLIENT_SECRET`.

use std::sync::Arc;

use anyhow::Context as _;
use protocol_droid::adapters::secondary::oauth::LoopbackBrowser;
use protocol_droid::adapters::secondary::oauth::environment::HttpOAuthEnvironment;
use protocol_droid::adapters::secondary::oauth::secrets::OsSecrets;
use protocol_droid::application::client::bearer::BearerAuth;
use protocol_droid::application::oauth::authorizer::{OAuthAuthorizer, OAuthConfig, PreRegistered};
use protocol_droid::domain::ProtocolVersion;

/// The environment variable naming a pre-registered client id.
pub const CLIENT_ID_VARIABLE: &str = "PROTOCOL_DROID_OAUTH_CLIENT_ID";

/// The environment variable holding that client's secret, if it has one.
pub const CLIENT_SECRET_VARIABLE: &str = "PROTOCOL_DROID_OAUTH_CLIENT_SECRET";

/// An authorizer for the server at `server`, signing in through the browser.
///
/// # Errors
///
/// No loopback port could be bound, `server` is not a URL, or the HTTP client
/// could not be built.
pub async fn browser_authorizer(server: &str) -> anyhow::Result<Arc<dyn BearerAuth>> {
    let browser = LoopbackBrowser::bind()
        .await
        .context("could not listen for the sign-in redirect on a loopback port")?;
    let mut config = OAuthConfig::new(server, ProtocolVersion::latest_known())
        .with_context(|| format!("{server} is not a URL the sign-in can be built for"))?
        .redirect_to(browser.redirect_uri())
        .named("protocol-droid")
        .wanting_refresh_tokens();
    if let Ok(id) = std::env::var(CLIENT_ID_VARIABLE) {
        let secret = std::env::var(CLIENT_SECRET_VARIABLE).ok();
        config = config.with_pre_registered(PreRegistered::new(&id, secret.as_deref()));
    }
    let environment = HttpOAuthEnvironment::new(Arc::new(browser))
        .map_err(|reason| anyhow::anyhow!("could not build the HTTP client: {reason}"))?;
    Ok(OAuthAuthorizer::new(config, environment, OsSecrets))
}
