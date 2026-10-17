//! The content-shape tools the conformance suite calls, asserted on the wire.
//!
//! The suite itself is an npm package and needs a network to run, so these
//! tests stand in for it on the five content-shape scenarios: each tool here
//! is written the way the reference server writes it, and the assertions read
//! the document a peer would read rather than the `ToolResult` that produced
//! it.
//!
//! That distinction is the whole point. The IR models all five content types,
//! and a test that asserted on the IR would pass for a fixture whose answer
//! never reached the wire correctly — which is precisely the failure this file
//! exists to catch. Every assertion below reads `content` and its blocks as
//! JSON, because that is where the revision's spelling actually lands.
//!
//! The per-revision half matters too. `test_audio_content` is accepted at
//! `2025-03-26` and *refused* at `2024-11-05`, and a fixture that quietly
//! degraded instead of refusing would report a pass at the revision and a lie
//! at every other.

// These handlers are `async fn` because `#[server]` generates a call to each
// one and awaits the future it returns. The five bodies here do no waiting,
// and `std::future::ready` would be the cheaper spelling for an ordinary
// function — but it is not the shape a tool is required to have, and the
// macro's `.await` is awaiting the handler, not anything inside it. Removing
// the `async` in the name of saving a state machine produces a server that
// does not build.
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
use protocol_droid::application::server::{DispatchError, Dispatcher, Server};
use protocol_droid::application::session::{FrameSink, SinkError};
use protocol_droid::domain::feature::{Negotiation, resolve};
use protocol_droid::domain::ir::{
    AudioContent, Content, EmbeddedResource, ImageContent, JsonObject, JsonObjectError,
    TextContent, ToolResult,
};
use protocol_droid::domain::registry::AuthContext;
use protocol_droid::domain::session::{CancellationToken, SessionId};
use protocol_droid::domain::{CapabilitySet, ProtocolVersion, RequestId, TransportKind};

/// A 1x1 transparent PNG, base64. Real, so a decoder can read it.
const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

/// A 44-byte silent WAV header, base64.
const WAV_SILENT: &str = "UklGRiQAAABXQVZFZm10IBAAAAABAAEAgD4AAAB9AAACABAAZGF0YQAAAAA=";

/// An exchange that answers nothing, because these tests drive the dispatcher
/// directly and never reach the wire's outbound half.
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

/// A sink that discards, for the same reason.
struct NoSink;

impl FrameSink for NoSink {
    fn send(
        &self,
        _frame: &str,
    ) -> impl std::future::Future<Output = Result<(), SinkError>> + Send {
        std::future::ready(Ok(()))
    }
}

/// The fixture's content-shape tools, written as the reference server writes
/// them.
struct ContentFixture;

#[protocol_droid::server(name = "content-fixture")]
impl ContentFixture {
    /// One text block and nothing else.
    #[protocol_droid::tool]
    async fn test_simple_text() -> ToolResult {
        ToolResult::text("This is a simple text response.")
    }

    /// An image block.
    #[protocol_droid::tool]
    async fn test_image_content() -> ToolResult {
        ToolResult::new(vec![Content::Image(ImageContent::new(
            PNG_1X1,
            "image/png",
        ))])
    }

    /// An audio block.
    #[protocol_droid::tool]
    async fn test_audio_content() -> ToolResult {
        ToolResult::new(vec![Content::Audio(AudioContent::new(
            WAV_SILENT,
            "audio/wav",
        ))])
    }

    /// A resource inline in the result.
    #[protocol_droid::tool]
    async fn test_embedded_resource() -> ToolResult {
        ToolResult::new(vec![Content::EmbeddedResource(EmbeddedResource::text(
            "test://embedded-resource",
            "This is an embedded resource's text.",
        ))])
    }

    /// Three blocks, text first.
    #[protocol_droid::tool]
    async fn test_multiple_content_types() -> ToolResult {
        ToolResult::new(vec![
            Content::Text(TextContent::new("This is a multi-type response.")),
            Content::Image(ImageContent::new(PNG_1X1, "image/png")),
            Content::EmbeddedResource(EmbeddedResource::text(
                "test://embedded-resource",
                "This is an embedded resource's text.",
            )),
        ])
    }
}

/// Everything a test here can fail on, collected so a fixture's own mistakes
/// and a dispatch failure report through one `?`.
#[derive(Debug)]
enum Failure {
    /// The server refused to build.
    Build(String),
    /// The dispatcher refused the call.
    Dispatch(String),
    /// The answer did not have the shape the scenario names.
    Shape(String),
}

impl core::fmt::Display for Failure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Build(text) | Self::Dispatch(text) | Self::Shape(text) => f.write_str(text),
        }
    }
}

impl std::error::Error for Failure {}

impl From<JsonObjectError> for Failure {
    fn from(error: JsonObjectError) -> Self {
        Self::Build(error.to_string())
    }
}

impl From<DispatchError> for Failure {
    fn from(error: DispatchError) -> Self {
        Self::Dispatch(error.to_string())
    }
}

type TestError = Failure;

/// A dispatcher for the fixture.
fn dispatcher(server: Server) -> Arc<Dispatcher> {
    Arc::new(Dispatcher::new(Arc::new(server)))
}

/// A context for `version`, with request id `id`.
///
/// The id is a parameter because dispatching twice under one id replays the
/// first answer through the idempotency record — correct, and a trap for a
/// test that varies its arguments.
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

/// The `tools/call` params for a tool with no arguments.
fn call(name: &str) -> JsonObject {
    JsonObject::from_value(serde_json::json!({ "name": name, "arguments": {} })).unwrap_or_default()
}

/// The answer as a peer would read it.
fn parsed(answer: &JsonObject) -> Result<serde_json::Value, Failure> {
    serde_json::from_str(answer.as_str())
        .map_err(|error| Failure::Shape(format!("the answer is not JSON: {error}")))
}

/// The `content` blocks of an answer.
///
/// A missing `content` is named rather than turned into an empty list, because
/// "no content array" and "an empty content array" are different failures and
/// a test that treated them alike would pass the second while the first was
/// what happened.
fn blocks(answer: &JsonObject) -> Result<Vec<serde_json::Value>, Failure> {
    parsed(answer)?
        .get("content")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .ok_or_else(|| Failure::Shape(format!("the answer has no content array: {answer}")))
}

/// A block's `type`, as a string.
fn block_type(block: &serde_json::Value) -> Result<&str, Failure> {
    block
        .get("type")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| Failure::Shape(format!("a content block has no type: {block}")))
}

/// A block's `type`, or `""` when it is absent.
///
/// For assertions about a sequence, where "wrong block" and "no block" both
/// mean the shape is wrong and the difference does not change the fix.
fn block_type_or_empty(block: &serde_json::Value) -> &str {
    block
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

/// Whether an answer claims to be a failure.
fn claims_error(answer: &JsonObject) -> Result<bool, Failure> {
    Ok(matches!(
        parsed(answer)?.get("isError"),
        Some(serde_json::Value::Bool(true))
    ))
}

/// `test_simple_text` answers with exactly one plain text block.
#[tokio::test]
async fn a_simple_text_tool_answers_with_one_text_block() -> Result<(), TestError> {
    let version = ProtocolVersion::V2025_06_18;
    let dispatcher = dispatcher(ContentFixture.into_server()?);
    let answer = dispatcher
        .dispatch(
            &context(version, "r1"),
            "tools/call",
            &call("test_simple_text"),
        )
        .await?;

    let blocks = blocks(&answer)?;
    if blocks.len() != 1 {
        return Err(Failure::Shape(format!(
            "expected 1 block, got {}",
            blocks.len()
        )));
    }
    let first = blocks
        .first()
        .ok_or_else(|| Failure::Shape("no block".to_owned()))?;
    if block_type(first)? != "text" {
        return Err(Failure::Shape(format!("the block was {first}")));
    }
    if claims_error(&answer)? {
        return Err(Failure::Shape(format!("claimed an error: {answer}")));
    }
    Ok(())
}

/// `test_image_content` puts an image block on the wire with its media type.
#[tokio::test]
async fn an_image_tool_answers_with_an_image_block() -> Result<(), TestError> {
    let version = ProtocolVersion::V2025_06_18;
    let dispatcher = dispatcher(ContentFixture.into_server()?);
    let answer = dispatcher
        .dispatch(
            &context(version, "r1"),
            "tools/call",
            &call("test_image_content"),
        )
        .await?;

    let blocks = blocks(&answer)?;
    let first = blocks
        .first()
        .ok_or_else(|| Failure::Shape("no block".to_owned()))?;
    if block_type(first)? != "image" {
        return Err(Failure::Shape(format!("the block was {first}")));
    }
    match first.get("mimeType").and_then(serde_json::Value::as_str) {
        Some("image/png") => {}
        other => return Err(Failure::Shape(format!("the mimeType was {other:?}"))),
    }
    if first.get("data").and_then(serde_json::Value::as_str) != Some(PNG_1X1) {
        return Err(Failure::Shape(format!("the data was {first}")));
    }
    Ok(())
}

/// `test_embedded_resource` carries a `resource` object with a `uri`.
#[tokio::test]
async fn an_embedded_resource_tool_answers_with_a_resource_block() -> Result<(), TestError> {
    let version = ProtocolVersion::V2025_06_18;
    let dispatcher = dispatcher(ContentFixture.into_server()?);
    let answer = dispatcher
        .dispatch(
            &context(version, "r1"),
            "tools/call",
            &call("test_embedded_resource"),
        )
        .await?;

    let blocks = blocks(&answer)?;
    let first = blocks
        .first()
        .ok_or_else(|| Failure::Shape("no block".to_owned()))?;
    if block_type(first)? != "resource" {
        return Err(Failure::Shape(format!("the block was {first}")));
    }
    match first
        .get("resource")
        .and_then(|resource| resource.get("uri"))
        .and_then(serde_json::Value::as_str)
    {
        Some("test://embedded-resource") => Ok(()),
        other => Err(Failure::Shape(format!("the uri was {other:?}"))),
    }
}

/// `test_multiple_content_types` answers with three blocks, in order.
#[tokio::test]
async fn a_multi_type_tool_answers_with_every_block_in_order() -> Result<(), TestError> {
    let version = ProtocolVersion::V2025_06_18;
    let dispatcher = dispatcher(ContentFixture.into_server()?);
    let answer = dispatcher
        .dispatch(
            &context(version, "r1"),
            "tools/call",
            &call("test_multiple_content_types"),
        )
        .await?;

    let blocks = blocks(&answer)?;
    let types: Vec<&str> = blocks.iter().map(block_type_or_empty).collect();
    if types != ["text", "image", "resource"] {
        return Err(Failure::Shape(format!("the blocks were {types:?}")));
    }
    Ok(())
}

/// The revision gate: `2025-03-26` introduced audio and answers, `2024-11-05`
/// has no audio block and refuses.
///
/// Both halves are named because the refusal is the half that regresses
/// quietly. A downgrade that dropped the block instead of refusing it would
/// leave a `2024-11-05` answer that parses and is missing the thing the
/// scenario asked for.
#[tokio::test]
async fn audio_is_answered_after_its_revision_and_refused_before_it() -> Result<(), TestError> {
    let later = dispatcher(ContentFixture.into_server()?);
    let answer = later
        .dispatch(
            &context(ProtocolVersion::V2025_03_26, "r1"),
            "tools/call",
            &call("test_audio_content"),
        )
        .await?;
    let blocks = blocks(&answer)?;
    let first = blocks
        .first()
        .ok_or_else(|| Failure::Shape("no block".to_owned()))?;
    if block_type(first)? != "audio" {
        return Err(Failure::Shape(format!("the block was {first}")));
    }

    let earlier = dispatcher(ContentFixture.into_server()?);
    let refused = earlier
        .dispatch(
            &context(ProtocolVersion::V2024_11_05, "r1"),
            "tools/call",
            &call("test_audio_content"),
        )
        .await?;
    if !claims_error(&refused)? {
        return Err(Failure::Shape(format!(
            "2024-11-05 answered with an audio block it does not have: {refused}"
        )));
    }
    Ok(())
}
