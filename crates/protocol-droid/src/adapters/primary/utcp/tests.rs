//! Tests for the manual publisher.
//!
//! The claim being tested is the one a UTCP client depends on: the route
//! serves the manual, and the manual says how to call the tools it lists.

use crate::adapters::primary::utcp::{MANUAL_PATH, ManualPublisher};
use crate::application::utcp::publish::PublishCommand;
use crate::domain::registry::Tool as RegistryTool;
use crate::domain::utcp::McpServer;

/// Why a test failed.
#[derive(Debug)]
enum Failure {
    /// The registry refused the tool.
    Tool(crate::domain::ir::JsonObjectError),
    /// The server could not be started.
    Server(String),
    /// The request did not come back the way the test needed.
    Response(String),
}

impl From<crate::domain::ir::JsonObjectError> for Failure {
    fn from(error: crate::domain::ir::JsonObjectError) -> Self {
        Self::Tool(error)
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tool(error) => write!(formatter, "the tool was refused: {error}"),
            Self::Server(detail) => write!(formatter, "the server failed: {detail}"),
            Self::Response(detail) => write!(formatter, "the response was wrong: {detail}"),
        }
    }
}

impl std::error::Error for Failure {}

/// A publisher holding one tool.
fn publisher() -> Result<ManualPublisher, Failure> {
    let tool = RegistryTool::new("get_user", r#"{"type":"object"}"#)?;
    Ok(ManualPublisher::new(&PublishCommand {
        tools: vec![tool],
        server_name: "mcp".to_owned(),
        server: McpServer::Http {
            url: "https://api.example.com/mcp".to_owned(),
            headers: std::collections::BTreeMap::new(),
            auth_token: None,
        },
    }))
}

/// Starts the publisher's router on an ephemeral port and returns its URL.
async fn serve(publisher: ManualPublisher) -> Result<String, Failure> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|error| Failure::Server(error.to_string()))?;
    let address = listener
        .local_addr()
        .map_err(|error| Failure::Server(error.to_string()))?;
    let router = publisher.router();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok(format!("http://{address}"))
}

#[tokio::test]
async fn the_route_serves_the_manual() -> Result<(), Failure> {
    let publisher = publisher()?;
    let base = serve(publisher).await?;
    let response = reqwest::get(format!("{base}{MANUAL_PATH}"))
        .await
        .map_err(|error| Failure::Response(error.to_string()))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| Failure::Response(error.to_string()))?;
    if !status.is_success() {
        return Err(Failure::Response(format!("the status was {status}")));
    }
    let manual = crate::domain::utcp::Manual::parse(&body)
        .map_err(|error| Failure::Response(error.to_string()))?;
    let Some(tool) = manual.tool("mcp.get_user") else {
        return Err(Failure::Response(
            "the manual did not carry the tool".to_owned(),
        ));
    };
    if tool.tool_call_template.call_template_type() != "mcp" {
        return Err(Failure::Response(
            "the tool's call template was not an mcp one".to_owned(),
        ));
    }
    Ok(())
}

#[tokio::test]
async fn the_response_carries_the_security_headers() -> Result<(), Failure> {
    let publisher = publisher()?;
    let base = serve(publisher).await?;
    let response = reqwest::get(format!("{base}{MANUAL_PATH}"))
        .await
        .map_err(|error| Failure::Response(error.to_string()))?;
    for header in [
        "content-security-policy",
        "strict-transport-security",
        "x-frame-options",
    ] {
        if response.headers().get(header).is_none() {
            return Err(Failure::Response(format!("the response had no `{header}`")));
        }
    }
    Ok(())
}

#[tokio::test]
async fn publishing_again_swaps_the_manual() -> Result<(), Failure> {
    let publisher = publisher()?;
    let second = RegistryTool::new("list_users", r#"{"type":"object"}"#)?;
    publisher.publish(&PublishCommand {
        tools: vec![second],
        server_name: "mcp".to_owned(),
        server: McpServer::Http {
            url: "https://api.example.com/mcp".to_owned(),
            headers: std::collections::BTreeMap::new(),
            auth_token: None,
        },
    });
    let base = serve(publisher).await?;
    let response = reqwest::get(format!("{base}{MANUAL_PATH}"))
        .await
        .map_err(|error| Failure::Response(error.to_string()))?;
    let body = response
        .text()
        .await
        .map_err(|error| Failure::Response(error.to_string()))?;
    let manual = crate::domain::utcp::Manual::parse(&body)
        .map_err(|error| Failure::Response(error.to_string()))?;
    if manual.tool("mcp.list_users").is_none() {
        return Err(Failure::Response(
            "the manual did not carry the new tool".to_owned(),
        ));
    }
    Ok(())
}
