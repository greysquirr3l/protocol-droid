//! What the attribute macros actually produce.
//!
//! A macro test that only compiles proves very little: the expansion can
//! typecheck and still register a tool with no description, or build a schema
//! with no properties. So every test here calls the tool and reads the listing,
//! because those are the two surfaces a user sees.
//!
//! The negative cases live in `tests/ui/`, driven by trybuild. This file is
//! the positive half: if the expansion is wrong in a way that still compiles,
//! it is wrong in a way only a runtime assertion can see.
//!
//! # Why the `async` handlers are allowed to be idle
//!
//! Clippy has a correct objection here that does not apply. For an ordinary
//! `async fn` with no `.await` in it, `std::future::ready` is a better answer:
//! one less state machine, and nothing waits. These handlers are not ordinary.
//! `#[server]` generates code that calls each one and awaits the result, and
//! the `async` is what makes that call produce a future. Delete it, as
//! `unused_async_trait_impl` suggests, and the macro's `.await` is awaiting a
//! plain value — so the lint's own remedy is the bug.
//!
//! The lint is `unused_async_trait_impl` rather than `unused_async` because
//! clippy reads these as impl-block methods. They are an inherent `impl`, not
//! a trait's, which is a small inaccuracy in a lint whose remedy does not hold.

#![expect(
    clippy::unused_async,
    reason = "the generated handler awaits the call, and a plain value has nothing to await"
)]
#![expect(
    clippy::unused_async_trait_impl,
    reason = "the generated handler awaits the call, and a plain value has nothing to await"
)]

use std::sync::Arc;

use protocol_droid::application::server::context::{
    CallbackExchange, ExchangeError, PeerSession, RequestContext,
};
use protocol_droid::application::server::{Dispatcher, Server};
use protocol_droid::application::session::{FrameSink, SinkError};
use protocol_droid::domain::feature::{Negotiation, resolve};
use protocol_droid::domain::ir::JsonObject;
use protocol_droid::domain::registry::AuthContext;
use protocol_droid::domain::session::{CancellationToken, SessionId};
use protocol_droid::domain::{CapabilitySet, ProtocolVersion, RequestId, TransportKind};
use serde::Serialize;

/// A server written with the macros, exercising every shape at once.
///
/// The `ctx` parameter on one tool and not the others is deliberate: the two
/// forms have to produce the same tool, and a test that only used one of them
/// would not notice a macro that mishandled the other.
#[derive(Default)]
struct Demo;

#[protocol_droid::server(name = "demo-server")]
impl Demo {
    /// Adds two numbers.
    #[protocol_droid::tool]
    async fn add(a: i32, b: i32) -> Result<AddAnswer, DemoError> {
        Ok(AddAnswer { total: a + b })
    }

    /// Takes a context and echoes a string back.
    #[protocol_droid::tool]
    async fn shout(ctx: RequestContext, text: String) -> Result<String, DemoError> {
        let _ = ctx;
        Ok(text.to_uppercase())
    }

    /// Takes no arguments at all.
    #[protocol_droid::tool]
    async fn ping() -> Result<String, DemoError> {
        Ok("pong".to_string())
    }

    /// Always refuses, so the error path has a tool to run.
    #[protocol_droid::tool]
    async fn explode(what: String) -> Result<String, DemoError> {
        Err(DemoError::Refused(what))
    }

    /// Renamed on the wire, and described by the attribute rather than a doc
    /// comment.
    #[protocol_droid::tool(name = "renamed", description = "A named description.")]
    async fn original_name() -> Result<String, DemoError> {
        Ok("named".to_string())
    }

    /// Not a tool, so the server must leave it out.
    #[allow(dead_code, reason = "the test asserts it was not registered as one")]
    async fn helper(&self) -> i32 {
        7
    }
}

/// The answer `add` returns.
#[derive(Serialize)]
struct AddAnswer {
    /// The sum.
    total: i32,
}

/// The one error type every handler here returns.
#[derive(Debug)]
enum DemoError {
    /// The tool refused, and says why.
    Refused(String),
}

impl core::fmt::Display for DemoError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Refused(what) => write!(f, "refused: {what}"),
        }
    }
}

/// What a test here can fail with.
#[derive(Debug)]
enum Failure {
    /// The test's own complaint.
    Message(String),
}

impl core::fmt::Display for Failure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Message(text) => f.write_str(text),
        }
    }
}

impl std::error::Error for Failure {}

impl From<String> for Failure {
    fn from(text: String) -> Self {
        Self::Message(text)
    }
}

impl From<&str> for Failure {
    fn from(text: &str) -> Self {
        Self::Message(text.to_string())
    }
}

impl From<protocol_droid::domain::ir::JsonObjectError> for Failure {
    fn from(error: protocol_droid::domain::ir::JsonObjectError) -> Self {
        Self::Message(format!("bad fixture: {error}"))
    }
}

type TestError = Failure;

/// An exchange that refuses, so no test here can pass by accident.
struct NoExchange;

impl CallbackExchange for NoExchange {
    fn exchange(
        &self,
        method: &str,
        _params: &JsonObject,
    ) -> impl std::future::Future<Output = Result<JsonObject, ExchangeError>> + Send {
        let _ = method;
        std::future::ready(Err(ExchangeError::new("no callbacks here")))
    }
}

/// A sink that discards.
struct NoSink;

impl FrameSink for NoSink {
    fn send(
        &self,
        _frame: &str,
    ) -> impl std::future::Future<Output = Result<(), SinkError>> + Send {
        std::future::ready(Ok(()))
    }
}

/// A dispatcher over `server` with everything a session needs.
fn dispatcher(server: Server) -> Arc<Dispatcher> {
    let version = ProtocolVersion::V2025_06_18;
    let peer = PeerSession::new(
        version,
        resolve(Negotiation {
            version,
            client_caps: CapabilitySet::all_for(version),
            server_caps: CapabilitySet::all_for(version),
            transport: TransportKind::StreamableHttp,
        }),
        SessionId::new(1),
        Arc::new(NoExchange),
        Arc::new(NoSink),
    )
    .with_auth(AuthContext::unrestricted("tester"));

    let ctx =
        RequestContext::for_request(&peer, RequestId::text("r1"), CancellationToken::new(), None);
    // The context is moved into the dispatcher, so the tools must be
    // callable from it. Every tool here is a plain function, so a per-request
    // context would work just as well; a tool reading `ctx` is what the
    // `shout` case covers.
    let _ = ctx;
    Arc::new(Dispatcher::new(Arc::new(server)))
}

/// The `tools/list` answer, as JSON.
async fn list(dispatcher: &Arc<Dispatcher>) -> Result<serde_json::Value, TestError> {
    let version = ProtocolVersion::V2025_06_18;
    let peer = PeerSession::new(
        version,
        resolve(Negotiation {
            version,
            client_caps: CapabilitySet::all_for(version),
            server_caps: CapabilitySet::all_for(version),
            transport: TransportKind::StreamableHttp,
        }),
        SessionId::new(1),
        Arc::new(NoExchange),
        Arc::new(NoSink),
    )
    .with_auth(AuthContext::unrestricted("tester"));
    let ctx = RequestContext::for_request(
        &peer,
        RequestId::text("list"),
        CancellationToken::new(),
        None,
    );

    let listed = dispatcher
        .dispatch(&ctx, "tools/list", &JsonObject::default())
        .await
        .map_err(|error| Failure::from(error.to_string()))?;
    serde_json::from_str(listed.as_str())
        .map_err(|error| Failure::from(format!("the listing is not JSON: {error}")))
}

/// The one tool named `name` in a listing, as JSON.
fn tool<'a>(
    listing: &'a serde_json::Value,
    name: &str,
) -> Result<&'a serde_json::Value, TestError> {
    listing
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .ok_or("the listing has no tools array")?
        .iter()
        .find(|tool| tool.get("name").and_then(serde_json::Value::as_str) == Some(name))
        .ok_or_else(|| Failure::from(format!("no tool named {name} in the listing")))
}

/// The schema's property names, sorted — the tool's arguments.
fn properties(schema: &serde_json::Value) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(members) = schema.get("properties").and_then(serde_json::Value::as_object) {
        for name in members.keys() {
            names.push(name.clone());
        }
    }
    // Sorted because a schema is a JSON object, and a JSON object has no order.
    // Comparing an unsorted list against `["a", "b"]` would assert on whatever
    // order the serialiser happened to produce.
    names.sort();
    names
}

/// The server's name comes from the attribute, not the type name.
#[test]
fn the_server_name_comes_from_the_attribute() -> Result<(), TestError> {
    let server = Demo.into_server()?;
    if server.name() != "demo-server" {
        return Err(Failure::from(format!(
            "the server is named {}",
            server.name()
        )));
    }
    Ok(())
}

/// A tool's schema carries its parameters, and nothing else.
#[tokio::test]
async fn a_tools_schema_carries_its_parameters() -> Result<(), TestError> {
    let dispatcher = dispatcher(Demo.into_server()?);
    let listing = list(&dispatcher).await?;

    let add = tool(&listing, "add")?;
    let schema = add
        .get("inputSchema")
        .ok_or("the tool has no inputSchema")?;
    let names = properties(schema);
    if names != ["a", "b"] {
        return Err(Failure::from(format!(
            "add's schema has properties {names:?}"
        )));
    }
    Ok(())
}

/// A tool with no parameters has no properties, and a call still reaches it.
#[tokio::test]
async fn a_tool_with_no_parameters_still_works() -> Result<(), TestError> {
    let dispatcher = dispatcher(Demo.into_server()?);
    let ctx = context();
    let answer = dispatcher
        .dispatch(&ctx, "tools/call", &call("ping", "{}"))
        .await
        .map_err(|error| Failure::from(error.to_string()))?;
    assert_text(&answer, "pong")
}

/// A handler's doc comment is its description, so the two cannot drift.
#[tokio::test]
async fn the_doc_comment_becomes_the_description() -> Result<(), TestError> {
    let dispatcher = dispatcher(Demo.into_server()?);
    let listing = list(&dispatcher).await?;

    let add = tool(&listing, "add")?;
    if add.get("description").and_then(serde_json::Value::as_str) != Some("Adds two numbers.") {
        return Err(Failure::from(format!(
            "add is described as {:?}",
            add.get("description")
        )));
    }
    Ok(())
}

/// An explicit `description` wins over a doc comment that says something else.
///
/// The attribute is the more specific instruction, so a doc comment kept for
/// the maintainer does not silently become the wire description.
#[tokio::test]
async fn an_explicit_description_wins_over_the_doc_comment() -> Result<(), TestError> {
    let dispatcher = dispatcher(Demo.into_server()?);
    let listing = list(&dispatcher).await?;

    let renamed = tool(&listing, "renamed")?;
    if renamed
        .get("description")
        .and_then(serde_json::Value::as_str)
        != Some("A named description.")
    {
        return Err(Failure::from(format!(
            "renamed is described as {:?}",
            renamed.get("description")
        )));
    }
    Ok(())
}

/// `name = "..."` renames the tool on the wire and nowhere else.
#[tokio::test]
async fn an_explicit_name_renames_the_tool() -> Result<(), TestError> {
    let dispatcher = dispatcher(Demo.into_server()?);
    let listing = list(&dispatcher).await?;

    if tool(&listing, "renamed").is_err() {
        return Err(Failure::from("the renamed tool is not in the listing"));
    }
    if tool(&listing, "original_name").is_ok() {
        return Err(Failure::from(
            "the Rust name is still on the wire after a rename",
        ));
    }
    Ok(())
}

/// A handler that takes a context gets one, and the context is not an argument.
#[tokio::test]
async fn a_context_parameter_is_not_part_of_the_schema() -> Result<(), TestError> {
    let dispatcher = dispatcher(Demo.into_server()?);
    let listing = list(&dispatcher).await?;

    let shout = tool(&listing, "shout")?;
    let schema = shout
        .get("inputSchema")
        .ok_or("the tool has no inputSchema")?;
    let names = properties(schema);
    if names != ["text"] {
        return Err(Failure::from(format!(
            "shout's schema has properties {names:?}"
        )));
    }
    Ok(())
}

/// A handler's return value reaches the caller, structured and as text.
#[tokio::test]
async fn a_structured_answer_reaches_both_members() -> Result<(), TestError> {
    let dispatcher = dispatcher(Demo.into_server()?);
    let ctx = context();
    let answer = dispatcher
        .dispatch(&ctx, "tools/call", &call("add", r#"{"a":2,"b":3}"#))
        .await
        .map_err(|error| Failure::from(error.to_string()))?;

    let parsed: serde_json::Value = serde_json::from_str(answer.as_str())
        .map_err(|error| Failure::from(format!("the answer is not JSON: {error}")))?;
    if parsed
        .get("structuredContent")
        .and_then(|value| value.get("total"))
        != Some(&serde_json::Value::from(5))
    {
        return Err(Failure::from(format!(
            "the structured answer is wrong: {parsed}"
        )));
    }
    Ok(())
}

/// Arguments are decoded into the handler's own types.
///
/// `a` is sent as a JSON number, and the handler receives an `i32`. A macro
/// that passed the raw object through would not typecheck, so this is really a
/// test that the generated struct's field types reached the call.
#[tokio::test]
async fn arguments_reach_the_handler_as_its_own_types() -> Result<(), TestError> {
    let dispatcher = dispatcher(Demo.into_server()?);
    let ctx = context();
    let answer = dispatcher
        .dispatch(&ctx, "tools/call", &call("shout", r#"{"text":"hello"}"#))
        .await
        .map_err(|error| Failure::from(error.to_string()))?;
    assert_text(&answer, "HELLO")
}

/// A handler that returns `Err` answers with an error result, not a failure.
///
/// The distinction is the whole point: a tool that refuses has been reached and
/// has said no, and a client deciding whether to retry needs to tell that from
/// a connection that dropped.
#[tokio::test]
async fn a_handler_error_is_an_error_result() -> Result<(), TestError> {
    let dispatcher = dispatcher(Demo.into_server()?);
    let ctx = context();
    let answer = dispatcher
        .dispatch(&ctx, "tools/call", &call("explode", r#"{"what":"nope"}"#))
        .await
        .map_err(|error| Failure::from(format!("the call itself failed: {error}")))?;

    let parsed: serde_json::Value = serde_json::from_str(answer.as_str())
        .map_err(|error| Failure::from(format!("the answer is not JSON: {error}")))?;
    if parsed.get("isError") != Some(&serde_json::Value::Bool(true)) {
        return Err(Failure::from(format!(
            "the refusal is not flagged: {parsed}"
        )));
    }
    Ok(())
}

/// Arguments that do not match the schema are refused before the handler runs.
///
/// The refusal names the mismatch rather than reporting a missing tool: the
/// tool exists and works, the call is what is wrong, and a caller told "no such
/// tool" would go looking for a tool that is right there in `tools/list`.
#[tokio::test]
async fn wrong_arguments_are_refused_with_the_reason() -> Result<(), TestError> {
    let dispatcher = dispatcher(Demo.into_server()?);
    let ctx = context();
    let answer = dispatcher
        .dispatch(&ctx, "tools/call", &call("add", r#"{"a":"not a number"}"#))
        .await
        .map_err(|error| Failure::from(format!("the call itself failed: {error}")))?;

    let parsed: serde_json::Value = serde_json::from_str(answer.as_str())
        .map_err(|error| Failure::from(format!("the answer is not JSON: {error}")))?;
    if parsed.get("isError") != Some(&serde_json::Value::Bool(true)) {
        return Err(Failure::from(format!(
            "the refusal is not flagged: {parsed}"
        )));
    }
    let text = parsed
        .get("content")
        .and_then(serde_json::Value::as_array)
        .and_then(|blocks| blocks.first())
        .and_then(|block| block.get("text"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    if !text.contains("arguments") {
        return Err(Failure::from(format!(
            "the refusal does not mention the arguments: {text}"
        )));
    }
    Ok(())
}

/// A method that is not `#[tool]` is not registered.
#[tokio::test]
async fn an_unannotated_method_is_not_a_tool() -> Result<(), TestError> {
    let dispatcher = dispatcher(Demo.into_server()?);
    let listing = list(&dispatcher).await?;
    let listed = listing
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .ok_or("the listing has no tools array")?;
    if listed
        .iter()
        .filter_map(|entry| entry.get("name"))
        .any(|name| name == "helper")
    {
        return Err(Failure::from("an unannotated method was registered"));
    }
    Ok(())
}

/// A context for a direct dispatch.
fn context() -> RequestContext {
    let version = ProtocolVersion::V2025_06_18;
    let peer = PeerSession::new(
        version,
        resolve(Negotiation {
            version,
            client_caps: CapabilitySet::all_for(version),
            server_caps: CapabilitySet::all_for(version),
            transport: TransportKind::StreamableHttp,
        }),
        SessionId::new(1),
        Arc::new(NoExchange),
        Arc::new(NoSink),
    )
    .with_auth(AuthContext::unrestricted("tester"));
    RequestContext::for_request(&peer, RequestId::text("r1"), CancellationToken::new(), None)
}

/// The `tools/call` params for `name` with `arguments`.
///
/// `arguments` is parsed rather than pasted in. `json!({ "arguments": text })`
/// on a `&str` produces a JSON *string*, so the tool would receive the two
/// characters `{` and `}` of its own arguments and nothing else — and the
/// failure would read as a decoding bug in the macro rather than as a mistake
/// here.
fn call(name: &str, arguments: &str) -> JsonObject {
    let parsed: serde_json::Value =
        serde_json::from_str(arguments).unwrap_or(serde_json::Value::Null);
    let document = serde_json::json!({ "name": name, "arguments": parsed });
    JsonObject::from_value(document).unwrap_or_default()
}

/// Asserts a result's single text block says `expected`.
fn assert_text(answer: &JsonObject, expected: &str) -> Result<(), TestError> {
    let parsed: serde_json::Value = serde_json::from_str(answer.as_str())
        .map_err(|error| Failure::from(format!("the answer is not JSON: {error}")))?;
    let text = parsed
        .get("content")
        .and_then(serde_json::Value::as_array)
        .and_then(|blocks| blocks.first())
        .and_then(|block| block.get("text"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    if text != expected {
        return Err(Failure::from(format!("got {text:?}, wanted {expected:?}")));
    }
    Ok(())
}
