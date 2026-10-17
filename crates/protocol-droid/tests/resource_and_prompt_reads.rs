//! `resources/read` and `prompts/get`, driven through the dispatcher.
//!
//! These two methods had no implementation at all before this: the dispatcher
//! answered seven methods and refused the rest, and a `Resource` carried no
//! contents for it to answer with. The tests below pin the behaviour that
//! fixes both halves — the routing, and the document each method produces.

use std::sync::Arc;

use protocol_droid::application::server::builder::{BoxReadFuture, PromptHandler, ResourceHandler};
use protocol_droid::application::server::context::{
    CallbackExchange, ExchangeError, PeerSession, RequestContext,
};
use protocol_droid::application::server::read::{prompt_messages, read_contents};
use protocol_droid::application::server::{DispatchError, Dispatcher, Server};
use protocol_droid::application::session::{FrameSink, SinkError};
use protocol_droid::domain::feature::{Negotiation, resolve};
use protocol_droid::domain::ir::Role;
use protocol_droid::domain::ir::{
    Content, GetPromptResult, ImageContent, JsonObject, PromptMessage, ResourceContents,
    TextContent,
};
use protocol_droid::domain::registry::{AuthContext, Prompt, Resource};
use protocol_droid::domain::session::{CancellationToken, SessionId};
use protocol_droid::domain::{CapabilitySet, ProtocolVersion, RequestId, TransportKind};

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

struct NoSink;

impl FrameSink for NoSink {
    fn send(
        &self,
        _frame: &str,
    ) -> impl std::future::Future<Output = Result<(), SinkError>> + Send {
        std::future::ready(Ok(()))
    }
}

/// A reader that answers with one text resource, whatever URI is asked for.
fn reader() -> ResourceHandler {
    let handler: ResourceHandler = Arc::new(move |_ctx: RequestContext, uri: String| {
        Box::pin(async move {
            let contents =
                vec![ResourceContents::text(uri, "the body").with_mime_type("text/plain")];
            read_contents(&contents).map_err(DispatchError::from)
        }) as BoxReadFuture
    });
    handler
}

/// A getter that answers with one user message.
fn getter() -> PromptHandler {
    let handler: PromptHandler = Arc::new(move |_ctx: RequestContext, _args: JsonObject| {
        let result = GetPromptResult::new(vec![PromptMessage::new(
            Role::User,
            Content::Text(TextContent::new("say something")),
        )])
        .with_description("asks for something");
        Box::pin(async move {
            prompt_messages(&result, ProtocolVersion::V2025_06_18).map_err(DispatchError::from)
        }) as BoxReadFuture
    });
    handler
}

/// A server with one readable resource and one gettable prompt.
fn server() -> Server {
    let resource = Resource::new("test://static-text", "static-text");
    let prompt = Prompt::new("greeting");
    Server::builder("fixture")
        .resource_with(resource, reader())
        .prompt_with(prompt, getter())
        .build()
}

/// A context at `version`, with request id `id`.
fn context(version: ProtocolVersion, id: &str) -> RequestContext {
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
    RequestContext::for_request(&peer, RequestId::text(id), CancellationToken::new(), None)
}

/// The answer as a peer would read it.
fn parsed(answer: &JsonObject) -> Result<serde_json::Value, String> {
    serde_json::from_str(answer.as_str()).map_err(|error| error.to_string())
}

fn params(body: serde_json::Value) -> Result<JsonObject, String> {
    JsonObject::from_value(body).map_err(|error| error.to_string())
}

const VERSION: ProtocolVersion = ProtocolVersion::V2025_06_18;

#[tokio::test]
async fn a_read_returns_the_resources_contents_array() -> Result<(), String> {
    let dispatcher = Dispatcher::new(Arc::new(server()));
    let answer = dispatcher
        .dispatch(
            &context(VERSION, "r1"),
            "resources/read",
            &params(serde_json::json!({ "uri": "test://static-text" }))?,
        )
        .await
        .map_err(|error| error.to_string())?;

    let document = parsed(&answer)?;
    let entries = document
        .get("contents")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("no contents array: {document}"))?;
    let first = entries
        .first()
        .ok_or_else(|| format!("the array was empty: {document}"))?;

    for (field, wanted) in [
        ("uri", "test://static-text"),
        ("text", "the body"),
        ("mimeType", "text/plain"),
    ] {
        match first.get(field).and_then(serde_json::Value::as_str) {
            Some(found) if found == wanted => {}
            other => return Err(format!("{field} was {other:?}, wanted {wanted:?}")),
        }
    }
    Ok(())
}

#[tokio::test]
async fn reading_an_unregistered_resource_is_refused() -> Result<(), String> {
    let dispatcher = Dispatcher::new(Arc::new(server()));
    let refused = dispatcher
        .dispatch(
            &context(VERSION, "r1"),
            "resources/read",
            &params(serde_json::json!({ "uri": "test://nothing-here" }))?,
        )
        .await;

    match refused {
        Err(protocol_droid::application::server::DispatchError::UnknownResource) => Ok(()),
        other => Err(format!("the answer was {other:?}")),
    }
}

#[tokio::test]
async fn a_get_returns_the_prompts_messages() -> Result<(), String> {
    let dispatcher = Dispatcher::new(Arc::new(server()));
    let answer = dispatcher
        .dispatch(
            &context(VERSION, "r1"),
            "prompts/get",
            &params(serde_json::json!({ "name": "greeting" }))?,
        )
        .await
        .map_err(|error| error.to_string())?;

    let document = parsed(&answer)?;
    match document
        .get("description")
        .and_then(serde_json::Value::as_str)
    {
        Some("asks for something") => {}
        other => return Err(format!("the description was {other:?}")),
    }
    let messages = document
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("no messages array: {document}"))?;
    let first = messages
        .first()
        .ok_or_else(|| format!("the array was empty: {document}"))?;

    match first.get("role").and_then(serde_json::Value::as_str) {
        Some("user") => {}
        other => return Err(format!("the role was {other:?}")),
    }
    match first
        .get("content")
        .and_then(|content| content.get("text"))
        .and_then(serde_json::Value::as_str)
    {
        Some("say something") => Ok(()),
        other => Err(format!("the content was {other:?}")),
    }
}

#[tokio::test]
async fn getting_an_unregistered_prompt_is_refused() -> Result<(), String> {
    let dispatcher = Dispatcher::new(Arc::new(server()));
    let refused = dispatcher
        .dispatch(
            &context(VERSION, "r1"),
            "prompts/get",
            &params(serde_json::json!({ "name": "nothing" }))?,
        )
        .await;

    match refused {
        Err(protocol_droid::application::server::DispatchError::UnknownPrompt) => Ok(()),
        other => Err(format!("the answer was {other:?}")),
    }
}

/// A prompt's content goes through the tool codec, so an image block is
/// spelled the same way in a prompt as in a tool result.
#[tokio::test]
async fn a_prompt_may_carry_an_image_block() -> Result<(), String> {
    let result = GetPromptResult::new(vec![PromptMessage::new(
        Role::User,
        Content::Image(ImageContent::new("aGk=", "image/png")),
    )]);
    let document = prompt_messages(&result, ProtocolVersion::V2025_06_18)
        .map_err(|error| error.to_string())?;

    let parsed = parsed(&document)?;
    let content = parsed
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .and_then(|messages| messages.first())
        .and_then(|message| message.get("content"))
        .ok_or_else(|| format!("no content block: {parsed}"))?;

    match content.get("type").and_then(serde_json::Value::as_str) {
        Some("image") => Ok(()),
        other => Err(format!("the block was {other:?}")),
    }
}
