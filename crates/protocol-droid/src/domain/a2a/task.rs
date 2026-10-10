//! A2A `Task`, `TaskStatus`, `TaskState`, and
//! `Artifact` value objects.
//!
//! # What these are
//!
//! A [`Task`] is the unit of work an A2A agent
//! tracks. A client opens a task by sending a
//! `SendMessage` request; the agent creates a
//! task, runs the work, and answers with a
//! `Task` whose [`TaskStatus::state`] moves
//! through [`Submitted`](TaskState::Submitted)
//! → [`Working`](TaskState::Working) →
//! [`Completed`](TaskState::Completed) (or one
//! of the failure states).
//!
//! A [`Task`] carries:
//! - An id (the task's identity) and an optional
//!   context id (a grouping across related
//!   tasks).
//! - A [`TaskStatus`] — the current state, the
//!   status message, and the timestamp.
//! - A list of [`Artifact`]s — the work's
//!   output. An artifact is a named bundle of
//!   [`Part`](super::message::Part)s.
//! - A list of [`Message`](super::message::Message)s
//!   — the conversation history that produced
//!   the task.
//!
//! # Why a `BTreeMap` for the metadata
//!
//! Same rationale as
//! [`Message::metadata`](super::message::Message::metadata):
//! the wire form is deterministic (alphabetical
//! by key), which keeps test fixtures readable
//! and makes diff-on-the-wire stable.
//!
//! # Why `timestamp` is a string
//!
//! The A2A spec uses RFC 3339 / ISO 8601
//! timestamps. The Rust type that round-trips
//! with the spec is a string — `chrono::DateTime`
//! is a more typed alternative but adds a
//! dependency and a `Result` on every read.
//! The string is parsed by the consumer if a
//! parsed form is needed.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::message::Message;

/// A unit of work an A2A agent tracks.
///
/// Wire form:
/// ```json
/// {
///   "id": "task-123",
///   "contextId": "context-456",
///   "status": { "state": "working" },
///   "artifacts": [...],
///   "history": [...],
///   "metadata": {...}
/// }
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    /// The task's id. Required and unique
    /// within the agent.
    pub id: Arc<str>,
    /// An optional context id. A context groups
    /// related tasks (a multi-turn conversation
    /// is a context; each user turn is a task
    /// within that context). The spec does not
    /// require contexts; an agent that does not
    /// support them leaves the field off the
    /// wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<Arc<str>>,
    /// The task's current status.
    pub status: TaskStatus,
    /// The task's output. An empty list is a
    /// task that has not produced any output
    /// yet (or a task whose output is empty by
    /// design).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<Artifact>,
    /// The conversation history that produced
    /// the task. A task created from a single
    /// `SendMessage` request has a one-element
    /// history; a task created from a streaming
    /// interaction has more.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<Message>,
    /// Per-task metadata. The spec leaves the
    /// shape open; the conventional keys are
    /// `"trace_id"`, `"session_id"`, and the
    /// custom names a particular deployment
    /// uses.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

use std::sync::Arc;

impl Task {
    /// Starts a new task in
    /// [`Submitted`](TaskState::Submitted) state
    /// with `id`. Use [`with_status`](Self::with_status)
    /// to advance the state, [`with_artifact`](Self::with_artifact)
    /// to attach an output, and so on.
    #[must_use]
    pub fn new(id: impl Into<Arc<str>>) -> Self {
        Self {
            id: id.into(),
            context_id: None,
            status: TaskStatus {
                state: TaskState::Submitted,
                message: None,
                timestamp: None,
            },
            artifacts: Vec::new(),
            history: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }

    /// Replaces the task's status. The task is
    /// consumed; chain
    /// `Task::new(id).with_status(...).with_artifact(...)`
    /// to build a task in one expression.
    #[must_use]
    pub fn with_status(mut self, status: TaskStatus) -> Self {
        self.status = status;
        self
    }

    /// Appends an artifact to the task.
    #[must_use]
    pub fn with_artifact(mut self, artifact: impl Into<Artifact>) -> Self {
        self.artifacts.push(artifact.into());
        self
    }

    /// Appends a message to the history.
    #[must_use]
    pub fn with_history_message(mut self, message: impl Into<Message>) -> Self {
        self.history.push(message.into());
        self
    }
}

/// A task's current status.
///
/// Wire form:
/// ```json
/// {
///   "state": "working",
///   "message": { "role": "agent", "parts": [...] },
///   "timestamp": "2025-01-01T00:00:00Z"
/// }
/// ```
///
/// `state` is required; `message` and
/// `timestamp` are optional. A status with
/// no message and no timestamp is the
/// minimum valid wire form.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskStatus {
    /// The state. Required.
    pub state: TaskState,
    /// An optional message the agent attaches
    /// to the status — typically used for
    /// error messages on a
    /// [`Failed`](TaskState::Failed) status or
    /// clarification questions on an
    /// [`InputRequired`](TaskState::InputRequired)
    /// status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Message>,
    /// An optional RFC 3339 timestamp. The
    /// string is the wire form; a consumer
    /// that wants a parsed form does the
    /// `chrono::DateTime::parse_from_rfc3339`
    /// on demand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<Arc<str>>,
}

/// The state of a [`Task`].
///
/// The six values are the spec's exhaustive
/// list. The state moves through the
/// [`Submitted`](Self::Submitted) →
/// [`Working`](Self::Working) → either
/// [`Completed`](Self::Completed),
/// [`Failed`](Self::Failed),
/// [`Cancelled`](Self::Cancelled), or
/// [`InputRequired`](Self::InputRequired)
/// progression. The wire form is
/// `snake_case` to match the spec's
/// examples.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// The task has been received by the
    /// agent and is queued for execution.
    #[default]
    Submitted,
    /// The task is currently being executed.
    /// This is the only "in-flight" state;
    /// every other state is terminal or
    /// blocking.
    Working,
    /// The task finished successfully. The
    /// task's `artifacts` list is the output.
    Completed,
    /// The task failed. The `status.message`
    /// field, if present, explains why.
    Failed,
    /// The task was cancelled by the client
    /// (a `CancelTask` request) or by the
    /// agent. The state is terminal; a
    /// cancelled task cannot be resumed.
    Cancelled,
    /// The task is blocked on user input.
    /// The `status.message` field, if
    /// present, is the agent's question.
    InputRequired,
}

/// A unit of output produced by a [`Task`].
///
/// Wire form:
/// ```json
/// {
///   "name": "answer",
///   "description": "The agent's final answer.",
///   "parts": [Part, ...],
///   "metadata": {...}
/// }
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    /// An optional name. The spec does not
    /// require it; a part-only artifact (no
    /// name, no description) is valid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// An optional human-readable description.
    /// Agents that want a UI surface to label
    /// the artifact set it; agents that treat
    /// the artifact as an opaque payload do
    /// not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The artifact's content. An empty list
    /// is a valid artifact (an empty result);
    /// the spec does not require at least one
    /// part.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<super::message::Part>,
    /// Per-artifact metadata. Same shape and
    /// rationale as [`Task::metadata`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl Artifact {
    /// Starts a new artifact with one part. Use
    /// [`with_part`](Self::with_part) to add more
    /// parts and [`with_name`](Self::with_name)
    /// / [`with_description`](Self::with_description)
    /// to set the optional fields.
    #[must_use]
    pub fn new(part: impl Into<super::message::Part>) -> Self {
        Self {
            name: None,
            description: None,
            parts: vec![part.into()],
            metadata: BTreeMap::new(),
        }
    }

    /// Appends a part to the artifact.
    #[must_use]
    pub fn with_part(mut self, part: impl Into<super::message::Part>) -> Self {
        self.parts.push(part.into());
        self
    }

    /// Sets the artifact's name.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Sets the artifact's description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use super::{Artifact, Task, TaskState, TaskStatus};
    use crate::domain::a2a::message::{Message, MessageRole, Part, TextPart};

    type TestError = Box<dyn std::error::Error>;

    fn text_part(text: &str) -> Part {
        Part::Text(TextPart {
            text: text.to_owned(),
            metadata: BTreeMap::new(),
        })
    }

    // --- Task wire form -----------------------------------------------

    #[test]
    fn task_serialises_with_required_id_and_status() -> Result<(), TestError> {
        let task = Task::new("task-1");
        let value = serde_json::to_value(&task)?;
        let obj = value
            .as_object()
            .ok_or("a Task must serialise to a JSON object")?;
        assert_eq!(
            obj.get("id").and_then(serde_json::Value::as_str),
            Some("task-1")
        );
        let status = obj
            .get("status")
            .and_then(serde_json::Value::as_object)
            .ok_or("`status` must be a JSON object")?;
        assert_eq!(
            status.get("state").and_then(serde_json::Value::as_str),
            Some("submitted")
        );
        Ok(())
    }

    #[test]
    fn task_with_no_artifacts_or_history_omits_the_fields() -> Result<(), TestError> {
        // Same `Option`-shaped wire form as
        // `Message::metadata`: an empty list is
        // absent, not present-but-empty.
        let task = Task::new("task-1");
        let value = serde_json::to_value(&task)?;
        let obj = value
            .as_object()
            .ok_or("a Task must serialise to a JSON object")?;
        assert!(!obj.contains_key("artifacts"));
        assert!(!obj.contains_key("history"));
        assert!(!obj.contains_key("metadata"));
        assert!(!obj.contains_key("contextId"));
        Ok(())
    }

    #[test]
    fn task_round_trips_with_artifacts_and_history() -> Result<(), TestError> {
        let task = Task::new("task-1")
            .with_status(TaskStatus {
                state: TaskState::Working,
                message: Some(Message::new(MessageRole::Agent).with_part(text_part("thinking..."))),
                timestamp: Some(Arc::from("2025-01-01T00:00:00Z")),
            })
            .with_artifact(Artifact::new(text_part("answer")))
            .with_history_message(Message::new(MessageRole::User).with_part(text_part("hi")));
        let json = serde_json::to_string(&task)?;
        let parsed: Task = serde_json::from_str(&json)?;
        assert_eq!(parsed, task);
        Ok(())
    }

    // --- TaskState wire form ------------------------------------------

    #[test]
    fn task_state_serialises_in_lowercase() -> Result<(), TestError> {
        // The wire form is `lowercase` to
        // match the spec's examples. Every
        // variant is pinned here so a future
        // rename-with-`#[serde(rename_all = ...)]`
        // change fails the lint.
        for (state, expected) in [
            (TaskState::Submitted, "submitted"),
            (TaskState::Working, "working"),
            (TaskState::Completed, "completed"),
            (TaskState::Failed, "failed"),
            (TaskState::Cancelled, "cancelled"),
            (TaskState::InputRequired, "input_required"),
        ] {
            let value = serde_json::to_value(state)?;
            let got = value
                .as_str()
                .ok_or("a TaskState must serialise to a JSON string")?;
            assert_eq!(got, expected, "TaskState::{state:?}");
        }
        Ok(())
    }

    #[test]
    fn task_state_round_trips_for_every_variant() -> Result<(), TestError> {
        for state in [
            TaskState::Submitted,
            TaskState::Working,
            TaskState::Completed,
            TaskState::Failed,
            TaskState::Cancelled,
            TaskState::InputRequired,
        ] {
            let json = serde_json::to_string(&state)?;
            let parsed: TaskState = serde_json::from_str(&json)?;
            assert_eq!(parsed, state);
        }
        Ok(())
    }

    // --- TaskStatus wire form -----------------------------------------

    #[test]
    fn task_status_with_no_message_or_timestamp_is_minimal() -> Result<(), TestError> {
        // A `TaskStatus` with just a state
        // serialises to `{"state": "..."}`
        // only. The other fields are absent
        // when they are `None`.
        let status = TaskStatus {
            state: TaskState::Working,
            message: None,
            timestamp: None,
        };
        let value = serde_json::to_value(&status)?;
        let obj = value
            .as_object()
            .ok_or("a TaskStatus must serialise to a JSON object")?;
        assert_eq!(obj.len(), 1);
        assert_eq!(
            obj.get("state").and_then(serde_json::Value::as_str),
            Some("working")
        );
        Ok(())
    }

    // --- Artifact wire form -------------------------------------------

    #[test]
    fn artifact_serialises_with_camel_case_name() -> Result<(), TestError> {
        // The `Artifact` is the only A2A
        // type that does not use the
        // `camelCase` rename. The spec's
        // wire form has `name` and
        // `description` in their natural
        // case (no `Name` / `Description`).
        // The test pins that choice.
        let artifact = Artifact::new(text_part("data"))
            .with_name("answer")
            .with_description("The agent's final answer.");
        let value = serde_json::to_value(&artifact)?;
        let obj = value
            .as_object()
            .ok_or("an Artifact must serialise to a JSON object")?;
        assert_eq!(
            obj.get("name").and_then(serde_json::Value::as_str),
            Some("answer")
        );
        assert_eq!(
            obj.get("description").and_then(serde_json::Value::as_str),
            Some("The agent's final answer.")
        );
        assert!(!obj.contains_key("Name"));
        assert!(!obj.contains_key("Description"));
        Ok(())
    }

    #[test]
    fn artifact_round_trips_with_multiple_parts() -> Result<(), TestError> {
        let artifact = Artifact::new(text_part("first")).with_part(text_part("second"));
        let json = serde_json::to_string(&artifact)?;
        let parsed: Artifact = serde_json::from_str(&json)?;
        assert_eq!(parsed, artifact);
        Ok(())
    }
}
