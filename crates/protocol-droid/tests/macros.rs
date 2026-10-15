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
use protocol_droid::domain::registry::{AuthContext, Prompt, PromptArgument, Resource};
use protocol_droid::domain::session::{CancellationToken, SessionId};
use protocol_droid::domain::{CapabilitySet, ProtocolVersion, RequestId, TransportKind};
use serde::Serialize;

/// The state a `#[tool]` handler reaches through `RequestContext::state`.
///
/// The point of the test that registers this: a `#[tool]` method
/// cannot see `self`, so it has no way to close over a `Greeting`
/// from the function it was declared in. The typed state extension
/// is what makes "share a database pool with a tool handler"
/// compile without a process global.
#[derive(Debug, Clone)]
struct Greeting {
    /// What the greeting prepends to the caller's argument.
    prefix: String,
}

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

    /// Reads state registered through `Server::builder().with_state::<Greeting>(...)`.
    ///
    /// The point of the test: the macro cannot see `self`, so it has
    /// no way to capture an `Arc<Greeting>`; the typed state
    /// extension in `RequestContext::state` is the only way a
    /// `#[tool]` method gets a handle on shared state without a
    /// process global.
    #[protocol_droid::tool(name = "greet")]
    async fn greet(ctx: RequestContext, who: String) -> Result<String, DemoError> {
        let greeting = ctx
            .state::<Greeting>()
            .ok_or_else(|| DemoError::Refused("no greeting registered".to_owned()))?;
        Ok(greeting.prefix.clone() + &who)
    }

    /// A resource, described by the method rather than by the doc comment
    /// above it: a `#[resource]` is a value, and a value carries its own
    /// description, so there is nothing to derive.
    #[protocol_droid::resource]
    fn readme() -> Resource {
        let mut resource = Resource::new("file:///README.md", "readme");
        resource.description = Some("The project's readme.".to_string());
        resource.mime_type = Some("text/markdown".to_string());
        resource
    }

    /// A second resource, so "the first one registered" is not a claim the
    /// test can make by looking at one entry.
    #[protocol_droid::resource]
    fn licence() -> Resource {
        Resource::new("file:///LICENSE", "licence")
    }

    /// A prompt, with two arguments of differing requiredness, so a
    /// `prompts/list` that flattened them to names — or lost the flag — fails.
    #[protocol_droid::prompt]
    fn review() -> Prompt {
        let mut prompt = Prompt::new("review");
        prompt.arguments.push(PromptArgument {
            name: "diff".to_string(),
            description: Some("The text to review.".to_string()),
            title: None,
            required: true,
        });
        prompt.arguments.push(PromptArgument {
            name: "focus".to_string(),
            description: None,
            title: None,
            required: false,
        });
        prompt
    }

    /// Not a tool, so the server must leave it out.
    #[allow(dead_code, reason = "the test asserts it was not registered as one")]
    async fn helper(&self) -> i32 {
        7
    }
}

/// A second server in the same module, with methods `Demo` also has.
///
/// Every generated item for a tool is named `__{Type}_{fn}_*`, and a
/// `#[resource]` or `#[prompt]` generates no name at all — only the
/// registration that calls the method. This type exists to hold that claim to
/// account: before the prefix reached the generated argument struct, `Demo`'s
/// `add` and this type's `add` both declared `__add_Args` at module scope and
/// the file would not compile at all.
struct Other;

#[protocol_droid::server(name = "other-server")]
impl Other {
    /// The same name, the same shape, a different answer.
    #[protocol_droid::tool]
    async fn add(a: i32, b: i32) -> Result<AddAnswer, DemoError> {
        Ok(AddAnswer { total: a * b })
    }

    /// Also the same name as `Demo::readme`, and also a resource.
    #[protocol_droid::resource]
    fn readme() -> Resource {
        Resource::new("file:///other/README.md", "other-readme")
    }

    /// Also the same name as `Demo::review`, and also a prompt.
    #[protocol_droid::prompt]
    fn review() -> Prompt {
        Prompt::new("other-review")
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
        TransportKind::Stdio,
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
    listing(dispatcher, "tools/list").await
}

/// The answer to one of the four listing methods, as JSON.
///
/// One function rather than four, because the four differ only in the method
/// name and each was otherwise a second copy of the session the `dispatch`
/// helper already builds. The `list` above is the tools case of this, kept
/// because most of the tests here are about tools.
async fn listing(
    dispatcher: &Arc<Dispatcher>,
    method: &str,
) -> Result<serde_json::Value, TestError> {
    let listed = dispatcher
        .dispatch(&context(), method, &JsonObject::default())
        .await
        .map_err(|error| Failure::from(format!("{method} was refused: {error}")))?;
    serde_json::from_str(listed.as_str())
        .map_err(|error| Failure::from(format!("the listing is not JSON: {error}")))
}

/// The one tool named `name` in a listing, as JSON.
fn tool<'a>(
    listing: &'a serde_json::Value,
    name: &str,
) -> Result<&'a serde_json::Value, TestError> {
    entry(listing, "tools", "name", name)
}

/// The one entry of `listing[key]` whose `field` reads `value`, as JSON.
///
/// A resource is keyed by its uri and a prompt by its name, so the field to
/// match on differs per kind; taking it as an argument is what lets one lookup
/// serve both listings without pretending they share a key.
fn entry<'a>(
    listing: &'a serde_json::Value,
    key: &str,
    field: &str,
    value: &str,
) -> Result<&'a serde_json::Value, TestError> {
    listing
        .get(key)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| Failure::from(format!("the listing has no {key} array")))?
        .iter()
        .find(|entry| entry.get(field).and_then(serde_json::Value::as_str) == Some(value))
        .ok_or_else(|| Failure::from(format!("no {key} entry whose {field} is {value}")))
}

/// Every identifier in one listing — a tool's name, a resource's uri, a
/// prompt's name — sorted.
///
/// Sorted for the same reason `properties` sorts: a listing is a JSON array
/// built in registration order, and comparing that against a literal asserts
/// on the order the methods happened to be written in.
fn ids(listing: &serde_json::Value, key: &str, field: &str) -> Result<Vec<String>, TestError> {
    let mut found: Vec<String> = listing
        .get(key)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| Failure::from(format!("the listing has no {key} array")))?
        .iter()
        .filter_map(|entry| entry.get(field).and_then(serde_json::Value::as_str))
        .map(ToString::to_string)
        .collect();
    found.sort();
    Ok(found)
}

/// One member of a call result's `structuredContent`, as JSON.
fn structured(answer: &JsonObject, member: &str) -> Result<serde_json::Value, TestError> {
    let parsed: serde_json::Value = serde_json::from_str(answer.as_str())
        .map_err(|error| Failure::from(format!("the answer is not JSON: {error}")))?;
    parsed
        .get("structuredContent")
        .and_then(|content| content.get(member))
        .cloned()
        .ok_or_else(|| Failure::from(format!("the answer has no structuredContent.{member}")))
}

/// The schema's property names, sorted — the tool's arguments.
fn properties(schema: &serde_json::Value) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(members) = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
    {
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

/// A `#[resource]` reaches `resources/list` with the value its method built.
///
/// Not merely that registration returned. `ServerBuilder::resource` publishes
/// to the registry and returns the builder, so a macro that expanded to a
/// registration nothing kept would pass any test that stopped at `Ok(…)` — and
/// the resource would simply be absent from a peer that asked for it. Every
/// field below is set in the method body, so the listing is a reading of the
/// returned value rather than of the attribute.
#[tokio::test]
async fn a_resource_lands_in_the_listing_as_its_method_returned_it() -> Result<(), TestError> {
    let dispatcher = dispatcher(Demo.into_server()?);
    let listed = listing(&dispatcher, "resources/list").await?;

    let readme = entry(&listed, "resources", "uri", "file:///README.md")?;
    let expected = serde_json::json!({
        "uri": "file:///README.md",
        "name": "readme",
        "description": "The project's readme.",
        "mimeType": "text/markdown",
    });
    if readme != &expected {
        return Err(Failure::from(format!(
            "the readme resource is listed as {readme}"
        )));
    }
    Ok(())
}

/// Every marked method is registered — both resources, not only the first.
///
/// A registration that overwrote its predecessor would pass a test that looks
/// at one entry, because the entry it looks at would still be right.
#[tokio::test]
async fn every_marked_resource_is_listed() -> Result<(), TestError> {
    let dispatcher = dispatcher(Demo.into_server()?);
    let listed = listing(&dispatcher, "resources/list").await?;

    let uris = ids(&listed, "resources", "uri")?;
    if uris != ["file:///LICENSE", "file:///README.md"] {
        return Err(Failure::from(format!("the resources listed are {uris:?}")));
    }
    Ok(())
}

/// A `#[prompt]` reaches `prompts/list` with its arguments, in order.
///
/// The arguments are the part that goes wrong quietly: a prompt that listed
/// without them is still a prompt, and a client that prompts for a review and
/// is given no argument to fill in fails at the peer rather than here.
#[tokio::test]
async fn a_prompt_lands_in_the_listing_with_its_arguments() -> Result<(), TestError> {
    let dispatcher = dispatcher(Demo.into_server()?);
    let listed = listing(&dispatcher, "prompts/list").await?;

    let review = entry(&listed, "prompts", "name", "review")?;
    let expected = serde_json::json!({
        "name": "review",
        "description": null,
        "arguments": [
            {"name": "diff", "description": "The text to review.", "required": true},
            {"name": "focus", "description": null, "required": false},
        ],
    });
    if review != &expected {
        return Err(Failure::from(format!(
            "the review prompt is listed as {review}"
        )));
    }
    Ok(())
}

/// A tool and a resource in one `#[server]` block reach their own listings.
///
/// They are registered through different builder methods and stored under
/// different kinds, so the failure worth ruling out is one displacing the
/// other: a server whose `tools/list` has lost the tool, or whose
/// `resources/list` holds something no `resources/read` can name. The tool is
/// called as well as listed, because a tool that lists and cannot run is the
/// other half of the same bug.
#[tokio::test]
async fn a_resource_and_a_tool_do_not_collide() -> Result<(), TestError> {
    let dispatcher = dispatcher(Demo.into_server()?);
    let tools = listing(&dispatcher, "tools/list").await?;
    let resources = listing(&dispatcher, "resources/list").await?;
    let prompts = listing(&dispatcher, "prompts/list").await?;

    // The tool is still a tool, and still answers.
    tool(&tools, "add")?;
    let ctx = context();
    let answer = dispatcher
        .dispatch(&ctx, "tools/call", &call("add", r#"{"a":2,"b":3}"#))
        .await
        .map_err(|error| Failure::from(error.to_string()))?;
    if structured(&answer, "total")? != 5 {
        return Err(Failure::from("add did not answer with the sum"));
    }

    // And the resource is still a resource. Every other assertion in this
    // test is a negative one, and a negative assertion about a listing that
    // came back empty holds vacuously: dropping the registration outright
    // leaves every "nothing landed in the wrong place" claim true. One
    // positive read is what keeps the rest of them about collision rather
    // than about absence.
    entry(&resources, "resources", "uri", "file:///README.md")?;
    entry(&prompts, "prompts", "name", "review")?;

    // And neither of the primitives is listed as a tool.
    let names = ids(&tools, "tools", "name")?;
    for primitive in ["readme", "review"] {
        if names.iter().any(|name| name == primitive) {
            return Err(Failure::from(format!("{primitive} was listed as a tool")));
        }
    }

    // Nor is the tool listed as either of the other two kinds.
    if ids(&resources, "resources", "uri")?
        .iter()
        .any(|uri| uri == "add")
        || ids(&prompts, "prompts", "name")?
            .iter()
            .any(|name| name == "add")
    {
        return Err(Failure::from("the tool was listed as a primitive"));
    }
    Ok(())
}

/// Two servers in one module each keep their own primitives.
///
/// The generated items for a tool are named `__{Type}_{fn}_*` and a
/// `#[resource]` or `#[prompt]` generates no name at all, so this file
/// compiling at all is half the claim — the other half is that `Other`'s `add`
/// multiplies where `Demo`'s adds, rather than one of the two registrations
/// having replaced the other.
#[tokio::test]
async fn two_servers_in_one_module_keep_their_own_primitives() -> Result<(), TestError> {
    let dispatcher = dispatcher(Other.into_server()?);

    let tools = listing(&dispatcher, "tools/list").await?;
    let ctx = context();
    let answer = dispatcher
        .dispatch(&ctx, "tools/call", &call("add", r#"{"a":2,"b":3}"#))
        .await
        .map_err(|error| Failure::from(error.to_string()))?;
    if structured(&answer, "total")? != 6 {
        return Err(Failure::from(
            "the other server's add did not answer with the product",
        ));
    }
    if ids(&tools, "tools", "name")? != ["add"] {
        return Err(Failure::from("the other server's tool listing is wrong"));
    }

    let resources = listing(&dispatcher, "resources/list").await?;
    if ids(&resources, "resources", "uri")? != ["file:///other/README.md"] {
        return Err(Failure::from(
            "the other server's resource listing is wrong",
        ));
    }

    let prompts = listing(&dispatcher, "prompts/list").await?;
    if ids(&prompts, "prompts", "name")? != ["other-review"] {
        return Err(Failure::from("the other server's prompt listing is wrong"));
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
        TransportKind::Stdio,
        Arc::new(NoExchange),
        Arc::new(NoSink),
    )
    .with_auth(AuthContext::unrestricted("tester"));
    RequestContext::for_request(&peer, RequestId::text("r1"), CancellationToken::new(), None)
}

/// A context for a direct dispatch, carrying the state registered on `server`.
///
/// The point of the helper: a test that wants to drive a `#[tool]`
/// handler reading `ctx.state::<T>()` has to hand the dispatcher a
/// context whose state map is the one the server was built with,
/// because the dispatcher reaches state through the context, not
/// through the server. The helper copies the `Arc<StateMap>` over.
fn context_with_server_state(server: &Server) -> RequestContext {
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
        TransportKind::Stdio,
        Arc::new(NoExchange),
        Arc::new(NoSink),
    )
    .with_auth(AuthContext::unrestricted("tester"));
    RequestContext::for_request_with_state(
        &peer,
        RequestId::text("r1"),
        CancellationToken::new(),
        None,
        Arc::clone(server.state()),
    )
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

/// A `#[tool]` method that reads `ctx.state::<T>()` returns the value
/// registered through `Server::builder().with_state(...)`.
///
/// The point of the test is end-to-end: a `#[tool]` cannot see
/// `self`, so it has no captured environment; the typed state
/// extension is the path the macro leaves open. The test builds a
/// `Server` with state, dispatches `greet` through it, and asserts
/// the response carries the greeting prefix.
#[tokio::test]
async fn a_tool_handler_reads_state_from_the_context() -> Result<(), TestError> {
    let mut server = Demo.into_server()?;
    server.with_state(Greeting {
        prefix: "hello, ".to_owned(),
    });
    let dispatcher = dispatcher(server);
    let answer = dispatcher
        .dispatch(
            &context_with_server_state(dispatcher.server()),
            "tools/call",
            &call("greet", r#"{"who": "world"}"#),
        )
        .await
        .map_err(|error| Failure::from(format!("greet was refused: {error}")))?;
    assert_text(&answer, "hello, world")
}

/// A `#[tool]` reading `ctx.state::<T>()` for a type that was not
/// registered returns the registered refusal, not a panic. The
/// `greet` method is written to convert a missing state into a
/// `DemoError::Refused`; this test asserts the error path works.
#[tokio::test]
async fn a_tool_handler_with_no_state_registered_refuses() -> Result<(), TestError> {
    let server = Demo.into_server()?;
    let dispatcher = dispatcher(server);
    let answer = dispatcher
        .dispatch(
            &context(),
            "tools/call",
            &call("greet", r#"{"who": "world"}"#),
        )
        .await
        .map_err(|error| Failure::from(format!("greet was refused at dispatch: {error}")))?;
    // The dispatch itself returned Ok; the tool handler returned an
    // Err, and the dispatcher wraps that as a JSON-RPC result with
    // isError: true. The test inspects the wrapped form.
    if !answer.as_str().contains("no greeting registered") {
        return Err(Failure::from(format!(
            "expected the refused-state message, got {}",
            answer.as_str()
        )));
    }
    if !answer.as_str().contains("\"isError\":true") {
        return Err(Failure::from(format!(
            "expected the answer to be marked isError, got {}",
            answer.as_str()
        )));
    }
    Ok(())
}
