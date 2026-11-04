//! Publishing this server's tools as a UTCP manual.
//!
//! # What a manual is for
//!
//! The UTCP specification says a provider publishes a manual at `GET /utcp`
//! and that the manual lists the provider's tools and says how to call
//! them. For a provider whose tools are MCP tools, "how to call them" is an
//! `mcp` call template naming the server — which is exactly what this crate
//! serves.
//!
//! So publishing is a translation: the registry's tools become the manual's
//! tools, and the server's own address becomes the template's `mcp`
//! configuration. The translation is a pure function of the tools and the
//! address, so it can be tested without a socket and recomputed whenever the
//! registry changes.
//!
//! # Why the tool names are prefixed
//!
//! The specification's MCP plugin registers a tool as `<server>.<tool>` and
//! splits it back that way to find the server to call. A manual published
//! this way follows the same convention, so a client that already knows how
//! to read an MCP plugin's manual reads this one the same way.
//!
//! # What the manual does not carry
//!
//! Nothing about the session, the negotiated revision, or the transport.
//! A manual is a document about tools; the negotiation stays where it
//! belongs, in the MCP session that a call actually opens.

use std::collections::BTreeMap;

use crate::domain::registry::Tool as RegistryTool;
use crate::domain::utcp::call_template::{
    CallTemplate, McpConfig, McpServer, McpTransport, Transport,
};
use crate::domain::utcp::{Manual, Tool, UtcpVersion};

/// The revision of the manual format this build publishes.
pub const MANUAL_VERSION: UtcpVersion = UtcpVersion::new(1, 0, 0);

/// The revision of the protocol this build publishes.
pub const UTCP_VERSION: UtcpVersion = UtcpVersion::new(1, 1, 0);

/// What to publish.
#[derive(Debug, Clone)]
pub struct PublishCommand {
    /// The tools to describe.
    pub tools: Vec<RegistryTool>,
    /// The name the template gives the server entry.
    ///
    /// The specification's MCP plugin names its server entries, and a client
    /// that splits `<server>.<tool>` uses this to find the server again.
    pub server_name: String,
    /// How to reach the server.
    pub server: McpServer,
}

/// Builds the manual for a set of tools.
///
/// # Examples
///
/// ```
/// use std::collections::BTreeMap;
///
/// use protocol_droid::application::utcp::publish::{PublishCommand, publish};
/// use protocol_droid::domain::registry::Tool as RegistryTool;
/// use protocol_droid::domain::utcp::McpServer;
///
/// let tool = RegistryTool::new("get_user", r#"{"type":"object"}"#)?;
/// let manual = publish(&PublishCommand {
///     tools: vec![tool],
///     server_name: "mcp".to_owned(),
///     server: McpServer::Http {
///         url: "https://api.example.com/mcp".to_owned(),
///         headers: BTreeMap::new(),
///         auth_token: None,
///     },
/// });
/// assert_eq!(manual.tools.len(), 1);
/// assert_eq!(manual.tools.first().map(|tool| tool.name.as_str()), Some("mcp.get_user"));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use]
pub fn publish(command: &PublishCommand) -> Manual {
    let template = template_for(&command.server_name, &command.server);
    let tools = command
        .tools
        .iter()
        .map(|tool| publish_tool(tool, &command.server_name, &template))
        .collect();
    Manual::new(MANUAL_VERSION, UTCP_VERSION, tools)
}

/// The call template every published tool carries.
fn template_for(server_name: &str, server: &McpServer) -> CallTemplate {
    let mut servers = BTreeMap::new();
    servers.insert(server_name.to_owned(), server.clone());
    CallTemplate {
        name: server_name.to_owned(),
        auth: None,
        allowed_communication_protocols: None,
        transport: Transport::Mcp(McpTransport {
            config: McpConfig { servers },
        }),
    }
}

/// The manual's entry for one registry tool.
fn publish_tool(tool: &RegistryTool, server_name: &str, template: &CallTemplate) -> Tool {
    Tool {
        name: format!("{server_name}.{}", tool.name.as_str()),
        description: tool.description.clone().unwrap_or_default(),
        inputs: tool.input_schema.clone(),
        outputs: tool.output_schema.clone(),
        tags: Vec::new(),
        average_response_size: None,
        tool_call_template: template.clone(),
    }
}
