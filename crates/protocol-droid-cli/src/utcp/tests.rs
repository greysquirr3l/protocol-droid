//! Tests for the `utcp` subcommands.
//!
//! The provider is the library's own manual, served over loopback HTTP by
//! the test. That is the path a user takes — read the manual at `GET /utcp`,
//! list the tools, call one — and the only part of it faked is the socket.
//! A test that mocked the manual client would prove the summary code runs,
//! not that the subcommand reaches a provider.
//!
//! The CLI's library target is dispatched into by the binary, which the
//! library's own test build cannot see. A `pub` item that no test reaches
//! is therefore reported as dead, and that report is the reason these
//! tests exist rather than a side effect of them.

use std::collections::BTreeMap;

use axum::Router;
use axum::routing::get;
use clap::{Parser, Subcommand};
use protocol_droid::domain::utcp::{
    CallTemplate, CliCommand, CliTransport, HttpMethod, Manual, Tool, Transport, UtcpVersion,
};
use serde_json::{Value, json};

use super::{CallArgs, ManualArgs, UtcpArgs, UtcpCommand, call_tool, read_manual, run};

/// Why a test failed.
#[derive(Debug)]
enum Failure {
    /// Something was not what the test said it should be.
    Message(String),
}

impl core::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Message(text) => formatter.write_str(text),
        }
    }
}

impl std::error::Error for Failure {}

impl From<String> for Failure {
    fn from(text: String) -> Self {
        Self::Message(text)
    }
}

impl From<anyhow::Error> for Failure {
    fn from(error: anyhow::Error) -> Self {
        Self::Message(error.to_string())
    }
}

/// The whole error, chain and all.
///
/// The binary exits with `anyhow::Result`, and the `Debug` of that error —
/// which is what a user sees — carries the chain. A test that read only
/// the top context would miss a cause the user does see, so the tests read
/// the same thing the user reads.
fn chain(error: &anyhow::Error) -> String {
    format!("{error:#}")
}

/// The manual a provider publishes.
///
/// The versions are the ones the specification names, so the document the
/// client parses is the document a real provider would publish.
fn manual(tools: Vec<Tool>) -> Manual {
    Manual::new(UtcpVersion::new(1, 0, 0), UtcpVersion::new(1, 1, 0), tools)
}

/// A tool called over HTTP.
fn http_tool(base: &str, name: &str) -> Tool {
    Tool::new(
        name,
        CallTemplate::http("provider", &format!("{base}/tool"), HttpMethod::Get),
    )
    .described("Echoes what it is given.")
}

/// A tool called by running a program.
///
/// The `cli` transport is modelled so a document naming one round-trips.
/// This build refuses to *call* it, which is the point of the refusal
/// test: a provider that names a program gets the program refused.
fn cli_tool(name: &str) -> Tool {
    Tool::new(
        name,
        CallTemplate {
            name: "provider".to_owned(),
            auth: None,
            allowed_communication_protocols: None,
            transport: Transport::Cli(CliTransport {
                commands: vec![CliCommand {
                    command: "echo".to_owned(),
                    append_to_final_output: true,
                }],
                env_vars: BTreeMap::new(),
                inherit_env_vars: false,
                working_dir: None,
            }),
        },
    )
}

/// A provider on loopback: the manual at `/utcp`, and the tool at `/tool`.
///
/// The listener is bound before the manual is built, because the manual
/// names the tool's URL and the tool's URL is the address the listener
/// got. The tool echoes its query string, so a test can see that the
/// arguments reached the endpoint rather than only that a call was made.
async fn serve(tools: impl FnOnce(&str) -> Vec<Tool>) -> Result<String, Failure> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|error| Failure::Message(format!("the listener did not bind: {error}")))?;
    let address = listener
        .local_addr()
        .map_err(|error| Failure::Message(format!("the address was not readable: {error}")))?;
    let base = format!("http://{address}");
    let manual = manual(tools(&base)).to_value();
    let router = Router::new()
        .route(
            "/utcp",
            get(move || {
                let value = manual.clone();
                async move { axum::Json(value) }
            }),
        )
        .route(
            "/tool",
            get(|request: axum::extract::Request| async move {
                let query = request.uri().query().unwrap_or_default().to_owned();
                axum::Json(json!({ "query": query }))
            }),
        );
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok(base)
}

/// The names of a summary's tools.
fn names(tools: &[super::ToolSummary]) -> Vec<&str> {
    tools.iter().map(|tool| tool.name.as_str()).collect()
}

/// The args parse the way the binary parses them.
///
/// The binary dispatches into this library, so the parse has to be the
/// same parse. A flag the library does not know is a flag the user cannot
/// pass.
#[test]
fn the_args_round_trip_through_clap() -> Result<(), Failure> {
    #[derive(Debug, Parser)]
    struct Wrapper {
        #[command(subcommand)]
        command: WrapperCmd,
    }

    #[derive(Debug, Subcommand)]
    enum WrapperCmd {
        Utcp(UtcpArgs),
    }

    let parsed = Wrapper::try_parse_from([
        "protocol-droid",
        "utcp",
        "manual",
        "--url",
        "http://127.0.0.1:9999",
        "--json",
        "--allow-protocol",
        "mcp",
    ]);
    let args = match parsed {
        Ok(Wrapper {
            command: WrapperCmd::Utcp(args),
        }) => args,
        Err(error) => return Err(Failure::Message(format!("clap parse failed: {error}"))),
    };
    let UtcpCommand::Manual(manual) = &args.command else {
        return Err(Failure::Message(format!(
            "expected manual, got {:?}",
            args.command
        )));
    };
    if manual.url != "http://127.0.0.1:9999" {
        return Err(Failure::Message(format!("url was {:?}", manual.url)));
    }
    if !manual.json {
        return Err(Failure::Message("--json was not set".to_owned()));
    }
    if manual.allow_protocol != ["mcp".to_owned()] {
        return Err(Failure::Message(format!(
            "allow-protocol was {:?}",
            manual.allow_protocol
        )));
    }

    let parsed = Wrapper::try_parse_from([
        "protocol-droid",
        "utcp",
        "call",
        "--url",
        "http://127.0.0.1:9999",
        "--tool",
        "echo",
        "--args",
        r#"{"message":"hello"}"#,
    ]);
    let args = match parsed {
        Ok(Wrapper {
            command: WrapperCmd::Utcp(args),
        }) => args,
        Err(error) => return Err(Failure::Message(format!("clap parse failed: {error}"))),
    };
    let UtcpCommand::Call(call) = &args.command else {
        return Err(Failure::Message(format!(
            "expected call, got {:?}",
            args.command
        )));
    };
    if call.tool != "echo" {
        return Err(Failure::Message(format!("tool was {:?}", call.tool)));
    }
    if call.args != r#"{"message":"hello"}"# {
        return Err(Failure::Message(format!("args were {:?}", call.args)));
    }
    if call.json {
        return Err(Failure::Message(
            "--json was set without being asked for".to_owned(),
        ));
    }
    Ok(())
}

/// The arguments the parser produces, read straight off the struct.
///
/// The parse test above goes through clap. This one goes through the
/// struct, because the struct is the contract the rest of the crate is
/// written against: a field the parser fills and the dispatcher reads.
#[test]
fn the_args_carry_what_the_dispatcher_reads() -> Result<(), Failure> {
    let args = UtcpArgs {
        command: UtcpCommand::Manual(ManualArgs {
            url: "http://127.0.0.1:9999".to_owned(),
            allow_protocol: vec!["mcp".to_owned()],
            json: true,
        }),
    };
    let UtcpCommand::Manual(manual) = &args.command else {
        return Err(Failure::Message("expected manual".to_owned()));
    };
    if manual.url != "http://127.0.0.1:9999" {
        return Err(Failure::Message(format!("url was {:?}", manual.url)));
    }

    let args = UtcpArgs {
        command: UtcpCommand::Call(CallArgs {
            url: "http://127.0.0.1:9999".to_owned(),
            tool: "echo".to_owned(),
            args: "{}".to_owned(),
            allow_protocol: Vec::new(),
            json: false,
        }),
    };
    let UtcpCommand::Call(call) = &args.command else {
        return Err(Failure::Message("expected call".to_owned()));
    };
    if call.tool != "echo" {
        return Err(Failure::Message(format!("tool was {:?}", call.tool)));
    }
    Ok(())
}

/// A manual read lists the tools the registration permits.
///
/// The registration a manual is read under permits HTTP and nothing else,
/// so an HTTP tool is listed and a tool of another protocol is not.
#[tokio::test]
async fn a_manual_read_lists_the_tools_the_registration_permits() -> Result<(), Failure> {
    let base = serve(|base| vec![http_tool(base, "echo")]).await?;
    let summary = read_manual(&base, &[], true).await?;
    if names(&summary.tools) != ["echo"] {
        return Err(Failure::Message(format!(
            "tools were {:?}",
            names(&summary.tools)
        )));
    }
    if !summary.refused.is_empty() {
        return Err(Failure::Message(format!(
            "refused was {:?}",
            summary.refused
        )));
    }
    if summary.allowed_protocols != ["http".to_owned()] {
        return Err(Failure::Message(format!(
            "permitted protocols were {:?}",
            summary.allowed_protocols
        )));
    }
    if summary.manual_version != "1.0.0" || summary.utcp_version != "1.1.0" {
        return Err(Failure::Message(format!(
            "versions were {} and {}",
            summary.manual_version, summary.utcp_version
        )));
    }
    Ok(())
}

/// A tool the registration would not permit is reported, not dropped.
///
/// A tool that silently does not appear is a tool the reader cannot act
/// on, and cannot tell apart from one that was never offered.
#[tokio::test]
async fn a_refused_tool_is_reported_with_its_reason() -> Result<(), Failure> {
    let base = serve(|base| vec![http_tool(base, "echo"), cli_tool("shell")]).await?;
    let summary = read_manual(&base, &[], true).await?;
    if names(&summary.tools) != ["echo"] {
        return Err(Failure::Message(format!(
            "tools were {:?}",
            names(&summary.tools)
        )));
    }
    let Some(refused) = summary.refused.first() else {
        return Err(Failure::Message("nothing was refused".to_owned()));
    };
    if refused.name != "shell" {
        return Err(Failure::Message(format!(
            "refused name was {:?}",
            refused.name
        )));
    }
    if refused.protocol != "cli" {
        return Err(Failure::Message(format!(
            "refused protocol was {:?}",
            refused.protocol
        )));
    }
    if refused.reason.is_empty() {
        return Err(Failure::Message("the refusal had no reason".to_owned()));
    }
    Ok(())
}

/// A call reaches the endpoint the template names, with the arguments.
///
/// The tool echoes its query string, so the assertion is that the
/// argument arrived — not merely that a request was made and answered.
#[tokio::test]
async fn a_call_reaches_the_endpoint_the_template_names() -> Result<(), Failure> {
    let base = serve(|base| vec![http_tool(base, "echo")]).await?;
    let summary = call_tool(&base, "echo", r#"{"message":"hello"}"#, &[], true).await?;
    if summary.tool != "echo" {
        return Err(Failure::Message(format!("tool was {:?}", summary.tool)));
    }
    if summary.transport != "http" {
        return Err(Failure::Message(format!(
            "transport was {:?}",
            summary.transport
        )));
    }
    if summary.status != Some(200) {
        return Err(Failure::Message(format!("status was {:?}", summary.status)));
    }
    let Value::Object(received) = &summary.result else {
        return Err(Failure::Message(format!("result was {}", summary.result)));
    };
    if received.get("query").and_then(Value::as_str) != Some("message=hello") {
        return Err(Failure::Message(format!("result was {}", summary.result)));
    }
    Ok(())
}

/// A call to a tool the registration refused says why.
///
/// The message has to name the tool, because the user asked for a tool by
/// name and a refusal that does not name it is not an answer.
#[tokio::test]
async fn a_call_to_a_refused_tool_names_the_tool_and_the_reason() -> Result<(), Failure> {
    let base = serve(|base| vec![http_tool(base, "echo"), cli_tool("shell")]).await?;
    let error = match call_tool(&base, "shell", "{}", &[], true).await {
        Ok(_) => return Err(Failure::Message("the refused tool was called".to_owned())),
        Err(error) => chain(&error),
    };
    if !error.contains("shell") {
        return Err(Failure::Message(format!(
            "the error did not name the tool: {error}"
        )));
    }
    if !error.contains("not permitted") {
        return Err(Failure::Message(format!(
            "the error gave no reason: {error}"
        )));
    }
    Ok(())
}

/// The dispatcher runs the manual read the args name.
///
/// `run` is what the binary calls, so it is the function a user reaches.
/// A parse that succeeds and a dispatch that goes somewhere else is a
/// subcommand that does the wrong thing quietly.
#[tokio::test]
async fn the_dispatcher_runs_a_manual_read() -> Result<(), Failure> {
    let base = serve(|base| vec![http_tool(base, "echo")]).await?;
    let args = UtcpArgs {
        command: UtcpCommand::Manual(ManualArgs {
            url: base,
            allow_protocol: Vec::new(),
            json: true,
        }),
    };
    run(&args).await?;
    Ok(())
}

/// The dispatcher runs the call the args name.
#[tokio::test]
async fn the_dispatcher_runs_a_call() -> Result<(), Failure> {
    let base = serve(|base| vec![http_tool(base, "echo")]).await?;
    let args = UtcpArgs {
        command: UtcpCommand::Call(CallArgs {
            url: base,
            tool: "echo".to_owned(),
            args: r#"{"message":"hello"}"#.to_owned(),
            allow_protocol: Vec::new(),
            json: true,
        }),
    };
    run(&args).await?;
    Ok(())
}

/// A tool the manual does not have is refused by name.
///
/// The error lists what the manual does have, so a user who guessed the
/// name knows what to try instead.
#[tokio::test]
async fn a_call_to_a_tool_the_manual_does_not_have_lists_what_it_does() -> Result<(), Failure> {
    let base = serve(|base| vec![http_tool(base, "echo")]).await?;
    let error = match call_tool(&base, "nope", "{}", &[], true).await {
        Ok(_) => return Err(Failure::Message("the absent tool was called".to_owned())),
        Err(error) => chain(&error),
    };
    if !error.contains("nope") {
        return Err(Failure::Message(format!(
            "the error did not name the tool: {error}"
        )));
    }
    if !error.contains("echo") {
        return Err(Failure::Message(format!(
            "the error did not list what the manual has: {error}"
        )));
    }
    Ok(())
}

/// A provider reached over plain HTTP, off loopback, is refused.
///
/// The rule is that a document that names what to run, or where to send a
/// credential, must not arrive over a channel that can be read on the way.
#[tokio::test]
async fn a_plain_http_remote_provider_is_refused() -> Result<(), Failure> {
    let error = match read_manual("http://api.example.com", &[], true).await {
        Ok(_) => return Err(Failure::Message("the remote provider was read".to_owned())),
        Err(error) => chain(&error),
    };
    if !error.contains("plain HTTP") {
        return Err(Failure::Message(format!(
            "the error did not say why: {error}"
        )));
    }
    Ok(())
}

/// The CLI reads the manual the library publishes.
///
/// The library can serve its own tools as a manual, and the CLI can read a
/// provider's manual. Nothing in the crate says the two halves agree on
/// the wire — and a publisher that emits a document the reader cannot
/// parse is exactly the kind of defect that survives a test suite, because
/// each half passes against its own fixture. This test serves the
/// publisher's own output and reads it back.
#[tokio::test]
async fn the_cli_reads_the_manual_the_library_publishes() -> Result<(), Failure> {
    use protocol_droid::adapters::primary::utcp::ManualPublisher;
    use protocol_droid::application::utcp::publish::PublishCommand;
    use protocol_droid::domain::registry::Tool as RegistryTool;
    use protocol_droid::domain::utcp::McpServer;

    let tool = RegistryTool::new("get_user", r#"{"type":"object"}"#)
        .map_err(|error| Failure::Message(format!("the tool did not build: {error}")))?;
    let publisher = ManualPublisher::new(&PublishCommand {
        tools: vec![tool],
        server_name: "mcp".to_owned(),
        server: McpServer::Http {
            url: "https://api.example.com/mcp".to_owned(),
            headers: BTreeMap::new(),
            auth_token: None,
        },
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|error| Failure::Message(format!("the listener did not bind: {error}")))?;
    let address = listener
        .local_addr()
        .map_err(|error| Failure::Message(format!("the address was not readable: {error}")))?;
    let router = publisher.router();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    // The published tool is an `mcp` tool, and the registration a manual
    // read over HTTP carries permits `http` alone. So the read refuses it
    // — which is the permission working, not a failure to read.
    let base = format!("http://{address}");
    let summary = read_manual(&base, &[], true).await?;
    if !summary.tools.is_empty() {
        return Err(Failure::Message(format!(
            "an mcp tool was permitted by an http registration: {:?}",
            summary.tools
        )));
    }
    let Some(refused) = summary.refused.first() else {
        return Err(Failure::Message(
            "the published tool was not refused".to_owned(),
        ));
    };
    if refused.name != "mcp.get_user" {
        return Err(Failure::Message(format!(
            "refused name was {:?}",
            refused.name
        )));
    }
    if refused.protocol != "mcp" {
        return Err(Failure::Message(format!(
            "refused protocol was {:?}",
            refused.protocol
        )));
    }

    // Naming the protocol permits it, and then the tool is listed with the
    // server it is reached through.
    let summary = read_manual(&base, &["mcp".to_owned()], true).await?;
    if names(&summary.tools) != ["mcp.get_user"] {
        return Err(Failure::Message(format!(
            "tools were {:?}",
            names(&summary.tools)
        )));
    }
    if summary.tools.first().map(|tool| tool.servers.as_slice())
        != Some(["mcp".to_owned()].as_slice())
    {
        return Err(Failure::Message(format!(
            "servers were {:?}",
            summary.tools.first().map(|tool| tool.servers.clone())
        )));
    }
    Ok(())
}

/// The JSON form and the human form are two renderings of one value.
///
/// The project's rule for every subcommand: one serialiser, two sinks.
/// The JSON on stdout is the same bytes whatever `--json` says, and the
/// human form on stderr is a rendering of the same value, so the two can
/// never disagree about what happened.
#[test]
fn the_two_forms_are_one_value() -> Result<(), Failure> {
    let summary = super::ManualSummary {
        url: "http://127.0.0.1:9999/utcp".to_owned(),
        manual_version: "1.0.0".to_owned(),
        utcp_version: "1.1.0".to_owned(),
        allowed_protocols: vec!["http".to_owned()],
        tools: vec![super::ToolSummary {
            name: "echo".to_owned(),
            description: "Echoes what it is given.".to_owned(),
            transport: "http".to_owned(),
            url: Some("http://127.0.0.1:9999/tool".to_owned()),
            servers: Vec::new(),
        }],
        refused: Vec::new(),
    };
    let once = serde_json::to_string_pretty(&summary)
        .map_err(|error| Failure::Message(format!("serialise: {error}")))?;
    let twice = serde_json::to_string_pretty(&summary)
        .map_err(|error| Failure::Message(format!("serialise: {error}")))?;
    if once != twice {
        return Err(Failure::Message(
            "two serialisations produced different bytes".to_owned(),
        ));
    }
    let parsed: Value =
        serde_json::from_str(&once).map_err(|error| Failure::Message(format!("parse: {error}")))?;
    if parsed.get("tools").and_then(Value::as_array).map(Vec::len) != Some(1) {
        return Err(Failure::Message(format!("the JSON was {once}")));
    }
    let human = summary.to_string();
    if !human.contains("echo") || !human.contains("http") {
        return Err(Failure::Message(format!("the human form was {human}")));
    }
    Ok(())
}
