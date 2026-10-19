//! Answering a server's request for input (SEP-2322, `2026-07-28`).
//!
//! A server cannot send a request of its own at this revision, so a call that
//! needs the user answers with an `InputRequiredResult`: `resultType:
//! "input_required"`, the questions in `inputRequests`, and optionally a
//! `requestState` of its own. The client answers by *retrying the original
//! request* with `inputResponses` keyed as the questions were, echoing
//! `requestState` back unchanged — and sending none when the server sent none.
//! Every retry is a new JSON-RPC request with a new id.
//! <https://modelcontextprotocol.io/specification/2026-07-28/basic/patterns/mrtr>
//!
//! This works on any method the SEP names (`tools/call`, `prompts/get`,
//! `resources/read`) and over any [`ClientTransport`], handshake or not.

use core::future::Future;
use core::pin::Pin;

use serde_json::{Map, Value};

use super::{ClientError, ClientTransport};
use crate::domain::ir::JsonObject;

/// Answers one input request.
///
/// Given the request's key, its method (`elicitation/create`,
/// `sampling/createMessage` or `roots/list`) and its params, returns the answer
/// — an `ElicitResult`, `CreateMessageResult` or `ListRootsResult` — or `None`
/// when it cannot. Any closure `Fn(String, String, JsonObject) -> impl Future`
/// is one, so most callers never name this trait.
///
/// A trait with a boxed future rather than an `AsyncFn` bound because an
/// `AsyncFn`'s future cannot be required to be `Send` on stable Rust, and a
/// call that cannot be moved onto a `tokio::spawn` is not usable by the
/// callers who need it most.
///
/// # Examples
///
/// ```
/// use protocol_droid::application::client::inputs::InputResolver;
/// use protocol_droid::domain::ir::JsonObject;
///
/// fn takes(_: &impl InputResolver) {}
///
/// takes(&|_key: String, method: String, _params: JsonObject| async move {
///     (method == "roots/list").then(|| JsonObject::new(r#"{"roots":[]}"#).unwrap_or_default())
/// });
/// ```
pub trait InputResolver: Send + Sync {
    /// Answers the request, or declines with `None`.
    fn resolve<'a>(
        &'a self,
        key: &'a str,
        method: &'a str,
        params: &'a JsonObject,
    ) -> Pin<Box<dyn Future<Output = Option<JsonObject>> + Send + 'a>>;
}

impl<F, Fut> InputResolver for F
where
    F: Fn(String, String, JsonObject) -> Fut + Send + Sync,
    Fut: Future<Output = Option<JsonObject>> + Send + 'static,
{
    fn resolve<'a>(
        &'a self,
        key: &'a str,
        method: &'a str,
        params: &'a JsonObject,
    ) -> Pin<Box<dyn Future<Output = Option<JsonObject>> + Send + 'a>> {
        Box::pin(self(key.to_owned(), method.to_owned(), params.clone()))
    }
}

/// How many times a call may be retried with fresh input before it fails.
///
/// A server that keeps asking is either broken or hostile, and a client that
/// answered forever would be a way to make it prompt a person without end.
pub const MAX_INPUT_ROUNDS: u32 = 8;

/// What a client answers an `InputRequiredResult` with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InputAnswers {
    /// The `inputResponses`, keyed as the server's `inputRequests` were.
    pub responses: Map<String, Value>,
    /// The server's `requestState`, to echo unchanged; `None` when it sent none.
    pub state: Option<String>,
}

impl InputAnswers {
    /// `params` with these answers added: the original call, retried.
    #[must_use]
    pub fn applied_to(&self, params: &JsonObject) -> JsonObject {
        let mut attempt: Map<String, Value> = match serde_json::from_str(params.as_str()) {
            Ok(Value::Object(object)) => object,
            _ => Map::new(),
        };
        attempt.insert(
            "inputResponses".to_owned(),
            Value::Object(self.responses.clone()),
        );
        if let Some(state) = &self.state {
            attempt.insert("requestState".to_owned(), Value::from(state.clone()));
        }
        JsonObject::from_value(Value::Object(attempt)).unwrap_or_default()
    }
}

/// Answers `result` if it is an interim `InputRequiredResult`.
///
/// `Ok(None)` for a complete result — including one with no `resultType` at
/// all, which a server older than the field never wrote and which must be read
/// as complete and not retried. This is the step [`call_resolving`] repeats;
/// it is public for a caller that has to do something between the question
/// and the retry.
///
/// # Errors
///
/// [`ClientError::InputUnresolved`] when `resolve` cannot answer a request.
pub async fn answer_input_required(
    result: &JsonObject,
    resolve: &(impl InputResolver + ?Sized),
) -> Result<Option<InputAnswers>, ClientError> {
    let parsed: Value = serde_json::from_str(result.as_str()).unwrap_or_default();
    if parsed.get("resultType").and_then(Value::as_str) != Some("input_required") {
        return Ok(None);
    }
    let state = parsed
        .get("requestState")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let requests = parsed
        .get("inputRequests")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut responses = Map::new();
    for (key, request) in requests {
        let asked = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let asked_params =
            JsonObject::from_value(request.get("params").cloned().unwrap_or_default())
                .unwrap_or_default();
        let Some(answer) = resolve.resolve(&key, &asked, &asked_params).await else {
            return Err(ClientError::InputUnresolved { method: asked });
        };
        responses.insert(
            key,
            serde_json::from_str(answer.as_str()).unwrap_or_default(),
        );
    }
    Ok(Some(InputAnswers { responses, state }))
}

/// Sends `method` with `params`, answering input requests through `resolve`.
///
/// `resolve` is given the request's key, its method (`elicitation/create`,
/// `sampling/createMessage` or `roots/list`) and its params, and returns the
/// answer — an `ElicitResult`, `CreateMessageResult` or `ListRootsResult` — or
/// `None` when it cannot.
///
/// # Errors
///
/// [`ClientError::Transport`] as for any call, [`ClientError::InputUnresolved`]
/// when `resolve` returns `None` (the call is abandoned, never answered on the
/// caller's behalf), and [`ClientError::TooManyRounds`] past
/// [`MAX_INPUT_ROUNDS`].
pub async fn call_resolving(
    transport: &(impl ClientTransport + ?Sized),
    method: &str,
    params: &JsonObject,
    resolve: &(impl InputResolver + ?Sized),
) -> Result<JsonObject, ClientError> {
    let mut attempt = params.clone();
    for _ in 0..MAX_INPUT_ROUNDS {
        let result = transport.send(method, &attempt).await?;
        let Some(answers) = answer_input_required(&result, resolve).await? else {
            return Ok(result);
        };
        // Each retry starts from the original params, not the last attempt:
        // an answer from round one must not ride along into round three.
        attempt = answers.applied_to(params);
    }
    Err(ClientError::TooManyRounds(MAX_INPUT_ROUNDS))
}
