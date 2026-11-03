//! `protocol-droid utcp` — read a provider's manual, and call a tool.
//!
//! ## Two subcommands, one shape
//!
//! `utcp manual` reads a provider's manual and prints what it says.
//! `utcp call` reads the same manual, finds a tool in it, and makes the call
//! that tool describes. Neither asks a question: each does one thing and
//! exits, which is the half of the contract an agent needs. There is no
//! prompt and no REPL, because a subcommand that waits for a human is a
//! subcommand the other half of the audience cannot use.
//!
//! ## Two sinks, one serialiser
//!
//! As everywhere in this binary: the answer is built once, serialised once,
//! and `--json` picks stdout over stderr. The human form is a `Display` over
//! the same value, so the two can never disagree about what happened.
//!
//! ## Where the variables come from
//!
//! A manual writes a credential as `${API_KEY}` rather than as the secret,
//! so the call resolves those references against the environment. The
//! resolution happens in the library; this module only hands it the
//! environment's names and values.

use std::collections::BTreeMap;

use anyhow::Context as _;
use clap::{Args, Subcommand};
use protocol_droid::adapters::secondary::utcp::{CallOutcome, ManualClient, call as make_call};
use protocol_droid::application::utcp::call::{ToolCall, plan_call};
use protocol_droid::application::utcp::register::{Registration, register};
use protocol_droid::domain::ir::JsonObject;
use serde::Serialize;
use serde_json::Value;

/// The `utcp` subcommands.
#[derive(Debug, Args)]
pub struct UtcpArgs {
    /// What to do.
    #[command(subcommand)]
    pub command: UtcpCommand,
}

/// The things `utcp` can do.
#[derive(Debug, Subcommand)]
pub enum UtcpCommand {
    /// Read a provider's manual and list the tools in it.
    Manual(ManualArgs),
    /// Call a tool that a provider's manual describes.
    Call(CallArgs),
}

/// The arguments of `utcp manual`.
#[derive(Debug, Args)]
pub struct ManualArgs {
    /// The provider's base URL. The specification's `/utcp` path is appended
    /// to it unless the URL already ends with one.
    #[arg(long, value_name = "URL")]
    pub url: String,

    /// A protocol the manual's tools may be called over. Repeatable.
    ///
    /// The specification's `allowed_communication_protocols`. Without it,
    /// a manual read over HTTP may only have HTTP tools, so a provider
    /// whose tools are `mcp` or `cli` needs this said out loud. Each flag
    /// adds to what the manual's own protocol already permits; it does not
    /// replace it.
    #[arg(long = "allow-protocol", value_name = "PROTOCOL")]
    pub allow_protocol: Vec<String>,

    /// Print the manual as JSON on stdout instead of the summary on stderr.
    #[arg(long)]
    pub json: bool,
}

/// The arguments of `utcp call`.
#[derive(Debug, Args)]
pub struct CallArgs {
    /// The provider's base URL.
    #[arg(long, value_name = "URL")]
    pub url: String,

    /// The tool to call, named as the manual names it.
    #[arg(long, value_name = "NAME")]
    pub tool: String,

    /// The arguments, as a JSON object.
    #[arg(long, value_name = "JSON", default_value = "{}")]
    pub args: String,

    /// A protocol the manual's tools may be called over. Repeatable.
    ///
    /// The specification's `allowed_communication_protocols`. Without it,
    /// a manual read over HTTP may only have HTTP tools, so a provider
    /// whose tools are `mcp` or `cli` needs this said out loud. Each flag
    /// adds to what the manual's own protocol already permits; it does not
    /// replace it.
    #[arg(long = "allow-protocol", value_name = "PROTOCOL")]
    pub allow_protocol: Vec<String>,

    /// Print the result as JSON on stdout instead of the summary on stderr.
    #[arg(long)]
    pub json: bool,
}

/// One tool, as the summary reports it.
#[derive(Debug, Clone, Serialize)]
pub struct ToolSummary {
    /// The tool's name, as the manual names it.
    pub name: String,
    /// What the tool says it does.
    pub description: String,
    /// The transport the call goes over.
    pub transport: String,
    /// Where an HTTP call goes.
    pub url: Option<String>,
    /// The MCP servers the template names.
    pub servers: Vec<String>,
}

/// A tool the registration would not permit, as the summary reports it.
#[derive(Debug, Clone, Serialize)]
pub struct RefusedSummary {
    /// The tool's name.
    pub name: String,
    /// The protocol it wanted to be called over.
    pub protocol: String,
    /// Why it was refused.
    pub reason: String,
}

/// What a manual says, as one value with two renderings.
#[derive(Debug, Serialize)]
pub struct ManualSummary {
    /// The URL the manual was read from.
    pub url: String,
    /// The manual's own version.
    pub manual_version: String,
    /// The protocol version the manual was written against.
    pub utcp_version: String,
    /// The protocols the tools may be called over.
    pub allowed_protocols: Vec<String>,
    /// The tools it lists.
    pub tools: Vec<ToolSummary>,
    /// The tools the registration would not permit.
    pub refused: Vec<RefusedSummary>,
}

impl std::fmt::Display for ManualSummary {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            formatter,
            "{} (manual {}, protocol {})",
            self.url, self.manual_version, self.utcp_version
        )?;
        if self.tools.is_empty() {
            return writeln!(formatter, "  no tools");
        }
        for tool in &self.tools {
            writeln!(formatter, "  {} [{}]", tool.name, tool.transport)?;
            if !tool.description.is_empty() {
                writeln!(formatter, "    {}", tool.description)?;
            }
            if let Some(url) = &tool.url {
                writeln!(formatter, "    {url}")?;
            }
            for server in &tool.servers {
                writeln!(formatter, "    mcp server: {server}")?;
            }
        }
        for refused in &self.refused {
            writeln!(
                formatter,
                "  {} [{}] — not permitted: {}",
                refused.name, refused.protocol, refused.reason
            )?;
        }
        Ok(())
    }
}

/// What a call produced, as one value with two renderings.
#[derive(Debug, Serialize)]
pub struct CallSummary {
    /// The tool that was called.
    pub tool: String,
    /// The transport the call went over.
    pub transport: String,
    /// The status, for an HTTP call.
    pub status: Option<u16>,
    /// The result.
    pub result: Value,
}

impl std::fmt::Display for CallSummary {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(formatter, "{} [{}]", self.tool, self.transport)?;
        if let Some(status) = self.status {
            writeln!(formatter, "  status: {status}")?;
        }
        match &self.result {
            Value::String(text) => writeln!(formatter, "  {text}"),
            other => writeln!(
                formatter,
                "  {}",
                serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string())
            ),
        }
    }
}

/// Runs the `utcp` subcommand.
///
/// # Errors
///
/// [`anyhow::Error`] when the provider cannot be reached, the manual does
/// not carry the tool, or the call fails.
pub async fn run(args: &UtcpArgs) -> anyhow::Result<()> {
    match &args.command {
        UtcpCommand::Manual(manual) => {
            read_manual(&manual.url, &manual.allow_protocol, manual.json).await?;
        }
        UtcpCommand::Call(call) => {
            call_tool(
                &call.url,
                &call.tool,
                &call.args,
                &call.allow_protocol,
                call.json,
            )
            .await?;
        }
    }
    Ok(())
}

/// Reads a provider's manual and lists its tools.
///
/// The tools the registration would not permit are listed too, and say
/// why: a tool that silently does not appear is a tool the reader cannot
/// act on.
///
/// # Errors
///
/// [`anyhow::Error`] when the provider cannot be reached, answers with
/// something that is not a manual, or the manual does not parse.
pub async fn read_manual(
    url: &str,
    allow_protocol: &[String],
    json: bool,
) -> anyhow::Result<ManualSummary> {
    let client = client_for(url, allow_protocol)?;
    let manual = client
        .fetch()
        .await
        .context("the provider's manual could not be read")?;
    let registration = register(client.registration(), &manual);
    let summary = summarise(client.url(), &registration);
    emit(&summary, json)?;
    Ok(summary)
}

/// The client that reads a provider's manual.
///
/// The permitted protocols are the caller's to name, and they are the
/// caller's own answer to "which protocols may this manual's tools be
/// called over". A manual read over HTTP permits only HTTP tools unless
/// the caller says otherwise.
fn client_for(url: &str, allow_protocol: &[String]) -> anyhow::Result<ManualClient> {
    let client = ManualClient::new(url).context("the provider's URL could not be used")?;
    if allow_protocol.is_empty() {
        return Ok(client);
    }
    Ok(client.allowing_protocols(allow_protocol.iter().cloned()))
}

/// Calls a tool a provider's manual describes.
///
/// # Errors
///
/// [`anyhow::Error`] when the provider cannot be reached, the manual does
/// not carry the tool, the call cannot be planned, or the call fails.
pub async fn call_tool(
    url: &str,
    tool: &str,
    arguments: &str,
    allow_protocol: &[String],
    json: bool,
) -> anyhow::Result<CallSummary> {
    let client = client_for(url, allow_protocol)?;
    let manual = client
        .fetch()
        .await
        .context("the provider's manual could not be read")?;
    let registration = register(client.registration(), &manual);
    let Some(described) = registration
        .tools
        .iter()
        .find(|candidate| candidate.name == tool)
    else {
        let names = registration
            .tools
            .iter()
            .map(|candidate| candidate.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let refused = registration
            .refused
            .iter()
            .find(|candidate| candidate.name == tool)
            .map(|candidate| format!(" (`{tool}` is not permitted: {})", candidate.reason))
            .unwrap_or_default();
        anyhow::bail!("the manual has no tool called `{tool}`{refused}; it lists: {names}");
    };
    let arguments = JsonObject::new(arguments).context("the arguments are not a JSON object")?;
    let variables = environment();
    let plan = plan_call(described, &arguments, &variables, &registration.policy)
        .context("the call could not be planned")?;
    let transport = match &plan {
        ToolCall::Http(_) => "http",
        ToolCall::Mcp(_) => "mcp",
    };
    let outcome = make_call(&plan)
        .await
        .context("the call failed")?;
    let summary = CallSummary {
        tool: tool.to_owned(),
        transport: transport.to_owned(),
        status: match &outcome {
            CallOutcome::Http { status, .. } => Some(*status),
            CallOutcome::Mcp { .. } => None,
        },
        result: outcome.as_json(),
    };
    emit(&summary, json)?;
    Ok(summary)
}

/// The environment, as the variables a manual refers to.
fn environment() -> BTreeMap<String, String> {
    std::env::vars().collect()
}

/// The summary of a registered manual.
fn summarise(url: &str, registration: &Registration) -> ManualSummary {
    let tools = registration
        .tools
        .iter()
        .map(|tool| ToolSummary {
            name: tool.name.as_str().to_owned(),
            description: tool.description.clone(),
            transport: tool.tool_call_template.call_template_type().to_owned(),
            url: tool.tool_call_template.http_url(),
            servers: tool.tool_call_template.mcp_servers(),
        })
        .collect();
    let refused = registration
        .refused
        .iter()
        .map(|refused| RefusedSummary {
            name: refused.name.clone(),
            protocol: refused.protocol.clone(),
            reason: refused.reason.to_string(),
        })
        .collect();
    ManualSummary {
        url: url.to_owned(),
        manual_version: registration.manual_version.clone(),
        utcp_version: registration.utcp_version.clone(),
        allowed_protocols: registration.policy.allowed(),
        tools,
        refused,
    }
}

/// Sends the answer to one of the two sinks.
fn emit<T: Serialize + std::fmt::Display>(answer: &T, json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(answer)?);
    } else {
        eprint!("{answer}");
    }
    Ok(())
}
