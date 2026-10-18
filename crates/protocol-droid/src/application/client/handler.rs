//! Answering the requests a server sends the client.
//!
//! Through `2025-11-25` a server can ask the client to run a model
//! (`sampling/createMessage`), ask its user a question (`elicitation/create`)
//! or list its roots (`roots/list`), and it does so as a JSON-RPC request on
//! the response stream of whatever call needed the answer. The call is blocked
//! until the client replies, so a client that declared one of these
//! capabilities and then did not answer left the server's tool waiting forever.
//!
//! [`ServerRequestHandler`] is that answer. It is a port the caller implements:
//! this crate cannot know what a person would say, and answering on their
//! behalf would be inventing consent.
//!
//! `2026-07-28` removed server-to-client requests. The same three questions
//! now arrive as an `InputRequiredResult` and are answered through
//! [`call_resolving`](super::inputs::call_resolving).

use core::future::Future;
use core::pin::Pin;
use std::sync::Arc;

use serde_json::{Map, Value};

use crate::domain::CapabilitySet;
use crate::domain::ir::JsonObject;

/// Why a handler declined a request.
///
/// Becomes the JSON-RPC `error` the server reads, so the server's tool learns
/// that the client said no rather than hanging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlerRefusal {
    /// The JSON-RPC error code.
    pub code: i64,
    /// A short reason.
    pub message: String,
}

impl HandlerRefusal {
    /// `-32601`: the client does not do this.
    #[must_use]
    pub fn unsupported(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("this client does not handle `{method}`"),
        }
    }
}

/// Answers the requests a server sends the client.
///
/// # Examples
///
/// ```
/// use std::pin::Pin;
///
/// use protocol_droid::application::client::handler::{HandlerRefusal, ServerRequestHandler};
/// use protocol_droid::domain::ir::JsonObject;
///
/// struct Decline;
///
/// impl ServerRequestHandler for Decline {
///     fn handle<'a>(
///         &'a self,
///         method: &'a str,
///         _params: &'a JsonObject,
///     ) -> Pin<Box<dyn Future<Output = Result<JsonObject, HandlerRefusal>> + Send + 'a>> {
///         Box::pin(async move { Err(HandlerRefusal::unsupported(method)) })
///     }
/// }
/// ```
pub trait ServerRequestHandler: Send + Sync {
    /// Answers one request with its `result`, or declines it.
    fn handle<'a>(
        &'a self,
        method: &'a str,
        params: &'a JsonObject,
    ) -> Pin<Box<dyn Future<Output = Result<JsonObject, HandlerRefusal>> + Send + 'a>>;
}

/// What a connection declares and how it answers the server.
#[derive(Clone, Default)]
pub struct ConnectOptions {
    pub(crate) capabilities: CapabilitySet,
    pub(crate) handler: Option<Arc<dyn ServerRequestHandler>>,
    pub(crate) auth: Option<Arc<dyn super::bearer::BearerAuth>>,
}

impl core::fmt::Debug for ConnectOptions {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ConnectOptions")
            .field("capabilities", &self.capabilities)
            .field("handler", &self.handler.is_some())
            .field("auth", &self.auth.is_some())
            .finish()
    }
}

impl ConnectOptions {
    /// Options that declare nothing and answer nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Declares `capabilities` to the server.
    ///
    /// A server asks for sampling, elicitation or roots only when the client
    /// declared them, so declare exactly what the handler can answer.
    #[must_use]
    pub const fn declaring(mut self, capabilities: CapabilitySet) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Authenticates to the server through `auth`.
    ///
    /// For an OAuth-protected server, pass an
    /// [`OAuthAuthorizer`](crate::application::oauth::authorizer::OAuthAuthorizer).
    #[must_use]
    pub fn authorizing(mut self, auth: Arc<dyn super::bearer::BearerAuth>) -> Self {
        self.auth = Some(auth);
        self
    }

    /// Answers the server's requests through `handler`.
    #[must_use]
    pub fn answering(mut self, handler: Arc<dyn ServerRequestHandler>) -> Self {
        self.handler = Some(handler);
        self
    }
}

/// Fills in the `default` of every property an `accept`ed elicitation omitted.
///
/// SEP-1034: a client that declared `elicitation.applyDefaults` applies the
/// schema's defaults for fields the user left out, so a server receives a
/// complete object. `request` is the `elicitation/create` params; `answer` is
/// the `ElicitResult` the handler produced. Only an `accept` is touched, and
/// only absent keys: a value the user gave is never overwritten.
#[must_use]
pub fn apply_elicitation_defaults(request: &JsonObject, answer: &JsonObject) -> JsonObject {
    let Ok(Value::Object(mut result)) = serde_json::from_str::<Value>(answer.as_str()) else {
        return answer.clone();
    };
    if result.get("action").and_then(Value::as_str) != Some("accept") {
        return answer.clone();
    }
    let Ok(Value::Object(params)) = serde_json::from_str::<Value>(request.as_str()) else {
        return answer.clone();
    };
    let Some(properties) = params
        .get("requestedSchema")
        .and_then(|schema| schema.get("properties"))
        .and_then(Value::as_object)
    else {
        return answer.clone();
    };
    let content = result
        .entry("content")
        .or_insert_with(|| Value::Object(Map::new()));
    if let Some(content) = content.as_object_mut() {
        for (name, schema) in properties {
            if !content.contains_key(name)
                && let Some(default) = schema.get("default")
            {
                content.insert(name.clone(), default.clone());
            }
        }
    }
    JsonObject::from_value(Value::Object(result)).unwrap_or_else(|_| answer.clone())
}

#[cfg(test)]
mod tests {
    use super::apply_elicitation_defaults;
    use crate::domain::ir::JsonObject;

    fn object(text: &str) -> JsonObject {
        JsonObject::new(text).unwrap_or_default()
    }

    #[test]
    fn omitted_fields_take_their_default() {
        let request = object(
            r#"{"message":"m","requestedSchema":{"type":"object","properties":{"name":{"type":"string","default":"John"},"age":{"type":"integer","default":30},"plain":{"type":"string"}}}}"#,
        );
        let filled = apply_elicitation_defaults(&request, &object(r#"{"action":"accept","content":{}}"#));
        let parsed: serde_json::Value = serde_json::from_str(filled.as_str()).unwrap_or_default();
        assert_eq!(parsed.pointer("/content/name"), Some(&serde_json::json!("John")));
        assert_eq!(parsed.pointer("/content/age"), Some(&serde_json::json!(30)));
        assert_eq!(parsed.pointer("/content/plain"), None, "no default, nothing invented");
    }

    #[test]
    fn a_value_the_user_gave_is_kept() {
        let request = object(
            r#"{"requestedSchema":{"properties":{"name":{"type":"string","default":"John"}}}}"#,
        );
        let filled = apply_elicitation_defaults(
            &request,
            &object(r#"{"action":"accept","content":{"name":"Ada"}}"#),
        );
        assert!(filled.as_str().contains("Ada") && !filled.as_str().contains("John"));
    }

    #[test]
    fn a_decline_is_untouched() {
        let request = object(
            r#"{"requestedSchema":{"properties":{"name":{"type":"string","default":"John"}}}}"#,
        );
        let answer = object(r#"{"action":"decline"}"#);
        assert_eq!(apply_elicitation_defaults(&request, &answer), answer);
    }
}
