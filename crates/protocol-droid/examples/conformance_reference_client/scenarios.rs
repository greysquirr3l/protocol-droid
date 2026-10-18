//! One function per scenario the suite's client mode runs.

use std::sync::Arc;

use protocol_droid::application::client::handler::{
    ConnectOptions, HandlerRefusal, ServerRequestHandler, apply_elicitation_defaults,
};
use protocol_droid::application::client::inputs::{InputAnswers, answer_input_required};
use protocol_droid::application::client::{Client, ClientTransport};
use protocol_droid::domain::ir::JsonObject;
use protocol_droid::domain::{
    Capability, CapabilitySet, ProtocolVersion, SupportedVersions, TransportKind, TransportSpec,
};
use protocol_droid::serde_json::{self, Value, json};

type Failure = Box<dyn std::error::Error>;

/// Runs the scenario the suite named.
pub async fn run(scenario: &str, url: &str) -> Result<(), Failure> {
    match scenario {
        "initialize" | "tools_call" | "tools-call" => basic(url).await,
        "json-schema-ref-no-deref" => list_only(url).await,
        "request-metadata" => request_metadata(url).await,
        "http-standard-headers" => standard_headers(url).await,
        "http-custom-headers" => custom_headers(url).await,
        "http-invalid-tool-headers" => invalid_tool_headers(url).await,
        "sep-2322-client-request-state" => request_state(url).await,
        "elicitation-sep1034-client-defaults" | "elicitation-defaults" => {
            elicitation_defaults(url).await
        }
        "sse-retry" => sse_retry(url).await,
        other => Err(format!("this client has no handler for `{other}`").into()),
    }
}

/// The revision the runner resolved, if it named one.
fn runner_version() -> Option<ProtocolVersion> {
    std::env::var("MCP_CONFORMANCE_PROTOCOL_VERSION")
        .ok()
        .and_then(|text| text.parse().ok())
}

/// Whether the revision under test has no handshake.
fn stateless() -> bool {
    runner_version().is_some_and(|version| !protocol_droid::domain::handshake_defined_in(version))
}

fn everything_declared() -> CapabilitySet {
    CapabilitySet::empty()
        .with(Capability::Roots)
        .with(Capability::Sampling)
        .with(Capability::Elicitation)
}

/// Connects the way the revision under test is spoken.
async fn connect(url: &str, options: ConnectOptions) -> Result<Client, Failure> {
    let (kind, versions) = if stateless() {
        let version = runner_version().ok_or("a stateless run names its revision")?;
        (
            TransportKind::StatelessHttp,
            SupportedVersions::from_slice(&[version])?,
        )
    } else {
        (TransportKind::StreamableHttp, SupportedVersions::all()?)
    };
    let spec = TransportSpec::new(kind, url)?;
    Ok(Client::connect_with(&spec, &versions, options).await?)
}

/// Connects statelessly regardless of the runner's revision: the scenarios that
/// exist only for the stateless lifecycle.
async fn connect_stateless(url: &str) -> Result<Client, Failure> {
    let version = runner_version()
        .filter(|version| !protocol_droid::domain::handshake_defined_in(*version))
        .unwrap_or(ProtocolVersion::V2026_07_28);
    let spec = TransportSpec::new(TransportKind::StatelessHttp, url)?;
    Ok(Client::connect_with(
        &spec,
        &SupportedVersions::from_slice(&[version])?,
        ConnectOptions::new().declaring(everything_declared()),
    )
    .await?)
}

fn names(listing: &JsonObject, key: &str, member: &str) -> Vec<String> {
    serde_json::from_str::<Value>(listing.as_str())
        .ok()
        .and_then(|value| value.get(key).and_then(Value::as_array).cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| entry.get(member).and_then(Value::as_str).map(ToOwned::to_owned))
        .collect()
}

fn object(value: &Value) -> JsonObject {
    JsonObject::from_value(value.clone()).unwrap_or_default()
}

// ── initialize / tools_call ────────────────────────────────────────────────

async fn basic(url: &str) -> Result<(), Failure> {
    let client = connect(url, ConnectOptions::new()).await?;
    // Unchecked: these scenarios' mocks advertise nothing, and what is judged
    // is what this client sends, not whether the server promised to answer.
    let tools = client.send_unchecked("tools/list", &JsonObject::default()).await?;
    if let Some(first) = names(&tools, "tools", "name").first() {
        let call = object(&json!({ "name": first, "arguments": { "a": 2, "b": 3 } }));
        client.send_unchecked("tools/call", &call).await?;
    }
    Ok(())
}

/// A client may list tools and must not fetch a network `$ref` it finds there.
async fn list_only(url: &str) -> Result<(), Failure> {
    let client = connect(url, ConnectOptions::new()).await?;
    client.send_unchecked("tools/list", &JsonObject::default()).await?;
    Ok(())
}

// ── SEP-2575 ───────────────────────────────────────────────────────────────

async fn request_metadata(url: &str) -> Result<(), Failure> {
    let client = connect_stateless(url).await?;
    // Unchecked: the scenario's `server/discover` advertises no capabilities,
    // and what is judged is the request this client sends, not whether the
    // server said it would answer.
    client
        .send_unchecked("tools/list", &JsonObject::default())
        .await?;
    Ok(())
}

// ── SEP-2243 ───────────────────────────────────────────────────────────────

async fn standard_headers(url: &str) -> Result<(), Failure> {
    let client = connect_stateless(url).await?;
    let tools = client.list_tools().await?;
    for tool in names(&tools, "tools", "name") {
        client.call_tool(&tool, &JsonObject::default()).await?;
    }
    let resources = client.list_resources().await?;
    for uri in names(&resources, "resources", "uri") {
        client.read_resource(&uri).await?;
    }
    let prompts = client.list_prompts().await?;
    for prompt in names(&prompts, "prompts", "name") {
        client.get_prompt(&prompt, &JsonObject::default()).await?;
    }
    Ok(())
}

/// The scenario hands the calls it wants made in `MCP_CONFORMANCE_CONTEXT`.
async fn custom_headers(url: &str) -> Result<(), Failure> {
    let context: Value = serde_json::from_str(&std::env::var("MCP_CONFORMANCE_CONTEXT")?)?;
    let client = connect_stateless(url).await?;
    // Listed first: the client learns each tool's `x-mcp-header` annotations
    // from the listing, and cannot mirror what it has not seen.
    client.list_tools().await?;
    for call in context
        .get("toolCalls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = call.get("name").and_then(Value::as_str).ok_or("a tool call has no name")?;
        let arguments = object(call.get("arguments").unwrap_or(&Value::Null));
        client.call_tool(name, &arguments).await?;
    }
    Ok(())
}

/// Calls whatever survives the client's own validation of the listing.
async fn invalid_tool_headers(url: &str) -> Result<(), Failure> {
    let client = connect_stateless(url).await?;
    let tools = client.list_tools().await?;
    for tool in names(&tools, "tools", "name") {
        // A failure of one call is the scenario's own mock rejecting the
        // arguments; what is judged is *which* tools were called at all.
        let _ = client.call_tool(&tool, &object(&json!({ "region": "us-west1" }))).await;
    }
    Ok(())
}

// ── SEP-2322 ───────────────────────────────────────────────────────────────

/// The scenario's mock does no handshake, so this drives the transport itself.
async fn request_state(url: &str) -> Result<(), Failure> {
    use protocol_droid::adapters::secondary::transport::http::{HttpClientConfig, StatelessHttpClient};
    let version = runner_version().unwrap_or(ProtocolVersion::V2026_07_28);
    let transport = StatelessHttpClient::new(
        HttpClientConfig::new(url)?,
        version,
        json!({ "elicitation": {}, "sampling": {}, "roots": {} }),
    )?;
    let transport: &dyn ClientTransport = &transport;

    let confirm = |_key: String, method: String, _params: JsonObject| async move {
        (method == "elicitation/create")
            .then(|| object(&json!({ "action": "accept", "content": { "confirmed": true } })))
    };

    transport.send("tools/list", &JsonObject::default()).await?;

    // A tool that answers with state: retried with it echoed, after an
    // unrelated call that must carry none of it.
    let echo = object(&json!({ "name": "test_mrtr_echo_state", "arguments": {} }));
    let first = transport.send("tools/call", &echo).await?;
    if let Some(answers) = answer_input_required(&first, &confirm).await? {
        transport
            .send("tools/call", &object(&json!({ "name": "test_mrtr_unrelated", "arguments": {} })))
            .await?;
        transport.send("tools/call", &answers.applied_to(&echo)).await?;
    }

    // A tool that sends no state: the retry sends none either.
    let plain = object(&json!({ "name": "test_mrtr_no_state", "arguments": {} }));
    let second = transport.send("tools/call", &plain).await?;
    if let Some(answers) = answer_input_required(&second, &confirm).await? {
        let answers = InputAnswers { state: None, ..answers };
        transport.send("tools/call", &answers.applied_to(&plain)).await?;
    }

    // A result with no `resultType` is complete: not retried.
    transport
        .send(
            "tools/call",
            &object(&json!({ "name": "test_mrtr_no_result_type", "arguments": {} })),
        )
        .await?;
    Ok(())
}

// ── 2025-11-25 ─────────────────────────────────────────────────────────────

/// Accepts every elicitation empty; the library fills the defaults in.
#[derive(Debug)]
struct EmptyAnswers;

impl ServerRequestHandler for EmptyAnswers {
    fn handle<'a>(
        &'a self,
        method: &'a str,
        params: &'a JsonObject,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<JsonObject, HandlerRefusal>> + Send + 'a>> {
        Box::pin(async move {
            if method != "elicitation/create" {
                return Err(HandlerRefusal::unsupported(method));
            }
            let accepted = object(&json!({ "action": "accept", "content": {} }));
            Ok(apply_elicitation_defaults(params, &accepted))
        })
    }
}

async fn elicitation_defaults(url: &str) -> Result<(), Failure> {
    let client = connect(
        url,
        ConnectOptions::new()
            .declaring(CapabilitySet::empty().with(Capability::Elicitation))
            .answering(Arc::new(EmptyAnswers)),
    )
    .await?;
    let tools = client.list_tools().await?;
    let tool = names(&tools, "tools", "name")
        .into_iter()
        .find(|name| name == "test_client_elicitation_defaults")
        .ok_or("the test tool was not listed")?;
    client.call_tool(&tool, &JsonObject::default()).await?;
    Ok(())
}

/// The mock closes the call's stream early and says when to come back.
async fn sse_retry(url: &str) -> Result<(), Failure> {
    let client = connect(url, ConnectOptions::new()).await?;
    let tools = client.list_tools().await?;
    let tool = names(&tools, "tools", "name")
        .into_iter()
        .next()
        .ok_or("no tool was listed")?;
    client.call_tool(&tool, &JsonObject::default()).await?;
    Ok(())
}
