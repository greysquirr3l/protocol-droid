//! A2A `SendMessage` typed envelopes — the
//! request, the configuration, and the response.
//!
//! # What these are
//!
//! [`SendMessageRequest`] is the typed shape of
//! the `params` field for the `SendMessage`
//! JSON-RPC method. [`SendMessageResponse`] is
//! the typed shape of the `result` field —
//! either a [`Task`] (the work is in progress or
//! complete) or a [`Message`] (the agent
//! answered directly without creating a task).
//!
//! The handler that implements
//! [`A2aRequestHandler`](crate::application::a2a::A2aRequestHandler)
//! decodes the raw `serde_json::Value` params
//! into a `SendMessageRequest` and serialises
//! the result back as a `SendMessageResponse`.
//! The serving adapter
//! ([`serve_jsonrpc`](crate::adapters::primary::a2a::jsonrpc_serve::serve_jsonrpc))
//! handles the JSON-RPC envelope; the handler
//! is responsible only for the method-specific
//! decoding.
//!
//! # Why an untagged enum for the response
//!
//! The wire form of a `SendMessage` response is
//! the inner object directly: a `Task` is
//! `{"id": "...", "status": ...}`, a `Message`
//! is `{"role": ..., "parts": ...}`. The two
//! shapes are discriminated by which top-level
//! field is present (`id` for `Task`, `role` for
//! `Message`). An untagged enum matches the
//! wire form; a tagged enum
//! (`{"kind": "task", "task": {...}}`) would
//! add a key the spec does not name.
//!
//! # Why `accepted_output_modes` is `BTreeSet`
//! and not `Vec`
//!
//! The `accepted_output_modes` list is a
//! set of MIME types the client can accept;
//! a duplicate is a wire bug. The Rust
//! type that surfaces a duplicate is a
//! `BTreeSet` (ordered, so the wire form is
//! deterministic; set, so duplicates fail
//! at the type level). The trade-off is
//! losing the order the client sent —
//! which the spec does not require
//! preserving.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::message::Message;
use super::task::Task;

/// A typed `SendMessage` request. Wire form:
/// ```json
/// {
///   "message": { "role": "user", "parts": [...] },
///   "configuration"?: { ... },
///   "metadata"?: { ... }
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageRequest {
    /// The message to send. Required.
    pub message: Message,
    /// Optional configuration for the request.
    /// An absent `configuration` means the
    /// agent uses its defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration: Option<MessageSendConfiguration>,
    /// Per-request metadata. The spec leaves
    /// the shape open; the conventional keys
    /// are `"trace_id"`, `"session_id"`, and
    /// the custom names a particular
    /// deployment uses.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl SendMessageRequest {
    /// Starts a new request with `message`. Use
    /// [`with_configuration`](Self::with_configuration)
    /// to set the optional configuration.
    #[must_use]
    pub const fn new(message: Message) -> Self {
        Self {
            message,
            configuration: None,
            metadata: BTreeMap::new(),
        }
    }

    /// Sets the configuration.
    #[must_use]
    pub fn with_configuration(mut self, configuration: MessageSendConfiguration) -> Self {
        self.configuration = Some(configuration);
        self
    }
}

/// Configuration for a `SendMessage` request.
///
/// Wire form:
/// ```json
/// {
///   "acceptedOutputModes": ["text/plain", "application/json"],
///   "historyLength": 10,
///   "blocking": true
/// }
/// ```
///
/// All fields are optional. A configuration
/// with every field set to its default (an
/// empty `accepted_output_modes`, no history
/// length, no blocking) serialises to `{}` on
/// the wire.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageSendConfiguration {
    /// The MIME types the client can accept.
    /// A client that sends a message but the
    /// agent's reply is in a type not in this
    /// list is a wire contract violation; the
    /// agent is free to refuse with a typed
    /// error.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub accepted_output_modes: BTreeSet<String>,
    /// The number of history messages to
    /// include in the response. A request with
    /// no history length lets the agent choose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_length: Option<u32>,
    /// Whether the request should block
    /// until the task completes. A
    /// non-blocking request returns a `Task`
    /// in `Submitted` or `Working` state;
    /// the client polls with `GetTask` to
    /// advance.
    #[serde(default, skip_serializing_if = "is_false")]
    pub blocking: bool,
}

// `skip_serializing_if` requires `fn(&T) -> bool`,
// so the argument is `&bool` even though the
// type is one byte. The clippy lint is a false
// positive for serde-internal helpers.
#[expect(clippy::trivially_copy_pass_by_ref, reason = "required by skip_serializing_if")]
const fn is_false(b: &bool) -> bool {
    !*b
}

impl MessageSendConfiguration {
    /// Starts a new configuration with the
    /// accepted output modes from `modes`. Use
    /// [`with_history_length`](Self::with_history_length)
    /// and [`with_blocking`](Self::with_blocking)
    /// to set the other fields.
    #[must_use]
    pub fn new(modes: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            accepted_output_modes: modes.into_iter().map(Into::into).collect(),
            history_length: None,
            blocking: false,
        }
    }

    /// Sets the history length.
    #[must_use]
    pub const fn with_history_length(mut self, length: u32) -> Self {
        self.history_length = Some(length);
        self
    }

    /// Marks the request as blocking.
    #[must_use]
    pub const fn with_blocking(mut self) -> Self {
        self.blocking = true;
        self
    }
}

/// A typed `SendMessage` response.
///
/// The wire form is the inner object directly —
/// a `Task` is `{"id": "...", "status": ...}`,
/// a `Message` is `{"role": ..., "parts": ...}`.
/// The two shapes are discriminated by which
/// top-level field is present (`id` for `Task`,
/// `role` for `Message`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SendMessageResponse {
    /// The agent created (or is creating) a
    /// task to do the work. The task's
    /// `status.state` is one of `Submitted`,
    /// `Working`, `Completed`, `Failed`,
    /// `Cancelled`, or `InputRequired`. For
    /// a blocking request, the state is
    /// `Completed`, `Failed`, `Cancelled`, or
    /// `InputRequired` by the time the
    /// response is sent.
    Task(Task),
    /// The agent answered directly without
    /// creating a task. The message is the
    /// agent's reply.
    Message(Message),
}

impl From<Task> for SendMessageResponse {
    fn from(task: Task) -> Self {
        Self::Task(task)
    }
}

impl From<Message> for SendMessageResponse {
    fn from(message: Message) -> Self {
        Self::Message(message)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MessageSendConfiguration, SendMessageRequest, SendMessageResponse,
    };
    use crate::domain::a2a::message::{Message, MessageRole, Part, TextPart};
    use crate::domain::a2a::task::{Artifact, Task, TaskState, TaskStatus};

    type TestError = Box<dyn std::error::Error>;

    fn user_message() -> Message {
        Message::new(MessageRole::User).with_part(Part::Text(TextPart {
            text: "Hello.".to_owned(),
            metadata: std::collections::BTreeMap::new(),
        }))
    }

    // --- SendMessageRequest ------------------------------------------

    #[test]
    fn request_serialises_with_message_field() -> Result<(), TestError> {
        let request = SendMessageRequest::new(user_message());
        let value = serde_json::to_value(&request)?;
        let obj = value
            .as_object()
            .ok_or("a SendMessageRequest must serialise to a JSON object")?;
        assert!(obj.contains_key("message"));
        // The optional fields are absent on
        // the wire when they are `None` /
        // empty.
        assert!(!obj.contains_key("configuration"));
        assert!(!obj.contains_key("metadata"));
        Ok(())
    }

    #[test]
    fn request_round_trips_with_configuration() -> Result<(), TestError> {
        let request = SendMessageRequest::new(user_message()).with_configuration(
            MessageSendConfiguration::new(["text/plain", "application/json"])
                .with_history_length(10)
                .with_blocking(),
        );
        let json = serde_json::to_string(&request)?;
        let parsed: SendMessageRequest = serde_json::from_str(&json)?;
        assert_eq!(parsed, request);
        Ok(())
    }

    // --- MessageSendConfiguration ------------------------------------

    #[test]
    fn configuration_with_no_fields_is_minimal() -> Result<(), TestError> {
        // A configuration with every field
        // at its default serialises to `{}`
        // on the wire. The `blocking` field
        // is `false` by default, the
        // `accepted_output_modes` is empty,
        // and `history_length` is `None`.
        let config = MessageSendConfiguration::default();
        let value = serde_json::to_value(&config)?;
        let obj = value
            .as_object()
            .ok_or("a MessageSendConfiguration must serialise to a JSON object")?;
        assert!(obj.is_empty(), "expected `{{}}`, got {value}");
        Ok(())
    }

    #[test]
    fn configuration_blocking_field_omits_when_false() -> Result<(), TestError> {
        // `blocking: false` is the default,
        // and a field that is at its default
        // must not be on the wire — the
        // spec's `OpenAPI 3.x` shape does
        // not distinguish "not present" from
        // "present and false", but a strict
        // peer is entitled to.
        let config = MessageSendConfiguration::new(["text/plain"]);
        let value = serde_json::to_value(&config)?;
        let obj = value
            .as_object()
            .ok_or("a MessageSendConfiguration must serialise to a JSON object")?;
        assert!(!obj.contains_key("blocking"));
        Ok(())
    }

    #[test]
    fn configuration_accepted_modes_round_trip_in_sorted_order() -> Result<(), TestError> {
        // The `BTreeSet` gives the wire form
        // a deterministic order (alphabetical),
        // which keeps test fixtures readable
        // and the diff-on-the-wire stable.
        let config =
            MessageSendConfiguration::new(["text/plain", "application/json", "image/png"]);
        let value = serde_json::to_value(&config)?;
        let modes = value
            .get("acceptedOutputModes")
            .and_then(serde_json::Value::as_array)
            .ok_or("`acceptedOutputModes` must be a JSON array")?;
        let names: Vec<&str> = modes
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect();
        assert_eq!(
            names,
            vec!["application/json", "image/png", "text/plain"],
            "BTreeSet is sorted alphabetically"
        );
        Ok(())
    }

    // --- SendMessageResponse -----------------------------------------

    #[test]
    fn response_serialises_task_as_task_object() -> Result<(), TestError> {
        let task = Task::new("task-1").with_status(TaskStatus {
            state: TaskState::Working,
            message: None,
            timestamp: None,
        });
        let response = SendMessageResponse::Task(task);
        let value = serde_json::to_value(&response)?;
        let obj = value
            .as_object()
            .ok_or("a SendMessageResponse must serialise to a JSON object")?;
        // The wire form is the inner Task
        // object directly — no wrapper.
        assert_eq!(
            obj.get("id").and_then(serde_json::Value::as_str),
            Some("task-1")
        );
        assert!(obj.contains_key("status"));
        // A `Task` has `id`, a `Message` has
        // `role`. The discriminator is the
        // presence of one or the other.
        assert!(!obj.contains_key("role"));
        Ok(())
    }

    #[test]
    fn response_serialises_message_as_message_object() -> Result<(), TestError> {
        let message = Message::new(MessageRole::Agent).with_part(Part::Text(TextPart {
            text: "Hello, user.".to_owned(),
            metadata: std::collections::BTreeMap::new(),
        }));
        let response = SendMessageResponse::Message(message);
        let value = serde_json::to_value(&response)?;
        let obj = value
            .as_object()
            .ok_or("a SendMessageResponse must serialise to a JSON object")?;
        assert_eq!(
            obj.get("role").and_then(serde_json::Value::as_str),
            Some("agent")
        );
        assert!(!obj.contains_key("id"));
        Ok(())
    }

    #[test]
    fn response_untagged_discrimination_works_both_ways() -> Result<(), TestError> {
        // A `Task` JSON object decodes to
        // `SendMessageResponse::Task`, not
        // `::Message`. A `Message` JSON
        // object decodes to `::Message`.
        let task_json = r#"{"id":"task-1","status":{"state":"working"}}"#;
        let message_json =
            r#"{"role":"agent","parts":[{"text":"hi"}]}"#;
        let from_task: SendMessageResponse = serde_json::from_str(task_json)
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let from_message: SendMessageResponse = serde_json::from_str(message_json)
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        assert!(matches!(from_task, SendMessageResponse::Task(_)));
        assert!(matches!(from_message, SendMessageResponse::Message(_)));
        Ok(())
    }

    #[test]
    fn response_task_with_artifacts_round_trips() -> Result<(), TestError> {
        let task = Task::new("task-1").with_artifact(Artifact::new(Part::Text(TextPart {
            text: "answer".to_owned(),
            metadata: std::collections::BTreeMap::new(),
        })));
        let response = SendMessageResponse::Task(task);
        let json = serde_json::to_string(&response)?;
        let parsed: SendMessageResponse = serde_json::from_str(&json)?;
        assert_eq!(parsed, response);
        Ok(())
    }

    #[test]
    fn response_from_message_via_into_works() {
        let message = user_message();
        let response: SendMessageResponse = message.clone().into();
        assert_eq!(response, SendMessageResponse::Message(message));
    }
}
