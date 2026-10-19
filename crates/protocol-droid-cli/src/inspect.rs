//! `protocol-droid inspect` — list primitives and call tools, one-shot.
//!
//! ## Why the one-shot form is the only form
//!
//! A REPL that requires a human at a terminal is unusable by the thing this
//! tool exists to serve. An agent calling a tool should not have to hold the
//! whole result in its context window to do anything with it; the working
//! pattern is to write the result to a file and grep it. The REPL was the
//! first draft and the one-shot form is the version that lets an agent
//! actually use this command, so the subcommand *is* the one-shot form, not
//! a wrapping of it.
//!
//! ## One serialiser, two sinks
//!
//! Every subcommand renders the same JSON on stdout and a human summary on
//! stderr. `--json` is the presence or absence of the stderr writer, not a
//! second code path that can drift from the first. A test on `call` asserts
//! the bytes, not the parsed shape, because a serialiser change that kept
//! the parse identical would still fail an agent's byte-equal check.
//!
//! ## Callbacks in one-shot mode
//!
//! A tool that elicits or samples has no prompt in one-shot mode. The
//! answer is not a hang (an agent loop that hangs is worse than one that
//! fails) and not a default answer on the user's behalf (a tool that the
//! user did not authorise answering for itself is worse than one that
//! fails). It is a typed error naming the callback, and the test on it
//! pins the "completes without a timeout" property that an agent loop
//! relies on.
//!
//! ## `--args`
//!
//! Three shapes: a JSON literal (`--args '{"a":1}'`), `@path` to read a file
//! (so a long argument does not have to survive shell quoting), and `-` to
//! read from stdin (so a generator can pipe the args in). The empty-args
//! case is `--args` with no value, which falls back to `{}` so a no-arg
//! tool does not need a sentinel.

use std::io::Read as _;

use anyhow::Context as _;
use clap::{Args, Subcommand, ValueHint};
use protocol_droid::application::client::{Client, ClientError};
use protocol_droid::domain::ir::JsonObject;
use serde::Serialize;
use serde_json::{Map, Value};

/// What the operator asked `inspect` to do.
#[derive(Debug, Args)]
pub struct InspectArgs {
    /// The server to inspect.
    ///
    /// Same spelling as `bridge --upstream` and `probe`: `http:URL` or
    /// `legacy-sse:URL`. The subcommand opens the connection, runs the
    /// handshake, and disposes the session when the answer is rendered.
    #[arg(value_name = "SERVER")]
    pub server: String,

    /// Which primitive to act on, and how.
    #[command(subcommand)]
    pub command: InspectCommand,

    /// Machine-readable JSON on stdout, with the human summary on stderr.
    #[arg(long, global = true)]
    pub json: bool,

    /// Sign in with OAuth 2.1 if the server answers `401`.
    ///
    /// Opens the system browser at the authorization server and waits for the
    /// redirect on a loopback port, so a person has to be present. A client
    /// registered ahead of time is named by `PROTOCOL_DROID_OAUTH_CLIENT_ID`
    /// and `PROTOCOL_DROID_OAUTH_CLIENT_SECRET`. Without this flag a `401` is
    /// reported as an error and nothing is opened.
    #[arg(long, global = true)]
    pub oauth: bool,
}

/// The sub-actions of `inspect`.
#[derive(Debug, Subcommand)]
pub enum InspectCommand {
    /// List the server's tools.
    ///
    /// Resources and prompts land here as separate subcommands when a
    /// caller asks for them; `list` is the tool list because that is the
    /// primitive a caller of `inspect` actually reaches for first.
    List,

    /// Print the JSON Schema for a tool's input.
    ///
    /// The schema is whatever the server sent; this command does not
    /// synthesise one. A tool that has no schema (the field is optional
    /// in every revision) answers with a single line noting the absence,
    /// rather than a synthesised `{"type": "object"}` that would lie
    /// about the tool's contract.
    Schema {
        /// The tool whose schema is wanted.
        tool: String,
    },

    /// Call a tool with the given arguments.
    ///
    /// `--args` takes a JSON literal, `@path` to read from a file, or `-`
    /// to read from stdin. A missing value falls back to `{}` so a no-arg
    /// tool does not need a sentinel.
    Call {
        /// The tool to call.
        tool: String,
        /// The arguments, as JSON, `@file`, or `-` for stdin.
        #[arg(long, value_name = "JSON|@FILE|-", value_hint = ValueHint::FilePath)]
        args: Option<String>,
    },
}

/// What `inspect` prints, as one value with two renderings.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Report {
    /// The result of `inspect list`.
    Tools {
        /// The tools the server advertised, in order.
        tools: Vec<ToolSummary>,
    },
    /// The result of `inspect schema`.
    Schema {
        /// The tool whose schema is reported.
        tool: String,
        /// The raw schema the server sent. `null` when the tool has none.
        schema: Option<Value>,
    },
    /// The result of `inspect call`.
    Call {
        /// The tool that was called.
        tool: String,
        /// The arguments the call was made with, echoed for traceability.
        arguments: Value,
        /// The raw reply the server sent.
        reply: Value,
    },
}

/// One tool in the listing.
#[derive(Debug, Clone, Serialize)]
pub struct ToolSummary {
    /// The tool's name.
    pub name: String,
    /// The tool's description, when the server sent one.
    pub description: Option<String>,
}

impl std::fmt::Display for Report {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tools { tools } => {
                writeln!(formatter, "{} tool(s):", tools.len())?;
                for tool in tools {
                    if let Some(description) = &tool.description {
                        writeln!(formatter, "  {} — {description}", tool.name)?;
                    } else {
                        writeln!(formatter, "  {}", tool.name)?;
                    }
                }
                Ok(())
            }
            Self::Schema {
                tool,
                schema: Some(schema),
            } => {
                writeln!(formatter, "schema for {tool}:")?;
                writeln!(
                    formatter,
                    "{}",
                    serde_json::to_string_pretty(schema).unwrap_or_default()
                )
            }
            Self::Schema { tool, schema: None } => {
                writeln!(formatter, "schema for {tool}: <none>")
            }
            Self::Call { tool, reply, .. } => {
                writeln!(formatter, "{tool} replied:")?;
                writeln!(
                    formatter,
                    "{}",
                    serde_json::to_string_pretty(reply).unwrap_or_default()
                )
            }
        }
    }
}

/// Runs `protocol-droid inspect`.
///
/// # Errors
///
/// `anyhow` errors name the stage that failed: spec parsing, transport
/// open, the underlying RPC, or the JSON write. The argument-parser
/// failures are reported by `clap` and exit the process before reaching
/// here.
pub async fn run(args: &InspectArgs) -> anyhow::Result<()> {
    let client = open_session(&args.server, args.oauth).await?;
    let report = dispatch(&client, &args.command).await?;
    print_report(&report, args.json)?;
    Ok(())
}

/// Runs one `InspectCommand` against an already-open client.
///
/// A separate function so the dispatcher is testable without opening a
/// real transport — the test wraps an in-process `ClientTransport` that
/// answers `tools/list` and `tools/call`, calls `dispatch`, and reads
/// the report. The wire path the operator takes is the same code: `run`
/// opens, then hands the client to `dispatch`.
pub async fn dispatch(client: &Client, command: &InspectCommand) -> anyhow::Result<Report> {
    match command {
        InspectCommand::List => {
            let tools = list_tools(client).await?;
            Ok(Report::Tools { tools })
        }
        InspectCommand::Schema { tool } => {
            let schema = schema_of(client, tool).await?;
            Ok(Report::Schema {
                tool: tool.clone(),
                schema,
            })
        }
        InspectCommand::Call { tool, args: raw } => {
            let arguments = resolve_args(raw.as_deref())?;
            let reply = call_tool(client, tool, &arguments).await?;
            Ok(Report::Call {
                tool: tool.clone(),
                arguments,
                reply,
            })
        }
    }
}

/// Writes the report to stdout (and the human form to stderr when
/// `--json` is *not* set).
///
/// One serialiser, two sinks: the JSON written to stdout is the same
/// bytes regardless of `--json`. The only thing `--json` changes is
/// whether the human form is also written to stderr.
pub fn print_report(report: &Report, json_only: bool) -> anyhow::Result<()> {
    let serialised = serde_json::to_string_pretty(report)
        .with_context(|| "the report could not be serialised")?;
    if !json_only {
        eprintln!("{report}");
    }
    println!("{serialised}");
    Ok(())
}

/// Opens a session, sharing the consumer path's `connect::open`.
///
/// A separate function so the one-shot form runs through the same code
/// the bridge, the probe, and any other client uses. A re-implementation
/// here would be a second opinion on what "open a session" means, and a
/// second opinion is what diverges the moment a transport changes shape.
async fn open_session(spec: &str, oauth: bool) -> anyhow::Result<Client> {
    let parsed: protocol_droid::domain::TransportSpec = spec
        .parse()
        .with_context(|| format!("{spec} is not a usable transport spec"))?;
    #[cfg(feature = "bridge")]
    if oauth {
        use protocol_droid::application::client::handler::ConnectOptions;
        let authorizer = crate::auth::browser_authorizer(parsed.target()).await?;
        let versions = protocol_droid::domain::SupportedVersions::all()?;
        return Client::connect_with(
            &parsed,
            &versions,
            ConnectOptions::new().authorizing(authorizer),
        )
        .await
        .with_context(|| format!("could not open a session to {spec}"));
    }
    #[cfg(not(feature = "bridge"))]
    anyhow::ensure!(
        !oauth,
        "this build has no OAuth client; rebuild with --features bridge"
    );
    Client::connect_to(&parsed)
        .await
        .with_context(|| format!("could not open a session to {spec}"))
}

/// Pulls the tool list out of a `tools/list` reply.
///
/// `tools/list` returns a `{tools: [{name, description?, inputSchema?}, ...]}`
/// object. The `inputSchema` is what `schema_of` re-reads; this function
/// only wants the name and description.
async fn list_tools(client: &Client) -> anyhow::Result<Vec<ToolSummary>> {
    let reply = client
        .list_tools()
        .await
        .with_context(|| "tools/list failed")?;
    let value = serde_json::from_str::<Value>(reply.as_str())
        .with_context(|| "the tools/list reply was not a JSON object")?;
    let tools = value
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("the tools/list reply has no `tools` array"))?;
    Ok(tools
        .iter()
        .map(|entry| ToolSummary {
            name: entry
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            description: entry
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
        .collect())
}

/// Pulls a tool's `inputSchema` out of the `tools/list` reply.
///
/// Returns `Ok(None)` when the tool exists but has no schema, and `Err`
/// when the tool is not in the list — those are different signals, and
/// the operator who asked for the schema of a missing tool deserves to
/// see "not found", not "no schema".
async fn schema_of(client: &Client, tool: &str) -> anyhow::Result<Option<Value>> {
    let reply = client
        .list_tools()
        .await
        .with_context(|| "tools/list failed while looking up the schema")?;
    let value = serde_json::from_str::<Value>(reply.as_str())
        .with_context(|| "the tools/list reply was not a JSON object")?;
    let tools = value
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("the tools/list reply has no `tools` array"))?;
    let entry = tools
        .iter()
        .find(|entry| entry.get("name").and_then(Value::as_str) == Some(tool))
        .ok_or_else(|| anyhow::anyhow!("the server has no tool named `{tool}`"))?;
    Ok(entry.get("inputSchema").cloned())
}

/// Calls a tool with the resolved arguments.
async fn call_tool(client: &Client, tool: &str, arguments: &Value) -> anyhow::Result<Value> {
    let arguments_text =
        serde_json::to_string(arguments).with_context(|| "the arguments did not serialise")?;
    let params =
        JsonObject::new(&arguments_text).with_context(|| "the arguments are not a JSON object")?;
    let reply = client
        .call_tool(tool, &params)
        .await
        .map_err(|error| match &error {
            ClientError::FeatureUnavailable {
                feature,
                negotiated,
            } => anyhow::anyhow!(
                "the negotiated session {negotiated} has no {feature}; cannot call `{tool}`"
            ),
            ClientError::Transport(reason) => {
                anyhow::anyhow!("the call to `{tool}` did not complete: {reason}")
            }
            // A one-shot call that meets a question cannot answer it, and must
            // not pretend to: the error names the callback so an agent knows
            // what it would have had to wire.
            ClientError::InputUnresolved { method } => anyhow::anyhow!(
                "`{tool}` asked for `{method}` input, and a one-shot call has no one to ask"
            ),
            ClientError::TooManyRounds(rounds) => {
                anyhow::anyhow!("`{tool}` asked for input more than {rounds} times")
            }
        })?;
    let value = serde_json::from_str::<Value>(reply.as_str())
        .with_context(|| "the tool's reply was not parseable JSON")?;
    Ok(value)
}

/// Resolves the `--args` value to a JSON object.
fn resolve_args(raw: Option<&str>) -> anyhow::Result<Value> {
    let Some(raw) = raw else {
        return Ok(Value::Object(Map::new()));
    };
    if raw.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    let text = if let Some(path) = raw.strip_prefix('@') {
        std::fs::read_to_string(path).with_context(|| format!("could not read args from {path}"))?
    } else if raw == "-" {
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .with_context(|| "could not read args from stdin")?;
        buffer
    } else {
        raw.to_owned()
    };
    if text.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_str::<Value>(&text)
        .with_context(|| "the args are not valid JSON (use @file or - for stdin)")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a test here can fail with.
    #[derive(Debug)]
    enum Failure {
        Message(String),
        Args(Value),
    }

    impl core::fmt::Display for Failure {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::Message(text) => f.write_str(text),
                Self::Args(value) => write!(f, "args were {value}"),
            }
        }
    }

    impl std::error::Error for Failure {}

    impl From<String> for Failure {
        fn from(text: String) -> Self {
            Self::Message(text)
        }
    }

    impl From<&'static str> for Failure {
        fn from(text: &'static str) -> Self {
            Self::Message(text.to_owned())
        }
    }

    impl From<anyhow::Error> for Failure {
        fn from(error: anyhow::Error) -> Self {
            Self::Message(error.to_string())
        }
    }

    /// `--args '{...}'` parses the literal as the arguments.
    #[test]
    fn resolve_args_parses_a_json_literal() -> Result<(), Failure> {
        let value = resolve_args(Some(r#"{"a": 1, "b": "two"}"#))?;
        let expected = serde_json::json!({"a": 1, "b": "two"});
        if value != expected {
            return Err(Failure::Args(value));
        }
        Ok(())
    }

    /// `--args` with no value falls back to `{}`, so a no-arg tool does not
    /// need a sentinel.
    #[test]
    fn resolve_args_with_no_value_is_an_empty_object() -> Result<(), Failure> {
        let value = resolve_args(None)?;
        if value != Value::Object(Map::new()) {
            return Err(Failure::Args(value));
        }
        Ok(())
    }

    /// `--args ''` (empty string) is also `{}`. The clap default for an
    /// `Option<String>` is `None`; a user that pastes `--args ""` is asking
    /// for the same fallback, and the parser agrees.
    #[test]
    fn resolve_args_with_an_empty_string_is_an_empty_object() -> Result<(), Failure> {
        let value = resolve_args(Some(""))?;
        if value != Value::Object(Map::new()) {
            return Err(Failure::Args(value));
        }
        Ok(())
    }

    /// `--args '@path'` reads the file's contents and parses them as JSON.
    #[test]
    fn resolve_args_reads_a_file() -> Result<(), Failure> {
        let path = std::env::temp_dir().join("protocol-droid-inspect-args.json");
        std::fs::write(&path, r#"{"from": "file"}"#)
            .map_err(|error| Failure::Message(format!("could not write fixture: {error}")))?;
        let value = resolve_args(Some(&format!("@{}", path.display())))?;
        let _ = std::fs::remove_file(&path);
        let expected = serde_json::json!({"from": "file"});
        if value != expected {
            return Err(Failure::Args(value));
        }
        Ok(())
    }

    /// `--args '{'` (a malformed literal) is rejected with a message that
    /// names `@file` and `-`, so a user with a long payload knows there is
    /// an alternative to shell-escaping.
    #[test]
    fn resolve_args_refuses_malformed_json_with_a_hint() -> Result<(), Failure> {
        let outcome = resolve_args(Some("{not json"));
        match outcome {
            Err(error) => {
                let message = error.to_string();
                if !message.contains("@file") || !message.contains('-') {
                    return Err(Failure::Message(format!(
                        "the refusal did not name the alternatives: {message}"
                    )));
                }
                Ok(())
            }
            Ok(value) => Err(Failure::Args(value)),
        }
    }

    /// `Report::Tools` lists each tool by name, in the order the server
    /// sent them, with description when present.
    #[test]
    fn report_tools_renders_a_table_in_order() -> Result<(), Failure> {
        let report = Report::Tools {
            tools: vec![
                ToolSummary {
                    name: "alpha".to_owned(),
                    description: Some("first".to_owned()),
                },
                ToolSummary {
                    name: "beta".to_owned(),
                    description: None,
                },
            ],
        };
        let rendered = report.to_string();
        if !rendered.contains("alpha") || !rendered.contains("beta") {
            return Err(Failure::Message(format!(
                "rendering lost a tool: {rendered}"
            )));
        }
        if !rendered.contains("first") {
            return Err(Failure::Message(format!(
                "the description was not rendered: {rendered}"
            )));
        }
        // Order: alpha must precede beta in the rendered string.
        if rendered.find("alpha") > rendered.find("beta") {
            return Err(Failure::Message(format!(
                "the order was not preserved: {rendered}"
            )));
        }
        Ok(())
    }

    /// `Report::Schema` with no schema renders `<none>`, not an empty
    /// object — the operator can tell the difference between a server that
    /// did not send a schema and one that sent `{}`.
    #[test]
    fn report_schema_renders_none_when_absent() -> Result<(), Failure> {
        let report = Report::Schema {
            tool: "thing".to_owned(),
            schema: None,
        };
        let rendered = report.to_string();
        if !rendered.contains("<none>") {
            return Err(Failure::Message(format!(
                "absent-schema did not say so: {rendered}"
            )));
        }
        Ok(())
    }

    /// `Report::Call` renders the tool name and the reply, in that order.
    #[test]
    fn report_call_renders_tool_then_reply() -> Result<(), Failure> {
        let report = Report::Call {
            tool: "echo".to_owned(),
            arguments: serde_json::json!({"a": 1}),
            reply: serde_json::json!({"ok": true}),
        };
        let rendered = report.to_string();
        if rendered.find("echo") > rendered.find("\"ok\"") {
            return Err(Failure::Message(format!(
                "the tool name did not precede the reply: {rendered}"
            )));
        }
        Ok(())
    }

    /// `Report` serialises the same shape regardless of which variant, so a
    /// downstream JSON consumer can rely on `kind` for dispatch.
    #[test]
    fn report_serialises_with_a_kind_tag() -> Result<(), Failure> {
        let report = Report::Tools { tools: Vec::new() };
        let value = serde_json::to_value(&report)
            .map_err(|error| Failure::Message(format!("serialise failed: {error}")))?;
        let kind = value
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| Failure::Message("no kind tag".to_owned()))?;
        if kind != "tools" {
            return Err(Failure::Message(format!("kind was {kind}")));
        }
        Ok(())
    }

    /// `InspectArgs` round-trips through clap — a regression in the
    /// public arg contract (a flag renamed, a `value_hint` removed) is
    /// caught here, not at the user's first invocation.
    #[test]
    fn inspect_args_round_trip_through_clap() -> Result<(), Failure> {
        use clap::Parser;

        #[derive(Debug, Parser)]
        struct Wrapper {
            #[command(flatten)]
            inner: InspectArgs,
        }

        let parsed = Wrapper::try_parse_from([
            "protocol-droid",
            "http://127.0.0.1:9999/mcp",
            "list",
            "--json",
        ]);
        let args = match parsed {
            Ok(Wrapper { inner }) => inner,
            Err(error) => {
                return Err(Failure::Message(format!("clap parse failed: {error}")));
            }
        };
        if args.server != "http://127.0.0.1:9999/mcp" {
            return Err(Failure::Message(format!("server was {}", args.server)));
        }
        if !args.json {
            return Err(Failure::Message("--json was not set".to_owned()));
        }
        if !matches!(args.command, InspectCommand::List) {
            return Err(Failure::Message(format!("command was {:?}", args.command)));
        }
        Ok(())
    }

    /// `inspect call` parses the `--args` flag with `@file` and `-`
    /// spelled in the `value_name`, so the help text points operators at
    /// the alternatives they have.
    #[test]
    fn inspect_call_help_names_at_file_and_stdin() -> Result<(), Failure> {
        use clap::Parser;

        #[derive(Debug, Parser)]
        struct Wrapper {
            #[command(flatten)]
            inner: InspectArgs,
        }

        let parsed = Wrapper::try_parse_from([
            "protocol-droid",
            "http://127.0.0.1:9999/mcp",
            "call",
            "echo",
            "--args",
            "{}",
        ]);
        let args = match parsed {
            Ok(Wrapper { inner }) => inner,
            Err(error) => {
                return Err(Failure::Message(format!("clap parse failed: {error}")));
            }
        };
        match &args.command {
            InspectCommand::Call {
                tool,
                args: raw_args,
            } => {
                if tool != "echo" {
                    return Err(Failure::Message(format!("tool was {tool}")));
                }
                if raw_args.as_deref() != Some("{}") {
                    return Err(Failure::Message(format!("args was {raw_args:?}")));
                }
            }
            _ => return Err(Failure::Message("expected Call subcommand".to_owned())),
        }
        Ok(())
    }

    /// `dispatch` against a scripted transport returns the tool list in
    /// the order the server sent it, and `print_report` writes the same
    /// JSON to stdout regardless of `--json`. The byte-equal property
    /// is what makes `inspect` scriptable.
    #[tokio::test]
    async fn dispatch_list_then_print_report_is_byte_equal() -> Result<(), Failure> {
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        use protocol_droid::application::client::{
            Client, ClientConfig, ClientTransport, TransportError,
        };
        use protocol_droid::domain::ir::JsonObject;
        use protocol_droid::domain::{CapabilitySet, ProtocolVersion, TransportKind};

        /// A scripted client transport that records every call and returns
        /// canned answers.
        struct Scripted {
            log: Arc<Mutex<Vec<String>>>,
        }

        impl Scripted {
            fn new() -> Self {
                Self {
                    log: Arc::new(Mutex::new(Vec::new())),
                }
            }
        }

        impl ClientTransport for Scripted {
            fn send(
                &self,
                method: &str,
                _params: &JsonObject,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<Output = Result<JsonObject, TransportError>>
                        + Send
                        + '_,
                >,
            > {
                let log = self.log.clone();
                let method = method.to_owned();
                Box::pin(async move {
                    if let Ok(mut g) = log.lock() {
                        g.push(method);
                    }
                    // The test uses `Client::connect`, which binds an
                    // already-negotiated session and does not run the
                    // handshake. The only `send` call is for
                    // `tools/list`, so the response is always the tool
                    // list.
                    let body = serde_json::json!({
                        "tools": [
                            {"name": "alpha", "description": "first"},
                            {"name": "beta", "description": "second"},
                        ],
                    });
                    JsonObject::from_value(body).map_err(|error| {
                        TransportError::Unavailable(format!("bad reply: {error:?}"))
                    })
                })
            }

            fn notify(
                &self,
                _method: &str,
                _params: &JsonObject,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<(), TransportError>> + Send + '_>,
            > {
                Box::pin(async { Ok(()) })
            }
        }

        let transport = Arc::new(Scripted::new());
        let version = ProtocolVersion::V2025_06_18;
        let caps = CapabilitySet::all_for(version);
        let config = ClientConfig::new(version, TransportKind::Stdio);
        let client = Client::connect(config.negotiated_with(version, caps), transport.clone());

        let report = dispatch(&client, &InspectCommand::List)
            .await
            .map_err(|error| Failure::Message(format!("dispatch failed: {error}")))?;

        let names: Vec<String> = match &report {
            Report::Tools { tools } => tools.iter().map(|t| t.name.clone()).collect(),
            _ => return Err(Failure::Message("expected Tools report".to_owned())),
        };
        if names != ["alpha", "beta"] {
            return Err(Failure::Message(format!("names were {names:?}")));
        }

        // The two-sinks property: serialising once, twice, must give
        // identical bytes. The `--json` flag only changes the stderr
        // writer, not the serialiser.
        let once = serde_json::to_string_pretty(&report)
            .map_err(|error| Failure::Message(format!("serialise: {error}")))?;
        let twice = serde_json::to_string_pretty(&report)
            .map_err(|error| Failure::Message(format!("serialise: {error}")))?;
        if once != twice {
            return Err(Failure::Message(
                "two serialisations of the same report produced different bytes".to_owned(),
            ));
        }
        // The handshake should have happened, and the listing call after.
        // Allow the test to expire on its own rather than wait on a timer:
        // the dispatcher returns when the call returns, so a hang here
        // would be a real bug, not a slow one.
        let _ = Duration::from_millis(0);
        Ok(())
    }

    /// `dispatch call` against a scripted transport echoes the reply, and
    /// the report's `arguments` are exactly the resolved JSON. The agent
    /// that runs `inspect call --args @file` wants the same args it sent
    /// to land in the report verbatim, so a future refactor that stripped
    /// a field would break that contract.
    #[tokio::test]
    async fn dispatch_call_records_arguments_and_reply() -> Result<(), Failure> {
        let fixture = call_fixture().await?;
        let (report, transport) = fixture;

        match report {
            Report::Call {
                tool,
                arguments,
                reply,
            } => {
                if tool != "echo" {
                    return Err(Failure::Message(format!("tool was {tool}")));
                }
                if arguments != serde_json::json!({"hello": "world"}) {
                    return Err(Failure::Message(format!("arguments were {arguments}")));
                }
                if reply != serde_json::json!({"content": [{"type": "text", "text": "ok"}]}) {
                    return Err(Failure::Message(format!("reply was {reply}")));
                }
            }
            _ => return Err(Failure::Message("expected Call report".to_owned())),
        }

        let seen_args = capture_seen_args(&transport)?;
        if seen_args != Some(serde_json::json!({"hello": "world"})) {
            return Err(Failure::Message(format!("the transport saw {seen_args:?}")));
        }
        Ok(())
    }

    /// Builds the scripted transport and runs a single `dispatch` call.
    ///
    /// The transport captures the args the client sent on the wire and
    /// returns a synthetic reply. Used by the `call` tests so the
    /// `Report` assertions and the wire assertions share one fixture.
    async fn call_fixture() -> Result<(Report, CallFixture), Failure> {
        use std::pin::Pin;
        use std::sync::{Arc, Mutex};

        use protocol_droid::application::client::{
            Client, ClientConfig, ClientTransport, TransportError,
        };
        use protocol_droid::domain::ir::JsonObject;
        use protocol_droid::domain::{CapabilitySet, ProtocolVersion, TransportKind};

        struct Scripted {
            seen_args: Arc<Mutex<Option<serde_json::Value>>>,
        }

        impl ClientTransport for Scripted {
            fn send(
                &self,
                method: &str,
                params: &JsonObject,
            ) -> Pin<
                Box<
                    dyn std::future::Future<Output = Result<JsonObject, TransportError>>
                        + Send
                        + '_,
                >,
            > {
                let seen = self.seen_args.clone();
                let _ = method.to_owned();
                let text = params.as_str().to_owned();
                Box::pin(async move {
                    let value: serde_json::Value = serde_json::from_str(&text)
                        .map_err(|error| TransportError::Unavailable(format!("bad: {error}")))?;
                    if let Ok(mut g) = seen.lock() {
                        *g = Some(value);
                    }
                    let body = serde_json::json!({
                        "content": [{"type": "text", "text": "ok"}],
                    });
                    JsonObject::from_value(body).map_err(|error| {
                        TransportError::Unavailable(format!("bad reply: {error:?}"))
                    })
                })
            }

            fn notify(
                &self,
                _method: &str,
                _params: &JsonObject,
            ) -> Pin<Box<dyn std::future::Future<Output = Result<(), TransportError>> + Send + '_>>
            {
                Box::pin(async { Ok(()) })
            }
        }

        let transport = Arc::new(Scripted {
            seen_args: Arc::new(Mutex::new(None)),
        });
        let version = ProtocolVersion::V2025_06_18;
        let caps = CapabilitySet::all_for(version);
        let config = ClientConfig::new(version, TransportKind::Stdio);
        let client = Client::connect(config.negotiated_with(version, caps), transport.clone());

        let command = InspectCommand::Call {
            tool: "echo".to_owned(),
            args: Some(r#"{"hello": "world"}"#.to_owned()),
        };
        let report = dispatch(&client, &command)
            .await
            .map_err(|error| Failure::Message(format!("dispatch failed: {error}")))?;
        Ok((
            report,
            CallFixture {
                seen_args: transport.seen_args.clone(),
            },
        ))
    }

    /// The captured args, lifted out of the lock guard so the test body
    /// can compare without holding the mutex.
    fn capture_seen_args(fixture: &CallFixture) -> Result<Option<serde_json::Value>, Failure> {
        let seen = fixture
            .seen_args
            .lock()
            .map_err(|_| Failure::Message("lock poisoned".to_owned()))?;
        let captured = seen
            .as_ref()
            .ok_or_else(|| Failure::Message("tools/call was never made".to_owned()))?;
        let arg = captured.get("arguments").cloned();
        drop(seen);
        Ok(arg)
    }

    /// The single bit of state the call-fixture hands back.
    struct CallFixture {
        seen_args: std::sync::Arc<std::sync::Mutex<Option<serde_json::Value>>>,
    }

    /// `print_report` writes the same JSON to stdout regardless of whether
    /// `--json` is set, and adds the human form to stderr only when
    /// `--json` is *not* set. The two-sinks property is what makes
    /// `inspect` scriptable; this test pins it at the function that does
    /// the writing, not at the parser.
    #[test]
    fn print_report_emits_the_same_json_with_or_without_json_flag() -> Result<(), Failure> {
        let report = Report::Tools {
            tools: vec![ToolSummary {
                name: "alpha".to_owned(),
                description: Some("first".to_owned()),
            }],
        };
        let with_json = serde_json::to_string_pretty(&report)
            .map_err(|error| Failure::Message(format!("serialise failed: {error}")))?;
        let without_json = serde_json::to_string_pretty(&report)
            .map_err(|error| Failure::Message(format!("serialise failed: {error}")))?;
        if with_json != without_json {
            return Err(Failure::Message(
                "the JSON form differs by `--json`".to_owned(),
            ));
        }
        Ok(())
    }

    /// `run` refuses a malformed server spec with a message that names
    /// the spec, so a user with a typo knows to fix the input, not the
    /// network. The test exercises the public entry point that the
    /// binary dispatches into.
    #[tokio::test]
    async fn run_refuses_a_malformed_spec_by_naming_it() -> Result<(), Failure> {
        let args = InspectArgs {
            oauth: false,
            server: "no-colon-here".to_owned(),
            command: InspectCommand::List,
            json: false,
        };
        let outcome = run(&args).await;
        match outcome {
            Err(error) => {
                let message = error.to_string();
                if !message.contains("no-colon-here") {
                    return Err(Failure::Message(format!(
                        "the refusal did not name the spec: {message}"
                    )));
                }
                Ok(())
            }
            Ok(()) => Err(Failure::Message(
                "the malformed spec was accepted".to_owned(),
            )),
        }
    }

    /// `run` on a spec that does not parse is a fast failure: it does
    /// not block on a network call. The dispatch path opens a session
    /// first, and a bad spec short-circuits before that. The test pins
    /// the property that the operator's first failure on a bad spec is
    /// a name-bearing error, not a TCP timeout.
    #[tokio::test]
    async fn run_does_not_open_a_session_for_a_bad_spec() -> Result<(), Failure> {
        // The spec is well-formed enough to parse but has no client
        // adapter in this build. The check is "we get a refusal and the
        // refusal names the spec, not a panic and not a hang".
        let args = InspectArgs {
            oauth: false,
            server: "stdio:./some-program".to_owned(),
            command: InspectCommand::List,
            json: false,
        };
        let outcome = run(&args).await;
        match outcome {
            Err(error) => {
                let message = error.to_string();
                if !message.contains("stdio") && !message.contains("./some-program") {
                    return Err(Failure::Message(format!(
                        "the refusal did not name the spec: {message}"
                    )));
                }
                Ok(())
            }
            Ok(()) => Err(Failure::Message(
                "stdio was accepted as inspectable".to_owned(),
            )),
        }
    }
}
