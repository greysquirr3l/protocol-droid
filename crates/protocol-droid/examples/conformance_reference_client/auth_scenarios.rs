//! The OAuth scenarios: a client that meets a `401` and has to get a token.
//!
//! All of them drive the same flow, `OAuthAuthorizer`, against a mock
//! authorization server and a mock MCP server that demands a bearer token. They
//! differ in what the mocks say — where the metadata lives, which scopes the
//! challenge names, whether the issuer matches — and in whether the right
//! answer is a token or a refusal.

use std::sync::Arc;

use protocol_droid::adapters::secondary::oauth::LoopbackBrowser;
use protocol_droid::adapters::secondary::oauth::environment::{
    FollowsRedirect, HttpOAuthEnvironment, UserAgent,
};
use protocol_droid::adapters::secondary::oauth::secrets::OsSecrets;
use protocol_droid::application::client::handler::ConnectOptions;
use protocol_droid::application::oauth::authorizer::{OAuthAuthorizer, OAuthConfig, PreRegistered};
use protocol_droid::domain::ir::JsonObject;
use protocol_droid::domain::ProtocolVersion;
use protocol_droid::serde_json::{self, Value, json};

type Failure = Box<dyn std::error::Error>;

/// The metadata document URL the suite's mocks expect to see as a `client_id`.
const CLIENT_METADATA_URL: &str = "https://conformance-test.local/client-metadata.json";

/// Scenarios whose correct outcome is that no token is obtained.
const REFUSALS: &[&str] = &[
    "auth/resource-mismatch",
    "auth/iss-supported-missing",
    "auth/iss-wrong-issuer",
    "auth/iss-unexpected",
    "auth/iss-normalized",
    "auth/metadata-issuer-mismatch",
];

/// Whether `scenario` belongs here.
pub fn handles(scenario: &str) -> bool {
    scenario.starts_with("auth/")
}

fn revision() -> ProtocolVersion {
    std::env::var("MCP_CONFORMANCE_PROTOCOL_VERSION")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(ProtocolVersion::V2026_07_28)
}

/// Runs one OAuth scenario.
pub async fn run(scenario: &str, url: &str) -> Result<(), Failure> {
    let outcome = flow(scenario, url).await;
    if REFUSALS.contains(&scenario) {
        // The mock judges what the client sent; a client that refused to go
        // through with the flow did what the scenario asks.
        return Ok(());
    }
    outcome
}

async fn flow(scenario: &str, url: &str) -> Result<(), Failure> {
    // `PD_CONFORMANCE_BROWSER=1` receives the redirect the way a person's
    // browser would, on a loopback port, instead of reading it off the
    // authorization response. The mock approves on its own, so the "browser" is
    // an HTTP client that follows the redirect to that port.
    let browser = if std::env::var_os("PD_CONFORMANCE_BROWSER").is_some() {
        Some(LoopbackBrowser::bind().await?)
    } else {
        None
    };
    let mut config = OAuthConfig::new(url, revision())?
        .named("protocol-droid conformance client")
        .with_metadata_document(CLIENT_METADATA_URL)
        .wanting_refresh_tokens();
    if let Some(browser) = &browser {
        config = config.redirect_to(browser.redirect_uri());
    }
    if scenario == "auth/pre-registration" {
        let context: Value = serde_json::from_str(&std::env::var("MCP_CONFORMANCE_CONTEXT")?)?;
        let field = |name: &str| {
            context
                .get(name)
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .ok_or_else(|| format!("the context has no `{name}`"))
        };
        config = config.with_pre_registered(
            PreRegistered::new(&field("client_id")?, Some(&field("client_secret")?))
                .issued_by(field("issuer")?),
        );
    }
    let agent: Arc<dyn UserAgent> = match browser {
        Some(browser) => Arc::new(browser.opening_with(|authorization_url| {
            let target = authorization_url.to_owned();
            tokio::spawn(async move {
                let _ = reqwest::get(target).await;
            });
            Ok(())
        })),
        None => Arc::new(FollowsRedirect::new()?),
    };
    let environment = HttpOAuthEnvironment::new(agent)?;
    let authorizer = OAuthAuthorizer::new(config, environment, OsSecrets);

    // The revision decides the lifecycle: the suite's mocks at `2026-07-28`
    // are stateless and refuse an `initialize`.
    let client = super::scenarios::connect(url, ConnectOptions::new().authorizing(authorizer)).await?;

    // Unchecked: the mocks advertise nothing, and what is judged is whether the
    // client got a token — which only a request the server answers can show.
    client.send_unchecked("tools/list", &JsonObject::default()).await?;
    if scenario != "auth/pre-registration" {
        let call = JsonObject::from_value(json!({ "name": "test-tool", "arguments": {} }))?;
        // The scope scenarios expect a second operation, which may need more
        // than the first; a refusal of the call itself is the mock's to judge.
        let _ = client.send_unchecked("tools/call", &call).await;
    }
    Ok(())
}
