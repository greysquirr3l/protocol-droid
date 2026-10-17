//! The JSON `resources/read` and `prompts/get` answer with.
//!
//! # Why these documents live here and not in the IR
//!
//! `GetPromptResult` and `ResourceContents` are pure data with no
//! serialisation of their own, exactly like `ToolResult`. Assembly is an
//! application concern because it depends on which revision the session
//! negotiated — a fact the domain layer is not allowed to know — and a
//! `to_json` on the IR type would have to take the revision as an argument
//! anyway, which puts it back where it started.
//!
//! # Why a prompt's content goes through the tool codec
//!
//! A `prompts/get` message carries a `ContentBlock`, and that union is spelled
//! differently per revision — `resource_link` arrives in `2025-06-18`, audio
//! in `2025-03-26`. Rather than answer "how is a content block spelled at this
//! revision" a second time, this wraps the block in the one-block `ToolResult`
//! the tool path already lowers and reads the block back out of the answer.
//!
//! That looks roundabout and is not. The alternative is a second copy of the
//! per-revision codecs answering the same question, and the two would agree
//! until the next revision added a field to one of them and not the other.

use std::future::Future;
use std::sync::Arc;

use crate::adapters::secondary::codec::result::verbatim;
use crate::application::server::DispatchError;
use crate::application::server::builder::{BoxReadFuture, PromptHandler, ResourceHandler};
use crate::application::server::context::RequestContext;
use crate::application::server::result::OutcomeError;
use crate::domain::ProtocolVersion;
use crate::domain::ir::{
    Content, GetPromptResult, JsonObject, PromptMessage, ResourceBody, ResourceContents, ToolResult,
};

/// The `ReadResourceResult` for `contents`.
///
/// # Errors
///
/// [`OutcomeError::NotAnObject`] when an assembled entry is not a JSON object.
pub fn read_contents(contents: &[ResourceContents]) -> Result<JsonObject, OutcomeError> {
    let mut entries = Vec::with_capacity(contents.len());
    for entry in contents {
        let mut object = serde_json::Map::new();
        object.insert("uri".to_owned(), entry.uri.clone().into());
        match &entry.body {
            ResourceBody::Text(text) => {
                object.insert("text".to_owned(), text.clone().into());
            }
            ResourceBody::Blob(data) => {
                object.insert("blob".to_owned(), data.clone().into());
            }
        }
        if let Some(mime_type) = &entry.mime_type {
            object.insert("mimeType".to_owned(), mime_type.clone().into());
        }
        entries.push(serde_json::Value::Object(object));
    }

    JsonObject::from_value(serde_json::json!({ "contents": entries }))
        .map_err(OutcomeError::NotAnObject)
}

/// The `GetPromptResult` for `result`, with content blocks spelled for `version`.
///
/// # Errors
///
/// [`OutcomeError::NotLowerable`] for a block the negotiated revision cannot
/// express, and [`OutcomeError::NotAnObject`] when the assembled document is
/// not an object.
pub fn prompt_messages(
    result: &GetPromptResult,
    version: ProtocolVersion,
) -> Result<JsonObject, OutcomeError> {
    let mut messages = Vec::with_capacity(result.messages.len());
    for message in &result.messages {
        let block = one_block(&message.content, version)?;
        // `block` is a validated JSON object held as a string, and `json!`
        // will not take it without a serde impl it does not need. Re-parsing
        // here is one parse per prompt message on a path that is not hot.
        let block: serde_json::Value = serde_json::from_str(block.as_str())
            .map_err(|error| OutcomeError::Answer(error.to_string()))?;
        messages.push(serde_json::json!({
            "role": message.role.as_str(),
            "content": block,
        }));
    }

    let mut document = serde_json::json!({ "messages": messages });
    if let (serde_json::Value::Object(map), Some(description)) =
        (&mut document, result.description.as_deref())
    {
        map.insert("description".to_string(), description.into());
    }
    JsonObject::from_value(document).map_err(OutcomeError::NotAnObject)
}

/// One content block, lowered for `version` and unwrapped from its result.
///
/// The wrapping is invisible on the wire and cannot fail for a reason the
/// caller could act on differently, so it does not get an error variant of its
/// own.
fn one_block(content: &Content, version: ProtocolVersion) -> Result<JsonObject, OutcomeError> {
    let document = verbatim(version, &ToolResult::new(vec![content.clone()]))?;
    let parsed: serde_json::Value = serde_json::from_str(document.as_str())
        .map_err(|error| OutcomeError::Answer(error.to_string()))?;
    let block = parsed
        .get("content")
        .and_then(serde_json::Value::as_array)
        .and_then(|blocks| blocks.first())
        .cloned()
        .ok_or_else(|| OutcomeError::Answer("the lowered result carried no block".to_owned()))?;
    JsonObject::from_value(block).map_err(OutcomeError::NotAnObject)
}

/// Adapts a resource reader into the [`ResourceHandler`] a server stores.
///
/// Takes the URI by value rather than by reference. A reference here would put
/// a lifetime on the bound, and a higher-ranked `for<'uri>` in a `where` clause
/// also binds the *return* type: an `async fn` returns a different future for
/// each `'uri`, so no single `Fut` can satisfy it and the bound rejects every
/// async reader. Owned arguments leave the future type independent of any
/// lifetime. `#[server]`'s expansion wraps a reference-taking reader into this
/// shape, so `#[resource]` readers are still written as `fn(&str)`.
///
/// The reader takes and returns owned values, so the boxed future is
/// `'static` and does not borrow the request.
///
/// # Examples
///
/// ```
/// use protocol_droid::application::server::builder::ResourceHandler;
/// use protocol_droid::application::server::context::RequestContext;
/// use protocol_droid::application::server::read::resource_reader;
/// use protocol_droid::domain::ir::ResourceContents;
///
/// // The context is required rather than optional: it carries who is asking,
/// // and a reader that could not see it would serve every caller as whoever
/// // opened the connection.
/// async fn readme(_context: RequestContext, uri: String) -> Vec<ResourceContents> {
///     vec![ResourceContents::text(uri, "the body")]
/// }
///
/// let handler: ResourceHandler = resource_reader(readme);
/// assert!(std::sync::Arc::strong_count(&handler) >= 1);
/// ```
pub fn resource_reader<F, Fut>(read: F) -> ResourceHandler
where
    F: Fn(RequestContext, String) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Vec<ResourceContents>> + Send + 'static,
{
    // Shared rather than captured. The handler is `Fn`, so it may be called
    // more than once, and a future that moved the reader out of the closure
    // would make the second call impossible. An `Arc` clone is cheap and the
    // future owns it, which is what lets the boxed future stay `'static`.
    let read = Arc::new(read);
    let handler: ResourceHandler = Arc::new(move |context, uri: String| {
        let read = Arc::clone(&read);
        Box::pin(async move {
            let contents = read(context, uri).await;
            read_contents(&contents).map_err(DispatchError::from)
        }) as BoxReadFuture
    });
    handler
}

/// Adapts a prompt getter into the [`PromptHandler`] a server stores.
///
/// Takes a future, as [`resource_reader`] does. A reader that has nothing to
/// await is written as a plain `fn` and wrapped by `#[server]`'s expansion;
/// calling the adapter directly means passing an `async fn`, because wrapping
/// is the macro's job and doing it here would mean every caller writes it.
///
/// The revision the content blocks are spelled for comes from the context, not
/// from a parameter, because it is the session's answer and not the caller's to
/// guess.
///
/// # Examples
///
/// ```
/// use protocol_droid::application::server::builder::PromptHandler;
/// use protocol_droid::application::server::context::RequestContext;
/// use protocol_droid::application::server::read::prompt_getter;
/// use protocol_droid::domain::ir::{
///     Content, JsonObject, PromptMessage, Role, TextContent,
/// };
///
/// async fn greeting(_context: RequestContext, _arguments: JsonObject) -> Vec<PromptMessage> {
///     vec![PromptMessage::new(
///         Role::User,
///         Content::Text(TextContent::new("hello")),
///     )]
/// }
///
/// let handler: PromptHandler = prompt_getter(greeting);
/// assert!(std::sync::Arc::strong_count(&handler) >= 1);
/// ```
pub fn prompt_getter<F, Fut>(get: F) -> PromptHandler
where
    F: Fn(RequestContext, JsonObject) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Vec<PromptMessage>> + Send + 'static,
{
    let get = Arc::new(get);
    let handler: PromptHandler = Arc::new(move |context, arguments: JsonObject| {
        let get = Arc::clone(&get);
        Box::pin(async move {
            // Read before the move, because the revision this result is
            // spelled for comes from the same context the getter consumes.
            let version = context.peer().negotiated();
            let messages = get(context, arguments).await;
            let result = GetPromptResult::new(messages);
            prompt_messages(&result, version).map_err(DispatchError::from)
        }) as BoxReadFuture
    });
    handler
}
