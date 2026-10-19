//! The tools and prompt the `2026-07-28` scenarios call by name.
//!
//! Every one of them is registered through the raw [`Server::add_tool`] path
//! rather than `#[tool]`, because each answers with a JSON-RPC result shape the
//! macro has no spelling for: a SEP-2322 `InputRequiredResult` is an interim
//! result, not a `CallToolResult`. That also makes this file the worked example
//! of the multi round-trip API — round one returns [`InputRequired`], round two
//! reads [`RequestContext::input_response`].

use std::sync::Arc;

use protocol_droid::application::mcp::input_required::InputRequired;
use protocol_droid::application::server::context::RequestContext;
use protocol_droid::application::server::{Server, ServerHandle, ToolHandler};
use protocol_droid::domain::Feature;
use protocol_droid::domain::ir::{JsonObject, JsonObjectError, LogLevel};
use protocol_droid::domain::registry::{PrimitiveKind, Prompt, Tool};
use protocol_droid::serde_json::{self, Value, json};

/// The diagnostic tool the trigger toggles in and out of `tools/list`.
const DYNAMIC_TOOL: &str = "test_dynamic_tool";

/// The prompt the prompt trigger toggles in and out of `prompts/list`.
const DYNAMIC_PROMPT: &str = "test_dynamic_prompt";

/// A `CallToolResult` with one text block.
fn text(message: &str) -> JsonObject {
    JsonObject::from_value(json!({ "content": [{ "type": "text", "text": message }] }))
        .unwrap_or_default()
}

/// A JSON literal this file wrote itself, as an object.
fn object(value: &Value) -> JsonObject {
    JsonObject::from_value(value.clone()).unwrap_or_default()
}

/// An `elicitation/create` asking for one string or boolean property.
fn ask(message: &str, property: &str, kind: &str) -> JsonObject {
    object(&json!({
        "message": message,
        "requestedSchema": {
            "type": "object",
            "properties": { property: { "type": kind } },
            "required": [property],
        },
    }))
}

/// A `sampling/createMessage` with one user text message.
fn sample(prompt: &str, max_tokens: u32) -> JsonObject {
    object(&json!({
        "messages": [{ "role": "user", "content": { "type": "text", "text": prompt } }],
        "maxTokens": max_tokens,
    }))
}

/// The accepted value of `property` in an `ElicitResult` answer.
fn accepted(answer: &JsonObject, property: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(answer.as_str()).ok()?;
    if parsed.get("action").and_then(Value::as_str) != Some("accept") {
        return None;
    }
    let value = parsed.get("content")?.get(property)?;
    Some(
        value
            .as_str()
            .map_or_else(|| value.to_string(), ToOwned::to_owned),
    )
}

/// The text a `CreateMessageResult` answer carries.
fn sampled(answer: &JsonObject) -> Option<String> {
    let parsed: Value = serde_json::from_str(answer.as_str()).ok()?;
    parsed
        .get("content")?
        .get("text")?
        .as_str()
        .map(ToOwned::to_owned)
}

/// The root URIs a `ListRootsResult` answer carries.
fn roots(answer: &JsonObject) -> Option<Vec<String>> {
    let parsed: Value = serde_json::from_str(answer.as_str()).ok()?;
    Some(
        parsed
            .get("roots")?
            .as_array()?
            .iter()
            .filter_map(|root| {
                root.get("uri")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            })
            .collect(),
    )
}

/// An interim result asking again, sealed with `state` when there is one.
fn interim(ctx: &RequestContext, ask: InputRequired, state: Option<&str>) -> JsonObject {
    let ask = match state.and_then(|state| ctx.seal_state(state).ok()) {
        Some(sealed) => ask.with_state(sealed),
        None => ask,
    };
    ask.into_result()
}

fn handler<F, Fut>(run: F) -> ToolHandler
where
    F: Fn(RequestContext) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = JsonObject> + Send + 'static,
{
    let run = Arc::new(run);
    Arc::new(move |ctx, _arguments| {
        let run = Arc::clone(&run);
        Box::pin(async move { run(ctx).await })
    })
}

/// `test_input_required_result_elicitation` (A1), shared by six scenarios.
///
/// A retry with the wrong keys, extra keys, or an answer that is not an
/// accepted `ElicitResult` is asked again rather than refused: SEP-2322 has
/// the server re-request what is missing.
async fn elicitation(ctx: RequestContext) -> JsonObject {
    if let Some(name) = ctx
        .input_response("user_name")
        .and_then(|answer| accepted(&answer, "name"))
    {
        return text(&format!("Hello, {name}!"));
    }
    InputRequired::new()
        .elicit("user_name", &ask("What is your name?", "name", "string"))
        .into_result()
}

async fn sampling(ctx: RequestContext) -> JsonObject {
    if let Some(answer) = ctx
        .input_response("capital_question")
        .and_then(|answer| sampled(&answer))
    {
        return text(&format!("The model said: {answer}"));
    }
    InputRequired::new()
        .sample(
            "capital_question",
            &sample("What is the capital of France?", 100),
        )
        .into_result()
}

async fn list_roots(ctx: RequestContext) -> JsonObject {
    if let Some(found) = ctx
        .input_response("client_roots")
        .and_then(|answer| roots(&answer))
    {
        return text(&format!("Roots: {}", found.join(", ")));
    }
    InputRequired::new()
        .list_roots("client_roots")
        .into_result()
}

async fn request_state(ctx: RequestContext) -> JsonObject {
    if ctx.request_state() == Some("confirm") && ctx.input_response("confirm").is_some() {
        return text("state-ok: the request state was verified");
    }
    interim(
        &ctx,
        InputRequired::new().elicit("confirm", &ask("Please confirm", "ok", "boolean")),
        Some("confirm"),
    )
}

async fn multiple_inputs(ctx: RequestContext) -> JsonObject {
    let answered = ["user_name", "greeting", "client_roots"]
        .iter()
        .all(|key| ctx.input_response(key).is_some());
    if answered && ctx.request_state() == Some("multiple") {
        return text("All three inputs received.");
    }
    interim(
        &ctx,
        InputRequired::new()
            .elicit("user_name", &ask("What is your name?", "name", "string"))
            .sample("greeting", &sample("Generate a greeting", 50))
            .list_roots("client_roots"),
        Some("multiple"),
    )
}

async fn multi_round(ctx: RequestContext) -> JsonObject {
    match ctx.request_state() {
        Some("round-2") if ctx.input_response("step2").is_some() => text("Both rounds complete."),
        Some("round-1") if ctx.input_response("step1").is_some() => interim(
            &ctx,
            InputRequired::new().elicit(
                "step2",
                &ask("Step 2: What is your favorite color?", "color", "string"),
            ),
            Some("round-2"),
        ),
        _ => interim(
            &ctx,
            InputRequired::new().elicit(
                "step1",
                &ask("Step 1: What is your name?", "name", "string"),
            ),
            Some("round-1"),
        ),
    }
}

/// Integrity is the library's: a tampered state never reaches this handler.
async fn tampered_state(ctx: RequestContext) -> JsonObject {
    if ctx.request_state() == Some("untampered") {
        return text("The request state was intact.");
    }
    interim(
        &ctx,
        InputRequired::new().elicit("confirm", &ask("Please confirm", "ok", "boolean")),
        Some("untampered"),
    )
}

/// Asks for whichever of sampling and elicitation the client declared.
///
/// The SEP-2322 rule a handler applies itself: an interim result should only
/// carry input requests the client can serve. Asking anyway is not a choice a
/// handler gets — the dispatcher would answer `-32021` — so a handler that can
/// make do with less asks for less.
async fn capabilities(ctx: RequestContext) -> JsonObject {
    let samples = ctx.peer().allows(Feature::Sampling);
    let elicits = ctx.peer().allows(Feature::Elicitation);
    if !(samples || elicits) || ctx.has_input_responses() {
        return text("Completed with what the client could provide.");
    }
    let mut ask_for = InputRequired::new();
    if samples {
        ask_for = ask_for.sample("summary", &sample("Summarise the request.", 50));
    }
    if elicits {
        ask_for = ask_for.elicit("confirm", &ask("Please confirm", "ok", "boolean"));
    }
    ask_for.into_result()
}

/// Asks for sampling unconditionally, so a client that did not declare it is
/// answered `-32021` by the dispatcher before anything is sent.
async fn missing_capability(_ctx: RequestContext) -> JsonObject {
    InputRequired::new()
        .sample("probe", &sample("This needs sampling.", 10))
        .into_result()
}

/// Elicits through an interim result — never as a request on the stream.
async fn streaming_elicitation(_ctx: RequestContext) -> JsonObject {
    InputRequired::new()
        .elicit(
            "answer",
            &ask("What should the tool do?", "answer", "string"),
        )
        .into_result()
}

/// Emits three log records, each subject to the request's `logLevel`.
async fn logging(ctx: RequestContext) -> JsonObject {
    for message in [
        "Tool execution started",
        "Tool processing data",
        "Tool execution completed",
    ] {
        let _ = ctx.log(LogLevel::Info, message).await;
    }
    text("Logged three messages.")
}

async fn trigger_tool_change(ctx: RequestContext) -> JsonObject {
    let Some(handle) = ctx.state::<ServerHandle>().cloned() else {
        return text("No server handle was registered.");
    };
    if handle.is_listed(PrimitiveKind::Tool, DYNAMIC_TOOL) {
        let _ = handle.remove_tool(DYNAMIC_TOOL).await;
    } else if let Ok(tool) = Tool::new(DYNAMIC_TOOL, r#"{"type":"object"}"#) {
        let dynamic = handler(|_ctx| async { text("dynamic") });
        let _ = handle
            .add_tool(
                tool.described("Toggled by test_trigger_tool_change"),
                dynamic,
            )
            .await;
    }
    text("The tool list changed.")
}

async fn trigger_prompt_change(ctx: RequestContext) -> JsonObject {
    let Some(handle) = ctx.state::<ServerHandle>().cloned() else {
        return text("No server handle was registered.");
    };
    if handle.is_listed(PrimitiveKind::Prompt, DYNAMIC_PROMPT) {
        let _ = handle.remove_prompt(DYNAMIC_PROMPT).await;
    } else {
        let _ = handle
            .add_prompt(
                Prompt {
                    description: Some("Toggled by test_trigger_prompt_change".to_owned()),
                    ..Prompt::new(DYNAMIC_PROMPT)
                },
                Arc::new(|_ctx, _arguments| {
                    Box::pin(async {
                        Ok(object(&json!({
                            "messages": [{ "role": "user", "content": { "type": "text", "text": "dynamic" } }],
                        })))
                    })
                }),
            )
            .await;
    }
    text("The prompt list changed.")
}

/// Registers the tools that need nothing but a handler.
fn register_tools(server: &mut Server) -> Result<(), JsonObjectError> {
    let tools: [(&str, &str, ToolHandler); 14] = [
        (
            "test_input_required_result_elicitation",
            "Elicits a name across two rounds",
            handler(elicitation),
        ),
        (
            "test_input_required_result_sampling",
            "Samples across two rounds",
            handler(sampling),
        ),
        (
            "test_input_required_result_list_roots",
            "Lists roots across two rounds",
            handler(list_roots),
        ),
        (
            "test_input_required_result_request_state",
            "Round-trips a request state",
            handler(request_state),
        ),
        (
            "test_input_required_result_multiple_inputs",
            "Asks for three inputs at once",
            handler(multiple_inputs),
        ),
        (
            "test_input_required_result_multi_round",
            "Asks across three rounds",
            handler(multi_round),
        ),
        (
            "test_input_required_result_tampered_state",
            "Refuses a tampered state",
            handler(tampered_state),
        ),
        (
            "test_input_required_result_capabilities",
            "Asks only for declared capabilities",
            handler(capabilities),
        ),
        (
            "test_missing_capability",
            "Needs the sampling capability",
            handler(missing_capability),
        ),
        (
            "test_streaming_elicitation",
            "Elicits without a server request",
            handler(streaming_elicitation),
        ),
        ("test_logging_tool", "Logs three records", handler(logging)),
        (
            "test_trigger_tool_change",
            "Changes the tool list",
            handler(trigger_tool_change),
        ),
        (
            "test_trigger_prompt_change",
            "Changes the prompt list",
            handler(trigger_prompt_change),
        ),
        (
            DYNAMIC_TOOL,
            "Toggled by test_trigger_tool_change",
            handler(|_ctx| async { text("dynamic") }),
        ),
    ];
    for (name, description, run) in tools {
        server.add_tool(
            Tool::new(name, r#"{"type":"object"}"#)?.described(description),
            run,
        );
    }
    Ok(())
}

/// Registers everything above on `server`.
///
/// # Errors
///
/// A [`JsonObjectError`] if a tool's schema does not parse, which for the
/// literals here would be a typo in this file.
pub async fn register(server: &mut Server) -> Result<(), JsonObjectError> {
    // The trigger tools mutate the server they run on, so the handle is put
    // where a handler can reach it. A handle is a clone of the server's shared
    // state, which is what lets a change made from inside a request reach every
    // session — including the listen stream the scenario holds open.
    server.with_state(server.handle());

    register_tools(server)?;

    // SEP-2243's `x-mcp-header`: the region argument is mirrored into
    // `Mcp-Param-Region`, and the transport refuses a call whose header and
    // body disagree before this handler runs.
    server.add_tool(
        Tool::new(
            "test_custom_header",
            r#"{"type":"object","properties":{"region":{"type":"string","x-mcp-header":"Region"}},"required":["region"]}"#,
        )?
        .described("Mirrors its region argument into Mcp-Param-Region"),
        handler(|_ctx| async { text("The region header matched the body.") }),
    );

    let _ = server
        .handle()
        .add_prompt(
            Prompt {
                description: Some("Needs context from the user before it can render".to_owned()),
                ..Prompt::new("test_input_required_result_prompt")
            },
            Arc::new(|ctx, _arguments| {
                Box::pin(async move {
                    if let Some(context) = ctx
                        .input_response("user_context")
                        .and_then(|answer| accepted(&answer, "context"))
                    {
                        return Ok(object(&json!({
                            "messages": [{
                                "role": "user",
                                "content": { "type": "text", "text": format!("Context: {context}") },
                            }],
                        })));
                    }
                    Ok(InputRequired::new()
                        .elicit(
                            "user_context",
                            &ask("What context should the prompt use?", "context", "string"),
                        )
                        .into_result())
                })
            }),
        )
        .await;
    Ok(())
}
